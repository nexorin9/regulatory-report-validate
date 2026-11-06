//! 本机校验库读写。
//!
//! 落库走一次事务：事务失败整批不落库（全成或全不成），中途不留半截状态。
//!
//! 打开库时先做完整性检查再按序迁移：文件损坏或版本不被识别时报明确错误码，
//! **不静默重建空库**（重建会把经办人已经确认过、已经上报过的记录悄悄抹掉）。

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use super::schema;
use super::schema::{AUTHORITY_AUTHORITATIVE, AUTHORITY_RAW};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::{row_uid, Field, RowMeta, SourceRow};
use crate::log::RunSummary;
use crate::rules::model::Issue;
use crate::rules::version::{Approval, ApprovalIndex};

/// 本机校验库句柄
pub struct Repo {
    conn: Connection,
    /// 库文件路径（恢复日志的路径由它推出来）
    path: PathBuf,
    /// 恢复日志的下一条序号（打开库时从日志里读出来）
    journal_seq: u64,
}

/// 一条人工确认记录（`confirmations` 表的一行）。
///
/// 确认与撤销都是**追加写**：撤销不删原记录，只追加一条 `revoke`。
/// 确认人 / 确认时间必填 —— 没有人的确认等于没人负责。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Confirmation {
    /// 批次号
    pub batch_ref: String,
    /// 被确认的行键
    pub row_uid: String,
    /// 动作（`confirm` / `revoke`）
    pub action: String,
    /// 确认人
    pub confirmed_by: String,
    /// 确认时间
    pub confirmed_at: String,
    /// 备注（可空）
    pub note: String,
    /// 撤销人（未撤销为空串）
    pub revoked_by: String,
    /// 撤销时间（未撤销为空串）
    pub revoked_at: String,
}

impl Confirmation {
    /// 是否是一条「已确认且未被撤销」的确认
    pub fn is_effective(&self) -> bool {
        self.action == "confirm" && self.revoked_at.trim().is_empty()
    }
}

/// 库里的一行（算上报身份用；比 [`RowMeta`] 全）。
///
/// 数值列存的是数值（存不到有限数值时为空），**原文另存 `numeric_raw`** ——
/// 「填坏了」与「没填」在库里分得开，载荷摘要才算得准。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRow {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub org_code: String,
    /// 权威归属状态（raw / authoritative / duplicate / pending_manual）
    pub authority_state: String,
    pub indicator_code: String,
    pub indicator_name: String,
    pub dept_code: String,
    pub dept_name: String,
    pub reporting_period: String,
    pub business_key: String,
    pub numerator: Option<f64>,
    pub denominator: Option<f64>,
    pub value: Option<f64>,
    pub unit: String,
    /// 数值列原文（JSON：`{"numerator":"…","denominator":"…","value":"…"}`）
    pub numeric_raw: String,
    /// 这批结论按哪套口径算的
    pub rule_version: String,
}

/// 一次上报提交（`submissions` 表的一行）。
///
/// 一次提交的身份是 `(idempotency_key, revision)`：同一个幂等键下，
/// 摘要变了就是订正（修订号 +1），摘要没变就是重复提交。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRecord {
    pub batch_ref: String,
    pub row_uid: String,
    pub idempotency_key: String,
    pub payload_digest: String,
    pub revision: i64,
    /// 提交状态（pending / submitted / rejected / retrying …）
    pub status: String,
    pub attempt_count: i64,
    pub last_error_code: String,
    pub last_error_field: String,
    pub created_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for Repo {    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Repo { 本机校验库 }")
    }
}

/// 一次上报的**身份**（落 `submissions` 表用）。
///
/// 身份 = `(幂等键, 修订号)`：同一个幂等键下摘要变了就是订正（修订号 +1），
/// 摘要没变就是重复提交（不重复推送）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionIdentity {
    /// 批次号
    pub batch_ref: String,
    /// 行键
    pub row_uid: String,
    /// 幂等键
    pub idempotency_key: String,
    /// 载荷摘要
    pub payload_digest: String,
    /// 修订号
    pub revision: i64,
}

/// 一条网关回执（落 `receipts` 表用）。
///
/// **只有受理才有回执** —— 退回没有回执号，待重试更没有。
/// 幂等键 ↔ 回执 ID 一一对应，由 `idx_receipts_idem` 与 `idx_receipts_receipt` 两个唯一索引兜住。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRow {
    /// 回执号
    pub receipt_id: String,
    /// 幂等键
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
    /// 批次号
    pub batch_ref: String,
    /// 行键
    pub row_uid: String,
    /// 网关给的状态
    pub status: String,
    /// 受理时间
    pub accepted_at: String,
    /// 错误码（受理时为空）
    pub error_code: String,
    /// 错误字段（受理时为空）
    pub error_field: String,
    /// 人话说明（受理时为空）
    pub message: String,
}

/// 一行的上报状态（从库里读出来，写回原表「上报状态」列用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPushState {
    /// 行键
    pub row_uid: String,
    /// 「上报状态」列取值
    pub status: String,
    /// 机器可读错误码
    pub error_code: String,
    /// 网关点名的字段
    pub error_field: String,
    /// 人话说明
    pub message: String,
    /// 回执号（拿到回执才有）
    pub receipt_id: String,
    /// 受理时间
    pub accepted_at: String,
    /// 幂等键（待重试时靠它续跑）
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
}

/// 一条「待重试」的上报提交 —— **未上报名单的数据来源**。
///
/// 退避重试耗尽的行留在库里（状态「待重试」、幂等键留着），
/// 由经办人核对后手工重推。重推时**复用同一幂等键**：
/// 网关侧若其实已经受理，会返回幂等命中，不会变成两条上报。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRetryRow {
    /// 行键
    pub row_uid: String,
    /// 幂等键
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
    /// 状态（这一类是「待重试」）
    pub status: String,
    /// 最后一次的机器可读错误码
    pub error_code: String,
    /// 网关点名的字段
    pub error_field: String,
    /// 已经推过几次
    pub attempt_count: i64,
    /// 最后一次落库时间
    pub updated_at: String,
}


impl Repo {
    /// 打开（或新建）本机校验库：完整性检查 → 按序迁移 → 就绪。
    ///
    /// - 文件不存在或为空 → 新建，走全部迁移；
    /// - 文件存在但不是有效校验库 → 报 `DB_CORRUPTED`，**不删库重建**；
    /// - 结构版本落后 → 按序迁移补齐（幂等，重复打开不会重复建表）；
    /// - 结构版本高于本引擎 → 报 `DB_VERSION_UNSUPPORTED`，拒绝打开。
    pub fn open(path: &Path) -> Result<Repo, EngineError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EngineError::at(
                        ErrorCode::Io,
                        format!("状态库目录创建失败：{}", e),
                        parent.display().to_string(),
                    )
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|e| {
            EngineError::at(
                ErrorCode::Db,
                format!("状态库打开失败：{}", e),
                path.display().to_string(),
            )
        })?;

        // 完整性检查：损坏的库只报错，不静默重建空库
        schema::check_integrity(&conn, &path.display().to_string())?;

        // 按序迁移（幂等）
        schema::migrate(&conn)?;

        // 恢复日志的下一条序号（日志在库旁边，读不到就当从 1 开始）
        let journal_seq = super::journal::read_all(&super::journal::journal_path(path))
            .map(|c| super::journal::next_seq(&c.records))
            .unwrap_or(1);

        Ok(Repo {
            conn,
            path: path.to_path_buf(),
            journal_seq,
        })
    }

    /// 库文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 恢复日志路径（`<状态库>.journal`）—— 状态库坏掉时靠它把「报过没有」找回来。
    pub fn journal_path(&self) -> PathBuf {
        super::journal::journal_path(&self.path)
    }

    /// 追加一条恢复日志记录（**只追加**）。
    ///
    /// 上报侧的关键事实（幂等键 ↔ 回执 ID、批次提交标记）除了落进状态库，
    /// 还要在状态库旁边另写一份 —— 状态库坏掉时，这份通常还活着。
    pub fn append_journal(&mut self, record: &mut super::journal::JournalRecord) -> Result<(), EngineError> {
        record.seq = self.journal_seq;
        super::journal::append(&self.journal_path(), record)?;
        self.journal_seq += 1;
        Ok(())
    }

    /// 只读借用（规则执行器用）
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 可变借用（落库层开事务用：`Connection::transaction` 需要 `&mut`）
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// 一次事务批量落库。事务失败整批不落库。
    ///
    /// 同批次重跑先清掉上一轮的行列，避免重复累积。
    /// 落库的行权威归属为 `raw`：原始来源行全部留着（重复检测漏斗要逐对比对两行取值），
    /// 提升为权威行由 [`Repo::promote_authoritative`] 显式做，受权威键唯一索引约束。
    pub fn insert_rows(
        &mut self,
        batch_ref: &str,
        rows: &[SourceRow],
    ) -> Result<usize, EngineError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM rows WHERE batch_ref = ?1", params![batch_ref])?;

        let created_at = now_iso8601();
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                                   authority_state, indicator_code, indicator_name, dept_code, dept_name,
                                   reporting_period, period_type, business_key, numerator, denominator,
                                   value, unit, remark, numeric_raw, rule_version, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            )?;

            for row in rows {
                let numeric_raw = numeric_raw_json(row);
                stmt.execute(params![
                    batch_ref,
                    row.source_ns,
                    row.source_file,
                    row.row_no as i64,
                    row_uid(row),
                    row.get_trimmed(Field::OrgCode).unwrap_or(""),
                    AUTHORITY_RAW,
                    row.get_trimmed(Field::IndicatorCode),
                    row.get_trimmed(Field::IndicatorName),
                    row.get_trimmed(Field::DeptCode),
                    row.get_trimmed(Field::DeptName),
                    row.get_trimmed(Field::ReportingPeriod),
                    row.get_trimmed(Field::PeriodType),
                    row.get_trimmed(Field::BusinessKey),
                    row.number(Field::Numerator),
                    row.number(Field::Denominator),
                    row.number(Field::Value),
                    row.get_trimmed(Field::Unit),
                    row.get_trimmed(Field::Remark),
                    numeric_raw,
                    Option::<String>::None,
                    created_at,
                ])?;
                count += 1;
            }
        }

        tx.commit()?;
        Ok(count)
    }

    /// 一次事务批量落问题。同一行同一规则同一区分键只落一条。
    ///
    /// 只清**本轮重算**的那部分：口径规则与实体绑定层的问题每次跑都重算，
    /// 旧的作废。**上报退回留下的问题（`R-PUSH-` 前缀）不清** ——
    /// 那一行还没报上去，退回原因得一直留在「问题说明（含依据）」列里；
    /// 报上去了由 [`Repo::clear_push_issues`] 清。
    pub fn insert_issues(
        &mut self,
        batch_ref: &str,
        issues: &[Issue],
    ) -> Result<usize, EngineError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM issues WHERE batch_ref = ?1 AND rule_id NOT LIKE 'R-PUSH-%'",
            params![batch_ref],
        )?;

        let created_at = now_iso8601();
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO issues (batch_ref, row_uid, rule_id, issue_kind, severity, message,
                                               basis, issue_key, missing_columns, error_code, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;

            for issue in issues {
                let changed = stmt.execute(params![
                    batch_ref,
                    issue.row_uid,
                    issue.rule_id,
                    issue.issue_kind,
                    issue.severity,
                    issue.message,
                    issue.basis,
                    issue.issue_key,
                    issue.missing_columns.join("、"),
                    issue.error_code,
                    created_at,
                ])?;
                count += changed;
            }
        }

        tx.commit()?;
        Ok(count)
    }

    /// 本批行数
    pub fn count_rows(&self, batch_ref: &str) -> Result<i64, EngineError> {
        let n = self.conn.query_row(
            "SELECT COUNT(*) FROM rows WHERE batch_ref = ?1",
            params![batch_ref],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// 把一行提升为权威行。
    ///
    /// 权威键 = `机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键`。
    /// 同一批次内同一权威键只能有**一行**权威数据 —— 重复的提升由唯一索引
    /// `idx_rows_authority` 直接拒掉，这里把它翻成 `AUTHORITY_KEY_CONFLICT` 并附上冲突方的行键，不吞异常。
    ///
    /// 提升按批次限定（`batch_ref` + 行键）：同一份表改过之后重跑，行键可能不变而批次号变了，
    /// 不限定批次就会把上一批的行一起提升。
    ///
    /// **提升策略**（何时把一行判为权威、同键异值如何标「待人工判定」、科室主数据怎么核）
    /// 由实体绑定层决定；本方法只提供「提升为权威行」这一个受唯一索引约束的原语。
    pub fn promote_authoritative(
        &mut self,
        batch_ref: &str,
        row_uid: &str,
    ) -> Result<(), EngineError> {
        let tx = self.conn.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM rows WHERE batch_ref = ?1 AND row_uid = ?2",
            params![batch_ref, row_uid],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(EngineError::at(
                ErrorCode::Db,
                "找不到要提升为权威行的行键",
                format!("batch_ref={} row_uid={}", batch_ref, row_uid),
            ));
        }

        let updated = tx.execute(
            "UPDATE rows SET authority_state = ?1
              WHERE batch_ref = ?2 AND row_uid = ?3 AND authority_state <> ?1",
            params![AUTHORITY_AUTHORITATIVE, batch_ref, row_uid],
        );

        match updated {
            Ok(0) => {
                // 本来就是权威行：幂等，不报错
                tx.commit()?;
                Ok(())
            }
            Ok(_) => {
                tx.commit()?;
                Ok(())
            }
            Err(e) => {
                let conflict = is_authority_conflict(&e);
                let _ = tx.rollback();
                if conflict {
                    Err(EngineError::at(
                        ErrorCode::AuthorityKeyConflict,
                        format!(
                            "权威键重复：机构代码+报告期+指标编号+科室代码+业务主键在本批次已有一行权威数据，行键 {} 不能提升为权威行",
                            row_uid
                        ),
                        row_uid.to_string(),
                    ))
                } else {
                    Err(EngineError::at(
                        ErrorCode::Db,
                        format!("提升权威行失败：{}", e),
                        row_uid.to_string(),
                    ))
                }
            }
        }
    }

    /// 一次事务批量落权威归属（实体绑定层算出的结果）。
    ///
    /// 按批次限定：只改本批次的行，别批次的行不动。
    /// 事务失败整批不落库 —— 不留「一半权威、一半没判定」的半截状态。
    pub fn apply_authority_states(
        &mut self,
        batch_ref: &str,
        assignments: &[(String, &'static str)],
    ) -> Result<usize, EngineError> {
        let tx = self.conn.transaction()?;
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "UPDATE rows SET authority_state = ?1
                  WHERE batch_ref = ?2 AND row_uid = ?3 AND authority_state <> ?1",
            )?;
            for (row_uid, state) in assignments {
                count += stmt.execute(params![state, batch_ref, row_uid])?;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// 某一行当前的权威归属状态
    pub fn authority_state(&self, row_uid: &str) -> Result<Option<String>, EngineError> {
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT authority_state FROM rows WHERE row_uid = ?1",
                params![row_uid],
                |r| r.get(0),
            )
            .ok();
        Ok(state)
    }

    /// 库里已占用的权威键清单（机构代码|报告期|指标编号|科室代码|业务主键）
    pub fn authoritative_keys(&self) -> Result<Vec<String>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT org_code, reporting_period, indicator_code, dept_code, business_key
               FROM rows WHERE authority_state = 'authoritative'
              ORDER BY org_code, reporting_period, indicator_code, dept_code, business_key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}|{}",
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 落一次运行摘要（`regval status` 读它）。
    pub fn insert_run_summary(&mut self, summary: &RunSummary) -> Result<(), EngineError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO run_summaries (command, batch_ref, tables, total_rows, rows_with_issues,
                                        issue_count, elapsed_ms, started_at, finished_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                summary.command,
                summary.batch_ref,
                summary.tables as i64,
                summary.total_rows as i64,
                summary.rows_with_issues as i64,
                summary.issue_count as i64,
                summary.elapsed_ms as i64,
                summary.started_at,
                summary.finished_at,
                now_iso8601(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 最近若干次运行摘要（新的在前）。
    ///
    /// 给 `batch_ref` 时只取该批次；不给时取最近 `limit` 次。
    pub fn list_run_summaries(
        &self,
        batch_ref: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RunSummary>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT command, batch_ref, tables, total_rows, rows_with_issues, issue_count,
                    elapsed_ms, started_at, finished_at
               FROM run_summaries
              WHERE (?1 IS NULL OR batch_ref = ?1)
              ORDER BY id DESC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![batch_ref, limit as i64], |row| {
            Ok(RunSummary {
                command: row.get(0)?,
                batch_ref: row.get(1)?,
                tables: row.get::<_, i64>(2)? as usize,
                total_rows: row.get::<_, i64>(3)? as usize,
                rows_with_issues: row.get::<_, i64>(4)? as usize,
                issue_count: row.get::<_, i64>(5)? as usize,
                elapsed_ms: row.get::<_, i64>(6)? as u64,
                started_at: row.get(7)?,
                finished_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 库里最近一次校验的批次号（导出时不指定批次时用它）。
    ///
    /// 只看 `validate` 的运行摘要 —— `push` / `export` 自己也落摘要，
    /// 按它们取批次会拿到一个没有校验结论的批次。
    pub fn latest_batch_ref(&self) -> Result<Option<String>, EngineError> {
        let batch: Option<String> = self
            .conn
            .query_row(
                "SELECT batch_ref FROM run_summaries
                  WHERE command = 'validate'
                  ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        Ok(batch)
    }

    /// 某批次落库的口径问题（按行键与规则编号排序，两次读出顺序一致）。
    ///
    /// 举证包与「未上报名单」都从这儿取问题：库里落的是什么，产物就说什么，
    /// 不重新跑一遍规则 —— 重跑可能与当初的结论不是同一套口径。
    pub fn list_issues(&self, batch_ref: &str) -> Result<Vec<Issue>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT row_uid, rule_id, issue_kind, severity, message, basis,
                    issue_key, missing_columns, error_code
               FROM issues
              WHERE batch_ref = ?1
              ORDER BY row_uid, rule_id, issue_kind, issue_key",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            let missing_raw: String = r.get(7)?;
            Ok(Issue {
                row_uid: r.get(0)?,
                rule_id: r.get(1)?,
                issue_kind: r.get(2)?,
                severity: r.get(3)?,
                message: r.get(4)?,
                basis: r.get(5)?,
                issue_key: r.get(6)?,
                missing_columns: split_missing_columns(&missing_raw),
                error_code: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 某批次的行标识（举证包按行逐条展开用；含没有命中任何问题的行）。
    pub fn list_row_metas(&self, batch_ref: &str) -> Result<Vec<RowMeta>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT row_uid, source_ns, source_file, row_no, indicator_code,
                    dept_code, reporting_period, business_key
               FROM rows
              WHERE batch_ref = ?1
              ORDER BY source_ns, row_no",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            Ok(RowMeta {
                row_uid: r.get(0)?,
                source_ns: r.get(1)?,
                source_file: r.get(2)?,
                row_no: r.get::<_, i64>(3)? as usize,
                indicator_code: r.get(4)?,
                dept_code: r.get(5)?,
                reporting_period: r.get(6)?,
                business_key: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 库里最近一次缓存的院内科室目录（按数据时间倒序取第一份）。
    ///
    /// 催报对照的「应交科室名册」在名册文件缺失时用它兜底 ——
    /// 目录是科室全量，名单可能宽于实际应交范围（这一点由调用方写进产物说明）。
    pub fn latest_dept_directory_cache(
        &self,
    ) -> Result<Option<crate::push::dept::CachedDirectory>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT org_code, fetched_at, entry_count, payload FROM dept_directory_cache
              ORDER BY fetched_at DESC, org_code LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(crate::push::dept::CachedDirectory {
            org_code: row.get(0)?,
            fetched_at: row.get(1)?,
            entry_count: row.get::<_, i64>(2)? as usize,
            payload: row.get(3)?,
        }))
    }

    /// 给本批次的行记上规则集版本（这批结论是按哪套口径算的）。
    ///
    /// 只补空值：行已经在库里时（比如同一批次重跑后行被重写），版本随新结论更新。
    pub fn set_batch_rule_version(
        &mut self,
        batch_ref: &str,
        version: &str,
    ) -> Result<usize, EngineError> {
        let updated = self.conn.execute(
            "UPDATE rows SET rule_version = ?1
              WHERE batch_ref = ?2 AND (rule_version IS NULL OR rule_version = '')",
            params![version, batch_ref],
        )?;
        Ok(updated)
    }

    /// 本批次记录的口径版本（行级版本；未记录时为空串）
    pub fn batch_rule_version(&self, batch_ref: &str) -> Result<String, EngineError> {
        let version: Option<String> = self
            .conn
            .query_row(
                "SELECT rule_version FROM rows
                  WHERE batch_ref = ?1 AND rule_version IS NOT NULL AND rule_version <> ''
                  LIMIT 1",
                params![batch_ref],
                |r| r.get(0),
            )
            .ok();
        Ok(version.unwrap_or_default())
    }

    // ————————————————————————————————————————————————————
    // 口径换版：确认记录与口径版本应用台账
    // ————————————————————————————————————————————————————

    /// 记一条换版确认（质控岗确认某个口径版本可以生效）。
    ///
    /// 确认人与确认时间**不得为空** —— 没有人的口径换版，等于没人负责。
    pub fn record_approval(&mut self, approval: &Approval) -> Result<(), EngineError> {
        if approval.approver.trim().is_empty() {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                "换版确认记录必须写确认人（质控岗），空名不入库",
                format!("to_version={}", approval.to_version),
            ));
        }
        if approval.to_version.trim().is_empty() {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                "换版确认记录必须写被确认的口径版本",
                "to_version",
            ));
        }
        self.conn.execute(
            "INSERT INTO rule_version_approvals
                (to_version, from_version, slot_key, approver, approved_at, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                approval.to_version.trim(),
                approval.from_version.trim(),
                approval.slot_key.trim(),
                approval.approver.trim(),
                approval.approved_at.trim(),
                approval.note.trim(),
                now_iso8601()
            ],
        )?;
        Ok(())
    }

    /// 全部换版确认记录（按写入顺序）
    pub fn list_approvals(&self) -> Result<Vec<Approval>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT to_version, from_version, slot_key, approver, approved_at, note
               FROM rule_version_approvals
              ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Approval {
                to_version: r.get(0)?,
                from_version: r.get(1)?,
                slot_key: r.get(2)?,
                approver: r.get(3)?,
                approved_at: r.get(4)?,
                note: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 换版确认索引（闸门用）
    pub fn approval_index(&self) -> Result<ApprovalIndex, EngineError> {
        Ok(ApprovalIndex {
            entries: self.list_approvals()?,
        })
    }

    // ————————————————————————————————————————————————————
    // 人工确认：疑似重复的确认记录（合并与否由经办人说了算）
    // ————————————————————————————————————————————————————

    /// 库里的一行（算上报身份用）。
    ///
    /// 比 [`RowMeta`] 全：带取值、单位、数值原文与口径版本 —— 上报身份（幂等键与
    /// 载荷摘要）要按这些字段算，缺一个就摘要对不上。
    ///
    /// 数值列存的是数值（存不到有限数值时为空），**原文另存 `numeric_raw`**：
    /// 「填坏了」与「没填」在库里分得开，摘要才算得准。
    pub fn list_stored_rows(&self, batch_ref: &str) -> Result<Vec<StoredRow>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT row_uid, source_ns, source_file, row_no, org_code, authority_state,
                    indicator_code, indicator_name, dept_code, dept_name, reporting_period,
                    business_key, numerator, denominator, value, unit, numeric_raw, rule_version
               FROM rows
              WHERE batch_ref = ?1
              ORDER BY source_ns, row_no",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            // 可空列一律取成空串：行可能缺列（缺列本身由口径规则报）
            let text = |idx: usize| -> rusqlite::Result<String> {
                Ok(r.get::<_, Option<String>>(idx)?.unwrap_or_default())
            };
            Ok(StoredRow {
                row_uid: r.get(0)?,
                source_ns: r.get(1)?,
                source_file: r.get(2)?,
                row_no: r.get::<_, i64>(3)? as usize,
                org_code: r.get(4)?,
                authority_state: r.get(5)?,
                indicator_code: text(6)?,
                indicator_name: text(7)?,
                dept_code: text(8)?,
                dept_name: text(9)?,
                reporting_period: text(10)?,
                business_key: text(11)?,
                numerator: r.get(12)?,
                denominator: r.get(13)?,
                value: r.get(14)?,
                unit: text(15)?,
                numeric_raw: text(16)?,
                rule_version: text(17)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 某批次已存在的上报提交（幂等上报用：同键同摘要即重复提交）。
    pub fn list_submissions(&self, batch_ref: &str) -> Result<Vec<SubmissionRecord>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT batch_ref, row_uid, idempotency_key, payload_digest, revision, status,
                    attempt_count, last_error_code, last_error_field, created_at, updated_at
               FROM submissions
              WHERE batch_ref = ?1
              ORDER BY idempotency_key, revision",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            Ok(SubmissionRecord {
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
            out.push(r?);
        }
        Ok(out)
    }

    /// 某一行在这一批里是否存在（确认前先核一下：确认错行等于白确认）
    pub fn row_exists(&self, batch_ref: &str, row_uid: &str) -> Result<bool, EngineError> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM rows WHERE batch_ref = ?1 AND row_uid = ?2 LIMIT 1")?;
        let mut rows = stmt.query(params![batch_ref, row_uid])?;
        Ok(rows.next()?.is_some())
    }

    /// 落一次上报提交（`submissions` 表）。
    ///
    /// 同一个 `(幂等键, 修订号)` 只落一条：重复落同一身份时**更新状态**，
    /// 不新开一行 —— 重复提交不是两次提交，是同一次提交又推了一遍。
    ///
    /// `attempt_count` 是「真推出去过几次」：幂等命中（本轮没发请求）不 +1，
    /// 免得台账上写着推了两次、其实只推过一次。
    pub fn upsert_submission(
        &mut self,
        identity: &SubmissionIdentity,
        status: &str,
        error_code: &str,
        error_field: &str,
        bump_attempt: bool,
    ) -> Result<(), EngineError> {
        if identity.idempotency_key.trim().is_empty() {
            return Err(EngineError::at(
                ErrorCode::IdempotencyMissing,
                "上报提交缺幂等键：不落库（幂等键只由引擎生成）",
                format!("row_uid={}", identity.row_uid),
            ));
        }
        let now = now_iso8601();
        let attempt = if bump_attempt { 1i64 } else { 0i64 };
        self.conn.execute(
            "INSERT INTO submissions (batch_ref, row_uid, idempotency_key, payload_digest, revision,
                                      status, attempt_count, last_error_code, last_error_field,
                                      created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?10, ?7, ?8, ?9, ?9)
             ON CONFLICT (idempotency_key, revision) DO UPDATE SET
                    status = excluded.status,
                    last_error_code = excluded.last_error_code,
                    last_error_field = excluded.last_error_field,
                    attempt_count = submissions.attempt_count + ?10,
                    updated_at = excluded.updated_at",
            params![
                identity.batch_ref,
                identity.row_uid,
                identity.idempotency_key,
                identity.payload_digest,
                identity.revision,
                status,
                error_code,
                error_field,
                now,
                attempt,
            ],
        )?;
        Ok(())
    }

    /// 落一条网关回执（`receipts` 表）。
    ///
    /// **幂等键 ↔ 回执 ID 一一对应**：先查后写，撞了报 `RECEIPT_CONFLICT` 并点名冲突的是哪一头 ——
    /// 已有回执**不覆盖**（网关那边已经受理过，覆盖等于把事实改掉）。
    pub fn insert_receipt(&mut self, receipt: &ReceiptRow) -> Result<(), EngineError> {
        if receipt.receipt_id.trim().is_empty() {
            return Err(EngineError::at(
                ErrorCode::GatewayResponseInvalid,
                "回执缺回执号：不落库（没有回执号就没有可追的上报事实）",
                format!("idempotency_key={}", receipt.idempotency_key),
            ));
        }
        // 同一 (幂等键, 修订号) 已有回执 → 冲突
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT receipt_id FROM receipts WHERE idempotency_key = ?1 AND revision = ?2",
                params![receipt.idempotency_key, receipt.revision],
                |r| r.get(0),
            )
            .ok();
        if let Some(previous) = existing {
            return Err(EngineError::at(
                ErrorCode::ReceiptConflict,
                format!(
                    "同一幂等键下已有回执（幂等键 {} · 修订 {} → 回执 {}），不再落第二条",
                    receipt.idempotency_key, receipt.revision, previous
                ),
                format!("idempotency_key={}", receipt.idempotency_key),
            ));
        }
        // 同一回执号已挂在别的幂等键上 → 冲突
        let owner: Option<String> = self
            .conn
            .query_row(
                "SELECT idempotency_key FROM receipts WHERE receipt_id = ?1",
                params![receipt.receipt_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(previous) = owner {
            return Err(EngineError::at(
                ErrorCode::ReceiptConflict,
                format!(
                    "回执号已被另一个幂等键占用（回执 {} ← 幂等键 {}），不再落第二条",
                    receipt.receipt_id, previous
                ),
                format!("receipt_id={}", receipt.receipt_id),
            ));
        }

        let now = now_iso8601();
        self.conn.execute(
            "INSERT INTO receipts (receipt_id, idempotency_key, revision, batch_ref, row_uid,
                                   status, accepted_at, error_code, error_field, message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                receipt.receipt_id,
                receipt.idempotency_key,
                receipt.revision,
                receipt.batch_ref,
                receipt.row_uid,
                receipt.status,
                receipt.accepted_at,
                receipt.error_code,
                receipt.error_field,
                receipt.message,
                now,
            ],
        )?;
        Ok(())
    }

    /// 某批次的全部回执（导出与核对用）
    pub fn list_receipts(&self, batch_ref: &str) -> Result<Vec<ReceiptRow>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT receipt_id, idempotency_key, revision, batch_ref, row_uid, status,
                    accepted_at, error_code, error_field, message
               FROM receipts
              WHERE batch_ref = ?1
              ORDER BY idempotency_key, revision",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
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
            out.push(r?);
        }
        Ok(out)
    }

    /// 某批次的行级上报状态（每行取**最新修订号**的那次提交）。
    ///
    /// 待重试的行也在这里：状态是「待重试」，幂等键留着，续跑时用得上。
    pub fn list_row_push_states(&self, batch_ref: &str) -> Result<Vec<RowPushState>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.row_uid, s.status, s.last_error_code, s.last_error_field,
                    COALESCE(r.receipt_id, ''), COALESCE(r.accepted_at, ''),
                    s.idempotency_key, s.revision, COALESCE(r.message, '')
               FROM submissions s
               LEFT JOIN receipts r
                 ON r.idempotency_key = s.idempotency_key AND r.revision = s.revision
              WHERE s.batch_ref = ?1
              ORDER BY s.row_uid, s.revision",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            Ok(RowPushState {
                row_uid: r.get(0)?,
                status: r.get(1)?,
                error_code: r.get(2)?,
                error_field: r.get(3)?,
                receipt_id: r.get(4)?,
                accepted_at: r.get(5)?,
                idempotency_key: r.get(6)?,
                revision: r.get(7)?,
                message: r.get(8)?,
            })
        })?;
        let mut all = Vec::new();
        for r in rows {
            all.push(r?);
        }
        // 同一行可能有多次修订（订正后重报），只留最新修订号的那次
        let mut latest: std::collections::BTreeMap<String, RowPushState> =
            std::collections::BTreeMap::new();
        for state in all {
            let replace = match latest.get(&state.row_uid) {
                Some(prev) => state.revision >= prev.revision,
                None => true,
            };
            if replace {
                latest.insert(state.row_uid.clone(), state);
            }
        }
        Ok(latest.into_values().collect())
    }

    /// 某批次**待重试**的行（退避重试耗尽的行）—— 未上报名单的数据来源。
    ///
    /// 只取状态「待重试」的行：被退回（4xx）的行要改数据才能再报，
    /// 不该混进「等网关恢复就能重推」的待办清单里。
    pub fn list_pending_retry_rows(&self, batch_ref: &str) -> Result<Vec<PendingRetryRow>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT row_uid, idempotency_key, revision, status, last_error_code,
                    last_error_field, attempt_count, updated_at
               FROM submissions
              WHERE batch_ref = ?1 AND status = ?2
              ORDER BY row_uid, revision",
        )?;
        let rows = stmt.query_map(
            params![batch_ref, crate::push::executor::STATUS_RETRYABLE],
            |r| {
                Ok(PendingRetryRow {
                    row_uid: r.get(0)?,
                    idempotency_key: r.get(1)?,
                    revision: r.get(2)?,
                    status: r.get(3)?,
                    error_code: r.get(4)?,
                    error_field: r.get(5)?,
                    attempt_count: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 落一条「上报退回」的问题（写回原表「问题说明（含依据）」列）。
    ///
    /// 同一行同一幂等键只留一条：修正后重报会换修订号，旧的那条由
    /// [`Repo::clear_push_issues`] 清掉。
    pub fn upsert_push_issue(
        &mut self,
        batch_ref: &str,
        row_uid: &str,
        idempotency_key: &str,
        message: &str,
        error_code: &str,
        error_field: &str,
    ) -> Result<(), EngineError> {
        let issue = crate::push::receipt::push_issue(
            row_uid,
            idempotency_key,
            message,
            error_code,
            error_field,
        );
        let now = now_iso8601();
        self.conn.execute(
            "INSERT INTO issues (batch_ref, row_uid, rule_id, issue_kind, severity, message,
                                 basis, issue_key, missing_columns, error_code, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (batch_ref, row_uid, rule_id, issue_key) DO UPDATE SET
                    message = excluded.message,
                    error_code = excluded.error_code,
                    missing_columns = excluded.missing_columns,
                    created_at = excluded.created_at",
            params![
                batch_ref,
                row_uid,
                issue.rule_id,
                issue.issue_kind,
                issue.severity,
                issue.message,
                issue.basis,
                issue.issue_key,
                issue.missing_columns.join("、"),
                issue.error_code,
                now,
            ],
        )?;
        Ok(())
    }

    /// 清掉某一行之前留下的「上报退回」问题（这一行已经报上去了）。
    pub fn clear_push_issues(&mut self, batch_ref: &str, row_uid: &str) -> Result<usize, EngineError> {
        let n = self.conn.execute(
            "DELETE FROM issues WHERE batch_ref = ?1 AND row_uid = ?2 AND rule_id LIKE 'R-PUSH-%'",
            params![batch_ref, row_uid],
        )?;
        Ok(n)
    }

    /// 某批次留着的「上报退回」问题（写回原表「问题说明（含依据）」列用）。
    ///
    /// 这些问题**不是**本轮校验算出来的 —— 是上一轮上报被网关退回时留下的，
    /// 重跑校验不会抹掉它（见 [`Repo::insert_issues`]）。
    pub fn list_push_issues(&self, batch_ref: &str) -> Result<Vec<Issue>, EngineError> {
        let mut stmt = self.conn.prepare(
            "SELECT row_uid, rule_id, issue_kind, severity, message, basis, issue_key,
                    missing_columns, error_code
               FROM issues
              WHERE batch_ref = ?1 AND rule_id LIKE 'R-PUSH-%'
              ORDER BY row_uid, rule_id, issue_key",
        )?;
        let rows = stmt.query_map(params![batch_ref], |r| {
            let missing: String = r.get(7)?;
            Ok(Issue {
                row_uid: r.get(0)?,
                rule_id: r.get(1)?,
                issue_kind: r.get(2)?,
                severity: r.get(3)?,
                message: r.get(4)?,
                basis: r.get(5)?,
                issue_key: r.get(6)?,
                missing_columns: missing
                    .split('、')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect(),
                error_code: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 确认记录（按写入顺序；`batch_ref` 为空则取全部批次）
    pub fn list_confirmations(
        &self,
        batch_ref: Option<&str>,
    ) -> Result<Vec<Confirmation>, EngineError> {
        let (sql, params): (&str, Vec<&str>) = match batch_ref {
            Some(b) if !b.trim().is_empty() => (
                "SELECT batch_ref, row_uid, action, confirmed_by, confirmed_at, note, revoked_by, revoked_at
                   FROM confirmations WHERE batch_ref = ?1 ORDER BY id",
                vec![b.trim()],
            ),
            _ => (
                "SELECT batch_ref, row_uid, action, confirmed_by, confirmed_at, note, revoked_by, revoked_at
                   FROM confirmations ORDER BY id",
                vec![],
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
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
            out.push(r?);
        }
        Ok(out)
    }

    /// 已被确认的行键集合（供写闸门用：只有确认过的行才允许上报）
    ///
    /// 撤销过的确认**不算** —— 撤销是追加一条 `revoke`，
    /// 撤销之后要重新确认才能再上报。
    ///
    /// 判定规则见自由函数 [`effective_confirmed_row_uids`]（唯一出处）。
    pub fn confirmed_row_uids(
        &self,
        batch_ref: &str,
    ) -> Result<std::collections::BTreeSet<String>, EngineError> {
        effective_confirmed_row_uids(self.conn(), batch_ref)
    }

    /// **实际用过**的口径版本清单（换版闸门判断「这是不是换版」的依据）
    pub fn applied_rule_versions(&self) -> Result<Vec<String>, EngineError> {
        let mut stmt = self
            .conn
            .prepare("SELECT rule_set_version FROM applied_rule_sets ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 记下一个口径版本已被实际采用（重复调用不重复记）
    pub fn record_applied_rule_set(&mut self, version: &str) -> Result<bool, EngineError> {
        let version = version.trim();
        if version.is_empty() {
            return Ok(false);
        }
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO applied_rule_sets (rule_set_version, applied_at, note, created_at)
             VALUES (?1, ?2, '', ?3)",
            params![version, now_iso8601(), now_iso8601()],
        )?;
        Ok(changed > 0)
    }
}

/// 缺失列清单的拆分（库里用「、」连接存，取出时拆回清单）
fn split_missing_columns(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split('、')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 是否是「权威键重复」错误（唯一索引 `idx_rows_authority` 拦下的）
///
/// SQLite 的报错文案给的是**被约束的列**（`rows.org_code, rows.reporting_period, …`），
/// 不是索引名，所以这里按「约束冲突 + 命中权威键列」判定。
fn is_authority_conflict(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(err, msg) => {
            if err.code != rusqlite::ErrorCode::ConstraintViolation {
                return false;
            }
            let m = msg.as_deref().unwrap_or("");
            m.contains("idx_rows_authority")
                || (m.contains("org_code")
                    && m.contains("reporting_period")
                    && m.contains("indicator_code")
                    && m.contains("dept_code")
                    && m.contains("business_key"))
        }
        _ => false,
    }
}

/// 数值列原文：保留「空」与「非数值」的差别，规则层据此区分缺失与格式错误。
///
/// 键与本机库列名同名（`numerator` / `denominator` / `value`）：
/// 口径规则用 `json_extract(numeric_raw, '$.<列名>')` 取原文，键名不必再译一遍。
/// 取值去首尾空白 —— 空白即「没填」，全角空格、Tab 这类「看着空」的写法与空串同义。
fn numeric_raw_json(row: &SourceRow) -> String {
    serde_json::json!({
        "numerator": row.get_trimmed(Field::Numerator).unwrap_or(""),
        "denominator": row.get_trimmed(Field::Denominator).unwrap_or(""),
        "value": row.get_trimmed(Field::Value).unwrap_or(""),
    })
    .to_string()
}

/// 运行时间戳（本地时区，`YYYY-MM-DD HH:mm:ss`）
pub fn now_iso8601() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 一条「当前有效的确认」的摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveConfirmation {
    /// 确认记录 id
    pub id: i64,
    /// 确认人
    pub confirmed_by: String,
    /// 确认时间
    pub confirmed_at: String,
}

/// 当前**有效**的确认记录：确认过，且**之后没有**撤销记录。
///
/// # 判定规则（唯一出处）
///
/// 确认与撤销都是追加写，所以「这一行现在确认了没有」= 存在一条 `confirm` 记录，
/// 且这条记录之后**没有**任何 `revoke` 记录。
///
/// - 确认 → 有效；
/// - 确认 → 撤销 → **无效**（要重新确认）；
/// - 确认 → 撤销 → 再确认 → **有效**（重新确认过就算数）。
///
/// 早先的写法是「有过撤销记录就一票否决」，那样「撤销后重新确认」永远不成立 ——
/// 撤销一次，这一行就再也报不上去。
pub fn effective_confirmation(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<Option<EffectiveConfirmation>, EngineError> {
    let rec = conn
        .query_row(
            "SELECT c.id, c.confirmed_by, c.confirmed_at
               FROM confirmations c
              WHERE c.batch_ref = ?1 AND c.row_uid = ?2 AND c.action = 'confirm'
                AND NOT EXISTS (SELECT 1 FROM confirmations r
                                 WHERE r.batch_ref = c.batch_ref
                                   AND r.row_uid = c.row_uid
                                   AND r.action = 'revoke'
                                   AND r.id > c.id)
              ORDER BY c.id DESC
              LIMIT 1",
            params![batch_ref.trim(), row_uid.trim()],
            |r| {
                Ok(EffectiveConfirmation {
                    id: r.get(0)?,
                    confirmed_by: r.get(1)?,
                    confirmed_at: r.get(2)?,
                })
            },
        )
        .ok();
    Ok(rec)
}

/// 最近一条撤销记录的 id（没有撤销过则 `None`）。
pub fn last_revoke_id(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<Option<i64>, EngineError> {
    let id = conn
        .query_row(
            "SELECT id FROM confirmations
              WHERE batch_ref = ?1 AND row_uid = ?2 AND action = 'revoke'
              ORDER BY id DESC LIMIT 1",
            params![batch_ref.trim(), row_uid.trim()],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    Ok(id)
}

/// 当前有效的已确认行键集合（写闸门用）。
///
/// 判定规则与 [`effective_confirmation`] 同源，见那里的说明。
pub fn effective_confirmed_row_uids(
    conn: &Connection,
    batch_ref: &str,
) -> Result<std::collections::BTreeSet<String>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.row_uid
           FROM confirmations c
          WHERE c.batch_ref = ?1 AND c.action = 'confirm'
            AND NOT EXISTS (SELECT 1 FROM confirmations r
                             WHERE r.batch_ref = c.batch_ref
                               AND r.row_uid = c.row_uid
                               AND r.action = 'revoke'
                               AND r.id > c.id)",
    )?;
    let rows = stmt.query_map(params![batch_ref.trim()], |r| r.get::<_, String>(0))?;
    let mut out = std::collections::BTreeSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}
