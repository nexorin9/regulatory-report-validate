//! 主干最窄端到端切片的验收测试：
//! `读表 → 落本机库 → 一条口径规则 → 结果契约 JSON`。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 跑一次 `regval validate`，返回 (退出码, stdout, stderr)
fn run_validate(input: &Path, out: &Path, state_db: &Path) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .output()
        .expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn 主干跑通并产出可核对的结果契约() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("result.json");
    let state_db = dir.path().join("state.db");

    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db);
    assert!(ok, "validate 应成功退出，stderr: {}", stderr);
    assert!(stdout.contains("已校验 6 行"), "应报出校验行数：{}", stdout);

    // 契约可被 serde_json 解析
    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    // schema_version 与引擎常量一致
    assert_eq!(
        contract["schema_version"],
        Value::String(regval::contract::SCHEMA_VERSION.to_string())
    );

    // 行结果非空且行数与样例一致
    let rows = contract["rows"].as_array().expect("rows 应为数组");
    assert_eq!(rows.len(), 6, "行数应与样例一致");
    assert!(!rows[0]["row_uid"].as_str().unwrap().is_empty());

    // 5 个新列的写回定义齐备
    let columns = contract["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 5);
    assert_eq!(columns[0]["name"], "校验结论");
    assert_eq!(columns[4]["name"], "上报状态");

    // 问题清单非空：样例里埋了 2 处必填缺失
    let issues = contract["issues"].as_array().expect("issues 应为数组");
    assert_eq!(issues.len(), 2, "应检出 2 处必填缺失：{}", raw);
    let kinds: Vec<&str> = issues
        .iter()
        .map(|i| i["issue_kind"].as_str().unwrap())
        .collect();
    assert!(kinds.iter().all(|k| *k == "必填缺失"));
    assert!(issues
        .iter()
        .all(|i| !i["basis"].as_str().unwrap().is_empty()));

    // 有问题的行判「不通过」，没问题的行判「通过」
    let conclusions: Vec<&str> = rows
        .iter()
        .map(|r| r["conclusion"].as_str().unwrap())
        .collect();
    assert_eq!(conclusions.iter().filter(|c| **c == "不通过").count(), 2);
    assert_eq!(conclusions.iter().filter(|c| **c == "通过").count(), 4);

    // 未上报的行状态列统一为「未上报」（状态只由网关回执驱动）
    assert!(rows.iter().all(|r| r["submission_status"] == "未上报"));
}

#[test]
fn 缺必填表头列时报错并点名缺哪一列() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("result.json");
    let state_db = dir.path().join("state.db");

    let (ok, _stdout, stderr) = run_validate(&fixture("mini_missing_col.csv"), &out, &state_db);
    assert!(!ok, "缺必填列时应以非 0 退出");
    assert!(
        stderr.contains("MISSING_COLUMN"),
        "应报出错误码：{}",
        stderr
    );
    assert!(
        stderr.contains("科室代码"),
        "应明确指出缺哪一列：{}",
        stderr
    );
    assert!(!out.exists(), "出错时不应产出契约文件");
}

#[test]
fn 同一份输入重跑批次号不变且行不重复累积() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");

    let out1 = dir.path().join("r1.json");
    let (ok1, _, _) = run_validate(&fixture("mini.csv"), &out1, &state_db);
    assert!(ok1);

    let out2 = dir.path().join("r2.json");
    let (ok2, _, _) = run_validate(&fixture("mini.csv"), &out2, &state_db);
    assert!(ok2);

    let c1: Value = serde_json::from_str(&std::fs::read_to_string(&out1).unwrap()).unwrap();
    let c2: Value = serde_json::from_str(&std::fs::read_to_string(&out2).unwrap()).unwrap();
    assert_eq!(c1["batch_ref"], c2["batch_ref"], "同一份输入应得同一批次号");
    assert_eq!(
        c1["rows"].as_array().unwrap().len(),
        c2["rows"].as_array().unwrap().len(),
        "重跑不应让行数翻倍"
    );

    // 本机库里本批仍是 6 行（重跑先清同批旧行）
    let repo = regval::db::repo::Repo::open(&state_db).unwrap();
    let n = repo.count_rows(c1["batch_ref"].as_str().unwrap()).unwrap();
    assert_eq!(n, 6);
}

#[test]
fn 规则只作用于本批次不串批() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");

    // 第一批：样例原样
    let out1 = dir.path().join("b1.json");
    let (ok1, _, _) = run_validate(&fixture("mini.csv"), &out1, &state_db);
    assert!(ok1);

    // 第二批：内容不同（改一个科室代码）→ 批次号不同，但同样有 2 处必填缺失
    let raw = std::fs::read_to_string(fixture("mini.csv")).unwrap();
    let other = dir.path().join("other.csv");
    std::fs::write(&other, raw.replace("0301", "0309")).unwrap();

    let out2 = dir.path().join("b2.json");
    let (ok2, _, _) = run_validate(&other, &out2, &state_db);
    assert!(ok2);

    let c1: Value = serde_json::from_str(&std::fs::read_to_string(&out1).unwrap()).unwrap();
    let c2: Value = serde_json::from_str(&std::fs::read_to_string(&out2).unwrap()).unwrap();
    assert_ne!(c1["batch_ref"], c2["batch_ref"], "两批应不同批次号");
    assert_eq!(
        c2["issues"].as_array().unwrap().len(),
        2,
        "第二批的问题数不应把第一批算进来：{}",
        c2["issues"]
    );
    // 第二批的行键不应出现第一批的行键
    let b1_uids: Vec<&str> = c1["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["row_uid"].as_str().unwrap())
        .collect();
    for row in c2["rows"].as_array().unwrap() {
        let uid = row["row_uid"].as_str().unwrap();
        assert!(!b1_uids.contains(&uid), "第二批出现第一批行键 {}", uid);
    }
}
