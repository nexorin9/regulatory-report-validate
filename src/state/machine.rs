//! 行级状态机：一条监管指标行从落库到上报的一生。
//!
//! # 六个状态
//!
//! | 状态 | 什么时候是它 | 谁推进 |
//! |---|---|---|
//! | 待校验 | 已落库，口径校验还没过（结论「不通过」） | 引擎（校验） |
//! | 校验通过 | 口径校验通过（结论「通过」或「有提示」） | 引擎（校验） |
//! | 待确认 | 已复核确认，等着被推上去 | 人工确认（任务 36） |
//! | 已上报 | 网关受理了，幂等键已生效 | 网关回执 |
//! | 被退回 | 网关 4xx 退回，改数据才能再报 | 网关回执 |
//! | 待重试 | 5xx / 超时 / 连不上，原样可以再试 | 网关回执 |
//!
//! 三个上报后状态的字面量直接引用 [`crate::push::status`] —— 同一串字两处各写一份，
//! 迟早会出现「库里写着已上报、状态机却当没报过」这种对不上的局面。
//!
//! # 合法迁移表
//!
//! 迁移表是**白名单**：表里没有的一律拒绝。典型被拒的迁移：
//!
//! - `待校验 → 已上报`：没过校验的行直接报上去，等于把不合格的数据推给网关；
//! - `校验通过 → 已上报`：跳过人工确认直接上报；
//! - `已上报 → 被退回`：网关受理过的那一版不会变成被退回（要订正走 `已上报 → 待确认`）；
//! - `已上报 → 校验通过`：已上报的行不得被普通校验改回。
//!
//! 唯一一条「往回走」的确认阶段迁移是 `待确认 → 校验通过`：
//! 那是**人工撤销确认**的路径（见 [`crate::state::confirm`]）——
//! 复核被撤回，这一行退回「等重新确认」，不是「它没被复核过」。
//!
//! # 单一入口
//!
//! **所有状态推进都走 [`advance`]。** 本模块是 `row_states` 表的唯一写入方 ——
//! `src/` 下别处不允许出现对这张表的 `UPDATE` / `INSERT`（`src/db/schema.rs` 里的建表语句除外）。
//! 状态推进散在各处，等于同一件事有两套写法：改了一处忘了另一处，
//! 表现出来就是「状态条说已上报、库里说待确认」，谁也说不清哪边是真的。
//!
//! # 并发：行级乐观锁
//!
//! 每次推进都带一个版本号。推进时用「读到的版本号」做条件更新
//! （`UPDATE ... WHERE version = ?`），更新不到行就是**后写者被拒**（`ROW_STATE_CONFLICT`），
//! 不会把前一个写入者的结果悄悄盖掉。两个信息科的人同时点「校验」与「上报」，
//! 撞上的那一个会拿到明确报错，而不是把状态改成一个两边都不认的值。

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::errors::{EngineError, ErrorCode};

/// 行级状态：待校验。
pub const PENDING_VALIDATION: &str = "待校验";
/// 行级状态：校验通过。
pub const VALIDATED: &str = "校验通过";
/// 行级状态：待确认。
pub const PENDING_CONFIRMATION: &str = "待确认";
/// 行级状态：已上报（与上报侧的取值同一处，不另写一份）。
pub const REPORTED: &str = crate::push::status::ACCEPTED;
/// 行级状态：被退回（与上报侧的取值同一处，不另写一份）。
pub const REJECTED: &str = crate::push::status::REJECTED;
/// 行级状态：待重试（与上报侧的取值同一处，不另写一份）。
pub const RETRY_PENDING: &str = crate::push::status::RETRYABLE;

/// 行级状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowState {
    /// 待校验：已落库，口径校验还没过。
    PendingValidation,
    /// 校验通过：口径校验通过，等人工确认。
    Validated,
    /// 待确认：已复核确认，等着被推上去。
    PendingConfirmation,
    /// 已上报：网关受理了。
    Reported,
    /// 被退回：网关 4xx 退回。
    Rejected,
    /// 待重试：可重试的失败。
    RetryPending,
}

impl RowState {
    /// 全部状态（顺序即生命周期顺序：先校验、后确认、再上报）
    pub const ALL: [RowState; 6] = [
        RowState::PendingValidation,
        RowState::Validated,
        RowState::PendingConfirmation,
        RowState::Reported,
        RowState::Rejected,
        RowState::RetryPending,
    ];

    /// 状态字面量（写回原表与落库都用它）
    pub fn as_str(&self) -> &'static str {
        match self {
            RowState::PendingValidation => PENDING_VALIDATION,
            RowState::Validated => VALIDATED,
            RowState::PendingConfirmation => PENDING_CONFIRMATION,
            RowState::Reported => REPORTED,
            RowState::Rejected => REJECTED,
            RowState::RetryPending => RETRY_PENDING,
        }
    }

    /// 按字面量取状态。**不认识的值返回 `None`** —— 不猜、不默认成某个状态。
    ///
    /// 库里出现一个不认识的状态值，说明状态库被别的工具改过；
    /// 这时候宁可报错让人来看，也不要默默当成「待校验」重新推一遍。
    pub fn parse(raw: &str) -> Option<RowState> {
        RowState::ALL.into_iter().find(|s| s.as_str() == raw)
    }

    /// 是不是「网关已经受理」（幂等键已生效，不再重复推送）
    pub fn is_reported(&self) -> bool {
        *self == RowState::Reported
    }

    /// 是不是「报过但没报成」（被退回 / 待重试）—— 这两种行要进待办清单
    pub fn is_failed_push(&self) -> bool {
        matches!(self, RowState::Rejected | RowState::RetryPending)
    }
}

impl std::fmt::Display for RowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 合法迁移表（白名单）。表里没有的迁移一律拒绝。
///
/// 自迁移（`待重试 → 待重试`、`已上报 → 已上报`）也在这里：
/// 它们是**合法空转** —— 状态不变、版本号不变，但也不算报错
/// （重试耗尽继续留在待重试、幂等命中继续算已上报，都是正常事）。
pub const LEGAL_TRANSITIONS: &[(RowState, RowState)] = &[
    // 校验阶段
    (RowState::PendingValidation, RowState::Validated),
    (RowState::Validated, RowState::PendingValidation),
    (RowState::PendingConfirmation, RowState::PendingValidation),
    // 确认阶段
    (RowState::Validated, RowState::PendingConfirmation),
    // 撤销确认：待确认 → 校验通过（人工撤回复核，这一行退回「等重新确认」）
    (RowState::PendingConfirmation, RowState::Validated),
    (RowState::Rejected, RowState::Validated),
    (RowState::Rejected, RowState::PendingConfirmation),
    (RowState::RetryPending, RowState::PendingConfirmation),
    (RowState::Reported, RowState::PendingConfirmation),
    // 上报阶段
    (RowState::PendingConfirmation, RowState::Reported),
    (RowState::PendingConfirmation, RowState::Rejected),
    (RowState::PendingConfirmation, RowState::RetryPending),
    (RowState::RetryPending, RowState::Reported),
    (RowState::RetryPending, RowState::Rejected),
    (RowState::RetryPending, RowState::RetryPending),
    (RowState::Reported, RowState::Reported),
];

/// 这条迁移合不合法。
pub fn can_transition(from: RowState, to: RowState) -> bool {
    LEGAL_TRANSITIONS
        .iter()
        .any(|(f, t)| *f == from && *t == to)
}

/// 校验一条迁移；不合法就报错，**状态一个字节都不动**。
pub fn check_transition(from: RowState, to: RowState) -> Result<(), EngineError> {
    if can_transition(from, to) {
        return Ok(());
    }
    Err(EngineError::at(
        ErrorCode::InvalidStateTransition,
        format!(
            "行级状态迁移不合法：{} → {} 不在合法迁移表里（状态未被改写）",
            from.as_str(),
            to.as_str()
        ),
        format!("{} → {}", from.as_str(), to.as_str()),
    ))
}

/// 来源文件身份：同一份文件换个写法（相对路径 / 绝对路径）不能算成两个文件。
///
/// 文件不存在时（测试里的合成文件名）退回原字符串。
pub fn file_identity(source_file: &str) -> String {
    std::fs::canonicalize(source_file)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| source_file.to_string())
}

/// 一行的数据摘要：判断「这一行有没有被改过」用。
///
/// 只拼**业务字段**（指标 / 科室 / 报告期 / 业务主键 / 分子 / 分母 / 填报值 / 单位），
/// 不含行号与时间 —— 换个人重报同一份表，摘要不变。
pub fn digest_of(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\x1f");
    }
    hex::encode(hasher.finalize())
}

/// 行状态快照（库里 `row_states` 的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSnapshot {
    /// 行键
    pub row_uid: String,
    /// 来源文件身份
    pub file_key: String,
    /// 来源文件原样路径（给人看）
    pub source_file: String,
    /// 最近一次碰这一行的批次号
    pub batch_ref: String,
    /// 当前状态
    pub state: RowState,
    /// 行级版本号（乐观锁用）
    pub version: i64,
    /// 数据摘要
    pub data_digest: String,
    /// 最近一次推进的时间
    pub updated_at: String,
    /// 最近一次推进的人 / 命令
    pub updated_by: String,
}

/// 一次状态推进的请求。
#[derive(Debug, Clone)]
pub struct StateChange {
    /// 行键
    pub row_uid: String,
    /// 来源文件（原样路径；内部会算成文件身份）
    pub source_file: String,
    /// 批次号
    pub batch_ref: String,
    /// 要推进到的状态
    pub to: RowState,
    /// 乐观锁：期望的版本号。给了就必须与库里的当前版本一致，不一致直接拒。
    pub expected_version: Option<i64>,
    /// 谁推进的（人名或命令名）
    pub actor: String,
    /// 为什么推进（进审计留痕）
    pub reason: String,
    /// 本轮这一行的数据摘要（变了才算「修正过」）
    pub data_digest: String,
}

impl StateChange {
    /// 一个只带必要字段的推进请求。
    pub fn new(
        row_uid: &str,
        source_file: &str,
        batch_ref: &str,
        to: RowState,
        actor: &str,
    ) -> Self {
        StateChange {
            row_uid: row_uid.to_string(),
            source_file: source_file.to_string(),
            batch_ref: batch_ref.to_string(),
            to,
            expected_version: None,
            actor: actor.to_string(),
            reason: String::new(),
            data_digest: String::new(),
        }
    }

    /// 带乐观锁期望版本号。
    pub fn with_expected_version(mut self, version: i64) -> Self {
        self.expected_version = Some(version);
        self
    }

    /// 带推进原因。
    pub fn with_reason(mut self, reason: &str) -> Self {
        self.reason = reason.to_string();
        self
    }

    /// 带数据摘要。
    pub fn with_digest(mut self, digest: &str) -> Self {
        self.data_digest = digest.to_string();
        self
    }
}

/// 读某一行的当前状态。没有记录返回 `None`。
pub fn current(
    conn: &Connection,
    row_uid: &str,
    file_key: &str,
) -> Result<Option<StateSnapshot>, EngineError> {
    let row = conn
        .query_row(
            "SELECT row_uid, file_key, source_file, batch_ref, state, version, data_digest, updated_at, updated_by
             FROM row_states WHERE row_uid = ?1 AND file_key = ?2",
            params![row_uid, file_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((row_uid, file_key, source_file, batch_ref, state, version, digest, at, by)) = row else {
        return Ok(None);
    };
    let state = RowState::parse(&state).ok_or_else(|| {
        EngineError::at(
            ErrorCode::InvalidStateTransition,
            format!(
                "状态库里的行状态取值不认识（{}），拒绝继续推进（不猜、不重置）",
                state
            ),
            format!("row_states.state={}", state),
        )
    })?;
    Ok(Some(StateSnapshot {
        row_uid,
        file_key,
        source_file,
        batch_ref,
        state,
        version,
        data_digest: digest,
        updated_at: at,
        updated_by: by,
    }))
}

/// 某一行的状态；没有记录时按「待校验、版本 0」看待（**不写库**）。
pub fn current_or_pending(
    conn: &Connection,
    row_uid: &str,
    file_key: &str,
) -> Result<StateSnapshot, EngineError> {
    match current(conn, row_uid, file_key)? {
        Some(snap) => Ok(snap),
        None => Ok(StateSnapshot {
            row_uid: row_uid.to_string(),
            file_key: file_key.to_string(),
            source_file: String::new(),
            batch_ref: String::new(),
            state: RowState::PendingValidation,
            version: 0,
            data_digest: String::new(),
            updated_at: String::new(),
            updated_by: String::new(),
        }),
    }
}

/// **状态推进的唯一入口。**
///
/// 顺序固定，一步都不省：
/// 1. 读当前状态与版本；
/// 2. 乐观锁校验：调用方给了期望版本就必须对上，对不上 → `ROW_STATE_CONFLICT`；
/// 3. 迁移合法性校验：不在合法迁移表里 → `INVALID_STATE_TRANSITION`，**状态一个字节都不动**；
/// 4. 条件更新（`WHERE version = 旧版本`）→ 更新不到行说明有人抢先写了 → `ROW_STATE_CONFLICT`。
///
/// 自迁移（推进到当前状态）是合法空转：状态与版本号都不变。
pub fn advance(
    conn: &mut Connection,
    change: &StateChange,
) -> Result<StateSnapshot, EngineError> {
    let file_key = file_identity(&change.source_file);
    let existing = current(conn, &change.row_uid, &file_key)?;

    // 乐观锁：调用方带的期望版本必须与库里一致（没记录时按版本 0 看）
    if let Some(expected) = change.expected_version {
        let actual = existing.as_ref().map(|s| s.version).unwrap_or(0);
        if expected != actual {
            return Err(EngineError::at(
                ErrorCode::RowStateConflict,
                format!(
                    "行状态版本冲突：期望版本 {}，库里已是 {}（后写者被拒，状态未被改写）",
                    expected, actual
                ),
                format!("row_uid={} version={}", change.row_uid, actual),
            ));
        }
    }

    // 没有记录的行按「待校验、版本 0」看待 —— 迁移表从待校验开始判
    let from = existing
        .as_ref()
        .map(|s| s.state)
        .unwrap_or(RowState::PendingValidation);

    // 合法空转：推进到当前状态 —— 状态不变、版本号不变
    if from == change.to {
        if existing.is_none() {
            insert_initial(conn, &change.row_uid, &file_key, change)?;
        }
        return current_or_pending(conn, &change.row_uid, &file_key);
    }

    // 迁移不合法 → 报错，**一个字节都不改**（连底账都不建）
    check_transition(from, change.to)?;

    // 没有记录时先补一条「待校验」的底账，版本号从 0 起
    if existing.is_none() {
        insert_initial(conn, &change.row_uid, &file_key, change)?;
    }
    let snap = current_or_pending(conn, &change.row_uid, &file_key)?;

    let updated_at = crate::db::repo::now_iso8601();
    let changed = conn
        .execute(
            "UPDATE row_states
                SET state = ?3,
                    version = version + 1,
                    data_digest = ?4,
                    batch_ref = ?5,
                    source_file = ?6,
                    updated_at = ?7,
                    updated_by = ?8
              WHERE row_uid = ?1 AND file_key = ?2 AND version = ?9",
            params![
                change.row_uid,
                file_key,
                change.to.as_str(),
                change.data_digest,
                change.batch_ref,
                change.source_file,
                updated_at,
                change.actor,
                snap.version,
            ],
        )
        .map_err(|e| {
            EngineError::at(
                ErrorCode::Db,
                format!("行状态推进失败：{}", e),
                format!("row_uid={}", change.row_uid),
            )
        })?;

    if changed == 0 {
        // 读到的版本到更新时已经变了 —— 后写者，让出
        return Err(EngineError::at(
            ErrorCode::RowStateConflict,
            format!(
                "行状态版本冲突：推进时版本已从 {} 变过（后写者被拒，状态未被改写）",
                snap.version
            ),
            format!("row_uid={} version={}", change.row_uid, snap.version),
        ));
    }

    current(conn, &change.row_uid, &file_key)?.ok_or_else(|| {
        EngineError::at(
            ErrorCode::Db,
            "行状态推进后读不回来",
            format!("row_uid={}", change.row_uid),
        )
    })
}

/// 建一条「待校验」的底账（版本 0）。
fn insert_initial(
    conn: &Connection,
    row_uid: &str,
    file_key: &str,
    change: &StateChange,
) -> Result<(), EngineError> {
    let now = crate::db::repo::now_iso8601();
    conn.execute(
        "INSERT INTO row_states
            (row_uid, file_key, source_file, batch_ref, state, version, data_digest, updated_at, updated_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9)",
        params![
            row_uid,
            file_key,
            change.source_file,
            change.batch_ref,
            PENDING_VALIDATION,
            change.data_digest,
            now,
            change.actor,
            now,
        ],
    )
    .map_err(|e| {
        EngineError::at(
            ErrorCode::Db,
            format!("行状态底账写入失败：{}", e),
            format!("row_uid={}", row_uid),
        )
    })?;
    Ok(())
}

/// 只更新数据摘要（状态不变、版本不变）。
///
/// 用于「行还在待确认 / 校验通过，但内容变了」的情形：确认记录里存着旧摘要，
/// 复核时能看出这一行被改过（比对摘要即可），而状态机不必为一个纯数据变化改状态。
fn refresh_digest(
    conn: &Connection,
    row_uid: &str,
    file_key: &str,
    digest: &str,
) -> Result<(), EngineError> {
    if digest.is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE row_states SET data_digest = ?3 WHERE row_uid = ?1 AND file_key = ?2",
        params![row_uid, file_key, digest],
    )
    .map_err(|e| {
        EngineError::at(
            ErrorCode::Db,
            format!("行状态摘要更新失败：{}", e),
            format!("row_uid={}", row_uid),
        )
    })?;
    Ok(())
}

/// 从 `rows` 表取这一行的来源文件（用于算文件身份）。
pub fn source_file_of_row(
    conn: &Connection,
    batch_ref: &str,
    row_uid: &str,
) -> Result<Option<String>, EngineError> {
    let file: Option<String> = conn
        .query_row(
            "SELECT source_file FROM rows WHERE batch_ref = ?1 AND row_uid = ?2 LIMIT 1",
            params![batch_ref, row_uid],
            |r| r.get(0),
        )
        .optional()?;
    Ok(file)
}

/// 上报裁决落到行状态上。
///
/// **回执是状态推进的唯一依据**（与 `push::receipt` 同源）：受理 → 已上报、
/// 退回 → 被退回、可重试失败 → 待重试。其余裁决（未发出 / 未执行 / 回执待人工核对）
/// 不是终态，不推进行状态。
///
/// 推进前先把这一行排进「待确认」（确认流落地后由人工确认动作直接置为待确认），
/// 再落到终态 —— 迁移表里没有的一律不推（没过校验的行不会被标成已上报）。
///
/// 这一行还没落进 `rows` 表时返回 `None`（合成测试数据没有来源文件，推不动）。
pub fn advance_after_push(
    conn: &mut Connection,
    batch_ref: &str,
    row_uid: &str,
    push_status: &str,
    actor: &str,
) -> Result<Option<RowState>, EngineError> {
    let target = match push_status {
        crate::push::status::ACCEPTED => RowState::Reported,
        crate::push::status::REJECTED => RowState::Rejected,
        crate::push::status::RETRYABLE => RowState::RetryPending,
        _ => return Ok(None),
    };
    let Some(source_file) = source_file_of_row(conn, batch_ref, row_uid)? else {
        return Ok(None);
    };
    let file_key = file_identity(&source_file);
    let snap = current_or_pending(conn, row_uid, &file_key)?;

    let change_to = |to: RowState, reason: &str| StateChange {
        row_uid: row_uid.to_string(),
        source_file: source_file.clone(),
        batch_ref: batch_ref.to_string(),
        to,
        expected_version: None,
        actor: actor.to_string(),
        reason: reason.to_string(),
        data_digest: snap.data_digest.clone(),
    };

    match snap.state {
        // 已经推过一次的终态：幂等命中，不再动
        RowState::Reported if target == RowState::Reported => return Ok(Some(snap.state)),
        // 待确认 / 待重试：直接落到终态
        RowState::PendingConfirmation | RowState::RetryPending => {
            let after = advance(conn, &change_to(target, "按网关回执推进行状态"))?;
            Ok(Some(after.state))
        }
        // 校验通过：先排进待上报队列，再落终态
        RowState::Validated => {
            advance(
                conn,
                &change_to(RowState::PendingConfirmation, "推送前排入待上报队列"),
            )?;
            let after = advance(conn, &change_to(target, "按网关回执推进行状态"))?;
            Ok(Some(after.state))
        }
        // 待校验：没过校验的行不被标成已上报 / 被退回 / 待重试
        _ => Ok(None),
    }
}

/// 某一批次（或全库）的行状态分布：状态 → 行数。
pub fn counts(
    conn: &Connection,
    batch_ref: Option<&str>,
) -> Result<BTreeMap<String, usize>, EngineError> {
    let mut out = BTreeMap::new();
    match batch_ref {
        Some(batch) => {
            let mut stmt = conn.prepare(
                "SELECT state, COUNT(*) FROM row_states WHERE batch_ref = ?1 GROUP BY state ORDER BY state",
            )?;
            let rows = stmt.query_map(params![batch], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (state, count) = row?;
                out.insert(state, count as usize);
            }
        }
        None => {
            let mut stmt =
                conn.prepare("SELECT state, COUNT(*) FROM row_states GROUP BY state ORDER BY state")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (state, count) = row?;
                out.insert(state, count as usize);
            }
        }
    }
    Ok(out)
}

/// 某一批次的行状态快照。
pub fn list_by_batch(
    conn: &Connection,
    batch_ref: &str,
) -> Result<Vec<StateSnapshot>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT row_uid, file_key, source_file, batch_ref, state, version, data_digest, updated_at, updated_by
         FROM row_states WHERE batch_ref = ?1 ORDER BY row_uid",
    )?;
    let rows = stmt.query_map(params![batch_ref], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, String>(8)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (row_uid, file_key, source_file, batch, state, version, digest, at, by) = row?;
        let Some(state) = RowState::parse(&state) else {
            continue;
        };
        out.push(StateSnapshot {
            row_uid,
            file_key,
            source_file,
            batch_ref: batch,
            state,
            version,
            data_digest: digest,
            updated_at: at,
            updated_by: by,
        });
    }
    Ok(out)
}

/// 某批次的行状态取值（行键 → 状态字面量）。
///
/// 与 [`list_by_batch`] 的区别：**不丢弃**不认识的取值。
/// `list_by_batch` 面向「给人看的快照」，跳过解析不了的行；
/// 本函数面向**机器归类**（未上报名单按状态分因）—— 不认识的状态必须原样带出去，
/// 由调用方报错。悄悄丢掉那一行，等于那一行在未上报名单里消失了，
/// 经办人会以为它已经报上去了。
pub fn list_state_values(
    conn: &Connection,
    batch_ref: &str,
) -> Result<Vec<(String, String)>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT row_uid, state FROM row_states WHERE batch_ref = ?1 ORDER BY row_uid",
    )?;
    let rows = stmt.query_map(params![batch_ref], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 校验完一批之后的行状态对账结果（可核对产物）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 本轮对账的行数
    pub rows: usize,
    /// 新建的行状态（第一次见到这一行）
    pub created: usize,
    /// 本轮推进过状态的行数
    pub advanced: usize,
    /// 状态没变的行数
    pub unchanged: usize,
    /// 已上报行被保护（普通校验不改回）
    pub protected_reported: usize,
    /// 已上报行内容有变化（提示可能要订正，不自动改状态）
    pub reported_changed: usize,
    /// 被退回的行修正后回到待确认
    pub rejected_fixed: usize,
    /// 复校验不通过被退回待校验
    pub regressed: usize,
    /// 本轮结束时的状态分布
    pub by_state: BTreeMap<String, usize>,
}

impl ReconcileReport {
    /// 一句话摘要（终端用）
    pub fn summary_line(&self) -> String {
        format!(
            "行状态：{} 行 · 推进 {} · 未变 {} · 已上报受保护 {} · 退回修正回待确认 {} · 复校验退回 {}",
            self.rows,
            self.advanced,
            self.unchanged,
            self.protected_reported,
            self.rejected_fixed,
            self.regressed,
        )
    }
}

/// 校验完一批之后的行状态输入（一行一条）。
#[derive(Debug, Clone)]
pub struct RowValidationInput {
    /// 行键
    pub row_uid: String,
    /// 来源文件
    pub source_file: String,
    /// 口径校验结论是不是「通过 / 有提示」（不通过 = false）
    pub conclusion_ok: bool,
    /// 本轮这一行的数据摘要
    pub data_digest: String,
}

/// 校验完一批之后，把行状态对到该在的位置上。
///
/// 规则（每条都有出处，不靠感觉）：
///
/// - **没见过的行**：建底账「待校验」，结论通过则推进到「校验通过」；
/// - **待校验**：结论通过 → 校验通过；仍不通过 → 保持待校验；
/// - **校验通过**：结论仍通过 → 保持（只更新摘要）；不通过 → 退回待校验；
/// - **待确认**：结论仍通过 → 保持（只更新摘要）；不通过 → 退回待校验（旧的确认作废）；
/// - **待重试**：保持（重试归上报侧管，校验不插手），只更新摘要；
/// - **被退回**：数据摘要变了 = 修正过 → 回到**待确认**（需重新确认）；
///   摘要没变 → 保持被退回（还没改就别急着重报）；
/// - **已上报**：**不改**（普通校验不得把已上报行改回）；摘要变了只记一条提示，
///   要重报得走明确的订正路径（`已上报 → 待确认`）。
pub fn reconcile_after_validation(
    conn: &mut Connection,
    batch_ref: &str,
    inputs: &[RowValidationInput],
) -> Result<ReconcileReport, EngineError> {
    let mut report = ReconcileReport::default();
    for input in inputs {
        report.rows += 1;
        let file_key = file_identity(&input.source_file);
        let existed = current(conn, &input.row_uid, &file_key)?;
        let before = current_or_pending(conn, &input.row_uid, &file_key)?;
        if existed.is_none() {
            report.created += 1;
        }

        // 已上报：普通校验不改状态，只提示内容变了
        if before.state == RowState::Reported {
            report.protected_reported += 1;
            if !input.data_digest.is_empty() && before.data_digest != input.data_digest {
                report.reported_changed += 1;
            }
            report.unchanged += 1;
            continue;
        }

        // 被退回：修正过（摘要变了）才回到待确认
        if before.state == RowState::Rejected {
            if !input.data_digest.is_empty() && before.data_digest != input.data_digest {
                advance(
                    conn,
                    &StateChange {
                        row_uid: input.row_uid.clone(),
                        source_file: input.source_file.clone(),
                        batch_ref: batch_ref.to_string(),
                        to: RowState::PendingConfirmation,
                        expected_version: None,
                        actor: "validate".to_string(),
                        reason: "被退回的行修正后回到待确认（需重新确认）".to_string(),
                        data_digest: input.data_digest.clone(),
                    },
                )?;
                report.advanced += 1;
                report.rejected_fixed += 1;
            } else {
                report.unchanged += 1;
            }
            continue;
        }

        // 待重试：状态归上报侧管，校验只更新摘要
        if before.state == RowState::RetryPending {
            refresh_digest(conn, &input.row_uid, &file_key, &input.data_digest)?;
            report.unchanged += 1;
            continue;
        }

        match before.state {
            RowState::PendingValidation => {
                // 第一次见到这一行：先把「待校验」的底账建上 ——
                // 没过校验的行也得有个状态（否则「还没过校验」与「没跑过」分不开）
                if existed.is_none() {
                    advance(
                        conn,
                        &StateChange {
                            row_uid: input.row_uid.clone(),
                            source_file: input.source_file.clone(),
                            batch_ref: batch_ref.to_string(),
                            to: RowState::PendingValidation,
                            expected_version: None,
                            actor: "validate".to_string(),
                            reason: "落库建底账".to_string(),
                            data_digest: input.data_digest.clone(),
                        },
                    )?;
                }
                if input.conclusion_ok {
                    advance(
                        conn,
                        &StateChange {
                            row_uid: input.row_uid.clone(),
                            source_file: input.source_file.clone(),
                            batch_ref: batch_ref.to_string(),
                            to: RowState::Validated,
                            expected_version: None,
                            actor: "validate".to_string(),
                            reason: "口径校验通过".to_string(),
                            data_digest: input.data_digest.clone(),
                        },
                    )?;
                    report.advanced += 1;
                } else {
                    report.unchanged += 1;
                }
            }
            RowState::Validated | RowState::PendingConfirmation => {
                if input.conclusion_ok {
                    refresh_digest(conn, &input.row_uid, &file_key, &input.data_digest)?;
                    report.unchanged += 1;
                } else {
                    advance(
                        conn,
                        &StateChange {
                            row_uid: input.row_uid.clone(),
                            source_file: input.source_file.clone(),
                            batch_ref: batch_ref.to_string(),
                            to: RowState::PendingValidation,
                            expected_version: None,
                            actor: "validate".to_string(),
                            reason: "复校验不通过，退回待校验".to_string(),
                            data_digest: input.data_digest.clone(),
                        },
                    )?;
                    report.advanced += 1;
                    report.regressed += 1;
                }
            }
            RowState::Reported | RowState::Rejected | RowState::RetryPending => unreachable!(),
        }
    }
    report.by_state = counts(conn, Some(batch_ref))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 六个状态互不相等且都能按字面量取回() {
        for state in RowState::ALL {
            assert_eq!(RowState::parse(state.as_str()), Some(state));
        }
        let mut seen = std::collections::BTreeSet::new();
        for state in RowState::ALL {
            assert!(seen.insert(state.as_str()), "状态字面量撞了");
        }
        assert_eq!(RowState::parse("不存在的状态"), None);
    }

    #[test]
    fn 上报后三个状态与上报侧同一处取值() {
        assert_eq!(RowState::Reported.as_str(), crate::push::status::ACCEPTED);
        assert_eq!(RowState::Rejected.as_str(), crate::push::status::REJECTED);
        assert_eq!(RowState::RetryPending.as_str(), crate::push::status::RETRYABLE);
    }

    #[test]
    fn 非法迁移被拒且报错里点名两头() {
        let err = check_transition(RowState::PendingValidation, RowState::Reported).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidStateTransition);
        assert!(err.to_string().contains("待校验"));
        assert!(err.to_string().contains("已上报"));
    }

    #[test]
    fn 迁移表里的都合法表外的都不合法() {
        for (from, to) in LEGAL_TRANSITIONS {
            assert!(can_transition(*from, *to), "{} → {} 应该合法", from, to);
        }
        // 表外的组合一律不合法
        assert!(!can_transition(RowState::PendingValidation, RowState::Reported));
        assert!(!can_transition(RowState::Validated, RowState::Reported));
        assert!(!can_transition(RowState::Reported, RowState::Rejected));
        assert!(!can_transition(RowState::Reported, RowState::Validated));
    }

    #[test]
    fn 摘要只随业务字段变() {
        let a = digest_of(&["MER-TL-04", "0301", "2026-Q3", "98"]);
        let b = digest_of(&["MER-TL-04", "0301", "2026-Q3", "98"]);
        let c = digest_of(&["MER-TL-04", "0301", "2026-Q3", "97"]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
