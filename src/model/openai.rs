//! OpenAI-compatible 模型端点客户端。
//!
//! # 协议
//!
//! `POST {OPENAI_BASE_URL}/chat/completions`，`Authorization: Bearer $OPENAI_API_KEY`。
//! 请求体字段全集：`model` / `temperature` / `stream` / `response_format` / `messages`。
//! 响应按 OpenAI-compatible 的 chat completion 结构解，取 `choices[0].message.content`
//! 里的 JSON 文本，再交给 [`crate::model::schema`] 做结构校验。
//!
//! # 端点兼容
//!
//! 默认带 `response_format: {"type":"json_object"}`（结构化输出的标准做法）。
//! 少数 OpenAI-compatible 端点不支持该字段并就此返回 4xx —— 此时客户端
//! **去掉该字段重试一次**（系统提示里仍然要求只回 JSON），仍失败才报
//! `MODEL_BAD_REQUEST`，并把端点原话带在错误里。不静默改协议，也不吞掉端点的抱怨。
//!
//! # 错误语义分层
//!
//! | 情形 | 错误码 |
//! |---|---|
//! | 连不上（连接被拒 / DNS / 网络不可达） | `MODEL_CONNECTION_FAILED` |
//! | 超时 | `MODEL_TIMEOUT` |
//! | HTTP 4xx（端点拒绝请求） | `MODEL_BAD_REQUEST` |
//! | HTTP 5xx（端点服务端错误） | `MODEL_SERVER_ERROR` |
//! | 响应不是 chat/completions 结构 / 内容不是 JSON / 结构校验不通过 | `MODEL_RESPONSE_INVALID` |
//!
//! 五种情形**互不混淆**：4xx 与 5xx 的处置不同（前者改请求，后者退避重试），
//! 混成一个码就看不出该找谁。

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{EnvSource, PartialConfig};
use crate::errors::{EngineError, ErrorCode};
use crate::model::gateway::{MergeRequest, ModelGateway};
use crate::model::schema::{self, MergeResponse, ModelUsage};

/// 模型端点的四个环境变量（缺任一项都不兜底）
pub const MODEL_ENV_VARS: [&str; 4] = [
    "OPENAI_BASE_URL",
    "OPENAI_API_KEY",
    "OPENAI_MODEL",
    "OPENAI_TIMEOUT_SECS",
];

/// 系统提示：只要求回 JSON，并划清「同指标」与「同一笔记录」的区别。
///
/// 提示里不出现任何数值口径 —— 数值对不对由引擎判，不该让模型顺手判。
pub const SYSTEM_PROMPT: &str = "\
你是医院卫健专项上报表的核对助手。我给你若干对「指标名称写法」，请判断每一对是否指同一件事。\
只输出一个 JSON 对象，不要输出解释文字，也不要用 Markdown 代码块包起来，格式必须是：\
{\"items\":[{\"left_row_key\":\"...\",\"right_row_key\":\"...\",\"kind\":\"same_indicator\",\"confidence\":0.5,\"reason\":\"中文理由\",\"evidence\":[\"依据\"]}]}\
规则：\
1. kind 取 same_indicator 表示两个名称指同一指标但未必是同一笔记录；取 duplicate_record 表示同一笔记录被填了两次。\
2. confidence 取 0.0 到 1.0；把握不大就给小值，不要凑高。\
3. evidence 至少给一条可核对的事实（例如两个名称共同包含的词、报告期是否相同）。\
4. 只能使用我给出的 row_key，不要发明新的行键；实在判断不了的那一对可以不列。";

/// 模型端点配置（四项全部来自环境变量）
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// 端点根地址（末尾斜杠会被去掉）
    pub base_url: String,
    /// Bearer 凭据
    pub api_key: String,
    /// 模型名
    pub model: String,
    /// 超时秒数
    pub timeout_secs: u64,
}

impl ModelConfig {
    /// 从环境变量读齐四项；缺项报 `CONFIG_MISSING` 并点名
    pub fn load(src: &dyn EnvSource) -> Result<ModelConfig, EngineError> {
        let partial = PartialConfig::load(src);
        partial.require(&MODEL_ENV_VARS)?;
        let timeout_raw = partial.get("OPENAI_TIMEOUT_SECS").unwrap_or("");
        let timeout_secs = timeout_raw.parse::<u64>().map_err(|_| {
            EngineError::at(
                ErrorCode::ConfigInvalid,
                format!("环境变量 OPENAI_TIMEOUT_SECS 不是合法整数：{}", timeout_raw),
                "OPENAI_TIMEOUT_SECS",
            )
        })?;
        Ok(ModelConfig {
            base_url: partial
                .get("OPENAI_BASE_URL")
                .unwrap_or("")
                .trim_end_matches('/')
                .to_string(),
            api_key: partial.get("OPENAI_API_KEY").unwrap_or("").to_string(),
            model: partial.get("OPENAI_MODEL").unwrap_or("").to_string(),
            timeout_secs,
        })
    }

    /// chat/completions 端点
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// chat/completions 请求体
#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    temperature: f32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<JsonObjectMode>,
    messages: Vec<ChatMessage>,
}

/// JSON 模式开关
#[derive(Debug, Clone, Serialize)]
struct JsonObjectMode {
    #[serde(rename = "type")]
    kind: String,
}

/// 一条消息
#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// 发给模型的载荷（只含最小必要字段）
#[derive(Debug, Clone, Serialize)]
struct PromptPayload {
    schema_version: String,
    pairs: Vec<PromptPair>,
}

/// 一对候选行
#[derive(Debug, Clone, Serialize)]
struct PromptPair {
    left: PromptCandidate,
    right: PromptCandidate,
}

/// 一行候选（行键 + 指标名称 + 科室 + 报告期，**没有别的**）
#[derive(Debug, Clone, Serialize)]
struct PromptCandidate {
    row_key: String,
    indicator_name: String,
    dept_name: String,
    reporting_period: String,
}

/// chat/completions 响应（只取用得到的字段）
#[derive(Debug, Clone, Deserialize)]
struct RawChatCompletion {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

/// 一个候选回答
#[derive(Debug, Clone, Deserialize)]
struct RawChoice {
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// 回答里的消息
#[derive(Debug, Clone, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<Value>,
}

/// token 用量
#[derive(Debug, Clone, Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

/// OpenAI-compatible 端点客户端。
///
/// **只有这一处**出现 HTTP 客户端与厂商协议细节；业务层只见 [`ModelGateway`]。
pub struct OpenAiModelGateway {
    config: ModelConfig,
    client: reqwest::blocking::Client,
}

impl OpenAiModelGateway {
    /// 用已装配好的配置建客户端
    pub fn new(config: ModelConfig) -> Result<OpenAiModelGateway, EngineError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .map_err(|e| {
                EngineError::new(
                    ErrorCode::ModelConnectionFailed,
                    format!("创建模型端点 HTTP 客户端失败：{}", e),
                )
            })?;
        Ok(OpenAiModelGateway { config, client })
    }

    /// 从环境变量装配（凭据与超时全在环境变量里）
    pub fn from_env(src: &dyn EnvSource) -> Result<OpenAiModelGateway, EngineError> {
        OpenAiModelGateway::new(ModelConfig::load(src)?)
    }

    /// 当前配置（只读）
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    /// 组装请求体
    fn build_body(&self, request: &MergeRequest, json_mode: bool) -> ChatRequest {
        let pairs = request
            .pairs
            .iter()
            .map(|pair| PromptPair {
                left: to_prompt(&pair.left),
                right: to_prompt(&pair.right),
            })
            .collect();
        let payload = PromptPayload {
            schema_version: request.schema_version.clone(),
            pairs,
        };
        ChatRequest {
            model: self.config.model.clone(),
            temperature: 0.0,
            stream: false,
            response_format: if json_mode {
                Some(JsonObjectMode {
                    kind: "json_object".to_string(),
                })
            } else {
                None
            },
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
                },
            ],
        }
    }

    /// 发一次请求并返回响应正文（HTTP 层）
    ///
    /// 端点不认识 `response_format` 时去掉它重试一次 —— 这是协议兼容，
    /// 不是静默改协议：重试仍失败就把端点原话带在错误里。
    fn post(&self, body: &ChatRequest) -> Result<String, EngineError> {
        match self.send(body) {
            Ok(text) => Ok(text),
            Err(err) => {
                if err.code == ErrorCode::ModelBadRequest
                    && body.response_format.is_some()
                    && mentions_response_format(&err.message)
                {
                    let mut retry = body.clone();
                    retry.response_format = None;
                    self.send(&retry)
                } else {
                    Err(err)
                }
            }
        }
    }

    /// 发一次请求，返回 (状态码, 响应正文)
    fn send(&self, body: &ChatRequest) -> Result<String, EngineError> {
        let resp = self
            .client
            .post(self.config.endpoint())
            .bearer_auth(&self.config.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .map_err(map_send_error)?;

        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if status.is_success() {
            return Ok(text);
        }
        Err(status_error(status, &text))
    }

    /// 端点状态说明（人话）
    pub fn describe_endpoint(&self) -> String {
        format!(
            "模型端点：{} · 模型：{} · 超时 {} 秒",
            self.config.base_url, self.config.model, self.config.timeout_secs
        )
    }
}

impl ModelGateway for OpenAiModelGateway {
    fn merge_candidates(&self, request: &MergeRequest) -> Result<MergeResponse, EngineError> {
        let known: BTreeSet<String> = request.known_row_keys();
        if request.pairs.is_empty() {
            // 没有要问的就不去打扰端点
            let mut empty = MergeResponse::empty();
            empty.served_model = Some(self.config.model.clone());
            return Ok(empty);
        }

        let body = self.build_body(request, true);
        let raw = self.post(&body)?;

        let completion: RawChatCompletion = serde_json::from_str(&raw).map_err(|e| {
            EngineError::new(
                ErrorCode::ModelResponseInvalid,
                format!("模型端点返回的不是 chat/completions 结构：{}", e),
            )
        })?;

        let choice = completion.choices.first().ok_or_else(|| {
            EngineError::new(
                ErrorCode::ModelResponseInvalid,
                "模型端点没有返回任何候选回答（choices 为空）",
            )
        })?;

        if choice.finish_reason.as_deref() == Some("length") {
            return Err(EngineError::new(
                ErrorCode::ModelResponseInvalid,
                "模型响应被长度截断（finish_reason=length），内容不完整",
            ));
        }

        let message = choice.message.as_ref().ok_or_else(|| {
            EngineError::new(
                ErrorCode::ModelResponseInvalid,
                "模型端点返回的候选回答里没有 message",
            )
        })?;
        let content = extract_content_text(message).ok_or_else(|| {
            EngineError::new(
                ErrorCode::ModelResponseInvalid,
                "模型端点返回的候选回答里没有可读的 content",
            )
        })?;

        let mut response = schema::parse_merge_response(&content, &known)?;
        response.served_model = completion.model.clone().or(Some(self.config.model.clone()));
        response.usage = completion.usage.map(|u| ModelUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });
        Ok(response)
    }

    fn describe(&self) -> String {
        self.describe_endpoint()
    }
}

/// 候选行 → 提示载荷（只四项）
fn to_prompt(candidate: &crate::model::gateway::MergeCandidate) -> PromptCandidate {
    PromptCandidate {
        row_key: candidate.row_key.clone(),
        indicator_name: candidate.indicator_name.clone(),
        dept_name: candidate.dept_name.clone(),
        reporting_period: candidate.reporting_period.clone(),
    }
}

/// 从响应消息里取文本（字符串或分片数组都认）
fn extract_content_text(message: &RawMessage) -> Option<String> {
    match &message.content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            let mut buf = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    buf.push_str(text);
                }
            }
            if buf.is_empty() {
                None
            } else {
                Some(buf)
            }
        }
        _ => None,
    }
}

/// 端点是不是在说「我不认识 response_format」
fn mentions_response_format(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("response_format") || lower.contains("response format")
}

/// HTTP 状态 → 错误码
fn status_error(status: reqwest::StatusCode, body: &str) -> EngineError {
    let snippet: String = body.chars().take(400).collect();
    let code = if status.is_server_error() {
        ErrorCode::ModelServerError
    } else {
        ErrorCode::ModelBadRequest
    };
    let head = if status.is_server_error() {
        "模型端点返回服务端错误"
    } else if status.is_client_error() {
        "模型端点拒绝了请求"
    } else {
        "模型端点返回了非预期状态"
    };
    EngineError::at(
        code,
        format!("{}（HTTP {}）：{}", head, status.as_u16(), snippet.trim()),
        "POST /chat/completions",
    )
}

/// 发送期错误 → 错误码（连接失败 / 超时 / 读取失败分开）
fn map_send_error(e: reqwest::Error) -> EngineError {
    if e.is_timeout() {
        EngineError::new(ErrorCode::ModelTimeout, format!("模型端点调用超时：{}", e))
    } else if e.is_connect() {
        EngineError::new(
            ErrorCode::ModelConnectionFailed,
            format!("模型端点连接失败：{}", e),
        )
    } else if e.is_body() || e.is_decode() {
        EngineError::new(
            ErrorCode::ModelResponseInvalid,
            format!("读取模型响应失败：{}", e),
        )
    } else {
        EngineError::new(
            ErrorCode::ModelConnectionFailed,
            format!("模型端点调用失败：{}", e),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MapEnv;
    use crate::model::gateway::{MergeCandidate, MergePair};

    fn candidate(k: &str) -> MergeCandidate {
        MergeCandidate {
            row_key: k.to_string(),
            indicator_name: "医院感染例数".to_string(),
            dept_name: "院感科".to_string(),
            reporting_period: "2026-Q3".to_string(),
        }
    }

    #[test]
    fn 配置缺项时报错并点名() {
        let mut env = MapEnv::new();
        env.set("OPENAI_BASE_URL", "https://model.internal.example/v1");
        let err = ModelConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigMissing);
        assert!(err.message.contains("OPENAI_API_KEY"));
        assert!(err.message.contains("OPENAI_MODEL"));
    }

    #[test]
    fn 超时取值不合法时报配置错误() {
        let mut env = MapEnv::new();
        env.set("OPENAI_BASE_URL", "https://model.internal.example/v1");
        env.set("OPENAI_API_KEY", "k");
        env.set("OPENAI_MODEL", "m");
        env.set("OPENAI_TIMEOUT_SECS", "soon");
        let err = ModelConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(err.location.unwrap().contains("OPENAI_TIMEOUT_SECS"));
    }

    #[test]
    fn 端点路径按_openai_compatible_拼() {
        let cfg = ModelConfig {
            base_url: "https://model.internal.example/v1/".to_string(),
            api_key: "k".to_string(),
            model: "m".to_string(),
            timeout_secs: 5,
        };
        assert_eq!(
            cfg.endpoint(),
            "https://model.internal.example/v1/chat/completions"
        );
    }

    #[test]
    fn 请求体字段全集固定() {
        let cfg = ModelConfig {
            base_url: "https://model.internal.example/v1".to_string(),
            api_key: "k".to_string(),
            model: "m".to_string(),
            timeout_secs: 5,
        };
        let gw = OpenAiModelGateway::new(cfg).unwrap();
        let req = MergeRequest::new(vec![MergePair {
            left: candidate("k1"),
            right: candidate("k2"),
        }]);
        let body = gw.build_body(&req, true);
        let v = serde_json::to_value(&body).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "messages",
                "model",
                "response_format",
                "stream",
                "temperature"
            ]
        );
        assert_eq!(obj["stream"], serde_json::json!(false));
        assert_eq!(
            obj["response_format"]["type"],
            serde_json::json!("json_object")
        );
        assert_eq!(obj["messages"].as_array().unwrap().len(), 2);
        // 载荷里只有四项
        let user = obj["messages"][1]["content"].as_str().unwrap();
        let payload: Value = serde_json::from_str(user).unwrap();
        let left = &payload["pairs"][0]["left"];
        let mut ckeys: Vec<&str> = left
            .as_object()
            .unwrap()
            .keys()
            .map(|s| s.as_str())
            .collect();
        ckeys.sort();
        assert_eq!(
            ckeys,
            vec!["dept_name", "indicator_name", "reporting_period", "row_key"]
        );
    }

    #[test]
    fn 不带_json模式时请求体少一个字段() {
        let cfg = ModelConfig {
            base_url: "https://model.internal.example/v1".to_string(),
            api_key: "k".to_string(),
            model: "m".to_string(),
            timeout_secs: 5,
        };
        let gw = OpenAiModelGateway::new(cfg).unwrap();
        let req = MergeRequest::new(vec![MergePair {
            left: candidate("k1"),
            right: candidate("k2"),
        }]);
        let body = gw.build_body(&req, false);
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.as_object().unwrap().get("response_format").is_none());
    }

    #[test]
    fn 提示里不带数值口径与患者字段() {
        let lower = SYSTEM_PROMPT.to_ascii_lowercase();
        for banned in [
            "numerator",
            "denominator",
            "business_key",
            "住院号",
            "身份证",
        ] {
            assert!(!lower.contains(banned), "提示里不该出现 {}", banned);
        }
    }
}
