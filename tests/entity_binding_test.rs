//! 实体绑定层（任务 6）的验收：
//! ① 同权威键异值 → 整组标「待人工判定」，绝不二选一合并；
//! ② 同权威键同值 → 一行权威 + 其余「疑似重复」，交重复检测漏斗；
//! ③ 行键两次 ingest 一致；行序变化后仍按业务主键对齐；
//! ④ 跨表同指标同科室行按匹配键对齐；
//! ⑤ 科室代码匹配不到 HIS 目录 → 报「科室主数据不一致」（`DEPT_NOT_FOUND`）；
//!    目录不可用 → 标「科室主数据未校验」，不静默通过；
//! ⑥ 端到端：一次 `regval validate` 之后，契约与本机库的归属都对得上。

use std::collections::BTreeMap;
use std::path::PathBuf;

use regval::bind::dept::{InMemoryDirectory, UnavailableDirectory};
use regval::bind::{self, AuthorityState};
use regval::db::repo::Repo;
use regval::ingest::key;
use regval::ingest::{Field, SourceRow};

/// 造一行来源行
fn row(
    source_ns: &str,
    row_no: usize,
    indicator: &str,
    dept: &str,
    business_key: &str,
    value: &str,
) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, indicator.to_string());
    values.insert(Field::IndicatorName, "病案首页完整率".to_string());
    values.insert(Field::DeptCode, dept.to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, business_key.to_string());
    values.insert(Field::Numerator, "10".to_string());
    values.insert(Field::Denominator, "20".to_string());
    values.insert(Field::Value, value.to_string());
    values.insert(Field::Unit, "%".to_string());
    SourceRow {
        source_ns: source_ns.to_string(),
        source_file: format!("{}.csv", source_ns),
        row_no,
        dup_ordinal: 0,
        values,
    }
}

fn bind_rows(rows: &mut Vec<SourceRow>) -> bind::BindingOutcome {
    key::assign_dup_ordinals(rows);
    bind::bind(rows, &UnavailableDirectory)
}

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn 同权威键异值不自动合并且整组标待人工判定() {
    let mut rows = vec![
        row("科室一表", 1, "MER-HR-01", "0301", "BK-01", "50"),
        row("科室二表", 4, "MER-HR-01", "0301", "BK-01", "60"),
    ];
    let out = bind_rows(&mut rows);

    // 整组都待人工判定 —— 没有任何一行被当成权威行
    assert!(
        out.rows
            .iter()
            .all(|r| r.state == AuthorityState::PendingManual),
        "同键异值必须整组待人工判定，实际 {:?}",
        out.rows.iter().map(|r| r.state).collect::<Vec<_>>()
    );
    assert_eq!(out.issues.len(), 2, "每行一条问题，经办人才知道要判哪两行");
    for issue in &out.issues {
        assert_eq!(issue.issue_kind, "待人工判定");
        assert_eq!(issue.severity, "阻断", "待人工判定的行不能被自动上报");
        assert_eq!(issue.error_code, "AUTHORITY_KEY_CONFLICT");
        assert!(!issue.basis.is_empty(), "每条问题都要带依据");
        assert!(
            issue.message.contains("未自动合并"),
            "问题说明要写清引擎没有替业务做决定：{}",
            issue.message
        );
        assert!(
            issue.message.contains("科室二表 第 4 行")
                || issue.message.contains("科室一表 第 1 行"),
            "问题说明要点名对照行：{}",
            issue.message
        );
    }
}

#[test]
fn 同权威键同值时一行权威其余标疑似重复() {
    let mut rows = vec![
        row("科室一表", 1, "MER-HR-01", "0301", "BK-02", "50"),
        row("科室二表", 7, "MER-HR-01", "0301", "BK-02", "50"),
    ];
    let out = bind_rows(&mut rows);

    let states: Vec<AuthorityState> = out.rows.iter().map(|r| r.state).collect();
    assert_eq!(
        states
            .iter()
            .filter(|s| **s == AuthorityState::Authoritative)
            .count(),
        1,
        "同权威键只能有一行权威数据"
    );
    assert_eq!(
        states
            .iter()
            .filter(|s| **s == AuthorityState::Duplicate)
            .count(),
        1
    );
    assert!(out.issues.is_empty(), "取值完全一致不是口径问题");
    // 「疑似重复项」列两行都要标出来，经办人才看得出是一对
    assert_eq!(out.suspected.len(), 2);
    assert!(out
        .suspected
        .values()
        .all(|text| text.contains("疑似重复") && text.contains("取值完全一致")));
}

#[test]
fn 数值写法不同但数值相同视为一致() {
    let mut a = row("科室一表", 1, "MER-HR-01", "0301", "BK-03", "50");
    let mut b = row("科室二表", 1, "MER-HR-01", "0301", "BK-03", "50.0");
    a.values.insert(Field::Value, "50".to_string());
    b.values.insert(Field::Value, "50.0".to_string());
    let mut rows = vec![a, b];
    let out = bind_rows(&mut rows);
    assert!(
        out.rows
            .iter()
            .all(|r| r.state != AuthorityState::PendingManual),
        "12 与 12.0 是同一个数，不该报待人工判定"
    );
}

#[test]
fn 行键两次ingest一致且行序变化后仍按业务主键对齐() {
    let mut first = vec![
        row("科室一表", 1, "MER-HR-01", "0301", "BK-01", "50"),
        row("科室一表", 2, "MER-HR-01", "0301", "BK-02", "60"),
    ];
    let mut again = first.clone();
    key::assign_dup_ordinals(&mut first);
    key::assign_dup_ordinals(&mut again);

    // 同一份输入两次 ingest → 行键一致
    for (a, b) in first.iter().zip(again.iter()) {
        assert_eq!(
            key::row_uid(a),
            key::row_uid(b),
            "同一份输入两次 ingest 行键必须一致"
        );
    }

    // 行序变了（同一批数据换了个排法）→ 行键不变，仍按业务主键对齐
    let mut reordered = vec![
        row("科室一表", 1, "MER-HR-01", "0301", "BK-02", "60"),
        row("科室一表", 2, "MER-HR-01", "0301", "BK-01", "50"),
    ];
    key::assign_dup_ordinals(&mut reordered);
    let uid_of = |rows: &[SourceRow], bk: &str| {
        rows.iter()
            .find(|r| r.get_trimmed(Field::BusinessKey) == Some(bk))
            .map(key::row_uid)
            .unwrap()
    };
    assert_eq!(uid_of(&first, "BK-01"), uid_of(&reordered, "BK-01"));
    assert_eq!(uid_of(&first, "BK-02"), uid_of(&reordered, "BK-02"));

    // 行序变了仍然能按业务主键对齐到同一条记录
    let out = bind_rows(&mut reordered);
    let records = &out.aligned["MER-HR-01|0301|2026-Q3"];
    assert_eq!(records.len(), 2, "两条业务记录各归其位");
}

#[test]
fn 跨表同指标同科室行按匹配键对齐() {
    let mut rows = vec![
        row("科室一表", 1, "MER-HR-01", "0301", "BK-01", "50"),
        row("科室二表", 3, "MER-HR-01", "0301", "BK-01", "50"),
        row("科室二表", 5, "MER-HR-01", "0301", "BK-09", "70"),
    ];
    let out = bind_rows(&mut rows);

    let records = &out.aligned["MER-HR-01|0301|2026-Q3"];
    assert_eq!(records.len(), 2, "BK-01 与 BK-09 是两条记录");
    let cross: Vec<_> = records.iter().filter(|r| r.cross_table).collect();
    assert_eq!(cross.len(), 1, "只有 BK-01 跨了两张表");
    assert_eq!(cross[0].occurrences.len(), 2);
    assert!(cross[0].occurrences[0].contains("科室一表"));
    assert!(cross[0].occurrences[1].contains("科室二表"));
    assert_eq!(out.cross_table_record_count(), 1);
}

#[test]
fn 科室代码匹配不到目录时报科室主数据不一致() {
    let mut rows = vec![row("科室一表", 1, "MER-HR-01", "0999", "BK-01", "50")];
    key::assign_dup_ordinals(&mut rows);
    let dir = InMemoryDirectory::new(["0301", "0302"]);
    let out = bind::bind(&rows, &dir);

    assert!(out.dept_master_checked, "目录可用时应真的核对过");
    assert_eq!(out.dept_unmatched, vec!["0999".to_string()]);
    assert_eq!(out.issues.len(), 1);
    let issue = &out.issues[0];
    assert_eq!(issue.issue_kind, "科室主数据不一致");
    assert_eq!(issue.error_code, "DEPT_NOT_FOUND");
    assert!(!issue.basis.is_empty());
}

#[test]
fn 目录不可用时标科室主数据未校验而不是静默通过() {
    let mut rows = vec![row("科室一表", 1, "MER-HR-01", "0999", "BK-01", "50")];
    key::assign_dup_ordinals(&mut rows);
    let out = bind::bind(&rows, &UnavailableDirectory);

    assert!(!out.dept_master_checked, "目录拉不到不等于科室没问题");
    assert!(out.issues.is_empty());
    assert!(out.dept_unmatched.is_empty());
}

#[test]
fn 端到端校验后契约与本机库的归属都对得上() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(project_dir().join("tests/fixtures/dup_rows.csv"))
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

    // 5 行全部落进结果
    let rows = contract["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);

    // 同键异值的两行：待人工判定 + 不通过
    let pending: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| {
            r["issue_types"]
                .as_str()
                .unwrap_or("")
                .contains("待人工判定")
        })
        .collect();
    assert_eq!(pending.len(), 2, "同键异值的两行都要标待人工判定");
    assert!(pending
        .iter()
        .all(|r| r["conclusion"].as_str() == Some("不通过")));
    assert!(pending
        .iter()
        .all(|r| r["issue_detail"].as_str().unwrap_or("").contains("依据")));

    // 同键同值的两行：疑似重复项列有值
    let suspected: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| !r["suspected_duplicate"].as_str().unwrap_or("").is_empty())
        .collect();
    assert_eq!(suspected.len(), 2, "一对疑似重复都要标出来");
    assert!(suspected.iter().all(|r| r["suspected_duplicate"]
        .as_str()
        .unwrap()
        .contains("疑似重复")));

    // 问题计数里能看到「待人工判定」
    assert_eq!(contract["summary"]["issue_count_by_kind"]["待人工判定"], 2);

    // 科室目录未接入 → 如实标「科室主数据未校验」，不静默通过
    assert_eq!(contract["summary"]["dept_master_checked"], false);
    assert_eq!(
        contract["summary"]["dept_master_note"].as_str(),
        Some("科室主数据未校验")
    );

    // 本机库里的权威归属与契约对得上
    let repo = Repo::open(&state_db).unwrap();
    let batch_ref = contract["batch_ref"].as_str().unwrap();
    let mut stmt = repo
        .conn()
        .prepare("SELECT authority_state, COUNT(*) FROM rows WHERE batch_ref = ?1 GROUP BY authority_state")
        .unwrap();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mapped = stmt
        .query_map(rusqlite::params![batch_ref], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .unwrap();
    for row in mapped {
        let (state, n) = row.unwrap();
        counts.insert(state, n);
    }
    assert_eq!(
        counts.get("pending_manual"),
        Some(&2),
        "同键异值两行待人工判定"
    );
    assert_eq!(
        counts.get("authoritative"),
        Some(&2),
        "BK-02 与 BK-03 各一行权威"
    );
    assert_eq!(
        counts.get("duplicate"),
        Some(&1),
        "BK-02 的另一行标疑似重复"
    );
    assert_eq!(counts.get("raw"), None, "不该留下归属未判定的行");
}
