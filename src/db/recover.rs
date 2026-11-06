//! 状态库恢复 —— 库坏了以后怎么把「报过没有」这件事找回来。
//!
//! # 这一层回答什么问题
//!
//! 状态库是单文件 SQLite。它坏掉时（掉电、文件被截断、被别的程序写坏），
//! 库里的东西全读不出来 —— 包括「这一行已经报给网关了」这条**外部事实**。
//! 少了它，重跑一遍上报就会把同样的行再推一次，网关那边变成两条上报。
//!
//! 所以恢复的目标很窄，也很清楚：
//!
//! - **恢复**幂等键 ↔ 回执 ID 的对应关系（有回执的行，重跑时判幂等命中，不重推）；
//! - **恢复**批次提交标记（哪个批次算跑完了）；
//! - **如实报告丢了什么**（校验结论、口径问题、行状态、没跑完的那一批）。
//!
//! # 恢复源有两个
//!
//! 1. **恢复日志**（[`crate::db::journal`]）：状态库旁边的 append-only 边车文件，
//!    只记上报侧的幂等键与回执。**库坏了它通常还活着** —— 这是它存在的全部理由。
//! 2. **原库残留**：文件损坏常常只是局部坏（某个页坏了），别的表可能还读得出来。
//!    能读出来的表（回执、提交、审计链、确认记录）一并抢救回来。
//!
//! 两个源都读不到时，如实说「恢复不了」，**不编**。
//!
//! # 不覆盖原库
//!
//! 恢复产物写在 `<状态库>.recovered`（另开一个文件），**原库一个字节都不动**。
//! 要换上去由人来做（[`install`] 把原库挪到 `.corrupt-N` 备份，再把恢复库放正位）。
//! 自动覆盖一个损坏的库 —— 那是把「还能抢救的东西」也弄没了。
//!
//! # 恢复到什么程度算「一致」
//!
//! 一批的上报可能跑到一半断掉。**批次提交标记**（`batch_commit`）是「这批跑完了」的凭据：
//! 标记在，这批就是完整的；标记不在，这批就是**半截批次**。
//!
//! 但**回执一律全恢复** —— 回执是「网关已经受理」这条外部事实，
//! 哪怕那一批没跑到最后，回执也照样是真的；少恢复一条，重跑时那一行就会被再推一遍。
//! 半截批次只影响**批次汇总**：报告里单独列出来，让人重跑这一批的上报补齐汇总。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db::journal::{self, JournalRecord};
use crate::db::repo::{Confirmation, ReceiptRow, Repo, SubmissionIdentity};
use crate::db::schema;
use crate::errors::{EngineError, ErrorCode};
use crate::state::audit::{self, AuditRecord};

/// 恢复结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryOutcome {
    /// 状态库完好 —— 没什么要恢复的
    #[default]
    NotNeeded,
    /// 恢复了（幂等键 ↔ 回执对应关系完整）
    Recovered,
    /// 恢复了一部分（有内容没能恢复，见报告里的丢失项）
    Partial,
    /// 恢复不了（两个恢复源都读不到东西）
    Unrecoverable,
}

impl RecoveryOutcome {
    /// 中文说法（终端与报告用）
    pub fn zh(&self) -> &'static str {
        match self {
            RecoveryOutcome::NotNeeded => "状态库完好，无需恢复",
            RecoveryOutcome::Recovered => "已恢复到最后一个一致状态",
            RecoveryOutcome::Partial => "部分恢复（有内容未能恢复，见丢失项）",
            RecoveryOutcome::Unrecoverable => "恢复不了（幂等键与回执记录都读不到）",
        }
    }
}

/// 恢复报告 —— 「恢复到哪一批、丢了什么」。
#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    /// 状态库路径
    pub state_db: String,
    /// 恢复产物路径（`<状态库>.recovered`）
    pub recovered_db: String,
    /// 结局
    pub outcome: RecoveryOutcome,
    /// 恢复到哪一批（最后一条批次提交标记所属批次）
    pub last_consistent_batch: String,
    /// 恢复的回执条数（幂等键 ↔ 回执 ID 的对应关系）
    pub restored_receipts: usize,
    /// 恢复的提交条数
    pub restored_submissions: usize,
    /// 恢复的审计事件条数
    pub restored_audit_events: usize,
    /// 恢复的人工确认条数
    pub restored_confirmations: usize,
    /// 没能恢复的批次（跑了但没跑完的半截批次）
    pub lost_batches: Vec<String>,
    /// 没能读出来的表
    pub lost_tables: Vec<String>,
    /// 校验结论（`rows` 表）是否丢了
    pub lost_rows: bool,
    /// 口径问题（`issues` 表）是否丢了
    pub lost_issues: bool,
    /// 行状态（行状态表）是否丢了
    pub lost_row_state: bool,
    /// 恢复日志的记录条数（恢复源之一）
    pub journal_records: usize,
    /// 恢复日志里读不出来的行数
    pub journal_skipped_lines: usize,
    /// 补充说明
    pub notes: Vec<String>,
}

impl RecoveryReport {
    /// 一行可读说法（终端用）
    pub fn summary_line(&self) -> String {
        format!(
            "{} · 恢复到批次 {} · 回执 {} 条 · 提交 {} 条 · 审计 {} 条 · 确认 {} 条",
            self.outcome.zh(),
            if self.last_consistent_batch.is_empty() {
                "（无）"
            } else {
                &self.last_consistent_batch
            },
            self.restored_receipts,
            self.restored_submissions,
            self.restored_audit_events,
            self.restored_confirmations
        )
    }

    /// 丢了什么（一行一条，给经办人看）
    pub fn loss_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.lost_rows {
            out.push("校验结论（原表各行的校验结果）未恢复：需对本批重新跑一次校验".to_string());
        }
        if self.lost_issues {
            out.push("口径问题清单未恢复：需对本批重新跑一次校验".to_string());
        }
        if self.lost_row_state {
            out.push("行状态（待确认 / 已上报 …）未恢复：需对本批重新跑一次校验并重新确认".to_string());
        }
        for table in &self.lost_tables {
            out.push(format!("表 {} 读不出来，未能恢复", table));
        }
        for batch in &self.lost_batches {
            out.push(format!(
                "批次 {} 没跑到最后（没有批次提交标记）：回执已恢复，但批次汇总未落库，需重跑这一批的上报补齐汇总",
                batch
            ));
        }
        out
    }

    /// 报告产物（JSON）
    pub fn to_json(&self) -> String {
        let losses = self.loss_lines();
        serde_json::json!({
            "schema_version": "regval.recovery/1.0.0",
            "state_db": self.state_db,
            "recovered_db": self.recovered_db,
            "outcome": self.outcome.zh(),
            "last_consistent_batch": self.last_consistent_batch,
            "restored": {
                "receipts": self.restored_receipts,
                "submissions": self.restored_submissions,
                "audit_events": self.restored_audit_events,
                "confirmations": self.restored_confirmations,
            },
            "journal": {
                "records": self.journal_records,
                "skipped_lines": self.journal_skipped_lines,
            },
            "lost": {
                "rows": self.lost_rows,
                "issues": self.lost_issues,
                "row_state": self.lost_row_state,
                "tables": self.lost_tables,
                "batches": self.lost_batches,
            },
            "losses": losses,
            "notes": self.notes,
        })
        .to_string()
    }
}

/// 恢复产物路径：`<状态库>.recovered`。
pub fn recovered_path(state_db: &Path) -> PathBuf {
    let mut name = state_db
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "state.db".to_string());
    name.push_str(".recovered");
    state_db.with_file_name(name)
}

/// 状态库是否完好（不改动任何东西）。
pub fn is_healthy(state_db: &Path) -> Result<bool, EngineError> {
    if !state_db.exists() {
        return Ok(true);
    }
    let conn = match Connection::open(state_db) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    Ok(schema::check_integrity(&conn, &state_db.display().to_string()).is_ok())
}

/// 恢复状态库。
///
/// - 库完好 → 结局 [`RecoveryOutcome::NotNeeded`]，**一个字节都不动**；
/// - 库坏了 → 从恢复日志 + 原库残留重建 `<状态库>.recovered`，给出恢复报告。
pub fn recover(state_db: &Path) -> Result<RecoveryReport, EngineError> {
    let mut report = RecoveryReport {
        state_db: state_db.display().to_string(),
        recovered_db: recovered_path(state_db).display().to_string(),
        ..RecoveryReport::default()
    };

    // 1. 库完好？→ 没什么要恢复的
    if is_healthy(state_db)? {
        report.outcome = RecoveryOutcome::NotNeeded;
        report.notes.push("状态库完整性检查通过，未做任何改动".to_string());
        return Ok(report);
    }

    // 2. 读恢复源
    let journal_path = journal::journal_path(state_db);
    let contents = journal::read_all(&journal_path)?;
    report.journal_records = contents.records.len();
    report.journal_skipped_lines = contents.skipped_lines;
    let last_batch = journal::last_consistent_batch(&contents.records);
    report.lost_batches = journal::uncommitted_batches(&contents.records);
    report.last_consistent_batch = last_batch.clone().unwrap_or_default();

    let salvaged = salvage_all(state_db)?;

    // 3. 一个恢复源都读不到东西 → 恢复不了，如实说
    if contents.records.is_empty() && salvaged.receipts.is_empty() && salvaged.submissions.is_empty() {
        report.outcome = RecoveryOutcome::Unrecoverable;
        report.lost_rows = true;
        report.lost_issues = true;
        report.lost_row_state = true;
        let unreadable = salvaged
            .lost_tables
            .iter()
            .any(|t| t == "receipts" || t == "submissions");
        if unreadable {
            report.notes.push(format!(
                "恢复日志（{}）读不到记录，原库里的回执与提交表也读不出来",
                journal_path.display()
            ));
        } else {
            report.notes.push(format!(
                "恢复日志（{}）里没有记录，原库里也没有回执与提交 —— 这一批还没上报过，没有「报过没有」的事实需要恢复",
                journal_path.display()
            ));
        }
        report.notes.push("原库未做任何改动 —— 请从备份恢复，或重新校验本批后重报".to_string());
        return Ok(report);
    }

    // 4. 建恢复库（另开一个文件，原库不动）
    let target = recovered_path(state_db);
    if target.exists() {
        // 上一轮的恢复产物 —— 覆盖它（它是本模块自己写的派生产物）
        std::fs::remove_file(&target).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("旧恢复产物删除失败：{}", e),
                target.display().to_string(),
            )
        })?;
    }
    let mut repo = Repo::open(&target)?;

    // 5. 先恢复幂等键 ↔ 回执 ID 的对应关系（最要紧的一件）
    //
    // **所有**回执都恢复 —— 回执是「网关已经受理」这条外部事实，
    // 哪怕那一批没跑到最后（没有批次提交标记），回执也照样是真的。
    // 少恢复一条，重跑时那一行就会被再推一遍。
    let mut restored_receipts = 0usize;
    let mut restored_submissions = 0usize;
    for rec in contents.records.iter() {
        match rec.kind.as_str() {
            journal::KIND_RECEIPT => {
                let (r, s) = restore_receipt(&mut repo, rec)?;
                restored_receipts += r;
                restored_submissions += s;
            }
            journal::KIND_SUBMISSION => {
                if restore_submission(&mut repo, rec)? {
                    restored_submissions += 1;
                }
            }
            _ => {}
        }
    }

    // 6. 原库残留：能读出来的表一并抢救
    for r in &salvaged.receipts {
        let (rc, sc) = restore_receipt_row(&mut repo, r)?;
        restored_receipts += rc;
        restored_submissions += sc;
    }
    for s in &salvaged.submissions {
        if restore_submission_row(&mut repo, s)? {
            restored_submissions += 1;
        }
    }
    for a in &salvaged.audit {
        let rec = AuditRecord {
            ts: a.0.clone(),
            batch_ref: a.1.clone(),
            row_uid: a.2.clone(),
            event: a.3.clone(),
            actor: a.4.clone(),
            detail: a.5.clone(),
        };
        audit::append(repo.conn(), &rec)?;
        report.restored_audit_events += 1;
    }
    for c in &salvaged.confirmations {
        crate::state::confirm::restore_confirmation(repo.conn_mut(), c)?;
        report.restored_confirmations += 1;
    }

    report.restored_receipts = restored_receipts;
    report.restored_submissions = restored_submissions;
    report.lost_tables = salvaged.lost_tables.clone();
    // 校验结论 / 口径问题 / 行状态没有恢复源 —— 库里没了就是没了，如实报
    report.lost_rows = true;
    report.lost_issues = true;
    report.lost_row_state = true;

    report.outcome = if report.lost_batches.is_empty() && report.lost_tables.is_empty() {
        RecoveryOutcome::Recovered
    } else {
        RecoveryOutcome::Partial
    };
    // 恢复这件事本身也留一条审计（恢复库里的审计链）
    audit::append(
        repo.conn(),
        &AuditRecord::new(
            &report.last_consistent_batch,
            "",
            audit::EVENT_RECOVER,
            "regval recover",
            &format!(
                "从恢复日志与原库残留恢复：回执 {} 条 · 提交 {} 条 · 审计 {} 条 · 确认 {} 条",
                restored_receipts, restored_submissions, report.restored_audit_events, report.restored_confirmations
            ),
        ),
    )?;
    report.notes.push(format!(
        "恢复库已写出：{}（原库 {} 未做任何改动）",
        target.display(),
        state_db.display()
    ));
    report.notes.push(
        "恢复的是「报过没有」这条事实（幂等键 ↔ 回执 ID）与审计链；校验结论与行状态需重新跑一次校验"
            .to_string(),
    );
    report.notes.push(
        "确认无误后再用 regval recover --apply 把恢复库换上去（原库会先被挪成 .corrupt-N 备份）"
            .to_string(),
    );

    Ok(report)
}

/// 把恢复库换上去：原库挪成 `.corrupt-N` 备份，恢复库放正位。
///
/// **不删原库** —— 挪走而不是删掉：万一恢复库也不对，备份还在。
pub fn install(state_db: &Path) -> Result<PathBuf, EngineError> {
    let target = recovered_path(state_db);
    if !target.exists() {
        return Err(EngineError::at(
            ErrorCode::Io,
            "没有恢复产物可换 —— 先跑 regval recover 生成恢复库",
            target.display().to_string(),
        ));
    }
    let backup = next_backup_path(state_db);
    if state_db.exists() {
        std::fs::rename(state_db, &backup).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("原库挪到备份位失败：{}", e),
                state_db.display().to_string(),
            )
        })?;
    }
    std::fs::rename(&target, state_db).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("恢复库换位失败：{}", e),
            state_db.display().to_string(),
        )
    })?;
    Ok(backup)
}

/// 下一个备份文件名（`<状态库>.corrupt-1`、`.corrupt-2` …）。
fn next_backup_path(state_db: &Path) -> PathBuf {
    let mut name = state_db
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "state.db".to_string());
    let stem = name.clone();
    for n in 1..1000 {
        name = format!("{}.corrupt-{}", stem, n);
        let candidate = state_db.with_file_name(&name);
        if !candidate.exists() {
            return candidate;
        }
    }
    state_db.with_file_name(format!("{}.corrupt-{}", stem, 999))
}

/// 从恢复日志恢复一条回执（幂等键 ↔ 回执 ID）。返回（新增回执数，新增提交数）。
fn restore_receipt(repo: &mut Repo, rec: &JournalRecord) -> Result<(usize, usize), EngineError> {
    if rec.idempotency_key.trim().is_empty() || rec.receipt_id.trim().is_empty() {
        return Ok((0, 0));
    }
    let row = ReceiptRow {
        receipt_id: rec.receipt_id.trim().to_string(),
        idempotency_key: rec.idempotency_key.trim().to_string(),
        revision: rec.revision,
        batch_ref: rec.batch_ref.trim().to_string(),
        row_uid: rec.row_uid.trim().to_string(),
        status: rec.status.trim().to_string(),
        accepted_at: String::new(),
        error_code: String::new(),
        error_field: String::new(),
        message: rec.detail.trim().to_string(),
    };
    restore_receipt_row(repo, &row)
}

/// 恢复一条回执（已存在则跳过 —— 不覆盖已有回执）。
fn restore_receipt_row(repo: &mut Repo, row: &ReceiptRow) -> Result<(usize, usize), EngineError> {
    let exists: i64 = repo.conn().query_row(
        "SELECT COUNT(*) FROM receipts WHERE idempotency_key = ?1 AND revision = ?2",
        rusqlite::params![row.idempotency_key, row.revision],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok((0, 0));
    }
    repo.insert_receipt(row)?;
    // 提交侧同步落一条（有回执 ⇒ 这一版报上去了）
    let identity = SubmissionIdentity {
        batch_ref: row.batch_ref.clone(),
        row_uid: row.row_uid.clone(),
        idempotency_key: row.idempotency_key.clone(),
        payload_digest: String::new(),
        revision: row.revision,
    };
    let sub_exists: i64 = repo.conn().query_row(
        "SELECT COUNT(*) FROM submissions WHERE idempotency_key = ?1 AND revision = ?2",
        rusqlite::params![identity.idempotency_key, identity.revision],
        |r| r.get(0),
    )?;
    repo.upsert_submission(&identity, crate::push::status::ACCEPTED, "", "", false)?;
    Ok((1, usize::from(sub_exists == 0)))
}

/// 从恢复日志恢复一条提交（没有回执的结局：被退回 / 待重试）。
fn restore_submission(repo: &mut Repo, rec: &JournalRecord) -> Result<bool, EngineError> {
    if rec.idempotency_key.trim().is_empty() {
        return Ok(false);
    }
    let identity = SubmissionIdentity {
        batch_ref: rec.batch_ref.trim().to_string(),
        row_uid: rec.row_uid.trim().to_string(),
        idempotency_key: rec.idempotency_key.trim().to_string(),
        payload_digest: String::new(),
        revision: rec.revision,
    };
    let status = if rec.status.trim().is_empty() {
        crate::push::status::RETRYABLE.to_string()
    } else {
        rec.status.trim().to_string()
    };
    let exists: i64 = repo.conn().query_row(
        "SELECT COUNT(*) FROM submissions WHERE idempotency_key = ?1 AND revision = ?2",
        rusqlite::params![identity.idempotency_key, identity.revision],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(false);
    }
    repo.upsert_submission(&identity, &status, "", "", false)?;
    Ok(true)
}

/// 恢复一条提交（从原库残留读出来的）。
fn restore_submission_row(
    repo: &mut Repo,
    row: &crate::db::repo::SubmissionRecord,
) -> Result<bool, EngineError> {
    let identity = SubmissionIdentity {
        batch_ref: row.batch_ref.clone(),
        row_uid: row.row_uid.clone(),
        idempotency_key: row.idempotency_key.clone(),
        payload_digest: row.payload_digest.clone(),
        revision: row.revision,
    };
    let exists: i64 = repo.conn().query_row(
        "SELECT COUNT(*) FROM submissions WHERE idempotency_key = ?1 AND revision = ?2",
        rusqlite::params![identity.idempotency_key, identity.revision],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(false);
    }
    repo.upsert_submission(
        &identity,
        &row.status,
        &row.last_error_code,
        &row.last_error_field,
        false,
    )?;
    Ok(true)
}

/// 从损坏的原库里抢救出来的内容。
#[derive(Debug, Clone, Default)]
struct Salvaged {
    receipts: Vec<ReceiptRow>,
    submissions: Vec<crate::db::repo::SubmissionRecord>,
    /// (ts, batch_ref, row_uid, event, actor, detail)
    audit: Vec<(String, String, String, String, String, String)>,
    confirmations: Vec<Confirmation>,
    lost_tables: Vec<String>,
}

/// 把原库里还读得出来的表抢救一遍。
fn salvage_all(state_db: &Path) -> Result<Salvaged, EngineError> {
    let mut out = Salvaged::default();
    if !state_db.exists() {
        return Ok(out);
    }
    let conn = match Connection::open(state_db) {
        Ok(c) => c,
        Err(_) => {
            out.lost_tables.push("（整个库）".to_string());
            return Ok(out);
        }
    };

    match salvage_receipts(&conn) {
        Ok(rows) => out.receipts = rows,
        Err(_) => out.lost_tables.push("receipts".to_string()),
    }
    match salvage_submissions(&conn) {
        Ok(rows) => out.submissions = rows,
        Err(_) => out.lost_tables.push("submissions".to_string()),
    }
    match salvage_audit(&conn) {
        Ok(rows) => out.audit = rows,
        Err(_) => out.lost_tables.push("audit_log".to_string()),
    }
    match salvage_confirmations(&conn) {
        Ok(rows) => out.confirmations = rows,
        Err(_) => out.lost_tables.push("confirmations".to_string()),
    }
    Ok(out)
}

/// 抢救回执表。
fn salvage_receipts(conn: &Connection) -> Result<Vec<ReceiptRow>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT receipt_id, idempotency_key, revision, batch_ref, row_uid, status,
                accepted_at, error_code, error_field, message
           FROM receipts",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ReceiptRow {
            receipt_id: r.get(0)?,
            idempotency_key: r.get(1)?,
            revision: r.get(2)?,
            batch_ref: r.get(3)?,
            row_uid: r.get(4)?,
            status: r.get(5)?,
            accepted_at: r.get(6)?,
            error_code: r.get(7)?,
            error_field: r.get(8)?,
            message: r.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            // 中途读坏：已读到的留下，读坏的那张表算「读不出来」
            Err(_) => return Err(EngineError::at(
                ErrorCode::DbCorrupted,
                "回执表读了一半就读坏了",
                "receipts",
            )),
        }
    }
    Ok(out)
}

/// 抢救提交表。
fn salvage_submissions(conn: &Connection) -> Result<Vec<crate::db::repo::SubmissionRecord>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT batch_ref, row_uid, idempotency_key, payload_digest, revision, status,
                attempt_count, last_error_code, last_error_field, created_at, updated_at
           FROM submissions",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(crate::db::repo::SubmissionRecord {
            batch_ref: r.get(0)?,
            row_uid: r.get(1)?,
            idempotency_key: r.get(2)?,
            payload_digest: r.get(3)?,
            revision: r.get(4)?,
            status: r.get(5)?,
            attempt_count: r.get(6)?,
            last_error_code: r.get(7)?,
            last_error_field: r.get(8)?,
            created_at: r.get(9)?,
            updated_at: r.get(10)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            Err(_) => return Err(EngineError::at(
                ErrorCode::DbCorrupted,
                "提交表读了一半就读坏了",
                "submissions",
            )),
        }
    }
    Ok(out)
}

/// 抢救审计链表。
fn salvage_audit(
    conn: &Connection,
) -> Result<Vec<(String, String, String, String, String, String)>, EngineError> {
    let mut stmt =
        conn.prepare("SELECT ts, batch_ref, row_uid, event, actor, detail FROM audit_log ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            Err(_) => return Err(EngineError::at(
                ErrorCode::DbCorrupted,
                "审计链读了一半就读坏了",
                "audit_log",
            )),
        }
    }
    Ok(out)
}

/// 抢救人工确认记录表。
fn salvage_confirmations(conn: &Connection) -> Result<Vec<Confirmation>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT batch_ref, row_uid, action, confirmed_by, confirmed_at, note, revoked_by, revoked_at
           FROM confirmations ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Confirmation {
            batch_ref: r.get(0)?,
            row_uid: r.get(1)?,
            action: r.get(2)?,
            confirmed_by: r.get(3)?,
            confirmed_at: r.get(4)?,
            note: r.get(5)?,
            revoked_by: r.get(6)?,
            revoked_at: r.get(7)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        match r {
            Ok(v) => out.push(v),
            Err(_) => return Err(EngineError::at(
                ErrorCode::DbCorrupted,
                "确认记录读了一半就读坏了",
                "confirmations",
            )),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 完好的库不需要恢复() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        Repo::open(&db).unwrap();
        let report = recover(&db).unwrap();
        assert_eq!(report.outcome, RecoveryOutcome::NotNeeded);
        assert!(report.loss_lines().is_empty());
    }

    #[test]
    fn 恢复库路径带后缀且不覆盖原库() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let target = recovered_path(&db);
        assert_eq!(target.file_name().unwrap(), "state.db.recovered");
        assert_ne!(target, db);
    }
}
