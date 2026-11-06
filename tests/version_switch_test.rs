//! 口径换版闸门与口径差异标注的验收。
//!
//! 覆盖 Task 19 的三条硬验收：
//! 1. **未确认换版不生效** —— 新版口径未经质控岗确认时不启用，继续用旧版并明确提示；
//! 2. **差异清单列出具体前后取值** —— 换版前能看到哪几条口径变了、前后是什么；
//! 3. **指定旧版本复算结果与原结论一致** —— 换版不让历史批次的结论漂移。
//!
//! 外加两条边界：生效日期未到不启用；换版确认必须写确认人。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 一条必填规则：检查某一列是否为空（列名由 `label_1` 给）
fn required_rule(version: &str, column: &str) -> Value {
    // 列名 → 本机库里的列（口径规则查的是列，不是拼进 SQL 的字面量）
    let db_column = match column {
        "指标名称" => "indicator_name",
        "科室代码" => "dept_code",
        "业务主键" => "business_key",
        other => panic!("测试规则暂不支持列 {}", other),
    };
    json!({
        "rule_id": "R-REQUIRED-001",
        "indicator_code": Value::Null,
        "check_column": Value::Null,
        "rule_kind": "required",
        "sql_lines": [
            "SELECT row_uid AS row_uid, :label_1 AS missing_column",
            "  FROM rows",
            " WHERE batch_ref = :batch_ref",
            "   AND (:scope IS NULL OR indicator_code = :scope)",
            &format!("   AND ({} IS NULL OR trim({}) = '')", db_column, db_column)
        ],
        "params": {"scope": Value::Null, "label_1": column},
        "basis": "国卫办医函〔2021〕28号 附件",
        "version": version,
        "severity": "blocking",
        "effective_from": Value::Null
    })
}

/// 一条单位规则：期望单位表逐指标给（差异清单按指标列出单位变化）
fn unit_rule(version: &str, expected_units: &str) -> Value {
    json!({
        "rule_id": "R-UNIT-001",
        "indicator_code": Value::Null,
        "check_column": "unit",
        "rule_kind": "unit",
        "sql_lines": [
            "SELECT row_uid AS row_uid, '单位不符' AS unit_note",
            "  FROM rows",
            " WHERE batch_ref = :batch_ref",
            "   AND 0 = 1"
        ],
        "params": {"expected_units": expected_units, "unit_aliases": "{}"},
        "basis": "国卫办医函〔2021〕28号 附件",
        "version": version,
        "severity": "blocking",
        "effective_from": Value::Null
    })
}

/// 写一张规则表文件
fn write_table(dir: &Path, file_name: &str, version: &str, rules: Vec<Value>) {
    let table = json!({"rule_set": version, "rules": rules});
    std::fs::write(
        dir.join(file_name),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();
}

/// 跑一次 `regval validate` → (是否成功, stdout, stderr)
fn run_validate(
    input: &Path,
    out: &Path,
    state_db: &Path,
    rule_dir: &Path,
) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .env("REGVAL_RULE_DIR", rule_dir)
        .output()
        .expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 跑一条 regval 子命令 → (退出码, stdout, stderr)
fn run_cmd(args: &[&str], state_db: &Path, rule_dir: &Path) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(args)
        .env("REGVAL_STATE_DB", state_db)
        .env("REGVAL_RULE_DIR", rule_dir)
        .output()
        .expect("regval 应可执行");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 契约 JSON 里的问题条数
fn issue_count(contract: &Value) -> usize {
    contract["issues"].as_array().map(|a| a.len()).unwrap_or(0)
}

#[test]
fn 未确认换版不生效_确认后才换口径() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");
    let out = work.path().join("result.json");

    // 1. 规则目录里只有 2021.1 —— 首次装载，不算换版
    write_table(
        &rule_dir,
        "a-2021.1.rules.json",
        "2021.1",
        vec![required_rule("2021.1", "指标名称")],
    );
    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "首次装载应跑通：{}", stderr);
    assert!(
        stdout.contains("版本 2021.1"),
        "首次装载应用目录里最高版本：{}",
        stdout
    );
    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        issue_count(&contract),
        1,
        "旧版口径命中 1 行（指标名称为空）"
    );

    // 2. 目录里新增 2022.1（同一口径槽位的新版本）—— 换版，未经确认不生效
    write_table(
        &rule_dir,
        "b-2022.1.rules.json",
        "2022.1",
        vec![required_rule("2022.1", "科室代码")],
    );
    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "未确认的换版不应让校验跑挂：{}", stderr);
    assert!(
        stdout.contains("版本 2021.1"),
        "未经确认的换版不生效，继续用旧版：{}",
        stdout
    );
    assert!(
        stdout.contains("换版闸门"),
        "未生效要明确提示，不能静默：{}",
        stdout
    );
    assert!(stdout.contains("2022.1"), "要点名哪一版没生效：{}", stdout);
    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        issue_count(&contract),
        1,
        "未确认换版时结论仍按旧版算（仍是指标名称那一行）"
    );
    let blocked = &contract["rule_status"]["manifest"]["blocked_switches"];
    assert_eq!(
        blocked.as_array().unwrap().len(),
        1,
        "被拦下的换版要逐条进结果契约：{}",
        contract["rule_status"]["manifest"]
    );
    assert_eq!(blocked[0]["code"], "RULE_SWITCH_UNCONFIRMED");
    assert_eq!(blocked[0]["to_version"], "2022.1");
    assert_eq!(
        blocked[0]["from_version"], "2021.1",
        "要说明实际继续用的是哪一版"
    );

    // 3. 质控岗确认后 —— 新版才生效
    let (code, stdout, stderr) = run_cmd(
        &[
            "rules",
            "approve",
            "--version",
            "2022.1",
            "--approver",
            "质控岗张老师",
            "--note",
            "属地 2022 版口径文件",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "确认记录应写成功：{}{}", stdout, stderr);
    assert!(
        stdout.contains("质控岗张老师"),
        "确认人要写进记录：{}",
        stdout
    );

    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "确认后应跑通：{}", stderr);
    assert!(
        stdout.contains("版本 2022.1"),
        "确认后新口径生效：{}",
        stdout
    );
    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        issue_count(&contract),
        0,
        "新口径查的是科室代码，样例里没有空值 → 0 项问题"
    );
    assert!(
        contract["rule_status"]["manifest"]["blocked_switches"]
            .as_array()
            .unwrap()
            .is_empty(),
        "确认后不该再有被拦下的换版"
    );
    assert_eq!(
        contract["rule_status"]["manifest"]["superseded"],
        json!(1),
        "旧版本保留可追，标「已被取代」"
    );
}

#[test]
fn 差异清单列出具体前后取值() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");

    write_table(
        &rule_dir,
        "a-2021.1.rules.json",
        "2021.1",
        vec![
            required_rule("2021.1", "指标名称"),
            unit_rule("2021.1", "{\"MER-TL-04\":\"%\"}"),
        ],
    );
    write_table(
        &rule_dir,
        "b-2022.1.rules.json",
        "2022.1",
        vec![
            required_rule("2022.1", "科室代码"),
            unit_rule("2022.1", "{\"MER-TL-04\":\"例\"}"),
        ],
    );

    let csv_out = work.path().join("diff.csv");
    let (code, stdout, stderr) = run_cmd(
        &[
            "export",
            "diff",
            "--out",
            csv_out.to_str().unwrap(),
            "--from-version",
            "2021.1",
            "--to-version",
            "2022.1",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "差异清单应导出成功：{}{}", stdout, stderr);

    let csv = std::fs::read_to_string(&csv_out).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert!(
        lines[0].starts_with("适用指标,检查类型,检查项,变化,变化项,前值,后值"),
        "列定义固定：{}",
        lines[0]
    );

    // 必填列清单：前后取值具体到列名
    let required_line = lines
        .iter()
        .find(|l| l.contains("必填列清单"))
        .expect("应列出必填列清单的变化");
    assert!(
        required_line.contains("指标名称") && required_line.contains("科室代码"),
        "必填列清单要给出前后取值：{}",
        required_line
    );

    // 期望单位：逐指标列出
    let unit_line = lines
        .iter()
        .find(|l| l.contains("期望单位"))
        .expect("应列出期望单位的变化");
    assert!(
        unit_line.contains("MER-TL-04"),
        "期望单位要落到具体指标：{}",
        unit_line
    );
    assert!(
        unit_line.contains("%,例") || unit_line.contains("%") && unit_line.contains("例"),
        "期望单位要给出前后取值：{}",
        unit_line
    );

    // JSON 产物保留完整结构
    let json_out = work.path().join("diff.json");
    let (code, _stdout, stderr) = run_cmd(
        &[
            "export",
            "diff",
            "--out",
            json_out.to_str().unwrap(),
            "--from-version",
            "2021.1",
            "--to-version",
            "2022.1",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "差异清单 JSON 应导出成功：{}", stderr);
    let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&json_out).unwrap()).unwrap();
    assert_eq!(parsed["from_version"], "2021.1");
    assert_eq!(parsed["to_version"], "2022.1");
    assert!(
        parsed["changes"].as_array().unwrap().len() >= 2,
        "至少列出必填列清单与期望单位两处变化：{}",
        parsed
    );
}

#[test]
fn 指定旧版本复算结果与原结论一致() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");
    let out = work.path().join("result.json");

    // 只用 2021.1 跑一轮
    write_table(
        &rule_dir,
        "a-2021.1.rules.json",
        "2021.1",
        vec![required_rule("2021.1", "指标名称")],
    );
    let (ok, _stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "首轮校验应跑通：{}", stderr);
    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let batch_ref = contract["batch_ref"].as_str().unwrap().to_string();
    let original_issues = issue_count(&contract);
    assert_eq!(original_issues, 1);

    // 按原版本复算 —— 结论必须一致
    let (code, stdout, stderr) = run_cmd(
        &["replay", "--batch", &batch_ref, "--rule-version", "2021.1"],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "复算一致时退出码 0：{}{}", stdout, stderr);
    assert!(
        stdout.contains("一致"),
        "复算结论要说明与原结论一致：{}",
        stdout
    );
    assert!(
        stdout.contains("原结论 1 项"),
        "复算要报出原结论条数：{}",
        stdout
    );

    // 换版之后（新版口径下结论变了）再复算旧版 —— 必须报不一致，不能装一致
    write_table(
        &rule_dir,
        "b-2022.1.rules.json",
        "2022.1",
        vec![required_rule("2022.1", "科室代码")],
    );
    let (code, _stdout, _stderr) = run_cmd(
        &[
            "rules",
            "approve",
            "--version",
            "2022.1",
            "--approver",
            "质控岗张老师",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0);
    let (ok, _stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "换版后校验应跑通：{}", stderr);

    let (code, stdout, _stderr) = run_cmd(
        &["replay", "--batch", &batch_ref, "--rule-version", "2021.1"],
        &state_db,
        &rule_dir,
    );
    assert_eq!(
        code, 1,
        "结论确实变了时要报不一致（退出码 1），不能装一致：{}",
        stdout
    );
    assert!(stdout.contains("不一致"), "要说明结论不一致：{}", stdout);
}

#[test]
fn 生效日期未到则新版本不启用() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");
    let out = work.path().join("result.json");

    write_table(
        &rule_dir,
        "a-2021.1.rules.json",
        "2021.1",
        vec![required_rule("2021.1", "指标名称")],
    );
    let (ok, _stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "首轮应跑通：{}", stderr);

    // 新版本带了一个还没到的生效日期
    let mut rule = required_rule("2022.1", "科室代码");
    rule["effective_from"] = json!("2099-01-01");
    write_table(&rule_dir, "b-2022.1.rules.json", "2022.1", vec![rule]);

    // 就算确认了，生效日期没到也不启用
    let (code, _stdout, stderr) = run_cmd(
        &[
            "rules",
            "approve",
            "--version",
            "2022.1",
            "--approver",
            "质控岗张老师",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "确认记录应写成功：{}", stderr);

    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, &rule_dir);
    assert!(ok, "生效日期未到不该让校验跑挂：{}", stderr);
    assert!(
        stdout.contains("版本 2021.1"),
        "生效日期未到，新版本不启用：{}",
        stdout
    );
    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let blocked = &contract["rule_status"]["manifest"]["blocked_switches"];
    assert_eq!(blocked.as_array().unwrap().len(), 1);
    assert_eq!(blocked[0]["code"], "RULE_EFFECTIVE_DATE_NOT_REACHED");
}

#[test]
fn 换版确认必须写确认人且记录含确认人与时间() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");

    write_table(
        &rule_dir,
        "a-2022.1.rules.json",
        "2022.1",
        vec![required_rule("2022.1", "指标名称")],
    );

    // 不给确认人 → 报错，不写记录
    let (code, _stdout, stderr) = run_cmd(
        &["rules", "approve", "--version", "2022.1"],
        &state_db,
        &rule_dir,
    );
    assert_ne!(code, 0, "没有确认人不能确认换版：{}", stderr);

    // 给了确认人 → 写进记录，含确认人与确认时间
    let (code, stdout, stderr) = run_cmd(
        &[
            "rules",
            "approve",
            "--version",
            "2022.1",
            "--approver",
            "质控岗李老师",
        ],
        &state_db,
        &rule_dir,
    );
    assert_eq!(code, 0, "确认应成功：{}{}", stdout, stderr);

    let (code, stdout, _stderr) = run_cmd(&["rules", "approvals"], &state_db, &rule_dir);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("质控岗李老师"),
        "确认记录里要有确认人：{}",
        stdout
    );
    assert!(
        stdout.contains("2022.1"),
        "确认记录里要有版本号：{}",
        stdout
    );
    // 确认时间（YYYY-MM-DD HH:mm:ss）
    assert!(
        stdout.contains("202") && stdout.contains(':'),
        "确认记录里要有确认时间：{}",
        stdout
    );
}

#[test]
fn 确认一个不存在的版本会被拒绝() {
    let work = tempfile::tempdir().unwrap();
    let rule_dir = work.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();
    let state_db = work.path().join("state.db");

    write_table(
        &rule_dir,
        "a-2022.1.rules.json",
        "2022.1",
        vec![required_rule("2022.1", "指标名称")],
    );

    let (code, _stdout, stderr) = run_cmd(
        &[
            "rules",
            "approve",
            "--version",
            "2099.9",
            "--approver",
            "质控岗李老师",
        ],
        &state_db,
        &rule_dir,
    );
    assert_ne!(code, 0, "确认不存在的版本应被拒绝：{}", stderr);
    assert!(
        stderr.contains("RULE_VERSION_NOT_FOUND"),
        "应给出明确错误码：{}",
        stderr
    );
}
