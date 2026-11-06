//! 参数化规则执行器（Task 9）。
//!
//! 覆盖三条硬验收：
//! 1. **规则以绑定参数执行** —— 代码里没有把取值拼进 SQL 的路径，
//!    带 SQL 注入片段的取值只当字面量比对，表不会被拼坏；
//! 2. **缺列规则不生效并报错** —— 点名缺哪一列，其余规则照常执行；
//! 3. **执行期错误不拖垮整轮** —— 失败规则计数与明细在结果契约里可见；
//!    同一行同一规则只落一条问题。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;

use regval::db::repo::Repo;
use regval::rules::executor::{self, RuleOutcome};
use regval::rules::model::{Issue, Rule, RuleKind, RuleParam, RuleStatus, Severity};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 造一条测试规则
fn rule(rule_id: &str, kind: RuleKind, sql: &str, params: Vec<(&str, RuleParam)>) -> Rule {
    Rule {
        rule_id: rule_id.to_string(),
        indicator_code: None,
        check_column: None,
        rule_kind: kind,
        sql: sql.to_string(),
        params: params
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
        basis: "国卫办医函〔2021〕28号 附件（测试用依据）".to_string(),
        version: "2021.1".to_string(),
        severity: Severity::Blocking,
        effective_from: None,
        status: RuleStatus::Active,
        source_file: "（测试）".to_string(),
    }
}

/// 建一个带样例行的本机库（内存库），行内容固定
fn seeded_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    regval::db::schema::migrate(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                           authority_state, indicator_code, indicator_name, dept_code, dept_name,
                           reporting_period, business_key, numerator, denominator, value, unit,
                           remark, numeric_raw, rule_version, created_at)
         VALUES ('B1','ns1','a.csv',1,'R1','ORG','raw','MER-HR-01','指标一','0301','科室一',
                 '2026-Q3','BK-1',50.0,100.0,50.0,'%','','',NULL,'2026-11-06 00:00:00');
         INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                           authority_state, indicator_code, indicator_name, dept_code, dept_name,
                           reporting_period, business_key, numerator, denominator, value, unit,
                           remark, numeric_raw, rule_version, created_at)
         VALUES ('B1','ns1','a.csv',2,'R2','ORG','raw','MER-HR-01','指标一','0301','科室一',
                 '2026-Q3','BK-2',10.0,100.0,10.0,'%','','',NULL,'2026-11-06 00:00:00');",
    )
    .unwrap();
    conn
}

// ————————————————————— 验收 1：参数化执行，无拼接路径 —————————————————————

/// 源码层面：执行器里没有把取值拼进 SQL 的路径。
///
/// 判据两条：
/// - 规则 SQL 文本只在「准备语句」这一处被使用（出现一次），不被二次加工；
/// - 执行器源码里没有 `format!` 拼 SQL、没有 `push_str` 追加。
#[test]
fn 执行器源码里没有把取值拼进_sql_的路径() {
    let src = include_str!("../src/rules/executor.rs");

    // 规则 SQL 文本只出现一次：conn.prepare(&rule.sql)
    let sql_uses = src.matches("rule.sql").count();
    assert_eq!(
        sql_uses, 1,
        "规则 SQL 文本只应在准备语句处使用一次，实际出现 {} 次",
        sql_uses
    );
    assert!(
        src.contains("conn.prepare(&rule.sql)"),
        "规则 SQL 应原样交给 prepare，不做加工"
    );

    // 没有拼接 SQL 的写法
    for forbidden in ["push_str", "write!", "format!(\"SELECT", "format!(\"select"] {
        assert!(
            !src.contains(forbidden),
            "执行器里出现了拼接 SQL 的写法：{}",
            forbidden
        );
    }
}

/// 行为层面：取值是绑定参数，不是拼进 SQL 的字面量。
///
/// 带注入片段的取值只当字面量比对 —— 表不会被拼坏，行也不会被误命中。
#[test]
fn 取值按绑定参数比对_注入片段只当字面量() {
    let conn = seeded_db();
    let injection = "MER-HR-01'; DROP TABLE rows; --";

    // 规则拿 :scope 去比 indicator_code，取值就是那段注入文本
    let r = rule(
        "R-INJECT-001",
        RuleKind::EnumBound,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND indicator_code = :scope",
        vec![
            ("scope", RuleParam::Text(injection.to_string())),
            ("label_1", RuleParam::Text("指标编号".to_string())),
        ],
    );

    let exec = executor::execute_rule(&conn, &r, "B1");
    assert_eq!(
        exec.outcome,
        RuleOutcome::Executed,
        "规则本身应正常执行，message={}",
        exec.message
    );
    assert!(
        exec.issues.is_empty(),
        "注入文本不应命中任何行，实际命中 {:?}",
        exec.issues.iter().map(|i| &i.row_uid).collect::<Vec<_>>()
    );

    // 表还在 —— 说明取值没有被拼进 SQL 执行
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM rows WHERE batch_ref = 'B1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "rows 表被拼坏了");
}

/// 取值确实生效：换一个真值就命中（证明绑定参数不是摆设）
#[test]
fn 绑定参数的取值确实生效() {
    let conn = seeded_db();
    let r = rule(
        "R-SCOPE-001",
        RuleKind::EnumBound,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND indicator_code = :scope",
        vec![
            ("scope", RuleParam::Text("MER-HR-01".to_string())),
            ("label_1", RuleParam::Text("指标编号".to_string())),
        ],
    );
    let exec = executor::execute_rule(&conn, &r, "B1");
    assert_eq!(exec.outcome, RuleOutcome::Executed);
    assert_eq!(exec.issues.len(), 2, "两行都该命中");
}

// ————————————————————— 验收 2：缺列规则不生效并报错 —————————————————————

#[test]
fn 缺列规则不生效并点名缺哪一列_其余规则照常执行() {
    let conn = seeded_db();

    let bad = rule(
        "R-NOCOL-001",
        RuleKind::RequiredField,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND (no_such_column IS NULL)",
        vec![("label_1", RuleParam::Text("业务主键".to_string()))],
    );
    let good = rule(
        "R-GOOD-001",
        RuleKind::RequiredField,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND business_key IS NULL",
        vec![("label_1", RuleParam::Text("业务主键".to_string()))],
    );

    let report = executor::execute_rules(&conn, &[&bad, &good], "B1");

    // 坏规则：不生效，报 MISSING_COLUMN，点名缺哪一列
    let bad_exec = &report.executions[0];
    assert_eq!(bad_exec.outcome, RuleOutcome::SkippedMissingColumn);
    assert_eq!(
        bad_exec.error_code,
        Some(regval::errors::ErrorCode::MissingColumn),
        "缺列要报 MISSING_COLUMN"
    );
    assert!(
        bad_exec.message.contains("no_such_column"),
        "报错要点名缺哪一列，实际：{}",
        bad_exec.message
    );
    assert!(
        bad_exec.message.contains("不生效"),
        "报错要说明该规则不生效：{}",
        bad_exec.message
    );
    assert!(bad_exec.issues.is_empty(), "不生效的规则不产出问题");

    // 好规则：照常执行
    let good_exec = &report.executions[1];
    assert_eq!(good_exec.outcome, RuleOutcome::Executed);

    // 计数可见
    assert_eq!(report.skipped_rules(), 1);
    assert_eq!(report.failed_rules(), 0);
    assert_eq!(report.executed_rules(), 1);
    assert_eq!(report.not_run_rules(), 1);

    // 明细逐条列出
    let entries = report.failure_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].rule_id, "R-NOCOL-001");
    assert_eq!(entries[0].outcome, "不生效");
    assert_eq!(entries[0].code, "MISSING_COLUMN");
    assert_eq!(entries[0].location, "no_such_column");
}

#[test]
fn 规则引用未提供的命名参数时报错而不静默跳过() {
    let conn = seeded_db();
    let r = rule(
        "R-NOPARAM-001",
        RuleKind::EnumBound,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND dept_code = :not_given",
        vec![("label_1", RuleParam::Text("科室代码".to_string()))],
    );
    let exec = executor::execute_rule(&conn, &r, "B1");
    assert_eq!(exec.outcome, RuleOutcome::Failed);
    assert_eq!(exec.error_code, Some(regval::errors::ErrorCode::RuleExec));
    assert!(
        exec.message.contains("not_given"),
        "报错要点名缺哪个参数：{}",
        exec.message
    );
}

// ————————————————————— 验收 3：执行期错误不拖垮整轮 + 问题不重复 —————————————————————

#[test]
fn 执行期失败_该规则记失败_其余规则继续() {
    let conn = seeded_db();

    // 运行期才炸的 SQL：abs(-9223372036854775808) 触发整数溢出
    let broken = rule(
        "R-RUNTIME-001",
        RuleKind::FormulaRecalc,
        "SELECT row_uid AS row_uid, abs(-9223372036854775808) AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref",
        vec![],
    );
    let good = rule(
        "R-GOOD-002",
        RuleKind::RequiredField,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows \
          WHERE batch_ref = :batch_ref AND business_key IS NULL",
        vec![("label_1", RuleParam::Text("业务主键".to_string()))],
    );

    let report = executor::execute_rules(&conn, &[&broken, &good], "B1");

    let broken_exec = &report.executions[0];
    assert_eq!(
        broken_exec.outcome,
        RuleOutcome::Failed,
        "运行期错误应记执行失败，message={}",
        broken_exec.message
    );
    assert_eq!(
        broken_exec.error_code,
        Some(regval::errors::ErrorCode::RuleExec)
    );

    // 其余规则继续
    assert_eq!(report.executions[1].outcome, RuleOutcome::Executed);
    assert_eq!(report.failed_rules(), 1);
    assert_eq!(report.executed_rules(), 1);

    // 失败明细进报告
    let entries = report.failure_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].rule_id, "R-RUNTIME-001");
    assert_eq!(entries[0].outcome, "执行失败");
    assert_eq!(entries[0].code, "RULE_EXEC_ERROR");
}

#[test]
fn 同一行同一规则只落一条问题_缺失列合并成清单() {
    let conn = seeded_db();

    // 一行同时缺三列 —— 规则 SQL 会返回三行（同一行键、三个区分键）
    conn.execute(
        "UPDATE rows SET indicator_name = '', dept_code = '', reporting_period = '' WHERE row_uid = 'R1'",
        [],
    )
    .unwrap();

    let r = rule(
        "R-MULTI-001",
        RuleKind::RequiredField,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows WHERE batch_ref = :batch_ref AND indicator_name = ''
         UNION ALL SELECT row_uid, :label_2 FROM rows WHERE batch_ref = :batch_ref AND dept_code = ''
         UNION ALL SELECT row_uid, :label_3 FROM rows WHERE batch_ref = :batch_ref AND reporting_period = ''",
        vec![
            ("label_1", RuleParam::Text("指标名称".to_string())),
            ("label_2", RuleParam::Text("科室代码".to_string())),
            ("label_3", RuleParam::Text("报告期".to_string())),
        ],
    );

    let exec = executor::execute_rule(&conn, &r, "B1");
    assert_eq!(exec.outcome, RuleOutcome::Executed);
    assert_eq!(
        exec.issues.len(),
        1,
        "同一行同一规则只应落一条问题，实际 {} 条",
        exec.issues.len()
    );

    let issue = &exec.issues[0];
    assert_eq!(issue.row_uid, "R1");
    assert_eq!(issue.rule_id, "R-MULTI-001");
    assert_eq!(issue.issue_kind, "必填缺失");
    assert_eq!(issue.severity, "阻断");
    assert!(!issue.basis.is_empty(), "问题必须带依据");
    // 缺失列合并成清单（可多列）
    assert_eq!(issue.missing_columns.len(), 3, "缺哪几列要给全");
    assert!(issue.missing_columns.contains(&"指标名称".to_string()));
    assert!(issue.missing_columns.contains(&"科室代码".to_string()));
    assert!(issue.missing_columns.contains(&"报告期".to_string()));
    assert!(
        issue.message.contains("指标名称")
            && issue.message.contains("科室代码")
            && issue.message.contains("报告期"),
        "问题说明要给全缺哪几列：{}",
        issue.message
    );
}

#[test]
fn 问题落库后同一行同一规则仍只有一条() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let mut repo = Repo::open(&db_path).unwrap();

    let conn = seeded_db();
    // 让 R1 真的缺两列，规则才命得到
    conn.execute(
        "UPDATE rows SET indicator_name = '', dept_code = '' WHERE row_uid = 'R1'",
        [],
    )
    .unwrap();

    let r = rule(
        "R-MULTI-002",
        RuleKind::RequiredField,
        "SELECT row_uid AS row_uid, :label_1 AS issue_key FROM rows WHERE batch_ref = :batch_ref AND indicator_name = ''
         UNION ALL SELECT row_uid, :label_2 FROM rows WHERE batch_ref = :batch_ref AND dept_code = ''",
        vec![
            ("label_1", RuleParam::Text("指标名称".to_string())),
            ("label_2", RuleParam::Text("科室代码".to_string())),
        ],
    );
    let exec = executor::execute_rule(&conn, &r, "B1");
    assert_eq!(exec.issues.len(), 1);

    // 落库（字段齐全：行键 / 规则编号 / 问题类型 / 严重度 / 说明 / 依据）
    let issues: Vec<Issue> = exec.issues.clone();
    let n = repo.insert_issues("B1", &issues).unwrap();
    assert_eq!(n, 1);

    let (row_uid, rule_id, kind, severity, basis): (String, String, String, String, String) = repo
        .conn()
        .query_row(
            "SELECT row_uid, rule_id, issue_kind, severity, basis FROM issues WHERE batch_ref='B1'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row_uid, "R1");
    assert_eq!(rule_id, "R-MULTI-002");
    assert_eq!(kind, "必填缺失");
    assert_eq!(severity, "阻断");
    assert!(!basis.is_empty());

    // 同一批重复落库不会累积（唯一索引兜底）
    let again = repo.insert_issues("B1", &issues).unwrap();
    assert_eq!(again, 1, "重复落库应被唯一索引拦下，返回受影响行数 1");
    let count: i64 = repo
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM issues WHERE batch_ref='B1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "同一行同一规则只应有一条问题");
}

// ————————————————————— 端到端：失败规则计数写进结果契约 —————————————————————

#[test]
fn 端到端_没跑起来的规则在结果契约里可见() {
    let dir = tempfile::tempdir().unwrap();
    let rule_dir = dir.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();

    let table = serde_json::json!({
        "rule_set": "2021.1",
        "rules": [
            {
                "rule_id": "R-NOCOL-E2E",
                "indicator_code": Value::Null,
                "rule_kind": "required",
                "sql_lines": [
                    "SELECT row_uid AS row_uid, :label_1 AS missing_column",
                    "  FROM rows",
                    " WHERE batch_ref = :batch_ref",
                    "   AND (no_such_column IS NULL)"
                ],
                "params": {"label_1": "业务主键"},
                "basis": "国卫办医函〔2021〕28号 附件（测试用依据）",
                "version": "2021.1",
                "severity": "blocking",
                "effective_from": Value::Null
            }
        ]
    });
    std::fs::write(
        rule_dir.join("rules-nocol.rules.json"),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();

    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("result.json");
    let state_db = work.path().join("state.db");

    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .env("REGVAL_RULE_DIR", &rule_dir)
        .output()
        .expect("regval 应可执行");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "一条规则写错不该让整轮校验失败，stdout={} stderr={}",
        stdout,
        stderr
    );
    // 操作的人看得见：stdout 点名说清楚
    assert!(
        stdout.contains("未生效"),
        "stdout 要说明有规则未生效：{}",
        stdout
    );
    assert!(
        stdout.contains("R-NOCOL-E2E"),
        "stdout 要点名是哪条规则：{}",
        stdout
    );

    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let manifest = &contract["rule_status"]["manifest"];
    assert_eq!(
        manifest["execution_skipped_count"], 1,
        "缺列不生效的规则数可见"
    );
    assert_eq!(manifest["execution_failed_count"], 0);
    let failures = manifest["execution_failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["rule_id"], "R-NOCOL-E2E");
    assert_eq!(failures[0]["outcome"], "不生效");
    assert_eq!(failures[0]["code"], "MISSING_COLUMN");
    assert_eq!(failures[0]["location"], "no_such_column");

    // 结构化日志里也有
    assert!(
        stderr.contains("validate.rule_not_executed"),
        "结构化日志要留痕：{}",
        stderr
    );
}

/// 规则目录里没有规则文件时的老路径不受影响（回归）
#[test]
fn 内置规则集照常跑通_问题数不变() {
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("result.json");
    let state_db = work.path().join("state.db");

    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .expect("regval 应可执行");
    assert!(output.status.success());

    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(contract["issues"].as_array().unwrap().len(), 2);
    assert_eq!(
        contract["rule_status"]["manifest"]["execution_failed_count"],
        0
    );
    assert_eq!(
        contract["rule_status"]["manifest"]["execution_skipped_count"],
        0
    );
}

/// 兼容：结果契约里仍然能读出原来的字段（新增字段不破坏旧读法）
#[test]
fn 结果契约新增字段不影响原有结构() {
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("result.json");
    let state_db = work.path().join("state.db");
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .unwrap();
    assert!(output.status.success());

    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    for key in [
        "schema_version",
        "columns",
        "rows",
        "issues",
        "summary",
        "rule_status",
    ] {
        assert!(contract.get(key).is_some(), "契约缺字段 {}", key);
    }
    assert_eq!(contract["columns"].as_array().unwrap().len(), 5);
}

/// 规则目录里的规则缺依据时仍在装载期被拒（执行器不越权放行）
#[test]
fn 缺依据的规则仍在装载期被拒() {
    let dir = tempfile::tempdir().unwrap();
    let table = serde_json::json!({
        "rule_set": "2021.1",
        "rules": [
            {
                "rule_id": "R-NOBASIS-001",
                "indicator_code": Value::Null,
                "rule_kind": "required",
                "sql_lines": ["SELECT row_uid AS row_uid, :label_1 AS missing_column FROM rows"],
                "params": {"label_1": "业务主键"},
                "basis": "",
                "version": "2021.1",
                "severity": "blocking",
                "effective_from": Value::Null
            }
        ]
    });
    std::fs::write(
        dir.path().join("rules-nobasis.rules.json"),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();

    let err = regval::rules::loader::load(dir.path()).unwrap_err();
    assert_eq!(err.code, regval::errors::ErrorCode::RuleBasisMissing);
}

/// 引擎不出网关时也不该有真实出站调用（本任务不引入任何出站依赖）
#[test]
fn 执行器不引入出站调用() {
    let src = include_str!("../src/rules/executor.rs");
    for forbidden in ["reqwest", "http://", "https://"] {
        assert!(
            !src.contains(forbidden),
            "执行器里出现了出站调用相关内容：{}",
            forbidden
        );
    }
    let _ = Path::new(".");
}
