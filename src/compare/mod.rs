//! 验证 C：与基线方案的可复现对比。
//!
//! # 这个模块回答两个问题
//!
//! | 轴 | 问题 | 对照 |
//! |---|---|---|
//! | **轴一 · 重复检测** | 四级漏斗比「一把 VLOOKUP」多出来的几级，值不值？ | 朴素确定性基线（[`naive`]） |
//! | **轴二 · 问题发现** | 分子分母公式复算，多找出多少不合格的行？ | 仅规则校验 |
//!
//! # 为什么一定要有对照数字
//!
//! 「我们的方案更先进」这句话没有意义 —— 要有数字，数字从**同一份带标注的样例**上来。
//! 本模块的两条轴都用同一把尺子（[`crate::dupe::eval`] 的召回率 / 精确率口径），
//! 算出来的对比表可以直接进评审材料。
//!
//! # 三条纪律
//!
//! 1. **同一把尺子**：基线与漏斗的召回率、精确率由同一个函数算出
//!    （[`crate::dupe::eval::level_metrics`]），口径不一致的数字放在一起比没有意义；
//! 2. **不利数字也写**：漏斗漏掉的真重复、基线多报的误报、模型级边际增益为负 ——
//!    全部照实写进结论。只报有利数字的对比表等于没有对比表；
//! 3. **可复现**：同一份输入跑两次，对比表文件逐字节相同（两个文件的 sha256 一致）。
//!
//! # 产物
//!
//! - `regval compare --out <path>.csv` —— 两轴合一的对比表（人看）
//! - `regval compare --out <path>.json` —— 同一份数据的结构化形式（机器读）
//!
//! # 边界
//!
//! 本模块**不进产品主路径**：基线的候选对不写回原表、不进人工确认流、不推送。
//! 它是尺子，不是功能。

pub mod naive;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use crate::dupe::eval::{self, LevelMetrics, MarginalGain, MissedPair, ResolvedBench};
use crate::dupe::{self, distance::DistanceConfig, SemanticCandidateSource, SuspectedDuplicate};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::SourceRow;
use crate::rules::executor;
use crate::rules::model::{Issue, Rule, RuleKind};

/// 轴一里「四级漏斗」这个方案的标识
pub const SCHEME_NAIVE: &str = "naive";
/// 轴一里「四级漏斗」这个方案的标识
pub const SCHEME_FUNNEL: &str = "funnel";
/// 轴二里「仅规则校验」这个方案的标识
pub const SCHEME_RULES_ONLY: &str = "rules_only";
/// 轴二里「规则校验 + 公式复算」这个方案的标识
pub const SCHEME_RULES_WITH_FORMULA: &str = "rules_with_formula";

/// 一个方案在轴一（重复检测）上的成绩
#[derive(Debug, Clone, PartialEq)]
pub struct DupeScheme {
    /// 方案标识
    pub scheme: String,
    /// 方案中文名（给人看）
    pub scheme_title: String,
    /// 逐级同一把尺子算出来的指标
    pub metrics: LevelMetrics,
    /// 漏掉的真重复标注（点名到行）
    pub missed_true_pairs: Vec<MissedPair>,
    /// 涉及的行数（被判为疑似重复的行）
    pub rows_affected: usize,
    /// 说明
    pub note: String,
}

/// 轴二里「加上公式复算后多发现了什么」
#[derive(Debug, Clone, PartialEq)]
pub struct FormulaDelta {
    /// 多发现的问题数
    pub new_issues: usize,
    /// 多判「不通过」的行数（原本判通过、加了复算才判不通过）
    pub newly_blocked_rows: usize,
    /// 公式复算这一组规则的编号
    pub rule_ids: Vec<String>,
    /// 新增问题按类型分计数
    pub by_kind: BTreeMap<String, usize>,
}

/// 一个方案在轴二（问题发现）上的成绩
#[derive(Debug, Clone, PartialEq)]
pub struct IssueScheme {
    /// 方案标识
    pub scheme: String,
    /// 方案中文名
    pub scheme_title: String,
    /// 参与本方案的口径规则条数
    pub rules: usize,
    /// 产出的问题总数
    pub issues: usize,
    /// 涉及的行数（至少命中一条问题的行）
    pub rows_affected: usize,
    /// 判「不通过」的行数（命中阻断级问题）
    pub rows_blocked: usize,
    /// 按问题类型分计数
    pub by_kind: BTreeMap<String, usize>,
    /// 说明
    pub note: String,
}

/// 一份对比报告（两条轴）
#[derive(Debug, Clone, PartialEq)]
pub struct CompareReport {
    /// 基准集编号（轴一）
    pub bench_set: String,
    /// 基准集说明
    pub bench_description: String,
    /// 基准集样例行数
    pub bench_rows: usize,
    /// 标注条数
    pub labeled_pairs: usize,
    /// 真重复标注条数
    pub labeled_true_pairs: usize,
    /// 负例标注条数
    pub labeled_negative_pairs: usize,
    /// 轴一 · 重复检测
    pub axis_a: Vec<DupeScheme>,
    /// 第四级（模型语义归并）相对第三级的边际增益
    pub marginal_gain: Option<MarginalGain>,
    /// 轴二用的输入表
    pub axis_b_input: String,
    /// 轴二输入表的行数
    pub axis_b_rows: usize,
    /// 轴二 · 问题发现
    pub axis_b: Vec<IssueScheme>,
    /// 轴二里「加上公式复算后多发现了什么」
    pub formula_delta: FormulaDelta,
    /// 结论（CLI 与文档都引它）
    pub conclusion: String,
}

/// 轴一：朴素确定性基线 vs 四级漏斗。
///
/// 两个方案用**同一把尺子**（[`eval::level_metrics`]）算召回率与精确率：
/// 基线报出的对与漏斗报出的对，都按行键对齐到人工标注上比对。
pub fn compare_dupe(
    bench: &ResolvedBench,
    rows: &[SourceRow],
    distance_cfg: &DistanceConfig,
    semantic: &dyn SemanticCandidateSource,
) -> (Vec<DupeScheme>, Option<MarginalGain>) {
    // 方案一：朴素基线（只按四段键）
    let naive = naive::detect(rows);
    let naive_cumulative = cumulative_map(&naive.pairs);
    let mut naive_hit: BTreeSet<String> = BTreeSet::new();
    let naive_metrics = eval::level_metrics(
        bench,
        naive::MATCH_LEVEL_NAIVE,
        "朴素确定性基线（仅四段键）",
        naive.pairs.len(),
        &naive_cumulative,
        &mut naive_hit,
        &format!(
            "只按「指标编号 + 科室代码 + 报告期 + 业务主键」判重复：候选 {} 对（其中同键且取值一致 {} 对 · 同键但取值冲突 {} 对）",
            naive.stats.pairs, naive.stats.identical_pairs, naive.stats.conflicting_pairs
        ),
    );
    let naive_missed = eval::missed_true_pairs(
        bench,
        &naive_hit,
        "基线候选里都没有这一对 —— 键就不一样，不是同一件事",
    );

    // 方案二：四级漏斗（跑一遍，指标直接取评估报告的末级累计口径）
    let funnel = dupe::run_funnel(rows, distance_cfg, semantic);
    let eval_report = eval::evaluate(bench, &funnel);
    let funnel_level = eval_report.levels.last().cloned();
    let rows_affected = count_rows(&funnel.cumulative);

    let mut schemes = Vec::new();
    schemes.push(DupeScheme {
        scheme: SCHEME_NAIVE.to_string(),
        scheme_title: "朴素确定性基线（仅四段键）".to_string(),
        rows_affected: count_rows(&naive.pairs),
        metrics: naive_metrics,
        missed_true_pairs: naive_missed,
        note: "对照方案：Excel VLOOKUP 一把的做法，键对上就算重复".to_string(),
    });
    if let Some(metrics) = funnel_level {
        schemes.push(DupeScheme {
            scheme: SCHEME_FUNNEL.to_string(),
            scheme_title: "四级漏斗（累计口径）".to_string(),
            rows_affected,
            metrics,
            missed_true_pairs: eval_report.missed_true_pairs.clone(),
            note: "本产品方案：确定性键 → 归一化 → 编辑距离 → 语义归并".to_string(),
        });
    }

    (schemes, eval_report.marginal_gain.clone())
}

/// 轴二：仅规则校验 vs 规则校验 + 公式复算。
///
/// 「公式复算」指的是分子分母复算那一组规则（[`RuleKind::FormulaRecalc`] 与
/// [`RuleKind::DataShapeError`]）：填报值与官方公式对不上、分母为 0、分子分母非数值。
/// 去掉这一组，就是「只查必填 / 枚举 / 单位」的纯规则校验。
pub fn compare_issue_discovery(
    rows: &[SourceRow],
    rules: &[&Rule],
) -> Result<(Vec<IssueScheme>, FormulaDelta), EngineError> {
    let batch_ref = "compare-axis-b";
    // 内存库：这轮对比不落任何持久状态
    let mut repo = crate::db::repo::Repo::open(Path::new(":memory:"))?;
    repo.insert_rows(batch_ref, rows)?;
    let conn = repo.conn();

    let (rule_only, formula_rules): (Vec<&Rule>, Vec<&Rule>) =
        rules.iter().partition(|r| !is_formula_rule(r));

    let only_report = executor::execute_rules(conn, &rule_only, batch_ref);
    let formula_report = executor::execute_rules(conn, &formula_rules, batch_ref);
    let all_report = executor::execute_rules(conn, rules, batch_ref);

    let only_issues = only_report.issues();
    let formula_issues = formula_report.issues();
    let all_issues = all_report.issues();

    let only = tally(&only_issues);
    let formula = tally(&formula_issues);
    let all = tally(&all_issues);

    let formula_rule_ids: Vec<String> = formula_rules
        .iter()
        .map(|r| r.rule_id.clone())
        .collect();

    let newly_blocked = all
        .blocked_rows
        .difference(&only.blocked_rows)
        .count();

    let schemes = vec![
        IssueScheme {
            scheme: SCHEME_RULES_ONLY.to_string(),
            scheme_title: "仅规则校验（必填 / 枚举 / 单位 / 唯一性）".to_string(),
            rules: rule_only.len(),
            issues: only.issue_count,
            rows_affected: only.rows_affected,
            rows_blocked: only.blocked_rows.len(),
            by_kind: only.by_kind.clone(),
            note: format!(
                "不含分子分母公式复算的 {} 条规则；发现 {} 项问题、涉及 {} 行，其中 {} 行判「不通过」",
                rule_only.len(),
                only.issue_count,
                only.rows_affected,
                only.blocked_rows.len()
            ),
        },
        IssueScheme {
            scheme: SCHEME_RULES_WITH_FORMULA.to_string(),
            scheme_title: "规则校验 + 分子分母公式复算".to_string(),
            rules: rules.len(),
            issues: all.issue_count,
            rows_affected: all.rows_affected,
            rows_blocked: all.blocked_rows.len(),
            by_kind: all.by_kind.clone(),
            note: format!(
                "加上公式复算的 {} 条规则（{}）；共发现 {} 项问题、涉及 {} 行，其中 {} 行判「不通过」；较仅规则校验多发现 {} 项问题、{} 行原本判「通过」加了复算才判「不通过」",
                formula_rules.len(),
                formula_rule_ids.join(" / "),
                all.issue_count,
                all.rows_affected,
                all.blocked_rows.len(),
                formula.issue_count,
                newly_blocked
            ),
        },
    ];

    // 「多发现了什么」取公式复算那一轮的实际产出 —— 不是两个总数相减的估计值
    let delta = FormulaDelta {
        new_issues: formula.issue_count,
        newly_blocked_rows: newly_blocked,
        rule_ids: formula_rule_ids.clone(),
        by_kind: formula.by_kind.clone(),
    };
    Ok((schemes, delta))
}

/// 是否属于「分子分母公式复算」这一组规则
pub fn is_formula_rule(rule: &Rule) -> bool {
    matches!(
        rule.rule_kind,
        RuleKind::FormulaRecalc | RuleKind::DataShapeError
    )
}

/// 候选对 → 规范键映射（同一对行不论被哪一级判出来，键都一样）
fn cumulative_map(pairs: &[SuspectedDuplicate]) -> BTreeMap<String, &SuspectedDuplicate> {
    pairs
        .iter()
        .map(|p| (dupe::pair_key(p), p))
        .collect()
}

/// 候选对涉及的行数（去重）
fn count_rows(pairs: &[SuspectedDuplicate]) -> usize {
    let mut rows: BTreeSet<&str> = BTreeSet::new();
    for pair in pairs {
        rows.insert(pair.left_row_key.as_str());
        rows.insert(pair.right_row_key.as_str());
    }
    rows.len()
}

/// 一批问题的汇总
#[derive(Debug, Clone, Default)]
struct Tally {
    /// 问题总数
    issue_count: usize,
    /// 涉及的行（行键集合）
    rows: BTreeSet<String>,
    /// 判「不通过」的行（命中阻断级问题）
    blocked_rows: BTreeSet<String>,
    /// 按问题类型分计数
    by_kind: BTreeMap<String, usize>,
    /// 涉及行数
    rows_affected: usize,
}

/// 汇总一批问题
fn tally(issues: &[Issue]) -> Tally {
    let mut t = Tally {
        issue_count: issues.len(),
        ..Tally::default()
    };
    for issue in issues {
        t.rows.insert(issue.row_uid.clone());
        if issue.severity == "阻断" {
            t.blocked_rows.insert(issue.row_uid.clone());
        }
        *t.by_kind.entry(issue.issue_kind.clone()).or_insert(0) += 1;
    }
    t.rows_affected = t.rows.len();
    t
}

/// 组装一份完整的对比报告（含结论）
pub fn build_report(
    bench: &ResolvedBench,
    rows: &[SourceRow],
    distance_cfg: &DistanceConfig,
    semantic: &dyn SemanticCandidateSource,
    axis_b_input: &str,
    axis_b_rows: &[SourceRow],
    rules: &[&Rule],
) -> Result<CompareReport, EngineError> {
    let (axis_a, marginal_gain) = compare_dupe(bench, rows, distance_cfg, semantic);
    let (axis_b, formula_delta) = compare_issue_discovery(axis_b_rows, rules)?;
    let conclusion = build_conclusion(
        bench,
        &axis_a,
        marginal_gain.as_ref(),
        &axis_b,
        &formula_delta,
        axis_b_input,
        axis_b_rows.len(),
    );

    Ok(CompareReport {
        bench_set: bench.bench_set.clone(),
        bench_description: bench.description.clone(),
        bench_rows: bench.total_rows,
        labeled_pairs: bench.labels.len(),
        labeled_true_pairs: bench.true_pair_count(),
        labeled_negative_pairs: bench.negative_pair_count(),
        axis_a,
        marginal_gain,
        axis_b_input: axis_b_input.to_string(),
        axis_b_rows: axis_b_rows.len(),
        axis_b,
        formula_delta,
        conclusion,
    })
}

/// 结论：一段话，把「这一轮量出来的到底是什么」说清楚。
///
/// **不利数字一并写进去**：谁漏了、谁多报了、模型级能不能测 —— 缺哪句就补哪句。
fn build_conclusion(
    bench: &ResolvedBench,
    axis_a: &[DupeScheme],
    marginal: Option<&MarginalGain>,
    axis_b: &[IssueScheme],
    delta: &FormulaDelta,
    axis_b_input: &str,
    axis_b_row_count: usize,
) -> String {
    let mut text = String::new();

    // —— 轴一 ——
    text.push_str(&format!(
        "轴一 · 重复检测（基准集 {}：{} 行、{} 条标注，真重复 {} 条 / 负例 {} 条）：",
        bench.bench_set,
        bench.total_rows,
        bench.labels.len(),
        bench.true_pair_count(),
        bench.negative_pair_count()
    ));
    for scheme in axis_a {
        let recall = fmt_pct(scheme.metrics.recall);
        let precision = fmt_pct(scheme.metrics.precision);
        text.push_str(&format!(
            "{} 候选 {} 对 · 命中 {} · 误报 {} · 召回 {} · 精确 {}",
            scheme.scheme_title,
            scheme.metrics.candidate_pairs,
            scheme.metrics.hit_labels,
            scheme.metrics.false_positives,
            recall,
            precision
        ));
        if !scheme.missed_true_pairs.is_empty() {
            text.push_str(&format!(
                "（漏报 {} 对：{}）",
                scheme.missed_true_pairs.len(),
                scheme
                    .missed_true_pairs
                    .iter()
                    .map(|m| format!("{} 与 {}", m.left_reference, m.right_reference))
                    .collect::<Vec<String>>()
                    .join("、")
            ));
        } else {
            text.push_str("（真重复标注全部报出）");
        }
        text.push_str("；");
    }

    // 谁抓到了谁没抓到 —— 两条曲线互有胜负就照实说
    if let (Some(naive), Some(funnel)) = (axis_a.first(), axis_a.get(1)) {
        // 漏斗漏掉、基线抓到的：基线没漏这一对，漏斗漏了
        let naive_caught: Vec<&MissedPair> = funnel
            .missed_true_pairs
            .iter()
            .filter(|m| {
                !naive
                    .missed_true_pairs
                    .iter()
                    .any(|n| n.left_reference == m.left_reference && n.right_reference == m.right_reference)
            })
            .collect();
        // 基线漏掉、漏斗抓到的：反过来
        let funnel_caught: Vec<&MissedPair> = naive
            .missed_true_pairs
            .iter()
            .filter(|m| {
                !funnel
                    .missed_true_pairs
                    .iter()
                    .any(|f| f.left_reference == m.left_reference && f.right_reference == m.right_reference)
            })
            .collect();
        if !naive_caught.is_empty() {
            text.push_str(&format!(
                "基线抓到而漏斗没抓到的真重复 {} 对（{}）；",
                naive_caught.len(),
                naive_caught
                    .iter()
                    .map(|m| format!("{} 与 {}", m.left_reference, m.right_reference))
                    .collect::<Vec<String>>()
                    .join("、")
            ));
        }
        if !funnel_caught.is_empty() {
            text.push_str(&format!(
                "漏斗抓到而基线没抓到的真重复 {} 对（{}）；",
                funnel_caught.len(),
                funnel_caught
                    .iter()
                    .map(|m| format!("{} 与 {}", m.left_reference, m.right_reference))
                    .collect::<Vec<String>>()
                    .join("、")
            ));
        }

        // 两个方案的取舍：基线换召回、漏斗换精确 —— 两边都说，不挑一边吹
        text.push_str(&format!(
            "取舍：基线只认「键相同」，召回 {}、精确 {}，代价是 {} 对取值冲突或跨院区同码的行也报成重复；漏斗要求取值也一致，召回 {}、精确 {}，代价是写法差异大（远大于字符编辑距离）时认不出。",
            fmt_pct(naive.metrics.recall),
            fmt_pct(naive.metrics.precision),
            naive.metrics.false_positives,
            fmt_pct(funnel.metrics.recall),
            fmt_pct(funnel.metrics.precision)
        ));
    }

    // 模型级：边际增益如实写，测不了就写「尚不可测」，不拿 0 冒充
    match marginal {
        Some(m) => match (m.recall_delta, m.precision_delta) {
            (Some(r), Some(p)) => text.push_str(&format!(
                "第四级（模型语义归并）相对第三级的边际增益：新增 {} 对，召回 {:+}、精确 {:+}（{}）。",
                m.new_pairs,
                r * 100.0,
                p * 100.0,
                m.note
            )),
            _ => text.push_str(&format!(
                "第四级（模型语义归并）相对第三级的边际增益：新增 {} 对（{}）。",
                m.new_pairs, m.note
            )),
        },
        None => text.push_str("第四级（模型语义归并）的边际增益本轮未测。"),
    }

    // —— 轴二 ——
    if let (Some(only), Some(with_formula)) = (axis_b.first(), axis_b.get(1)) {
        text.push_str(&format!(
            "轴二 · 问题发现（输入 {}，{} 行）：仅规则校验发现 {} 项问题、涉及 {} 行、{} 行判「不通过」；加上分子分母公式复算（{}）后共 {} 项问题、涉及 {} 行、{} 行判「不通过」，多出的 {} 项全部来自公式复算这一组规则，其中 {} 行原本判「通过」、加了复算才判「不通过」。",
            axis_b_input,
            axis_b_row_count,
            only.issues,
            only.rows_affected,
            only.rows_blocked,
            delta.rule_ids.join(" / "),
            with_formula.issues,
            with_formula.rows_affected,
            with_formula.rows_blocked,
            delta.new_issues,
            delta.newly_blocked_rows
        ));
    }

    // —— 是否保留模型级 ——
    text.push_str(
        "是否保留模型级：第四级当前未接入模型端点，边际增益尚不可测；确定性三级（键 / 归一化 / 编辑距离）在带标注基准集上已达召回与精确各约八成半，模型级要证明自己，得把「写法差异远大于字符编辑距离」的那一类认下来、且不多报 —— 接入模型端点后用同一份基准集复跑本命令即可复算。",
    );

    text
}

/// 百分比（无定义时写「无定义」）
fn fmt_pct(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.2}%", x * 100.0),
        None => "无定义".to_string(),
    }
}

/// 对比表转 CSV（两轴合一，一行一个方案）。
///
/// 列：轴 | 方案 | 候选对数 | 命中标注 | 误报 | 召回率 | 精确率 | 漏报真重复
///     | 问题数 | 涉及行数 | 不通过行数 | 说明
///
/// 两条轴的指标不是一回事（轴一量的是「对」，轴二量的是「问题」），
/// 不适用的格子留空 —— 不拿 0 冒充「这一格没有」。
pub fn to_csv(report: &CompareReport) -> String {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    let header = vec![
        "轴",
        "方案",
        "候选对数",
        "命中标注",
        "误报",
        "召回率",
        "精确率",
        "漏报真重复",
        "问题数",
        "涉及行数",
        "不通过行数",
        "说明",
    ];
    let _ = wtr.write_record(&header);
    for scheme in &report.axis_a {
        let _ = wtr.write_record(vec![
            "重复检测".to_string(),
            scheme.scheme_title.clone(),
            scheme.metrics.candidate_pairs.to_string(),
            scheme.metrics.hit_labels.to_string(),
            scheme.metrics.false_positives.to_string(),
            fmt_rate(scheme.metrics.recall),
            fmt_rate(scheme.metrics.precision),
            scheme.missed_true_pairs.len().to_string(),
            String::new(),
            scheme.rows_affected.to_string(),
            String::new(),
            scheme.note.clone(),
        ]);
    }
    for scheme in &report.axis_b {
        let _ = wtr.write_record(vec![
            "问题发现".to_string(),
            scheme.scheme_title.clone(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            scheme.issues.to_string(),
            scheme.rows_affected.to_string(),
            scheme.rows_blocked.to_string(),
            scheme.note.clone(),
        ]);
    }
    let mut text = String::from_utf8_lossy(&wtr.into_inner().unwrap_or_default()).to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// 对比报告转 JSON（机器读）
pub fn to_json(report: &CompareReport) -> String {
    let payload = json!({
        "bench_set": report.bench_set,
        "bench_description": report.bench_description,
        "bench_rows": report.bench_rows,
        "labeled_pairs": report.labeled_pairs,
        "labeled_true_pairs": report.labeled_true_pairs,
        "labeled_negative_pairs": report.labeled_negative_pairs,
        "axis_a": {
            "title": "轴一 · 重复检测",
            "schemes": report.axis_a.iter().map(|s| json!({
                "scheme": s.scheme,
                "scheme_title": s.scheme_title,
                "candidate_pairs": s.metrics.candidate_pairs,
                "new_pairs": s.metrics.new_pairs,
                "hit_labels": s.metrics.hit_labels,
                "false_positives": s.metrics.false_positives,
                "false_positive_same_indicator": s.metrics.fp_same_indicator,
                "false_positive_not_duplicate": s.metrics.fp_not_duplicate,
                "false_positive_unlabeled": s.metrics.fp_unlabeled,
                "recall": s.metrics.recall,
                "precision": s.metrics.precision,
                "rows_affected": s.rows_affected,
                "missed_true_pairs": s.missed_true_pairs.iter().map(|m| json!({
                    "left_reference": m.left_reference,
                    "right_reference": m.right_reference,
                    "basis": m.basis,
                    "reason": m.reason,
                })).collect::<Vec<_>>(),
                "note": s.note,
            })).collect::<Vec<_>>(),
            "marginal_gain": report.marginal_gain.as_ref().map(|m| json!({
                "level": m.level,
                "new_pairs": m.new_pairs,
                "recall_delta": m.recall_delta,
                "precision_delta": m.precision_delta,
                "note": m.note,
            })),
        },
        "axis_b": {
            "title": "轴二 · 问题发现",
            "input": report.axis_b_input,
            "rows": report.axis_b_rows,
            "schemes": report.axis_b.iter().map(|s| json!({
                "scheme": s.scheme,
                "scheme_title": s.scheme_title,
                "rules": s.rules,
                "issues": s.issues,
                "rows_affected": s.rows_affected,
                "rows_blocked": s.rows_blocked,
                "by_kind": s.by_kind,
                "note": s.note,
            })).collect::<Vec<_>>(),
            "formula_delta": {
                "new_issues": report.formula_delta.new_issues,
                "newly_blocked_rows": report.formula_delta.newly_blocked_rows,
                "rule_ids": report.formula_delta.rule_ids,
                "by_kind": report.formula_delta.by_kind,
            },
        },
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

/// 读一张表（轴二的输入）—— 复用主路径的读表口径
pub fn read_input(path: &Path) -> Result<Vec<SourceRow>, EngineError> {
    let tables = crate::ingest::read_tables(path)?;
    let mut rows = Vec::new();
    for table in &tables {
        rows.extend(crate::ingest::map_table_with_report(table)?.rows);
    }
    if rows.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            "输入表里没有可用的数据行",
            path.display().to_string(),
        ));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 公式复算规则与非公式规则分得开() {
        let rules = crate::rules::model::builtin_rules();
        let refs: Vec<&Rule> = rules.iter().collect();
        let formula: Vec<&Rule> = refs.iter().copied().filter(|r| is_formula_rule(r)).collect();
        assert_eq!(formula.len(), 3, "公式复算三条：公式不符 / 分母为 0 / 数值形态");
        assert!(formula
            .iter()
            .all(|r| matches!(r.rule_kind, RuleKind::FormulaRecalc | RuleKind::DataShapeError)));
        assert!(refs.len() > formula.len());
    }
}
