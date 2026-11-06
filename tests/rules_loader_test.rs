//! 版本化口径规则表的装载与换版验收。
//!
//! 覆盖 Task 7 的三条硬验收：
//! 1. 同一目录装载两次结果一致（装载可复现）；
//! 2. 旧版本标为「已被取代」但**保留可追**（不物理删除）；
//! 3. 缺依据的规则**拒绝装载**并报出明确错误码，不以空依据进入执行。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use regval::errors::ErrorCode;
use regval::rules::loader;
use regval::rules::model::RuleStatus;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 一条规则（文件形态，可指定检查类型 —— 决定它落在哪个口径槽位）
fn rule_def_kind(version: &str, basis: &str, rule_id: &str, kind: &str) -> Value {
    json!({
        "rule_id": rule_id,
        "indicator_code": Value::Null,
        "rule_kind": kind,
        "sql_lines": [
            "SELECT row_uid AS row_uid, :label_1 AS missing_column",
            "  FROM rows",
            " WHERE batch_ref = :batch_ref",
            "   AND (indicator_code IS NULL OR trim(indicator_code) = '')"
        ],
        "params": {"scope": Value::Null, "label_1": "指标编号"},
        "basis": basis,
        "version": version,
        "severity": "blocking",
        "effective_from": Value::Null
    })
}

/// 写一张规则表文件
fn write_table(dir: &Path, file_name: &str, version: &str, basis: &str, rule_id: &str) {
    write_table_kind(dir, file_name, version, basis, rule_id, "required");
}

/// 写一张规则表文件（可指定检查类型）
fn write_table_kind(
    dir: &Path,
    file_name: &str,
    version: &str,
    basis: &str,
    rule_id: &str,
    kind: &str,
) {
    let table = json!({
        "rule_set": version,
        "rules": [rule_def_kind(version, basis, rule_id, kind)]
    });
    std::fs::write(
        dir.join(file_name),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();
}

/// 跑一次 `regval validate`，返回 (是否成功, stdout, stderr)
fn run_validate(
    input: &Path,
    out: &Path,
    state_db: &Path,
    rule_dir: Option<&Path>,
) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(["validate"])
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db);
    match rule_dir {
        Some(d) => cmd.env("REGVAL_RULE_DIR", d),
        None => cmd.env_remove("REGVAL_RULE_DIR"),
    };
    let output = cmd.output().expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn 同一目录装载两次结果一致() {
    let dir = tempfile::tempdir().unwrap();
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "国卫办医函〔2021〕28号 附件",
        "R-REQUIRED-001",
    );
    write_table_kind(
        dir.path(),
        "b-2022.1.rules.json",
        "2022.1",
        "属地 2022 版口径文件",
        "R-UNIT-001",
        "unit",
    );

    let one = loader::load(dir.path()).unwrap();
    let two = loader::load(dir.path()).unwrap();

    assert_eq!(one.manifest(), two.manifest(), "两次装载的规则清单必须一致");
    assert_eq!(
        one.status_section(),
        two.status_section(),
        "两次装载的逐条规则状态必须一致"
    );
    assert_eq!(one.manifest().total, 2);
    assert_eq!(one.manifest().active, 2);
}

#[test]
fn 旧版本标为已被取代且仍可查() {
    let dir = tempfile::tempdir().unwrap();
    // 同一口径槽位（检查类型 required · 适用指标 全部）的两个版本
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "国卫办医函〔2021〕28号 附件",
        "R-REQUIRED-001",
    );
    write_table(
        dir.path(),
        "b-2022.1.rules.json",
        "2022.1",
        "属地 2022 版口径文件",
        "R-REQUIRED-001",
    );

    let set = loader::load(dir.path()).unwrap();
    let m = set.manifest();

    assert_eq!(m.total, 2, "两条规则都在清单里");
    assert_eq!(m.active, 1, "同一槽位只有一条生效");
    assert_eq!(m.superseded, 1, "旧版本标为已被取代");
    assert_eq!(m.slots, 1, "两条规则同属一个口径槽位");
    assert_eq!(m.by_status.get("生效"), Some(&1));
    assert_eq!(m.by_status.get("已被取代"), Some(&1));

    // 高版本生效
    let active = set.executable();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].version, "2022.1");

    // 旧版本**保留可追**：仍在规则集里、状态为「已被取代」、依据原文还在
    let old = set.by_version("2021.1");
    assert_eq!(old.len(), 1, "旧版本不能被物理删除");
    assert_eq!(old[0].status, RuleStatus::Superseded);
    assert_eq!(old[0].basis, "国卫办医函〔2021〕28号 附件");

    // 旧版本不参与执行
    assert!(
        !active.iter().any(|r| r.version == "2021.1"),
        "已被取代的旧版本不得参与执行"
    );

    // 规则清单里能看到旧版本（运维可查）
    let section = set.status_section();
    let old_entry = section
        .rules
        .iter()
        .find(|r| r.version == "2021.1")
        .expect("清单里应保留旧版本");
    assert_eq!(old_entry.status, "已被取代");
    assert!(!old_entry.basis.is_empty(), "旧版本的依据要留着可追");
}

#[test]
fn 缺依据的规则拒绝装载并报出明确错误码() {
    let dir = tempfile::tempdir().unwrap();
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "", // 缺依据
        "R-REQUIRED-001",
    );

    // 严格模式（validate 走这条）：直接报错，点名规则与文件
    let err = loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleBasisMissing);
    assert_eq!(err.code.as_str(), "RULE_BASIS_MISSING");
    assert!(
        err.message.contains("R-REQUIRED-001"),
        "报错要点名是哪条规则：{}",
        err.message
    );
    let location = err.location.clone().expect("报错应给出位置");
    assert!(
        location.contains("a-2021.1.rules.json"),
        "报错要指出来自哪个规则表文件：{}",
        location
    );

    // 汇报模式（regval rules 走这条）：规则被剔除出执行集合，但计入依据缺失计数
    let set = loader::load_reporting(dir.path()).unwrap();
    assert_eq!(set.executable().len(), 0, "缺依据的规则不得进入执行集合");
    assert_eq!(set.manifest().basis_missing, 1);
    assert_eq!(set.manifest().rejected.len(), 1);
    assert_eq!(set.manifest().rejected[0].code, "RULE_BASIS_MISSING");
    assert_eq!(set.manifest().rejected[0].rule_id, "R-REQUIRED-001");
}

#[test]
fn 缺依据的规则不拖累其它规则() {
    let dir = tempfile::tempdir().unwrap();
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "国卫办医函〔2021〕28号 附件",
        "R-REQUIRED-001",
    );
    write_table(
        dir.path(),
        "b-2022.1.rules.json",
        "2022.1",
        "", // 缺依据
        "R-UNIT-001",
    );

    // 汇报模式：好规则照常可执行，坏规则被剔除并计入清单
    let set = loader::load_reporting(dir.path()).unwrap();
    assert_eq!(set.executable().len(), 1);
    assert_eq!(set.executable()[0].rule_id, "R-REQUIRED-001");
    assert_eq!(set.manifest().basis_missing, 1);
    assert_eq!(set.manifest().active, 1);
}

#[test]
fn 同槽位同版本两条规则报口径歧义() {
    let dir = tempfile::tempdir().unwrap();
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "依据甲",
        "R-REQUIRED-001",
    );
    write_table(
        dir.path(),
        "b-2021.1.rules.json",
        "2021.1",
        "依据乙",
        "R-REQUIRED-002",
    );

    let err = loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleLoadFailed);
    assert!(
        err.message.contains("歧义"),
        "同槽位同版本应报口径歧义，不二选一：{}",
        err.message
    );
}

#[test]
fn 目录里没有规则表文件时报错() {
    let dir = tempfile::tempdir().unwrap();
    let err = loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleLoadFailed);
    assert!(err.message.contains(".rules.json"), "要说清文件命名要求");
}

#[test]
fn 目录不存在时报错不静默退回内置规则集() {
    let err = loader::load(Path::new("/nonexistent/rule/dir")).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleLoadFailed);
}

#[test]
fn 规则文件解析失败时报错并点名文件() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.rules.json"), "{ 这不是 JSON").unwrap();
    let err = loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleLoadFailed);
    assert!(err.location.unwrap().contains("bad.rules.json"));
}

#[test]
fn 字段名写错不静默忽略() {
    let dir = tempfile::tempdir().unwrap();
    // severity 写成了 severityy —— 口径文件是手工维护的，字段名写错必须报错而不是照旧生效
    let table = json!({
        "rule_set": "2021.1",
        "rules": [{
            "rule_id": "R-REQUIRED-001",
            "rule_kind": "required",
            "sql_lines": ["SELECT 1"],
            "basis": "依据",
            "version": "2021.1",
            "severityy": "blocking"
        }]
    });
    std::fs::write(
        dir.path().join("typo.rules.json"),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();

    let err = loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, ErrorCode::RuleLoadFailed);
    assert!(
        err.message.contains("severityy"),
        "报错要点出错写的字段名：{}",
        err.message
    );
}

#[test]
fn 交付的规则表与内置兜底口径表一致() {
    // 规则目录里的表与内置兜底表是同一套口径 —— 口径不因「从哪装载」而变
    let dir = project_dir().join("rules");
    let set = loader::load(&dir).unwrap();
    let builtin = regval::rules::model::builtin_rules();

    assert_eq!(
        set.executable().len(),
        builtin.len(),
        "规则目录与内置兜底表的规则条数应一致"
    );
    assert!(
        builtin.len() >= 3,
        "内置口径表至少含五列必填、条件必填与表内唯一三条"
    );
    // 逐条比对：口径内容（SQL / 参数 / 依据 / 版本 / 严重度 / 检查类型）必须逐字一致
    for (loaded, expect) in set.executable().iter().zip(builtin.iter()) {
        assert_eq!(loaded.rule_id, expect.rule_id);
        assert_eq!(loaded.rule_kind, expect.rule_kind);
        assert_eq!(loaded.indicator_code, expect.indicator_code);
        assert_eq!(loaded.sql.trim(), expect.sql.trim());
        assert_eq!(loaded.params, expect.params);
        assert_eq!(loaded.basis, expect.basis);
        assert_eq!(loaded.version, expect.version);
        assert_eq!(loaded.severity, expect.severity);
    }
}

#[test]
fn 规则清单可写成可核对产物() {
    let dir = tempfile::tempdir().unwrap();
    write_table(
        dir.path(),
        "a-2021.1.rules.json",
        "2021.1",
        "国卫办医函〔2021〕28号 附件",
        "R-REQUIRED-001",
    );
    write_table(
        dir.path(),
        "b-2022.1.rules.json",
        "2022.1",
        "属地 2022 版口径文件",
        "R-REQUIRED-001",
    );

    let out = dir.path().join("rule-manifest.json");
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["rules"])
        .arg("--dir")
        .arg(dir.path())
        .arg("--out")
        .arg(&out)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "regval rules 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("装载结果"), "应报出装载结果：{}", stdout);
    assert!(
        stdout.contains("已被取代"),
        "应报出旧版本已被取代：{}",
        stdout
    );
    assert!(stdout.contains("2022.1"), "应报出生效版本：{}", stdout);

    // 清单文件可被解析，且含清单汇总与逐条规则
    let raw = std::fs::read_to_string(&out).unwrap();
    let report: Value = serde_json::from_str(&raw).expect("规则清单 JSON 应可解析");
    assert_eq!(report["manifest"]["active"], json!(1));
    assert_eq!(report["manifest"]["superseded"], json!(1));
    assert_eq!(report["manifest"]["set_version"], json!("2022.1"));
    assert_eq!(report["rules"].as_array().unwrap().len(), 2);
}

#[test]
fn 走目录规则集与走内置规则集结论一致() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    std::fs::copy(
        project_dir().join("rules/rules-2021.1.rules.json"),
        rule_dir.join("rules-2021.1.rules.json"),
    )
    .unwrap();

    // 内置兜底口径表
    let out_builtin = work.path().join("builtin.json");
    let (ok1, _s1, e1) = run_validate(
        &fixture("mini.csv"),
        &out_builtin,
        &work.path().join("db1.db"),
        None,
    );
    assert!(ok1, "内置口径表应跑通：{}", e1);

    // 目录里的版本化规则表
    let out_dir = work.path().join("dir.json");
    let (ok2, _s2, e2) = run_validate(
        &fixture("mini.csv"),
        &out_dir,
        &work.path().join("db2.db"),
        Some(&rule_dir),
    );
    assert!(ok2, "目录规则集应跑通：{}", e2);

    let builtin: Value =
        serde_json::from_str(&std::fs::read_to_string(&out_builtin).unwrap()).unwrap();
    let from_dir: Value =
        serde_json::from_str(&std::fs::read_to_string(&out_dir).unwrap()).unwrap();

    // 结论一致：问题清单与行结论逐条相同
    assert_eq!(
        builtin["issues"], from_dir["issues"],
        "同一口径两处装载结论应一致"
    );
    assert_eq!(builtin["rows"], from_dir["rows"], "行结论逐条应一致");

    // 契约里带上规则清单：这批结论按哪套口径算的有据可查
    let status = &from_dir["rule_status"];
    let expected_rules = regval::rules::model::builtin_rules().len();
    assert_eq!(status["manifest"]["set_version"], json!("2021.1"));
    assert_eq!(status["manifest"]["active"], json!(expected_rules));
    assert_eq!(status["manifest"]["basis_missing"], json!(0));
    assert_eq!(status["rules"].as_array().unwrap().len(), expected_rules);
    assert!(!status["rules"][0]["basis"].as_str().unwrap().is_empty());
    // 各条口径各归各的槽位：五列必填 / 条件必填 / 表内唯一 / 四条枚举越界互不顶掉
    let kinds: Vec<&str> = status["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rule_kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"必填缺失"));
    assert!(kinds.contains(&"条件必填缺失"));
    assert!(kinds.contains(&"疑似重复"));
    assert_eq!(
        kinds.iter().filter(|k| **k == "枚举越界").count(),
        4,
        "枚举越界下有四条规则（报告期格式 / 报告期类型 / 计量单位 / 指标编号）"
    );
    // 四条枚举越界各带各的检查项 —— 换一版不会把另外三条顶掉
    let enum_columns: Vec<&str> = status["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["rule_kind"].as_str() == Some("枚举越界"))
        .map(|r| r["check_column"].as_str().unwrap_or(""))
        .collect();
    let mut uniq = enum_columns.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "四条枚举规则的检查项应各不相同");
}
