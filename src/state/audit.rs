//! 审计链：append-only（只追加不修改，更正以新记录表达）。
//!
//! # 这一层回答什么问题
//!
//! 「这行为什么被合并了」「这一行是谁确认的」「这一行报上去过几版」——
//! 事后要能一句话答上来，证据就得在**当时**记下来，而不是靠人回忆。
//!
//! # 只追加，不修改
//!
//! `audit_log` 表**只有 INSERT**。写错了不删、不改 —— 再追加一条说明更正。
//! 「更正以新记录表达」是审计链的基本规矩：改历史比留历史更危险。
//!
//! # 记什么、不记什么
//!
//! 记：谁（`actor`）、什么时候（`ts`）、哪一行（`row_uid`）、哪一批（`batch_ref`）、
//! 做了什么（`event`）、补充说明（`detail`）。
//!
//! 不记：患者可识别明细。这一层只有指标级与科室级的标识 —— 监管上报的数据
//! 本身就不含患者明细，审计链也不该成为它的例外。
//!
//! # 本轮范围
//!
//! 写侧：确认、撤销、合并、上报结果（哪一批推了什么）、状态库恢复。
//! 读侧：按行回放、按批次回放、幂等键 ↔ 回执 ID 的对应关系核对。
//!
//! # 回放是给谁用的
//!
//! 半年后有人问「这一行为什么报上去了」「这一批到底推了哪些行」，
//! 答案不该靠回忆，也不该靠翻日志文件 —— 一次查询就该有。
//! 按行回放给的是**这一行的完整轨迹**（谁确认的 → 什么时候推的 → 拿到哪个回执）；
//! 按批次回放给的是**这一批推了什么**（每一步的成败与错误码）。

use rusqlite::{params, Connection};

use crate::errors::{EngineError, ErrorCode};

/// 事件：人工确认（复核通过，可上报）
pub const EVENT_CONFIRM: &str = "confirm";
/// 事件：人工撤销确认
pub const EVENT_REVOKE: &str = "revoke";
/// 事件：疑似重复合并（逐条勾选后执行）
pub const EVENT_MERGE: &str = "merge";
/// 事件：写闸门拦下一行（未确认就推）
pub const EVENT_PUSH_BLOCKED: &str = "push_blocked";
/// 事件：上报被网关受理（拿到回执）
pub const EVENT_PUSH_ACCEPTED: &str = "push.accepted";
/// 事件：上报被网关退回（4xx）
pub const EVENT_PUSH_REJECTED: &str = "push.rejected";
/// 事件：上报失败但可重试（5xx / 超时 / 连不上）
pub const EVENT_PUSH_RETRYABLE: &str = "push.retryable";
/// 事件：幂等命中（此前已上报，本轮未重复推送）
pub const EVENT_PUSH_IDEMPOTENT_HIT: &str = "push.idempotent_hit";
/// 事件：状态库恢复（恢复动作本身也留痕）
pub const EVENT_RECOVER: &str = "recover.restored";

/// 一条审计记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    /// 发生时间（`YYYY-MM-DD HH:mm:ss`）
    pub ts: String,
    /// 批次号
    pub batch_ref: String,
    /// 行键（批次级动作可为空串）
    pub row_uid: String,
    /// 事件名（见本模块的事件常量）
    pub event: String,
    /// 谁做的（人名或命令名）
    pub actor: String,
    /// 补充说明
    pub detail: String,
}

impl AuditRecord {
    /// 造一条审计记录（时间取当前）。
    pub fn new(
        batch_ref: &str,
        row_uid: &str,
        event: &str,
        actor: &str,
        detail: &str,
    ) -> AuditRecord {
        AuditRecord {
            ts: crate::db::repo::now_iso8601(),
            batch_ref: batch_ref.trim().to_string(),
            row_uid: row_uid.trim().to_string(),
            event: event.trim().to_string(),
            actor: actor.trim().to_string(),
            detail: detail.trim().to_string(),
        }
    }
}

/// 追加一条审计记录（**只追加**）。
///
/// 返回新记录的 id（便于调用方接着引用）。
pub fn append(conn: &Connection, record: &AuditRecord) -> Result<i64, EngineError> {
    if record.event.trim().is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "审计记录必须写事件名",
            format!("batch_ref={} row_uid={}", record.batch_ref, record.row_uid),
        ));
    }
    conn.execute(
        "INSERT INTO audit_log (ts, batch_ref, row_uid, event, actor, detail, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?1)",
        params![
            record.ts,
            record.batch_ref,
            record.row_uid,
            record.event,
            record.actor,
            record.detail,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 审计链里某一行的事件条数（按行核对用）。
pub fn count_for_row(conn: &Connection, row_uid: &str) -> Result<usize, EngineError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE row_uid = ?1",
        params![row_uid.trim()],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// 审计链里某一批次的事件条数（按批次核对用）。
pub fn count_for_batch(conn: &Connection, batch_ref: &str) -> Result<usize, EngineError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM audit_log WHERE batch_ref = ?1",
        params![batch_ref.trim()],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// 某一行的事件清单（按时间顺序）—— 任务 37 的回放读侧的底子。
pub fn events_for_row(conn: &Connection, row_uid: &str) -> Result<Vec<(String, String, String)>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT ts, event, actor FROM audit_log WHERE row_uid = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![row_uid.trim()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 一条审计事件（读侧回放用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// 记录序号（append-only 的先后顺序）
    pub id: i64,
    /// 发生时间
    pub ts: String,
    /// 批次号
    pub batch_ref: String,
    /// 行键（批次级动作为空串）
    pub row_uid: String,
    /// 事件名
    pub event: String,
    /// 谁做的
    pub actor: String,
    /// 补充说明
    pub detail: String,
}

impl AuditEntry {
    /// 一行可读说法（终端回放用）
    pub fn to_line(&self) -> String {
        format!(
            "{}  {}  {}  {}",
            self.ts,
            if self.row_uid.is_empty() {
                "（批次级）"
            } else {
                &self.row_uid
            },
            self.event,
            self.detail
        )
    }
}

/// 按行回放：这一行的完整轨迹（谁确认的 → 什么时候推的 → 拿到哪个回执）。
///
/// 按写入顺序（`id` 升序）返回 —— 审计链的价值就在顺序上。
pub fn replay_row(conn: &Connection, row_uid: &str) -> Result<Vec<AuditEntry>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, batch_ref, row_uid, event, actor, detail
           FROM audit_log
          WHERE row_uid = ?1
          ORDER BY id",
    )?;
    let rows = stmt.query_map(params![row_uid.trim()], |r| {
        Ok(AuditEntry {
            id: r.get(0)?,
            ts: r.get(1)?,
            batch_ref: r.get(2)?,
            row_uid: r.get(3)?,
            event: r.get(4)?,
            actor: r.get(5)?,
            detail: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 按批次回放：这一批推了什么（每一步的成败与错误码）。
pub fn replay_batch(conn: &Connection, batch_ref: &str) -> Result<Vec<AuditEntry>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT id, ts, batch_ref, row_uid, event, actor, detail
           FROM audit_log
          WHERE batch_ref = ?1
          ORDER BY id",
    )?;
    let rows = stmt.query_map(params![batch_ref.trim()], |r| {
        Ok(AuditEntry {
            id: r.get(0)?,
            ts: r.get(1)?,
            batch_ref: r.get(2)?,
            row_uid: r.get(3)?,
            event: r.get(4)?,
            actor: r.get(5)?,
            detail: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 一条「幂等键 ↔ 回执 ID」的对应关系。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyReceiptPair {
    /// 幂等键
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
    /// 回执 ID
    pub receipt_id: String,
    /// 批次号
    pub batch_ref: String,
    /// 行键
    pub row_uid: String,
    /// 网关给的状态
    pub status: String,
}

/// 幂等键 ↔ 回执 ID 的对应关系清单（库里实际的，一一对应）。
pub fn idempotency_receipt_pairs(
    conn: &Connection,
) -> Result<Vec<IdempotencyReceiptPair>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT idempotency_key, revision, receipt_id, batch_ref, row_uid, status
           FROM receipts
          ORDER BY idempotency_key, revision",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(IdempotencyReceiptPair {
            idempotency_key: r.get(0)?,
            revision: r.get(1)?,
            receipt_id: r.get(2)?,
            batch_ref: r.get(3)?,
            row_uid: r.get(4)?,
            status: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 对应关系的核对结果。
///
/// 一一对应不是靠自觉，是靠两个唯一索引兜住；这一层把「有没有破」查出来：
///
/// - `orphan_receipts`：有回执却查不到对应提交 —— 回执落库了但提交记录不在（异常）；
/// - `missing_receipts`：提交状态是「已上报」却查不到回执 —— **最危险的一种**
///   （下次重跑会以为没报过，又推一遍）；
/// - `duplicate_receipt_ids`：同一个回执号挂在多个幂等键上 —— 一一对应破了。
#[derive(Debug, Clone, Default)]
pub struct MappingReport {
    /// 对应关系清单
    pub pairs: Vec<IdempotencyReceiptPair>,
    /// 有回执但没有提交
    pub orphan_receipts: Vec<String>,
    /// 已上报但没有回执
    pub missing_receipts: Vec<String>,
    /// 回执号重复
    pub duplicate_receipt_ids: Vec<String>,
}

impl MappingReport {
    /// 对应关系是否完好（一一对应没破）
    pub fn is_intact(&self) -> bool {
        self.orphan_receipts.is_empty()
            && self.missing_receipts.is_empty()
            && self.duplicate_receipt_ids.is_empty()
    }

    /// 一行可读说法（终端用）
    pub fn summary_line(&self) -> String {
        format!(
            "幂等键 ↔ 回执 ID：{} 对对应 · 孤儿回执 {} 条 · 缺回执 {} 条 · 回执号重复 {} 条 · {}",
            self.pairs.len(),
            self.orphan_receipts.len(),
            self.missing_receipts.len(),
            self.duplicate_receipt_ids.len(),
            if self.is_intact() {
                "一一对应成立"
            } else {
                "一一对应有破口（见明细）"
            }
        )
    }
}

/// 核对幂等键 ↔ 回执 ID 的一一对应关系。
pub fn verify_idempotency_mapping(conn: &Connection) -> Result<MappingReport, EngineError> {
    let mut report = MappingReport {
        pairs: idempotency_receipt_pairs(conn)?,
        ..MappingReport::default()
    };

    // 有回执却没有对应提交
    let mut stmt = conn.prepare(
        "SELECT r.idempotency_key, r.revision
           FROM receipts r
           LEFT JOIN submissions s
             ON s.idempotency_key = r.idempotency_key AND s.revision = r.revision
          WHERE s.idempotency_key IS NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(format!("{}@{}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        report.orphan_receipts.push(row?);
    }

    // 提交说「已上报」却查不到回执
    let mut stmt = conn.prepare(
        "SELECT s.idempotency_key, s.revision
           FROM submissions s
           LEFT JOIN receipts r
             ON r.idempotency_key = s.idempotency_key AND r.revision = s.revision
          WHERE r.idempotency_key IS NULL
            AND s.status IN ('已上报', 'accepted')",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(format!("{}@{}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        report.missing_receipts.push(row?);
    }

    // 同一个回执号挂在多个幂等键上
    let mut stmt = conn.prepare(
        "SELECT receipt_id, COUNT(*) FROM receipts GROUP BY receipt_id HAVING COUNT(*) > 1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(format!("{}×{}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        report.duplicate_receipt_ids.push(row?);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 事件名不得为空() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::db::repo::Repo::open(&dir.path().join("state.db")).unwrap();
        let rec = AuditRecord {
            ts: "2026-01-01 00:00:00".to_string(),
            batch_ref: "b-1".to_string(),
            row_uid: "r-1".to_string(),
            event: "  ".to_string(),
            actor: "信息科甲".to_string(),
            detail: String::new(),
        };
        let err = append(repo.conn(), &rec).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }

    #[test]
    fn 追加后能按行与按批次数出条数() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::db::repo::Repo::open(&dir.path().join("state.db")).unwrap();
        let rec = AuditRecord::new("b-1", "r-1", EVENT_CONFIRM, "信息科甲", "复核通过");
        append(repo.conn(), &rec).unwrap();
        let rec2 = AuditRecord::new("b-1", "r-1", EVENT_REVOKE, "信息科甲", "撤回确认");
        append(repo.conn(), &rec2).unwrap();
        let rec3 = AuditRecord::new("b-2", "r-2", EVENT_CONFIRM, "信息科乙", "复核通过");
        append(repo.conn(), &rec3).unwrap();

        assert_eq!(count_for_row(repo.conn(), "r-1").unwrap(), 2);
        assert_eq!(count_for_row(repo.conn(), "r-2").unwrap(), 1);
        assert_eq!(count_for_batch(repo.conn(), "b-1").unwrap(), 2);
        assert_eq!(count_for_batch(repo.conn(), "b-2").unwrap(), 1);

        let events = events_for_row(repo.conn(), "r-1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1, EVENT_CONFIRM);
        assert_eq!(events[1].1, EVENT_REVOKE);
    }
}
