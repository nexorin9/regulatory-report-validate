//! 人工确认流与上报写闸门。
//!
//! # 三件事
//!
//! - **谁确认了这一行** —— [`confirm`]：确认记录**追加**写进 `confirmations` 表
//!   （确认人 / 确认时间 / 行键 / 批次），同时把行状态推进到「待确认」；
//! - **确认能不能撤** —— [`revoke`]：撤销也是**追加**一条记录，原记录留着
//!   （确认过又撤销过，本身就是要查的事）；撤销后这一行回到「校验通过」，需重新确认；
//! - **能不能推** —— [`write_gate`]：**写闸门**。只有「处在可推送状态」且
//!   「有有效确认记录」的行才允许进入推送，其余一律拦下并回 `UNCONFIRMED_ROW`。
//!
//! # 为什么模型不算数
//!
//! 模型给的语义结论只是**标注**：疑似重复那一列写着「像重复」，但合不合并由人勾选。
//! 合并动作本身也记一条确认记录（[`merge_confirmed_pair`]）—— 合并是谁做的、
//! 什么时候做的，事后查得到。**模型永远不推进任何业务状态。**
//!
//! # 空名不入库
//!
//! 确认人不得为空（[`ErrorCode::ConfirmActorMissing`]）。没有人的确认等于没人负责：
//! 半年后问「这行为什么被合并了」，空名字段谁也答不上来。
//!
//! # 一个事务
//!
//! 确认 / 撤销 / 合并都是**确认记录 + 行状态推进 + 审计留痕**三件事写在一起，
//! 放在同一个事务里：要么都成，要么都不成。半截状态（「确认了但状态没推进」）
//! 会让写闸门与状态条对不上号。
//!
//! # 写侧只有一个出处
//!
//! `confirmations` 表的写语句只在本模块里（`src/state/confirm.rs`）。
//! `src/db/repo.rs` 只留读侧。理由与行状态机一样：同一件事有两套写法，
//! 迟早会出现「库里说没确认、闸门说确认了」这种对不上的局面。

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::repo;
use crate::errors::{EngineError, ErrorCode};
use crate::state::audit::{self, AuditRecord};
use crate::state::machine::{self, RowState, StateChange};

/// 确认动作：复核通过，可以上报
pub const ACTION_CONFIRM: &str = "confirm";
/// 撤销动作：撤回之前的确认
pub const ACTION_REVOKE: &str = "revoke";
/// 合并动作：确认一对疑似重复确实是同一件事
pub const ACTION_MERGE: &str = "merge";

/// 权威归属状态：被合并进来的重复行（同一笔记录只报一次）
const AUTHORITY_DUPLICATE: &str = "duplicate";

/// 写闸门为什么不放行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReason {
    /// 这一行在本机库里查不到（批次不对，或行键抄错了）
    RowUnknown,
    /// 从来没有人工确认记录
    NotConfirmed,
    /// 确认过，但后来被撤销了（要重新确认才能再报）
    Revoked,
    /// 行状态还没到可推送的状态（口径没过校验 / 已上报）
    StateNotPushable,
}

impl GateReason {
    /// 中文说法（终端与错误说明用）
    pub fn zh(&self) -> &'static str {
        match self {
            GateReason::RowUnknown => "本机库里没有这一行",
            GateReason::NotConfirmed => "这一行还没有人工确认记录",
            GateReason::Revoked => "这一行的确认已被撤销",
            GateReason::StateNotPushable => "这一行还没处在可推送的状态",
        }
    }
}

/// 写闸门的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// 放行：这一行已确认，可以推
    Allowed {
        /// 确认人
        confirmed_by: String,
        /// 确认时间
        confirmed_at: String,
    },
    /// 拦下：不允许推
    Rejected {
        /// 拦下的原因
        reason: GateReason,
        /// 具体说明（带当前行状态）
        detail: String,
    },
}

impl GateVerdict {
    /// 是否放行
    pub fn is_allowed(&self) -> bool {
        matches!(self, GateVerdict::Allowed { .. })
    }

    /// 是否拦下
    pub fn is_rejected(&self) -> bool {
        matches!(self, GateVerdict::Rejected { .. })
    }
}

/// 一次人工确认的请求。
#[derive(Debug, Clone)]
pub struct ConfirmRequest {
    /// 批次号
    pub batch_ref: String,
    /// 被确认的行键
    pub row_uid: String,
    /// 确认人 —— **不得为空**
    pub actor: String,
    /// 备注（确认理由，写给复核人看）
    pub note: String,
    /// 对方行键（确认的是一对行时写上）
    pub peer_row_uid: String,
}

impl ConfirmRequest {
    /// 一个只带必要字段的确认请求。
    pub fn new(batch_ref: &str, row_uid: &str, actor: &str) -> ConfirmRequest {
        ConfirmRequest {
            batch_ref: batch_ref.to_string(),
            row_uid: row_uid.to_string(),
            actor: actor.to_string(),
            note: String::new(),
            peer_row_uid: String::new(),
        }
    }

    /// 带备注。
    pub fn with_note(mut self, note: &str) -> ConfirmRequest {
        self.note = note.to_string();
        self
    }

    /// 带对方行键（确认的是一对行）。
    pub fn with_peer(mut self, peer_row_uid: &str) -> ConfirmRequest {
        self.peer_row_uid = peer_row_uid.to_string();
        self
    }
}

/// 一次撤销确认的请求。
#[derive(Debug, Clone)]
pub struct RevokeRequest {
    /// 批次号
    pub batch_ref: String,
    /// 被撤销确认的行键
    pub row_uid: String,
    /// 撤销人 —— **不得为空**
    pub actor: String,
    /// 撤销理由
    pub note: String,
}

impl RevokeRequest {
    /// 一个只带必要字段的撤销请求。
    pub fn new(batch_ref: &str, row_uid: &str, actor: &str) -> RevokeRequest {
        RevokeRequest {
            batch_ref: batch_ref.to_string(),
            row_uid: row_uid.to_string(),
            actor: actor.to_string(),
            note: String::new(),
        }
    }

    /// 带撤销理由。
    pub fn with_note(mut self, note: &str) -> RevokeRequest {
        self.note = note.to_string();
        self
    }
}

/// 一次疑似重复合并的请求。
#[derive(Debug, Clone)]
pub struct MergeRequest {
    /// 批次号
    pub batch_ref: String,
    /// 保留的行键（继续以这一行为准上报）
    pub keep_row_uid: String,
    /// 并进来的行键（不再单独上报）
    pub drop_row_uid: String,
    /// 确认人 —— **不得为空**
    pub actor: String,
    /// 合并理由（写给复核人看）
    pub note: String,
}

impl MergeRequest {
    /// 一个只带必要字段的合并请求。
    pub fn new(batch_ref: &str, keep_row_uid: &str, drop_row_uid: &str, actor: &str) -> MergeRequest {
        MergeRequest {
            batch_ref: batch_ref.to_string(),
            keep_row_uid: keep_row_uid.to_string(),
            drop_row_uid: drop_row_uid.to_string(),
            actor: actor.to_string(),
            note: String::new(),
        }
    }

    /// 带合并理由。
    pub fn with_note(mut self, note: &str) -> MergeRequest {
        self.note = note.to_string();
        self
    }
}

/// 一次确认 / 撤销 / 合并的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmOutcome {
    /// 批次号
    pub batch_ref: String,
    /// 行键
    pub row_uid: String,
    /// 谁做的
    pub actor: String,
    /// 什么时候做的
    pub at: String,
    /// 动作（confirm / revoke / merge）
    pub action: String,
    /// 动作之后这一行的状态
    pub state_after: RowState,
    /// 留下的备注
    pub note: String,
}

impl ConfirmOutcome {
    /// 一句话结论（终端用）
    pub fn summary_line(&self) -> String {
        format!(
            "{} · 批次 {} · 行 {} · {} · 动作后状态 {}",
            self.action, self.batch_ref, self.row_uid, self.actor, self.state_after
        )
    }
}

/// 这一行在库里存不存在（确认错行等于白确认）。
fn row_source_file(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<Option<String>, EngineError> {
    let file = conn
        .query_row(
            "SELECT source_file FROM rows WHERE batch_ref = ?1 AND row_uid = ?2 LIMIT 1",
            params![batch_ref.trim(), row_uid.trim()],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(file)
}

/// 写闸门：这一行能不能进推送。
///
/// # 放行条件（两条都要满足）
///
/// 1. **有有效的人工确认记录** —— 确认过，且之后没有撤销；
/// 2. **行状态处在可推送的状态** —— 「待确认」或「待重试」。
///
/// 「待重试」也放行：那一行确认过，只是上次推送时网关临时故障（5xx / 超时），
/// 重试不需要重新找人确认 —— 它的确认记录本来就在。
///
/// # 拦下时给什么
///
/// 不报错，只给结论（[`GateVerdict::Rejected`]）：谁在哪儿被拦、为什么被拦，
/// 一句话说清楚。要变成错误码用 [`assert_push_allowed`]。
pub fn write_gate(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<GateVerdict, EngineError> {
    let batch = batch_ref.trim();
    let row = row_uid.trim();

    // 这一行在本批里存不存在
    let Some(source_file) = row_source_file(conn, batch, row)? else {
        return Ok(GateVerdict::Rejected {
            reason: GateReason::RowUnknown,
            detail: format!("批次 {} 里没有行键 {} 这一行（批次不对或行键抄错了）", batch, row),
        });
    };

    // 当前行状态
    let file_key = machine::file_identity(&source_file);
    let snap = machine::current_or_pending(conn, row, &file_key)?;

    // 确认记录：有没有、是不是被撤销了
    let effective = repo::effective_confirmation(conn, batch, row)?;
    let last_revoke = repo::last_revoke_id(conn, batch, row)?;
    let Some(eff) = effective else {
        let detail = match last_revoke {
            Some(_) => format!(
                "批次 {} 的行 {}：确认被撤销过，需重新确认才能上报（当前行状态：{}）",
                batch,
                row,
                snap.state.as_str()
            ),
            None => format!(
                "批次 {} 的行 {}：还没有人工确认记录（当前行状态：{}）",
                batch,
                row,
                snap.state.as_str()
            ),
        };
        return Ok(GateVerdict::Rejected {
            reason: if last_revoke.is_some() {
                GateReason::Revoked
            } else {
                GateReason::NotConfirmed
            },
            detail,
        });
    };

    // 状态必须处在可推送的状态
    if !matches!(snap.state, RowState::PendingConfirmation | RowState::RetryPending) {
        return Ok(GateVerdict::Rejected {
            reason: GateReason::StateNotPushable,
            detail: format!(
                "批次 {} 的行 {}：已由 {} 确认，但当前行状态是「{}」，不在可推送状态（待确认 / 待重试）",
                batch,
                row,
                eff.confirmed_by,
                snap.state.as_str()
            ),
        });
    }

    Ok(GateVerdict::Allowed {
        confirmed_by: eff.confirmed_by,
        confirmed_at: eff.confirmed_at,
    })
}

/// 写闸门：不允许就报 `UNCONFIRMED_ROW`。
///
/// 上报路径上真正拦人的就是它 —— 拦下即**不推**，不是「推了再说」。
pub fn assert_push_allowed(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<(), EngineError> {
    match write_gate(conn, batch_ref, row_uid)? {
        GateVerdict::Allowed { .. } => Ok(()),
        GateVerdict::Rejected { reason, detail } => Err(EngineError::at(
            ErrorCode::UnconfirmedRow,
            format!("写闸门拦下：{}（{}）", reason.zh(), detail),
            format!("batch_ref={} row_uid={}", batch_ref.trim(), row_uid.trim()),
        )),
    }
}

/// 人工确认：人勾选了这一条。
///
/// **两种勾选共用这一个入口**：
///
/// - **复核通过**（这一行已过口径校验）→ 行状态推进到「待确认」，可以上报；
/// - **疑似重复确实是同一件事**（这一行可能没过校验）→ 只留痕，
///   行状态留在「待校验」。
///
/// 无论哪种，**没过口径校验的行都推不出去** —— 写闸门还看状态那一关
/// （见 [`write_gate`]）。确认 ≠ 可上报，这两件事分开记。
///
/// 顺序（一个事务里做完）：
/// 1. 校验确认人非空、行键非空、这一行在本批里存在；
/// 2. 追加一条 `confirm` 记录（确认人 / 确认时间 / 备注）；
/// 3. 行状态推进到「待确认」（口径没过的行推不动，状态留在「待校验」）；
/// 4. 审计链追加一条 `confirm` 事件。
pub fn confirm(
    conn: &mut Connection,
    req: &ConfirmRequest,
) -> Result<ConfirmOutcome, EngineError> {
    let actor = req.actor.trim();
    if actor.is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfirmActorMissing,
            "确认记录必须写确认人，空名不入库（没有人的确认等于没人负责）",
            format!("batch_ref={} row_uid={}", req.batch_ref, req.row_uid),
        ));
    }
    let row_uid = req.row_uid.trim();
    let batch_ref = req.batch_ref.trim();
    if row_uid.is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "确认记录必须写行键（确认的是哪一行）",
            format!("batch_ref={}", batch_ref),
        ));
    }

    let Some(source_file) = row_source_file(conn, batch_ref, row_uid)? else {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!(
                "批次 {} 里没有行键 {} 这一行 —— 确认前请核对行键（行键在结果契约 rows[].row_uid 里）",
                batch_ref, row_uid
            ),
            format!("batch_ref={} row_uid={}", batch_ref, row_uid),
        ));
    };

    let file_key = machine::file_identity(&source_file);
    let snap = machine::current_or_pending(conn, row_uid, &file_key)?;

    // 已上报的行不再重复确认（要重报走订正路径）
    if snap.state == RowState::Reported {
        return Err(EngineError::at(
            ErrorCode::InvalidStateTransition,
            "这一行已上报，不能再确认（要重报请走订正路径）",
            format!("batch_ref={} row_uid={} state={}", batch_ref, row_uid, snap.state.as_str()),
        ));
    }

    // 口径没过的行**也能确认**（这一条是「人勾选过了」：疑似重复要逐条勾、
    // 复核也要留痕），但**推不出去** —— 状态留在「待校验」，
    // 写闸门照拦（状态不在可推送状态那一档）。
    let can_enter_queue = machine::can_transition(snap.state, RowState::PendingConfirmation);

    let now = repo::now_iso8601();
    let note = compose_note(&req.note, &req.peer_row_uid);

    in_transaction(conn, |tx| {
        // 1. 确认记录（只追加）
        tx.execute(
            "INSERT INTO confirmations
                (batch_ref, row_uid, action, confirmed_by, confirmed_at, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)",
            params![batch_ref, row_uid, ACTION_CONFIRM, actor, now, note],
        )?;
        // 2. 行状态推进到「待确认」（口径没过的行推不动，状态留在「待校验」）
        let state_after = if can_enter_queue {
            machine::advance(
                tx,
                &StateChange {
                    row_uid: row_uid.to_string(),
                    source_file: source_file.clone(),
                    batch_ref: batch_ref.to_string(),
                    to: RowState::PendingConfirmation,
                    expected_version: None,
                    actor: actor.to_string(),
                    reason: "人工确认复核通过，排入待上报队列".to_string(),
                    data_digest: snap.data_digest.clone(),
                },
            )?
            .state
        } else {
            snap.state
        };
        // 3. 审计链
        audit::append(
            tx,
            &AuditRecord::new(
                batch_ref,
                row_uid,
                audit::EVENT_CONFIRM,
                actor,
                &format!("人工确认留痕，行状态 {}", state_after.as_str()),
            ),
        )?;
        Ok(ConfirmOutcome {
            batch_ref: batch_ref.to_string(),
            row_uid: row_uid.to_string(),
            actor: actor.to_string(),
            at: now.clone(),
            action: ACTION_CONFIRM.to_string(),
            state_after,
            note: note.clone(),
        })
    })
}

/// 恢复一条人工确认记录（**状态库恢复时用**）。
///
/// # 为什么需要这个入口
///
/// 状态库损坏后，恢复流程要把「谁确认过哪一行」抢救回来。
/// 但 `confirmations` 表的写语句只允许出现在本模块（见模块文档「写侧只有一个出处」）——
/// 所以恢复走的是这个函数，而不是在恢复模块里另写一条 INSERT。
///
/// # 追加，不是覆盖
///
/// 恢复出来的确认记录**追加**进 `confirmations` 表，保留原来的确认人与确认时间。
/// 已经存在同一行同一动作的确认时不重复追加（恢复要幂等：跑两遍结果一样）。
///
/// **不推进行状态** —— 行状态由状态机唯一入口管（`src/state/machine.rs`），
/// 恢复模块不碰它；恢复完的行状态由经办人重新校验、重新确认。
pub fn restore_confirmation(
    conn: &mut Connection,
    record: &repo::Confirmation,
) -> Result<bool, EngineError> {
    let row_uid = record.row_uid.trim();
    let batch_ref = record.batch_ref.trim();
    if row_uid.is_empty() || batch_ref.is_empty() {
        return Ok(false);
    }
    let actor = record.confirmed_by.trim();
    if actor.is_empty() {
        // 空名不入库 —— 没有人的确认等于没人负责（与正常确认流同一条规矩）
        return Ok(false);
    }
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM confirmations
          WHERE batch_ref = ?1 AND row_uid = ?2 AND action = ?3 AND confirmed_by = ?4",
        params![batch_ref, row_uid, record.action.trim(), actor],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO confirmations
            (batch_ref, row_uid, action, confirmed_by, confirmed_at, note,
             revoked_by, revoked_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5)",
        params![
            batch_ref,
            row_uid,
            record.action.trim(),
            actor,
            record.confirmed_at.trim(),
            record.note.trim(),
            record.revoked_by.trim(),
            record.revoked_at.trim(),
        ],
    )?;
    Ok(true)
}

/// 撤销确认：撤回之前的复核，行回到「校验通过」，需重新确认才能再报。
///
/// 顺序（一个事务里做完）：
/// 1. 校验撤销人非空、这一行确实有可撤销的确认；
/// 2. 追加一条 `revoke` 记录（原 `confirm` 记录留着，只标出撤销人与撤销时间）；
/// 3. 行状态从「待确认」退回「校验通过」；
/// 4. 审计链追加一条 `revoke` 事件。
pub fn revoke(
    conn: &mut Connection,
    req: &RevokeRequest,
) -> Result<ConfirmOutcome, EngineError> {
    let actor = req.actor.trim();
    if actor.is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfirmActorMissing,
            "撤销记录必须写撤销人，空名不入库（没有人的撤销等于没人负责）",
            format!("batch_ref={} row_uid={}", req.batch_ref, req.row_uid),
        ));
    }
    let row_uid = req.row_uid.trim();
    let batch_ref = req.batch_ref.trim();

    let Some(source_file) = row_source_file(conn, batch_ref, row_uid)? else {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!(
                "批次 {} 里没有行键 {} 这一行 —— 撤销前请核对行键",
                batch_ref, row_uid
            ),
            format!("batch_ref={} row_uid={}", batch_ref, row_uid),
        ));
    };

    // 最近的确认记录与撤销记录
    let last_confirm_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM confirmations
              WHERE batch_ref = ?1 AND row_uid = ?2 AND action = ?3
              ORDER BY id DESC LIMIT 1",
            params![batch_ref, row_uid, ACTION_CONFIRM],
            |r| r.get(0),
        )
        .optional()?;
    let last_revoke_id = repo::last_revoke_id(conn, batch_ref, row_uid)?;
    let Some(confirm_id) = last_confirm_id else {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!(
                "批次 {} 的行 {} 没有人工确认记录，没什么可撤销的",
                batch_ref, row_uid
            ),
            format!("batch_ref={} row_uid={}", batch_ref, row_uid),
        ));
    };
    if let Some(rev) = last_revoke_id {
        if rev > confirm_id {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                format!(
                    "批次 {} 的行 {} 的确认已经被撤销过了 —— 要再上报请重新确认",
                    batch_ref, row_uid
                ),
                format!("batch_ref={} row_uid={}", batch_ref, row_uid),
            ));
        }
    }

    let file_key = machine::file_identity(&source_file);
    let snap = machine::current_or_pending(conn, row_uid, &file_key)?;
    if snap.state != RowState::PendingConfirmation {
        return Err(EngineError::at(
            ErrorCode::InvalidStateTransition,
            format!(
                "只有「待确认」的行能撤销确认（当前行状态：{}）",
                snap.state.as_str()
            ),
            format!("batch_ref={} row_uid={} state={}", batch_ref, row_uid, snap.state.as_str()),
        ));
    }

    let now = repo::now_iso8601();
    let note = req.note.trim().to_string();

    in_transaction(conn, |tx| {
        // 1. 撤销记录（只追加）
        tx.execute(
            "INSERT INTO confirmations
                (batch_ref, row_uid, action, confirmed_by, confirmed_at, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)",
            params![batch_ref, row_uid, ACTION_REVOKE, actor, now, note],
        )?;
        // 2. 在原确认记录上标出撤销人与撤销时间（原记录留着：确认过又撤销过，本身就是要查的事）
        tx.execute(
            "UPDATE confirmations SET revoked_by = ?1, revoked_at = ?2 WHERE id = ?3",
            params![actor, now, confirm_id],
        )?;
        // 3. 行状态退回「校验通过」（需重新确认才能再报）
        let after = machine::advance(
            tx,
            &StateChange {
                row_uid: row_uid.to_string(),
                source_file: source_file.clone(),
                batch_ref: batch_ref.to_string(),
                to: RowState::Validated,
                expected_version: None,
                actor: actor.to_string(),
                reason: "人工撤销确认，退回校验通过（需重新确认才能再报）".to_string(),
                data_digest: snap.data_digest.clone(),
            },
        )?;
        // 4. 审计链
        audit::append(
            tx,
            &AuditRecord::new(
                batch_ref,
                row_uid,
                audit::EVENT_REVOKE,
                actor,
                &format!(
                    "撤销人工确认（确认记录 id {}），行状态退回 {}",
                    confirm_id,
                    after.state.as_str()
                ),
            ),
        )?;
        Ok(ConfirmOutcome {
            batch_ref: batch_ref.to_string(),
            row_uid: row_uid.to_string(),
            actor: actor.to_string(),
            at: now.clone(),
            action: ACTION_REVOKE.to_string(),
            state_after: after.state,
            note: note.clone(),
        })
    })
}

/// 疑似重复合并：逐条勾选后，把并进来的那行标成重复（同一笔记录只报一次）。
///
/// **模型给的疑似重复只是标注** —— 只有人勾了这一条，才会走到这里。
/// 合并动作本身也记一条确认记录（`merge`），审计链同步留痕：
/// 合并是谁做的、什么时候做的、留了哪一行，事后都查得到。
///
/// 两行的**权威键必须一致**（机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键）——
/// 权威键不同的两行不是同一笔记录，合并了就是把两条记录并成一条，数据就错了。
pub fn merge_confirmed_pair(
    conn: &mut Connection,
    req: &MergeRequest,
) -> Result<ConfirmOutcome, EngineError> {
    let actor = req.actor.trim();
    if actor.is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfirmActorMissing,
            "合并记录必须写确认人，空名不入库（没有人的合并等于没人负责）",
            format!("batch_ref={} row_uid={}", req.batch_ref, req.drop_row_uid),
        ));
    }
    let batch_ref = req.batch_ref.trim();
    let keep = req.keep_row_uid.trim();
    let drop = req.drop_row_uid.trim();
    if keep.is_empty() || drop.is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "合并必须写清两行：保留哪一行、并进来哪一行",
            format!("batch_ref={}", batch_ref),
        ));
    }
    if keep == drop {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "合并的两行不能是同一行",
            format!("batch_ref={} row_uid={}", batch_ref, keep),
        ));
    }

    let keep_key = authority_key(conn, batch_ref, keep)?;
    let Some(keep_key) = keep_key else {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!("批次 {} 里没有行键 {} 这一行", batch_ref, keep),
            format!("batch_ref={} row_uid={}", batch_ref, keep),
        ));
    };
    let Some(drop_key) = authority_key(conn, batch_ref, drop)? else {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!("批次 {} 里没有行键 {} 这一行", batch_ref, drop),
            format!("batch_ref={} row_uid={}", batch_ref, drop),
        ));
    };
    if keep_key != drop_key {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            format!(
                "这两行的权威键不一样，不是同一笔记录，不能合并（保留行 {}：{}；并入行 {}：{}）",
                keep, keep_key, drop, drop_key
            ),
            format!("batch_ref={} keep={} drop={}", batch_ref, keep, drop),
        ));
    }

    let now = repo::now_iso8601();
    let note = compose_note(&req.note, drop);

    in_transaction(conn, |tx| {
        // 1. 并进来的那行标成重复（同一笔记录只报一次）
        tx.execute(
            "UPDATE rows SET authority_state = ?1
              WHERE batch_ref = ?2 AND row_uid = ?3 AND authority_state <> ?1",
            params![AUTHORITY_DUPLICATE, batch_ref, drop],
        )?;
        // 2. 合并动作本身也记一条确认记录
        tx.execute(
            "INSERT INTO confirmations
                (batch_ref, row_uid, action, confirmed_by, confirmed_at, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)",
            params![batch_ref, drop, ACTION_MERGE, actor, now, note],
        )?;
        // 3. 审计链
        audit::append(
            tx,
            &AuditRecord::new(
                batch_ref,
                drop,
                audit::EVENT_MERGE,
                actor,
                &format!(
                    "疑似重复合并：保留行 {}，并入行 {}（同一笔记录只报一次）",
                    keep, drop
                ),
            ),
        )?;
        Ok(())
    })?;

    // 合并后这一行的状态：合并不改行状态（状态归校验与上报管），取当前值
    let source_file = row_source_file(conn, batch_ref, drop)?.unwrap_or_default();
    let file_key = machine::file_identity(&source_file);
    let snap = machine::current_or_pending(conn, drop, &file_key)?;

    Ok(ConfirmOutcome {
        batch_ref: batch_ref.to_string(),
        row_uid: drop.to_string(),
        actor: actor.to_string(),
        at: now,
        action: ACTION_MERGE.to_string(),
        state_after: snap.state,
        note,
    })
}

/// 在一个事务里做完三件事（写记录 → 推进状态 → 留痕）。
///
/// # 为什么自己 BEGIN / COMMIT
///
/// rusqlite 的 `Transaction` 只实现 `Deref`（**没有** `DerefMut`），
/// 事务里调不动 `&mut Connection` 的接口 —— 行状态推进（[`machine::advance`]）
/// 正是这种接口。所以这里自己发 `BEGIN IMMEDIATE` / `COMMIT` / `ROLLBACK`：
/// 效果一样，但事务里仍拿得到 `&mut Connection`。
///
/// 出错一律 `ROLLBACK`：不留半截状态（「确认了但状态没推进」会让闸门与状态条对不上号）。
fn in_transaction<T>(
    conn: &mut Connection,
    f: impl FnOnce(&mut Connection) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| {
        EngineError::at(
            ErrorCode::Db,
            format!("开事务失败：{}", e),
            "confirmations",
        )
    })?;
    match f(conn) {
        Ok(value) => {
            conn.execute_batch("COMMIT").map_err(|e| {
                EngineError::at(
                    ErrorCode::Db,
                    format!("提交事务失败：{}", e),
                    "confirmations",
                )
            })?;
            Ok(value)
        }
        Err(err) => {
            // 回滚失败不掩盖原始错误：原始错误才是要处理的那个
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

/// 权威键：机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键。
fn authority_key(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<Option<String>, EngineError> {
    let key = conn
        .query_row(
            "SELECT org_code, reporting_period, indicator_code, dept_code, business_key
               FROM rows WHERE batch_ref = ?1 AND row_uid = ?2 LIMIT 1",
            params![batch_ref.trim(), row_uid.trim()],
            |r| {
                Ok(format!(
                    "{}|{}|{}|{}|{}",
                    r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                ))
            },
        )
        .optional()?;
    Ok(key)
}

/// 备注拼装：确认理由 + 对方行键（确认的是哪一对，要看得见）。
fn compose_note(note: &str, peer_row_uid: &str) -> String {
    let mut out = note.trim().to_string();
    let peer = peer_row_uid.trim();
    if !peer.is_empty() {
        if !out.is_empty() {
            out.push('；');
        }
        out.push_str(&format!("对方行键 {}", peer));
    }
    out
}

/// 当前有效的已确认行键集合（写闸门与清单共用）。
pub fn confirmed_row_uids(
    conn: &Connection,
    batch_ref: &str,
) -> Result<std::collections::BTreeSet<String>, EngineError> {
    repo::effective_confirmed_row_uids(conn, batch_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 闸门理由的中文说法都不为空() {
        for reason in [
            GateReason::RowUnknown,
            GateReason::NotConfirmed,
            GateReason::Revoked,
            GateReason::StateNotPushable,
        ] {
            assert!(!reason.zh().is_empty());
        }
    }

    #[test]
    fn 备注拼装带上对方行键() {
        assert_eq!(compose_note("复核通过", ""), "复核通过");
        assert_eq!(compose_note("", "r-2"), "对方行键 r-2");
        assert_eq!(compose_note("复核通过", "r-2"), "复核通过；对方行键 r-2");
    }
}
