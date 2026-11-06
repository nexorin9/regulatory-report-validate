//! 院内卫健上报网关出站客户端。
//!
//! # 协议（正式对接规范，见 `spec.md`「出站报文（正式对接规范）」）
//!
//! - 路径：`POST {REGULATORY_REPORT_BASE_URL}/api/v1/regulatory-submissions`
//! - 凭据：`Authorization: Bearer $REGULATORY_REPORT_TOKEN` —— **只从环境变量读**，
//!   程序里不硬编码、不落盘、不进测试。
//! - 幂等：`Idempotency-Key: <幂等键>` 头，与 body 里的 `idempotency_key` 同值。
//! - body：字段全集固定 **15 项**（见 [`OUTBOUND_FIELDS`]），少传多传都不合规。
//!
//! # 状态码语义分类
//!
//! | 对端响应 | 裁决 | 后续处置 |
//! |---|---|---|
//! | `200 {receipt_id, accepted_at, status}` | [`GatewayOutcomeKind::Accepted`] | 行状态由回执驱动（Task 32 落库） |
//! | `4xx {error_code, error_field, message}` | [`GatewayOutcomeKind::Rejected`] | 该行置「被退回」，错误码与字段名写回原表说明列；**不重试** |
//! | `5xx` | [`GatewayOutcomeKind::Retryable`] | 退避重试（Task 33） |
//! | 超时 / 连不上 | [`GatewayOutcomeKind::Retryable`] | 退避重试（Task 33） |
//! | `200` 但回执结构不合法 | [`GatewayOutcomeKind::Invalid`] | **不当成成功**；该行标「回执待人工核对」（Task 32） |
//! | 本行数值填坏了（发请求之前） | [`GatewayOutcomeKind::Blocked`] | **请求没发出去**；该行先修正数值再报 |
//!
//! 网络失败**绝不**被改写成成功 —— 上报接上去的必须是网关的裁决。
//!
//! # 凭据缺失在发请求之前就拦下
//!
//! [`GatewayConfig::load`] 缺 `REGULATORY_REPORT_BASE_URL` / `REGULATORY_REPORT_TOKEN`
//! 任一项即报 `CONFIG_MISSING` 并点名；此时**一个包都没发出去**。
//! 不带着假地址去连院内系统 —— 那样报出来的错会指向网络，而不是指向配置。
//!
//! # 源产品能力 → 融合后行为
//!
//! 参考实现里的**远端连接层**（由根地址拼出端点、`Bearer {token}` 拼出鉴权头）借的是
//! 「端点 + 凭据 + 批次上下文」这三件套的组织方式。本产品**改写**为：
//! - 端点固定成一条上报路径（`/api/v1/regulatory-submissions`），不做多端点分派；
//! - 鉴权头与幂等头**并存** —— 业务幂等靠 `Idempotency-Key` 交给网关判定，
//!   不是靠客户端自己记住「发过了」（客户端记的那份，重启就没了）；
//! - 批次上下文（`batch_ref`）进 **body** 而不是进 URL —— 网关按 body 里的批次号归集，
//!   URL 里塞批次号会让 URL 变长且把批次语义混进资源定位。
//!
//! 协议兼容替身只出现在 `tests/` 测试数据层，**不是**最终对接方式。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{optional_usize, EnvSource, PartialConfig};
use crate::errors::{EngineError, ErrorCode};
use crate::push::idempotency::{NumericField, SubmissionPayload};

/// 上报提交路径（固定一条，挂在网关根地址下）
pub const SUBMISSIONS_PATH: &str = "/api/v1/regulatory-submissions";

/// 幂等键请求头名
pub const IDEMPOTENCY_HEADER: &str = "Idempotency-Key";

/// 网关端的两个环境变量（缺任一项都不兜底）
pub const GATEWAY_ENV_VARS: [&str; 2] = [
    "REGULATORY_REPORT_BASE_URL",
    "REGULATORY_REPORT_TOKEN",
];

/// 网关超时（秒）—— 可选调参项，不设就用默认值
pub const GATEWAY_TIMEOUT_ENV: &str = "REGULATORY_REPORT_TIMEOUT_SECS";

/// 网关超时默认值（秒）
pub const DEFAULT_GATEWAY_TIMEOUT_SECS: u64 = 30;

/// 出站报文字段全集（顺序固定；**少传多传都不合规**）。
///
/// 共 15 项：批次号 1 项 + 业务标识 5 项 + 数值 3 项 + 口径 2 项 + 上报身份 4 项。
pub const OUTBOUND_FIELDS: [&str; 15] = [
    "batch_ref",
    "reporting_period",
    "org_code",
    "indicator_code",
    "dept_code",
    "business_key",
    "numerator",
    "denominator",
    "value",
    "unit",
    "basis",
    "rule_version",
    "idempotency_key",
    "payload_digest",
    "revision",
];

/// 网关配置（地址 + 凭据 + 超时，全来自环境变量）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayConfig {
    /// 网关根地址（末尾斜杠会被去掉）
    pub base_url: String,
    /// Bearer 凭据
    pub token: String,
    /// 超时秒数
    pub timeout_secs: u64,
}

impl GatewayConfig {
    /// 从环境变量读齐；缺项报 `CONFIG_MISSING` 并点名（**发请求之前**就拦下）
    pub fn load(src: &dyn EnvSource) -> Result<GatewayConfig, EngineError> {
        let partial = PartialConfig::load(src);
        partial.require(&GATEWAY_ENV_VARS)?;
        let timeout_secs = optional_usize(src, GATEWAY_TIMEOUT_ENV)?
            .unwrap_or(DEFAULT_GATEWAY_TIMEOUT_SECS as usize) as u64;
        Ok(GatewayConfig {
            base_url: partial
                .get("REGULATORY_REPORT_BASE_URL")
                .unwrap_or("")
                .trim_end_matches('/')
                .to_string(),
            token: partial.get("REGULATORY_REPORT_TOKEN").unwrap_or("").to_string(),
            timeout_secs,
        })
    }

    /// 上报提交端点
    pub fn submissions_endpoint(&self) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), SUBMISSIONS_PATH)
    }
}

/// 一次上报请求：载荷 + 它属于哪一行。
///
/// **行键不进报文** —— 报文字段全集固定 15 项；行键只是客户端侧的上下文，
/// 用来把网关的裁决挂回原表的那一行。
#[derive(Debug, Clone)]
pub struct SubmissionRequest {
    /// 行键（行内稳定标识，不进报文）
    pub row_uid: String,
    /// 上报载荷
    pub payload: SubmissionPayload,
}

/// 出站报文 body（字段全集固定 15 项）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubmissionBody {
    pub batch_ref: String,
    pub reporting_period: String,
    pub org_code: String,
    pub indicator_code: String,
    pub dept_code: String,
    pub business_key: String,
    /// 分子（合法数值 → JSON 数字；没填 → null；填坏了 → 原文字符串，**不推断成 0**）
    pub numerator: Value,
    pub denominator: Value,
    pub value: Value,
    pub unit: String,
    /// 官方依据出处
    pub basis: String,
    /// 这批结论按哪套口径算的
    pub rule_version: String,
    /// 幂等键（与 `Idempotency-Key` 头同值）
    pub idempotency_key: String,
    /// 载荷摘要
    pub payload_digest: String,
    /// 修订号（订正后 +1，业务键不变）
    pub revision: i64,
}

impl SubmissionBody {
    /// 由上报载荷逐字段装配（字段顺序与 [`OUTBOUND_FIELDS`] 一致）
    pub fn from_payload(payload: &SubmissionPayload) -> SubmissionBody {
        SubmissionBody {
            batch_ref: payload.batch_ref.clone(),
            reporting_period: payload.reporting_period.clone(),
            org_code: payload.org_code.clone(),
            indicator_code: payload.indicator_code.clone(),
            dept_code: payload.dept_code.clone(),
            business_key: payload.business_key.clone(),
            numerator: numeric_json(&payload.numerator),
            denominator: numeric_json(&payload.denominator),
            value: numeric_json(&payload.value),
            unit: payload.unit.clone(),
            basis: payload.basis.clone(),
            rule_version: payload.rule_version.clone(),
            idempotency_key: payload.idempotency_key.clone(),
            payload_digest: payload.payload_digest.clone(),
            revision: payload.revision,
        }
    }

    /// 序列化后的字段名集合（与 [`OUTBOUND_FIELDS`] 逐字对照用）
    pub fn field_names(&self) -> Vec<String> {
        let mut names: Vec<String> = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }
}

/// 数值字段 → JSON：合法数值给数字、空给 null、填坏了保留原文。
///
/// 填坏了不推断成 0 —— 0 是一个合法上报值，把「填坏了」当成 0 等于替科室编了数。
/// （填坏的行在 [`GatewayClient::submit`] 里就被拦下，这个分支只留给直接调
/// [`SubmissionBody::from_payload`] 的场合。）
fn numeric_json(field: &NumericField) -> Value {
    match field {
        NumericField::Empty => Value::Null,
        NumericField::Number(v) => serde_json::Number::from_f64(*v)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        NumericField::Raw(s) => Value::String(s.clone()),
    }
}

/// 本行的数值列有没有填坏的（`NumericField::Raw`）。
///
/// 返回人话说明（点名哪一列、原文是什么）；没有则返回 `None`。
fn broken_numeric(payload: &SubmissionPayload) -> Option<String> {
    let columns = [
        ("分子", &payload.numerator),
        ("分母", &payload.denominator),
        ("填报值", &payload.value),
    ];
    let broken: Vec<String> = columns
        .iter()
        .filter_map(|(name, field)| match field {
            NumericField::Raw(raw) => Some(format!("{} 填坏了（原文「{}」）", name, raw)),
            _ => None,
        })
        .collect();
    if broken.is_empty() {
        None
    } else {
        Some(format!("{}；该行不发往网关", broken.join("、")))
    }
}

/// 网关受理（`200`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAcceptance {
    /// 回执号
    pub receipt_id: String,
    /// 受理时间（网关给，原样保留）
    pub accepted_at: String,
    /// 网关给的状态
    pub status: String,
}

/// 网关退回（`4xx`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRejection {
    /// HTTP 状态码
    pub http_status: u16,
    /// 网关给的错误码
    pub error_code: String,
    /// 网关点名的字段（哪一列不合格）
    pub error_field: String,
    /// 网关给的原因
    pub message: String,
}

impl GatewayRejection {
    /// 写回原表「问题说明（含依据）」列的文本。
    ///
    /// 这一行为什么被退回、按哪个字段不合格，科室要能在自己的表里直接看到。
    pub fn row_note(&self) -> String {
        let mut parts = vec![format!("上报被退回：错误码 {}", self.error_code)];
        if !self.error_field.is_empty() {
            parts.push(format!("字段 {}", self.error_field));
        }
        if !self.message.is_empty() {
            parts.push(self.message.clone());
        }
        parts.join(" · ")
    }
}

/// 网关裁决的四种形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayOutcomeKind {
    /// `200`：网关受理
    Accepted(GatewayAcceptance),
    /// `4xx`：网关退回（错误码与字段名写回这一行，**不重试**）
    Rejected(GatewayRejection),
    /// `5xx` / 超时 / 连不上：可退避重试
    Retryable {
        /// 细分错误码（`GATEWAY_SERVER_ERROR` / `GATEWAY_TIMEOUT` / `GATEWAY_CONNECTION_FAILED`）
        error_code: ErrorCode,
        /// 人话说明
        message: String,
    },
    /// `200` 但回执结构不合法：**不可当成成功**（该行标「回执待人工核对」）
    Invalid {
        /// 细分错误码（`GATEWAY_RESPONSE_INVALID`）
        error_code: ErrorCode,
        /// 人话说明
        message: String,
    },
    /// 本行数据不合格，**请求根本没发出去**（不是网关的裁决）
    Blocked {
        /// 细分错误码（`INVALID_NUMERIC`）
        error_code: ErrorCode,
        /// 人话说明
        message: String,
    },
}

/// 一行上报的裁决结果：网关说了什么 + 这一行是谁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowOutcome {
    /// 行键（不进报文，只用来把裁决挂回这一行）
    pub row_uid: String,
    /// 裁决
    pub kind: GatewayOutcomeKind,
}

impl RowOutcome {
    /// 网关是否受理
    pub fn is_accepted(&self) -> bool {
        matches!(self.kind, GatewayOutcomeKind::Accepted(_))
    }

    /// 网关是否退回
    pub fn is_rejected(&self) -> bool {
        matches!(self.kind, GatewayOutcomeKind::Rejected(_))
    }

    /// 是否可退避重试
    pub fn is_retryable(&self) -> bool {
        matches!(self.kind, GatewayOutcomeKind::Retryable { .. })
    }

    /// 回执是否不可用（200 但结构不合法）
    pub fn is_invalid(&self) -> bool {
        matches!(self.kind, GatewayOutcomeKind::Invalid { .. })
    }

    /// 是否本行就没发出去（数据不合格，请求没发）
    pub fn is_blocked(&self) -> bool {
        matches!(self.kind, GatewayOutcomeKind::Blocked { .. })
    }

    /// 一句话结论（终端与导出用）
    pub fn zh(&self) -> String {
        match &self.kind {
            GatewayOutcomeKind::Accepted(a) => {
                format!("已上报 · 回执 {} · 受理时间 {}", a.receipt_id, a.accepted_at)
            }
            GatewayOutcomeKind::Rejected(r) => r.row_note(),
            GatewayOutcomeKind::Retryable { error_code, message } => format!(
                "待重试（{}）：{}",
                error_code.as_str(),
                message
            ),
            GatewayOutcomeKind::Invalid { error_code, message } => {
                format!("回执待人工核对（{}）：{}", error_code.as_str(), message)
            }
            GatewayOutcomeKind::Blocked { error_code, message } => {
                format!("未发出（{}）：{}", error_code.as_str(), message)
            }
        }
    }
}

/// 上报网关出站客户端。
///
/// **只有这一处**出现网关协议细节（路径、头、报文字段、状态码语义）；
/// 业务层只见 [`GatewayOutcomeKind`]。
pub struct GatewayClient {
    config: GatewayConfig,
    http: reqwest::blocking::Client,
}

/// 调试输出**不打印凭据** —— 报错信息会被写进日志与终端。
impl std::fmt::Debug for GatewayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayClient")
            .field("base_url", &self.config.base_url)
            .field("token", &"（已设置，不显示）")
            .field("timeout_secs", &self.config.timeout_secs)
            .finish()
    }
}

impl GatewayClient {
    /// 用已装配好的配置建客户端
    pub fn new(config: GatewayConfig) -> Result<GatewayClient, EngineError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .map_err(|e| {
                EngineError::new(
                    ErrorCode::GatewayConnectionFailed,
                    format!("创建上报网关 HTTP 客户端失败：{}", e),
                )
            })?;
        Ok(GatewayClient { config, http })
    }

    /// 从环境变量装配（凭据与超时全在环境变量里；缺项即报错，不发请求）
    pub fn from_env(src: &dyn EnvSource) -> Result<GatewayClient, EngineError> {
        Ok(GatewayClient::new(GatewayConfig::load(src)?)?)
    }

    /// 当前配置（只读）
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// 上报提交端点
    pub fn endpoint(&self) -> String {
        self.config.submissions_endpoint()
    }

    /// 提交一行上报。
    ///
    /// 返回**裁决**而不是成功/失败：HTTP 层的失败（5xx / 超时 / 连不上）也会被
    /// 分类成可重试的裁决，绝不静默改成「已上报」。
    ///
    /// 发请求**之前**先看本行数值是否填坏了：填坏了就**不发**（[`GatewayOutcomeKind::Blocked`]）
    /// —— 把「填坏了」当成空值或 0 发给网关，等于替科室编了一个数上去。
    pub fn submit(&self, request: &SubmissionRequest) -> RowOutcome {
        let row_uid = request.row_uid.clone();
        if let Some(problem) = broken_numeric(&request.payload) {
            return RowOutcome {
                row_uid,
                kind: GatewayOutcomeKind::Blocked {
                    error_code: ErrorCode::InvalidNumeric,
                    message: problem,
                },
            };
        }
        let body = SubmissionBody::from_payload(&request.payload);
        let idempotency_key = request.payload.idempotency_key.clone();
        let kind = match self.send(&body, &idempotency_key) {
            Ok(kind) => kind,
            Err(err) => GatewayOutcomeKind::Retryable {
                error_code: err.code,
                message: err.message,
            },
        };
        RowOutcome { row_uid, kind }
    }

    /// 发一次请求并给出裁决。
    ///
    /// `Err` **只**表示「可退避重试」的失败（5xx / 超时 / 连不上）；
    /// 回执结构不合法走 `Ok(GatewayOutcomeKind::Invalid)` —— 它既不是成功，
    /// 也不是重试能解决的（重试同样的载荷只会拿到同样的坏回执）。
    fn send(
        &self,
        body: &SubmissionBody,
        idempotency_key: &str,
    ) -> Result<GatewayOutcomeKind, EngineError> {
        let resp = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.config.token)
            .header(IDEMPOTENCY_HEADER, idempotency_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .map_err(map_send_error)?;

        let status = resp.status();
        let text = resp.text().unwrap_or_default();

        if status.is_success() {
            return Ok(match parse_acceptance(&text) {
                Ok(kind) => kind,
                Err(err) => GatewayOutcomeKind::Invalid {
                    error_code: err.code,
                    message: err.message,
                },
            });
        }
        if status.is_client_error() {
            // 4xx：网关明确拒绝 —— 不重试，把错误码与字段名带回这一行
            return Ok(GatewayOutcomeKind::Rejected(parse_rejection(
                status.as_u16(),
                &text,
            )));
        }
        // 5xx 及其它非预期状态：服务端侧问题，可退避重试
        Err(EngineError::at(
            ErrorCode::GatewayServerError,
            format!(
                "上报网关返回服务端错误（HTTP {}）：{}",
                status.as_u16(),
                snippet(&text)
            ),
            SUBMISSIONS_PATH,
        ))
    }
}

/// 解析 `200` 回执：三个字段缺一不可，受理时间格式要对。
///
/// 缺字段**不当成成功** —— 当成成功就等于替网关编了一个回执号。
/// 受理时间格式不对同样不当成功：受理时间要落上报台账，
/// 「昨天下午大概那会儿」落不进台账，也说明网关那边没按协议回。
fn parse_acceptance(text: &str) -> Result<GatewayOutcomeKind, EngineError> {
    let raw: Value = serde_json::from_str(text).map_err(|e| {
        EngineError::new(
            ErrorCode::GatewayResponseInvalid,
            format!("上报网关返回的不是合法 JSON：{}", e),
        )
    })?;
    let receipt_id = raw
        .get("receipt_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let accepted_at = raw
        .get("accepted_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let status = raw
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if receipt_id.is_empty() || accepted_at.is_empty() || status.is_empty() {
        return Err(EngineError::new(
            ErrorCode::GatewayResponseInvalid,
            format!(
                "上报网关回执缺字段（receipt_id / accepted_at / status 至少缺一个）：{}",
                snippet(text)
            ),
        ));
    }
    if !crate::push::receipt::is_valid_accepted_at(&accepted_at) {
        return Err(EngineError::new(
            ErrorCode::GatewayResponseInvalid,
            format!(
                "上报网关回执的受理时间格式不合法（原文「{}」）：应为 2026-09-06 14:30:00 或 2026-09-06T14:30:00+08:00",
                accepted_at
            ),
        ));
    }
    Ok(GatewayOutcomeKind::Accepted(GatewayAcceptance {
        receipt_id,
        accepted_at,
        status,
    }))
}

/// 解析 `4xx` 退回：网关给了错误码与字段名就用网关的，没给就退回 HTTP 状态码。
fn parse_rejection(http_status: u16, text: &str) -> GatewayRejection {
    let raw: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let pick = |key: &str| -> String {
        raw.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let error_code = pick("error_code");
    let error_field = pick("error_field");
    let message = pick("message");
    GatewayRejection {
        http_status,
        error_code: if error_code.is_empty() {
            format!("HTTP_{}", http_status)
        } else {
            error_code
        },
        error_field,
        message: if message.is_empty() {
            snippet(text)
        } else {
            message
        },
    }
}

/// 发送期错误 → 错误码（超时 / 连不上分开，不塌成一个通用错）
fn map_send_error(e: reqwest::Error) -> EngineError {
    if e.is_timeout() {
        EngineError::new(
            ErrorCode::GatewayTimeout,
            format!("上报网关调用超时：{}", e),
        )
    } else if e.is_connect() {
        EngineError::new(
            ErrorCode::GatewayConnectionFailed,
            format!("上报网关连接失败：{}", e),
        )
    } else if e.is_body() || e.is_decode() {
        EngineError::new(
            ErrorCode::GatewayResponseInvalid,
            format!("读取上报网关响应失败：{}", e),
        )
    } else {
        EngineError::new(
            ErrorCode::GatewayConnectionFailed,
            format!("上报网关调用失败：{}", e),
        )
    }
}

/// 响应正文摘要（只取前 400 字，够看又不刷屏）
fn snippet(text: &str) -> String {
    text.chars().take(400).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MapEnv;
    use crate::push::idempotency::{PayloadInput, SubmissionPayload};

    fn payload() -> SubmissionPayload {
        SubmissionPayload::new(PayloadInput {
            batch_ref: "batch-a",
            org_code: "ORG-001",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: "BK-01",
            numerator: NumericField::Number(980.0),
            denominator: NumericField::Number(1000.0),
            value: NumericField::Number(98.0),
            unit: "%",
            basis: "国卫办医函〔2021〕28号 附件 指标七",
            rule_version: "2021.1",
        })
    }

    #[test]
    fn 报文字段全集固定十五项() {
        let body = SubmissionBody::from_payload(&payload());
        let mut expected: Vec<String> = OUTBOUND_FIELDS.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(body.field_names(), expected);
        assert_eq!(OUTBOUND_FIELDS.len(), 15);
    }

    #[test]
    fn 端点按网关根地址拼出固定路径() {
        let cfg = GatewayConfig {
            base_url: "https://gateway.internal.example/".to_string(),
            token: "t".to_string(),
            timeout_secs: 5,
        };
        assert_eq!(
            cfg.submissions_endpoint(),
            "https://gateway.internal.example/api/v1/regulatory-submissions"
        );
    }

    #[test]
    fn 缺凭据时报错并点名且不发请求() {
        let mut env = MapEnv::new();
        env.set("REGULATORY_REPORT_BASE_URL", "https://gateway.internal.example");
        let err = GatewayConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigMissing);
        assert!(err.message.contains("REGULATORY_REPORT_TOKEN"));
        assert!(err.location.unwrap().contains("REGULATORY_REPORT_TOKEN"));
    }

    #[test]
    fn 超时取值不合法时报配置错误() {
        let mut env = MapEnv::new();
        env.set("REGULATORY_REPORT_BASE_URL", "https://gateway.internal.example");
        env.set("REGULATORY_REPORT_TOKEN", "t");
        env.set(GATEWAY_TIMEOUT_ENV, "soon");
        let err = GatewayConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(err.location.unwrap().contains(GATEWAY_TIMEOUT_ENV));
    }

    #[test]
    fn 超时未设时用默认值() {
        let mut env = MapEnv::new();
        env.set("REGULATORY_REPORT_BASE_URL", "https://gateway.internal.example");
        env.set("REGULATORY_REPORT_TOKEN", "t");
        let cfg = GatewayConfig::load(&env).unwrap();
        assert_eq!(cfg.timeout_secs, DEFAULT_GATEWAY_TIMEOUT_SECS);
    }

    #[test]
    fn 数值字段按数字空与原文分别落() {
        let body = SubmissionBody::from_payload(&payload());
        assert_eq!(body.numerator, serde_json::json!(980.0));
        assert_eq!(body.denominator, serde_json::json!(1000.0));
        assert_eq!(body.value, serde_json::json!(98.0));

        let mut broken = SubmissionPayload::new(PayloadInput {
            batch_ref: "batch-a",
            org_code: "ORG-001",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: "BK-01",
            numerator: NumericField::Raw("填坏了".to_string()),
            denominator: NumericField::Empty,
            value: NumericField::Number(98.0),
            unit: "%",
            basis: "依据",
            rule_version: "2021.1",
        });
        broken.batch_ref = "batch-a".to_string();
        let body = SubmissionBody::from_payload(&broken);
        assert_eq!(body.numerator, serde_json::json!("填坏了"));
        assert_eq!(body.denominator, serde_json::Value::Null);
    }

    #[test]
    fn 回执缺字段时不当成成功() {
        let err = parse_acceptance(r#"{"receipt_id":"R-1"}"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::GatewayResponseInvalid);
    }

    #[test]
    fn 退回原因带错误码与字段名() {
        let rejection = parse_rejection(
            422,
            r#"{"error_code":"E_REPORTING_PERIOD","error_field":"reporting_period","message":"报告期格式不合法"}"#,
        );
        assert_eq!(rejection.error_code, "E_REPORTING_PERIOD");
        assert_eq!(rejection.error_field, "reporting_period");
        let note = rejection.row_note();
        assert!(note.contains("E_REPORTING_PERIOD"));
        assert!(note.contains("reporting_period"));
    }

    #[test]
    fn 退回正文非标准时退回状态码() {
        let rejection = parse_rejection(400, "不是 JSON");
        assert_eq!(rejection.error_code, "HTTP_400");
        assert!(!rejection.message.is_empty());
    }

    #[test]
    fn 数值填坏时点名哪一列() {
        assert!(broken_numeric(&payload()).is_none());

        let mut broken = payload();
        broken.denominator = NumericField::Raw("N/A".to_string());
        let note = broken_numeric(&broken).unwrap();
        assert!(note.contains("分母"));
        assert!(note.contains("N/A"));

        let mut two = payload();
        two.numerator = NumericField::Raw("甲".to_string());
        two.value = NumericField::Raw("乙".to_string());
        let note = broken_numeric(&two).unwrap();
        assert!(note.contains("分子"));
        assert!(note.contains("填报值"));
    }
}
