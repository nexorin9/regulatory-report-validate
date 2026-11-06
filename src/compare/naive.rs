//! 验证 C · 轴一：朴素确定性基线（naive）。
//!
//! # 这是什么
//!
//! **大多数医院现在就是这么查重的**：在 Excel 里 `VLOOKUP` 一把，
//! 「指标编号 + 科室代码 + 报告期 + 业务主键」四段对上了，就当重复。
//! 除了键，别的什么都不看：
//!
//! - 不比取值 —— 同键但数字冲突的（97.0 与 97.5），基线也当重复报出来；
//! - 不做归一化 —— 全角 `％` 与半角 `%`、指标名称别名，一概不管；
//! - 不做近似 —— 名字差一个字不算；
//! - 不查规则语义 —— 不跑口径规则，也不问模型。
//!
//! # 为什么要留一个这么糙的基线
//!
//! 因为它就是**现状**。拿四级漏斗跟它比，才知道多出来的那几级值不值：
//! 如果漏斗比基线还差，那这几级就是白做的 —— 这种话只有在有对照数字的时候才能说。
//!
//! # 与产品主路径的关系
//!
//! **基线只活在对比里**：本模块的产出**不进结果契约、不进「疑似重复项」列、不推送**。
//! 它是尺子，不是功能。产品路径上的重复检测始终是四级漏斗。
//!
//! 所以本模块的候选对 `match_level` 取 [`MATCH_LEVEL_NAIVE`]（与漏斗四级的
//! `exact` / `normalize` / `distance` / `semantic` 区分开），
//! 且 `mergeable` 恒为 `false` —— 基线认下的对**一律不进人工确认流**。

use std::collections::BTreeMap;

use crate::dupe::{
    value_signature, SuspectedDuplicate, PAIR_EXPANSION_MAX, STATE_SUSPECTED,
};
use crate::ingest::key;
use crate::ingest::SourceRow;

/// 命中层级取值（朴素基线）—— 与漏斗四级的取值区分开
pub const MATCH_LEVEL_NAIVE: &str = "naive_key";

/// 基线判定依据（写清它有多糙）
pub const NAIVE_BASIS: &str = "朴素确定性基线（对照用）：只按「指标编号 + 科室代码 + 报告期 + 业务主键」四段完全一致即判重复，不比取值、不做归一化、不做近似、不查规则语义 —— 即 Excel VLOOKUP 一把的做法；本对只用于与四级漏斗做对比，不进人工确认流、不上报";

/// 基线的运行计数（可观测：对比表与结论里都能看到）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NaiveStats {
    /// 参与判定的行数（四段键齐全）
    pub eligible_rows: usize,
    /// 键要素不全、未参与判定的行数
    pub skipped_rows: usize,
    /// 键分组数（组内 ≥2 行才算一组）
    pub groups: usize,
    /// 候选对数
    pub pairs: usize,
    /// 其中「同键且取值完全一致」的对（这一类四级漏斗的第一级也会报）
    pub identical_pairs: usize,
    /// 其中「同键但取值冲突」的对（这一类四级漏斗不报 —— 归「待人工判定」）
    pub conflicting_pairs: usize,
    /// 大桶折叠掉的对数（不静默截断）
    pub collapsed_pairs: usize,
}

/// 基线的产出
#[derive(Debug, Clone, Default)]
pub struct NaiveOutcome {
    /// 候选对
    pub pairs: Vec<SuspectedDuplicate>,
    /// 运行计数
    pub stats: NaiveStats,
}

/// 朴素确定性基线：四段键相同即判重复。
///
/// 判定步骤：
///
/// 1. 按四段键（业务主键）分组 —— 键要素不全的行不参与（它缺的是必填列，
///    该报的是「必填缺失」，不该在这里再报一次重复）；
/// 2. 组内 ≥2 行 → 两两成对（桶过大时折成星形并计数，与漏斗第一级同一把尺子）；
/// 3. 每对记下「取值是否完全一致」—— 一致的与冲突的分开计数：
///    **冲突的那些正是漏斗第一级不报、实体绑定层判「待人工判定」的那批**。
pub fn detect(rows: &[SourceRow]) -> NaiveOutcome {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut skipped_rows = 0usize;
    for (i, row) in rows.iter().enumerate() {
        if !key::is_bindable(row) {
            skipped_rows += 1;
            continue;
        }
        groups.entry(key::business_key(row)).or_default().push(i);
    }

    let mut outcome = NaiveOutcome {
        stats: NaiveStats {
            eligible_rows: rows.len() - skipped_rows,
            skipped_rows,
            ..NaiveStats::default()
        },
        ..NaiveOutcome::default()
    };

    for (business_key, members) in &groups {
        if members.len() < 2 {
            continue;
        }
        outcome.stats.groups += 1;

        // 稳定顺序：按行键排序 —— 同一份输入两次判定结果逐字相同
        let mut ordered = members.clone();
        ordered.sort_by_key(|&i| key::row_uid(&rows[i]));

        let n = ordered.len();
        if n <= PAIR_EXPANSION_MAX {
            for a in 0..n {
                for b in (a + 1)..n {
                    push_pair(
                        &mut outcome.pairs,
                        &mut outcome.stats,
                        rows,
                        ordered[a],
                        ordered[b],
                        business_key,
                    );
                }
            }
        } else {
            // 星形：首行与其余各行成对；折叠掉的对数记进计数，不静默截断
            let anchor = ordered[0];
            for &other in ordered.iter().skip(1) {
                push_pair(
                    &mut outcome.pairs,
                    &mut outcome.stats,
                    rows,
                    anchor,
                    other,
                    business_key,
                );
            }
            outcome.stats.collapsed_pairs += n * (n - 1) / 2 - (n - 1);
        }
    }

    outcome.stats.pairs = outcome.pairs.len();
    outcome
}

/// 把一对行记成基线候选对
fn push_pair(
    pairs: &mut Vec<SuspectedDuplicate>,
    stats: &mut NaiveStats,
    rows: &[SourceRow],
    a: usize,
    b: usize,
    business_key: &str,
) {
    let key_a = key::row_uid(&rows[a]);
    let key_b = key::row_uid(&rows[b]);
    if key_a == key_b {
        // 行键相同 = 其实是同一行，自己和自己不构成一对
        return;
    }
    let (left, right, left_key, right_key) = if key_a <= key_b {
        (a, b, key_a.clone(), key_b.clone())
    } else {
        (b, a, key_b.clone(), key_a.clone())
    };

    // 取值是否完全一致 —— 基线不拿它当判据，但分开计数（对比表要说明白差异出在哪）
    let identical = value_signature(&rows[left]) == value_signature(&rows[right]);
    if identical {
        stats.identical_pairs += 1;
    } else {
        stats.conflicting_pairs += 1;
    }

    pairs.push(SuspectedDuplicate {
        left_row_key: left_key.clone(),
        right_row_key: right_key.clone(),
        match_level: MATCH_LEVEL_NAIVE.to_string(),
        basis: NAIVE_BASIS.to_string(),
        business_key: business_key.to_string(),
        left_reference: reference(&rows[left]),
        right_reference: reference(&rows[right]),
        cross_table: rows[left].source_ns != rows[right].source_ns,
        state: STATE_SUSPECTED.to_string(),
        left_original: String::new(),
        right_original: String::new(),
        left_normalized: String::new(),
        right_normalized: String::new(),
        normalize_detail: String::new(),
        distance: None,
        distance_detail: String::new(),
        source: crate::dupe::default_candidate_source(),
        semantic_kind: String::new(),
        semantic_confidence: None,
        semantic_reason: String::new(),
        semantic_evidence: Vec::new(),
        // 基线认下的对不进人工确认流 —— 它是尺子，不是功能
        mergeable: false,
    });
}

/// 行的可读引用："表名 第 N 行"
fn reference(row: &SourceRow) -> String {
    format!("{} 第 {} 行", row.source_ns, row.row_no)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Field;
    use std::collections::BTreeMap;

    fn row(source_ns: &str, row_no: usize, business_key: &str, value: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
        values.insert(Field::IndicatorName, "病案首页24小时内完成率".to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::DeptName, "病案统计室".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, business_key.to_string());
        values.insert(Field::Numerator, "980".to_string());
        values.insert(Field::Denominator, "1000".to_string());
        values.insert(Field::Value, value.to_string());
        values.insert(Field::Unit, "%".to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    fn detect_rows(rows: &mut Vec<SourceRow>) -> NaiveOutcome {
        key::assign_dup_ordinals(rows);
        detect(rows)
    }

    #[test]
    fn 同键即判重复_取值冲突也算() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98.0"),
            row("科室二表", 7, "BK-01", "97.5"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1, "基线只看键，取值冲突也照报");
        assert_eq!(out.stats.conflicting_pairs, 1);
        assert_eq!(out.stats.identical_pairs, 0);
        assert_eq!(out.pairs[0].match_level, MATCH_LEVEL_NAIVE);
        assert!(!out.pairs[0].mergeable, "基线认下的对不进人工确认流");
    }

    #[test]
    fn 同键且取值一致的分到一致计数() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98.0"),
            row("科室二表", 7, "BK-01", "98"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.stats.identical_pairs, 1);
        assert_eq!(out.stats.conflicting_pairs, 0);
    }

    #[test]
    fn 键要素不全的行不参与判定() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98.0"),
            row("科室二表", 7, "", "98.0"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.skipped_rows, 1);
    }

    #[test]
    fn 同一份输入两次判定结果逐字相同() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98.0"),
            row("科室二表", 7, "BK-01", "98.0"),
            row("科室三表", 3, "BK-02", "60"),
        ];
        let first = detect_rows(&mut rows);
        let second = detect_rows(&mut rows);
        assert_eq!(first.pairs, second.pairs);
    }
}
