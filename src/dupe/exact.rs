//! 重复检测四级漏斗 · 第一级：确定性键重复检测（L1）。
//!
//! # 判据
//!
//! 同一**确定性键**（指标编号 + 科室代码 + 报告期 + 业务主键）下，
//! **取值完全一致**（分子 / 分母 / 填报值 / 计量单位 / 名称等全部一致）的行，
//! 判为**疑似重复**。
//!
//! 这一级最严：只认「字面上就是同一行填了两遍」，不猜任何语义。
//!
//! # 为什么取值一致要连单位一起比
//!
//! 单位写法不同（全角 `％` 与半角 `%`、`例数` 与 `例`）在肉眼看不出差别，
//! 按字面比却是两个值。所以**单位先按别名表归一再比**（[`crate::rules::normalize`]）——
//! 归一表是口径层的一张表，单位检查器与本级共用，改口径不用改代码。
//!
//! # 与实体绑定层的关系
//!
//! 实体绑定层（[`crate::bind`]）按**权威键**（多一个机构代码）分组，
//! 判定「同一权威键下取值完全一致 → 疑似重复；取值不同 → 待人工判定」；
//! 本级按**确定性键**分组，判定「取值完全一致 → 疑似重复对」。
//!
//! 两层用的是**同一份取值签名**（[`crate::dupe::value_signature`]）——
//! 同一对行不会在两个环节得出两个结论。
//!
//! 差别只在**粒度**：
//!
//! - 本级只管「取值完全一致」的那些对，产出**成对的候选**（谁和谁像）；
//! - 绑定层管归属（同一条业务记录哪一行是权威行），取值不同的那些行它标
//!   「待人工判定」——**本级不碰**，避免把「要人判的」混进「疑似重复」。
//!
//! # 铁律
//!
//! **只标「疑似」，不合并、不推送。** 候选对进结果契约的 `suspected_duplicates`，
//! 行级标注进「疑似重复项」列，等经办人逐条勾选确认（确认流见任务 36）。

use std::collections::BTreeMap;

use crate::dupe::{
    short_key, value_signature, SuspectedDuplicate, PAIR_EXPANSION_MAX, STATE_SUSPECTED,
};
use crate::ingest::key;
use crate::ingest::{Field, SourceRow};

/// 命中层级取值（确定性键）
pub const MATCH_LEVEL_EXACT: &str = "exact";

/// L1 判定依据
pub const EXACT_BASIS: &str = "本产品最小数据格式与字段要求（确定性键 = 指标编号 + 科室代码 + 报告期 + 业务主键）；同一确定性键下取值完全一致（分子 / 分母 / 填报值 / 计量单位等全部一致，单位按单位别名表归一后比较）的行标「疑似重复」，由经办人逐条勾选确认后才合并，本轮不合并、不上报";

/// L1 的运行计数（可观测：结果契约与日志里都能看到）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExactStats {
    /// 参与判定的行数（键要素齐全）
    pub eligible_rows: usize,
    /// 键要素不全、未参与判定的行数
    pub skipped_rows: usize,
    /// 确定性键分组数（组内 ≥2 行才算一组）
    pub groups: usize,
    /// 命中 L1 的取值桶数
    pub duplicate_groups: usize,
    /// 含「同键异值」行的分组数（这些行归实体绑定层判「待人工判定」）
    pub conflicting_groups: usize,
    /// 同键异值的行数（不属本级）
    pub conflicting_rows: usize,
    /// 候选对数
    pub pairs: usize,
    /// 大桶折叠掉的对数（不静默截断）
    pub collapsed_pairs: usize,
    /// 行键相同的对被跳过（同一行不可能和自己重复；出现即说明行键未编号）
    pub degenerate_pairs: usize,
}

/// L1 的产出
#[derive(Debug, Clone, Default)]
pub struct ExactOutcome {
    /// 疑似重复候选对
    pub pairs: Vec<SuspectedDuplicate>,
    /// 行键 → 「疑似重复项」列取值
    pub suspected: BTreeMap<String, String>,
    /// 运行计数
    pub stats: ExactStats,
}

/// 确定性键重复检测（L1）。
///
/// 判定步骤：
///
/// 1. 按确定性键（业务主键）分组 —— 键要素不全的行不参与（它缺的是必填列，
///    该报的是「必填缺失」，不该在这里再报一次重复）；
/// 2. 组内按**取值签名**分桶；
/// 3. 桶内 ≥2 行 → 两两成对（桶过大时折成星形并计数）；
/// 4. 桶内 1 行 → 与同组其他行取值不同，**不是本级的疑似重复**，
///    归实体绑定层的「待人工判定」。
pub fn detect(rows: &[SourceRow]) -> ExactOutcome {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut skipped_rows = 0usize;
    for (i, row) in rows.iter().enumerate() {
        if !key::is_bindable(row) {
            skipped_rows += 1;
            continue;
        }
        groups.entry(key::business_key(row)).or_default().push(i);
    }

    let mut outcome = ExactOutcome {
        stats: ExactStats {
            eligible_rows: rows.len() - skipped_rows,
            skipped_rows,
            ..ExactStats::default()
        },
        ..ExactOutcome::default()
    };

    // 行键 → 疑似对象（可读引用 + 对方行键短码）
    let mut peers: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (business_key, members) in &groups {
        if members.len() < 2 {
            continue;
        }
        outcome.stats.groups += 1;

        // 组内按取值签名分桶
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in members {
            buckets
                .entry(value_signature(&rows[i]))
                .or_default()
                .push(i);
        }

        let mut conflicting_rows = 0usize;
        for bucket in buckets.values() {
            if bucket.len() < 2 {
                // 与同组其他行取值不同 —— 不是本级的疑似重复
                conflicting_rows += 1;
                continue;
            }
            outcome.stats.duplicate_groups += 1;

            // 稳定顺序：按行键排序，同一份输入两次判定结果逐字相同
            let mut ordered = bucket.clone();
            ordered.sort_by_key(|&i| key::row_uid(&rows[i]));

            let n = ordered.len();
            if n <= PAIR_EXPANSION_MAX {
                for a in 0..n {
                    for b in (a + 1)..n {
                        push_pair(
                            &mut outcome,
                            &mut peers,
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
                    push_pair(&mut outcome, &mut peers, rows, anchor, other, business_key);
                }
                outcome.stats.collapsed_pairs += n * (n - 1) / 2 - (n - 1);
            }
        }

        if conflicting_rows > 0 {
            outcome.stats.conflicting_groups += 1;
            outcome.stats.conflicting_rows += conflicting_rows;
        }
    }

    // 「疑似重复项」列取值：每行都写清「和谁像」
    for (row_key, list) in peers {
        let text = format!("疑似重复：与 {} 取值完全一致", list.join("、"));
        outcome.suspected.insert(row_key, text);
    }
    outcome.stats.pairs = outcome.pairs.len();

    outcome
}

/// 把一对行记成候选对，并给两行各记一条疑似对象
fn push_pair(
    outcome: &mut ExactOutcome,
    peers: &mut BTreeMap<String, Vec<String>>,
    rows: &[SourceRow],
    a: usize,
    b: usize,
    business_key: &str,
) {
    let key_a = key::row_uid(&rows[a]);
    let key_b = key::row_uid(&rows[b]);
    // 行键相同 = 其实是同一行（调用前没给同键行编号），自己和自己不构成一对
    if key_a == key_b {
        outcome.stats.degenerate_pairs += 1;
        return;
    }
    // 左小右大：同一对行不论谁先算出来，顺序都一样
    let (left, right, left_key, right_key) = if key_a <= key_b {
        (a, b, key_a.clone(), key_b.clone())
    } else {
        (b, a, key_b.clone(), key_a.clone())
    };

    let left_ref = reference(&rows[left]);
    let right_ref = reference(&rows[right]);

    outcome.pairs.push(SuspectedDuplicate {
        left_row_key: left_key.clone(),
        right_row_key: right_key.clone(),
        match_level: MATCH_LEVEL_EXACT.to_string(),
        basis: EXACT_BASIS.to_string(),
        business_key: business_key.to_string(),
        left_reference: left_ref.clone(),
        right_reference: right_ref.clone(),
        cross_table: rows[left].source_ns != rows[right].source_ns,
        state: STATE_SUSPECTED.to_string(),
        // 确定性键这一级比的是「字面完全一致」，没有「归一前 / 归一后」两版可留
        left_original: String::new(),
        right_original: String::new(),
        left_normalized: String::new(),
        right_normalized: String::new(),
        normalize_detail: String::new(),
        // 编辑距离是第三级的事，这一级不填
        distance: None,
        distance_detail: String::new(),
        // 确定性键这一级：候选来源是确定性
        source: crate::dupe::default_candidate_source(),

        semantic_kind: String::new(),
        semantic_confidence: None,
        semantic_reason: String::new(),
        semantic_evidence: Vec::new(),
        mergeable: crate::dupe::default_mergeable(),
    });

    peers.entry(left_key.clone()).or_default().push(format!(
        "{}（行键 {}）",
        right_ref,
        short_key(&right_key)
    ));
    peers.entry(right_key.clone()).or_default().push(format!(
        "{}（行键 {}）",
        left_ref,
        short_key(&left_key)
    ));
}

/// 行的可读引用："表名 第 N 行"
fn reference(row: &SourceRow) -> String {
    format!("{} 第 {} 行", row.source_ns, row.row_no)
}

/// 确定性键的可读写法（给经办人看，不是一串拼接符）
pub fn describe_key(row: &SourceRow) -> String {
    format!(
        "指标编号 {}｜科室代码 {}｜报告期 {}｜业务主键 {}",
        text(row, Field::IndicatorCode),
        text(row, Field::DeptCode),
        text(row, Field::ReportingPeriod),
        text(row, Field::BusinessKey)
    )
}

fn text(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        source_ns: &str,
        row_no: usize,
        business_key: &str,
        value: &str,
        unit: &str,
    ) -> SourceRow {
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
        values.insert(Field::Unit, unit.to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    fn detect_rows(rows: &mut Vec<SourceRow>) -> ExactOutcome {
        key::assign_dup_ordinals(rows);
        detect(rows)
    }

    #[test]
    fn 完全一致的两行被检出并标疑似() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "BK-01", "98", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1);
        assert_eq!(out.pairs[0].match_level, MATCH_LEVEL_EXACT);
        assert!(!out.pairs[0].basis.is_empty());
        assert_eq!(out.pairs[0].state, STATE_SUSPECTED);
        assert!(out.pairs[0].cross_table, "两张来源表各填了一次");
        assert_eq!(out.suspected.len(), 2, "两行都要标出疑似重复");
    }

    #[test]
    fn 取值不同的同键行不属本级() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "BK-01", "60", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty(), "取值不同不能判成疑似重复");
        assert_eq!(out.stats.conflicting_rows, 2);
        assert!(out.suspected.is_empty());
    }

    #[test]
    fn 单位写法不同经归一后判为一致() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "BK-01", "98", "％"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1, "全角百分号与半角百分号是同一个单位");
    }

    #[test]
    fn 键要素不全的行不参与判定() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "", "98", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.skipped_rows, 1);
    }

    #[test]
    fn 三行完全一致出三对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "BK-01", "98", "%"),
            row("科室三表", 3, "BK-01", "98", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 3);
        assert_eq!(out.suspected.len(), 3);
    }

    #[test]
    fn 大桶折成星形且折叠数有计数() {
        let mut rows = Vec::new();
        for i in 0..10 {
            rows.push(row("科室一表", i + 1, "BK-01", "98", "%"));
        }
        let out = detect_rows(&mut rows);
        // 星形：首行与其余 9 行成对
        assert_eq!(out.pairs.len(), 9);
        // 折叠掉的对数 = C(10,2) - 9 = 45 - 9
        assert_eq!(out.stats.collapsed_pairs, 36);
        assert_eq!(out.suspected.len(), 10, "每一行都还能看到自己和谁重复");
    }

    #[test]
    fn 同一份输入两次判定结果逐字相同() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "98", "%"),
            row("科室二表", 7, "BK-01", "98", "%"),
            row("科室三表", 3, "BK-02", "60", "%"),
        ];
        let first = detect_rows(&mut rows);
        let second = detect_rows(&mut rows);
        assert_eq!(first.pairs, second.pairs);
        assert_eq!(first.suspected, second.suspected);
    }
}
