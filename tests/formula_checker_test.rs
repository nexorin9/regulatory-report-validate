//! 分子分母公式复算检查器（Task 13）。
//!
//! 覆盖任务验收与口径分工：
//! 1. **复算一致通过** —— 填报值等于「分子 ÷ 分母 × 100%」按指标精度四舍五入的结果，不误报；
//! 2. **超容差报「公式不符」并给出实际值与期望值** —— 科室照着能改；
//! 3. **分母为 0 单列**（错误码 `DENOMINATOR_ZERO`），不静默 0 处理、不报成「公式不符」；
//! 4. **负值与非数值归数据形态错误**（错误码 `INVALID_NUMERIC`），与公式不符分开；
//! 5. **容差按指标给** —— 精度来自逐指标精度表，不是一个全局常数；
//! 6. **人均负担型不判** —— 官方公式与本产品最小数据格式对不上，不套用比率型口径；
//! 7. **端到端** —— 契约里能看到两类问题与各自错误码。

use std::path::PathBuf;
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;

use regval::rules::checks::formula::{
    DENOMINATOR_ZERO_CODE, DENOMINATOR_ZERO_RULE_ID, FORMULA_BASIS, INVALID_NUMERIC_CODE,
    NUMERIC_SHAPE_RULE_ID, RECALC_RULE_ID,
};
use regval::rules::executor;
use regval::rules::indicators;
use regval::rules::model::{Issue, Rule};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 一行样例（数值列的「解析值」与「原文」分开给，才能造出非数值这类形态）
#[derive(Debug, Clone)]
struct RowSpec {
    uid: String,
    biz_key: String,
    indicator: String,
    name: String,
    unit: String,
    numerator: Option<f64>,
    denominator: Option<f64>,
    value: Option<f64>,
    raw_numerator: String,
    raw_denominator: String,
    raw_value: String,
}

impl RowSpec {
    /// 默认行：比率型指标 MER-TL-04，980 ÷ 1000 = 98%，取值齐全
    fn new(uid: &str, biz_key: &str) -> Self {
        RowSpec {
            uid: uid.to_string(),
            biz_key: biz_key.to_string(),
            indicator: "MER-TL-04".to_string(),
            name: "病案首页24小时内完成率".to_string(),
            unit: "%".to_string(),
            numerator: Some(980.0),
            denominator: Some(1000.0),
            value: Some(98.0),
            raw_numerator: "980".to_string(),
            raw_denominator: "1000".to_string(),
            raw_value: "98".to_string(),
        }
    }
}

/// 建一个带样例行的本机库（内存库）
fn seed(rows: &[RowSpec]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    regval::db::schema::migrate(&conn).unwrap();
    for (i, r) in rows.iter().enumerate() {
        let numeric_raw = serde_json::json!({
            "numerator": r.raw_numerator,
            "denominator": r.raw_denominator,
            "value": r.raw_value,
        })
        .to_string();
        conn.execute(
            "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                               authority_state, indicator_code, indicator_name, dept_code, dept_name,
                               reporting_period, period_type, business_key, numerator, denominator,
                               value, unit, remark, numeric_raw, rule_version, created_at)
             VALUES ('B1', ?1, 'a.csv', ?2, ?3, 'ORG', 'raw', ?4, ?5, '0301', '病案统计室',
                     '2026-Q3', '季报', ?6, ?7, ?8, ?9, ?10, '', ?11, NULL, '2026-11-06 00:00:00')",
            rusqlite::params![
                "表一",
                (i + 1) as i64,
                r.uid,
                r.indicator,
                r.name,
                r.biz_key,
                r.numerator,
                r.denominator,
                r.value,
                r.unit,
                numeric_raw,
            ],
        )
        .unwrap();
    }
    conn
}

/// 跑内置口径表，返回本轮全部问题
fn run(conn: &Connection) -> Vec<Issue> {
    let rules: Vec<Rule> = regval::rules::model::builtin_rules();
    let refs: Vec<&Rule> = rules.iter().collect();
    executor::execute_rules(conn, &refs, "B1").issues()
}

/// 某行的问题
fn issues_of<'a>(issues: &'a [Issue], uid: &str) -> Vec<&'a Issue> {
    issues.iter().filter(|i| i.row_uid == uid).collect()
}

/// 某行的某类问题
fn kind_of<'a>(issues: &'a [Issue], uid: &str, kind: &str) -> Vec<&'a Issue> {
    issues_of(issues, uid)
        .into_iter()
        .filter(|i| i.issue_kind == kind)
        .collect()
}

// ————————————————————— 验收 1：复算一致通过 —————————————————————

#[test]
fn 复算一致不报公式不符() {
    let conn = seed(&[RowSpec::new("R1", "BK-01")]);
    let issues = run(&conn);

    assert!(
        kind_of(&issues, "R1", "公式不符").is_empty(),
        "980 ÷ 1000 = 98%，填报值 98 一致，不该报公式不符：{:?}",
        issues
    );
    assert!(
        issues.is_empty(),
        "取值齐全且复算一致的一行不该有任何问题：{:?}",
        issues
    );
}

#[test]
fn 复算一致允许四舍五入到指标精度() {
    // 970 ÷ 1000 = 97.0%；填 97.04 与 97.0 在小数点后 1 位上是同一个数
    let conn = seed(&[RowSpec {
        numerator: Some(970.0),
        denominator: Some(1000.0),
        value: Some(97.04),
        raw_value: "97.04".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);
    assert!(
        kind_of(&issues, "R1", "公式不符").is_empty(),
        "四舍五入到小数点后 1 位后一致，不该报错：{:?}",
        issues
    );
}

// ————————————————————— 验收 2：超容差报公式不符 —————————————————————

#[test]
fn 超容差报公式不符并给出实际值与期望值() {
    let conn = seed(&[RowSpec {
        numerator: Some(960.0),
        denominator: Some(1000.0),
        value: Some(75.0),
        raw_value: "75".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "公式不符");
    assert_eq!(got.len(), 1, "应恰好一条公式不符：{:?}", issues);
    assert_eq!(got[0].rule_id, RECALC_RULE_ID);
    assert_eq!(got[0].severity, "阻断");
    assert!(!got[0].basis.is_empty(), "每条口径问题都要带依据");

    let msg = &got[0].message;
    assert!(msg.contains("75"), "问题说明要给出实际值：{}", msg);
    assert!(
        msg.contains("96") && msg.contains("期望取值"),
        "问题说明要给出期望取值：{}",
        msg
    );
    assert!(
        msg.contains("分子 ÷ 分母 × 100%"),
        "问题说明要点明官方公式：{}",
        msg
    );
}

// ————————————————————— 验收 3：分母为 0 单列 —————————————————————

#[test]
fn 分母为零报数据形态错误且错误码为分母为零() {
    let conn = seed(&[RowSpec {
        numerator: Some(970.0),
        denominator: Some(0.0),
        value: Some(97.0),
        raw_denominator: "0".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "数据形态错误");
    assert_eq!(got.len(), 1, "应恰好一条数据形态错误：{:?}", issues);
    assert_eq!(got[0].rule_id, DENOMINATOR_ZERO_RULE_ID);
    assert_eq!(got[0].error_code, DENOMINATOR_ZERO_CODE);
    assert!(
        kind_of(&issues, "R1", "公式不符").is_empty(),
        "分母为 0 不该报成公式不符（要改的是分母口径，不是填报值）：{:?}",
        issues
    );
    assert!(
        got[0].message.contains("分母为 0"),
        "说明要点明分母为 0：{}",
        got[0].message
    );
}

// ————————————————————— 验收 4：负值与非数值归数据形态错误 —————————————————————

#[test]
fn 分子非数值报数值形态错误而不是必填缺失() {
    let conn = seed(&[RowSpec {
        numerator: None,
        raw_numerator: "abc".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "数据形态错误");
    assert_eq!(got.len(), 1, "应恰好一条数据形态错误：{:?}", issues);
    assert_eq!(got[0].rule_id, NUMERIC_SHAPE_RULE_ID);
    assert_eq!(got[0].error_code, INVALID_NUMERIC_CODE);
    assert!(
        got[0].message.contains("abc"),
        "说明要点明填的是「abc」：{}",
        got[0].message
    );
    assert!(
        kind_of(&issues, "R1", "必填缺失").is_empty(),
        "非数值原文是「填坏了」，不是「没填」，不该报必填缺失：{:?}",
        issues
    );
}

#[test]
fn 分子为负报数值形态错误() {
    let conn = seed(&[RowSpec {
        numerator: Some(-5.0),
        raw_numerator: "-5".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "数据形态错误");
    assert_eq!(got.len(), 1, "应恰好一条数据形态错误：{:?}", issues);
    assert_eq!(got[0].error_code, INVALID_NUMERIC_CODE);
    assert!(
        got[0].message.contains("负"),
        "说明要点明是负值：{}",
        got[0].message
    );
    assert!(
        kind_of(&issues, "R1", "公式不符").is_empty(),
        "负值不该报成公式不符：{:?}",
        issues
    );
}

#[test]
fn 填报值非数值同样归数值形态错误() {
    let conn = seed(&[RowSpec {
        value: None,
        raw_value: "n/a".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "数据形态错误");
    assert_eq!(got.len(), 1, "应恰好一条数据形态错误：{:?}", issues);
    assert_eq!(got[0].error_code, INVALID_NUMERIC_CODE);
}

#[test]
fn 分母非数值同样归数值形态错误() {
    let conn = seed(&[RowSpec {
        denominator: None,
        raw_denominator: "待补".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    let got = kind_of(&issues, "R1", "数据形态错误");
    assert_eq!(got.len(), 1, "应恰好一条数据形态错误：{:?}", issues);
    assert_eq!(got[0].error_code, INVALID_NUMERIC_CODE);
}

// ————————————————————— 验收 5：容差按指标给 —————————————————————

#[test]
fn 容差按逐指标精度表给而不是一个全局常数() {
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&indicators::precision_table_json()).unwrap();
    assert_eq!(items.len(), 24, "官方写明公式的比率型指标共 24 项");
    for item in items.iter() {
        assert_eq!(item["precision"].as_u64(), Some(1));
        assert_eq!(item["tolerance"].as_f64(), Some(0.05));
        // 精度表是规则的绑定参数，不是写进 SQL 的字面量
        let recalc =
            regval::rules::checks::formula::compile_formula_recalc_rule("2021.1", FORMULA_BASIS);
        assert!(recalc.sql.contains(":precision_table"));
        assert!(!recalc.sql.contains("0.05"));
    }
    // 精度不同 → 容差不同（口径种子按官方文件逐项填后即可生效）
    assert_eq!(indicators::tolerance_of(1), 0.05);
    assert_eq!(indicators::tolerance_of(2), 0.005);
}

// ————————————————————— 验收 6：人均负担型不判 —————————————————————

#[test]
fn 人均负担型不套用比率型公式() {
    let conn = seed(&[RowSpec {
        indicator: "MER-HR-01".to_string(),
        name: "住院病案管理人员月均负担出院患者病历数".to_string(),
        unit: "%".to_string(),
        numerator: Some(120.0),
        denominator: Some(10.0),
        value: Some(120.0),
        raw_numerator: "120".to_string(),
        raw_denominator: "10".to_string(),
        raw_value: "120".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    assert!(
        kind_of(&issues, "R1", "公式不符").is_empty(),
        "人均负担型的官方公式与本产品字段对不上，本轮不判：{:?}",
        issues
    );
    assert!(
        kind_of(&issues, "R1", "数据形态错误").is_empty(),
        "人均负担型不套用比率型口径：{:?}",
        issues
    );
}

// ————————————————————— 验收 7：端到端 —————————————————————

#[test]
fn 端到端契约里两类问题与错误码都在() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("result.json");
    let state = dir.path().join("state.db");
    let status = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("formula_bad.csv"))
        .arg("--out")
        .arg(&out)
        .env("REGVAL_STATE_DB", &state)
        .output()
        .expect("regval 应可执行");
    assert!(
        status.status.success(),
        "validate 应成功退出，stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    let by_kind = &contract["summary"]["issue_count_by_kind"];
    assert_eq!(by_kind["公式不符"], 1, "一行填报值与复算不符：{}", raw);
    assert_eq!(
        by_kind["数据形态错误"], 4,
        "分母为 0 / 分子非数值 / 分子为负 / 填报值非数值：{}",
        raw
    );

    let issues = contract["issues"].as_array().expect("issues 应为数组");
    let codes: Vec<&str> = issues
        .iter()
        .map(|i| i["error_code"].as_str().unwrap_or(""))
        .collect();
    assert!(
        codes.contains(&DENOMINATOR_ZERO_CODE),
        "契约里应带分母为 0 的错误码：{:?}",
        codes
    );
    assert!(
        codes.contains(&INVALID_NUMERIC_CODE),
        "契约里应带数值非法的错误码：{:?}",
        codes
    );
    // 数据形态错误的问题行，结论应为不通过
    let shape_rows: Vec<&Value> = issues
        .iter()
        .filter(|i| i["issue_kind"] == "数据形态错误")
        .collect();
    assert_eq!(shape_rows.len(), 4);
    for issue in shape_rows {
        assert_eq!(issue["severity"], "阻断");
        assert!(!issue["basis"].as_str().unwrap().is_empty());
    }
}
