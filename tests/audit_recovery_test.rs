//! 审计链与状态库恢复的验收测试（Task 37）。
//!
//! 验收点：
//! 1. **按行回放**出完整确认与推送轨迹（谁确认的 → 什么时候推的 → 拿到哪个回执）；
//! 2. **按批次回放**这一批推了什么；
//! 3. **幂等键 ↔ 回执 ID 一一对应**成立，且破口能被查出来；
//! 4. 审计链**只追加不修改**，且只含行级 / 科室级数据（不落业务明细）；
//! 5. **状态库损坏后按幂等键与回执记录恢复**，恢复后对应关系不丢，
//!    并给出恢复报告（恢复到哪一批、丢了什么）。
//!
//! 数据全部是脱敏的指标级监管样例（无患者可识别明细）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regval::db::journal;
use regval::db::recover::{self, RecoveryOutcome};
use regval::db::repo::{Repo, SubmissionIdentity};
use regval::ingest::{self, Field, SourceRow};
use regval::push::receipt::{self, SubmitIdentity, Verdict};
use regval::state::audit;
use regval::state::confirm::{self, ConfirmRequest};
use regval::state::machine::{self, RowState, StateChange};

const BATCH: &str = "batch-2026-Q3";
const BATCH2: &str = "batch-2026-Q4";
const IDEM_KEY: &str = "idem-sha256-0001";
const RECEIPT_ID: &str = "RCPT-2026-0001";

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn open_repo(dir: &Path) -> Repo {
    Repo::open(&dir.join("state.db")).expect("状态库应可打开")
}

/// 造一行科室上报表（指标级监管数据，无患者可识别明细）
fn source_row(ns: &str, key: &str) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::IndicatorName, "病案首页完整率".to_string());
    values.insert(Field::DeptCode, "0301".to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, key.to_string());
    values.insert(Field::Numerator, "980".to_string());
    values.insert(Field::Denominator, "1000".to_string());
    values.insert(Field::Value, "98".to_string());
    values.insert(Field::Unit, "%".to_string());
    SourceRow {
        source_ns: ns.to_string(),
        source_file: format!("{}.csv", ns),
        row_no: 1,
        dup_ordinal: 0,
        values,
    }
}

/// 落库并把这一行推进到「校验通过」
fn seed_validated(repo: &mut Repo, batch: &str, row: &SourceRow) -> String {
    repo.insert_rows(batch, std::slice::from_ref(row)).expect("落库应成功");
    let uid = ingest::key::row_uid(row);
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(
            &uid,
            &row.source_file,
            batch,
            RowState::PendingValidation,
            "validate",
        ),
    )
    .expect("建底账");
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(
            &uid,
            &row.source_file,
            batch,
            RowState::Validated,
            "validate",
        ),
    )
    .expect("推进到校验通过");
    uid
}

/// 上报身份
fn identity(batch: &str, row_uid: &str, idem: &str, revision: i64) -> SubmitIdentity {
    SubmissionIdentity {
        batch_ref: batch.to_string(),
        row_uid: row_uid.to_string(),
        idempotency_key: idem.to_string(),
        payload_digest: "digest-0001".to_string(),
        revision,
    }
}

/// 走一遍「确认 → 上报被受理 → 回执落库」
fn confirm_and_push(repo: &mut Repo, batch: &str, row_uid: &str, idem: &str, receipt_id: &str) {
    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(batch, row_uid, "信息科甲"),
    )
    .expect("确认应成功");

    let ident = identity(batch, row_uid, idem, 1);
    let adv = receipt::advance(
        &ident,
        &Verdict::Accepted {
            receipt_id: receipt_id.to_string(),
            accepted_at: "2026-11-06 09:12:00".to_string(),
            status: "accepted".to_string(),
        },
    );
    receipt::persist(repo, &ident, &adv).expect("回执落库应成功");
}

// ————————————————————————————————————————————————————————————
// 验收一：按行回放 / 按批次回放
// ————————————————————————————————————————————————————————————

#[test]
fn 按行回放出完整确认与推送轨迹() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);

    let entries = audit::replay_row(repo.conn(), &uid).expect("回放应成功");
    assert!(entries.len() >= 2, "至少要有确认与上报两条：{:?}", entries);

    // 顺序：先确认，后上报
    let events: Vec<&str> = entries.iter().map(|e| e.event.as_str()).collect();
    let confirm_at = events.iter().position(|e| *e == audit::EVENT_CONFIRM);
    let push_at = events.iter().position(|e| *e == audit::EVENT_PUSH_ACCEPTED);
    assert!(confirm_at.is_some(), "轨迹里要有确认事件：{:?}", events);
    assert!(push_at.is_some(), "轨迹里要有上报受理事件：{:?}", events);
    assert!(
        confirm_at < push_at,
        "确认在前、上报在后：{:?}",
        events
    );

    // 上报那条说明里带幂等键与回执 ID —— 这就是「幂等键 ↔ 回执 ID 一一对应」的留痕
    let push_entry = entries[push_at.unwrap()].clone();
    assert!(
        push_entry.detail.contains(IDEM_KEY),
        "上报事件要带幂等键：{}",
        push_entry.detail
    );
    assert!(
        push_entry.detail.contains(RECEIPT_ID),
        "上报事件要带回执 ID：{}",
        push_entry.detail
    );
    assert_eq!(push_entry.row_uid, uid);
    assert_eq!(push_entry.batch_ref, BATCH);
}

#[test]
fn 按批次回放这一批推了什么() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);

    let entries = audit::replay_batch(repo.conn(), BATCH).expect("回放应成功");
    assert!(!entries.is_empty(), "这一批应有审计记录");
    assert!(
        entries.iter().all(|e| e.batch_ref == BATCH),
        "按批次回放只给这一批的记录"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.event == audit::EVENT_PUSH_ACCEPTED),
        "这一批推了什么要看得见"
    );
    // 别的批次回放不到
    let other = audit::replay_batch(repo.conn(), "batch-没有这个批次").unwrap();
    assert!(other.is_empty());
}

// ————————————————————————————————————————————————————————————
// 验收二：幂等键 ↔ 回执 ID 一一对应
// ————————————————————————————————————————————————————————————

#[test]
fn 幂等键与回执一一对应成立_破口查得出来() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);

    let report = audit::verify_idempotency_mapping(repo.conn()).expect("核对应成功");
    assert!(report.is_intact(), "一一对应应成立：{:?}", report.summary_line());
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.pairs[0].idempotency_key, IDEM_KEY);
    assert_eq!(report.pairs[0].receipt_id, RECEIPT_ID);
    assert_eq!(report.pairs[0].revision, 1);

    // 破口：提交说「已上报」却没有回执 —— 最危险的一种（下次重跑会再推一遍）
    let orphan = identity(BATCH, "r-orphan", "idem-orphan", 1);
    repo.upsert_submission(&orphan, "已上报", "", "", false)
        .expect("提交落库应成功");
    let report = audit::verify_idempotency_mapping(repo.conn()).expect("核对应成功");
    assert!(!report.is_intact(), "破口要能查出来");
    assert_eq!(report.missing_receipts.len(), 1, "缺回执要列出来");
    assert!(report.missing_receipts[0].contains("idem-orphan"));
}

// ————————————————————————————————————————————————————————————
// 验收三：审计链只追加、只含行级与科室级数据
// ————————————————————————————————————————————————————————————

#[test]
fn 审计链只追加不改_写语句只在审计模块里() {
    // 静态判据：`audit_log` 的写语句只允许出现在 src/state/audit.rs
    let src_dir = project_dir().join("src");
    let mut files = Vec::new();
    collect_rs(&src_dir, &mut files);
    assert!(!files.is_empty(), "应能扫到源码文件");

    let mut write_hits: Vec<String> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let rel = path.strip_prefix(&src_dir).unwrap().to_string_lossy().to_string();
        for needle in [
            "INSERT INTO audit_log",
            "UPDATE audit_log",
            "DELETE FROM audit_log",
        ] {
            if text.contains(needle) {
                write_hits.push(format!("{}：{}", rel, needle));
            }
        }
    }
    let files_hit: Vec<&str> = write_hits
        .iter()
        .map(|hit| hit.split('：').next().unwrap())
        .collect();
    assert!(
        files_hit.iter().all(|f| *f == "state/audit.rs"),
        "审计链的写语句只允许出现在 state/audit.rs，实际命中：{:?}",
        write_hits
    );

    // 运行期判据：追加只增不减 —— 追加 N 条后条数只多不少，且旧的记录一条没变
    let dir = tempfile::tempdir().unwrap();
    let repo = open_repo(dir.path());
    let before = audit::count_for_row(repo.conn(), "r-1").unwrap();
    let rec = audit::AuditRecord::new("b-1", "r-1", audit::EVENT_CONFIRM, "信息科甲", "复核通过");
    audit::append(repo.conn(), &rec).unwrap();
    let rec2 = audit::AuditRecord::new("b-1", "r-1", audit::EVENT_REVOKE, "信息科甲", "撤回");
    audit::append(repo.conn(), &rec2).unwrap();
    let after = audit::count_for_row(repo.conn(), "r-1").unwrap();
    assert_eq!(after, before + 2, "只追加：条数只增不减");
    let events = audit::events_for_row(repo.conn(), "r-1").unwrap();
    assert_eq!(events[0].1, audit::EVENT_CONFIRM, "旧记录一条都没被改掉");
    assert_eq!(events[1].1, audit::EVENT_REVOKE);
}

#[test]
fn 审计表不含业务明细列() {
    // 审计链只到行级与科室级：表里没有取值列（分子 / 分母 / 填报值一个都没有）
    let dir = tempfile::tempdir().unwrap();
    let repo = open_repo(dir.path());
    let cols = regval::db::schema::column_names(repo.conn(), "audit_log").unwrap();
    for forbidden in ["numerator", "denominator", "value", "patient", "name", "remark"] {
        assert!(
            !cols.iter().any(|c| c == forbidden),
            "审计表不该有 {} 列，实际：{:?}",
            forbidden,
            cols
        );
    }
    assert!(cols.contains(&"row_uid".to_string()));
    assert!(cols.contains(&"event".to_string()));
    assert!(cols.contains(&"actor".to_string()));
}

#[test]
fn 恢复日志只追加不改() {
    let dir = tempfile::tempdir().unwrap();
    let path = journal::journal_path(&dir.path().join("state.db"));
    let mut rec = journal::JournalRecord::receipt(BATCH, "r-1", IDEM_KEY, 1, RECEIPT_ID, "已上报");
    rec.seq = 1;
    journal::append(&path, &rec).unwrap();
    let size_1 = std::fs::metadata(&path).unwrap().len();
    let mut rec2 = journal::JournalRecord::batch_commit(BATCH, "这批跑完了");
    rec2.seq = 2;
    journal::append(&path, &rec2).unwrap();
    let size_2 = std::fs::metadata(&path).unwrap().len();
    assert!(size_2 > size_1, "恢复日志只往后接，不改已有内容");

    let contents = journal::read_all(&path).unwrap();
    assert_eq!(contents.records.len(), 2);
    assert_eq!(contents.records[0].receipt_id, RECEIPT_ID);
}

// ————————————————————————————————————————————————————————————
// 验收四：状态库损坏 → 按幂等键与回执记录恢复
// ————————————————————————————————————————————————————————————

/// 把状态库文件写坏（模拟掉电 / 文件被截断）
fn corrupt(path: &Path) {
    let mut bytes = vec![0x53; 4096];
    bytes[0..17].copy_from_slice(b"not-a-sqlite-file");
    std::fs::write(path, &bytes).expect("写坏状态库");
}

#[test]
fn 状态库损坏后按幂等键与回执恢复_对应关系不丢() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let mut repo = Repo::open(&db_path).unwrap();
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);
    // 批次提交标记：这批跑完了
    let mut marker = journal::JournalRecord::batch_commit(BATCH, "批次状态 上报完成");
    repo.append_journal(&mut marker).unwrap();
    drop(repo);

    // 库坏了
    corrupt(&db_path);
    let broken_bytes = std::fs::read(&db_path).unwrap();

    // 恢复
    let report = recover::recover(&db_path).expect("恢复应成功");
    assert_ne!(
        report.outcome,
        RecoveryOutcome::Unrecoverable,
        "有恢复日志就该恢复得回来：{}",
        report.summary_line()
    );
    assert_eq!(report.last_consistent_batch, BATCH, "恢复到哪一批要说清楚");
    assert_eq!(report.restored_receipts, 1, "回执恢复回来了");

    // 原库一个字节都没动
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        broken_bytes,
        "恢复不许动原库"
    );

    // 恢复库里幂等键 ↔ 回执 ID 的对应关系不丢
    let recovered = Repo::open(&recover::recovered_path(&db_path)).unwrap();
    let pairs = audit::idempotency_receipt_pairs(recovered.conn()).unwrap();
    assert_eq!(pairs.len(), 1, "对应关系不丢");
    assert_eq!(pairs[0].idempotency_key, IDEM_KEY);
    assert_eq!(pairs[0].receipt_id, RECEIPT_ID);
    assert_eq!(pairs[0].revision, 1);
    let mapping = audit::verify_idempotency_mapping(recovered.conn()).unwrap();
    assert!(
        mapping.is_intact(),
        "恢复后一一对应仍成立：{}",
        mapping.summary_line()
    );

    // 丢了什么要如实报出来
    let losses = report.loss_lines();
    assert!(
        losses.iter().any(|l| l.contains("校验结论")),
        "校验结论丢了要报出来：{:?}",
        losses
    );
    assert!(report.lost_rows && report.lost_issues && report.lost_row_state);
}

#[test]
fn 恢复到最后一个一致状态_半截批次如实报出() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let mut repo = Repo::open(&db_path).unwrap();

    // 第一批：跑完了（有提交标记）
    let row1 = source_row("科室一表", "BK-01");
    let uid1 = seed_validated(&mut repo, BATCH, &row1);
    confirm_and_push(&mut repo, BATCH, &uid1, IDEM_KEY, RECEIPT_ID);
    let mut marker = journal::JournalRecord::batch_commit(BATCH, "批次状态 上报完成");
    repo.append_journal(&mut marker).unwrap();

    // 第二批：跑到一半断了（只有回执，没有提交标记）
    let row2 = source_row("科室二表", "BK-02");
    let uid2 = seed_validated(&mut repo, BATCH2, &row2);
    confirm_and_push(&mut repo, BATCH2, &uid2, "idem-sha256-0002", "RCPT-2026-0002");
    drop(repo);

    corrupt(&db_path);
    let report = recover::recover(&db_path).unwrap();

    assert_eq!(report.last_consistent_batch, BATCH, "一致批次是第一批");
    assert!(
        report.lost_batches.contains(&BATCH2.to_string()),
        "半截批次要如实报出来：{:?}",
        report.lost_batches
    );

    // 半截批次的回执**照样恢复** —— 回执是网关已经受理的事实，少恢复一条就可能重推
    let recovered = Repo::open(&recover::recovered_path(&db_path)).unwrap();
    let pairs = audit::idempotency_receipt_pairs(recovered.conn()).unwrap();
    assert_eq!(pairs.len(), 2, "两条回执都要恢复");
    assert!(pairs.iter().any(|p| p.batch_ref == BATCH2));
}

#[test]
fn 恢复库换上去后原库被备份不删除() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let mut repo = Repo::open(&db_path).unwrap();
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);
    drop(repo);

    corrupt(&db_path);
    recover::recover(&db_path).unwrap();
    let backup = recover::install(&db_path).expect("换位应成功");

    assert!(backup.exists(), "原库被挪成备份，不能删");
    assert!(db_path.exists(), "恢复库换到了正位");
    let repo = Repo::open(&db_path).unwrap();
    let pairs = audit::idempotency_receipt_pairs(repo.conn()).unwrap();
    assert_eq!(pairs.len(), 1, "换上去之后对应关系还在");
}

#[test]
fn 没有任何恢复源时如实说恢复不了() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    Repo::open(&db_path).unwrap();
    corrupt(&db_path);

    // 没有恢复日志（原库也读不出来）→ 如实说恢复不了
    let report = recover::recover(&db_path).unwrap();
    assert_eq!(report.outcome, RecoveryOutcome::Unrecoverable);
    assert!(report.restored_receipts == 0);
    assert!(
        report.notes.iter().any(|n| n.contains("恢复日志")),
        "要说清恢复源在哪儿：{:?}",
        report.notes
    );
    // 原库依然一个字节没动
    assert!(db_path.exists());
}

#[test]
fn 完好的库不需要恢复() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.db");
    let mut repo = Repo::open(&db_path).unwrap();
    let row = source_row("科室一表", "BK-01");
    let uid = seed_validated(&mut repo, BATCH, &row);
    confirm_and_push(&mut repo, BATCH, &uid, IDEM_KEY, RECEIPT_ID);

    let report = recover::recover(&db_path).unwrap();
    assert_eq!(report.outcome, RecoveryOutcome::NotNeeded);
    assert!(report.loss_lines().is_empty());
    assert!(
        !recover::recovered_path(&db_path).exists(),
        "库好好的不该生成恢复产物"
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}
