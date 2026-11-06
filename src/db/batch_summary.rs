//! 批次汇总的落库层：**一次事务**写快照 + 历史。
//!
//! # 这层解决什么
//!
//! 批次汇总说的是「这批整体算不算数」。它由两部分组成：
//!
//! - **批次计数与汇总值**：计划步数、尝试数、受理数、幂等命中数、被退回数、
//!   待重试数、未发出数、未执行数，以及已上报行的填报值 / 分子 / 分母合计；
//! - **批次状态**：这批现在是「已校验」「上报完成」「批末回滚」还是「部分完成」。
//!
//! # 为什么必须一次事务
//!
//! 汇总的用处是给经办人判断「这批能不能算数」。**半截的汇总比没有汇总更坏** ——
//! 「已上报 3 行」配「被退回 0 行」看着像一批干净的账，实际只写进去一半，
//! 经办人会照着它去催科室。
//!
//! 所以：批次计数、汇总值与批次状态在**同一个事务**里写进去。
//! 中途失败（历史表撞唯一索引、写库出错）→ 整次事务回滚，
//! 快照回到批前值，**不留半截状态**。
//!
//! # 快照与历史的关系
//!
//! - `batch_summaries`：一个批次一行的**快照**，永远是最新的那版；
//! - `batch_summary_history`：**append-only** 的历史版，每次落库追加一行。
//!
//! 修订号由快照表的当前修订号 +1 得出（同一批次的历史只往后长）。
//! 历史表对 `(batch_ref, revision)` 唯一 —— 同一修订号写第二次会撞唯一索引，
//! 整次事务回滚。这是**重放防护**：同一版汇总不会被写两遍。
//!
//! # 批末回滚与这张表
//!
//! 批末条件回滚（见 [`crate::push::executor`]）时，**批次汇总回到批前值** ——
//! 整批没跑完就不落账。行级回执不受影响：网关侧已受理的事实不可撤销。
//! 这条区分在业务层（[`crate::push::batch`]）做决定，本层只负责把结果原样存下来。

use rusqlite::params;

use crate::db::repo::{now_iso8601, Repo};
use crate::errors::{EngineError, ErrorCode};

/// 批次汇总记录（`batch_summaries` 的一行 / `batch_summary_history` 的一行）。
///
/// 字段与 spec.md「最小数据格式」一致：只含监管指标行与科室级数据，
/// **不落患者可识别明细**。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSummaryRecord {
    /// 批次号
    pub batch_ref: String,
    /// 计划里的总步数
    pub planned_steps: i64,
    /// 真正发过请求的步数
    pub attempted: i64,
    /// 网关受理的行数
    pub accepted: i64,
    /// 幂等命中的行数（此前已上报，本轮没推）
    pub idempotent_hit: i64,
    /// 已上报行数（受理 + 幂等命中）
    pub reported: i64,
    /// 被退回的行数（4xx）
    pub rejected: i64,
    /// 待重试的行数（5xx / 超时 / 连不上）
    pub retryable: i64,
    /// 未发出的行数（本行数据不合格）
    pub blocked: i64,
    /// 回执待人工核对的行数
    pub invalid_receipt: i64,
    /// 未执行的行数（因前一步失败）
    pub not_executed: i64,
    /// 问题项数（该批次问题条数）
    pub issue_count: i64,
    /// 已上报行的填报值合计
    pub sum_value: f64,
    /// 已上报行的分子合计
    pub sum_numerator: f64,
    /// 已上报行的分母合计
    pub sum_denominator: f64,
    /// 批次状态（已校验 / 上报完成 / 批末回滚 / 部分完成）
    pub batch_status: String,
    /// 这批结论按哪套口径算的
    pub rule_version: String,
    /// 修订号（由落库层算，写入时忽略传入值）
    pub revision: i64,
    /// 首次落库时间
    pub created_at: String,
    /// 最近一次落库时间
    pub updated_at: String,
}

impl BatchSummaryRecord {
    /// 一条空汇总（批次还没跑过任何一步时用）
    pub fn empty(batch_ref: &str) -> BatchSummaryRecord {
        BatchSummaryRecord {
            batch_ref: batch_ref.to_string(),
            planned_steps: 0,
            attempted: 0,
            accepted: 0,
            idempotent_hit: 0,
            reported: 0,
            rejected: 0,
            retryable: 0,
            blocked: 0,
            invalid_receipt: 0,
            not_executed: 0,
            issue_count: 0,
            sum_value: 0.0,
            sum_numerator: 0.0,
            sum_denominator: 0.0,
            batch_status: String::new(),
            rule_version: String::new(),
            revision: 0,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// 一行可读摘要（`regval status` 用）
    pub fn to_line(&self) -> String {
        format!(
            "{} · 批次 {} · 状态 {} · 修订 {} · 已上报 {} 行 · 被退回 {} 行 · 待重试 {} 行 · 问题 {} 项",
            self.updated_at,
            self.batch_ref,
            self.batch_status,
            self.revision,
            self.reported,
            self.rejected,
            self.retryable,
            self.issue_count
        )
    }

    /// JSON 产物（可核对）
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "batch_ref": self.batch_ref,
            "planned_steps": self.planned_steps,
            "attempted": self.attempted,
            "accepted": self.accepted,
            "idempotent_hit": self.idempotent_hit,
            "reported": self.reported,
            "rejected": self.rejected,
            "retryable": self.retryable,
            "blocked": self.blocked,
            "invalid_receipt": self.invalid_receipt,
            "not_executed": self.not_executed,
            "issue_count": self.issue_count,
            "sum_value": self.sum_value,
            "sum_numerator": self.sum_numerator,
            "sum_denominator": self.sum_denominator,
            "batch_status": self.batch_status,
            "rule_version": self.rule_version,
            "revision": self.revision,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

impl Repo {
    /// 落一次批次汇总 —— **一次事务**：快照 + 历史。
    ///
    /// 事务里做两件事，顺序是**先快照、后历史**：
    ///
    /// 1. 写/更新 `batch_summaries` 的快照行（批次计数、汇总值、批次状态）；
    /// 2. 追加一行 `batch_summary_history`（append-only）。
    ///
    /// 第 2 步撞 `(batch_ref, revision)` 唯一索引时（同一版汇总被写第二次），
    /// **整次事务回滚** —— 第 1 步写进去的快照也一起撤销，快照回到批前值。
    /// 这就是「半途失败不留半截状态」的落法。
    ///
    /// 修订号由快照表当前修订号 +1 得出；传入记录的 `revision` 字段被忽略
    /// （修订号由库说了算，不接受调用方指定 —— 否则重放能覆盖历史）。
    ///
    /// 返回本次落库的修订号。
    pub fn save_batch_summary(&mut self, rec: &BatchSummaryRecord) -> Result<i64, EngineError> {
        let batch_ref = rec.batch_ref.trim();
        if batch_ref.is_empty() {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                "批次汇总落库失败：批次号不能为空",
                "batch_ref",
            ));
        }

        let tx = self.conn_mut().transaction()?;

        // 批前修订号（同一批次的历史只往后长）
        let current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision), 0) FROM batch_summaries WHERE batch_ref = ?1",
            params![batch_ref],
            |r| r.get(0),
        )?;
        let revision = current + 1;
        let now = now_iso8601();

        // —— 第 1 步：快照 ——
        tx.execute(
            r#"
INSERT INTO batch_summaries (
    batch_ref, planned_steps, attempted, accepted, idempotent_hit, reported,
    rejected, retryable, blocked, invalid_receipt, not_executed, issue_count,
    sum_value, sum_numerator, sum_denominator, batch_status, rule_version,
    revision, created_at, updated_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
ON CONFLICT(batch_ref) DO UPDATE SET
    planned_steps   = excluded.planned_steps,
    attempted       = excluded.attempted,
    accepted        = excluded.accepted,
    idempotent_hit  = excluded.idempotent_hit,
    reported        = excluded.reported,
    rejected        = excluded.rejected,
    retryable       = excluded.retryable,
    blocked         = excluded.blocked,
    invalid_receipt = excluded.invalid_receipt,
    not_executed    = excluded.not_executed,
    issue_count     = excluded.issue_count,
    sum_value       = excluded.sum_value,
    sum_numerator   = excluded.sum_numerator,
    sum_denominator = excluded.sum_denominator,
    batch_status    = excluded.batch_status,
    rule_version    = excluded.rule_version,
    revision        = excluded.revision,
    updated_at      = excluded.updated_at
"#,
            params![
                batch_ref,
                rec.planned_steps,
                rec.attempted,
                rec.accepted,
                rec.idempotent_hit,
                rec.reported,
                rec.rejected,
                rec.retryable,
                rec.blocked,
                rec.invalid_receipt,
                rec.not_executed,
                rec.issue_count,
                rec.sum_value,
                rec.sum_numerator,
                rec.sum_denominator,
                rec.batch_status,
                rec.rule_version,
                revision,
                now,
                now,
            ],
        )?;

        // —— 第 2 步：历史（append-only）——
        // 这一步失败（同一修订号写第二次）→ 上面的快照一起回滚。
        let history = tx.execute(
            r#"
INSERT INTO batch_summary_history (
    batch_ref, revision, batch_status, planned_steps, attempted, accepted,
    idempotent_hit, reported, rejected, retryable, blocked, invalid_receipt,
    not_executed, issue_count, sum_value, sum_numerator, sum_denominator,
    rule_version, written_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
"#,
            params![
                batch_ref,
                revision,
                rec.batch_status,
                rec.planned_steps,
                rec.attempted,
                rec.accepted,
                rec.idempotent_hit,
                rec.reported,
                rec.rejected,
                rec.retryable,
                rec.blocked,
                rec.invalid_receipt,
                rec.not_executed,
                rec.issue_count,
                rec.sum_value,
                rec.sum_numerator,
                rec.sum_denominator,
                rec.rule_version,
                now,
            ],
        );
        if let Err(e) = history {
            // 同一修订号写第二次 = 重放。整次事务在这里回滚（tx 被丢弃），
            // 快照回到批前值 —— 报一个机器可读的错误码，不静默吞掉。
            return Err(match is_unique_violation(&e) {
                true => EngineError::at(
                    ErrorCode::BatchSummaryConflict,
                    format!(
                        "批次 {} 的第 {} 版汇总已经写过一次（重放），本次事务已整笔回滚，汇总回到批前值",
                        batch_ref, revision
                    ),
                    format!("batch_ref={} revision={}", batch_ref, revision),
                ),
                false => EngineError::at(
                    ErrorCode::Db,
                    format!("批次汇总历史写入失败：{}", e),
                    format!("batch_ref={} revision={}", batch_ref, revision),
                ),
            });
        }

        tx.commit()?;
        Ok(revision)
    }

    /// 取某批次的汇总快照（最新一版）。
    ///
    /// 库里没有 → `Ok(None)`（**不是错误**）：这批还没落过汇总。
    pub fn batch_summary(&self, batch_ref: &str) -> Result<Option<BatchSummaryRecord>, EngineError> {
        let mut stmt = self.conn().prepare(
            r#"
SELECT batch_ref, planned_steps, attempted, accepted, idempotent_hit, reported,
       rejected, retryable, blocked, invalid_receipt, not_executed, issue_count,
       sum_value, sum_numerator, sum_denominator, batch_status, rule_version,
       revision, created_at, updated_at
  FROM batch_summaries WHERE batch_ref = ?1
"#,
        )?;
        let mut rows = stmt.query(params![batch_ref])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(read_summary_row(row)?))
    }

    /// 最近的批次汇总（按更新时间倒序）。
    pub fn list_batch_summaries(&self, limit: usize) -> Result<Vec<BatchSummaryRecord>, EngineError> {
        let mut stmt = self.conn().prepare(
            r#"
SELECT batch_ref, planned_steps, attempted, accepted, idempotent_hit, reported,
       rejected, retryable, blocked, invalid_receipt, not_executed, issue_count,
       sum_value, sum_numerator, sum_denominator, batch_status, rule_version,
       revision, created_at, updated_at
  FROM batch_summaries ORDER BY updated_at DESC, batch_ref ASC LIMIT ?1
"#,
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(read_summary_row(row)?);
        }
        Ok(out)
    }

    /// 某批次的汇总历史（按修订号升序）—— 汇总被改过几次一目了然。
    pub fn batch_summary_history(
        &self,
        batch_ref: &str,
    ) -> Result<Vec<BatchSummaryRecord>, EngineError> {
        let mut stmt = self.conn().prepare(
            r#"
SELECT batch_ref, planned_steps, attempted, accepted, idempotent_hit, reported,
       rejected, retryable, blocked, invalid_receipt, not_executed, issue_count,
       sum_value, sum_numerator, sum_denominator, batch_status, rule_version,
       revision, written_at, written_at
  FROM batch_summary_history WHERE batch_ref = ?1 ORDER BY revision ASC
"#,
        )?;
        let mut rows = stmt.query(params![batch_ref])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(read_summary_row(row)?);
        }
        Ok(out)
    }

    /// 某批次的问题条数（口径问题 + 上报退回问题）。
    pub fn count_issues(&self, batch_ref: &str) -> Result<i64, EngineError> {
        let n = self.conn().query_row(
            "SELECT COUNT(*) FROM issues WHERE batch_ref = ?1",
            params![batch_ref],
            |r| r.get(0),
        )?;
        Ok(n)
    }
}

/// 是不是唯一约束冲突（重放同一版汇总）
fn is_unique_violation(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(failure, _) => {
            failure.code == rusqlite::ErrorCode::ConstraintViolation
        }
        _ => false,
    }
}

/// 读一行汇总（快照表与历史表的列序一致）
fn read_summary_row(row: &rusqlite::Row<'_>) -> Result<BatchSummaryRecord, EngineError> {
    Ok(BatchSummaryRecord {
        batch_ref: row.get(0)?,
        planned_steps: row.get(1)?,
        attempted: row.get(2)?,
        accepted: row.get(3)?,
        idempotent_hit: row.get(4)?,
        reported: row.get(5)?,
        rejected: row.get(6)?,
        retryable: row.get(7)?,
        blocked: row.get(8)?,
        invalid_receipt: row.get(9)?,
        not_executed: row.get(10)?,
        issue_count: row.get(11)?,
        sum_value: row.get(12)?,
        sum_numerator: row.get(13)?,
        sum_denominator: row.get(14)?,
        batch_status: row.get(15)?,
        rule_version: row.get(16)?,
        revision: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
    })
}
