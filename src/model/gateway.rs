//! provider 中立的模型入口（trait 与请求侧数据）。
//!
//! # 为什么要 trait
//!
//! 业务层（[`crate::dupe`] 的第四级）只该知道「有个东西能判语义等价」，
//! 不该知道对面是 HTTP 客户端还是本地实现，更不该知道厂商是谁。
//! 因此协议细节（端点路径、请求体形状、错误分类）全部关在实现里，
//! 本模块只放**请求侧数据**与**抽象**。
//!
//! 本模块**不引入任何 HTTP 依赖**（源码里没有 `reqwest`）—— 这一点有静态用例盯着：
//! 只有 `src/model/openai.rs` 可以出现 HTTP 客户端。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::errors::EngineError;
use crate::ingest::key;
use crate::ingest::SourceRow;
use crate::model::schema::MergeResponse;

/// 请求侧的契约版本（模型层与业务层之间的 JSON 形状）。
///
/// 与结果契约（`regval.contract/1.0.0`）是**两回事**：结果契约给表格插件看，
/// 这一版给模型请求体与响应体看。
pub const MODEL_REQUEST_SCHEMA_VERSION: &str = "regval.merge/1.0.0";

/// 一行送给模型的最小必要字段。
///
/// **只有四项**：行键、指标名称、科室名称、报告期。
///
/// - 不送填报值 / 分子 / 分母 / 业务主键：模型要判的是「这两个说法是不是同一件事」，
///   数值对不对是引擎的事（口径规则与实体绑定层各管各的），不该让模型顺手判。
/// - 不送任何患者可识别明细：上报表本身是科室级汇总表，没有患者明细，
///   这一层更不会去拼装它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeCandidate {
    /// 行键（与结果契约里的行键同一套，模型回的键必须是我们问过的键）
    pub row_key: String,
    /// 指标名称（要判同义与否的就是它）
    pub indicator_name: String,
    /// 科室名称
    pub dept_name: String,
    /// 报告期
    pub reporting_period: String,
}

impl MergeCandidate {
    /// 从一行已映射的业务字段取最小必要信息。
    ///
    /// 行键取 [`key::row_uid`] —— 与漏斗、实体绑定层、结果契约用的是同一套，
    /// 模型回来的键才对得上。
    pub fn from_row(row: &SourceRow) -> MergeCandidate {
        MergeCandidate {
            row_key: key::row_uid(row),
            indicator_name: text(row, crate::ingest::Field::IndicatorName),
            dept_name: text(row, crate::ingest::Field::DeptName),
            reporting_period: text(row, crate::ingest::Field::ReportingPeriod),
        }
    }
}

/// 取一个字段的去空白取值（空串就是空串，不编造占位）
fn text(row: &SourceRow, field: crate::ingest::Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

/// 一对候选行（要问模型「这两个是不是同一件事」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePair {
    pub left: MergeCandidate,
    pub right: MergeCandidate,
}

/// 一次归并请求：若干对候选行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRequest {
    /// 契约版本（请求体里带着，便于端点侧排障）
    pub schema_version: String,
    /// 待判定的候选对
    pub pairs: Vec<MergePair>,
}

impl MergeRequest {
    /// 用给定的候选对建一个请求
    pub fn new(pairs: Vec<MergePair>) -> MergeRequest {
        MergeRequest {
            schema_version: MODEL_REQUEST_SCHEMA_VERSION.to_string(),
            pairs,
        }
    }

    /// 本次请求里出现过的全部行键 —— **模型回的键必须落在这个集合里**。
    ///
    /// 这是防「模型编了个不存在的行」的那道闸：不在集合里的键一律判为
    /// 响应不合法，整次调用作废。
    pub fn known_row_keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for pair in &self.pairs {
            keys.insert(pair.left.row_key.clone());
            keys.insert(pair.right.row_key.clone());
        }
        keys
    }

    /// 候选对涉及的行数（去重后）
    pub fn row_count(&self) -> usize {
        self.known_row_keys().len()
    }
}

/// 模型入口：provider 中立的抽象。
///
/// 业务层只依赖这个 trait；换协议、换端点、换厂商都不改业务层。
pub trait ModelGateway {
    /// 提交归并候选，返回**已通过结构校验**的归并结果。
    ///
    /// 返回 `Err` 时，这次调用不可用 —— 调用方不得拿未校验的结果去写任何业务表。
    fn merge_candidates(&self, request: &MergeRequest) -> Result<MergeResponse, EngineError>;

    /// 这一轮模型端点是什么状态（人话，进日志与对比表）
    fn describe(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Field;

    fn row(pairs: &[(Field, &str)]) -> SourceRow {
        let mut values = std::collections::BTreeMap::new();
        for (f, v) in pairs {
            values.insert(*f, v.to_string());
        }
        SourceRow {
            source_ns: "表一".to_string(),
            source_file: "表一.csv".to_string(),
            row_no: 1,
            dup_ordinal: 0,
            values,
        }
    }

    #[test]
    fn 候选只取四项最小必要字段() {
        let r = row(&[
            (Field::IndicatorName, " 医院感染例数 "),
            (Field::DeptName, "院感科"),
            (Field::ReportingPeriod, "2026-Q3"),
            (Field::Value, "12"),
            (Field::Numerator, "6"),
            (Field::BusinessKey, "BK-01"),
        ]);
        let c = MergeCandidate::from_row(&r);
        assert_eq!(c.indicator_name, "医院感染例数");
        assert_eq!(c.dept_name, "院感科");
        assert_eq!(c.reporting_period, "2026-Q3");
        assert!(!c.row_key.is_empty());
        // 序列化后的候选对象只有四个键 —— 数值与业务主键不外送
        let v = serde_json::to_value(&c).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys.len(), 4);
        assert!(keys.contains(&"row_key"));
        assert!(keys.contains(&"indicator_name"));
        assert!(keys.contains(&"dept_name"));
        assert!(keys.contains(&"reporting_period"));
    }

    #[test]
    fn 已知行键来自本次请求的两侧() {
        let mk = |k: &str| MergeCandidate {
            row_key: k.to_string(),
            indicator_name: "指标".to_string(),
            dept_name: "科室".to_string(),
            reporting_period: "2026-Q3".to_string(),
        };
        let req = MergeRequest::new(vec![MergePair {
            left: mk("k1"),
            right: mk("k2"),
        }]);
        let keys = req.known_row_keys();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains("k1"));
        assert!(keys.contains("k2"));
        assert_eq!(req.row_count(), 2);
    }

    #[test]
    fn 请求带契约版本() {
        let req = MergeRequest::new(Vec::new());
        assert_eq!(req.schema_version, MODEL_REQUEST_SCHEMA_VERSION);
    }
}
