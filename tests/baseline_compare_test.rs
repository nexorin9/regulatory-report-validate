//! 验证 C：与基线方案的可复现对比（Task 48）。
//!
//! 验收口径（见 task.json 任务 48）：
//! 1. **基线方案落地**：朴素确定性基线只按「指标编号 + 科室代码 + 报告期 + 业务主键」
//!    判重复，不做归一化、不做近似、不查规则语义；
//! 2. **两条轴各有数值**：轴一在带标注基准集上比「基线 vs 四级漏斗」的召回率 / 精确率；
//!    轴二比「仅规则校验 vs 规则校验 + 公式复算」的问题发现数；
//! 3. **产物可核对**：对比表落盘，两条轴的数值都在里面；
//! 4. **可复现**：同一份输入两次运行，对比表文件 sha256 一致；
//! 5. **不利数字也写**：漏斗漏掉的真重复、模型级边际增益（未接入时如实写「尚不可测」）
//!    全部进结论 —— 只报有利数字的对比表等于没有对比表。
//!
//! 本轮量出来的结论（写在 `docs/baseline-comparison.md`）：
//! 基线召回 100.00% / 精确 77.78%，四级漏斗召回 85.71% / 精确 85.71% ——
//! **基线召回更高、漏斗精确更高**，两条曲线互有胜负，结论里照实写。

use std::path::{Path, PathBuf};
use std::process::Command;

use regval::compare;
use regval::compare::naive;
use regval::dupe;
use regval::dupe::distance::DistanceConfig;
use regval::rules::loader;

const BENCH_DIR: &str = "samples/bench";
const AXIS_B_INPUT: &str = "samples/dataset/合并表.csv";

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn regval_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_regval"))
}

/// 读基准集（标注 + 样例表），解析成行键
fn load_bench() -> (regval::dupe::eval::ResolvedBench, Vec<regval::ingest::SourceRow>) {
    let dir = project_dir().join(BENCH_DIR);
    let bench_set = dupe::eval::load_bench(&dir.join("labels.json")).unwrap();
    let mut rows = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("tables"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    for file in &files {
        for table in regval::ingest::read_tables(file).unwrap() {
            rows.extend(regval::ingest::map_table_with_report(&table).unwrap().rows);
        }
    }
    let resolved = dupe::eval::resolve(&bench_set, &rows).unwrap();
    (resolved, rows)
}

fn distance_config() -> DistanceConfig {
    DistanceConfig::default()
}

/// 跑一遍完整的对比（引擎内）
fn run_compare() -> compare::CompareReport {
    let (bench, rows) = load_bench();
    let axis_b_rows = compare::read_input(&project_dir().join(AXIS_B_INPUT)).unwrap();
    let rule_set = loader::builtin_set();
    let rules = rule_set.executable();
    compare::build_report(
        &bench,
        &rows,
        &distance_config(),
        &dupe::DisconnectedSemanticSource,
        AXIS_B_INPUT,
        &axis_b_rows,
        &rules,
    )
    .unwrap()
}

// ————————————————————————————————————————————————————————————
// 验收 1：基线方案本身
// ————————————————————————————————————————————————————————————

/// 基线只按四段键判重复 —— 取值冲突的同键行也报（漏斗第一级不报这类）
#[test]
fn 基线只按四段键_取值冲突的同键行也报() {
    let (_, rows) = load_bench();
    let outcome = naive::detect(&rows);

    // 基线的候选对必须比漏斗第一级多 —— 多出来的正是「同键但取值冲突」那批
    let exact_pairs = dupe::exact::detect(&rows).pairs.len();
    assert!(
        outcome.stats.pairs > exact_pairs,
        "基线（{} 对）应多于漏斗第一级（{} 对）：基线不比对取值",
        outcome.stats.pairs,
        exact_pairs
    );
    assert!(
        outcome.stats.conflicting_pairs > 0,
        "基线应报出同键但取值冲突的对（这批在漏斗里归「待人工判定」）"
    );
    assert_eq!(
        outcome.stats.identical_pairs + outcome.stats.conflicting_pairs,
        outcome.stats.pairs
    );
}

/// 基线的候选对不进人工确认流 —— 它是尺子，不是功能
#[test]
fn 基线候选对不进人工确认流() {
    let (_, rows) = load_bench();
    let outcome = naive::detect(&rows);
    for pair in &outcome.pairs {
        assert!(!pair.mergeable, "基线认下的对不应进人工确认流");
        assert_eq!(pair.match_level, naive::MATCH_LEVEL_NAIVE);
        assert!(!pair.basis.is_empty(), "基线候选对也要写清判据");
    }
}

/// 基线的 2 对误报点名到行：同键异值（订正与冲突）与跨院区同码各一对
///
/// 这两条是文档 `docs/baseline-comparison.md` 里的话 —— 数字对得上、点得到名，
/// 文档的说法才站得住。
#[test]
fn 基线的两对误报是同键异值与跨院区同码() {
    let (bench, rows) = load_bench();
    let outcome = naive::detect(&rows);
    let labels = bench.by_pair();

    let mut fps: Vec<(String, String, String, String)> = Vec::new();
    for pair in &outcome.pairs {
        let pk = dupe::pair_key(pair);
        if let Some(l) = labels.get(&pk) {
            if l.label != dupe::eval::LABEL_DUPLICATE_RECORD {
                fps.push((
                    pair.left_reference.clone(),
                    pair.right_reference.clone(),
                    l.label.clone(),
                    l.basis.clone(),
                ));
            }
        }
    }
    assert_eq!(fps.len(), 2, "基线应恰好多报 2 对");
    for (_, _, label, _) in &fps {
        assert_eq!(
            label, dupe::eval::LABEL_SAME_INDICATOR,
            "基线的误报应落在「同指标但不是同一件事」这类负例上"
        );
    }

    // 按标注依据点名：一对是取值冲突（97.0 / 97.5），一对是跨院区（ORG-001 / ORG-002）
    let bases: Vec<String> = fps.iter().map(|(_, _, _, b)| b.clone()).collect();
    assert!(
        bases.iter().any(|b| b.contains("97.5")),
        "应有一对误报是同键异值（填报值冲突）：{:?}",
        bases
    );
    assert!(
        bases.iter().any(|b| b.contains("ORG-002")),
        "应有一对误报是跨院区同码：{:?}",
        bases
    );
}

// ————————————————————————————————————————————————————————————
// 验收 2：轴一 —— 基线 vs 四级漏斗（同一把尺子）
// ————————————————————————————————————————————————————————————

/// 轴一的数值：基线召回 100% / 精确 77.78%；漏斗召回 85.71% / 精确 85.71%
#[test]
fn 轴一_基线与漏斗的召回精确率可复算() {
    let report = run_compare();
    assert_eq!(report.axis_a.len(), 2, "两个方案各一行");

    let naive_row = &report.axis_a[0];
    assert_eq!(naive_row.scheme, compare::SCHEME_NAIVE);
    assert_eq!(naive_row.metrics.candidate_pairs, 9);
    assert_eq!(naive_row.metrics.hit_labels, 7);
    assert_eq!(naive_row.metrics.false_positives, 2);
    assert_eq!(naive_row.metrics.recall, Some(1.0));
    assert_eq!(naive_row.metrics.precision, Some(0.7778));

    let funnel_row = &report.axis_a[1];
    assert_eq!(funnel_row.scheme, compare::SCHEME_FUNNEL);
    assert_eq!(funnel_row.metrics.candidate_pairs, 7);
    assert_eq!(funnel_row.metrics.hit_labels, 6);
    assert_eq!(funnel_row.metrics.false_positives, 1);
    assert_eq!(funnel_row.metrics.recall, Some(0.8571));
    assert_eq!(funnel_row.metrics.precision, Some(0.8571));

    // 两条曲线互有胜负 —— 这正是本任务要照实写出来的结论
    assert!(
        naive_row.metrics.recall > funnel_row.metrics.recall,
        "基线召回更高（它只看键）"
    );
    assert!(
        funnel_row.metrics.precision > naive_row.metrics.precision,
        "漏斗精确更高（它还比取值）"
    );
}

/// 漏斗漏掉的那一对，基线抓到了 —— 结论里必须点名，不许只报有利数字
#[test]
fn 轴一_漏斗漏报的真重复在结论里被点名() {
    let report = run_compare();
    let funnel_row = &report.axis_a[1];
    let naive_row = &report.axis_a[0];

    assert_eq!(
        funnel_row.missed_true_pairs.len(),
        1,
        "漏斗应漏掉 1 对（指标名称写法差异远大于字符编辑距离的那一对）"
    );
    let missed = &funnel_row.missed_true_pairs[0];
    assert!(missed.left_reference.contains("第 10 行"));
    assert!(missed.right_reference.contains("第 10 行"));
    assert!(!missed.basis.is_empty(), "漏报要写清依据");

    // 基线没漏这一对（它只看键）
    assert!(
        naive_row.missed_true_pairs.is_empty(),
        "基线应把真重复全部报出"
    );

    // 结论里必须出现「基线抓到而漏斗没抓到」这一句，且方向不能反
    assert!(
        report.conclusion.contains("基线抓到而漏斗没抓到的真重复 1 对"),
        "结论要照实写：{}",
        report.conclusion
    );
    assert!(
        !report.conclusion.contains("漏斗抓到而基线没抓到的真重复"),
        "基线没有漏报任何真重复，结论不许写反"
    );
}

/// 模型级未接入时，边际增益如实写「尚不可测」，不拿 0 冒充「查了没查到」
#[test]
fn 轴一_模型级边际增益如实写() {
    let report = run_compare();
    let marginal = report.marginal_gain.as_ref().expect("应有边际增益记录");
    assert_eq!(marginal.new_pairs, 0, "第四级未接入模型端点，候选为空");
    assert!(
        marginal.note.contains("未接入模型端点"),
        "边际增益要写明本轮第四级是什么状态：{}",
        marginal.note
    );
    assert!(
        report.conclusion.contains("未接入模型端点"),
        "结论里也要写明第四级未接入"
    );
}

// ————————————————————————————————————————————————————————————
// 验收 3：轴二 —— 仅规则校验 vs 规则校验 + 公式复算
// ————————————————————————————————————————————————————————————

/// 轴二的数值：加公式复算后多发现问题，且多出来的全部来自公式复算那一组规则
#[test]
fn 轴二_公式复算多发现的问题全部来自公式规则() {
    let report = run_compare();
    assert_eq!(report.axis_b.len(), 2);

    let only = &report.axis_b[0];
    let with_formula = &report.axis_b[1];
    assert_eq!(only.scheme, compare::SCHEME_RULES_ONLY);
    assert_eq!(with_formula.scheme, compare::SCHEME_RULES_WITH_FORMULA);

    assert!(with_formula.issues > only.issues, "加公式复算后应多发现问题");
    assert!(
        with_formula.rows_blocked > only.rows_blocked,
        "加公式复算后应多判出不合格的行"
    );

    // 多出来的数量对得上：全查 = 只查规则 + 只查公式
    assert_eq!(
        report.formula_delta.new_issues,
        with_formula.issues - only.issues,
        "公式复算多发现的问题数应等于两组之差"
    );

    // 多出来的问题类型只可能是公式复算那两类
    for kind in report.formula_delta.by_kind.keys() {
        assert!(
            matches!(kind.as_str(), "公式不符" | "数据形态错误"),
            "公式复算只应产出公式不符 / 数据形态错误，实际多了「{}」",
            kind
        );
    }
    assert_eq!(report.formula_delta.rule_ids.len(), 3, "公式复算三条规则");
    assert!(report
        .formula_delta
        .rule_ids
        .contains(&"R-FORMULA-001".to_string()));
}

/// 公式复算的规则与非公式规则分得开（分组口径唯一）
#[test]
fn 轴二_公式复算规则分组正确() {
    let rules = loader::builtin_set();
    let refs = rules.executable();
    let formula: Vec<_> = refs
        .iter()
        .copied()
        .filter(|r| compare::is_formula_rule(r))
        .collect();
    assert_eq!(formula.len(), 3, "公式复算三条");
    assert!(refs.len() > formula.len());
}

// ————————————————————————————————————————————————————————————
// 验收 4：产物落盘 + 可复现
// ————————————————————————————————————————————————————————————

/// 对比表文件存在，且两条轴的数值都在里面
#[test]
fn 产物_对比表含两条轴的数值() {
    let report = run_compare();
    let csv = compare::to_csv(&report);
    assert!(csv.contains("重复检测"), "轴一要在表里");
    assert!(csv.contains("问题发现"), "轴二要在表里");
    assert!(csv.contains("1.0000"), "基线召回率要在表里");
    assert!(csv.contains("0.7778"), "基线精确率要在表里");
    assert!(csv.contains("0.8571"), "漏斗召回率要在表里");

    let json = compare::to_json(&report);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["axis_a"]["schemes"].as_array().unwrap().len() == 2);
    assert!(parsed["axis_b"]["schemes"].as_array().unwrap().len() == 2);
    assert!(parsed["axis_b"]["formula_delta"]["new_issues"]
        .as_u64()
        .unwrap()
        > 0);
    assert!(!parsed["conclusion"].as_str().unwrap().is_empty());
}

/// 同一份输入两次运行，对比表文件逐字节相同（sha256 一致）
#[test]
fn 可复现_两次运行的对比表文件一致() {
    let dir = std::env::temp_dir().join("regval-compare-repro");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut hashes = Vec::new();
    for i in 0..2 {
        let out = dir.join(format!("cmp{}.csv", i));
        let out_json = dir.join(format!("cmp{}.json", i));
        for target in [&out, &out_json] {
            let output = Command::new(regval_bin())
                .args([
                    "compare",
                    "--bench-dir",
                    BENCH_DIR,
                    "--input",
                    AXIS_B_INPUT,
                    "--out",
                    target.to_str().unwrap(),
                ])
                .current_dir(project_dir())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "regval compare 退出码应为 0：{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        hashes.push(sha256(&out));
        hashes.push(sha256(&out_json));
    }
    assert_eq!(hashes[0], hashes[2], "两次运行的 CSV 应逐字节相同");
    assert_eq!(hashes[1], hashes[3], "两次运行的 JSON 应逐字节相同");

    let _ = std::fs::remove_dir_all(&dir);
}

/// CLI 实跑：退出码 0，stdout 报出两条轴的数值
#[test]
fn cli_实跑报出两条轴() {
    let dir = std::env::temp_dir().join("regval-compare-cli");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("cmp.csv");

    let output = Command::new(regval_bin())
        .args([
            "compare",
            "--bench-dir",
            BENCH_DIR,
            "--input",
            AXIS_B_INPUT,
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(project_dir())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("轴一 · 重复检测"));
    assert!(stdout.contains("轴二 · 问题发现"));
    assert!(stdout.contains("召回 100.00%"), "基线召回要报出来");
    assert!(stdout.contains("召回 85.71%"), "漏斗召回要报出来");
    assert!(out.exists(), "对比表文件要落盘");

    let _ = std::fs::remove_dir_all(&dir);
}

// ————————————————————————————————————————————————————————————
// 验收 5：对比不进产品主路径
// ————————————————————————————————————————————————————————————

/// 基线与对比模块不被上报侧与状态层引用 —— 它只量数，不改业务状态
#[test]
fn 对比模块不进上报侧与状态层() {
    let src = project_dir().join("src");
    for dir in ["push", "state"] {
        let path = src.join(dir);
        let mut checked = 0usize;
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !text.contains("compare::"),
                    "{} 里不该引用 compare 模块",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "{} 目录里应当有源文件", dir);
    }
}

/// 文件 sha256
fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}
