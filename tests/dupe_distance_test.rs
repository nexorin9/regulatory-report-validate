//! 重复检测四级漏斗 · 第三级（编辑距离候选，L3）的验收：
//! ① 阈值内的行对出候选对，并给出**距离值**与**哪几个字不同**的说明；
//! ② 阈值外的行对只计数、不出候选对；
//! ③ 名称长度差过大的行对被跳过并计数（不静默丢，也不把一次校验拖垮）；
//! ④ 与前两级不重复报；数值不同的对不归本级（归「待人工判定」）；
//! ⑤ **距离分布直方**可复现；
//! ⑥ 端到端：结果契约的 `dupe_distance` 段 + 候选对 + 「疑似重复项」列；
//! ⑦ `regval export dupe` 导出候选与直方；
//! ⑧ 阈值可配（环境变量调大调小，结论随之变化）；
//! ⑨ 只标疑似 —— 不合并、不推送、不推进任何状态。

use std::path::PathBuf;

use regval::dupe::distance::{self, DistanceConfig, MATCH_LEVEL_DISTANCE};
use regval::dupe::SuspectedDuplicate;
use regval::ingest;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 读样例表并映射成业务行
fn load_rows(name: &str) -> Vec<ingest::SourceRow> {
    let tables = ingest::read_tables(&fixture(name)).expect("样例表应可读");
    let mut rows = Vec::new();
    for table in &tables {
        rows.extend(ingest::map_table(table).expect("表头应完整"));
    }
    rows
}

/// 跑一次校验（把契约写进临时目录）
fn validate(out: &PathBuf, state_db: &PathBuf) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("dupe_distance.csv"))
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .output()
        .expect("regval 应可执行")
}

#[test]
fn 阈值内的行对出候选对并给出距离与差异说明() {
    let rows = load_rows("dupe_distance.csv");
    let out = distance::detect(&rows, &DistanceConfig::with_threshold(2));

    assert_eq!(
        out.pairs.len(),
        2,
        "BK-DIST-01（差 2 字）与 BK-DIST-02（差 1 字）各一对"
    );
    for pair in &out.pairs {
        assert_eq!(pair.match_level, MATCH_LEVEL_DISTANCE);
        assert_eq!(pair.state, "疑似重复", "只标疑似");
        assert!(!pair.basis.is_empty(), "判定依据必须落盘");
        let d = pair.distance.expect("命中本级就要有距离值");
        assert!(d >= 1 && d <= 2, "距离应在阈值内：{}", d);
        assert!(!pair.distance_detail.is_empty(), "差异说明必须落盘");
        // 差异说明要说清「哪几个字不同」
        assert!(
            pair.distance_detail.contains("多") || pair.distance_detail.contains("不同"),
            "差异说明要讲清差在哪：{}",
            pair.distance_detail
        );
        // 原值与归一值同时留证
        assert!(pair.left_original.contains("指标名称="));
        assert!(pair.right_original.contains("指标名称="));
    }
    // 两对的距离分别是 2 与 1
    let distances: Vec<usize> = out.pairs.iter().filter_map(|p| p.distance).collect();
    assert!(distances.contains(&2) && distances.contains(&1));
}

#[test]
fn 阈值外的行对只计数不出候选对() {
    let rows = load_rows("dupe_distance.csv");
    let out = distance::detect(&rows, &DistanceConfig::with_threshold(2));

    // BK-DIST-03：病案室 与 病案质量管理科 差 5 个字，超出阈值 2
    assert_eq!(out.stats.beyond_threshold_pairs, 1, "超阈值的对要计数");
    assert_eq!(out.stats.compared_pairs, 3, "比过距离的行对：3 对");
    let bk03: Vec<&SuspectedDuplicate> = out
        .pairs
        .iter()
        .filter(|p| p.business_key.contains("BK-DIST-03"))
        .collect();
    assert!(bk03.is_empty(), "超阈值的对不出候选");

    // 阈值调到 5 → 那一对进候选
    let relaxed = distance::detect(&rows, &DistanceConfig::with_threshold(5));
    assert_eq!(relaxed.stats.beyond_threshold_pairs, 0);
    assert_eq!(relaxed.pairs.len(), 3, "阈值放到 5 后 BK-DIST-03 进候选");
}

#[test]
fn 名称长度差过大的行对被跳过并计数() {
    let rows = load_rows("dupe_distance.csv");
    let out = distance::detect(&rows, &DistanceConfig::with_threshold(40));

    assert_eq!(
        out.stats.skipped_long_gap_pairs, 1,
        "BK-DIST-04 那一对名称长度差 45 字，应跳过并计数"
    );
    let bk04: Vec<&SuspectedDuplicate> = out
        .pairs
        .iter()
        .filter(|p| p.business_key.contains("BK-DIST-04"))
        .collect();
    assert!(bk04.is_empty(), "跳过的不出候选对");
}

#[test]
fn 与前两级不重复报且数值不同的对不归本级() {
    let rows = load_rows("dupe_distance.csv");
    let out = distance::detect(&rows, &DistanceConfig::with_threshold(2));

    assert_eq!(out.stats.exact_overlap_pairs, 1, "逐字一致的那一归 L1");
    assert_eq!(
        out.stats.normalize_overlap_pairs, 1,
        "归一后一致的那一归 L2"
    );
    assert_eq!(
        out.stats.value_conflict_pairs, 1,
        "数值不同的归「待人工判定」"
    );
    assert_eq!(out.stats.eligible_rows, 14);
    assert_eq!(out.stats.skipped_empty_name_rows, 1, "名称为空的行计数留痕");
}

#[test]
fn 距离分布直方可复现() {
    let rows = load_rows("dupe_distance.csv");
    let first = distance::detect(&rows, &DistanceConfig::with_threshold(2));
    let second = distance::detect(&rows, &DistanceConfig::with_threshold(2));

    assert_eq!(
        first.histogram, second.histogram,
        "同一份输入两次直方逐字相同"
    );
    assert_eq!(first.pairs, second.pairs);

    // 直方按距离分桶：距离 1 一对、距离 2 一对、超出阈值一对
    let bucket = |d: i64| -> usize {
        first
            .histogram
            .iter()
            .find(|b| b.distance == Some(d as usize))
            .map(|b| b.pairs)
            .unwrap_or(0)
    };
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(1), 1);
    assert_eq!(bucket(2), 1);
    let beyond = first
        .histogram
        .iter()
        .find(|b| b.distance.is_none())
        .map(|b| b.pairs)
        .unwrap_or(0);
    assert_eq!(beyond, 1, "超出阈值的行对也进直方（看得到分布的全貌）");
    // 直方总数 = 实际比过距离的行对数
    let total: usize = first.histogram.iter().map(|b| b.pairs).sum();
    assert_eq!(total, first.stats.compared_pairs);
}

#[test]
fn 端到端_契约里有编辑距离段与候选对且只标疑似() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = validate(&out, &state_db);
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("编辑距离候选 2 对"),
        "终端摘要要写清第三级抓了多少：{}",
        stdout
    );
    assert!(
        stdout.contains("距离分布"),
        "终端摘要要给出距离分布直方：{}",
        stdout
    );

    let contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    // 契约里的第三级段：阈值、来源、直方
    let section = &contract["dupe_distance"];
    assert_eq!(section["threshold"], 2);
    assert!(!section["threshold_source"].as_str().unwrap().is_empty());
    assert_eq!(section["max_length_gap"], 40);
    assert_eq!(section["pairs"], 2);
    assert!(section["histogram"].as_array().unwrap().len() >= 3);
    assert_eq!(section["stats"]["beyond_threshold_pairs"], 1);
    assert_eq!(section["stats"]["skipped_long_gap_pairs"], 1);
    assert_eq!(section["stats"]["value_conflict_pairs"], 1);

    // 候选对：确定性键 1 对 + 归一化 1 对 + 编辑距离 2 对
    let pairs = contract["suspected_duplicates"].as_array().unwrap();
    let distance_pairs: Vec<&serde_json::Value> = pairs
        .iter()
        .filter(|p| p["match_level"] == MATCH_LEVEL_DISTANCE)
        .collect();
    assert_eq!(distance_pairs.len(), 2);
    for pair in &distance_pairs {
        assert_eq!(pair["state"], "疑似重复");
        assert!(!pair["basis"].as_str().unwrap().is_empty());
        assert!(pair["distance"].as_u64().unwrap() >= 1);
        assert!(!pair["distance_detail"].as_str().unwrap().is_empty());
    }
    assert_eq!(contract["summary"]["suspected_duplicate_count"], 4);

    // 「疑似重复项」列：两对共四行要标出疑似对象
    let rows = contract["rows"].as_array().unwrap();
    let marked: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| !r["suspected_duplicate"].as_str().unwrap_or("").is_empty())
        .collect();
    assert_eq!(
        marked.len(),
        8,
        "确定性键 2 行 + 归一化 2 行 + 编辑距离 4 行"
    );
    let distance_marked = marked
        .iter()
        .filter(|r| {
            r["suspected_duplicate"]
                .as_str()
                .unwrap()
                .contains("字符级编辑距离")
        })
        .count();
    assert_eq!(distance_marked, 4, "编辑距离那两对的四行都要写出距离");

    // 行没少、状态没推进
    assert_eq!(rows.len(), 15, "15 行一行没少");
    for r in rows {
        assert_eq!(r["submission_status"], "未上报", "上报状态不由本级推进");
    }

    // 本机库里没有任何上报、回执与确认记录
    let repo = regval::db::repo::Repo::open(&state_db).expect("状态库应可打开");
    for table in ["submissions", "receipts", "confirmations"] {
        let n: i64 = repo
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("统计 {} 应成功：{}", table, e));
        assert_eq!(n, 0, "{} 表应为空 —— 本级只标疑似，不上报", table);
    }
}

#[test]
fn 导出编辑距离候选与距离分布直方() {
    let dir = tempfile::tempdir().unwrap();
    let contract = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let output = validate(&contract, &state_db);
    assert!(output.status.success());

    // CSV：候选对一行一对
    let csv_out = dir.path().join("dupe.csv");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["export", "dupe"])
        .arg("--contract")
        .arg(&contract)
        .arg("--out")
        .arg(&csv_out)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "导出应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let csv_text = std::fs::read_to_string(&csv_out).unwrap();
    let lines: Vec<&str> = csv_text.lines().collect();
    assert_eq!(lines.len(), 3, "表头 + 两对候选");
    assert!(lines[0].contains("字符级编辑距离"), "表头要说明距离列");
    assert!(lines[0].contains("差异说明"));
    assert!(
        csv_text.contains("字符级编辑距离"),
        "候选行要带距离：{}",
        csv_text
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("距离 1 1 对"),
        "导出摘要要带直方：{}",
        stdout
    );

    // JSON：阈值 + 直方 + 候选对
    let json_out = dir.path().join("dupe.json");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["export", "dupe"])
        .arg("--contract")
        .arg(&contract)
        .arg("--out")
        .arg(&json_out)
        .output()
        .expect("regval 应可执行");
    assert!(output.status.success());
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_out).unwrap()).unwrap();
    assert_eq!(value["threshold"], 2);
    assert_eq!(value["pairs"].as_array().unwrap().len(), 2);
    assert!(value["histogram"].as_array().unwrap().len() >= 3);

    // 不给契约路径 → 明确报错，不静默导出空表
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["export", "dupe"])
        .arg("--out")
        .arg(dir.path().join("nope.json"))
        .output()
        .expect("regval 应可执行");
    assert!(!output.status.success(), "缺契约路径应报错");
    assert!(!dir.path().join("nope.json").exists(), "报错时不产出文件");
}

#[test]
fn 阈值可配_环境变量调小则超阈值的对退回候选之外() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("dupe_distance.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .env("REGVAL_DUP_DISTANCE_THRESHOLD", "1")
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        contract["dupe_distance"]["threshold"], 1,
        "阈值取环境变量的值"
    );
    assert!(
        contract["dupe_distance"]["threshold_source"]
            .as_str()
            .unwrap()
            .contains("REGVAL_DUP_DISTANCE_THRESHOLD"),
        "阈值来源要写明是环境变量"
    );
    let distance_pairs = contract["suspected_duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["match_level"] == MATCH_LEVEL_DISTANCE)
        .count();
    assert_eq!(distance_pairs, 1, "阈值收紧到 1 后只剩差 1 字的那一对");
}
