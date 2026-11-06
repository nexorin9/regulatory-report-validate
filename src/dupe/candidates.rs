//! 第四级（语义归并）要问模型的那一批行对。
//!
//! # 问哪些行对
//!
//! 语义归并这一级问的是「这两行是不是同一件事」—— 但**不是所有行对都值得问**：
//!
//! - 前三级（确定性键 / 归一化 / 编辑距离）已经认下的对，不问 —— 证据更硬的那一级说了算；
//! - 不同指标编号、不同科室、不同报告期的行，本来就是两件事，不问；
//! - 值完全一样的行，确定性键那一级就抓到了，不问。
//!
//! 所以候选集是：**同一个跨表匹配键（指标编号 + 科室代码 + 报告期）下，
//! 前三级没认下的行对**。
//!
//! # 为什么要有上限
//!
//! 一个匹配键下理论上可能有成百上千行（整表粘贴漏了筛选就是这个后果），
//! 两两配对是平方级的量，发给模型既慢又贵，而且模型也答不动那么多。
//! 所以这里有两条上限（组内行数上限、候选对总数上限），**超出的部分计数留痕**
//! —— 不静默截断：截了多少写在审计与结果契约里，读的人知道模型这一级没看全。

use std::collections::{BTreeMap, BTreeSet};

use crate::dupe::{pair_key, SuspectedDuplicate};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::key;
use crate::ingest::SourceRow;
use crate::model::gateway::{MergeCandidate, MergePair};

/// 控制「单次提交给模型多少对候选」的环境变量
pub const MAX_PAIRS_ENV: &str = "REGVAL_MODEL_MAX_PAIRS";

/// 候选集上限（读环境变量；非法值报明确错误码，不静默兜底）
///
/// 只有「对数上限」可调：组内行数上限调大容易让请求平方级膨胀，
/// 那是结构问题，不留给运维随手改。
pub fn limits_from_env() -> Result<CandidateLimits, EngineError> {
    let mut limits = CandidateLimits::default();
    let Some(raw) = std::env::var(MAX_PAIRS_ENV).ok() else {
        return Ok(limits);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(limits);
    }
    limits.max_pairs = trimmed.parse::<usize>().map_err(|_| {
        EngineError::at(
            ErrorCode::ConfigInvalid,
            format!("环境变量 {} 不是合法的非负整数：{}", MAX_PAIRS_ENV, trimmed),
            MAX_PAIRS_ENV,
        )
    })?;
    Ok(limits)
}

/// 候选集的上限（防止平方级膨胀）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateLimits {
    /// 单个匹配键组内最多取多少行（超出的整组跳过并计数）
    pub max_group_rows: usize,
    /// 最多提交多少对给模型（超出的不再提交并计数）
    pub max_pairs: usize,
}

impl Default for CandidateLimits {
    fn default() -> CandidateLimits {
        CandidateLimits {
            max_group_rows: 12,
            max_pairs: 40,
        }
    }
}

/// 候选集的构造结果（附带可数的截断情况）
#[derive(Debug, Clone, Default)]
pub struct SemanticCandidateSet {
    /// 要问模型的行对（只含最小必要字段）
    pub pairs: Vec<MergePair>,
    /// 参与分组的匹配键组数
    pub groups: usize,
    /// 组内行数合计（参与配对的）
    pub group_rows: usize,
    /// 已被前三级认下、不再问模型的对数
    pub excluded_claimed: usize,
    /// 组内行数超限被整组跳过的组数
    pub skipped_large_groups: usize,
    /// 因候选对总数超限而没提交的对数
    pub skipped_over_limit: usize,
    /// 考虑过的行对总数（含被排除与被截断的）
    pub considered_pairs: usize,
}

impl SemanticCandidateSet {
    /// 候选对数
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// 是否有被截断的部分（有就说明模型这一级没看全）
    pub fn truncated(&self) -> bool {
        self.skipped_large_groups > 0 || self.skipped_over_limit > 0
    }

    /// 一行中文摘要（审计与结果契约里用）
    pub fn summary_line(&self) -> String {
        let mut parts = vec![format!("候选 {} 对", self.pairs.len())];
        if self.excluded_claimed > 0 {
            parts.push(format!("前三级已认下 {} 对", self.excluded_claimed));
        }
        if self.skipped_large_groups > 0 {
            parts.push(format!("整组跳过 {} 组", self.skipped_large_groups));
        }
        if self.skipped_over_limit > 0 {
            parts.push(format!("超出上限未提交 {} 对", self.skipped_over_limit));
        }
        parts.join(" · ")
    }
}

/// 构造要问模型的候选行对。
///
/// `claimed` 是前三级已经认下的候选对（逐级累加的结果）—— 已被认下的不再问。
pub fn residual_candidates(
    rows: &[SourceRow],
    claimed: &[SuspectedDuplicate],
    limits: &CandidateLimits,
) -> SemanticCandidateSet {
    let mut out = SemanticCandidateSet::default();

    // 前三级已认下的行对（规范键：左右行键排序后拼接）
    let claimed_keys: BTreeSet<String> = claimed.iter().map(pair_key).collect();

    // 按跨表匹配键分组（指标编号 + 科室代码 + 报告期）
    let mut by_match: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        if !key::is_bindable(row) {
            // 键要素不全的行不参与：它缺的是必填列，该报的是「必填缺失」
            continue;
        }
        by_match.entry(key::match_key(row)).or_default().push(i);
    }
    out.groups = by_match.len();

    for members in by_match.values() {
        if members.len() < 2 {
            continue;
        }
        if members.len() > limits.max_group_rows {
            // 整组太大：跳过并计数（不静默截断）
            out.skipped_large_groups += 1;
            continue;
        }
        out.group_rows += members.len();
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                out.considered_pairs += 1;
                let left = &rows[members[a]];
                let right = &rows[members[b]];
                let left_key = key::row_uid(left);
                let right_key = key::row_uid(right);
                // 规范键：左右行键排序后拼接（与漏斗去重用的是同一套）
                let pk = if left_key <= right_key {
                    format!("{}|{}", left_key, right_key)
                } else {
                    format!("{}|{}", right_key, left_key)
                };
                if claimed_keys.contains(&pk) {
                    out.excluded_claimed += 1;
                    continue;
                }
                if out.pairs.len() >= limits.max_pairs {
                    out.skipped_over_limit += 1;
                    continue;
                }
                out.pairs.push(MergePair {
                    left: MergeCandidate::from_row(left),
                    right: MergeCandidate::from_row(right),
                });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Field;

    fn row(ns: &str, no: usize, name: &str, business_key: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-HR-01".to_string());
        values.insert(Field::IndicatorName, name.to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::DeptName, "院感科".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, business_key.to_string());
        values.insert(Field::Value, "1".to_string());
        SourceRow {
            source_ns: ns.to_string(),
            source_file: format!("{}.csv", ns),
            row_no: no,
            dup_ordinal: 0,
            values,
        }
    }

    #[test]
    fn 同一匹配键下的行对进候选集() {
        let rows = vec![
            row("表一", 1, "医院感染例数", "BK-1"),
            row("表二", 2, "院感例数", "BK-2"),
        ];
        let set = residual_candidates(&rows, &[], &CandidateLimits::default());
        assert_eq!(set.pair_count(), 1);
        assert_eq!(set.groups, 1);
        assert_eq!(set.excluded_claimed, 0);
        assert!(!set.truncated());
        // 候选只带四项最小必要字段
        let candidate = serde_json::to_value(&set.pairs[0].left).unwrap();
        let keys: Vec<&str> = candidate
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(keys.len(), 4);
    }

    #[test]
    fn 前三级已认下的对不再问模型() {
        let rows = vec![
            row("表一", 1, "医院感染例数", "BK-1"),
            row("表二", 2, "院感例数", "BK-2"),
        ];
        let left = key::row_uid(&rows[0]);
        let right = key::row_uid(&rows[1]);
        let (l, r) = if left <= right {
            (left.clone(), right.clone())
        } else {
            (right.clone(), left.clone())
        };
        let claimed = vec![SuspectedDuplicate {
            left_row_key: l,
            right_row_key: r,
            match_level: "exact".to_string(),
            basis: "取值完全一致".to_string(),
            business_key: "k".to_string(),
            left_reference: "表一 第 1 行".to_string(),
            right_reference: "表二 第 2 行".to_string(),
            cross_table: true,
            state: "疑似重复".to_string(),
            left_original: String::new(),
            right_original: String::new(),
            left_normalized: String::new(),
            right_normalized: String::new(),
            normalize_detail: String::new(),
            distance: None,
            distance_detail: String::new(),
            source: "deterministic".to_string(),

            semantic_kind: String::new(),
            semantic_confidence: None,
            semantic_reason: String::new(),
            semantic_evidence: Vec::new(),
            mergeable: crate::dupe::default_mergeable(),
        }];
        let set = residual_candidates(&rows, &claimed, &CandidateLimits::default());
        assert_eq!(set.pair_count(), 0);
        assert_eq!(set.excluded_claimed, 1);
        assert_eq!(set.considered_pairs, 1);
    }

    #[test]
    fn 不同匹配键的行不成对() {
        let mut other = row("表二", 2, "院感例数", "BK-2");
        other
            .values
            .insert(Field::ReportingPeriod, "2026-Q2".to_string());
        let rows = vec![row("表一", 1, "医院感染例数", "BK-1"), other];
        let set = residual_candidates(&rows, &[], &CandidateLimits::default());
        assert_eq!(set.pair_count(), 0);
        assert_eq!(set.groups, 2);
    }

    #[test]
    fn 组太大与候选超限都计数留痕() {
        let mut rows = Vec::new();
        for i in 0..5 {
            rows.push(row(
                "表一",
                i + 1,
                &format!("指标{}", i),
                &format!("BK-{}", i),
            ));
        }
        let limits = CandidateLimits {
            max_group_rows: 3,
            max_pairs: 2,
        };
        let set = residual_candidates(&rows, &[], &limits);
        assert_eq!(set.skipped_large_groups, 1, "整组超限应被跳过并计数");
        assert_eq!(set.pairs.len(), 0);
        assert!(set.truncated());

        // 组不超限但候选对总数超限
        let mut rows = Vec::new();
        for i in 0..3 {
            rows.push(row(
                "表一",
                i + 1,
                &format!("指标{}", i),
                &format!("BK-{}", i),
            ));
        }
        let limits = CandidateLimits {
            max_group_rows: 10,
            max_pairs: 2,
        };
        let set = residual_candidates(&rows, &[], &limits);
        assert_eq!(set.pairs.len(), 2);
        assert_eq!(set.considered_pairs, 3);
        assert_eq!(set.skipped_over_limit, 1);
        assert!(set.truncated());
    }

    #[test]
    fn 键要素不全的行不参与配对() {
        let mut broken = row("表一", 1, "医院感染例数", "BK-1");
        broken.values.insert(Field::IndicatorCode, String::new());
        let rows = vec![broken, row("表二", 2, "院感例数", "BK-2")];
        let set = residual_candidates(&rows, &[], &CandidateLimits::default());
        assert_eq!(set.pair_count(), 0);
    }
}
