//! 枚举越界与条件必填检查器（Task 11）。
//!
//! 覆盖五条硬验收：
//! 1. **五类枚举越界各一例** —— 报告期格式 / 报告期类型 / 计量单位 / 指标编号（值域枚举）
//!    与 科室代码（目录型枚举）各报各的，不串台；
//! 2. **非法报告期格式被报出且给出期望取值清单** —— 只说「不在枚举内」科室改不动；
//! 3. **条件必填按指标类别判定** —— 比率型生效、人均负担型与「编号不在册」不误报；
//! 4. **未填 ≠ 写错** —— 空值不报枚举越界（那是必填缺失的事）；
//! 5. **目录拉不到时不静默通过** —— 标「科室主数据未校验」，不当作通过。

use std::collections::BTreeMap;

use rusqlite::Connection;

use regval::bind::dept::{InMemoryDirectory, UnavailableDirectory};
use regval::ingest::{Field, SourceRow};
use regval::rules::checks::enum_::{
    conditional_required_applies, dept_master_check, expected_values, ENUM_DIMENSIONS,
};
use regval::rules::executor;
use regval::rules::model::{Issue, Rule};

/// 一行样例（字段全给，缺哪列写空串）
#[derive(Debug, Clone)]
struct RowSpec {
    uid: String,
    ns: String,
    indicator: String,
    name: String,
    dept: String,
    period: String,
    period_type: String,
    biz_key: String,
    numerator: String,
    denominator: String,
    value: String,
    unit: String,
}

/// 默认样例行：比率型指标（MER-TL-04），取值齐全
fn row(uid: &str) -> RowSpec {
    RowSpec {
        uid: uid.to_string(),
        ns: "表一".to_string(),
        indicator: "MER-TL-04".to_string(),
        name: "病案首页24小时内完成率".to_string(),
        dept: "0301".to_string(),
        period: "2026-Q3".to_string(),
        period_type: "季报".to_string(),
        biz_key: "BK-01".to_string(),
        numerator: "80".to_string(),
        denominator: "100".to_string(),
        value: "80".to_string(),
        unit: "%".to_string(),
    }
}

/// 解析数值列：空 / 非数值 / 非有限数都存不进数值列（与读表层一致）
fn parse_number(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => Some(v),
        _ => None,
    }
}

/// 数值原文（与落库层同格式：键与列名同名，取值去首尾空白）
fn numeric_raw(r: &RowSpec) -> String {
    serde_json::json!({
        "numerator": r.numerator.trim(),
        "denominator": r.denominator.trim(),
        "value": r.value.trim(),
    })
    .to_string()
}

/// 建一个带样例行的本机库（内存库）
fn seed(rows: &[RowSpec]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    regval::db::schema::migrate(&conn).unwrap();
    for (i, r) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                               authority_state, indicator_code, indicator_name, dept_code, dept_name,
                               reporting_period, period_type, business_key, numerator, denominator,
                               value, unit, remark, numeric_raw, rule_version, created_at)
             VALUES ('B1', ?1, 'a.csv', ?2, ?3, 'ORG', 'raw', ?4, ?5, ?6, '科室一',
                     ?7, ?8, ?9, ?10, ?11, ?12, ?13, '', ?14, NULL, '2026-11-06 00:00:00')",
            rusqlite::params![
                r.ns,
                (i + 1) as i64,
                r.uid,
                r.indicator,
                r.name,
                r.dept,
                r.period,
                r.period_type,
                r.biz_key,
                parse_number(&r.numerator),
                parse_number(&r.denominator),
                parse_number(&r.value),
                r.unit,
                numeric_raw(r),
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

/// 某行的枚举越界问题
fn enum_issues_of<'a>(issues: &'a [Issue], uid: &str) -> Vec<&'a Issue> {
    issues_of(issues, uid)
        .into_iter()
        .filter(|i| i.issue_kind == "枚举越界")
        .collect()
}

/// 造一个来源行（给目录型枚举用）
fn source_row(dept: &str) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::DeptCode, dept.to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, "BK-01".to_string());
    SourceRow {
        source_ns: "表一".to_string(),
        source_file: "表一.csv".to_string(),
        row_no: 1,
        dup_ordinal: 0,
        values,
    }
}

// ————————————————————— 验收 1：五类枚举越界各一例 —————————————————————

#[test]
fn 五类枚举越界各一例且各归各的规则() {
    let rows = vec![
        // ① 报告期格式：2026Q3 不在四种写法之内
        RowSpec {
            biz_key: "BK-01".into(),
            period: "2026Q3".into(),
            ..row("R1")
        },
        // ② 报告期类型：季度报 不在五种类型之内
        RowSpec {
            biz_key: "BK-02".into(),
            period_type: "季度报".into(),
            ..row("R2")
        },
        // ③ 计量单位：台 不在五种单位之内
        RowSpec {
            biz_key: "BK-03".into(),
            unit: "台".into(),
            ..row("R3")
        },
        // ④ 指标编号：MER-XX-99 不在官方 27 项之内
        RowSpec {
            biz_key: "BK-04".into(),
            indicator: "MER-XX-99".into(),
            ..row("R4")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    let expect = [
        ("R1", "R-ENUM-001", "报告期格式"),
        ("R2", "R-ENUM-002", "报告期类型"),
        ("R3", "R-ENUM-003", "计量单位"),
        ("R4", "R-ENUM-004", "指标编号"),
    ];
    for (uid, rule_id, dimension) in expect {
        let got = enum_issues_of(&issues, uid);
        assert_eq!(
            got.len(),
            1,
            "{}（{}）应恰好一条枚举越界问题",
            uid,
            dimension
        );
        assert_eq!(got[0].rule_id, rule_id, "{} 应由 {} 报出", uid, rule_id);
        assert_eq!(got[0].issue_kind, "枚举越界");
        assert_eq!(got[0].severity, "阻断");
        assert!(!got[0].basis.is_empty(), "每条口径问题都要带依据");
        assert!(
            got[0].message.contains(dimension),
            "问题说明要点名是哪一项：{}",
            got[0].message
        );
    }
    assert_eq!(issues.len(), 4, "四行各一条，不多报");
}

#[test]
fn 五类枚举维度齐备且期望取值清单都在() {
    assert_eq!(ENUM_DIMENSIONS.len(), 5);
    for dim in ENUM_DIMENSIONS.iter() {
        assert!(!dim.expected.is_empty(), "{} 缺期望取值清单", dim.name);
        assert_eq!(
            expected_values(dim.code),
            Some(dim.expected),
            "{} 的期望取值清单应能按规则编号取到",
            dim.name
        );
    }
    // 值域枚举 4 类 + 目录型枚举 1 类
    assert_eq!(
        ENUM_DIMENSIONS
            .iter()
            .filter(|d| d.kind == regval::rules::checks::enum_::EnumDimensionKind::ValueEnum)
            .count(),
        4
    );
    assert_eq!(
        ENUM_DIMENSIONS
            .iter()
            .filter(|d| d.kind == regval::rules::checks::enum_::EnumDimensionKind::DirectoryEnum)
            .count(),
        1
    );
}

// ————————————————————— 验收 2：期望取值清单 —————————————————————

#[test]
fn 非法报告期格式被报出且给出期望取值清单() {
    let rows = vec![RowSpec {
        period: "2026 年第三季度".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = enum_issues_of(&issues, "R1");
    assert_eq!(got.len(), 1);
    let msg = &got[0].message;
    assert!(
        msg.contains("期望取值"),
        "问题说明要给期望取值清单：{}",
        msg
    );
    // 清单里四种写法都要列出来，科室照着改
    for want in [
        "2026（年报",
        "2026-H1",
        "2026-Q1…2026-Q4",
        "2026-M01…2026-M12",
    ] {
        assert!(msg.contains(want), "期望取值清单缺 {}：{}", want, msg);
    }
    // 也要说清实际填了什么
    assert!(
        msg.contains("2026 年第三季度"),
        "问题说明要回显实际取值：{}",
        msg
    );
}

#[test]
fn 其余四类枚举越界同样给出期望取值清单() {
    let rows = vec![
        RowSpec {
            biz_key: "BK-01".into(),
            period_type: "季度报".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "BK-02".into(),
            unit: "台".into(),
            ..row("R2")
        },
        RowSpec {
            biz_key: "BK-03".into(),
            indicator: "MER-XX-99".into(),
            ..row("R3")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    let r1 = enum_issues_of(&issues, "R1");
    assert!(r1[0].message.contains("期望取值"));
    for want in ["年报", "半年报", "季报", "月报", "实时报"] {
        assert!(
            r1[0].message.contains(want),
            "报告期类型清单缺 {}：{}",
            want,
            r1[0].message
        );
    }

    let r2 = enum_issues_of(&issues, "R2");
    assert!(r2[0].message.contains("期望取值"));
    for want in ["例", "例次", "人次", "人", "%"] {
        assert!(
            r2[0].message.contains(want),
            "计量单位清单缺 {}：{}",
            want,
            r2[0].message
        );
    }

    let r3 = enum_issues_of(&issues, "R3");
    assert!(r3[0].message.contains("期望取值"));
    assert!(
        r3[0].message.contains("27 项"),
        "指标编号清单要点明共 27 项：{}",
        r3[0].message
    );
}

// ————————————————————— 验收 3：条件必填按指标类别判定 —————————————————————

#[test]
fn 条件必填在比率型指标上生效() {
    let rows = vec![RowSpec {
        numerator: "".into(),
        denominator: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = issues_of(&issues, "R1");
    assert_eq!(got.len(), 1, "比率型指标缺分子分母，应报一条");
    assert_eq!(got[0].issue_kind, "必填缺失");
    assert_eq!(
        got[0].missing_columns,
        vec!["分子".to_string(), "分母".to_string()]
    );
    assert!(got[0].message.contains("条件必填列"));
}

#[test]
fn 条件必填在人均负担型指标上不误报() {
    // MER-HR-01 是人力资源配置指标（人均负担），官方口径不要求分子/分母/单位
    let rows = vec![RowSpec {
        indicator: "MER-HR-01".into(),
        name: "住院病案管理人员月均负担出院患者病历数".into(),
        numerator: "".into(),
        denominator: "".into(),
        value: "".into(),
        unit: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        issues_of(&issues, "R1").is_empty(),
        "人均负担型指标不适用条件必填，不应误报：{:?}",
        issues
    );
}

#[test]
fn 指标编号不在册时条件必填不生效_改报枚举越界() {
    // 编号不在官方 27 项之内 → 判不出指标类别 → 不套条件必填，
    // 先让科室把编号改对（报枚举越界）
    let rows = vec![RowSpec {
        indicator: "MER-XX-99".into(),
        numerator: "".into(),
        denominator: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = issues_of(&issues, "R1");
    assert_eq!(got.len(), 1, "只应报「指标编号不在册」一条");
    assert_eq!(got[0].issue_kind, "枚举越界");
    assert_eq!(got[0].rule_id, "R-ENUM-004");
    assert!(
        !got[0].missing_columns.contains(&"分子".to_string()),
        "编号不在册时不该按猜出来的口径报条件必填"
    );
    assert!(conditional_required_applies("MER-XX-99") == false);
}

#[test]
fn 条件必填判定函数与官方登记表一致() {
    assert!(conditional_required_applies("MER-TL-04"));
    assert!(conditional_required_applies("MER-TQ-09"));
    assert!(!conditional_required_applies("MER-HR-01"));
    assert!(!conditional_required_applies("MER-HR-02"));
    assert!(!conditional_required_applies("MER-HR-03"));
    assert!(!conditional_required_applies("MER-XX-99"));
}

// ————————————————————— 验收 4：未填 ≠ 写错 —————————————————————

#[test]
fn 未填不报枚举越界_空值归必填缺失() {
    let rows = vec![RowSpec {
        period: "".into(),
        period_type: "".into(),
        unit: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        issues.iter().all(|i| i.issue_kind != "枚举越界"),
        "没填不叫写错，不该报枚举越界：{:?}",
        issues
    );
    let got = issues_of(&issues, "R1");
    assert!(
        got.iter().any(|i| i.issue_kind == "必填缺失"),
        "空值归必填缺失：{:?}",
        got
    );
}

#[test]
fn 合法取值一律不报() {
    let rows = vec![
        RowSpec {
            period: "2026".into(),
            period_type: "年报".into(),
            unit: "例".into(),
            ..row("R1")
        },
        RowSpec {
            period: "2026-H1".into(),
            period_type: "半年报".into(),
            unit: "例次".into(),
            ..row("R2")
        },
        RowSpec {
            period: "2026-Q3".into(),
            period_type: "季报".into(),
            unit: "人次".into(),
            ..row("R3")
        },
        RowSpec {
            period: "2026-M03".into(),
            period_type: "月报".into(),
            unit: "人".into(),
            ..row("R4")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        issues.iter().all(|i| i.issue_kind != "枚举越界"),
        "四种报告期写法与五种单位都合法，不该报枚举越界：{:?}",
        issues
    );
}

#[test]
fn 报告期格式越界不误伤合法写法() {
    // 越界的写法要能被抓到，合法的一律放过
    for bad in [
        "2026Q3",
        "2026-Q5",
        "2026-M13",
        "2026-H2",
        "26-Q3",
        "第三季度",
    ] {
        let rows = vec![RowSpec {
            period: bad.into(),
            ..row("R1")
        }];
        let conn = seed(&rows);
        let issues = run(&conn);
        assert_eq!(
            enum_issues_of(&issues, "R1").len(),
            1,
            "{} 应被判为报告期格式越界",
            bad
        );
    }
    for good in [
        "2026", "2026-H1", "2026-Q1", "2026-Q4", "2026-M01", "2026-M12",
    ] {
        let rows = vec![RowSpec {
            period: good.into(),
            ..row("R1")
        }];
        let conn = seed(&rows);
        let issues = run(&conn);
        assert!(
            enum_issues_of(&issues, "R1").is_empty(),
            "{} 是合法写法，不该报枚举越界",
            good
        );
    }
}

// ————————————————————— 验收 5：目录型枚举 —————————————————————

#[test]
fn 科室代码不在目录里报科室主数据不一致() {
    let dir = InMemoryDirectory::new(["0301", "0302"]);
    let out = dept_master_check(&[source_row("0999")], &dir);

    assert!(out.checked, "目录可用时应真的核对过");
    assert_eq!(out.issues.len(), 1);
    assert_eq!(out.issues[0].issue_kind, "科室主数据不一致");
    assert_eq!(out.issues[0].error_code, "DEPT_NOT_FOUND");
    assert_eq!(out.issues[0].rule_id, "R-DEPT-001");
    assert!(out.issues[0].message.contains("0999"));
    assert!(!out.issues[0].basis.is_empty());
    // 与另外四类值域枚举同一口径：报「不在枚举内」科室改不动，要给期望取值
    assert!(
        out.issues[0].message.contains("期望取值"),
        "科室代码越界也要给期望取值清单：{}",
        out.issues[0].message
    );
    assert!(out.issues[0].message.contains("HIS 科室目录"));
    assert_eq!(out.unmatched, vec!["0999".to_string()]);
}

#[test]
fn 科室代码在目录里不报() {
    let dir = InMemoryDirectory::new(["0301", "0302"]);
    let out = dept_master_check(&[source_row("0301")], &dir);
    assert!(out.checked);
    assert!(out.issues.is_empty());
}

#[test]
fn 目录不可用时跳过且不静默通过() {
    // 目录拉不到 → 跳过检查；上层据此标「科室主数据未校验」，不当成通过
    let out = dept_master_check(&[source_row("0999")], &UnavailableDirectory);
    assert!(
        out.issues.is_empty(),
        "目录不可用时不该凭空报科室主数据不一致：{:?}",
        out.issues
    );
    // 关键：一条都没报 ≠ 核对过。checked=false 才是「没查成」与「查过没问题」的区别
    assert!(
        !out.checked,
        "目录拉不到不等于科室没问题，checked 必须为 false（否则就是静默通过）"
    );
}

// ————————————————————— 端到端 —————————————————————

#[test]
fn 端到端校验后契约里能看到枚举越界与期望取值() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/enum_bad.csv"))
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
    let contract: serde_json::Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    // 样例表四行各犯一类枚举越界
    assert_eq!(contract["summary"]["issue_count_by_kind"]["枚举越界"], 4);

    // 问题说明带期望取值清单
    let issues = contract["issues"].as_array().unwrap();
    let enum_issues: Vec<&serde_json::Value> = issues
        .iter()
        .filter(|i| i["issue_kind"].as_str() == Some("枚举越界"))
        .collect();
    assert_eq!(enum_issues.len(), 4);
    assert!(enum_issues
        .iter()
        .all(|i| i["message"].as_str().unwrap_or("").contains("期望取值")));

    // 行结论：犯枚举越界的行都是「不通过」（阻断级）
    let rows = contract["rows"].as_array().unwrap();
    let failed = rows
        .iter()
        .filter(|r| r["issue_types"].as_str().unwrap_or("").contains("枚举越界"))
        .collect::<Vec<_>>();
    assert_eq!(failed.len(), 4);
    assert!(failed.iter().all(|r| r["conclusion"] == "不通过"));

    // 规则清单里四条枚举越界各带各的检查项
    let rules = contract["rule_status"]["rules"].as_array().unwrap();
    let enum_rules: Vec<&serde_json::Value> = rules
        .iter()
        .filter(|r| r["rule_kind"].as_str() == Some("枚举越界"))
        .collect();
    assert_eq!(enum_rules.len(), 4);
    for r in enum_rules.iter() {
        assert!(!r["check_column"].as_str().unwrap_or("").is_empty());
        assert!(!r["basis"].as_str().unwrap_or("").is_empty());
    }
}

#[test]
fn 报告期写法判定与_sql_规则结论一致() {
    // SQL 判据（GLOB）与 Rust 判据（is_valid_reporting_period）必须一致：
    // 两处一宽一紧，催报对照就会漏人或冤枉人。
    let cases = [
        ("2026", true),
        ("2026-H1", true),
        ("2026-Q1", true),
        ("2026-Q4", true),
        ("2026-M01", true),
        ("2026-M09", true),
        ("2026-M12", true),
        ("2026Q3", false),
        ("2026-Q5", false),
        ("2026-M00", false),
        ("2026-M13", false),
        ("2026-H2", false),
        ("26-Q3", false),
        ("第三季度", false),
        ("2026 年第三季度", false),
    ];
    let mut rows: Vec<RowSpec> = Vec::new();
    for (i, (period, _)) in cases.iter().enumerate() {
        let mut r = row(&format!("u{}", i));
        r.period = period.to_string();
        rows.push(r);
    }
    let conn = seed(&rows);
    let issues = run(&conn);

    for (i, (period, expected_valid)) in cases.iter().enumerate() {
        let uid = format!("u{}", i);
        let sql_says_invalid = !enum_issues_of(&issues, &uid).is_empty();
        assert_eq!(
            sql_says_invalid,
            !expected_valid,
            "报告期「{}」两处判据不一致（SQL 判越界={}，Rust 判合法={}）",
            period,
            sql_says_invalid,
            expected_valid
        );
        assert_eq!(
            regval::rules::checks::enum_::is_valid_reporting_period(period),
            *expected_valid,
            "报告期「{}」的 Rust 判据不对",
            period
        );
    }
}
