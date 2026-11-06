//! 模型响应的结构化校验。
//!
//! # 为什么模型响应要单独校验一层
//!
//! 模型返回的是**一段文本**，里面可能夹带解释、可能字段名不对、可能把两行搞反、
//! 可能编出一个我们根本没问过的行键。这些都不是「模型能力不够」的问题，
//! 是「文本不是结构」的问题 —— 把它当结构收下来，错误会一路流到业务表里。
//!
//! 所以规则很简单：**校验不过，整次调用不可用**。
//! 不做「部分采用」（挑几条能用的收下），也不做「修补」（把缺的字段补上默认值）。
//! 修补出来的结果看着能用，实际是谁补的没人知道 —— 这类错在半年后查起来最费劲。
//!
//! 校验项：
//!
//! | 项 | 要求 | 不通过的后果 |
//! |---|---|---|
//! | 外形 | 合法 JSON 对象，`items` 是数组 | 整次调用作废 |
//! | 行键 | 左右行键都必须在**本次请求问过的行**里 | 整次调用作废（防编造） |
//! | 左右 | 左 ≠ 右 | 整次调用作废 |
//! | 归并类型 | `same_indicator` / `duplicate_record` | 整次调用作废 |
//! | 置信度 | 0.0–1.0 的有限数 | 整次调用作废 |
//! | 理由 | 非空文本 | 整次调用作废 |
//! | 依据 | 至少一条非空文本 | 整次调用作废 |
//!
//! 只有一种情况不整次作废：**同一对行在响应里出现多次** —— 那是模型重复，
//! 不是模型说错，保留置信度更高的一条即可，并记下丢了几条（不静默丢）。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{EngineError, ErrorCode};

/// 归并类型：两个名称是不是同一件事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeKind {
    /// 同一指标，但未必是同一笔记录（指标名写法不同，业务主键也不同）
    SameIndicator,
    /// 同一笔记录被填了两次
    DuplicateRecord,
}

impl MergeKind {
    /// 机器可读取值
    pub fn as_str(self) -> &'static str {
        match self {
            MergeKind::SameIndicator => "same_indicator",
            MergeKind::DuplicateRecord => "duplicate_record",
        }
    }

    /// 全部取值（错误提示里列给端点看）
    pub const ALL: [MergeKind; 2] = [MergeKind::SameIndicator, MergeKind::DuplicateRecord];

    /// 解析端点给的取值（大小写不敏感，容许首尾空白）
    pub fn parse(raw: &str) -> Option<MergeKind> {
        let norm = raw.trim().to_ascii_lowercase();
        MergeKind::ALL.into_iter().find(|k| k.as_str() == norm)
    }

    /// 中文说明（给人看）
    pub fn zh(self) -> &'static str {
        match self {
            MergeKind::SameIndicator => "同一指标（未必是同一笔记录）",
            MergeKind::DuplicateRecord => "同一笔记录被填了两次",
        }
    }
}

/// 一条归并结论（模型给的一条）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeItem {
    /// 左侧行键（必须是我们问过的行键）
    pub left_row_key: String,
    /// 右侧行键
    pub right_row_key: String,
    /// 归并类型
    pub kind: MergeKind,
    /// 置信度 0.0–1.0
    pub confidence: f64,
    /// 判断理由（中文）
    pub reason: String,
    /// 判断依据（可核对的事实，如「两处指标名都含『感染』且报告期相同」）
    pub evidence: Vec<String>,
}

/// 一次调用的用量元数据（审计用，见 `regval` 的调用审计表）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// 一次归并调用的结果（**已通过结构校验**）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeResponse {
    /// 归并结论
    pub items: Vec<MergeItem>,
    /// 同一对行重复出现而被合并掉的条数（不静默丢：计数可见）
    #[serde(default)]
    pub dropped_duplicate_pairs: usize,
    /// 端点回报的模型名（审计用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    /// token 用量（审计用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
}

impl MergeResponse {
    /// 空结果（没有一对行被判为同义）
    pub fn empty() -> MergeResponse {
        MergeResponse {
            items: Vec::new(),
            dropped_duplicate_pairs: 0,
            served_model: None,
            usage: None,
        }
    }

    /// 置信度不低于给定阈值的结论
    pub fn confident_items(&self, threshold: f64) -> Vec<&MergeItem> {
        self.items
            .iter()
            .filter(|i| i.confidence >= threshold)
            .collect()
    }
}

/// 响应不合法：整次调用不可用。
fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::ModelResponseInvalid, message)
}

/// 解析并校验模型响应正文。
///
/// `known_row_keys` 是本次请求问过的行键集合 —— 响应里出现集合外的键，
/// 说明模型编了行（或我们发错了请求），整次调用作废。
pub fn parse_merge_response(
    raw: &str,
    known_row_keys: &BTreeSet<String>,
) -> Result<MergeResponse, EngineError> {
    let raw = strip_code_fence(raw);
    let value: Value =
        serde_json::from_str(raw).map_err(|e| invalid(format!("模型响应不是合法 JSON：{}", e)))?;
    let items = value
        .get("items")
        .ok_or_else(|| invalid("模型响应缺 items 字段（期望形如 {\"items\":[...]} 的对象）"))?;
    let arr = items
        .as_array()
        .ok_or_else(|| invalid("模型响应的 items 不是数组"))?;

    let mut parsed: Vec<MergeItem> = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let at = format!("第 {} 条结果", idx + 1);
        let obj = item
            .as_object()
            .ok_or_else(|| invalid(format!("{}不是对象", at)))?;

        let left_row_key = require_str(obj, "left_row_key", &at)?;
        let right_row_key = require_str(obj, "right_row_key", &at)?;

        if left_row_key == right_row_key {
            return Err(invalid(format!("{}的左右行键相同", at)));
        }
        for (field, k) in [
            ("left_row_key", &left_row_key),
            ("right_row_key", &right_row_key),
        ] {
            if !known_row_keys.contains(k.as_str()) {
                return Err(invalid(format!(
                    "{}的 {} 指向我们没问过的行（{}，模型编了一个行键）",
                    at, field, k
                )));
            }
        }

        let kind_raw = require_str(obj, "kind", &at)?;
        let kind = MergeKind::parse(&kind_raw).ok_or_else(|| {
            invalid(format!(
                "{}的 kind 取值不认识：{}（只接受 {}）",
                at,
                kind_raw,
                MergeKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ))
        })?;

        let confidence = obj
            .get("confidence")
            .ok_or_else(|| invalid(format!("{}缺 confidence", at)))?
            .as_f64()
            .ok_or_else(|| invalid(format!("{}的 confidence 不是数值", at)))?;
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err(invalid(format!(
                "{}的 confidence 超出 0.0–1.0 范围：{}",
                at, confidence
            )));
        }

        let reason = require_str(obj, "reason", &at)?;

        let evidence_raw = obj
            .get("evidence")
            .ok_or_else(|| invalid(format!("{}缺 evidence", at)))?;
        let evidence_arr = evidence_raw
            .as_array()
            .ok_or_else(|| invalid(format!("{}的 evidence 不是数组", at)))?;
        let mut evidence = Vec::with_capacity(evidence_arr.len());
        for (i, e) in evidence_arr.iter().enumerate() {
            let s = e
                .as_str()
                .ok_or_else(|| invalid(format!("{}的 evidence 第 {} 项不是文本", at, i + 1)))?;
            if s.trim().is_empty() {
                return Err(invalid(format!(
                    "{}的 evidence 第 {} 项是空文本",
                    at,
                    i + 1
                )));
            }
            evidence.push(s.to_string());
        }
        if evidence.is_empty() {
            return Err(invalid(format!(
                "{}的 evidence 是空数组（模型没给出依据）",
                at
            )));
        }

        parsed.push(MergeItem {
            left_row_key,
            right_row_key,
            kind,
            confidence,
            reason,
            evidence,
        });
    }

    // 同一对行重复出现：保留置信度更高的一条，计数留痕（不静默丢）
    let mut best: BTreeMap<(String, String), MergeItem> = BTreeMap::new();
    let mut dropped = 0usize;
    for item in parsed {
        let key = if item.left_row_key <= item.right_row_key {
            (item.left_row_key.clone(), item.right_row_key.clone())
        } else {
            (item.right_row_key.clone(), item.left_row_key.clone())
        };
        match best.get(&key) {
            Some(prev) if prev.confidence >= item.confidence => {
                dropped += 1;
            }
            Some(_) => {
                dropped += 1;
                best.insert(key, item);
            }
            None => {
                best.insert(key, item);
            }
        }
    }

    Ok(MergeResponse {
        items: best.into_values().collect(),
        dropped_duplicate_pairs: dropped,
        served_model: None,
        usage: None,
    })
}

/// 去掉模型偶尔加的 Markdown 代码块围栏。
///
/// 有些端点即使开了 JSON 模式也会把内容包在 ```json … ``` 里。
/// 围栏是**包装**不是内容 —— 剥掉它再解析；剥不掉就照旧报错，不做别的猜测。
fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest
        .strip_prefix("json")
        .or_else(|| rest.strip_prefix("JSON"))
        .unwrap_or(rest);
    let rest = rest.trim_start();
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// 取一个必填的非空文本字段
fn require_str(
    obj: &serde_json::Map<String, Value>,
    field: &str,
    at: &str,
) -> Result<String, EngineError> {
    let v = obj
        .get(field)
        .ok_or_else(|| invalid(format!("{}缺 {}", at, field)))?;
    let s = v
        .as_str()
        .ok_or_else(|| invalid(format!("{}的 {} 不是文本", at, field)))?;
    if s.trim().is_empty() {
        return Err(invalid(format!("{}的 {} 是空文本", at, field)));
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> BTreeSet<String> {
        let mut s = BTreeSet::new();
        s.insert("k1".to_string());
        s.insert("k2".to_string());
        s
    }

    fn good() -> String {
        r#"{"items":[{"left_row_key":"k1","right_row_key":"k2","kind":"same_indicator","confidence":0.82,"reason":"两处指标名指的都是医院感染例数","evidence":["左侧含『感染』","右侧含『感染』"]}]}"#.to_string()
    }

    #[test]
    fn 合规响应解析成功() {
        let r = parse_merge_response(&good(), &keys()).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].kind, MergeKind::SameIndicator);
        assert_eq!(r.items[0].confidence, 0.82);
        assert_eq!(r.items[0].evidence.len(), 2);
        assert_eq!(r.dropped_duplicate_pairs, 0);
    }

    #[test]
    fn 空数组是合法结果_不是错误() {
        let r = parse_merge_response(r#"{"items":[]}"#, &keys()).unwrap();
        assert!(r.items.is_empty());
    }

    #[test]
    fn 缺字段_类型不对_取值越界一律作废() {
        for bad in [
            r#"{"nope":[]}"#,
            r#"{"items":{}}"#,
            r#"{"items":[{"right_row_key":"k2","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
            r#"{"items":[{"left_row_key":"k1","right_row_key":"k2","kind":"unknown_kind","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
            r#"{"items":[{"left_row_key":"k1","right_row_key":"k2","kind":"same_indicator","confidence":1.7,"reason":"r","evidence":["a"]}]}"#,
            r#"{"items":[{"left_row_key":"k1","right_row_key":"k2","kind":"same_indicator","confidence":0.5,"evidence":["a"]}]}"#,
            r#"{"items":[{"left_row_key":"k1","right_row_key":"k2","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":[]}]}"#,
            r#"{"items":[{"left_row_key":"k1","right_row_key":"k1","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
        ] {
            let err = parse_merge_response(bad, &keys()).unwrap_err();
            assert_eq!(err.code, ErrorCode::ModelResponseInvalid, "输入：{}", bad);
        }
    }

    #[test]
    fn 模型编了没问过的行键_整次作废() {
        let bad = r#"{"items":[{"left_row_key":"k1","right_row_key":"k9","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":["a"]}]}"#;
        let err = parse_merge_response(bad, &keys()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ModelResponseInvalid);
        assert!(err.message.contains("k9"));
    }

    #[test]
    fn 同一对行重复出现_保留置信度更高的一条_计数留痕() {
        let raw = r#"{"items":[
            {"left_row_key":"k1","right_row_key":"k2","kind":"same_indicator","confidence":0.4,"reason":"r1","evidence":["a"]},
            {"left_row_key":"k2","right_row_key":"k1","kind":"duplicate_record","confidence":0.9,"reason":"r2","evidence":["b"]}
        ]}"#;
        let r = parse_merge_response(raw, &keys()).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].confidence, 0.9);
        assert_eq!(r.dropped_duplicate_pairs, 1);
    }

    #[test]
    fn 不是合法_json时报响应不合法() {
        let err = parse_merge_response("这不是 JSON", &keys()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ModelResponseInvalid);
    }

    #[test]
    fn 归并类型取值可解析() {
        assert_eq!(
            MergeKind::parse("same_indicator"),
            Some(MergeKind::SameIndicator)
        );
        assert_eq!(
            MergeKind::parse(" duplicate_record "),
            Some(MergeKind::DuplicateRecord)
        );
        assert_eq!(MergeKind::parse("other"), None);
    }
}
