//! 问题清单汇总：两个维度 + 校验结论取值规则。
//!
//! # 两个维度
//!
//! | 维度 | 回答谁的问题 | 落在哪 |
//! |---|---|---|
//! | 按行 | 「这一行有没有问题、是什么问题」 | 契约的 `rows[]`（三列取值）与举证包的每行条目 |
//! | 按问题类型 | 「这一批一共哪几类问题、各几项」 | 契约的 `summary.issue_count_by_kind` 与举证包的 `counts` |
//!
//! 两个维度由**同一份索引**（[`IssueIndex`]）算出 —— 不是两段各数一遍。
//! 各数一遍的话，迟早会出现「状态条说 3 项、清单里列出 4 项」，
//! 经办人先要怀疑的是工具，不是自己的表。
//!
//! # 校验结论取值只有一处
//!
//! 有阻断级问题 → 「不通过」；只有提示级 → 「有提示」；没有命中 → 「通过」。
//! 契约写回与举证包共用 [`conclusion_of`] —— 两处各写一套的话，
//! 「提示级算不算不通过」上迟早打架，届时报给科室的是两套说法。

use std::collections::BTreeMap;

use crate::rules::model::Issue;

/// 校验结论：无任何命中
pub const CONCLUSION_PASS: &str = "通过";
/// 校验结论：只有提示级问题
pub const CONCLUSION_HINT: &str = "有提示";
/// 校验结论：有阻断级问题
pub const CONCLUSION_BLOCKED: &str = "不通过";

/// 严重度：阻断级
pub const SEVERITY_BLOCKING: &str = "阻断";
/// 严重度：提示级
pub const SEVERITY_HINT: &str = "提示";

/// 按行归集的问题索引。
///
/// 一次建索引，两个维度都从它出（见模块文档）。
#[derive(Debug, Clone, Default)]
pub struct IssueIndex {
    /// 行键 → 该行命中的问题（按命中先后）
    by_row: BTreeMap<String, Vec<Issue>>,
}

impl IssueIndex {
    /// 按行归集问题。
    pub fn build(issues: &[Issue]) -> IssueIndex {
        let mut by_row: BTreeMap<String, Vec<Issue>> = BTreeMap::new();
        for issue in issues {
            by_row
                .entry(issue.row_uid.clone())
                .or_default()
                .push(issue.clone());
        }
        IssueIndex { by_row }
    }

    /// 某一行命中的问题；没有命中时是空切片（不是 `None` —— 「没命中」本身是信息）
    pub fn of_row(&self, row_uid: &str) -> &[Issue] {
        self.by_row
            .get(row_uid)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 有问题的行数（按行维度）
    pub fn rows_with_issues(&self) -> usize {
        self.by_row.values().filter(|v| !v.is_empty()).count()
    }

    /// 问题总数
    pub fn total(&self) -> usize {
        self.by_row.values().map(|v| v.len()).sum()
    }

    /// 按问题类型计数（按类型维度）
    pub fn count_by_kind(&self) -> BTreeMap<String, usize> {
        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        for list in self.by_row.values() {
            for issue in list {
                *out.entry(issue.issue_kind.clone()).or_insert(0) += 1;
            }
        }
        out
    }

    /// 按严重度计数
    pub fn count_by_severity(&self) -> BTreeMap<String, usize> {
        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        for list in self.by_row.values() {
            for issue in list {
                *out.entry(issue.severity.clone()).or_insert(0) += 1;
            }
        }
        out
    }

    /// 某一行的「问题类型」列取值：去重，按首次出现顺序（「必填缺失、枚举越界」）
    pub fn kinds_of_row(&self, row_uid: &str) -> String {
        let mut kinds: Vec<String> = Vec::new();
        for issue in self.of_row(row_uid) {
            if !kinds.contains(&issue.issue_kind) {
                kinds.push(issue.issue_kind.clone());
            }
        }
        kinds.join("、")
    }

    /// 某一行的「问题说明（含依据）」列取值：每条问题「说明；依据：依据原文」，多条用 ` / ` 连接
    pub fn detail_of_row(&self, row_uid: &str) -> String {
        self.of_row(row_uid)
            .iter()
            .map(|i| format!("{}；依据：{}", i.message, i.basis))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// 校验结论取值规则。
///
/// - 无命中 → 「通过」
/// - 有阻断级问题 → 「不通过」
/// - 只有提示级问题 → 「有提示」
pub fn conclusion_of(issues: &[Issue]) -> &'static str {
    if issues.is_empty() {
        return CONCLUSION_PASS;
    }
    if issues.iter().any(|i| i.severity == SEVERITY_BLOCKING) {
        return CONCLUSION_BLOCKED;
    }
    CONCLUSION_HINT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(row: &str, kind: &str, severity: &str) -> Issue {
        Issue {
            row_uid: row.into(),
            rule_id: format!("R-{}", kind),
            issue_kind: kind.into(),
            severity: severity.into(),
            message: format!("{}说明", kind),
            basis: "依据X".into(),
            issue_key: String::new(),
            missing_columns: Vec::new(),
            error_code: String::new(),
        }
    }

    #[test]
    fn 四类问题齐全时按类型计数正确() {
        let issues = vec![
            issue("u1", "必填缺失", SEVERITY_BLOCKING),
            issue("u2", "枚举越界", SEVERITY_BLOCKING),
            issue("u3", "单位不符", SEVERITY_BLOCKING),
            issue("u4", "疑似重复", SEVERITY_HINT),
        ];
        let index = IssueIndex::build(&issues);
        let by_kind = index.count_by_kind();
        assert_eq!(by_kind.get("必填缺失"), Some(&1));
        assert_eq!(by_kind.get("枚举越界"), Some(&1));
        assert_eq!(by_kind.get("单位不符"), Some(&1));
        assert_eq!(by_kind.get("疑似重复"), Some(&1));
        assert_eq!(by_kind.len(), 4, "不该多出第五类：{:?}", by_kind);
        assert_eq!(index.total(), 4);
        assert_eq!(index.rows_with_issues(), 4);
    }

    #[test]
    fn 结论取值规则三档都对() {
        assert_eq!(conclusion_of(&[]), CONCLUSION_PASS);
        assert_eq!(
            conclusion_of(&[issue("u1", "疑似重复", SEVERITY_HINT)]),
            CONCLUSION_HINT
        );
        assert_eq!(
            conclusion_of(&[
                issue("u1", "疑似重复", SEVERITY_HINT),
                issue("u1", "必填缺失", SEVERITY_BLOCKING),
            ]),
            CONCLUSION_BLOCKED
        );
    }

    #[test]
    fn 行维度与类型维度出自同一份索引() {
        let issues = vec![
            issue("u1", "必填缺失", SEVERITY_BLOCKING),
            issue("u1", "枚举越界", SEVERITY_BLOCKING),
            issue("u2", "疑似重复", SEVERITY_HINT),
        ];
        let index = IssueIndex::build(&issues);
        // 两维度相加自洽：类型维度总数 == 问题总数
        let by_kind: usize = index.count_by_kind().values().sum();
        assert_eq!(by_kind, index.total());
        // 行维度：u1 命中 2 条，u2 命中 1 条，u3 没有
        assert_eq!(index.of_row("u1").len(), 2);
        assert_eq!(index.of_row("u2").len(), 1);
        assert_eq!(index.of_row("u3").len(), 0);
        assert_eq!(index.rows_with_issues(), 2);
    }

    #[test]
    fn 问题类型去重且按首次出现顺序() {
        let issues = vec![
            issue("u1", "必填缺失", SEVERITY_BLOCKING),
            issue("u1", "枚举越界", SEVERITY_BLOCKING),
            issue("u1", "必填缺失", SEVERITY_BLOCKING),
        ];
        let index = IssueIndex::build(&issues);
        assert_eq!(index.kinds_of_row("u1"), "必填缺失、枚举越界");
    }
}
