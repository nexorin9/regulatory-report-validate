//! 重复检测四级漏斗 · 第二级：归一化候选（L2）。
//!
//! # 判据
//!
//! **同一条业务记录被填了两遍，只是写法不一样** —— 院感写成「院感」还是「医院感染」，
//! `CT/MRI` 写成半角还是全角，「病案 室」中间多一个空格，单位写「例数」还是「例」。
//!
//! 确定性键那一级（[L1](super::exact)）按**字面**比，取值完全一致才算命中；
//! 写法差一个字、差一个空格、差一个全角，它就抓不到 —— 而现场天天有。
//! 这一级把这些写法差异**归一**之后再比。
//!
//! # 判定的四道条件
//!
//! 候选对必须同时满足：
//!
//! 1. **同一条记录**：跨表匹配键（指标编号 + 科室代码 + 报告期）相同，**且业务主键相同**
//!    —— 同一指标下的两条不同业务记录不是重复填报，是两件事；
//! 2. **数是同一个数**：分子 / 分母 / 填报值按数值相等（`12` 与 `12.0` 是同一个数）；
//! 3. **归一后一致**：归一后的指标名称与归一后的计量单位都一致；
//! 4. **原文确实有差异**：两行的指标名称原文或计量单位原文不完全一样 ——
//!    归一没有吸收到任何东西的话，那不是本级要管的事。
//!
//! 另外两条「不归本级」的：
//!
//! - **字面完全一致** → 确定性键那一级已经报了，本级不重复报（计数在
//!   [`NormalizeStats::exact_overlap_pairs`]，看得见，不静默吞掉）；
//! - **数不一样** → 那是取值冲突，归实体绑定层的「待人工判定」，本级不碰
//!   （计数在 [`NormalizeStats::value_conflict_pairs`]）。
//!
//! # 与实体绑定层的关系：两个提示不矛盾
//!
//! 归一化候选对的字面取值确实不同（名称写法不同 → 取值签名不同），所以实体绑定层
//! 会把这两行标成「待人工判定」。两个提示说的是同一件事的两面：
//!
//! - 绑定层：「字面取值不同，**我不替你定**，等人判」；
//! - 本级：「归一后**看着像**同一件事，去看一眼」。
//!
//! 两个提示都不合并、不推送 —— 合并与否仍由经办人逐条勾选（确认流见任务 36）。
//!
//! # 归一函数族在哪
//!
//! 归一的**口径数据**（单位别名表、指标名称别名表）放在口径层
//! （[`crate::rules::normalize`]）—— 与单位检查器共用同一份表，改口径不用改代码。
//! 本模块只做**归一怎么用**（分组、候选对、举证）。
//!
//! # 铁律
//!
//! **只标「疑似」，不合并、不推送。** 候选对进结果契约的 `suspected_duplicates`，
//! 行级标注进「疑似重复项」列，等经办人逐条勾选确认。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::dupe::{
    short_key, value_signature, values_signature, SuspectedDuplicate, PAIR_EXPANSION_MAX,
    STATE_SUSPECTED,
};
use crate::ingest::key;
use crate::ingest::{Field, SourceRow};
use crate::rules::normalize::{
    normalize_key, AliasTable, NameAliasTable, CANONICAL_UNITS, INDICATOR_NAME_ALIAS_BASIS,
};

/// 命中层级取值（归一化候选）
pub const MATCH_LEVEL_NORMALIZE: &str = "normalize";

/// L2 判定依据
pub const NORMALIZE_BASIS: &str = "本产品最小数据格式与字段要求（跨表匹配键 = 指标编号 + 科室代码 + 报告期，业务主键 = 指标编号 + 科室代码 + 报告期 + 业务主键）；同一业务主键下取值按归一规则归一（去首尾空白与不间断空格、全角转半角、大小写折叠、计量单位按单位别名表归一、指标名称按指标名称别名表归一）后一致、且原文写法确有差异的行标「疑似重复」，由经办人逐条勾选确认后才合并，本轮不合并、不上报";

/// 归一状态取值
pub const STATUS_NORMALIZED: &str = "已归一";
/// 归一状态取值（未归一）
pub const STATUS_UNNORMALIZED: &str = "未归一";

/// 未归一的原因：指标名称为空
pub const REASON_NAME_EMPTY: &str = "指标名称为空，没有可比对的名称";
/// 未归一的原因：计量单位别名未覆盖
pub const REASON_UNIT_NOT_COVERED: &str = "计量单位不在规范单位清单内，别名表未覆盖该写法";

/// 一行的归一结果（举证用：原值与归一值一起留着）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedRow {
    /// 行键
    pub row_key: String,
    /// 可读引用（"表名 第 N 行"）
    pub reference: String,
    /// 业务主键
    pub business_key: String,
    /// 指标名称原值（去首尾空白）
    pub indicator_name_raw: String,
    /// 指标名称归一后
    pub indicator_name_normalized: String,
    /// 计量单位原值（去首尾空白）
    pub unit_raw: String,
    /// 计量单位归一后（规范单位）
    pub unit_normalized: String,
    /// 数值部分签名（分子 / 分母 / 填报值）
    pub values_signature: String,
    /// 归一状态：已归一 / 未归一
    pub status: String,
    /// 未归一的原因（已归一时为空串）
    pub status_note: String,
}

impl NormalizedRow {
    /// 分桶键：归一后的（指标名称 + 计量单位）
    pub fn bucket_key(&self) -> String {
        format!(
            "{}{}",
            normalize_key(&self.indicator_name_normalized),
            self.unit_normalized
        )
    }
}

/// L2 的运行计数（可观测：结果契约与日志里都能看到）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizeStats {
    /// 参与判定的行数（键要素齐全）
    pub eligible_rows: usize,
    /// 键要素不全、未参与判定的行数
    pub skipped_rows: usize,
    /// 未归一的行数（指标名称为空 / 单位别名未覆盖）
    pub unnormalized_rows: usize,
    /// 其中：指标名称为空的行数
    pub unnormalized_name_rows: usize,
    /// 其中：计量单位别名未覆盖的行数
    pub unnormalized_unit_rows: usize,
    /// 跨表匹配键分组数
    pub match_key_groups: usize,
    /// 归一后成桶的桶数（桶内 ≥2 行才算一桶）
    pub buckets: usize,
    /// 候选对数
    pub pairs: usize,
    /// 大桶折叠掉的对数（不静默截断）
    pub collapsed_pairs: usize,
    /// 行键相同的对被跳过（同一行不可能和自己重复）
    pub degenerate_pairs: usize,
    /// 与确定性键那一级重叠的对数（字面完全一致，本级不重复报）
    pub exact_overlap_pairs: usize,
    /// 数值不同的对数（归「待人工判定」，本级不碰）
    pub value_conflict_pairs: usize,
}

/// L2 的产出
#[derive(Debug, Clone, Default)]
pub struct NormalizeOutcome {
    /// 疑似重复候选对
    pub pairs: Vec<SuspectedDuplicate>,
    /// 行键 → 「疑似重复项」列取值
    pub suspected: BTreeMap<String, String>,
    /// 逐行归一结果（举证用）
    pub rows: Vec<NormalizedRow>,
    /// 运行计数
    pub stats: NormalizeStats,
}

/// 归一化候选检测（L2）。
///
/// 判定步骤：
///
/// 1. 逐行归一（指标名称、计量单位），算数值部分签名；
/// 2. 按跨表匹配键（指标编号 + 科室代码 + 报告期）分组，组内再按业务主键分记录；
/// 3. 每条记录内按归一后的（指标名称 + 计量单位）分桶；
/// 4. 桶内成对，按四道条件判定（见模块头部）。
pub fn detect(rows: &[SourceRow]) -> NormalizeOutcome {
    let mut outcome = NormalizeOutcome::default();

    // 1. 逐行归一
    let mut normalized: Vec<Option<NormalizedRow>> = Vec::with_capacity(rows.len());
    let mut eligible: Vec<usize> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        normalized.push(None);
        if !key::is_bindable(row) {
            // 键要素不全的行不参与：它缺的是必填列，该报的是「必填缺失」
            outcome.stats.skipped_rows += 1;
            continue;
        }
        let norm = normalize_row(row);
        if norm.indicator_name_raw.is_empty() {
            // 没有名称可比：本级不参与，但**计数留痕**（不静默丢行）
            outcome.stats.unnormalized_name_rows += 1;
            outcome.stats.unnormalized_rows += 1;
            outcome.rows.push(norm);
            continue;
        }
        if !is_known_unit(&norm.unit_normalized) {
            // 单位别名未覆盖：照样归一（确定性键照样算得出），但标「未归一」并计数
            outcome.stats.unnormalized_unit_rows += 1;
            outcome.stats.unnormalized_rows += 1;
        }
        outcome.stats.eligible_rows += 1;
        outcome.rows.push(norm.clone());
        normalized[i] = Some(norm);
        eligible.push(i);
    }

    // 2. 按跨表匹配键分组
    let mut by_match: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &eligible {
        by_match
            .entry(key::match_key(&rows[i]))
            .or_default()
            .push(i);
    }
    outcome.stats.match_key_groups = by_match.len();

    // 3. 组内按业务主键分记录，每条记录内按归一键分桶
    let mut peers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for members in by_match.values() {
        let mut by_business: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in members {
            by_business
                .entry(key::business_key(&rows[i]))
                .or_default()
                .push(i);
        }
        for group in by_business.values() {
            if group.len() < 2 {
                continue;
            }
            let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for &i in group {
                let bucket_key = normalized[i]
                    .as_ref()
                    .expect("参与判定的行必有归一结果")
                    .bucket_key();
                buckets.entry(bucket_key).or_default().push(i);
            }
            for bucket in buckets.values() {
                if bucket.len() < 2 {
                    continue;
                }
                outcome.stats.buckets += 1;
                // 稳定顺序：按行键排序，同一份输入两次判定结果逐字相同
                let mut ordered = bucket.clone();
                ordered.sort_by_key(|&i| key::row_uid(&rows[i]));
                let n = ordered.len();
                if n <= PAIR_EXPANSION_MAX {
                    for a in 0..n {
                        for b in (a + 1)..n {
                            consider_pair(
                                &mut outcome,
                                &mut peers,
                                rows,
                                &normalized,
                                ordered[a],
                                ordered[b],
                            );
                        }
                    }
                } else {
                    // 星形：首行与其余各行成对；折叠掉的对数记进计数，不静默截断
                    let anchor = ordered[0];
                    for &other in ordered.iter().skip(1) {
                        consider_pair(&mut outcome, &mut peers, rows, &normalized, anchor, other);
                    }
                    outcome.stats.collapsed_pairs += n * (n - 1) / 2 - (n - 1);
                }
            }
        }
    }

    // 4. 「疑似重复项」列取值：每行都写清「和谁像、像在哪」
    for (row_key, list) in peers {
        let text = format!("疑似重复：与 {}", list.join("、"));
        outcome.suspected.insert(row_key, text);
    }
    outcome.stats.pairs = outcome.pairs.len();

    outcome
}

/// 判定一对行是不是「归一化候选」，是则记候选对并给两行各记一条疑似对象
fn consider_pair(
    outcome: &mut NormalizeOutcome,
    peers: &mut BTreeMap<String, Vec<String>>,
    rows: &[SourceRow],
    normalized: &[Option<NormalizedRow>],
    a: usize,
    b: usize,
) {
    let key_a = key::row_uid(&rows[a]);
    let key_b = key::row_uid(&rows[b]);
    // 行键相同 = 其实是同一行，自己和自己不构成一对
    if key_a == key_b {
        outcome.stats.degenerate_pairs += 1;
        return;
    }
    // 字面完全一致 → 确定性键那一级已经报了，本级不重复报（计数留痕）
    if value_signature(&rows[a]) == value_signature(&rows[b]) {
        outcome.stats.exact_overlap_pairs += 1;
        return;
    }
    // 数不一样 → 取值冲突，归实体绑定层的「待人工判定」，本级不碰（计数留痕）
    if values_signature(&rows[a]) != values_signature(&rows[b]) {
        outcome.stats.value_conflict_pairs += 1;
        return;
    }
    let norm_a = normalized[a].as_ref().expect("参与判定的行必有归一结果");
    let norm_b = normalized[b].as_ref().expect("参与判定的行必有归一结果");
    // 原文写法必须真的有差异 —— 归一没吸收到东西的，不是本级要管的
    if norm_a.indicator_name_raw == norm_b.indicator_name_raw && norm_a.unit_raw == norm_b.unit_raw
    {
        return;
    }

    // 左小右大：同一对行不论谁先算出来，顺序都一样
    let (left, right, left_key, right_key, left_norm, right_norm) = if key_a <= key_b {
        (a, b, key_a.clone(), key_b.clone(), norm_a, norm_b)
    } else {
        (b, a, key_b.clone(), key_a.clone(), norm_b, norm_a)
    };

    let left_ref = reference(&rows[left]);
    let right_ref = reference(&rows[right]);
    let business_key = key::business_key(&rows[left]);

    outcome.pairs.push(SuspectedDuplicate {
        left_row_key: left_key.clone(),
        right_row_key: right_key.clone(),
        match_level: MATCH_LEVEL_NORMALIZE.to_string(),
        basis: NORMALIZE_BASIS.to_string(),
        business_key: business_key.clone(),
        left_reference: left_ref.clone(),
        right_reference: right_ref.clone(),
        cross_table: rows[left].source_ns != rows[right].source_ns,
        state: STATE_SUSPECTED.to_string(),
        left_original: original_summary(left_norm),
        right_original: original_summary(right_norm),
        left_normalized: normalized_summary(left_norm),
        right_normalized: normalized_summary(right_norm),
        normalize_detail: normalize_detail(left_norm, right_norm),
        // 编辑距离是第三级的事，这一级不填
        distance: None,
        distance_detail: String::new(),
        // 归一化这一级：候选来源是确定性
        source: crate::dupe::default_candidate_source(),

        semantic_kind: String::new(),
        semantic_confidence: None,
        semantic_reason: String::new(),
        semantic_evidence: Vec::new(),
        mergeable: crate::dupe::default_mergeable(),
    });

    peers.entry(left_key.clone()).or_default().push(format!(
        "{}（行键 {}）{}",
        right_ref,
        short_key(&right_key),
        difference_phrase(left_norm, right_norm)
    ));
    peers.entry(right_key.clone()).or_default().push(format!(
        "{}（行键 {}）{}",
        left_ref,
        short_key(&left_key),
        difference_phrase(right_norm, left_norm)
    ));
}

/// 归一一行
fn normalize_row(row: &SourceRow) -> NormalizedRow {
    let name_raw = trimmed(row, Field::IndicatorName);
    let unit_raw = trimmed(row, Field::Unit);
    let name_normalized = name_alias_table().canonicalize(&name_raw);
    let unit_normalized = loose_unit(&unit_raw);

    let (status, status_note) = if name_raw.is_empty() {
        (
            STATUS_UNNORMALIZED.to_string(),
            REASON_NAME_EMPTY.to_string(),
        )
    } else if !is_known_unit(&unit_normalized) {
        (
            STATUS_UNNORMALIZED.to_string(),
            REASON_UNIT_NOT_COVERED.to_string(),
        )
    } else {
        (STATUS_NORMALIZED.to_string(), String::new())
    };

    NormalizedRow {
        row_key: key::row_uid(row),
        reference: reference(row),
        business_key: key::business_key(row),
        indicator_name_raw: name_raw,
        indicator_name_normalized: name_normalized,
        unit_raw,
        unit_normalized,
        values_signature: values_signature(row),
        status,
        status_note,
    }
}

/// 计量单位归一：先按分组用归一（全角折半角、去空白、小写），再过单位别名表。
///
/// 别名表在口径层（与单位检查器共用同一份），归一顺序写死在这里：
/// 先去空白再查表，「例 数」才能命中「例数」这条别名。
fn loose_unit(raw: &str) -> String {
    let folded = normalize_key(raw);
    alias_table().canonicalize(&folded)
}

/// 归一后的单位是不是五种规范单位之一
fn is_known_unit(canonical: &str) -> bool {
    CANONICAL_UNITS.contains(&canonical)
}

/// 原值摘要（举证用）
fn original_summary(norm: &NormalizedRow) -> String {
    format!(
        "指标名称={}｜计量单位={}",
        norm.indicator_name_raw, norm.unit_raw
    )
}

/// 归一值摘要（举证用）
fn normalized_summary(norm: &NormalizedRow) -> String {
    format!(
        "指标名称={}｜计量单位={}",
        norm.indicator_name_normalized, norm.unit_normalized
    )
}

/// 归一详情：写法差异是怎么被吸收的
fn normalize_detail(left: &NormalizedRow, right: &NormalizedRow) -> String {
    format!(
        "指标名称：左「{}」归一为「{}」，右「{}」归一为「{}」；计量单位：左「{}」归一为「{}」，右「{}」归一为「{}」",
        left.indicator_name_raw,
        left.indicator_name_normalized,
        right.indicator_name_raw,
        right.indicator_name_normalized,
        left.unit_raw,
        left.unit_normalized,
        right.unit_raw,
        right.unit_normalized
    )
}

/// 差异短语（写进「疑似重复项」列）：像在哪
///
/// 写成「A 与 B 归一后一致」而不是「A 归一后＝X」—— 归一吸收掉的是**两行之间的差异**，
/// 只说本行归一成了什么，看不出对面那一行原来写的是什么。
fn difference_phrase(self_norm: &NormalizedRow, other: &NormalizedRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    if self_norm.indicator_name_raw != other.indicator_name_raw {
        parts.push(format!(
            "指标名称「{}」与「{}」归一后一致",
            self_norm.indicator_name_raw, other.indicator_name_raw
        ));
    }
    if self_norm.unit_raw != other.unit_raw {
        parts.push(format!(
            "计量单位「{}」与「{}」归一后一致",
            self_norm.unit_raw, other.unit_raw
        ));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("（{}）", parts.join("、"))
}

/// 行的可读引用："表名 第 N 行"
fn reference(row: &SourceRow) -> String {
    format!("{} 第 {} 行", row.source_ns, row.row_no)
}

/// 取值（去首尾空白；空串算空）
fn trimmed(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

/// 内置单位别名表（只建一次；与单位检查器共用同一份口径表）
fn alias_table() -> &'static AliasTable {
    static TABLE: OnceLock<AliasTable> = OnceLock::new();
    TABLE.get_or_init(AliasTable::builtin)
}

/// 内置指标名称别名表（只建一次）
fn name_alias_table() -> &'static NameAliasTable {
    static TABLE: OnceLock<NameAliasTable> = OnceLock::new();
    TABLE.get_or_init(NameAliasTable::builtin)
}

/// 指标名称别名表的依据（给文档与规则清单引用）
pub fn name_alias_basis() -> &'static str {
    INDICATOR_NAME_ALIAS_BASIS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        source_ns: &str,
        row_no: usize,
        business_key: &str,
        name: &str,
        unit: &str,
    ) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
        values.insert(Field::IndicatorName, name.to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::DeptName, "病案统计室".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, business_key.to_string());
        values.insert(Field::Numerator, "980".to_string());
        values.insert(Field::Denominator, "1000".to_string());
        values.insert(Field::Value, "98".to_string());
        values.insert(Field::Unit, unit.to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    fn detect_rows(rows: &mut Vec<SourceRow>) -> NormalizeOutcome {
        key::assign_dup_ordinals(rows);
        detect(rows)
    }

    #[test]
    fn 指标名称别名不同但归一后一致则出候选对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "院感", "%"),
            row("科室二表", 7, "BK-01", "医院感染", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1);
        assert_eq!(out.pairs[0].match_level, MATCH_LEVEL_NORMALIZE);
        assert!(!out.pairs[0].basis.is_empty());
        assert_eq!(out.pairs[0].state, STATE_SUSPECTED);
        assert!(out.pairs[0].cross_table);
        assert_eq!(out.suspected.len(), 2);
    }

    #[test]
    fn 全角半角写法不同也出候选对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "CT/MRI检查记录符合率", "%"),
            row("科室二表", 7, "BK-01", "ＣＴ／ＭＲＩ检查记录符合率", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1);
    }

    #[test]
    fn 名称中间多一个中文空格也出候选对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案 室", "%"),
            row("科室二表", 7, "BK-01", "病案室", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1);
    }

    #[test]
    fn 原值与归一值同时落盘() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "院感", "例数"),
            row("科室二表", 7, "BK-01", "医院感染", "例"),
        ];
        let out = detect_rows(&mut rows);
        let pair = &out.pairs[0];
        // 左右顺序按行键排，不固定 —— 断言写成「两版原值都在」，不挑边
        let originals = [pair.left_original.as_str(), pair.right_original.as_str()];
        assert!(
            originals.contains(&"指标名称=院感｜计量单位=例数"),
            "原值摘要应留「院感」「例数」：{:?}",
            originals
        );
        assert!(
            originals.contains(&"指标名称=医院感染｜计量单位=例"),
            "原值摘要应留「医院感染」「例」：{:?}",
            originals
        );
        assert!(pair.left_normalized.contains("医院感染"));
        assert!(pair.right_normalized.contains("医院感染"));
        assert!(pair.normalize_detail.contains("院感"));
        assert!(pair.normalize_detail.contains("医院感染"));
    }

    #[test]
    fn 字面完全一致的不在本级重复报() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室", "%"),
            row("科室二表", 7, "BK-01", "病案室", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.exact_overlap_pairs, 1);
    }

    #[test]
    fn 数值不同不归本级() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "院感", "%"),
            row("科室二表", 7, "BK-01", "医院感染", "%"),
        ];
        rows[1].values.insert(Field::Numerator, "960".to_string());
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.value_conflict_pairs, 1);
    }

    #[test]
    fn 业务主键不同不是同一件事() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "院感", "%"),
            row("科室二表", 7, "BK-02", "医院感染", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty(), "同指标下的两条不同记录不是重复填报");
    }

    #[test]
    fn 指标名称为空落到未归一并计数() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "", "%"),
            row("科室二表", 7, "BK-01", "医院感染", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.unnormalized_name_rows, 1);
        assert_eq!(out.stats.unnormalized_rows, 1);
        let bad = out.rows.iter().find(|r| r.indicator_name_raw.is_empty());
        assert!(bad.is_some());
        assert_eq!(bad.unwrap().status, STATUS_UNNORMALIZED);
    }

    #[test]
    fn 单位别名未覆盖落到未归一并计数() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室", "台"),
            row("科室二表", 7, "BK-01", "病案室", "台"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.stats.unnormalized_unit_rows, 2);
        assert_eq!(out.stats.unnormalized_rows, 2);
        // 归一本身不 panic，行照样在结果里
        assert_eq!(out.rows.len(), 2);
        assert!(out.rows.iter().all(|r| r.status == STATUS_UNNORMALIZED));
    }

    #[test]
    fn 生僻字不panic() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室（𠮷）", "%"),
            row("科室二表", 7, "BK-01", "病案室(𠮷)", "%"),
        ];
        let out = detect_rows(&mut rows);
        assert_eq!(out.pairs.len(), 1);
    }

    #[test]
    fn 同一份输入两次判定结果逐字相同() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "院感", "%"),
            row("科室二表", 7, "BK-01", "医院感染", "%"),
            row("科室三表", 3, "BK-02", "病案室", "%"),
        ];
        let first = detect_rows(&mut rows);
        let second = detect_rows(&mut rows);
        assert_eq!(first.pairs, second.pairs);
        assert_eq!(first.suspected, second.suspected);
    }
}
