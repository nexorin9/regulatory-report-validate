//! 重复检测四级漏斗 · 第一级（确定性键，L1）的验收：
//! ① 取值完全一致的行被检出并标「疑似重复」；
//! ② 取值不同的同键行**不被本级误判**（归实体绑定层的「待人工判定」）；
//! ③ 候选对带行键、命中级别与依据；
//! ④ 只标疑似 —— 不合并、不推送、不推进任何状态；
//! ⑤ 单位写法不同经归一后判为一致（归一表与单位检查器共用同一份）；
//! ⑥ 键要素不全的行不参与判定；
//! ⑦ 端到端：结果契约的 `suspected_duplicates` 段 + 「疑似重复项」列 + 摘要计数。

use std::collections::BTreeMap;
use std::path::PathBuf;

use regval::bind::dept::UnavailableDirectory;
use regval::bind::{self, AuthorityState};
use regval::db::repo::Repo;
use regval::dupe::exact::{self, MATCH_LEVEL_EXACT};
use regval::dupe::SuspectedDuplicate;
use regval::ingest::key;
use regval::ingest::{Field, SourceRow};

/// 造一行来源行
fn row(source_ns: &str, row_no: usize, business_key: &str, value: &str, unit: &str) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::IndicatorName, "病案首页24小时内完成率".to_string());
    values.insert(Field::DeptCode, "0301".to_string());
    values.insert(Field::DeptName, "病案统计室".to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, business_key.to_string());
    values.insert(Field::Numerator, "980".to_string());
    values.insert(Field::Denominator, "1000".to_string());
    values.insert(Field::Value, value.to_string());
    values.insert(Field::Unit, unit.to_string());
    SourceRow {
        source_ns: source_ns.to_string(),
        source_file: format!("{}.csv", source_ns),
        row_no,
        dup_ordinal: 0,
        values,
    }
}

/// 编号同键行序后跑 L1
fn detect(rows: &mut Vec<SourceRow>) -> exact::ExactOutcome {
    key::assign_dup_ordinals(rows);
    exact::detect(rows)
}

/// 编号同键行序后跑实体绑定层
fn bind(rows: &mut Vec<SourceRow>) -> bind::BindingOutcome {
    key::assign_dup_ordinals(rows);
    bind::bind(rows, &UnavailableDirectory)
}

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 候选对里的两行行键
fn pair_keys(pair: &SuspectedDuplicate) -> (&str, &str) {
    (pair.left_row_key.as_str(), pair.right_row_key.as_str())
}

#[test]
fn 取值完全一致的行被检出并标疑似() {
    let mut rows = vec![
        row("科室一表", 1, "BK-01", "98", "%"),
        row("科室二表", 7, "BK-01", "98", "%"),
    ];
    let out = detect(&mut rows);

    assert_eq!(out.pairs.len(), 1, "完全一致的两行应构成一对");
    let pair = &out.pairs[0];
    assert_eq!(pair.match_level, MATCH_LEVEL_EXACT);
    assert_eq!(pair.state, "疑似重复", "只标疑似，不合并");
    assert!(!pair.basis.is_empty(), "每对都要带依据");
    assert_eq!(pair.business_key, "MER-TL-04|0301|2026-Q3|BK-01");
    assert!(pair.cross_table, "两张来源表各填了一次");
    assert_ne!(
        pair.left_row_key, pair.right_row_key,
        "一对必须是两行，不能是同一行"
    );
    // 两行都要在「疑似重复项」列里标出对方
    assert_eq!(out.suspected.len(), 2);
    for text in out.suspected.values() {
        assert!(text.contains("疑似重复"), "标注：{}", text);
        assert!(text.contains("取值完全一致"), "标注：{}", text);
    }
}

#[test]
fn 取值不同的同键行不被本级误判为重复() {
    let mut rows = vec![
        row("科室一表", 1, "BK-01", "98", "%"),
        row("科室二表", 7, "BK-01", "60", "%"),
    ];
    let out = detect(&mut rows);

    assert!(
        out.pairs.is_empty(),
        "取值不同不能判成疑似重复：{:?}",
        out.pairs
    );
    assert_eq!(out.stats.conflicting_rows, 2);
    assert_eq!(out.stats.conflicting_groups, 1);
    assert!(
        out.suspected.is_empty(),
        "同键异值不标「疑似重复」，它归待人工判定"
    );

    // 这两行由实体绑定层判「待人工判定」，本级不越权
    let out = bind(&mut rows);
    assert!(out
        .rows
        .iter()
        .all(|r| r.state == AuthorityState::PendingManual));
    assert!(out.issues.iter().all(|i| i.issue_kind == "待人工判定"));
}

#[test]
fn 漏斗与实体绑定层对同一对行结论一致() {
    // 两层共用同一份取值签名 —— 同一对行不会一处说疑似重复、一处说待人工判定
    let mut rows = vec![
        row("科室一表", 1, "BK-01", "98", "%"),
        row("科室二表", 7, "BK-01", "98", "%"),
        row("科室三表", 2, "BK-02", "98", "%"),
        row("科室三表", 3, "BK-02", "60", "%"),
    ];
    let detected = detect(&mut rows);
    let bound = bind(&mut rows);

    // L1 命中的两行都不是「待人工判定」
    for pair in &detected.pairs {
        for r in &bound.rows {
            if r.keys.row_uid == pair.left_row_key || r.keys.row_uid == pair.right_row_key {
                assert_ne!(
                    r.state,
                    AuthorityState::PendingManual,
                    "L1 判为完全一致的行，绑定层不能同时判待人工判定"
                );
            }
        }
    }
    // 待人工判定的行不出现在 L1 候选对里
    let pending: Vec<&str> = bound
        .rows
        .iter()
        .filter(|r| r.state == AuthorityState::PendingManual)
        .map(|r| r.keys.row_uid.as_str())
        .collect();
    for pair in &detected.pairs {
        let (l, r) = pair_keys(pair);
        assert!(!pending.contains(&l) && !pending.contains(&r));
    }
}

#[test]
fn 单位写法不同经归一后判为一致() {
    let mut rows = vec![
        row("科室一表", 1, "BK-01", "98", "%"),
        row("科室二表", 7, "BK-01", "98", "％"),
    ];
    let out = detect(&mut rows);
    assert_eq!(
        out.pairs.len(),
        1,
        "全角百分号与半角百分号是同一个单位，不该判成待人工判定"
    );

    // 「例数」与「例」同理（单位别名表与单位检查器共用同一份）
    let mut rows = vec![
        row("科室一表", 1, "BK-02", "98", "例数"),
        row("科室二表", 7, "BK-02", "98", "例"),
    ];
    let out = detect(&mut rows);
    assert_eq!(out.pairs.len(), 1, "例数与例是同一个单位");
}

#[test]
fn 键要素不全的行不参与判定() {
    let mut rows = vec![
        row("科室一表", 1, "BK-01", "98", "%"),
        row("科室二表", 7, "", "98", "%"),
    ];
    let out = detect(&mut rows);
    assert!(out.pairs.is_empty());
    assert_eq!(out.stats.skipped_rows, 1, "缺键要素的行不参与");
    assert_eq!(out.stats.eligible_rows, 1);
}

#[test]
fn 只标疑似_不合并不推送() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(project_dir().join("tests/fixtures/dupe_exact.csv"))
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

    let contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    // 疑似重复的两对进了结果契约
    let pairs = contract["suspected_duplicates"].as_array().unwrap();
    assert_eq!(pairs.len(), 2, "BK-DUPE-01 与 BK-DUPE-02 各一对");
    for pair in pairs {
        assert_eq!(pair["match_level"], MATCH_LEVEL_EXACT);
        assert!(!pair["basis"].as_str().unwrap().is_empty());
        assert!(!pair["left_row_key"].as_str().unwrap().is_empty());
        assert!(!pair["right_row_key"].as_str().unwrap().is_empty());
        assert_eq!(pair["state"], "疑似重复");
    }
    assert_eq!(contract["summary"]["suspected_duplicate_count"], 2);

    // 「疑似重复项」列只标那四行
    let rows = contract["rows"].as_array().unwrap();
    let marked: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| !r["suspected_duplicate"].as_str().unwrap_or("").is_empty())
        .collect();
    assert_eq!(marked.len(), 4, "两对共四行都要标出疑似对象");
    for r in &marked {
        assert!(r["suspected_duplicate"]
            .as_str()
            .unwrap()
            .contains("疑似重复"));
    }

    // 同键异值的两行走「待人工判定」，不混进疑似重复
    let pending: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| {
            r["issue_types"]
                .as_str()
                .unwrap_or("")
                .contains("待人工判定")
        })
        .collect();
    assert_eq!(pending.len(), 2);
    for r in &pending {
        assert!(
            r["suspected_duplicate"].as_str().unwrap_or("").is_empty(),
            "待人工判定的行不能同时被标成疑似重复"
        );
        assert_eq!(r["conclusion"], "不通过");
    }

    // 只标疑似 —— 不合并、不推送：行没少、状态没推进、没有任何上报记录
    assert_eq!(rows.len(), 7, "7 行一行没少，没有被合并掉");
    for r in rows {
        assert_eq!(r["submission_status"], "未上报", "上报状态不由本级推进");
    }
    let repo = Repo::open(&state_db).unwrap();
    let count = |sql: &str| -> i64 {
        repo.conn()
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(count("SELECT COUNT(*) FROM submissions"), 0, "没有上报提交");
    assert_eq!(count("SELECT COUNT(*) FROM receipts"), 0, "没有回执");
    assert_eq!(
        count("SELECT COUNT(*) FROM confirmations"),
        0,
        "没有人工确认"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM rows WHERE authority_state = 'pending_manual'"),
        2,
        "同键异值的两行仍是待人工判定"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM rows WHERE authority_state = 'duplicate'"),
        2,
        "取值一致的两行标疑似重复，不合并"
    );
}

#[test]
fn 端到端_契约版本不变且候选对可解析() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(project_dir().join("tests/fixtures/dupe_exact.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .output()
        .expect("regval 应可执行");
    assert!(output.status.success());

    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: serde_json::Value = serde_json::from_str(&raw).unwrap();

    // 追加疑似重复段不升契约版本 —— 写回原表的 5 个新列结构没变，插件读法不变
    assert_eq!(contract["schema_version"], "regval.contract/1.0.0");

    // 候选对能按行键回到具体行
    let rows = contract["rows"].as_array().unwrap();
    for pair in contract["suspected_duplicates"].as_array().unwrap() {
        let left = pair["left_row_key"].as_str().unwrap();
        let right = pair["right_row_key"].as_str().unwrap();
        assert!(
            rows.iter().any(|r| r["row_uid"] == left),
            "左行键要能定位到行"
        );
        assert!(
            rows.iter().any(|r| r["row_uid"] == right),
            "右行键要能定位到行"
        );
    }

    // stdout 里疑似重复的件数单独报出来（与口径问题分开计数）
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("已校验 7 行"), "stdout={}", stdout);
    assert!(
        stdout.contains("2 项疑似重复待确认"),
        "疑似重复件数要单独报出来：{}",
        stdout
    );
}
