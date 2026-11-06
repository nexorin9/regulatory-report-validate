//! 重复检测四级漏斗的标注基准集与逐级召回率 / 精确率评估。
//!
//! # 这个模块回答一个问题
//!
//! 「多报一级的漏斗，值不值？」
//!
//! 第四级是模型级语义归并：它多认得一些写法差异很大的重复，也可能多认错。
//! 「模型级大概更准」这句话没有意义 —— 要有数字。数字从哪来？
//! **从一份带人工标注的对照基准集来。**
//!
//! # 基准集怎么用
//!
//! 1. `samples/bench/tables/` 里放几张脱敏样例表（只含指标级监管数据，无患者明细）；
//! 2. `samples/bench/labels.json` 里对**成对的行**给出人工判定；
//! 3. 漏斗跑一遍，逐级算召回率与精确率，落一张可核对的对比表。
//!
//! # 三种标注分别是什么意思
//!
//! | 标注 | 含义 | 漏斗该不该报 |
//! |---|---|---|
//! | `duplicate_record` | 同一件事被填了两遍 | **应该报**（正例） |
//! | `same_indicator` | 同一个指标，但不是同一件事（例如两个院区各报各的） | 不该报（报了算误报） |
//! | `not_duplicate` | 明确不是同一件事（例如业务主键不同，代表两条记录） | 不该报（报了算误报） |
//!
//! # 指标口径（写死在这里，不留第二种解释）
//!
//! - **召回率** = 命中真重复标注的候选对数 ÷ 基准集里真重复标注的对数。
//!   分母由标注给定 —— **漏斗没报出来的真重复就是漏**。
//! - **精确率** = 命中真重复标注的候选对数 ÷ 该级累计候选对数。
//!   **候选里没有标注的对一律算误报**（没标注不等于对，宁可严）。
//! - 每一级的指标都是**累计口径**：算的是「跑到这一级为止一共报了多少」。
//!   逐级新增多少单独有一列（`本级新增`）—— 两个数一起看才知道
//!   「这一级加进来多少、值不值」。
//!
//! # 标注文件里为什么写「表 + 行号」而不是行键
//!
//! 行键是算出来的摘要（来源命名空间 + 业务主键 + 同键行序的 sha256），
//! 人读不懂、改一行数据就得重算。标注文件里写「科室一表 第 3 行」这类
//! **可读定位**，装载时再解析成行键 —— 对齐仍然严格按行键走，
//! 但标注文件是人能读能改的。定位不到行就报错（不静默跳过），
//! 否则标注文件里一个错字会悄悄把召回率抬高。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dupe::{pair_key, FunnelLevel, FunnelOutcome, SuspectedDuplicate};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::{key, SourceRow};

/// 标注取值：同一件事被填了两遍（漏斗应当报出来）
pub const LABEL_DUPLICATE_RECORD: &str = "duplicate_record";
/// 标注取值：同一个指标，但不是同一件事（漏斗不应报出来）
pub const LABEL_SAME_INDICATOR: &str = "same_indicator";
/// 标注取值：明确不是同一件事（漏斗不应报出来）
pub const LABEL_NOT_DUPLICATE: &str = "not_duplicate";

/// 标注文件里允许的取值全集
pub const LABELS: [&str; 3] = [
    LABEL_DUPLICATE_RECORD,
    LABEL_SAME_INDICATOR,
    LABEL_NOT_DUPLICATE,
];

/// 标注里的一行定位：来源表 + 表内行号
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RowLocator {
    /// 来源表命名空间（CSV 取文件名主干，Excel 取工作表名）
    pub source: String,
    /// 表内数据行序（从 1 起，不含表头）
    pub row: usize,
}

impl RowLocator {
    /// 人话（报错时点名到行）
    pub fn describe(&self) -> String {
        format!("{} 第 {} 行", self.source, self.row)
    }
}

/// 一条人工标注
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BenchLabel {
    /// 左侧行的定位
    pub left: RowLocator,
    /// 右侧行的定位
    pub right: RowLocator,
    /// 标注取值（三选一）
    pub label: String,
    /// 判定依据（为什么这么标）
    #[serde(default)]
    pub basis: String,
    /// 备注
    #[serde(default)]
    pub note: String,
}

/// 标注文件（格式固定；样例见 `samples/bench/labels.json`）
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BenchSet {
    /// 基准集编号
    pub bench_set: String,
    /// 基准集说明
    #[serde(default)]
    pub description: String,
    /// 标注清单
    pub labels: Vec<BenchLabel>,
}

/// 定位解析后的一条标注（左右都换成了行键）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLabel {
    /// 左侧行键
    pub left_row_key: String,
    /// 右侧行键
    pub right_row_key: String,
    /// 左侧行可读引用
    pub left_reference: String,
    /// 右侧行可读引用
    pub right_reference: String,
    /// 标注取值
    pub label: String,
    /// 判定依据
    pub basis: String,
    /// 备注
    pub note: String,
}

impl ResolvedLabel {
    /// 标注对的规范键（与漏斗候选对同一把尺子）
    pub fn pair_key(&self) -> String {
        if self.left_row_key <= self.right_row_key {
            format!("{}|{}", self.left_row_key, self.right_row_key)
        } else {
            format!("{}|{}", self.right_row_key, self.left_row_key)
        }
    }

    /// 是否为真重复标注（正例）
    pub fn is_true_duplicate(&self) -> bool {
        self.label == LABEL_DUPLICATE_RECORD
    }
}

/// 解析后的基准集
#[derive(Debug, Clone, Default)]
pub struct ResolvedBench {
    /// 基准集编号
    pub bench_set: String,
    /// 基准集说明
    pub description: String,
    /// 解析后的标注
    pub labels: Vec<ResolvedLabel>,
    /// 参与比对的行数
    pub total_rows: usize,
}

impl ResolvedBench {
    /// 真重复标注的对数（召回率的分母）
    pub fn true_pair_count(&self) -> usize {
        self.labels.iter().filter(|l| l.is_true_duplicate()).count()
    }

    /// 负例标注的对数
    pub fn negative_pair_count(&self) -> usize {
        self.labels.len() - self.true_pair_count()
    }

    /// 标注对的规范键 → 标注
    pub fn by_pair(&self) -> BTreeMap<String, &ResolvedLabel> {
        self.labels.iter().map(|l| (l.pair_key(), l)).collect()
    }
}

/// 读取标注文件。
///
/// 文件读不出来、JSON 解析失败、标注取值不认识 —— 三种都报错，不静默跳过。
/// 静默跳过会让基准集悄悄变小，召回率反而变好看。
pub fn load_bench(path: &Path) -> Result<BenchSet, EngineError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        EngineError::at(
            ErrorCode::InputUnreadable,
            format!("标注基准集读取失败：{}", e),
            path.display().to_string(),
        )
    })?;
    let set: BenchSet = serde_json::from_str(&raw).map_err(|e| {
        EngineError::at(
            ErrorCode::BenchSetInvalid,
            format!("标注基准集解析失败：{}", e),
            path.display().to_string(),
        )
    })?;
    if set.bench_set.trim().is_empty() {
        return Err(EngineError::at(
            ErrorCode::BenchSetInvalid,
            "标注基准集缺 bench_set 字段（写明这是哪一套基准集）",
            path.display().to_string(),
        ));
    }
    for (i, label) in set.labels.iter().enumerate() {
        if !LABELS.contains(&label.label.as_str()) {
            return Err(EngineError::at(
                ErrorCode::BenchSetInvalid,
                format!(
                    "第 {} 条标注的取值「{}」不认识（只能是 {}）",
                    i + 1,
                    label.label,
                    LABELS.join(" / ")
                ),
                path.display().to_string(),
            ));
        }
        if label.left == label.right {
            return Err(EngineError::at(
                ErrorCode::BenchSetInvalid,
                format!("第 {} 条标注的左右两行是同一行", i + 1),
                path.display().to_string(),
            ));
        }
    }
    Ok(set)
}

/// 把标注里的可读定位解析成行键。
///
/// 定位不到行 → 报错并点名定位（「标注文件里改了一行数据，行号就变了」这类
/// 问题要看得见，不能靠漏掉一条标注蒙混过去）。
pub fn resolve(bench: &BenchSet, rows: &[SourceRow]) -> Result<ResolvedBench, EngineError> {
    // (来源命名空间, 行号) → 行键
    let mut index: BTreeMap<(String, usize), String> = BTreeMap::new();
    for row in rows {
        let uid = key::row_uid(row);
        let slot = (row.source_ns.clone(), row.row_no);
        if let Some(prev) = index.get(&slot) {
            if prev != &uid {
                return Err(EngineError::at(
                    ErrorCode::BenchSetInvalid,
                    format!(
                        "同一来源「{} 第 {} 行」算出了两个不同的行键，基准集对不上",
                        row.source_ns, row.row_no
                    ),
                    row.source_file.clone(),
                ));
            }
        }
        index.insert(slot, uid);
    }

    let mut labels = Vec::with_capacity(bench.labels.len());
    for label in &bench.labels {
        let left_key = lookup(&index, &label.left)?;
        let right_key = lookup(&index, &label.right)?;
        if left_key == right_key {
            return Err(EngineError::at(
                ErrorCode::BenchSetInvalid,
                format!("标注的左右两行指向同一行（{}）", label.left.describe()),
                label.left.describe(),
            ));
        }
        labels.push(ResolvedLabel {
            left_row_key: left_key,
            right_row_key: right_key,
            left_reference: label.left.describe(),
            right_reference: label.right.describe(),
            label: label.label.clone(),
            basis: label.basis.clone(),
            note: label.note.clone(),
        });
    }

    Ok(ResolvedBench {
        bench_set: bench.bench_set.clone(),
        description: bench.description.clone(),
        labels,
        total_rows: rows.len(),
    })
}

fn lookup(
    index: &BTreeMap<(String, usize), String>,
    locator: &RowLocator,
) -> Result<String, EngineError> {
    index
        .get(&(locator.source.clone(), locator.row))
        .cloned()
        .ok_or_else(|| {
            EngineError::at(
                ErrorCode::BenchSetInvalid,
                format!(
                    "标注里的定位「{}」在样例表里找不到（行号对不上：表头算不算第一行、改过表要同步改标注）",
                    locator.describe()
                ),
                locator.describe(),
            )
        })
}

/// 一级的评估指标（累计口径）
#[derive(Debug, Clone, PartialEq)]
pub struct LevelMetrics {
    /// 层级
    pub level: String,
    /// 层级中文名
    pub level_title: String,
    /// 本级新增候选对数
    pub new_pairs: usize,
    /// 累计候选对数（跑到这一级为止一共报了多少）
    pub candidate_pairs: usize,
    /// 命中真重复标注的候选对数
    pub hit_labels: usize,
    /// 误报数（候选里不是真重复标注的）
    pub false_positives: usize,
    /// 误报细分：同一指标但不是同一件事
    pub fp_same_indicator: usize,
    /// 误报细分：明确不是同一件事
    pub fp_not_duplicate: usize,
    /// 误报细分：基准集里没有标注
    pub fp_unlabeled: usize,
    /// 召回率（命中真重复标注 ÷ 基准集真重复标注总数）
    pub recall: Option<f64>,
    /// 精确率（命中真重复标注 ÷ 累计候选对数）
    pub precision: Option<f64>,
    /// 本级说明
    pub note: String,
}

/// 漏掉的真重复标注（漏斗没报出来的那几对）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissedPair {
    /// 左侧行可读引用
    pub left_reference: String,
    /// 右侧行可读引用
    pub right_reference: String,
    /// 判定依据
    pub basis: String,
    /// 说明（为什么没报出来）
    pub reason: String,
}

/// 一级的边际增益（相对上一级）
#[derive(Debug, Clone, PartialEq)]
pub struct MarginalGain {
    /// 层级
    pub level: String,
    /// 本级新增候选对数
    pub new_pairs: usize,
    /// 召回率变化
    pub recall_delta: Option<f64>,
    /// 精确率变化
    pub precision_delta: Option<f64>,
    /// 说明
    pub note: String,
}

/// 一份评估报告
#[derive(Debug, Clone, PartialEq)]
pub struct EvalReport {
    /// 基准集编号
    pub bench_set: String,
    /// 基准集说明
    pub description: String,
    /// 参与比对的行数
    pub total_rows: usize,
    /// 标注总条数
    pub labeled_pairs: usize,
    /// 真重复标注条数
    pub labeled_true_pairs: usize,
    /// 负例标注条数
    pub labeled_negative_pairs: usize,
    /// 逐级指标
    pub levels: Vec<LevelMetrics>,
    /// 漏掉的真重复标注
    pub missed_true_pairs: Vec<MissedPair>,
    /// 末级相对上一级的边际增益
    pub marginal_gain: Option<MarginalGain>,
    /// 结论（一段话，CLI 与文档都引它）
    pub conclusion: String,
}

/// 按级算召回率与精确率。
///
/// 每一级都算**累计口径**（跑到这一级为止一共报了多少），
/// 本级新增多少单独一列 —— 两个数一起看才知道这一级值不值。
pub fn evaluate(bench: &ResolvedBench, funnel: &FunnelOutcome) -> EvalReport {
    let total_true = bench.true_pair_count();

    let mut levels: Vec<LevelMetrics> = Vec::new();
    let mut cumulative: BTreeMap<String, &SuspectedDuplicate> = BTreeMap::new();
    let mut hit_true: BTreeSet<String> = BTreeSet::new();

    for level_result in &funnel.levels {
        for pair in &level_result.new_pairs {
            cumulative.insert(pair_key(pair), pair);
        }
        levels.push(level_metrics(
            bench,
            level_result.level.match_level(),
            level_result.level.title(),
            level_result.new_pair_count(),
            &cumulative,
            &mut hit_true,
            &level_result.note,
        ));
    }

    // 漏掉的真重复：标注说是重复，漏斗从头到尾没报出来
    let missed = missed_true_pairs(bench, &hit_true, "四级候选里都没有这一对 —— 现有判据认不出它");

    // 末级相对上一级的边际增益
    let marginal_gain = marginal_gain_of(&levels, funnel);
    let conclusion = build_conclusion(&levels, &missed, marginal_gain.as_ref(), bench);

    EvalReport {
        bench_set: bench.bench_set.clone(),
        description: bench.description.clone(),
        total_rows: bench.total_rows,
        labeled_pairs: bench.labels.len(),
        labeled_true_pairs: total_true,
        labeled_negative_pairs: bench.negative_pair_count(),
        levels,
        missed_true_pairs: missed,
        marginal_gain,
        conclusion,
    }
}

/// 算一级的指标（累计口径）。
///
/// **评估与基线对比共用这一把尺子** —— 基线方案（朴素确定性）算出来的召回率、
/// 精确率必须和漏斗逐级的口径完全一致，否则两列数字放在一起比没有意义。
///
/// `cumulative` 是「跑到这一级为止一共报了多少对」（按规范键去重）；
/// `hit_true` 收集命中的真重复标注键 —— 算漏报时要用。
pub fn level_metrics(
    bench: &ResolvedBench,
    level: &str,
    level_title: &str,
    new_pairs: usize,
    cumulative: &BTreeMap<String, &SuspectedDuplicate>,
    hit_true: &mut BTreeSet<String>,
    note: &str,
) -> LevelMetrics {
    let labels = bench.by_pair();
    let mut hit = 0usize;
    let mut fp_same = 0usize;
    let mut fp_not = 0usize;
    let mut fp_unlabeled = 0usize;
    for (pk, _pair) in cumulative.iter() {
        match labels.get(pk).map(|l| l.label.as_str()) {
            Some(LABEL_DUPLICATE_RECORD) => {
                hit += 1;
                hit_true.insert(pk.clone());
            }
            Some(LABEL_SAME_INDICATOR) => fp_same += 1,
            Some(LABEL_NOT_DUPLICATE) => fp_not += 1,
            _ => fp_unlabeled += 1,
        }
    }
    let candidate_pairs = cumulative.len();
    let false_positives = fp_same + fp_not + fp_unlabeled;
    LevelMetrics {
        level: level.to_string(),
        level_title: level_title.to_string(),
        new_pairs,
        candidate_pairs,
        hit_labels: hit,
        false_positives,
        fp_same_indicator: fp_same,
        fp_not_duplicate: fp_not,
        fp_unlabeled,
        recall: ratio(hit, bench.true_pair_count()),
        precision: ratio(hit, candidate_pairs),
        note: note.to_string(),
    }
}

/// 漏掉的真重复标注：标注说是重复，方案却没报出来。
///
/// 漏报要**点名**（哪两行、依据是什么）—— 只报一个数字，
/// 读的人没法判断是判据不行还是标注写错了。
pub fn missed_true_pairs(
    bench: &ResolvedBench,
    hit_true: &BTreeSet<String>,
    reason: &str,
) -> Vec<MissedPair> {
    bench
        .labels
        .iter()
        .filter(|l| l.is_true_duplicate())
        .filter(|l| !hit_true.contains(&l.pair_key()))
        .map(|label| MissedPair {
            left_reference: label.left_reference.clone(),
            right_reference: label.right_reference.clone(),
            basis: label.basis.clone(),
            reason: reason.to_string(),
        })
        .collect()
}

/// 末级（语义归并）相对上一级（编辑距离）的边际增益。
///
/// 增益**如实算**：接了模型而有增益就写增益，接了模型而增益为负就写负，
/// 没接模型就写「尚未可测」—— 不拿 0 冒充「查了没查到」。
fn marginal_gain_of(levels: &[LevelMetrics], funnel: &FunnelOutcome) -> Option<MarginalGain> {
    let semantic = levels
        .iter()
        .find(|l| l.level == FunnelLevel::Semantic.match_level())?;
    let previous = levels
        .iter()
        .rev()
        .find(|l| l.level != FunnelLevel::Semantic.match_level())?;
    let note = funnel
        .levels
        .iter()
        .find(|l| l.level == FunnelLevel::Semantic)
        .map(|l| l.note.clone())
        .unwrap_or_default();
    Some(MarginalGain {
        level: semantic.level.clone(),
        new_pairs: semantic.new_pairs,
        recall_delta: delta(semantic.recall, previous.recall),
        precision_delta: delta(semantic.precision, previous.precision),
        note,
    })
}

fn delta(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some((x - y).round_to_4()),
        _ => None,
    }
}

/// 比率：分母为 0 时无定义（返回 None，不拿 0 或 1 冒充）
fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(((numerator as f64) / (denominator as f64)).round_to_4())
    }
}

trait RoundTo4 {
    fn round_to_4(self) -> f64;
}

impl RoundTo4 for f64 {
    fn round_to_4(self) -> f64 {
        (self * 10_000.0).round() / 10_000.0
    }
}

/// 结论：一段话，把「这一轮量出来的到底是什么」说清楚
fn build_conclusion(
    levels: &[LevelMetrics],
    missed: &[MissedPair],
    marginal: Option<&MarginalGain>,
    bench: &ResolvedBench,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for level in levels {
        let recall = level
            .recall
            .map(|v| format!("{:.2}%", v * 100.0))
            .unwrap_or_else(|| "无定义".to_string());
        let precision = level
            .precision
            .map(|v| format!("{:.2}%", v * 100.0))
            .unwrap_or_else(|| "无定义".to_string());
        parts.push(format!(
            "{} 累计 {} 对 · 命中 {} · 误报 {} · 召回 {} · 精确 {}",
            level.level_title,
            level.candidate_pairs,
            level.hit_labels,
            level.false_positives,
            recall,
            precision
        ));
    }
    let mut text = String::new();
    text.push_str(&format!(
        "基准集 {}：{} 行、{} 条标注（真重复 {} 条 / 负例 {} 条）。逐级：{}。",
        bench.bench_set,
        bench.total_rows,
        bench.labels.len(),
        bench.true_pair_count(),
        bench.negative_pair_count(),
        parts.join("；")
    ));
    if !missed.is_empty() {
        text.push_str(&format!(
            "有 {} 条真重复标注四级都没报出来（{}）。",
            missed.len(),
            missed
                .iter()
                .map(|m| format!("{} 与 {}", m.left_reference, m.right_reference))
                .collect::<Vec<String>>()
                .join("、")
        ));
    } else {
        text.push_str("真重复标注全部报出。");
    }
    if let Some(m) = marginal {
        match (m.recall_delta, m.precision_delta) {
            (Some(r), Some(p)) => text.push_str(&format!(
                "第四级相对第三级的边际增益：新增 {} 对，召回率 {:+.2}%、精确率 {:+.2}%（{}）。",
                m.new_pairs,
                r * 100.0,
                p * 100.0,
                m.note
            )),
            _ => text.push_str(&format!(
                "第四级相对第三级的边际增益：新增 {} 对（{}）。",
                m.new_pairs, m.note
            )),
        }
    }
    text
}

/// 评估结果转 CSV（一行一级）。
///
/// 列：级 | 候选数 | 命中标注 | 误报 | 召回率 | 精确率 | 本级新增 |
///     误报-同指标 | 误报-明确不是 | 误报-无标注 | 说明
pub fn to_csv(report: &EvalReport) -> String {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    let _ = wtr.write_record([
        "级",
        "候选数",
        "命中标注",
        "误报",
        "召回率",
        "精确率",
        "本级新增",
        "误报-同指标",
        "误报-明确不是",
        "误报-无标注",
        "说明",
    ]);
    for level in &report.levels {
        let _ = wtr.write_record([
            level.level_title.clone(),
            level.candidate_pairs.to_string(),
            level.hit_labels.to_string(),
            level.false_positives.to_string(),
            fmt_rate(level.recall),
            fmt_rate(level.precision),
            level.new_pairs.to_string(),
            level.fp_same_indicator.to_string(),
            level.fp_not_duplicate.to_string(),
            level.fp_unlabeled.to_string(),
            level.note.clone(),
        ]);
    }
    let mut text = String::from_utf8_lossy(&wtr.into_inner().unwrap_or_default()).to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// 评估结果转 JSON（机器读；含结论、漏掉的真重复与边际增益）
pub fn to_json(report: &EvalReport) -> String {
    let payload = serde_json::json!({
        "bench_set": report.bench_set,
        "description": report.description,
        "total_rows": report.total_rows,
        "labeled_pairs": report.labeled_pairs,
        "labeled_true_pairs": report.labeled_true_pairs,
        "labeled_negative_pairs": report.labeled_negative_pairs,
        "levels": report.levels.iter().map(|l| serde_json::json!({
            "level": l.level,
            "level_title": l.level_title,
            "new_pairs": l.new_pairs,
            "candidate_pairs": l.candidate_pairs,
            "hit_labels": l.hit_labels,
            "false_positives": l.false_positives,
            "false_positive_same_indicator": l.fp_same_indicator,
            "false_positive_not_duplicate": l.fp_not_duplicate,
            "false_positive_unlabeled": l.fp_unlabeled,
            "recall": l.recall,
            "precision": l.precision,
            "note": l.note,
        })).collect::<Vec<_>>(),
        "missed_true_pairs": report.missed_true_pairs.iter().map(|m| serde_json::json!({
            "left_reference": m.left_reference,
            "right_reference": m.right_reference,
            "basis": m.basis,
            "reason": m.reason,
        })).collect::<Vec<_>>(),
        "marginal_gain": report.marginal_gain.as_ref().map(|m| serde_json::json!({
            "level": m.level,
            "new_pairs": m.new_pairs,
            "recall_delta": m.recall_delta,
            "precision_delta": m.precision_delta,
            "note": m.note,
        })),
        "conclusion": report.conclusion,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn fmt_rate(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.4}", x),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 比率分母为零时无定义() {
        assert_eq!(ratio(0, 0), None);
        assert_eq!(ratio(1, 4), Some(0.25));
    }

    #[test]
    fn 标注取值不认识时报错() {
        let json = r#"{"bench_set":"x","labels":[{"left":{"source":"a","row":1},"right":{"source":"b","row":1},"label":"瞎写的"}]}"#;
        let dir = std::env::temp_dir().join("regval-bench-label-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("labels.json");
        std::fs::write(&path, json).unwrap();
        let err = load_bench(&path).unwrap_err();
        assert_eq!(err.code, ErrorCode::BenchSetInvalid);
        assert!(err.message.contains("瞎写的"));
        let _ = std::fs::remove_file(&path);
    }
}
