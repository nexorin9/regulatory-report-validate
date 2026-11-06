//! 校验库完整 schema 与迁移的验收：
//! ① 迁移连跑两次结构一致（表结构与索引集合相同）；② 权威键重复被唯一索引拒绝；
//! ③ 损坏库打开时报错，不静默重建空库；④ 六张业务表齐备；⑤ 幂等键 ↔ 回执 ID 一一对应。

use std::collections::BTreeMap;

use regval::db::repo::Repo;
use regval::db::schema;
use regval::errors::ErrorCode;
use regval::ingest::{Field, SourceRow};

/// 造一行最小来源行
fn row(
    source_ns: &str,
    row_no: usize,
    business_key: &str,
    dept: &str,
    indicator: &str,
) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, indicator.to_string());
    values.insert(Field::IndicatorName, "示例指标".to_string());
    values.insert(Field::DeptCode, dept.to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, business_key.to_string());
    values.insert(Field::Unit, "%".to_string());
    values.insert(Field::Numerator, "10".to_string());
    values.insert(Field::Denominator, "20".to_string());
    values.insert(Field::Value, "50".to_string());
    SourceRow {
        source_ns: source_ns.to_string(),
        source_file: format!("{}.csv", source_ns),
        row_no,
        dup_ordinal: 0,
        values,
    }
}

#[test]
fn 迁移连跑两次表结构与索引集合一致() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");

    let repo = Repo::open(&db).unwrap();
    let tables_1 = schema::table_names(repo.conn()).unwrap();
    let indexes_1 = schema::index_names(repo.conn()).unwrap();
    let columns_1 = schema::column_names(repo.conn(), "rows").unwrap();
    drop(repo);

    // 第二次打开 = 再跑一遍迁移：版本已最新，什么都不该变
    let repo2 = Repo::open(&db).unwrap();
    assert_eq!(
        schema::table_names(repo2.conn()).unwrap(),
        tables_1,
        "第二次打开后表清单必须完全一致"
    );
    assert_eq!(
        schema::index_names(repo2.conn()).unwrap(),
        indexes_1,
        "第二次打开后索引清单必须完全一致"
    );
    assert_eq!(
        schema::column_names(repo2.conn(), "rows").unwrap(),
        columns_1,
        "第二次打开后 rows 列定义必须完全一致"
    );
    assert_eq!(
        schema::current_version(repo2.conn()).unwrap(),
        schema::SCHEMA_VERSION,
        "迁移后版本必须等于 SCHEMA_VERSION"
    );
}

#[test]
fn 六张业务表齐备() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let tables = schema::table_names(repo.conn()).unwrap();
    for want in [
        "rows",
        "issues",
        "confirmations",
        "audit_log",
        "receipts",
        "submissions",
    ] {
        assert!(
            tables.iter().any(|t| t == want),
            "缺业务表 {}；实际有 {:?}",
            want,
            tables
        );
    }
}

#[test]
fn 权威键重复被唯一索引拒绝() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    // 两行权威键完全相同（机构+报告期+指标+科室+业务主键），来源表不同
    let a = row("表A", 1, "BK-01", "0301", "MER-TQ-01");
    let b = row("表B", 1, "BK-01", "0301", "MER-TQ-01");
    repo.insert_rows("batch-1", &[a.clone(), b.clone()])
        .unwrap();

    let uid_a = regval::ingest::row_uid(&a);
    let uid_b = regval::ingest::row_uid(&b);

    // 原始来源行都在库里（重复检测漏斗要逐对比对两行取值，不能在落库时丢掉一行）
    assert_eq!(repo.count_rows("batch-1").unwrap(), 2);

    // 提升第一行为权威行：成功
    repo.promote_authoritative("batch-1", &uid_a).unwrap();
    assert_eq!(
        repo.authority_state(&uid_a).unwrap().as_deref(),
        Some("authoritative")
    );

    // 提升第二行为权威行：被唯一索引 idx_rows_authority 拒掉
    let err = repo.promote_authoritative("batch-1", &uid_b).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::AuthorityKeyConflict,
        "权威键重复必须报 AUTHORITY_KEY_CONFLICT，实际 {:?}",
        err.code
    );
    assert!(
        err.message.contains("权威键重复"),
        "错误说明要写清是权威键重复：{}",
        err.message
    );
    // 被拒的那行状态不变，仍是 raw（没有半个状态）
    assert_eq!(
        repo.authority_state(&uid_b).unwrap().as_deref(),
        Some("raw")
    );
}

#[test]
fn 权威键唯一索引在库层面直接拦重复() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let conn = repo.conn();

    conn.execute(
        "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                            authority_state, indicator_code, reporting_period, dept_code,
                            business_key, created_at)
         VALUES ('b1', 'ns1', 'a.csv', 1, 'uid-1', 'ORG-001', 'authoritative',
                 'MER-TQ-01', '2026-Q3', '0301', 'BK-01', '2026-11-06 10:00:00')",
        [],
    )
    .unwrap();

    // 同权威键再来一行权威行 → 唯一索引直接拒绝
    let err = conn
        .execute(
            "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                                authority_state, indicator_code, reporting_period, dept_code,
                                business_key, created_at)
             VALUES ('b1', 'ns2', 'b.csv', 1, 'uid-2', 'ORG-001', 'authoritative',
                     'MER-TQ-01', '2026-Q3', '0301', 'BK-01', '2026-11-06 10:00:00')",
            [],
        )
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("UNIQUE constraint failed"),
        "重复权威键必须被唯一索引拒绝，实际报错：{}",
        msg
    );
    // SQLite 报的是被约束的列 —— 权威键五列都要在报错里
    for col in [
        "org_code",
        "reporting_period",
        "indicator_code",
        "dept_code",
        "business_key",
    ] {
        assert!(
            msg.contains(col),
            "报错要指名权威键被约束的列 {}：{}",
            col,
            msg
        );
    }
}

#[test]
fn 损坏库打开时报错而不是静默重建空库() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("broken.db");
    let garbage = "这不是一个 SQLite 库文件，只是被误写到状态库路径上的普通文本。";
    std::fs::write(&db, garbage).unwrap();

    let err = Repo::open(&db).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::DbCorrupted,
        "损坏库必须报 DB_CORRUPTED，实际 {:?}",
        err.code
    );
    assert!(
        err.to_string().contains("状态库"),
        "错误说明要让经办人看得出是状态库坏了：{}",
        err
    );

    // 原文件没被动过：既没被删，也没被重建成空库
    assert_eq!(
        std::fs::read_to_string(&db).unwrap(),
        garbage,
        "损坏库不得被静默重建或改写"
    );
}

#[test]
fn 版本高于引擎的库拒绝打开() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("future.db");
    {
        let repo = Repo::open(&db).unwrap();
        repo.conn()
            .execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    schema::SCHEMA_VERSION + 7,
                    "from_the_future",
                    "2099-01-01 00:00:00"
                ],
            )
            .unwrap();
    }
    let err = Repo::open(&db).unwrap_err();
    assert_eq!(err.code, ErrorCode::DbVersionUnsupported);
}

#[test]
fn 老库在迁移台账之前建的也能按序迁移到最新() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("legacy.db");
    {
        // 模拟 Task 5 之前的老库：有 rows/issues/run_summaries，没有 org_code / authority_state，
        // 也没有 schema_migrations 台账
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE rows (
                 id INTEGER PRIMARY KEY, batch_ref TEXT NOT NULL, source_ns TEXT NOT NULL,
                 source_file TEXT NOT NULL, row_no INTEGER NOT NULL, row_uid TEXT NOT NULL,
                 indicator_code TEXT, indicator_name TEXT, dept_code TEXT, dept_name TEXT,
                 reporting_period TEXT, business_key TEXT, numerator REAL, denominator REAL,
                 value REAL, unit TEXT, remark TEXT, numeric_raw TEXT, rule_version TEXT,
                 created_at TEXT NOT NULL);
             CREATE TABLE issues (
                 id INTEGER PRIMARY KEY, batch_ref TEXT NOT NULL, row_uid TEXT NOT NULL,
                 rule_id TEXT NOT NULL, issue_kind TEXT NOT NULL, severity TEXT NOT NULL,
                 message TEXT NOT NULL, basis TEXT NOT NULL, issue_key TEXT NOT NULL DEFAULT '',
                 missing_columns TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
                 UNIQUE (batch_ref, row_uid, rule_id, issue_key));
             CREATE TABLE run_summaries (
                 id INTEGER PRIMARY KEY, command TEXT NOT NULL, batch_ref TEXT NOT NULL,
                 tables INTEGER NOT NULL, total_rows INTEGER NOT NULL,
                 rows_with_issues INTEGER NOT NULL, issue_count INTEGER NOT NULL,
                 elapsed_ms INTEGER NOT NULL, started_at TEXT NOT NULL, finished_at TEXT NOT NULL,
                 created_at TEXT NOT NULL);",
        )
        .unwrap();
    }

    let repo = Repo::open(&db).unwrap();
    assert_eq!(
        schema::current_version(repo.conn()).unwrap(),
        schema::SCHEMA_VERSION
    );
    let tables = schema::table_names(repo.conn()).unwrap();
    for want in ["confirmations", "audit_log", "receipts", "submissions"] {
        assert!(
            tables.iter().any(|t| t == want),
            "老库迁移后应补上 {} 表",
            want
        );
    }
    let columns = schema::column_names(repo.conn(), "rows").unwrap();
    assert!(
        columns.iter().any(|c| c == "org_code"),
        "老库迁移后 rows 应补上 org_code 列，实际 {:?}",
        columns
    );
    assert!(
        columns.iter().any(|c| c == "authority_state"),
        "老库迁移后 rows 应补上 authority_state 列，实际 {:?}",
        columns
    );
}

#[test]
fn 幂等键与回执ID一一对应() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let conn = repo.conn();

    conn.execute(
        "INSERT INTO submissions (batch_ref, row_uid, idempotency_key, payload_digest, revision,
                                  status, created_at, updated_at)
         VALUES ('b1', 'uid-1', 'IDEM-1', 'digest-1', 1, 'pending', '2026-11-06 10:00:00', '2026-11-06 10:00:00')",
        [],
    )
    .unwrap();
    // 同幂等键同修订号再来一次 → 唯一索引拒绝
    let err = conn
        .execute(
            "INSERT INTO submissions (batch_ref, row_uid, idempotency_key, payload_digest, revision,
                                      status, created_at, updated_at)
             VALUES ('b1', 'uid-2', 'IDEM-1', 'digest-1', 1, 'pending', '2026-11-06 10:00:00', '2026-11-06 10:00:00')",
            [],
        )
        .unwrap_err();
    assert!(format!("{}", err).contains("UNIQUE constraint failed"));

    conn.execute(
        "INSERT INTO receipts (receipt_id, idempotency_key, revision, batch_ref, row_uid, status, created_at)
         VALUES ('RCPT-1', 'IDEM-1', 1, 'b1', 'uid-1', 'accepted', '2026-11-06 10:00:00')",
        [],
    )
    .unwrap();
    // 同一 (幂等键, 修订号) 的第二条回执 → 拒绝（一一对应）
    let err = conn
        .execute(
            "INSERT INTO receipts (receipt_id, idempotency_key, revision, batch_ref, row_uid, status, created_at)
             VALUES ('RCPT-2', 'IDEM-1', 1, 'b1', 'uid-1', 'accepted', '2026-11-06 10:00:00')",
            [],
        )
        .unwrap_err();
    assert!(format!("{}", err).contains("UNIQUE constraint failed"));

    // 回执 ID 自身唯一：换个修订号也不能复用同一个 receipt_id
    let err = conn
        .execute(
            "INSERT INTO receipts (receipt_id, idempotency_key, revision, batch_ref, row_uid, status, created_at)
             VALUES ('RCPT-1', 'IDEM-1', 2, 'b1', 'uid-1', 'accepted', '2026-11-06 10:00:00')",
            [],
        )
        .unwrap_err();
    assert!(format!("{}", err).contains("UNIQUE constraint failed"));

    // 订正后新修订号可以并存，旧回执仍可查
    conn.execute(
        "INSERT INTO submissions (batch_ref, row_uid, idempotency_key, payload_digest, revision,
                                  status, created_at, updated_at)
         VALUES ('b1', 'uid-1', 'IDEM-1', 'digest-2', 2, 'pending', '2026-11-06 11:00:00', '2026-11-06 11:00:00')",
        [],
    )
    .unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM submissions WHERE idempotency_key = 'IDEM-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "订正后应保留两个修订号");
}

#[test]
fn 落库行带机构代码与权威归属状态() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let r = row("表A", 1, "BK-01", "0301", "MER-TQ-01");
    repo.insert_rows("batch-1", &[r.clone()]).unwrap();

    let (org, state): (String, String) = repo
        .conn()
        .query_row(
            "SELECT org_code, authority_state FROM rows WHERE row_uid = ?1",
            rusqlite::params![regval::ingest::row_uid(&r)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(org, "ORG-001");
    assert_eq!(state, "raw", "刚落库的行权威归属未判定");
}
