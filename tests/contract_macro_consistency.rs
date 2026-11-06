//! 契约一致性测试：引擎与 Excel 宏必须说同一套话。
//!
//! 引擎的列定义与宏里的常量一旦漂移，宏就会把结果写错列 —— 这类错在表上
//! 看不出来，只有被测出来才拦得住。所以本测试直接读 `macro/vba/RegVal.bas`，
//! 把里面的契约常量抠出来与引擎常量逐项比对。

use std::path::PathBuf;

use serde_json::Value;

use regval::contract::{NEW_COLUMNS, SCHEMA_VERSION};
use regval::errors::ErrorCode;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn macro_source() -> String {
    let path = project_dir().join("macro/vba/RegVal.bas");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到宏文件 {}：{}", path.display(), e))
}

/// 抠出 `Public Const <名字> As String = "值"` 的值
fn const_string(source: &str, name: &str) -> String {
    let needle = format!("Const {} As String = \"", name);
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("宏里找不到常量 {}", name))
        + needle.len();
    let rest = &source[start..];
    let end = rest.find('"').expect("常量值应有结束引号");
    rest[..end].to_string()
}

#[test]
fn 宏的契约版本与引擎一致() {
    let source = macro_source();
    assert_eq!(
        const_string(&source, "REGVAL_SCHEMA_VERSION"),
        SCHEMA_VERSION,
        "宏与引擎的契约版本必须一致，否则宏拒绝写回"
    );
}

#[test]
fn 宏的列名数组与引擎一致() {
    let source = macro_source();
    let raw = const_string(&source, "REGVAL_COLUMN_NAMES");
    let macro_columns: Vec<String> = raw.split('|').map(|s| s.to_string()).collect();
    let engine_columns: Vec<String> = NEW_COLUMNS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        macro_columns, engine_columns,
        "宏里的 5 个新列与引擎写回列必须逐字一致"
    );
    assert_eq!(macro_columns.len(), 5, "写回新列固定 5 个");
}

#[test]
fn 版本不一致时的提示文案与引擎错误码口径一致() {
    let source = macro_source();
    // 宏侧的提示文案
    assert!(
        source.contains("引擎与宏版本不一致，请升级"),
        "宏里必须有版本不一致的提示文案"
    );
    // 引擎侧的错误码与中文说明
    assert_eq!(
        ErrorCode::ContractVersionMismatch.as_str(),
        "CONTRACT_VERSION_MISMATCH"
    );
    assert_eq!(
        ErrorCode::ContractVersionMismatch.zh(),
        "引擎与宏版本不一致，请升级",
        "引擎错误码的中文说明应与宏里的提示文案同一口径"
    );
}

#[test]
fn 宏用引号包裹绝对路径_支持空格与中文路径() {
    let source = macro_source();
    let quoted = source.matches("Chr$(34)").count();
    assert!(
        quoted >= 6,
        "宏里所有绝对路径都应经 Chr$(34) 包引号，实际只包了 {} 处",
        quoted
    );
    // 命令行拼接处必须成对出现：路径 + 空格 + 参数
    assert!(
        source.contains("\" validate\""),
        "宏应拼出引擎的 validate 子命令"
    );
}

#[test]
fn 宏写回用批量赋值而不是逐格写() {
    let source = macro_source();
    assert!(
        source.contains(".Value2 = arr"),
        "宏应把 5 列一次性批量赋值（Range.Value2），避免大表逐格写回卡顿"
    );
    assert!(
        source.contains("REGVAL_CHUNK_ROWS"),
        "宏应分块写回，每块一次批量赋值"
    );
}

/// 抠出宏里所有 `JsonStringValue(..., "键")` / `JsonNumberValue(..., "键")` /
/// `JsonArrayText(..., "键")` 读的键
fn macro_read_keys(source: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Private Function")
            || trimmed.starts_with("Public Function")
            || trimmed.starts_with("Function")
        {
            continue; // 函数声明行不是调用点
        }
        for func in ["JsonStringValue(", "JsonNumberValue(", "JsonArrayText("] {
            let mut pos = 0usize;
            while let Some(rel) = line[pos..].find(func) {
                let tail = &line[pos + rel + func.len()..];
                // 第二个参数是键名（第一个参数是被读的文本）
                let first_comma = tail.find(',').unwrap_or(tail.len());
                let rest = &tail[first_comma + 1..];
                if let Some(q) = rest.find('"') {
                    let after = &rest[q + 1..];
                    if let Some(end) = after.find('"') {
                        let key = after[..end].to_string();
                        if !key.is_empty() && !keys.contains(&key) {
                            keys.push(key);
                        }
                    }
                }
                pos = pos + rel + func.len();
            }
        }
    }
    keys
}

/// JSON 里是否存在这个键（任意层级）
fn json_has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(key) || map.values().any(|v| json_has_key(v, key)),
        Value::Array(items) => items.iter().any(|v| json_has_key(v, key)),
        _ => false,
    }
}

/// 宏从**错误报告**里读的键（不在结果契约里 —— 那是另一份文件）。
///
/// 引擎失败时按 `--error-out` 写一份 JSON：宏按字段读错误码与中文说明，
/// 不去解析人类可读的 stderr（终端文案一改就断）。
const ERROR_REPORT_KEYS: &[&str] = &["code", "code_zh", "message", "advice"];

#[test]
fn 宏读的每一个键都真实存在于结果契约里() {
    let source = macro_source();
    let keys = macro_read_keys(&source);
    assert!(keys.len() >= 8, "应抠出宏读的全部键，实际 {}", keys.len());

    // 跑一次真实的引擎，拿到真实契约
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(project_dir().join("tests/fixtures/mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    for key in &keys {
        if ERROR_REPORT_KEYS.contains(&key.as_str()) {
            continue; // 这几个键属于错误报告，由下面那条用例核对
        }
        assert!(
            json_has_key(&contract, key),
            "宏要读的键「{}」在结果契约里不存在，写回会写成空值",
            key
        );
    }
}

#[test]
fn 宏从错误报告里读的每个键都真实存在于错误报告里() {
    // 引擎失败时写一份错误报告：宏按 code / code_zh / message / advice 读
    let dir = tempfile::tempdir().unwrap();
    let err_path = dir.path().join("error.json");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(dir.path().join("这张表根本不存在.csv"))
        .arg("--out")
        .arg(dir.path().join("contract.json"))
        .arg("--state-db")
        .arg(dir.path().join("state.db"))
        .arg("--error-out")
        .arg(&err_path)
        .output()
        .expect("regval 应可执行");
    assert!(
        !output.status.success(),
        "输入不存在时应以非 0 退出，宏据此中止写回"
    );
    assert!(err_path.exists(), "失败时应写出错误报告 JSON");

    let raw = std::fs::read_to_string(&err_path).unwrap();
    let report: Value = serde_json::from_str(&raw).expect("错误报告 JSON 应可解析");

    for key in ERROR_REPORT_KEYS {
        assert!(
            json_has_key(&report, key),
            "错误报告里少了「{}」，宏会读出空值",
            key
        );
    }
    assert_eq!(
        report["code"], "INPUT_UNREADABLE",
        "错误报告应给出机器可读的错误码"
    );
    assert!(
        !report["code_zh"].as_str().unwrap_or("").is_empty(),
        "错误报告应给出中文说明"
    );
    assert!(
        !report["advice"].as_str().unwrap_or("").is_empty(),
        "错误报告应给出处理建议"
    );
}
