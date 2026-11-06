//! 重复检测四级漏斗的编排与逐级召回率 / 精确率基准。
//!
//! 覆盖：
//! ① 漏斗逐级候选集**单调不减**（后一级只增不删，不丢已认下的对）；
//! ② 逐级对比表文件存在，每一级都有召回率与精确率数值；
//! ③ 逐级数值与基准集的人工标注一致（命中 / 误报 / 漏报逐条对得上）；
//! ④ 标注定位不到行、取值不认识时**报错**（不静默跳过 —— 静默跳过会悄悄抬高召回率）；
//! ⑤ 同一份输入两次跑，对比表逐字相同；
//! ⑥ 第四级（语义归并）未接入模型端点时状态照实报出，不拿 0 冒充「查过了没查到」；
//! ⑦ 语义候选来源接上后，第四级能并入候选、单调性仍成立（接线是活的，不是死代码）；
//! ⑧ 端到端：`regval bench` 跑通并产出可核对的对比表。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regval::dupe::distance::DistanceConfig;
use regval::dupe::eval::{self, LABEL_DUPLICATE_RECORD};
use regval::dupe::{
    run_funnel, DisconnectedSemanticSource, SemanticCandidateSource, SuspectedDuplicate,
};
use regval::ingest::{self, SourceRow};

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/bench")
}

/// 读基准集的两张表并映射成业务行
fn bench_rows() -> Vec<SourceRow> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(bench_dir().join("tables"))
        .expect("基准集样例表目录应存在")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let mut rows = Vec::new();
    for file in files {
        for table in ingest::read_tables(&file).expect("样例表应可读") {
            rows.extend(ingest::map_table(&table).expect("样例表应可映射"));
        }
    }
    rows
}

fn load_bench_set() -> eval::BenchSet {
    eval::load_bench(&bench_dir().join("labels.json")).expect("标注文件应可装载")
}

fn resolved() -> eval::ResolvedBench {
    let set = load_bench_set();
    eval::resolve(&set, &bench_rows()).expect("标注定位应全部命中")
}

/// 跑一遍漏斗 + 评估
fn evaluate_bench() -> (eval::EvalReport, regval::dupe::FunnelOutcome) {
    let rows = bench_rows();
    let cfg = DistanceConfig::default();
    let funnel = run_funnel(&rows, &cfg, &DisconnectedSemanticSource);
    let report = eval::evaluate(&resolved(), &funnel);
    (report, funnel)
}

/// 测试替身：假装第四级能认出一对「写法差异很大但语义等价」的行。
///
/// **这不是模型成绩** —— 它只证明第四级在代码里是接得上的、候选能并进漏斗、
/// 逐级单调性仍然成立。真实模型端的成绩要等模型接入后重跑基准集。
struct FakeSemanticSource {
    pairs: Vec<SuspectedDuplicate>,
}

impl SemanticCandidateSource for FakeSemanticSource {
    fn available(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "测试替身：假装模型认出了 1 对写法差异很大的行（非模型成绩）".to_string()
    }
    fn candidates(
        &self,
        _rows: &[SourceRow],
        _prior: &[SuspectedDuplicate],
    ) -> Vec<SuspectedDuplicate> {
        self.pairs.clone()
    }
}

/// 找出基准集里那一对「四级都认不出」的候选对（BK-12：CT/MRI… vs 计算机断层与磁共振…）
fn semantic_target_pair(rows: &[SourceRow]) -> SuspectedDuplicate {
    let mut found: Option<SuspectedDuplicate> = None;
    for a in rows {
        for b in rows {
            if a.source_ns >= b.source_ns && a.source_ns == b.source_ns {
                continue;
            }
            let a_name = a
                .get_trimmed(regval::ingest::Field::IndicatorName)
                .unwrap_or("");
            let b_name = b
                .get_trimmed(regval::ingest::Field::IndicatorName)
                .unwrap_or("");
            if a_name.contains("计算机断层") && b_name.contains("CT/MRI") {
                let key_a = regval::ingest::key::row_uid(a);
                let key_b = regval::ingest::key::row_uid(b);
                let (left, right, lk, rk) = if key_a <= key_b {
                    (a, b, key_a.clone(), key_b.clone())
                } else {
                    (b, a, key_b.clone(), key_a.clone())
                };
                found = Some(SuspectedDuplicate {
                    left_row_key: lk,
                    right_row_key: rk,
                    match_level: regval::dupe::SEMANTIC_MATCH_LEVEL.to_string(),
                    basis: "测试替身产出的语义候选".to_string(),
                    business_key: regval::ingest::key::business_key(left),
                    left_reference: format!("{} 第 {} 行", left.source_ns, left.row_no),
                    right_reference: format!("{} 第 {} 行", right.source_ns, right.row_no),
                    cross_table: true,
                    state: regval::dupe::STATE_SUSPECTED.to_string(),
                    left_original: String::new(),
                    right_original: String::new(),
                    left_normalized: String::new(),
                    right_normalized: String::new(),
                    normalize_detail: String::new(),
                    distance: None,
                    distance_detail: String::new(),
                    // 这一对是第四级（语义归并）产出的，候选来源标语义级
                    source: "semantic".to_string(),
                    semantic_kind: "same_indicator".to_string(),
                    semantic_confidence: Some(0.9),
                    semantic_reason: "测试替身产出的语义候选".to_string(),
                    semantic_evidence: vec!["测试依据".to_string()],
                    mergeable: true,
                });
            }
        }
    }
    found.expect("基准集里应有一对写法差异很大的真重复")
}

// ============================================================================
// ① 漏斗逐级候选集单调不减
// ============================================================================

#[test]
fn 漏斗逐级候选集单调不减() {
    let rows = bench_rows();
    let cfg = DistanceConfig::default();
    let funnel = run_funnel(&rows, &cfg, &DisconnectedSemanticSource);

    assert_eq!(funnel.levels.len(), 4, "四级都要有");
    let mut acc = 0usize;
    let mut seen: Vec<String> = Vec::new();
    for level in &funnel.levels {
        acc += level.new_pair_count();
        assert!(
            acc >= funnel.cumulative_len_at(level.level),
            "{} 级累计数不该少于累加和",
            level.level.title()
        );
        // 累加集合逐级只增不删：前一级认下的对，后一级还在
        for pair in &level.new_pairs {
            let k = regval::dupe::pair_key(pair);
            assert!(!seen.contains(&k), "同一对行被两级重复报：{}", k);
            seen.push(k);
        }
    }
    assert_eq!(acc, funnel.cumulative.len(), "累加总数 = 各级新增之和");
    assert_eq!(funnel.cumulative.len(), seen.len());
}

#[test]
fn 漏斗每一级都不丢已认下的候选对() {
    let rows = bench_rows();
    let cfg = DistanceConfig::default();
    let funnel = run_funnel(&rows, &cfg, &DisconnectedSemanticSource);

    // 逐级快照：跑到第 n 级为止的候选对集合
    let mut snapshots: Vec<Vec<String>> = Vec::new();
    let mut acc: Vec<String> = Vec::new();
    for level in &funnel.levels {
        for pair in &level.new_pairs {
            acc.push(regval::dupe::pair_key(pair));
        }
        snapshots.push(acc.clone());
    }
    for i in 1..snapshots.len() {
        for k in &snapshots[i - 1] {
            assert!(
                snapshots[i].contains(k),
                "第 {} 级把前一级认下的候选对弄丢了：{}",
                i + 1,
                k
            );
        }
    }
}

// ============================================================================
// ②③ 逐级对比表与数值
// ============================================================================

#[test]
fn 每一级都有召回率与精确率数值且对比表可写() {
    let (report, _funnel) = evaluate_bench();
    assert_eq!(report.levels.len(), 4);
    for level in &report.levels {
        assert!(
            level.recall.is_some(),
            "{} 级没有召回率数值",
            level.level_title
        );
        assert!(
            level.precision.is_some(),
            "{} 级没有精确率数值",
            level.level_title
        );
    }

    let csv = eval::to_csv(&report);
    assert!(
        csv.starts_with("级,候选数,命中标注,误报,召回率,精确率"),
        "表头不对"
    );
    assert_eq!(csv.lines().count(), 5, "表头 + 四级");

    let json = eval::to_json(&report);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON 可解析");
    assert_eq!(parsed["levels"].as_array().expect("levels 是数组").len(), 4);
    assert!(parsed["conclusion"]
        .as_str()
        .expect("有结论")
        .contains("基准集"));
}

#[test]
fn 逐级数值与人工标注一致() {
    let (report, _funnel) = evaluate_bench();

    // 基准集：7 条真重复标注 + 4 条负例
    assert_eq!(report.labeled_true_pairs, 7, "真重复标注 7 条");
    assert_eq!(report.labeled_negative_pairs, 4, "负例标注 4 条");
    assert_eq!(report.total_rows, 22, "两张表各 11 行");

    let l1 = &report.levels[0];
    assert_eq!(l1.level, "exact");
    assert_eq!(l1.new_pairs, 3, "L1 认下 3 对（取值完全一致）");
    assert_eq!(l1.candidate_pairs, 3);
    assert_eq!(l1.hit_labels, 3, "3 对全是真的");
    assert_eq!(l1.false_positives, 0, "L1 不误报");
    assert_eq!(l1.recall, Some(0.4286));
    assert_eq!(l1.precision, Some(1.0));

    let l2 = &report.levels[1];
    assert_eq!(l2.level, "normalize");
    assert_eq!(
        l2.new_pairs, 3,
        "L2 新增 3 对（指标名称别名写法 ×2 + 跨机构误报 ×1）"
    );
    assert_eq!(l2.candidate_pairs, 6);
    assert_eq!(l2.hit_labels, 5);
    assert_eq!(l2.false_positives, 1, "跨机构那一对是误报");
    assert_eq!(l2.fp_same_indicator, 1, "误报细分：同指标但不是同一件事");
    assert_eq!(l2.fp_not_duplicate, 0);
    assert_eq!(l2.fp_unlabeled, 0, "候选里没有没标注的对");
    assert_eq!(l2.recall, Some(0.7143));
    assert_eq!(l2.precision, Some(0.8333));

    let l3 = &report.levels[2];
    assert_eq!(l3.level, "distance");
    assert_eq!(l3.new_pairs, 1, "L3 新增 1 对（指标名称差 1 个字）");
    assert_eq!(l3.candidate_pairs, 7);
    assert_eq!(l3.hit_labels, 6);
    assert_eq!(l3.false_positives, 1);
    assert_eq!(l3.recall, Some(0.8571));
    assert_eq!(l3.precision, Some(0.8571));

    let l4 = &report.levels[3];
    assert_eq!(l4.level, "semantic");
    assert_eq!(l4.new_pairs, 0, "第四级本轮未接入模型端点");
    assert_eq!(l4.candidate_pairs, 7, "累计口径：与第三级相同");
}

#[test]
fn 漏掉的真重复被如实报出() {
    let (report, _funnel) = evaluate_bench();
    assert_eq!(
        report.missed_true_pairs.len(),
        1,
        "BK-12 那一对四级都认不出，要如实报出来"
    );
    let missed = &report.missed_true_pairs[0];
    assert!(missed.left_reference.contains("第 10 行"));
    assert!(missed.right_reference.contains("第 10 行"));
    assert!(!missed.basis.is_empty(), "漏报要带依据");
    assert!(
        report.conclusion.contains("四级都没报出来"),
        "结论里要写明漏报：{}",
        report.conclusion
    );
}

// ============================================================================
// ④ 标注不合法时报错，不静默跳过
// ============================================================================

#[test]
fn 标注定位不到行时报错并点名() {
    let mut set = load_bench_set();
    set.labels[0].left.row = 999;
    let err = eval::resolve(&set, &bench_rows()).expect_err("定位不到行必须报错");
    assert_eq!(err.code, regval::errors::ErrorCode::BenchSetInvalid);
    assert!(
        err.message.contains("找不到"),
        "报错要说明是定位问题：{}",
        err.message
    );
}

#[test]
fn 标注取值不认识时报错() {
    let mut set = load_bench_set();
    set.labels[0].label = "瞎写的".to_string();
    // 取值校验在装载期（load_bench），这里直接构造绕过装载也要在解析期挡住
    let json = serde_json::to_string(&set).expect("可序列化");
    let path = std::env::temp_dir().join("regval-bench-bad-label.json");
    std::fs::write(&path, json).expect("可写");
    let err = eval::load_bench(&path).expect_err("取值不认识必须报错");
    assert_eq!(err.code, regval::errors::ErrorCode::BenchSetInvalid);
    assert!(err.message.contains("瞎写的"));
    let _ = std::fs::remove_file(&path);
}

// ============================================================================
// ⑤ 可复现
// ============================================================================

#[test]
fn 同一份输入两次跑对比表逐字相同() {
    let (first, _) = evaluate_bench();
    let (second, _) = evaluate_bench();
    assert_eq!(eval::to_csv(&first), eval::to_csv(&second));
    assert_eq!(eval::to_json(&first), eval::to_json(&second));
}

// ============================================================================
// ⑥⑦ 第四级：未接入时照实报；接上后候选能并进来
// ============================================================================

#[test]
fn 第四级未接入模型端点时状态照实报出() {
    let (report, _funnel) = evaluate_bench();
    let l4 = &report.levels[3];
    assert_eq!(l4.new_pairs, 0);
    assert!(
        l4.note.contains("未接入模型端点"),
        "第四级要说清是「没接」而不是「查了没查到」：{}",
        l4.note
    );
    let marginal = report.marginal_gain.as_ref().expect("要有边际增益");
    assert_eq!(marginal.new_pairs, 0);
    assert!(marginal.note.contains("未接入模型端点"));
}

#[test]
fn 语义候选接上后能并入漏斗且单调性仍成立() {
    let rows = bench_rows();
    let cfg = DistanceConfig::default();
    let target = semantic_target_pair(&rows);
    let source = FakeSemanticSource {
        pairs: vec![target.clone()],
    };
    let funnel = run_funnel(&rows, &cfg, &source);
    let report = eval::evaluate(&resolved(), &funnel);

    let l4 = &report.levels[3];
    assert_eq!(l4.new_pairs, 1, "第四级认下了那一对");
    assert_eq!(l4.candidate_pairs, 8);
    assert_eq!(l4.hit_labels, 7, "真重复标注全部命中");
    assert_eq!(l4.recall, Some(1.0), "召回率补到 100%");
    assert_eq!(report.missed_true_pairs.len(), 0, "没有漏报的了");

    // 单调性仍然成立
    let mut acc = 0usize;
    for level in &funnel.levels {
        acc += level.new_pair_count();
        assert!(acc >= funnel.cumulative_len_at(level.level));
    }
    assert_eq!(acc, funnel.cumulative.len());
}

// ============================================================================
// ⑧ 端到端：regval bench
// ============================================================================

#[test]
fn 端到端_bench_子命令产出可核对的对比表() {
    let out = std::env::temp_dir().join("regval-bench-e2e.csv");
    let _ = std::fs::remove_file(&out);
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "bench",
            "--bench-dir",
            bench_dir().to_str().expect("路径可转字符串"),
            "--out",
            out.to_str().expect("路径可转字符串"),
        ])
        .output()
        .expect("regval bench 可执行");
    assert!(
        status.status.success(),
        "bench 退出码 {:?}：{}",
        status.status.code(),
        String::from_utf8_lossy(&status.stderr)
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("重复检测漏斗基准集"), "stdout：{}", stdout);
    assert!(stdout.contains("L4 语义归并"), "四级都要报出来：{}", stdout);

    let csv = std::fs::read_to_string(&out).expect("对比表已写出");
    assert_eq!(csv.lines().count(), 5, "表头 + 四级");
    assert!(csv.contains("L1 确定性键"));
    assert!(csv.contains("L4 语义归并"));

    // 再跑一次：逐字相同
    let out2 = std::env::temp_dir().join("regval-bench-e2e-2.csv");
    let _ = std::fs::remove_file(&out2);
    let status2 = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "bench",
            "--bench-dir",
            bench_dir().to_str().expect("路径可转字符串"),
            "--out",
            out2.to_str().expect("路径可转字符串"),
        ])
        .output()
        .expect("regval bench 可执行");
    assert!(status2.status.success());
    assert_eq!(
        csv,
        std::fs::read_to_string(&out2).expect("对比表已写出"),
        "同一份输入两次跑必须逐字相同"
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&out2);
}

#[test]
fn 端到端_标注文件不合法时报错且不产出对比表() {
    let dir = std::env::temp_dir().join("regval-bench-bad");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tables")).expect("可建目录");
    // 表格抄一份基准集的，标注改成一个不存在的行号
    for name in ["科室一表.csv", "科室二表.csv"] {
        std::fs::copy(
            bench_dir().join("tables").join(name),
            dir.join("tables").join(name),
        )
        .expect("可复制样例表");
    }
    let mut set = load_bench_set();
    set.labels[0].left.row = 999;
    std::fs::write(
        dir.join("labels.json"),
        serde_json::to_string_pretty(&set).expect("可序列化"),
    )
    .expect("可写");

    let out = dir.join("out.csv");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "bench",
            "--bench-dir",
            dir.to_str().expect("路径可转字符串"),
            "--out",
            out.to_str().expect("路径可转字符串"),
        ])
        .output()
        .expect("regval bench 可执行");
    assert!(!status.status.success(), "标注对不上必须非 0 退出");
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("BENCH_SET_INVALID"),
        "stderr 要给出机器可读的错误码：{}",
        stderr
    );
    assert!(!out.exists(), "报错时不得产出一份半截的对比表");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 基准集里三种标注都真的用上了（别让某一种标注白写）
#[test]
fn 基准集三种标注都用上了() {
    let set = load_bench_set();
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for label in &set.labels {
        *count.entry(label.label.as_str()).or_insert(0) += 1;
    }
    assert_eq!(count.get(LABEL_DUPLICATE_RECORD).copied(), Some(7));
    assert_eq!(count.get(eval::LABEL_SAME_INDICATOR).copied(), Some(3));
    assert_eq!(count.get(eval::LABEL_NOT_DUPLICATE).copied(), Some(1));
    // 每条标注都要有依据 —— 没有依据的标注不算标注
    for label in &set.labels {
        assert!(!label.basis.trim().is_empty(), "标注缺依据");
    }
}
