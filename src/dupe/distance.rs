//! 重复检测四级漏斗 · 第三级：编辑距离候选（L3）。
//!
//! # 判据
//!
//! 归一化吸收不了的写法差异，用**字符级编辑距离**再判一次：
//! 「病案室」与「病案统计室」差两个字，像不像同一件事？
//!
//! 前两级各有各的盲区：
//!
//! - [L1 确定性键](super::exact) 按**字面**比 —— 差一个字就抓不到；
//! - [L2 归一化](super::normalize) 按**归一规则**比 —— 空白、全半角、别名能吸收，
//!   吸收不掉的多字／少字／改字它就放过了。
//!
//! 这一级补的正是那一块：**名字长得像，但归一没把它变成一样**。
//!
//! # 候选对的四道条件
//!
//! 1. **同一条业务记录**：业务主键（指标编号 + 科室代码 + 报告期 + 业务主键）相同；
//! 2. **数是同一个数**：分子 / 分母 / 填报值按数值相等 —— 数不一样是取值冲突，
//!    归实体绑定层的「待人工判定」，本级不碰（理由见模块底部的「铁律」）；
//! 3. **归一没吸收**：归一后的名称仍然不同（归一后一致的是 L2 的活）；
//! 4. **距离在阈值内**：字符级编辑距离 ≤ 阈值。
//!
//! # 阈值可配
//!
//! 阈值不是写死的常数 —— 院内指标名长短差别大，「差 2 个字」有时是同一件事、
//! 有时是两件事。取���顺序（**先到先得，后面的不覆盖前面的**）：
//!
//! 1. 环境变量 `REGVAL_DUP_DISTANCE_THRESHOLD`；
//! 2. 规则目录里的 `dupe-distance.json`（`{"threshold": 2, "max_length_gap": 40}`）；
//! 3. 内置默认值（阈值 2、长度差上限 40）。
//!
//! # 为什么要跳过长名称对
//!
//! 编辑距离是 O(n·m) 的两两比较。一张 5000 行的表里如果全是长名字，
//! 全量两两比较会把一次校验拖到跑不动 —— 而且**算出来的多半也没人看**。
//! 所以两���名称长度差超过上限（默认 40 字）的行对**直接跳过并计数**：
//! 「差 40 个字还叫同一件事」本来就说不通，跳过不丢人，但**不能静默跳**
//! —— 跳了多少对要看得见（[`DistanceStats::skipped_long_gap_pairs`]）。
//!
//! # 铁律
//!
//! **只标「疑似」，不合并、不推送。** 候选对进结果契约的 `suspected_duplicates`，
//! 行级标注进「疑似重复项」列，等经办人逐条勾选确认（确认流见任务 36）。
//!
//! 与实体绑定层不打架：取值不同的对**不出**候选对 —— 绑定层已经把它们标成
//! 「待人工判定」，同一对行不会一处说「疑似重复」、一处说「待人工判定」。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::{optional_usize, EnvSource};
use crate::dupe::{
    short_key, value_signature, values_signature, SuspectedDuplicate, PAIR_EXPANSION_MAX,
    STATE_SUSPECTED,
};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::key;
use crate::ingest::{Field, SourceRow};
use crate::rules::normalize::NameAliasTable;

/// 命中层级取值（编辑距离候选）
pub const MATCH_LEVEL_DISTANCE: &str = "distance";

/// L3 判定依据
pub const DISTANCE_BASIS: &str = "本产品最小数据格式与字段要求（业务主键 = 指标编号 + 科室代码 + 报告期 + 业务主键）；同一条业务记录下、归一后名称仍有差异的行按字符级编辑距离（中文按字算，不按字节）比较，距离在阈值内的行标「疑似重复」，并给出距离值与差异字说明，由经办人逐条勾选确认后才合并，本轮不合并、不上报";

/// 默认阈值：差 2 个字以内算「像」
pub const DEFAULT_THRESHOLD: usize = 2;

/// 默认长度差上限：两行名称长度差超过 40 字直接跳过
pub const DEFAULT_MAX_LENGTH_GAP: usize = 40;

/// 环境变量：编辑距离阈值
pub const ENV_THRESHOLD: &str = "REGVAL_DUP_DISTANCE_THRESHOLD";

/// 环境变量：名称长度差上限
pub const ENV_MAX_LENGTH_GAP: &str = "REGVAL_DUP_MAX_LENGTH_GAP";

/// 规则目录里的调参文件名（`{"threshold": 2, "max_length_gap": 40}`）
pub const CONFIG_FILE_NAME: &str = "dupe-distance.json";

/// 距离矩阵的规模上限（超过就不算这一对 —— 名字长到这份上已经不是「同一件事」了）
const MAX_MATRIX_CELLS: usize = 1_000_000;

/// L3 的运行参数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistanceConfig {
    /// 命中阈值（距离 ≤ 阈值算候选对）
    pub threshold: usize,
    /// 名称长度差上限（超过则跳过该行对并计数）
    pub max_length_gap: usize,
    /// 阈值来源（人话：给人看这轮用的是哪套阈值）
    pub source: String,
}

impl Default for DistanceConfig {
    fn default() -> Self {
        DistanceConfig {
            threshold: DEFAULT_THRESHOLD,
            max_length_gap: DEFAULT_MAX_LENGTH_GAP,
            source: format!(
                "内置默认值（阈值 {} · 长度差上限 {}）",
                DEFAULT_THRESHOLD, DEFAULT_MAX_LENGTH_GAP
            ),
        }
    }
}

impl DistanceConfig {
    /// 指定阈值的构造（单测与内部调用用）
    pub fn with_threshold(threshold: usize) -> Self {
        DistanceConfig {
            threshold,
            max_length_gap: DEFAULT_MAX_LENGTH_GAP,
            source: format!("调用方指定（阈值 {}）", threshold),
        }
    }
}

/// 取 L3 的运行参数。
///
/// 取值顺序：环境变量 → 规则目录 `dupe-distance.json` → 内置默认值。
/// 规则目录里的文件存在但写坏了 → **报错**（不静默回落到默认值 ——
/// 运维以为改了阈值、其实没生效，这种事比报错难查得多）。
pub fn resolve_config(
    src: &dyn EnvSource,
    rule_dir: Option<&Path>,
) -> Result<DistanceConfig, EngineError> {
    let mut threshold: Option<usize> = None;
    let mut max_length_gap: Option<usize> = None;
    let mut sources: Vec<String> = Vec::new();

    // 1. 环境变量
    if let Some(v) = optional_usize(src, ENV_THRESHOLD)? {
        threshold = Some(v);
        sources.push(format!("环境变量 {}", ENV_THRESHOLD));
    }
    if let Some(v) = optional_usize(src, ENV_MAX_LENGTH_GAP)? {
        max_length_gap = Some(v);
        let s = format!("环境变量 {}", ENV_MAX_LENGTH_GAP);
        if !sources.contains(&s) {
            sources.push(s);
        }
    }

    // 2. 规则目录里的调参文件（环境变量已定的项不覆盖）
    if let Some(dir) = rule_dir {
        let path = dir.join(CONFIG_FILE_NAME);
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                EngineError::at(
                    ErrorCode::ConfigInvalid,
                    format!("重复检测调参文件读取失败：{}", e),
                    path.display().to_string(),
                )
            })?;
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                EngineError::at(
                    ErrorCode::ConfigInvalid,
                    format!("重复检测调参文件解析失败：{}", e),
                    path.display().to_string(),
                )
            })?;
            let dir_source = format!("规则目录 {}", path.display());
            if threshold.is_none() {
                if let Some(v) = value.get("threshold") {
                    threshold = Some(parse_threshold(v, &path)?);
                    if !sources.contains(&dir_source) {
                        sources.push(dir_source.clone());
                    }
                }
            }
            if max_length_gap.is_none() {
                if let Some(v) = value.get("max_length_gap") {
                    max_length_gap = Some(parse_threshold(v, &path)?);
                    if !sources.contains(&dir_source) {
                        sources.push(dir_source);
                    }
                }
            }
        }
    }

    let threshold = threshold.unwrap_or(DEFAULT_THRESHOLD);
    let max_length_gap = max_length_gap.unwrap_or(DEFAULT_MAX_LENGTH_GAP);
    if sources.is_empty() {
        sources.push("内置默认值".to_string());
    }
    Ok(DistanceConfig {
        threshold,
        max_length_gap,
        source: format!(
            "{}（阈值 {} · 长度差上限 {}）",
            sources.join(" + "),
            threshold,
            max_length_gap
        ),
    })
}

/// 规则目录调参文件里的阈值字段：必须是非负整数
fn parse_threshold(value: &serde_json::Value, path: &Path) -> Result<usize, EngineError> {
    let raw = value.as_u64().ok_or_else(|| {
        EngineError::at(
            ErrorCode::ConfigInvalid,
            "重复检测调参文件里的阈值必须是非负整数",
            path.display().to_string(),
        )
    })?;
    if raw > usize::MAX as u64 {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!("重复检测调参文件里的阈值过大：{}", raw),
            path.display().to_string(),
        ));
    }
    Ok(raw as usize)
}

/// L3 的运行计数（可观测：结果契约与日志里都能看到）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistanceStats {
    /// 参与判定的行数（键要素齐全且指标名称非空）
    pub eligible_rows: usize,
    /// 键要素不全、未参与判定的行数
    pub skipped_rows: usize,
    /// 指标名称为空的行数（没有可比对的名称）
    pub skipped_empty_name_rows: usize,
    /// 同一条业务记录（业务主键）的分组数（组内 ≥2 行才算一组）
    pub groups: usize,
    /// 实际比过距离的行对数
    pub compared_pairs: usize,
    /// 命中阈值的候选对数
    pub pairs: usize,
    /// 距离超出阈值的行对数（只计数，不出候选）
    pub beyond_threshold_pairs: usize,
    /// 名称长度差过大被跳过的行对数（防两两比较拖垮一次校验）
    pub skipped_long_gap_pairs: usize,
    /// 名字太长、矩阵规模超上限而跳过的行对数
    pub skipped_oversized_pairs: usize,
    /// 行键相同的对被跳过（同一行不可能和自己重复）
    pub degenerate_pairs: usize,
    /// 与 L1 重叠的对数（字面完全一致，L1 已报）
    pub exact_overlap_pairs: usize,
    /// 与 L2 重叠的对数（归一后一致，L2 已报）
    pub normalize_overlap_pairs: usize,
    /// 数值不同的对数（归「待人工判定」，本级不碰）
    pub value_conflict_pairs: usize,
    /// 大桶折叠掉的对数（不静默截断）
    pub collapsed_pairs: usize,
}

/// 距离分布的一个桶
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistanceBucket {
    /// 分桶标签：距离 0 / 1 / 2 … 或「超过阈值」
    pub label: String,
    /// 距离值（超过阈值的桶为 `None`）
    pub distance: Option<usize>,
    /// 该桶的行对数
    pub pairs: usize,
}

/// L3 的产出
#[derive(Debug, Clone, Default)]
pub struct DistanceOutcome {
    /// 疑似重复候选对
    pub pairs: Vec<SuspectedDuplicate>,
    /// 行键 → 「疑似重复项」列取值
    pub suspected: BTreeMap<String, String>,
    /// 距离分布直方（按距离分桶的行对数）
    pub histogram: Vec<DistanceBucket>,
    /// 运行计数
    pub stats: DistanceStats,
}

/// L3 的运行参数与计数（进结果契约的那一段）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistanceStatusSection {
    /// 生效阈值
    pub threshold: usize,
    /// 阈值来源
    pub threshold_source: String,
    /// 名称长度差上限
    pub max_length_gap: usize,
    /// 候选对数
    pub pairs: usize,
    /// 距离分布直方
    pub histogram: Vec<DistanceBucket>,
    /// 运行计数
    pub stats: DistanceStats,
}

/// 一行在 L3 眼里长什么样
#[derive(Debug, Clone, PartialEq, Eq)]
struct DistanceRow {
    row_key: String,
    reference: String,
    business_key: String,
    name_raw: String,
    name_normalized: String,
    /// 比较键（归一 + 去空白 + 大小写折叠）
    compare_key: String,
}

/// 编辑距离候选检测（L3）。
///
/// 判定步骤：
///
/// 1. 逐行取指标名称（原值 + 归一后 + 比较键）；键要素不全或名称为空的行不参与（并计数）；
/// 2. 按业务主键分组 —— 同一条业务记录才可能是「填了两遍」；
/// 3. 组内两两比：字面一致归 L1、数值冲突归绑定层、归一后一致归 L2，
///    剩下的才算编辑距离（长度差过大 / 矩阵超上限的直接跳过并计数）；
/// 4. 距离 ≤ 阈值的记候选对，并给两行各记一条疑似对象。
pub fn detect(rows: &[SourceRow], cfg: &DistanceConfig) -> DistanceOutcome {
    let mut outcome = DistanceOutcome::default();
    let mut prepared: Vec<Option<DistanceRow>> = Vec::with_capacity(rows.len());
    let mut eligible: Vec<usize> = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        prepared.push(None);
        if !key::is_bindable(row) {
            outcome.stats.skipped_rows += 1;
            continue;
        }
        let name_raw = trimmed(row, Field::IndicatorName);
        if name_raw.is_empty() {
            // 没有名字可比：本级不参与，但计数留痕（不静默丢行）
            outcome.stats.skipped_empty_name_rows += 1;
            continue;
        }
        let name_normalized = name_alias_table().canonicalize(&name_raw);
        let compare_key = name_alias_table().canonical_key(&name_raw);
        outcome.stats.eligible_rows += 1;
        prepared[i] = Some(DistanceRow {
            row_key: key::row_uid(row),
            reference: reference(row),
            business_key: key::business_key(row),
            name_raw,
            name_normalized,
            compare_key,
        });
        eligible.push(i);
    }

    // 按业务主键分组：同一条业务记录
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &eligible {
        groups
            .entry(key::business_key(&rows[i]))
            .or_default()
            .push(i);
    }

    let mut peers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // 距离 → 行对数（直方用）
    let mut distance_counts: BTreeMap<usize, usize> = BTreeMap::new();

    for members in groups.values() {
        if members.len() < 2 {
            continue;
        }
        outcome.stats.groups += 1;

        // 稳定顺序：按行键排序，同一份输入两次判定结果逐字相同
        let mut ordered = members.clone();
        ordered.sort_by_key(|&i| key::row_uid(&rows[i]));

        let n = ordered.len();
        let pairs: Vec<(usize, usize)> = if n <= PAIR_EXPANSION_MAX {
            let mut all = Vec::new();
            for a in 0..n {
                for b in (a + 1)..n {
                    all.push((ordered[a], ordered[b]));
                }
            }
            all
        } else {
            // 星形：首行与其余各行成对；折叠掉的对数记进计数，不静默截断
            let anchor = ordered[0];
            let all: Vec<(usize, usize)> = ordered
                .iter()
                .skip(1)
                .map(|&other| (anchor, other))
                .collect();
            outcome.stats.collapsed_pairs += n * (n - 1) / 2 - (n - 1);
            all
        };

        for (a, b) in pairs {
            consider_pair(
                &mut outcome,
                &mut peers,
                &mut distance_counts,
                rows,
                &prepared,
                a,
                b,
                cfg,
            );
        }
    }

    // 距离分布直方：0..=阈值 各一桶，再加一桶「超过阈值」
    for distance in 0..=cfg.threshold {
        let pairs = distance_counts.get(&distance).copied().unwrap_or(0);
        outcome.histogram.push(DistanceBucket {
            label: format!("距离 {}", distance),
            distance: Some(distance),
            pairs,
        });
    }
    let beyond: usize = distance_counts
        .iter()
        .filter(|(d, _)| **d > cfg.threshold)
        .map(|(_, c)| *c)
        .sum();
    if beyond > 0 {
        outcome.histogram.push(DistanceBucket {
            label: "超过阈值".to_string(),
            distance: None,
            pairs: beyond,
        });
    }

    // 「疑似重复项」列取值：每行都写清「和谁像、像在哪」
    for (row_key, list) in peers {
        outcome
            .suspected
            .insert(row_key, format!("疑似重复：与 {}", list.join("、")));
    }
    outcome.stats.pairs = outcome.pairs.len();

    outcome
}

/// 判定一对行是不是「编辑距离候选」
fn consider_pair(
    outcome: &mut DistanceOutcome,
    peers: &mut BTreeMap<String, Vec<String>>,
    distance_counts: &mut BTreeMap<usize, usize>,
    rows: &[SourceRow],
    prepared: &[Option<DistanceRow>],
    a: usize,
    b: usize,
    cfg: &DistanceConfig,
) {
    let row_a = prepared[a].as_ref().expect("参与判定的行必有准备结果");
    let row_b = prepared[b].as_ref().expect("参与判定的行必有准备结果");

    // 行键相同 = 其实是同一行，自己和自己不构成一对
    if row_a.row_key == row_b.row_key {
        outcome.stats.degenerate_pairs += 1;
        return;
    }
    // 字面完全一致 → L1 已经报了，本级不重复报（计数留痕）
    if value_signature(&rows[a]) == value_signature(&rows[b]) {
        outcome.stats.exact_overlap_pairs += 1;
        return;
    }
    // 数不一样 → 取值冲突，归实体绑定层的「待人工判定」，本级不碰（计数留痕）
    if values_signature(&rows[a]) != values_signature(&rows[b]) {
        outcome.stats.value_conflict_pairs += 1;
        return;
    }
    // 归一后一致 → L2 已经报了，本级不重复报（计数留痕）
    if row_a.compare_key == row_b.compare_key {
        outcome.stats.normalize_overlap_pairs += 1;
        return;
    }

    // 长度差过大 → 直接跳过并计数（不静默丢）
    let gap = row_a
        .compare_key
        .chars()
        .count()
        .abs_diff(row_b.compare_key.chars().count());
    if gap > cfg.max_length_gap {
        outcome.stats.skipped_long_gap_pairs += 1;
        return;
    }

    // 算编辑距离（矩阵规模超上限的直接跳过并计数）
    let (distance, detail) = match distance_and_detail(&row_a.compare_key, &row_b.compare_key) {
        Some(v) => v,
        None => {
            outcome.stats.skipped_oversized_pairs += 1;
            return;
        }
    };
    outcome.stats.compared_pairs += 1;
    *distance_counts.entry(distance).or_insert(0) += 1;

    if distance > cfg.threshold {
        outcome.stats.beyond_threshold_pairs += 1;
        return;
    }

    // 左小右大：同一对行不论谁先算出来，顺序都一样
    let (left, right, left_row, right_row) = if row_a.row_key <= row_b.row_key {
        (a, b, row_a, row_b)
    } else {
        (b, a, row_b, row_a)
    };

    outcome.pairs.push(SuspectedDuplicate {
        left_row_key: left_row.row_key.clone(),
        right_row_key: right_row.row_key.clone(),
        match_level: MATCH_LEVEL_DISTANCE.to_string(),
        basis: DISTANCE_BASIS.to_string(),
        business_key: left_row.business_key.clone(),
        left_reference: left_row.reference.clone(),
        right_reference: right_row.reference.clone(),
        cross_table: rows[left].source_ns != rows[right].source_ns,
        state: STATE_SUSPECTED.to_string(),
        left_original: format!("指标名称={}", left_row.name_raw),
        right_original: format!("指标名称={}", right_row.name_raw),
        left_normalized: format!("指标名称={}", left_row.name_normalized),
        right_normalized: format!("指标名称={}", right_row.name_normalized),
        normalize_detail: detail.clone(),
        distance: Some(distance),
        distance_detail: detail.clone(),
        // 编辑距离这一级仍是确定性判据：候选来源是确定性
        source: crate::dupe::default_candidate_source(),

        semantic_kind: String::new(),
        semantic_confidence: None,
        semantic_reason: String::new(),
        semantic_evidence: Vec::new(),
        mergeable: crate::dupe::default_mergeable(),
    });

    peers
        .entry(left_row.row_key.clone())
        .or_default()
        .push(format!(
            "{}（行键 {}）（字符级编辑距离 {}：{}）",
            right_row.reference,
            short_key(&right_row.row_key),
            distance,
            detail
        ));
    peers
        .entry(right_row.row_key.clone())
        .or_default()
        .push(format!(
            "{}（行键 {}）（字符级编辑距离 {}：{}）",
            left_row.reference,
            short_key(&left_row.row_key),
            distance,
            detail
        ));
}

/// 字符级编辑距离（Levenshtein）。
///
/// **按字符不按字节**：一个汉字算 1，不算 3。按字节算的话，
/// 「改了一个字」会变成「改了三个」，阈值就失去意义了。
pub fn edit_distance(a: &str, b: &str) -> usize {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.is_empty() {
        return y.len();
    }
    if y.is_empty() {
        return x.len();
    }
    // 剥掉公共前后缀 —— 名字通常只差中间一两个字，剥完矩阵小得多。
    // 剥掉的前后缀是**已经对上的字**，不计入距离（距离只看还没对上的那段）。
    let (_pre, xs, ys, _suf) = trim_common(&x, &y);
    levenshtein(&xs, &ys)
}

/// 剥公共前后缀，返回（公共前缀长度、剩余左段、剩余右段、公共后缀长度）
fn trim_common(x: &[char], y: &[char]) -> (usize, Vec<char>, Vec<char>, usize) {
    let mut pre = 0usize;
    while pre < x.len() && pre < y.len() && x[pre] == y[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < x.len() - pre && suf < y.len() - pre && x[x.len() - 1 - suf] == y[y.len() - 1 - suf]
    {
        suf += 1;
    }
    (
        pre,
        x[pre..x.len() - suf].to_vec(),
        y[pre..y.len() - suf].to_vec(),
        suf,
    )
}

/// 标准 Levenshtein（插入 / 删除 / 替换各算 1）
fn levenshtein(x: &[char], y: &[char]) -> usize {
    let n = x.len();
    let m = y.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if x[i - 1] == y[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// 距离 + 差异说明（哪几个字不同）。
///
/// 差异说明用**最长公共子序列**对齐得出：公共部分之外的字，
/// 一边多出来的说「左行多「…」」／「右行多「…」」，两边等长且对不上的
/// 说「第 N 字不同：「X」↔「Y」」。这套说法是给经办人看的 ——
/// 「右行多「统计」」比「第 3 字不同、第 4 字不同」好懂。
///
/// 矩阵规模超上限时返回 `None`（调用方跳过这一对并计数）。
fn distance_and_detail(left: &str, right: &str) -> Option<(usize, String)> {
    let x: Vec<char> = left.chars().collect();
    let y: Vec<char> = right.chars().collect();
    if x.len().saturating_mul(y.len()) > MAX_MATRIX_CELLS {
        return None;
    }
    let distance = edit_distance(left, right);
    let detail = difference_detail_chars(&x, &y);
    Some((distance, detail))
}

/// 差异说明（最长公共子序列对齐）
pub fn difference_detail(left: &str, right: &str) -> String {
    let x: Vec<char> = left.chars().collect();
    let y: Vec<char> = right.chars().collect();
    difference_detail_chars(&x, &y)
}

/// 差异说明（字符切片版）
///
/// 对齐用**最长公共子序列**：公共的字是「对上了的」，锚点之间的空隙才是差异。
/// 两个锚点之间，左右两段各有几个字是**对齐定死的**（与回溯时先删还是先插无关），
/// 所以描述是确定的、可复现的：
///
/// - 一边有、另一边没有 → 「左行多「…」」／「右行多「…」」；
/// - 两边都有且等长 → 逐字「第 N 字不同：「X」↔「Y」」（N 是字在**左侧名称**里的位置）。
fn difference_detail_chars(x: &[char], y: &[char]) -> String {
    let mut parts: Vec<String> = Vec::new();

    // 最长公共子序列（回溯出对齐）
    let n = x.len();
    let m = y.len();
    let mut dp: Vec<Vec<u32>> = vec![vec![0; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if x[i - 1] == y[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    // 回溯出对齐步（Match / Del / Ins），再按锚点之间的空隙分段
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Match,
        Del,
        Ins,
    }
    let mut steps: Vec<Step> = Vec::with_capacity(n + m);
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && x[i - 1] == y[j - 1] {
            steps.push(Step::Match);
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            steps.push(Step::Ins);
            j -= 1;
        } else {
            steps.push(Step::Del);
            i -= 1;
        }
    }
    steps.reverse();

    // 按锚点分段：每个空隙记（左侧多出来的字、右侧多出来的字、左侧起始位置）
    let mut gaps: Vec<(Vec<char>, Vec<char>, usize)> = Vec::new();
    let mut left_run: Vec<char> = Vec::new();
    let mut right_run: Vec<char> = Vec::new();
    let mut left_pos = 0usize;
    let mut gap_start = 0usize;
    let mut x_cursor = 0usize;
    let mut y_cursor = 0usize;
    for step in steps {
        match step {
            Step::Match => {
                if !left_run.is_empty() || !right_run.is_empty() {
                    gaps.push((
                        std::mem::take(&mut left_run),
                        std::mem::take(&mut right_run),
                        gap_start,
                    ));
                }
                x_cursor += 1;
                y_cursor += 1;
                left_pos = x_cursor;
                gap_start = left_pos;
            }
            Step::Del => {
                left_run.push(x[x_cursor]);
                x_cursor += 1;
            }
            Step::Ins => {
                right_run.push(y[y_cursor]);
                y_cursor += 1;
            }
        }
    }
    if !left_run.is_empty() || !right_run.is_empty() {
        gaps.push((left_run, right_run, gap_start));
    }
    let _ = left_pos;

    // 逐段描述
    for (left, right, start) in gaps {
        if left.is_empty() && right.is_empty() {
            continue;
        }
        if left.is_empty() {
            parts.push(format!("右行多「{}」", right.iter().collect::<String>()));
            continue;
        }
        if right.is_empty() {
            parts.push(format!("左行多「{}」", left.iter().collect::<String>()));
            continue;
        }
        let common = left.len().min(right.len());
        for k in 0..common {
            parts.push(format!(
                "第 {} 字不同：「{}」↔「{}」",
                start + k + 1,
                left[k],
                right[k]
            ));
        }
        if left.len() > common {
            parts.push(format!(
                "左行多「{}」",
                left[common..].iter().collect::<String>()
            ));
        }
        if right.len() > common {
            parts.push(format!(
                "右行多「{}」",
                right[common..].iter().collect::<String>()
            ));
        }
    }

    if parts.is_empty() {
        return "字符完全一致".to_string();
    }
    parts.join("、")
}

/// L3 的结果转成契约里的那一段
impl DistanceOutcome {
    /// 运行参数 + 计数 + 直方（进结果契约）
    pub fn status_section(&self, cfg: &DistanceConfig) -> DistanceStatusSection {
        status_section_from(&self.stats, &self.histogram, self.pairs.len(), cfg)
    }
}

/// 由运行计数与直方拼出进结果契约的那一段。
///
/// 单独给一个自由函数：漏斗编排层拿得到「计数 + 直方」，但拿不到整份产出对象
/// （它只留候选对与行级标注）—— 契约那一段仍然要完整。
pub fn status_section_from(
    stats: &DistanceStats,
    histogram: &[DistanceBucket],
    pairs: usize,
    cfg: &DistanceConfig,
) -> DistanceStatusSection {
    DistanceStatusSection {
        threshold: cfg.threshold,
        threshold_source: cfg.source.clone(),
        max_length_gap: cfg.max_length_gap,
        pairs,
        histogram: histogram.to_vec(),
        stats: stats.clone(),
    }
}

/// 行的可读引用："表名 第 N 行"
fn reference(row: &SourceRow) -> String {
    format!("{} 第 {} 行", row.source_ns, row.row_no)
}

/// 取值（去首尾空白；空串算空）
fn trimmed(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

/// 内置指标名称别名表（只建一次）
fn name_alias_table() -> &'static NameAliasTable {
    static TABLE: OnceLock<NameAliasTable> = OnceLock::new();
    TABLE.get_or_init(NameAliasTable::builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source_ns: &str, row_no: usize, business_key: &str, name: &str) -> SourceRow {
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
        values.insert(Field::Unit, "%".to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    fn detect_rows(rows: &mut Vec<SourceRow>, cfg: &DistanceConfig) -> DistanceOutcome {
        key::assign_dup_ordinals(rows);
        detect(rows, cfg)
    }

    #[test]
    fn 编辑距离按字符不按字节() {
        // 一个汉字 3 个字节：按字节算是 3，按字符算是 1
        assert_eq!(edit_distance("病案室", "病案检室"), 1);
        assert_eq!(edit_distance("病案室", "病案统计室"), 2);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("", "病案室"), 3);
        assert_eq!(edit_distance("病案室", "病案室"), 0);
    }

    #[test]
    fn 差异说明说清哪几个字不同() {
        let detail = difference_detail("病案室", "病案统计室");
        assert!(
            detail.contains("右行多"),
            "应说明右边多出哪些字：{}",
            detail
        );
        assert!(detail.contains("统计"), "应点名多出来的字：{}", detail);
        // 两边等长且对不上 → 逐字说明，位置是字在左侧名称里的位置
        let detail = difference_detail("甲乙丙", "甲丁丙");
        assert!(
            detail.contains("第 2 字不同"),
            "应说明哪一字不同：{}",
            detail
        );
        assert!(
            detail.contains("乙") && detail.contains("丁"),
            "应点名两个字：{}",
            detail
        );
    }

    #[test]
    fn 阈值内的出候选对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", "病案统计室"),
        ];
        let out = detect_rows(&mut rows, &DistanceConfig::with_threshold(2));
        assert_eq!(out.pairs.len(), 1);
        assert_eq!(out.pairs[0].match_level, MATCH_LEVEL_DISTANCE);
        assert_eq!(out.pairs[0].distance, Some(2));
        assert!(!out.pairs[0].distance_detail.is_empty());
        assert_eq!(out.pairs[0].state, STATE_SUSPECTED);
    }

    #[test]
    fn 阈值外的只计数不出候选对() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", "病案统计室"),
        ];
        let out = detect_rows(&mut rows, &DistanceConfig::with_threshold(1));
        assert!(out.pairs.is_empty(), "距离 2 超出阈值 1");
        assert_eq!(out.stats.beyond_threshold_pairs, 1);
        assert_eq!(out.stats.compared_pairs, 1);
    }

    #[test]
    fn 字面一致与归一后一致的不在本级重复报() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", "病案室"),
            row("科室三表", 3, "BK-02", "院感"),
            row("科室三表", 4, "BK-02", "医院感染"),
        ];
        let out = detect_rows(&mut rows, &DistanceConfig::with_threshold(2));
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.exact_overlap_pairs, 1);
        assert_eq!(out.stats.normalize_overlap_pairs, 1);
    }

    #[test]
    fn 数值不同的对不出候选() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", "病案统计室"),
        ];
        rows[1].values.insert(Field::Value, "60".to_string());
        let out = detect_rows(&mut rows, &DistanceConfig::with_threshold(2));
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.value_conflict_pairs, 1);
    }

    #[test]
    fn 长度差过大的对被跳过并计数() {
        let long_name = "病案".to_string() + &"质量管理科".repeat(10);
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", &long_name),
        ];
        let out = detect_rows(&mut rows, &DistanceConfig::with_threshold(40));
        assert!(out.pairs.is_empty());
        assert_eq!(out.stats.skipped_long_gap_pairs, 1);
    }

    #[test]
    fn 距离分布直方可复现() {
        let mut rows = vec![
            row("科室一表", 1, "BK-01", "病案室"),
            row("科室二表", 7, "BK-01", "病案统计室"),
            row("科室三表", 3, "BK-02", "病案室"),
            row("科室三表", 4, "BK-02", "病案检室"),
        ];
        let first = detect_rows(&mut rows, &DistanceConfig::with_threshold(2));
        let second = detect_rows(&mut rows, &DistanceConfig::with_threshold(2));
        assert_eq!(first.histogram, second.histogram);
        assert_eq!(first.pairs, second.pairs);
        assert!(first
            .histogram
            .iter()
            .any(|b| b.distance == Some(2) && b.pairs == 1));
        assert!(first
            .histogram
            .iter()
            .any(|b| b.distance == Some(1) && b.pairs == 1));
    }

    #[test]
    fn 阈值来源可配且规则目录可覆盖() {
        use crate::config::MapEnv;
        let mut env = MapEnv::new();
        env.set(ENV_THRESHOLD, "3");
        let cfg = resolve_config(&env, None).expect("应解析出阈值");
        assert_eq!(cfg.threshold, 3);
        assert!(cfg.source.contains(ENV_THRESHOLD));
    }

    #[test]
    fn 规则目录里的调参文件可配阈值且环境变量优先() {
        use crate::config::MapEnv;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, "{\"threshold\": 4, \"max_length_gap\": 10}").unwrap();

        // 规则目录里的文件生效
        let cfg = resolve_config(&MapEnv::new(), Some(dir.path())).expect("应解析出阈值");
        assert_eq!(cfg.threshold, 4);
        assert_eq!(cfg.max_length_gap, 10);
        assert!(
            cfg.source.contains("规则目录"),
            "来源要写明：{}",
            cfg.source
        );

        // 环境变量优先于规则目录
        let mut env = MapEnv::new();
        env.set(ENV_THRESHOLD, "1");
        let cfg = resolve_config(&env, Some(dir.path())).expect("应解析出阈值");
        assert_eq!(cfg.threshold, 1);
        assert_eq!(cfg.max_length_gap, 10, "环境变量没定的项仍取规则目录的值");
    }

    #[test]
    fn 调参文件写坏了要报错不静默回落默认值() {
        use crate::config::MapEnv;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        std::fs::write(&path, "{\"threshold\": \"两\"}").unwrap();
        let err = resolve_config(&MapEnv::new(), Some(dir.path())).expect_err("应报错");
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }
}
