//! 口径规则 SQL 的语法预校验与出错定位（Task 8）。
//!
//! 覆盖三条硬验收：
//! 1. **故意写错的规则载入后报出出错 token 与偏移**，该规则状态为「不生效」；
//! 2. **其余规则照常执行** —— 语法错不中止整轮，好规则仍产出问题清单；
//! 3. **三级降级链各自产出的出错定位字段结构一致**。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use regval::rules::loader;
use regval::rules::model::RuleStatus;
use regval::rules::syntax::{self, SyntaxDetector};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 故意写错的规则 SQL：关键字拼错（SELEC）+ 左括号没闭合
const BROKEN_SQL: &[&str] = &[
    "SELEC row_uid AS row_uid, :label_1 AS missing_column",
    "  FROM rows",
    " WHERE batch_ref = :batch_ref",
    "   AND (indicator_code = :scope",
];

/// 好规则：查「业务主键」缺失（mini.csv 里有一行缺业务主键，能跑出真实问题）
const GOOD_SQL: &[&str] = &[
    "SELECT row_uid AS row_uid, :label_1 AS missing_column",
    "  FROM rows",
    " WHERE batch_ref = :batch_ref",
    "   AND (business_key IS NULL OR trim(business_key) = '')",
];

fn rule_def(
    rule_id: &str,
    kind: &str,
    sql_lines: &[&str],
    indicator_code: Option<&str>,
    params: Value,
) -> Value {
    json!({
        "rule_id": rule_id,
        "indicator_code": indicator_code,
        "rule_kind": kind,
        "sql_lines": sql_lines,
        "params": params,
        "basis": "国卫办医函〔2021〕28号 附件（测试用依据）",
        "version": "2021.1",
        "severity": "blocking",
        "effective_from": Value::Null
    })
}

/// 写一张规则表：一条故意写错的规则 + 一条好规则
fn write_rule_dir(dir: &Path) {
    let table = json!({
        "rule_set": "2021.1",
        "rules": [
            rule_def(
                "R-ENUM-BROKEN",
                "enum",
                BROKEN_SQL,
                Some("MER-HR-01"),
                json!({"scope": Value::Null, "label_1": "业务主键"}),
            ),
            rule_def(
                "R-REQUIRED-GOOD",
                "required",
                GOOD_SQL,
                None,
                json!({"label_1": "业务主键"}),
            )
        ]
    });
    std::fs::write(
        dir.join("rules-broken.rules.json"),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();
}

/// 跑一次 `regval validate`，返回 (是否成功, stdout, stderr)
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

// ————————————————————— 验收 1：报出错 token 与偏移，该规则不生效 —————————————————————

#[test]
fn 语法错的规则报出出错_token_与偏移且状态为不生效() {
    let dir = tempfile::tempdir().unwrap();
    write_rule_dir(dir.path());

    // 严格模式下装载**不报错** —— 语法错不中止整轮
    let set = loader::load(dir.path()).expect("语法错的规则不应中止整轮装载");

    let broken = set
        .all()
        .iter()
        .find(|r| r.rule_id == "R-ENUM-BROKEN")
        .expect("坏规则应仍在规则集里可查");
    assert_eq!(
        broken.status,
        RuleStatus::Invalid,
        "语法错的规则状态应为「不生效」"
    );

    let good = set
        .all()
        .iter()
        .find(|r| r.rule_id == "R-REQUIRED-GOOD")
        .expect("好规则应在规则集里");
    assert_eq!(good.status, RuleStatus::Active, "好规则应照常生效");

    // 出错位置逐条列出（不静默通过）
    let manifest = set.manifest();
    assert_eq!(manifest.syntax_error_count, 1);
    let se = &manifest.syntax_errors[0];
    assert_eq!(se.rule_id, "R-ENUM-BROKEN");
    assert_eq!(se.code, "RULE_SYNTAX_ERROR");
    assert!(!se.token_type.is_empty(), "出错 token 类型不能为空");
    assert_eq!(se.found, "SELEC", "应指出出错 token 原文");
    assert_eq!(se.offset, 0, "出错 token 在规则 SQL 开头");
    // 报错要能直接读懂：点名规则 id 与规则中文名
    assert!(se.message.contains("R-ENUM-BROKEN"), "报错要点名规则 id");
    assert!(
        se.message.contains("枚举越界"),
        "报错要点名规则中文名，实际：{}",
        se.message
    );
    assert!(
        se.message.contains("不生效"),
        "报错要说明该规则不生效：{}",
        se.message
    );
    assert!(
        se.rule_name.contains("MER-HR-01"),
        "规则中文名应含适用指标：{}",
        se.rule_name
    );

    // 状态计数里能看到「不生效」
    assert_eq!(manifest.status_count("不生效"), 1);
}

#[test]
fn 语法错的规则不参与执行但好规则照常执行() {
    let dir = tempfile::tempdir().unwrap();
    write_rule_dir(dir.path());
    let set = loader::load(dir.path()).unwrap();

    let executable: Vec<&str> = set
        .executable()
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert_eq!(executable, vec!["R-REQUIRED-GOOD"], "只有好规则参与执行");

    // 规则清单段（结果契约里同一份）能看到坏规则的状态
    let section = set.status_section();
    let broken_entry = section
        .rules
        .iter()
        .find(|r| r.rule_id == "R-ENUM-BROKEN")
        .unwrap();
    assert_eq!(broken_entry.status, "不生效");
    assert_eq!(section.manifest.syntax_error_count, 1);
}

// ————————————————————— 验收 2：端到端 —— 校验照常跑完，问题清单照常有 —————————————————————

#[test]
fn 端到端校验照常跑完且问题清单来自好规则() {
    let dir = tempfile::tempdir().unwrap();
    write_rule_dir(dir.path());
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("result.json");
    let state_db = work.path().join("state.db");

    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &out, &state_db, dir.path());
    assert!(ok, "校验不应因一条规则写错而失败，stderr={}", stderr);
    assert!(stdout.contains("已校验 6 行"), "stdout={}", stdout);

    let contract: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    // 好规则照常执行：mini.csv 有一行缺业务主键 → 一条口径问题
    let issues = contract["issues"].as_array().unwrap();
    assert_eq!(
        issues.len(),
        1,
        "只有好规则命中，issues={}",
        contract["issues"]
    );
    assert_eq!(issues[0]["rule_id"], "R-REQUIRED-GOOD");
    assert_eq!(issues[0]["issue_kind"], "必填缺失");

    // 坏规则在结果契约的 rule_status 段里可见（不静默通过）
    let manifest = &contract["rule_status"]["manifest"];
    assert_eq!(manifest["syntax_error_count"], 1);
    let se = &manifest["syntax_errors"][0];
    assert_eq!(se["rule_id"], "R-ENUM-BROKEN");
    assert_eq!(se["found"], "SELEC");
    assert_eq!(se["offset"], 0);
    assert!(se["message"].as_str().unwrap().contains("SELEC"));

    let rules = contract["rule_status"]["rules"].as_array().unwrap();
    let broken = rules
        .iter()
        .find(|r| r["rule_id"] == "R-ENUM-BROKEN")
        .unwrap();
    assert_eq!(broken["status"], "不生效");
}

#[test]
fn 缺列缺表不算语法错_规则仍生效() {
    // 「列不存在」是语义问题：由执行器报 MISSING_COLUMN，不该在装载期被当成语法错
    let dir = tempfile::tempdir().unwrap();
    let table = json!({
        "rule_set": "2021.1",
        "rules": [rule_def(
            "R-REQUIRED-NOCOL",
            "required",
            &["SELECT row_uid AS row_uid, :label_1 AS missing_column",
              "  FROM rows",
              " WHERE batch_ref = :batch_ref",
              "   AND (no_such_column IS NULL)"],
            None,
            json!({"label_1": "业务主键"}),
        )]
    });
    std::fs::write(
        dir.path().join("rules-nocol.rules.json"),
        serde_json::to_string_pretty(&table).unwrap(),
    )
    .unwrap();

    let set = loader::load(dir.path()).expect("缺列不该在装载期报错");
    assert_eq!(set.manifest().syntax_error_count, 0);
    assert_eq!(set.all()[0].status, RuleStatus::Active);
}

// ————————————————————— 验收 3：三级降级链字段结构一致 —————————————————————

#[test]
fn 三级降级链产出的出错定位字段结构一致() {
    let broken = "SELEC row_uid FROM rows WHERE (indicator_code = :scope";
    let mut reports = Vec::new();
    for detector in [
        SyntaxDetector::Sqlite3Parser,
        SyntaxDetector::RusqlitePrepare,
        SyntaxDetector::BalanceCheck,
    ] {
        let report = syntax::check_sql_with(broken, detector)
            .unwrap_or_else(|| panic!("{} 级别没能报出坏规则", detector));
        assert_eq!(report.detector, detector);
        // 字段结构一致：三个字段都填上了，且取值有意义
        assert!(
            !report.error.token_type.is_empty(),
            "{} 级别的出错 token 类型为空",
            detector
        );
        assert!(
            report.error.offset <= broken.len(),
            "{} 级别的偏移越界：{}",
            detector,
            report.error.offset
        );
        assert!(
            !report.detail.is_empty(),
            "{} 级别应带检测器原始说明",
            detector
        );
        reports.push(report);
    }

    // 一级与二级都是 SQLite 解析器 —— 定位必须完全一致
    assert_eq!(
        reports[0].error, reports[1].error,
        "两个 SQLite 解析器应给出一致的出错定位"
    );
    assert_eq!(reports[0].error.found, "SELEC");
    assert_eq!(reports[0].error.offset, 0);

    // 三级是兜底：定位到没闭合的左括号
    assert_eq!(reports[2].error.token_type, "括号未闭合");
    assert_eq!(reports[2].error.found, "(");
    assert_eq!(reports[2].error.offset, 30);
}

#[test]
fn 好规则三级都放过且降级链整体通过() {
    let good = "SELECT row_uid AS row_uid, :label_1 AS missing_column FROM rows \
                WHERE batch_ref = :batch_ref AND business_key = :scope";
    for detector in [
        SyntaxDetector::Sqlite3Parser,
        SyntaxDetector::RusqlitePrepare,
        SyntaxDetector::BalanceCheck,
    ] {
        assert!(
            syntax::check_sql_with(good, detector).is_none(),
            "{} 级别误报了好规则",
            detector
        );
    }
    assert!(syntax::check_sql(good).is_none(), "降级链整体误报了好规则");
}
