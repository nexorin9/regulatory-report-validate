//! 口径规则引擎 A-9 的验收：问题清单汇总（两个维度）与每行依据举证包。
//!
//! 验收点：
//! - 四类口径问题齐全时，四类计数正确；
//! - 举证包行数与行状态计数一致；
//! - 每条问题都带 basis 字段；
//! - 命中 0 条规则的行也在举证包里，标「无命中」。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use regval::report::evidence::{EVIDENCE_SCHEMA_VERSION, MATCH_HIT, MATCH_NONE, NO_HIT_SUGGESTION};
use regval::report::summary::{
    conclusion_of, CONCLUSION_BLOCKED, CONCLUSION_HINT, CONCLUSION_PASS,
};
use regval::rules::model::Issue;

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

/// 跑一次 `regval export evidence`，返回 (退出码, stdout, stderr)
fn run_export_evidence(out: &Path, state_db: &Path, batch: Option<&str>) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(["export", "evidence"])
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        // 逐项官方条款依据取自规则目录里的口径种子文件
        .env("REGVAL_RULE_DIR", project_dir().join("rules"));
    if let Some(b) = batch {
        cmd.arg("--batch").arg(b);
    }
    let output = cmd.output().expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 校验一次 + 导出一份举证包，返回 (结果契约, 举证包)
fn validate_and_export(dir: &Path) -> (Value, Value) {
    let contract_path = dir.join("result.json");
    let evidence_path = dir.join("evidence.json");
    let state_db = dir.join("state.db");

    let (ok, stdout, stderr) = run_validate(&fixture("four_kinds.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("已校验 7 行"), "应报出校验行数：{}", stdout);

    let (ok, stdout, stderr) = run_export_evidence(&evidence_path, &state_db, None);
    assert!(ok, "export evidence 应成功退出，stderr：{}", stderr);
    assert!(
        stdout.contains("依据举证包已写出"),
        "应报出举证包已写出：{}",
        stdout
    );

    let contract: Value =
        serde_json::from_str(&std::fs::read_to_string(&contract_path).unwrap()).unwrap();
    let evidence: Value =
        serde_json::from_str(&std::fs::read_to_string(&evidence_path).unwrap()).unwrap();
    (contract, evidence)
}

#[test]
fn 四类口径问题齐全时四类计数正确() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, _evidence) = validate_and_export(dir.path());

    let issues = contract["issues"].as_array().expect("issues 应为数组");
    // 必填缺失 2（两行指标名称缺列）· 枚举越界 1（报告期写法）·
    // 单位不符 1（比率型指标单位写成例）· 疑似重复 4（两组表内业务主键撞键）
    assert_eq!(issues.len(), 8, "问题总数应正确：{}", contract["issues"]);

    let by_kind = contract["summary"]["issue_count_by_kind"]
        .as_object()
        .expect("按问题类型计数应为对象");
    assert_eq!(by_kind.get("必填缺失"), Some(&Value::from(2)));
    assert_eq!(by_kind.get("枚举越界"), Some(&Value::from(1)));
    assert_eq!(by_kind.get("单位不符"), Some(&Value::from(1)));
    assert_eq!(by_kind.get("疑似重复"), Some(&Value::from(4)));
    assert_eq!(by_kind.len(), 4, "不该冒出第五类问题类型：{:?}", by_kind);

    // 举证包里的类型计数与契约一致（两个维度出自同一份问题集合）
    let evidence = &dir_evidence(dir.path());
    let e_by_kind = evidence["counts"]["by_issue_kind"].as_object().unwrap();
    for kind in ["必填缺失", "枚举越界", "单位不符", "疑似重复"] {
        assert_eq!(
            e_by_kind.get(kind),
            by_kind.get(kind),
            "举证包与契约的「{}」计数应一致",
            kind
        );
    }
}

/// 读回刚导出的举证包 JSON
fn dir_evidence(dir: &Path) -> Value {
    let path = dir.join("evidence.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn 每条问题都带_basis_字段() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, evidence) = validate_and_export(dir.path());

    for issue in contract["issues"].as_array().unwrap() {
        let basis = issue["basis"].as_str().expect("basis 应为字符串");
        assert!(
            !basis.trim().is_empty(),
            "问题 {} 缺官方依据",
            issue["rule_id"]
        );
    }
    for row in evidence["rows"].as_array().unwrap() {
        for hit in row["hits"].as_array().unwrap() {
            assert!(
                !hit["basis"].as_str().unwrap().trim().is_empty(),
                "命中 {} 缺官方依据",
                hit["rule_id"]
            );
            assert!(
                !hit["suggestion"].as_str().unwrap().trim().is_empty(),
                "命中 {} 缺建议修改动作",
                hit["rule_id"]
            );
        }
    }
}

#[test]
fn 举证包行数与行状态计数一致() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, evidence) = validate_and_export(dir.path());

    let contract_rows = contract["rows"].as_array().unwrap();
    let evidence_rows = evidence["rows"].as_array().unwrap();

    assert_eq!(
        evidence_rows.len(),
        contract_rows.len(),
        "举证包行数应等于行结果数"
    );
    assert_eq!(evidence["counts"]["rows"].as_u64().unwrap(), 7);
    assert_eq!(
        evidence["counts"]["rows_with_hits"].as_u64().unwrap()
            + evidence["counts"]["rows_without_hits"].as_u64().unwrap(),
        evidence["counts"]["rows"].as_u64().unwrap(),
        "有命中 + 无命中应等于总行数"
    );
    let hit_sum: u64 = evidence_rows
        .iter()
        .map(|r| r["hit_count"].as_u64().unwrap())
        .sum();
    assert_eq!(hit_sum, evidence["counts"]["hits"].as_u64().unwrap());

    // 逐行对得上：同一行键在两边的结论必须一致
    for row in contract_rows {
        let uid = row["row_uid"].as_str().unwrap();
        let e = evidence_rows
            .iter()
            .find(|r| r["row_uid"].as_str() == Some(uid))
            .unwrap_or_else(|| panic!("举证包缺行 {}", uid));
        assert_eq!(
            e["conclusion"].as_str().unwrap(),
            row["conclusion"].as_str().unwrap(),
            "行 {} 的校验结论两边应一致",
            uid
        );
    }
}

#[test]
fn 命中零条规则的行也在举证包里且标无命中() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, evidence) = validate_and_export(dir.path());

    // 样例第 1 行取值齐全 —— 没有任何规则命中它
    let pass_uid = contract["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["conclusion"] == "通过")
        .expect("样例里应有一行通过")["row_uid"]
        .as_str()
        .unwrap()
        .to_string();

    let row = evidence["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["row_uid"].as_str() == Some(pass_uid.as_str()))
        .expect("未命中的行也必须在举证包里");
    assert_eq!(row["match_state"], MATCH_NONE);
    assert_eq!(row["hit_count"], 0);
    assert_eq!(row["hits"].as_array().unwrap().len(), 0);
    assert_eq!(row["suggestion"], NO_HIT_SUGGESTION);
    assert_eq!(row["conclusion"], CONCLUSION_PASS);
}

#[test]
fn 命中行标命中并给出建议修改动作() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, evidence) = validate_and_export(dir.path());

    let hit_rows: Vec<&Value> = evidence["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["match_state"] == MATCH_HIT)
        .collect();
    assert_eq!(hit_rows.len(), 6, "样例里 6 行有命中");
    for row in &hit_rows {
        assert!(row["hit_count"].as_u64().unwrap() > 0);
        assert!(!row["suggestion"].as_str().unwrap().trim().is_empty());
        // 必填缺失的命中要说清缺哪几列
        if let Some(hit) = row["hits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["issue_kind"] == "必填缺失")
        {
            assert_eq!(
                hit["missing_columns"],
                serde_json::json!(["指标名称"]),
                "缺失列清单要带上列名"
            );
            assert!(
                hit["suggestion"].as_str().unwrap().contains("指标名称"),
                "建议动作要说清缺哪列：{}",
                hit["suggestion"]
            );
        }
    }

    // 只有提示级问题的行判「有提示」（不拦上报，但要科室看一眼）
    let hint_rows: Vec<&Value> = evidence["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["conclusion"] == CONCLUSION_HINT)
        .collect();
    assert_eq!(hint_rows.len(), 2, "样例里 2 行只有提示级问题");
    for row in &hint_rows {
        for hit in row["hits"].as_array().unwrap() {
            assert_eq!(hit["severity"], "提示");
        }
    }
}

#[test]
fn 结论取值三档都对() {
    let mk = |severity: &str| Issue {
        row_uid: "u1".into(),
        rule_id: "R-X".into(),
        issue_kind: "必填缺失".into(),
        severity: severity.into(),
        message: "说明".into(),
        basis: "依据".into(),
        issue_key: String::new(),
        missing_columns: vec![],
        error_code: String::new(),
    };
    assert_eq!(conclusion_of(&[]), CONCLUSION_PASS);
    assert_eq!(conclusion_of(&[mk("提示")]), CONCLUSION_HINT);
    assert_eq!(conclusion_of(&[mk("阻断")]), CONCLUSION_BLOCKED);
    assert_eq!(
        conclusion_of(&[mk("提示"), mk("阻断")]),
        CONCLUSION_BLOCKED,
        "有阻断级问题就该判不通过"
    );
}

#[test]
fn csv_举证包一次命中一行且无命中行也出行() {
    let dir = tempfile::tempdir().unwrap();
    let contract_path = dir.path().join("result.json");
    let csv_path = dir.path().join("evidence.csv");
    let state_db = dir.path().join("state.db");

    let (ok, _stdout, stderr) = run_validate(&fixture("four_kinds.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    let (ok, _stdout, stderr) = run_export_evidence(&csv_path, &state_db, None);
    assert!(ok, "export evidence 应成功退出，stderr：{}", stderr);

    let text = std::fs::read_to_string(&csv_path).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // 表头 + 8 次命中 + 1 行无命中
    assert_eq!(lines.len(), 10, "CSV 行数不对：\n{}", text);
    assert!(lines[0].contains("官方依据"));
    assert!(lines[0].contains("建议修改动作"));
    assert!(lines.iter().any(|l| l.contains(MATCH_NONE)));
}

#[test]
fn 举证包带版本号且可被第三方解析() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, evidence) = validate_and_export(dir.path());
    assert_eq!(evidence["schema_version"], EVIDENCE_SCHEMA_VERSION);
    assert!(!evidence["batch_ref"].as_str().unwrap().is_empty());
    assert!(
        !evidence["rule_set_version"].as_str().unwrap().is_empty(),
        "举证包应记下这批结论按哪套口径算的"
    );
}

#[test]
fn 举证包引到逐项官方条款依据_不只是规则级依据() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, evidence) = validate_and_export(dir.path());

    // 逐项依据：MER-TL-04 在官方附件里是「指标七」
    let row = evidence["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["indicator_code"] == "MER-TL-04")
        .expect("举证包应含 MER-TL-04 那一行");
    assert_eq!(row["official_clause"], "指标七");
    let basis = row["indicator_basis"].as_str().unwrap();
    assert!(
        basis.contains("MER-TL-04") && basis.contains("病案首页24小时内完成率"),
        "逐项依据应点到本指标的条款与名称：{}",
        basis
    );

    // 依据来源要说清楚出自哪一版口径种子
    let note = evidence["indicator_basis_note"].as_str().unwrap();
    assert!(
        note.contains("2021.1"),
        "逐项依据来源说明应给出种子版本：{}",
        note
    );
}

#[test]
fn 没有口径种子时逐项依据为空且说明原因() {
    let dir = tempfile::tempdir().unwrap();
    let contract_path = dir.path().join("result.json");
    let evidence_path = dir.path().join("evidence.json");
    let state_db = dir.path().join("state.db");

    let (ok, _stdout, stderr) = run_validate(&fixture("four_kinds.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    // 不配 REGVAL_RULE_DIR → 找不到口径种子 → 逐项依据为空，但要说清为什么
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["export", "evidence"])
        .arg("--out")
        .arg(&evidence_path)
        .arg("--state-db")
        .arg(&state_db)
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .expect("regval 应可执行");
    assert!(output.status.success());

    let evidence: Value =
        serde_json::from_str(&std::fs::read_to_string(&evidence_path).unwrap()).unwrap();
    assert_eq!(evidence["rows"][0]["indicator_basis"], "");
    let note = evidence["indicator_basis_note"].as_str().unwrap();
    assert!(
        note.contains("未取到逐项官方条款依据"),
        "应说明为什么没有逐项依据：{}",
        note
    );
}

#[test]
fn 导出不存在的批次时报错且不产出文件() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out = dir.path().join("evidence.json");

    let (ok, _stdout, stderr) = run_validate(
        &fixture("four_kinds.csv"),
        &dir.path().join("r.json"),
        &state_db,
    );
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    let (ok, _stdout, stderr) = run_export_evidence(&out, &state_db, Some("不存在的批次"));
    assert!(!ok, "批次不存在时应以非 0 退出");
    assert!(stderr.contains("EMPTY_TABLE"), "应报出错误码：{}", stderr);
    assert!(!out.exists(), "出错时不应产出举证包文件");
}
