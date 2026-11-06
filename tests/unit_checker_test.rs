//! 计量单位检查器（Task 12）。
//!
//! 覆盖任务验收与口径分工：
//! 1. **比率型指标单位为 % 时通过** —— 不误报；
//! 2. **比率型指标单位写成「例」时报错并给出期望单位** —— 科室照着能改；
//! 3. **单位别名两种写法判定一致** —— 全角 `％` 与 `%`、`例数` 与 `例` 是同一个单位，
//!    结论必须一样（否则科室改完一遍，下期又冒出另一种写法）；
//! 4. **值域与匹配分开报、同一行同一件事不重复报** —— 单位「台」只报枚举越界，
//!    单位「例」只报单位不符；
//! 5. **口径待补不猜** —— 官方未写明单位的指标不判单位是否匹配；
//! 6. **别名表在口径层** —— 随规则表下发，与口径层内置表逐条一致。

use rusqlite::Connection;

use regval::rules::checks::unit::{UNIT_ENUM, UNIT_MATCH_BASIS};
use regval::rules::executor;
use regval::rules::indicators;
use regval::rules::model::{Issue, Rule};
use regval::rules::normalize;

/// 一行样例（只改关心的字段）
#[derive(Debug, Clone)]
struct RowSpec {
    uid: String,
    biz_key: String,
    indicator: String,
    name: String,
    unit: String,
}

impl RowSpec {
    /// 默认行：比率型指标 MER-TL-04，单位 %，取值齐全
    fn new(uid: &str, biz_key: &str) -> Self {
        RowSpec {
            uid: uid.to_string(),
            biz_key: biz_key.to_string(),
            indicator: "MER-TL-04".to_string(),
            name: "病案首页24小时内完成率".to_string(),
            unit: "%".to_string(),
        }
    }
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
             VALUES ('B1', ?1, 'a.csv', ?2, ?3, 'ORG', 'raw', ?4, ?5, '0301', '病案统计室',
                     '2026-Q3', '季报', ?6, 98.0, 100.0, 98.0, ?7, '', ?8, NULL, '2026-11-06 00:00:00')",
            rusqlite::params![
                "表一",
                (i + 1) as i64,
                r.uid,
                r.indicator,
                r.name,
                r.biz_key,
                r.unit,
                serde_json::json!({ "numerator": "98", "denominator": "100", "value": "98" })
                    .to_string(),
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

/// 某行的「单位不符」问题
fn unit_issues_of<'a>(issues: &'a [Issue], uid: &str) -> Vec<&'a Issue> {
    issues_of(issues, uid)
        .into_iter()
        .filter(|i| i.issue_kind == "单位不符")
        .collect()
}

/// 某行的「枚举越界」问题
fn enum_issues_of<'a>(issues: &'a [Issue], uid: &str) -> Vec<&'a Issue> {
    issues_of(issues, uid)
        .into_iter()
        .filter(|i| i.issue_kind == "枚举越界")
        .collect()
}

// ————————————————————— 验收 1：单位为 % 时通过 —————————————————————

#[test]
fn 比率型指标单位为百分号时通过() {
    let rows = vec![RowSpec::new("R1", "BK-01")];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        unit_issues_of(&issues, "R1").is_empty(),
        "单位 % 与指标匹配，不该报单位不符：{:?}",
        issues
    );
    assert!(
        issues.iter().all(|i| i.issue_kind != "枚举越界"),
        "% 是合法单位，不该报枚举越界：{:?}",
        issues
    );
    assert!(
        issues.is_empty(),
        "取值齐全的一行不该有任何问题：{:?}",
        issues
    );
}

// ————————————————————— 验收 2：写成「例」时报错并给出期望单位 —————————————————————

#[test]
fn 比率型指标单位写成例时报单位不符并给出期望单位() {
    let rows = vec![RowSpec {
        unit: "例".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    let got = unit_issues_of(&issues, "R1");
    assert_eq!(got.len(), 1, "应恰好一条单位不符：{:?}", issues);
    assert_eq!(got[0].rule_id, "R-UNIT-001");
    assert_eq!(got[0].severity, "阻断");
    assert!(!got[0].basis.is_empty(), "每条口径问题都要带依据");

    let msg = &got[0].message;
    assert!(msg.contains("期望单位"), "问题说明要给期望单位：{}", msg);
    assert!(msg.contains("%"), "期望单位就是 %：{}", msg);
    assert!(msg.contains("例"), "问题说明要回显实际填了什么：{}", msg);
    assert!(msg.contains("MER-TL-04"), "要点名是哪个指标：{}", msg);
    assert!(
        msg.contains("病案首页24小时内完成率"),
        "要带上指标中文名，科室才知道改哪张表：{}",
        msg
    );
}

#[test]
fn 单位不符与枚举越界分开报_同一行同一件事不重复报() {
    // ① 单位「例」在合法取值之内，但与比率型指标不匹配 → 只报单位不符
    let conn = seed(&[RowSpec {
        unit: "例".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);
    assert_eq!(unit_issues_of(&issues, "R1").len(), 1);
    assert!(
        enum_issues_of(&issues, "R1").is_empty(),
        "「例」是合法取值，不该再报枚举越界：{:?}",
        issues
    );
    assert_eq!(issues.len(), 1, "只报一条，不重复：{:?}", issues);

    // ② 单位「台」不在合法取值之内 → 只报枚举越界（值域问题）
    let conn = seed(&[RowSpec {
        unit: "台".to_string(),
        ..RowSpec::new("R2", "BK-02")
    }]);
    let issues = run(&conn);
    assert_eq!(enum_issues_of(&issues, "R2").len(), 1);
    assert_eq!(enum_issues_of(&issues, "R2")[0].rule_id, "R-ENUM-003");
    assert!(
        unit_issues_of(&issues, "R2").is_empty(),
        "值域问题报了就不该再报单位不符 —— 科室改一次就够：{:?}",
        issues
    );
    assert_eq!(issues.len(), 1, "只报一条，不重复：{:?}", issues);
}

// ————————————————————— 验收 3：别名写法判定一致 —————————————————————

#[test]
fn 单位别名两种写法判定一致() {
    // 同一单位的两种写法，结论必须一模一样（规则编号 / 问题类型 / 严重度）
    let pairs = [
        ("%", "％"),        // 半角 / 全角百分号
        ("例", "例数"),     // 例 / 例数
        ("例次", "例次数"), // 例次 / 例次数
        ("人次", "人次数"), // 人次 / 人次数
        ("人", "人数"),     // 人 / 人数
    ];
    for (canonical, alias) in pairs.iter() {
        let conn = seed(&[
            RowSpec {
                unit: canonical.to_string(),
                ..RowSpec::new("R1", "BK-01")
            },
            RowSpec {
                unit: alias.to_string(),
                ..RowSpec::new("R2", "BK-02")
            },
        ]);
        let issues = run(&conn);

        let a = issues_of(&issues, "R1");
        let b = issues_of(&issues, "R2");
        let shape = |v: &[&Issue]| -> Vec<(String, String, String)> {
            v.iter()
                .map(|i| (i.rule_id.clone(), i.issue_kind.clone(), i.severity.clone()))
                .collect()
        };
        assert_eq!(
            shape(&a),
            shape(&b),
            "「{}」与「{}」是同一个单位，结论应一致",
            canonical,
            alias
        );
    }
}

#[test]
fn 全角百分号不算枚举越界_别名写法在枚举之内() {
    for raw in ["％", " % ", "\u{3000}％\u{3000}"] {
        let conn = seed(&[RowSpec {
            unit: raw.to_string(),
            ..RowSpec::new("R1", "BK-01")
        }]);
        let issues = run(&conn);
        assert!(
            enum_issues_of(&issues, "R1").is_empty(),
            "「{}」归一后就是 %，不该报枚举越界：{:?}",
            raw,
            issues
        );
        assert!(
            unit_issues_of(&issues, "R1").is_empty(),
            "「{}」归一后就是 %，不该报单位不符：{:?}",
            raw,
            issues
        );
    }
}

#[test]
fn 例数的别名写法判成单位不符_与写例同结论() {
    let conn = seed(&[RowSpec {
        unit: "例数".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    assert!(
        enum_issues_of(&issues, "R1").is_empty(),
        "「例数」归一后是「例」，是合法取值：{:?}",
        issues
    );
    let got = unit_issues_of(&issues, "R1");
    assert_eq!(
        got.len(),
        1,
        "归一后与「例」一样，与指标不匹配：{:?}",
        issues
    );
    assert!(
        got[0].message.contains("归一后为「例」"),
        "问题说明要给出归一后的值，科室才知道该填什么：{}",
        got[0].message
    );
}

#[test]
fn sql_判定与_rust_归一结论一致() {
    // 规则 SQL 里的归一（trim + 全角转半角 + 小写）与口径层 Rust 归一必须一致 ——
    // 两边不一致 = 同一行在两个环节结论不同。
    let table = normalize::AliasTable::builtin();
    let samples = [
        "%",
        "％",
        " % ",
        "例",
        "例数",
        "例次",
        "例次数",
        "人次",
        "人次数",
        "人",
        "人数",
        "台",
        "万元",
    ];
    for raw in samples.iter() {
        let canonical = table.canonicalize(raw);
        let conn = seed(&[RowSpec {
            unit: raw.to_string(),
            ..RowSpec::new("R1", "BK-01")
        }]);
        let issues = run(&conn);

        let unit_bad = unit_issues_of(&issues, "R1").len();
        let enum_bad = enum_issues_of(&issues, "R1").len();
        if canonical == "%" {
            assert_eq!(
                (unit_bad, enum_bad),
                (0, 0),
                "「{}」归一后是 %，不该报任何单位问题",
                raw
            );
        } else if UNIT_ENUM.contains(&canonical.as_str()) {
            assert_eq!(
                (unit_bad, enum_bad),
                (1, 0),
                "「{}」归一后是「{}」（合法但不匹配），应只报单位不符",
                raw,
                canonical
            );
        } else {
            assert_eq!(
                (unit_bad, enum_bad),
                (0, 1),
                "「{}」归一后是「{}」（不合法），应只报枚举越界",
                raw,
                canonical
            );
        }
    }
}

// ————————————————————— 验收 4：口径待补不猜 —————————————————————

#[test]
fn 官方未写明单位的指标不判单位是否匹配() {
    // MER-HR-01…03 是人均负担型，官方附件没写计量单位 → 标「口径待补」，
    // 本轮不判；猜一个单位会让科室按错口径填数。
    let rows = vec![RowSpec {
        indicator: "MER-HR-01".to_string(),
        name: "住院病案管理人员月均负担出院患者病历数".to_string(),
        unit: "例".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        unit_issues_of(&issues, "R1").is_empty(),
        "口径待补的指标不该判单位匹配：{:?}",
        issues
    );
    assert!(
        enum_issues_of(&issues, "R1").is_empty(),
        "「例」本身是合法取值：{:?}",
        issues
    );
}

#[test]
fn 指标编号不在册时不套单位口径() {
    // 编号不在官方 27 项之内 → 判不出该配什么单位 → 先让科室把编号改对
    let rows = vec![RowSpec {
        indicator: "MER-XX-99".to_string(),
        name: "不在册的指标".to_string(),
        unit: "例".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }];
    let conn = seed(&rows);
    let issues = run(&conn);

    assert!(
        unit_issues_of(&issues, "R1").is_empty(),
        "编号不在册时不该套猜出来的单位口径：{:?}",
        issues
    );
    let got = enum_issues_of(&issues, "R1");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].rule_id, "R-ENUM-004");
}

#[test]
fn 单位没填归必填缺失_不由单位检查器报() {
    // 比率型指标缺单位 → 必填缺失（条件必填），不是单位不符
    let conn = seed(&[RowSpec {
        unit: "".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    assert!(
        unit_issues_of(&issues, "R1").is_empty(),
        "没填不是写错，不该报单位不符：{:?}",
        issues
    );
    let got = issues_of(&issues, "R1");
    assert!(
        got.iter()
            .any(|i| i.issue_kind == "必填缺失"
                && i.missing_columns.contains(&"计量单位".to_string())),
        "缺单位归必填缺失：{:?}",
        got
    );
}

#[test]
fn 非比率型指标缺单位不报必填缺失() {
    let conn = seed(&[RowSpec {
        indicator: "MER-HR-01".to_string(),
        name: "住院病案管理人员月均负担出院患者病历数".to_string(),
        unit: "".to_string(),
        ..RowSpec::new("R1", "BK-01")
    }]);
    let issues = run(&conn);

    assert!(
        issues.is_empty(),
        "人均负担型指标官方未要求填单位，不该报：{:?}",
        issues
    );
}

// ————————————————————— 验收 5：别名表在口径层 —————————————————————

#[test]
fn 期望单位表来自官方二十七项登记表() {
    // 官方写明单位的 24 项比率型指标，单位都是 %
    let entries = indicators::expected_unit_entries();
    assert_eq!(entries.len(), 24, "官方写明单位的只有 24 项");
    for (code, _name, unit) in entries.iter() {
        assert_eq!(*unit, "%", "{} 的期望单位应是 %", code);
        assert!(indicators::is_ratio(code), "{} 应是比率型", code);
    }
    // 口径待补的三项不在期望单位表里
    let pending = indicators::pending_unit_codes();
    assert_eq!(pending.len(), 3);
    for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
        assert!(pending.contains(&code));
        assert!(
            !entries.iter().any(|(c, _, _)| *c == code),
            "{} 是口径待补，不该有期望单位",
            code
        );
    }
}

#[test]
fn 别名表随规则表下发_与口径层内置表逐条一致() {
    // 交付的规则表里带着别名表（运维改口径不用改代码）
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rules");
    let set = regval::rules::loader::load(&dir).expect("交付的规则表应能装载");
    let builtin_table = normalize::AliasTable::builtin();

    let mut checked = 0;
    for rule in set.executable() {
        let raw = match rule.params.get("unit_aliases") {
            Some(regval::rules::model::RuleParam::Text(t)) => t.clone(),
            _ => continue,
        };
        let loaded = normalize::AliasTable::from_json(&raw).expect("规则表里的别名表应能装载");
        assert_eq!(
            loaded.to_json(),
            builtin_table.to_json(),
            "规则 {} 的别名表与口径层内置表应逐条一致",
            rule.rule_id
        );
        checked += 1;
    }
    assert!(checked >= 2, "值域检查与匹配检查两条规则都应带别名表");
}

#[test]
fn 单位规则的口径_版本_依据齐备() {
    let rules = regval::rules::model::builtin_rules();
    let unit_rules: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.rule_kind == regval::rules::model::RuleKind::UnitCheck)
        .collect();
    assert_eq!(unit_rules.len(), 1, "单位匹配规则一条");
    let r = unit_rules[0];
    assert_eq!(r.rule_id, "R-UNIT-001");
    assert_eq!(r.version, "2021.1");
    assert_eq!(r.basis, UNIT_MATCH_BASIS);
    assert!(r.basis.contains("口径待补"), "依据要点明哪些指标口径待补");
    assert!(!r.params.is_empty());
}

// ————————————————————— 端到端 —————————————————————

#[test]
fn 端到端校验后契约里能看到单位不符() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unit_bad.csv"))
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

    // 样例表两行单位与指标不匹配（写「例」与写「例数」），一行编号不在册
    assert_eq!(contract["summary"]["issue_count_by_kind"]["单位不符"], 2);
    assert_eq!(contract["summary"]["issue_count_by_kind"]["枚举越界"], 1);

    // 单位不符的问题说明带期望单位
    let issues = contract["issues"].as_array().unwrap();
    let unit_issues: Vec<&serde_json::Value> = issues
        .iter()
        .filter(|i| i["issue_kind"].as_str() == Some("单位不符"))
        .collect();
    assert_eq!(unit_issues.len(), 2);
    assert!(unit_issues
        .iter()
        .all(|i| i["message"].as_str().unwrap_or("").contains("期望单位")));

    // 规则清单里有单位匹配这条口径，且与枚举越界分属两个槽位
    let rules = contract["rule_status"]["rules"].as_array().unwrap();
    let unit_rule = rules
        .iter()
        .find(|r| r["rule_id"].as_str() == Some("R-UNIT-001"))
        .expect("规则清单里应有 R-UNIT-001");
    assert_eq!(unit_rule["rule_kind"].as_str(), Some("单位不符"));
    assert_eq!(unit_rule["check_column"].as_str(), Some("unit"));
    assert!(!unit_rule["basis"].as_str().unwrap_or("").is_empty());

    // 全角百分号那一行通过（归一后就是 %），口径待补那一行不判
    let rows = contract["rows"].as_array().unwrap();
    let row_conclusion = |biz: &str| -> String {
        rows.iter()
            .find(|r| r["business_key"].as_str() == Some(biz))
            .map(|r| r["conclusion"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    assert_eq!(row_conclusion("BK-UNIT-01"), "通过");
    assert_eq!(
        row_conclusion("BK-UNIT-03"),
        "通过",
        "全角百分号归一后是 %，应通过"
    );
    assert_eq!(
        row_conclusion("BK-UNIT-05"),
        "通过",
        "口径待补的指标不判单位匹配"
    );
    assert_eq!(row_conclusion("BK-UNIT-02"), "不通过");
    assert_eq!(row_conclusion("BK-UNIT-04"), "不通过");
}
