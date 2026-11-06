//! 必填缺失与表内唯一性检查器（Task 10）。
//!
//! 覆盖五条硬验收：
//! 1. **五类必填列缺失各一例** —— 缺哪一列报哪一列，一行缺多列合并成清单；
//! 2. **空串与纯空格判为缺失** —— 表里「看着空、其实有字符」的写法不能放过；
//! 3. **条件必填在比率型指标上生效**，在非比率型指标上**不误报**；
//! 4. **0 不是缺失**、`NaN` 是缺失、非数值原文不算缺失（是格式错误）；
//! 5. **表内业务主键撞键标「疑似重复」**（提示级，不合并、不上报）。

use rusqlite::Connection;

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
                               reporting_period, business_key, numerator, denominator, value, unit,
                               remark, numeric_raw, rule_version, created_at)
             VALUES ('B1', ?1, 'a.csv', ?2, ?3, 'ORG', 'raw', ?4, ?5, ?6, '科室一',
                     ?7, ?8, ?9, ?10, ?11, ?12, '', ?13, NULL, '2026-11-06 00:00:00')",
            rusqlite::params![
                r.ns,
                (i + 1) as i64,
                r.uid,
                r.indicator,
                r.name,
                r.dept,
                r.period,
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

// ————————————————————— 验收 1：五类必填列缺失各一例 —————————————————————

#[test]
fn 五类必填列缺失各一例且各报各的列() {
    let rows = vec![
        RowSpec {
            biz_key: "BK-01".into(),
            indicator: "".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "BK-02".into(),
            name: "".into(),
            ..row("R2")
        },
        RowSpec {
            biz_key: "BK-03".into(),
            dept: "".into(),
            ..row("R3")
        },
        RowSpec {
            biz_key: "BK-04".into(),
            period: "".into(),
            ..row("R4")
        },
        RowSpec {
            biz_key: "".into(),
            ..row("R5")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    let expect = [
        ("R1", "指标编号"),
        ("R2", "指标名称"),
        ("R3", "科室代码"),
        ("R4", "报告期"),
        ("R5", "业务主键"),
    ];
    for (uid, column) in expect {
        let got = issues_of(&issues, uid);
        assert_eq!(got.len(), 1, "{} 应恰好一条必填缺失问题", uid);
        assert_eq!(got[0].issue_kind, "必填缺失");
        assert_eq!(got[0].missing_columns, vec![column.to_string()]);
        assert!(
            got[0].message.contains(column),
            "问题说明要点名缺哪一列：{}",
            got[0].message
        );
        assert!(!got[0].basis.is_empty(), "每条口径问题都要带依据");
    }
    assert_eq!(issues.len(), 5, "五行各一条，不多报");
}

#[test]
fn 一行缺多列合并成一张清单() {
    let rows = vec![RowSpec {
        biz_key: "BK-01".into(),
        name: "".into(),
        dept: "".into(),
        period: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = issues_of(&issues, "R1");
    assert_eq!(got.len(), 1, "同一行同一规则只落一条问题");
    assert_eq!(
        got[0].missing_columns,
        vec![
            "指标名称".to_string(),
            "科室代码".to_string(),
            "报告期".to_string()
        ]
    );
    assert!(got[0].message.contains("指标名称"));
    assert!(got[0].message.contains("科室代码"));
    assert!(got[0].message.contains("报告期"));
}

#[test]
fn 空串与纯空格判为缺失() {
    let rows = vec![
        RowSpec {
            biz_key: "BK-01".into(),
            name: "   ".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "BK-02".into(),
            name: "\u{3000}".into(), // 全角空格
            ..row("R2")
        },
        RowSpec {
            biz_key: "BK-03".into(),
            dept: "\t".into(),
            ..row("R3")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    for (uid, column) in [("R1", "指标名称"), ("R2", "指标名称"), ("R3", "科室代码")] {
        let got = issues_of(&issues, uid);
        assert_eq!(got.len(), 1, "{} 的空白应判为缺失", uid);
        assert_eq!(got[0].missing_columns, vec![column.to_string()]);
    }
}

// ————————————————————— 验收 2：条件必填 —————————————————————

#[test]
fn 条件必填在比率型指标上生效() {
    let rows = vec![RowSpec {
        biz_key: "BK-01".into(),
        numerator: "".into(),
        denominator: "".into(),
        value: "".into(),
        unit: "".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = issues_of(&issues, "R1");
    assert_eq!(got.len(), 1, "比率型指标缺分子分母填报值单位，应报一条");
    assert_eq!(got[0].issue_kind, "必填缺失", "条件必填归入必填缺失一类");
    assert_eq!(
        got[0].missing_columns,
        vec![
            "分子".to_string(),
            "分母".to_string(),
            "填报值".to_string(),
            "计量单位".to_string()
        ]
    );
    assert!(
        got[0].message.contains("条件必填列"),
        "措辞要说明是条件必填：{}",
        got[0].message
    );
}

#[test]
fn 条件必填在非比率型指标上不误报() {
    // MER-HR-01 是人力资源配置指标（人均负担），官方口径里没有分子分母单位的要求
    let rows = vec![RowSpec {
        biz_key: "BK-01".into(),
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
        "非比率型指标不适用条件必填，不应误报：{:?}",
        issues
    );
}

// ————————————————————— 验收 3：空值语义 —————————————————————

#[test]
fn 零值不算缺失() {
    // 分子为 0 是真实业务取值（本期零事件）；分母为 0 也不是「没填」，
    // 而是分母为 0 的口径问题（Task 13 起报 DENOMINATOR_ZERO）。
    let rows = vec![RowSpec {
        biz_key: "BK-01".into(),
        numerator: "0".into(),
        denominator: "0".into(),
        value: "0".into(),
        ..row("R1")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let row_issues = issues_of(&issues, "R1");
    assert!(
        !row_issues.iter().any(|i| i.issue_kind == "必填缺失"),
        "0 是有效取值，不判为缺失：{:?}",
        row_issues
    );
    // 分母为 0 归数据形态错误单列（错误码 DENOMINATOR_ZERO），不静默当 0 处理
    let zero = row_issues
        .iter()
        .find(|i| i.error_code == "DENOMINATOR_ZERO")
        .expect("分母为 0 应报 DENOMINATOR_ZERO");
    assert_eq!(zero.issue_kind, "数据形态错误");
    assert_eq!(zero.rule_id, "R-FORMULA-002");
}

#[test]
fn nan_判为缺失而非数值原文不算缺失() {
    let rows = vec![
        RowSpec {
            biz_key: "BK-01".into(),
            numerator: "NaN".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "BK-02".into(),
            numerator: "abc".into(),
            ..row("R2")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    let nan_row = issues_of(&issues, "R1");
    assert_eq!(nan_row.len(), 1, "NaN 判为缺失");
    assert_eq!(nan_row[0].missing_columns, vec!["分子".to_string()]);

    let garbage_row = issues_of(&issues, "R2");
    assert!(
        !garbage_row.iter().any(|i| i.issue_kind == "必填缺失"),
        "非数值原文不是「没填」，不该报必填缺失：{:?}",
        garbage_row
    );
    // 非数值原文是格式错误：Task 13 起报 INVALID_NUMERIC（数据形态错误）
    let shape = garbage_row
        .iter()
        .find(|i| i.error_code == "INVALID_NUMERIC")
        .expect("非数值原文应报 INVALID_NUMERIC");
    assert_eq!(shape.issue_kind, "数据形态错误");
    assert_eq!(shape.rule_id, "R-FORMULA-003");
}

// ————————————————————— 验收 4：表内唯一性 —————————————————————

#[test]
fn 表内业务主键撞键标疑似重复且是提示级() {
    let rows = vec![
        RowSpec {
            biz_key: "BK-01".into(),
            numerator: "80".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "BK-01".into(),
            numerator: "80".into(),
            ..row("R2")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    let dup: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.issue_kind == "疑似重复")
        .collect();
    assert_eq!(dup.len(), 2, "一对撞键的两行都要标出来");
    for issue in dup {
        assert_eq!(issue.severity, "提示", "疑似重复只是候选，不阻断");
        assert!(issue.message.contains("疑似重复"));
        assert!(
            issue.message.contains("第 ") && issue.message.contains(" 行"),
            "要点名对照行：{}",
            issue.message
        );
        assert!(issue.message.contains("不合并"));
        assert!(!issue.basis.is_empty());
    }
    // 撞键不报必填缺失
    assert!(issues
        .iter()
        .all(|i| i.issue_kind != "必填缺失" || i.missing_columns.is_empty()));
}

#[test]
fn 跨表撞键不由表内唯一性规则管() {
    // 跨表撞键是实体绑定层的事（对齐、待人工判定），本轮只管同一张表内
    let rows = vec![
        RowSpec {
            ns: "表一".into(),
            biz_key: "BK-01".into(),
            ..row("R1")
        },
        RowSpec {
            ns: "表二".into(),
            biz_key: "BK-01".into(),
            ..row("R2")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        issues.iter().all(|i| i.issue_kind != "疑似重复"),
        "跨表撞键不报表内疑似重复：{:?}",
        issues
    );
}

#[test]
fn 键要素不全的行不参与表内唯一性检查() {
    // 缺业务主键的行连键都拼不出来，不能再报一次「重复」
    let rows = vec![
        RowSpec {
            biz_key: "".into(),
            ..row("R1")
        },
        RowSpec {
            biz_key: "".into(),
            ..row("R2")
        },
    ];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        issues.iter().all(|i| i.issue_kind != "疑似重复"),
        "缺键要素的行不报疑似重复：{:?}",
        issues
    );
}

// ————————————————————— 验收 5：端到端 —————————————————————

#[test]
fn 端到端校验后契约里能看到四类口径问题() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dup_rows.csv"))
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

    // dup_rows.csv：BK-01 两行、BK-02 两行 → 四行撞键
    assert_eq!(contract["summary"]["issue_count_by_kind"]["疑似重复"], 4);
    // 实体绑定层的待人工判定照旧
    assert_eq!(contract["summary"]["issue_count_by_kind"]["待人工判定"], 2);

    // 行结论：只有提示级问题的行是「有提示」；
    // 同键异值的行还带着阻断级的「待人工判定」，结论是「不通过」
    let rows = contract["rows"].as_array().unwrap();
    let only_hint = rows
        .iter()
        .filter(|r| {
            let t = r["issue_types"].as_str().unwrap_or("");
            t.contains("疑似重复") && !t.contains("待人工判定")
        })
        .collect::<Vec<_>>();
    assert_eq!(only_hint.len(), 2, "两行取值完全一致的撞键行只有提示级问题");
    assert!(only_hint.iter().all(|r| r["conclusion"] == "有提示"));

    let blocked = rows
        .iter()
        .filter(|r| {
            r["issue_types"]
                .as_str()
                .unwrap_or("")
                .contains("待人工判定")
        })
        .collect::<Vec<_>>();
    assert_eq!(blocked.len(), 2);
    assert!(blocked.iter().all(|r| r["conclusion"] == "不通过"));
}
