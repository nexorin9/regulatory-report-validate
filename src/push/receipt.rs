//! 回执解析与行级状态推进。
//!
//! # 这一层解决什么
//!
//! 网关给回来的东西决定这一行现在是什么处境 —— **回执是状态推进的唯一依据**：
//! 模型说像重复不算，模型说差异也不算，本地时钟到了也不算；
//! 只有网关的回执（受理 / 退回）才能把一行从「未上报」推到别的状态。
//!
//! | 网关给了什么 | 这一行变成 | 落库 |
//! |---|---|---|
//! | `200 {receipt_id, accepted_at, status}` | 已上报 | `receipts` 一行 + `submissions.status` |
//! | `4xx {error_code, error_field, message}` | 被退回 | `submissions.status` + 退回问题写回说明列 |
//! | `5xx` / 超时 / 连不上 | 待重试 | `submissions.status`（**幂等键留着**，回执没有） |
//! | `200` 但回执缺字段 / `accepted_at` 格式非法 | 回执待人工核对 | `submissions.status`（**不写空回执**） |
//!
//! # 幂等键 ↔ 回执 ID 一一对应
//!
//! 一次提交的身份是 `(幂等键, 修订号)`；一次受理的身份是 `receipt_id`。
//! 两者一一对应，由两个唯一索引兜住：
//! `idx_receipts_idem (idempotency_key, revision)` 与 `idx_receipts_receipt (receipt_id)`。
//! 落库时**先查后写**，撞了就报 `RECEIPT_CONFLICT` 并点名冲突的是哪一头 ——
//! 不覆盖、不静默改写已有的回执。
//!
//! # 源产品能力 → 融合后行为
//!
//! 源仓库的远端连接层把「端点 + 凭据 + 批次上下文」三件套组织在一起，
//! 本产品**改写**为「端点 + 凭据 + 幂等头 + 批次上下文」：
//! 幂等交给网关判定（`Idempotency-Key` 头），本机库里那份回执只是**网关裁决的留痕**，
//! 拿它当状态依据，而不是拿它当「发过了没」的唯一凭据。

use crate::db::repo::{ReceiptRow, Repo, RowPushState};
use crate::errors::{EngineError, ErrorCode};
use crate::push::client::{GatewayOutcomeKind, RowOutcome};
use crate::push::executor::{
    StepOutcome, STATUS_ACCEPTED, STATUS_BLOCKED, STATUS_INVALID_RECEIPT, STATUS_NOT_EXECUTED,
    STATUS_REJECTED, STATUS_RETRYABLE,
};
use crate::rules::model::Issue;
use crate::state::audit::{self, AuditRecord};

pub use crate::db::repo::SubmissionIdentity as SubmitIdentity;

/// 退回问题在「问题说明（含依据）」列里的规则编号。
///
/// 带 `R-PUSH-` 前缀是给落库那一步用的：校验规则的问题每次重跑都要重算，
/// 而**上报退回**留下的问题不能被重跑抹掉 —— 那一行还没报上去，退回原因要一直在。
pub const PUSH_ISSUE_RULE_ID: &str = "R-PUSH-GATEWAY";

/// 退回问题归入的「问题类型」。
pub const PUSH_ISSUE_KIND: &str = "上报退回";

/// 上报退回问题的依据出处（写进「问题说明（含依据）」列的「依据」部分）。
pub const PUSH_ISSUE_BASIS: &str = "院内卫健上报网关回执";

/// 网关裁决的四种形态（与协议层一一对应，剥掉 HTTP 细节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 受理（`200`）
    Accepted {
        receipt_id: String,
        accepted_at: String,
        status: String,
    },
    /// 幂等命中：这一行此前已上报过，**本轮没有再发请求**
    ///
    /// 回执在库里已经有了 —— 再落一条就是破坏「幂等键 ↔ 回执 ID 一一对应」。
    IdempotentHit {
        receipt_id: String,
        revision: i64,
    },
    /// 退回（`4xx`）
    Rejected {
        error_code: String,
        error_field: String,
        message: String,
    },
    /// 可退避重试（`5xx` / 超时 / 连不上）
    Retryable {
        error_code: ErrorCode,
        message: String,
    },
    /// 回执不可用（`200` 但结构不合法）
    InvalidReceipt {
        error_code: ErrorCode,
        message: String,
    },
    /// 本行数据不合格，请求根本没发出去
    Blocked {
        error_code: ErrorCode,
        message: String,
    },
    /// 因前一步失败而未执行
    NotExecuted { message: String },
}

impl Verdict {
    /// 协议层裁决 → 本层裁决
    pub fn from_row_outcome(outcome: &RowOutcome) -> Verdict {
        match &outcome.kind {
            GatewayOutcomeKind::Accepted(a) => Verdict::Accepted {
                receipt_id: a.receipt_id.clone(),
                accepted_at: a.accepted_at.clone(),
                status: a.status.clone(),
            },
            GatewayOutcomeKind::Rejected(r) => Verdict::Rejected {
                error_code: r.error_code.clone(),
                error_field: r.error_field.clone(),
                message: r.message.clone(),
            },
            GatewayOutcomeKind::Retryable { error_code, message } => Verdict::Retryable {
                error_code: *error_code,
                message: message.clone(),
            },
            GatewayOutcomeKind::Invalid { error_code, message } => Verdict::InvalidReceipt {
                error_code: *error_code,
                message: message.clone(),
            },
            GatewayOutcomeKind::Blocked { error_code, message } => Verdict::Blocked {
                error_code: *error_code,
                message: message.clone(),
            },
        }
    }

    /// 执行器裁决 → 本层裁决
    pub fn from_step_outcome(outcome: &StepOutcome) -> Verdict {
        match outcome {
            StepOutcome::Accepted {
                receipt_id,
                accepted_at,
                status,
            } => Verdict::Accepted {
                receipt_id: receipt_id.clone(),
                accepted_at: accepted_at.clone(),
                status: status.clone(),
            },
            StepOutcome::IdempotentHit { receipt_id, revision } => Verdict::IdempotentHit {
                receipt_id: receipt_id.clone(),
                revision: *revision,
            },
            StepOutcome::Failed {
                class,
                error_code,
                error_field,
                message,
            } => match class {
                crate::push::executor::FailureClass::NonRetryableRejected => Verdict::Rejected {
                    error_code: error_code.as_str().to_string(),
                    error_field: error_field.clone(),
                    message: message.clone(),
                },
                crate::push::executor::FailureClass::RetryableServer
                | crate::push::executor::FailureClass::RetryableTimeout
                | crate::push::executor::FailureClass::RetryableConnection => Verdict::Retryable {
                    error_code: *error_code,
                    message: message.clone(),
                },
                crate::push::executor::FailureClass::NonRetryableInvalidReceipt => {
                    Verdict::InvalidReceipt {
                        error_code: *error_code,
                        message: message.clone(),
                    }
                }
                crate::push::executor::FailureClass::NonRetryableBlocked => Verdict::Blocked {
                    error_code: *error_code,
                    message: message.clone(),
                },
            },
        }
    }
}

/// 行级状态推进的结果。
///
/// 这一行的状态、错误码、字段名、回执与要写回原表的说明，全在这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowAdvance {
    /// 行键
    pub row_uid: String,
    /// 「上报状态」列取值（已上报 / 被退回 / 待重试 / 回执待人工核对 …）
    pub status: String,
    /// 机器可读错误码（没有则空串）
    pub error_code: String,
    /// 网关点名的字段（没有则空串）
    pub error_field: String,
    /// 人话说明
    pub message: String,
    /// 受理回执（只有受理才有；退回 / 待重试 / 回执不可用都没有）
    pub receipt: Option<ReceiptRow>,
    /// 写回原表「问题说明（含依据）」列的文本（只有退回有）
    pub note: String,
    /// 是否是幂等命中（本轮没发请求，回执在库里已经有了）
    pub idempotent_hit: bool,
}

impl RowAdvance {
    /// 是否是「网关已受理」
    pub fn is_accepted(&self) -> bool {
        self.status == STATUS_ACCEPTED
    }

    /// 是否是「被退回」
    pub fn is_rejected(&self) -> bool {
        self.status == STATUS_REJECTED
    }
}

/// 受理时间格式是否合法。
///
/// 合法写法两种：
/// - `2026-09-06 14:30:00`（院内系统最常见，可带小数秒）
/// - `2026-09-06T14:30:00Z` / `2026-09-06T14:30:00+08:00`（ISO 8601，可带小数秒与时区偏移）
///
/// 其它写法一律判为非法 —— 受理时间是要写进上报台账的，
/// 「昨天下午大概那会儿」这种话落不进台账，也证明网关那边没按协议回。
pub fn is_valid_accepted_at(raw: &str) -> bool {
    let text = raw.trim();
    if text.len() < 19 {
        return false;
    }
    let bytes = text.as_bytes();
    // 日期段：YYYY-MM-DD
    let date_ok = bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if !date_ok {
        return false;
    }
    // 分隔符：空格或 ISO 的 T
    if bytes[10] != b' ' && bytes[10] != b'T' {
        return false;
    }
    let time_ok = bytes[11..13].iter().all(u8::is_ascii_digit)
        && bytes[13] == b':'
        && bytes[14..16].iter().all(u8::is_ascii_digit)
        && bytes[16] == b':'
        && bytes[17..19].iter().all(u8::is_ascii_digit);
    if !time_ok {
        return false;
    }
    // 尾部：允许小数秒、Z、±HH:MM 三者之一或为空
    let rest = &text[19..];
    if rest.is_empty() {
        return true;
    }
    if let Some(fraction) = rest.strip_prefix('.') {
        return !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit());
    }
    if rest == "Z" {
        return true;
    }
    if rest.len() == 6 {
        let b = rest.as_bytes();
        return (b[0] == b'+' || b[0] == b'-')
            && b[1..3].iter().all(u8::is_ascii_digit)
            && b[3] == b':'
            && b[4..6].iter().all(u8::is_ascii_digit);
    }
    false
}

/// 回执解析 + 行级状态推进（纯函数，不碰数据库）。
///
/// 状态由裁决推出，不由模型、不由本地时钟推出：
/// - 受理 → 已上报；
/// - 退回 → 被退回（错误码与字段名带在说明里，写回原表）；
/// - 可重试 → 待重试（**没有回执**，幂等键留着，等退避重试）；
/// - 回执不可用 → 回执待人工核对（**不写空状态**，也不落一条空回执）；
/// - 数据不合格 → 未发出；前一步失败 → 未执行。
pub fn advance(identity: &SubmitIdentity, verdict: &Verdict) -> RowAdvance {
    match verdict {
        Verdict::Accepted {
            receipt_id,
            accepted_at,
            status,
        } => {
            // 受理时间不合法 → 不当成功：这一行没有拿到可落账的回执
            if !is_valid_accepted_at(accepted_at) {
                return RowAdvance {
                    row_uid: identity.row_uid.clone(),
                    status: STATUS_INVALID_RECEIPT.to_string(),
                    error_code: ErrorCode::GatewayResponseInvalid.as_str().to_string(),
                    error_field: String::new(),
                    message: format!(
                        "受理时间格式不合法（原文「{}」），这一行没有拿到可落账的回执，按「回执待人工核对」处理",
                        accepted_at
                    ),
                    receipt: None,
                    note: String::new(),
                    idempotent_hit: false,
                };
            }
            RowAdvance {
                row_uid: identity.row_uid.clone(),
                status: STATUS_ACCEPTED.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: format!("网关已受理（回执 {}）", receipt_id),
                receipt: Some(ReceiptRow {
                    receipt_id: receipt_id.clone(),
                    idempotency_key: identity.idempotency_key.clone(),
                    revision: identity.revision,
                    batch_ref: identity.batch_ref.clone(),
                    row_uid: identity.row_uid.clone(),
                    status: status.clone(),
                    accepted_at: accepted_at.clone(),
                    error_code: String::new(),
                    error_field: String::new(),
                    message: String::new(),
                }),
                note: String::new(),
                idempotent_hit: false,
            }
        }
        Verdict::IdempotentHit { receipt_id, revision } => {
            // 幂等命中：这一行此前已上报，回执在库里已经有了 ——
            // 本轮没发请求，也**不新落回执**（再落一条就破坏一一对应）
            RowAdvance {
                row_uid: identity.row_uid.clone(),
                status: STATUS_ACCEPTED.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: format!(
                    "此前已上报（幂等命中 · 回执 {} · 修订 {}），本轮不重复推送",
                    receipt_id, revision
                ),
                receipt: None,
                note: String::new(),
                idempotent_hit: true,
            }
        }
        Verdict::Rejected {
            error_code,
            error_field,
            message,
        } => {
            let mut parts = vec![format!("上报被退回：错误码 {}", error_code)];
            if !error_field.is_empty() {
                parts.push(format!("字段 {}", error_field));
            }
            if !message.is_empty() {
                parts.push(message.clone());
            }
            RowAdvance {
                row_uid: identity.row_uid.clone(),
                status: STATUS_REJECTED.to_string(),
                error_code: ErrorCode::GatewayRejected.as_str().to_string(),
                error_field: error_field.clone(),
                message: parts.join(" · "),
                receipt: None,
                note: parts.join(" · "),
                idempotent_hit: false,
            }
        }
        Verdict::Retryable { error_code, message } => RowAdvance {
            row_uid: identity.row_uid.clone(),
            status: STATUS_RETRYABLE.to_string(),
            error_code: error_code.as_str().to_string(),
            error_field: String::new(),
            message: message.clone(),
            receipt: None,
            note: String::new(),
            idempotent_hit: false,
        },
        Verdict::InvalidReceipt { error_code, message } => RowAdvance {
            row_uid: identity.row_uid.clone(),
            status: STATUS_INVALID_RECEIPT.to_string(),
            error_code: error_code.as_str().to_string(),
            error_field: String::new(),
            message: message.clone(),
            receipt: None,
            note: String::new(),
            idempotent_hit: false,
        },
        Verdict::Blocked { error_code, message } => RowAdvance {
            row_uid: identity.row_uid.clone(),
            status: STATUS_BLOCKED.to_string(),
            error_code: error_code.as_str().to_string(),
            error_field: String::new(),
            message: message.clone(),
            receipt: None,
            note: String::new(),
            idempotent_hit: false,
        },
        Verdict::NotExecuted { message } => RowAdvance {
            row_uid: identity.row_uid.clone(),
            status: STATUS_NOT_EXECUTED.to_string(),
            error_code: ErrorCode::UnconfirmedRow.as_str().to_string(),
            error_field: String::new(),
            message: message.clone(),
            receipt: None,
            note: String::new(),
            idempotent_hit: false,
        },
    }
}

/// 把一次推进落库。
///
/// 三件事，按顺序：
/// 1. **提交行落库**：`submissions` 一行记这次提交的身份（幂等键 + 修订号）与当前状态；
/// 2. **回执落库**（只有受理才有）：`receipts` 一行，幂等键 ↔ 回执 ID 一一对应；
/// 3. **退回原因写回说明列**：被退回的行留一条 `R-PUSH-GATEWAY` 问题，
///    错误码与字段名在里面；已上报的行把之前退回留下的问题清掉（这一行已经报上去了）。
pub fn persist(
    repo: &mut Repo,
    identity: &SubmitIdentity,
    adv: &RowAdvance,
) -> Result<(), EngineError> {
    // 幂等命中：本轮没发请求 → 提交次数不 +1，回执也不重落
    repo.upsert_submission(
        identity,
        &adv.status,
        &adv.error_code,
        &adv.error_field,
        !adv.idempotent_hit,
    )?;

    if let Some(receipt) = &adv.receipt {
        repo.insert_receipt(receipt)?;
        // 报上去了 → 之前退回留下的问题说明作废
        repo.clear_push_issues(&identity.batch_ref, &identity.row_uid)?;
    } else if adv.is_rejected() {
        repo.upsert_push_issue(
            &identity.batch_ref,
            &identity.row_uid,
            &identity.idempotency_key,
            &adv.note,
            &adv.error_code,
            &adv.error_field,
        )?;
    }

    // 行级状态机：回执也是行状态的推进依据（与 submissions.status 同一份裁决）。
    // 推进只走状态机唯一入口；推不动（没过校验的行 / 状态表里没这一行）时如实记一条
    // 结构化日志，不静默改状态，也不因为状态推不动就把已经成功的上报说成失败。
    match crate::state::machine::advance_after_push(
        repo.conn_mut(),
        &identity.batch_ref,
        &identity.row_uid,
        &adv.status,
        "push",
    ) {
        Ok(Some(state)) => {
            crate::log::log(
                crate::log::Level::Info,
                "push.row_state_advanced",
                Some(&identity.batch_ref),
                None,
                &format!("按网关回执把该行推进到「{}」", state.as_str()),
                &[
                    ("row_uid", serde_json::json!(identity.row_uid)),
                    ("state", serde_json::json!(state.as_str())),
                    ("push_status", serde_json::json!(adv.status)),
                ],
            );
        }
        Ok(None) => {
            crate::log::log(
                crate::log::Level::Warn,
                "push.row_state_not_advanced",
                Some(&identity.batch_ref),
                None,
                "这一行的行状态本轮没推进（没过校验的行不被标成已上报）",
                &[
                    ("row_uid", serde_json::json!(identity.row_uid)),
                    ("push_status", serde_json::json!(adv.status)),
                ],
            );
        }
        Err(e) => {
            crate::log::log(
                crate::log::Level::Warn,
                "push.row_state_error",
                Some(&identity.batch_ref),
                Some(&e.code),
                &e.message,
                &[
                    ("row_uid", serde_json::json!(identity.row_uid)),
                    ("push_status", serde_json::json!(adv.status)),
                ],
            );
        }
    }

    // 审计链：这一批推了什么。
    //
    // 说明里带上**幂等键与回执 ID** —— 「幂等键 ↔ 回执 ID 一一对应」这件事
    // 平时靠两个唯一索引兜住，事后能查出来靠的就是这条审计记录。
    let event = if adv.idempotent_hit {
        audit::EVENT_PUSH_IDEMPOTENT_HIT
    } else if adv.is_accepted() {
        audit::EVENT_PUSH_ACCEPTED
    } else if adv.is_rejected() {
        audit::EVENT_PUSH_REJECTED
    } else {
        audit::EVENT_PUSH_RETRYABLE
    };
    let detail = match &adv.receipt {
        Some(receipt) => format!(
            "幂等键 {} · 修订 {} · 回执 {} · {}",
            identity.idempotency_key, identity.revision, receipt.receipt_id, adv.message
        ),
        None => format!(
            "幂等键 {} · 修订 {} · {}",
            identity.idempotency_key, identity.revision, adv.message
        ),
    };
    audit::append(
        repo.conn(),
        &AuditRecord::new(
            &identity.batch_ref,
            &identity.row_uid,
            event,
            "regval push",
            &detail,
        ),
    )?;

    // 恢复日志：状态库坏掉时，「报过没有」这条事实靠这份边车文件找回来。
    // 幂等键与回执 ID 都在里面 —— 恢复后重跑不会把同一行再推一遍。
    //
    // 写不进去**不判本次上报失败** —— 网关已经受理了，那是既成事实；
    // 记一条告警日志，让人知道这份备份没写上。
    let mut journal_record = match &adv.receipt {
        Some(receipt) => crate::db::journal::JournalRecord::receipt(
            &identity.batch_ref,
            &identity.row_uid,
            &identity.idempotency_key,
            identity.revision,
            &receipt.receipt_id,
            &adv.status,
        ),
        None => crate::db::journal::JournalRecord::submission(
            &identity.batch_ref,
            &identity.row_uid,
            &identity.idempotency_key,
            identity.revision,
            &adv.status,
            &adv.message,
        ),
    };
    if let Err(e) = repo.append_journal(&mut journal_record) {
        crate::log::log(
            crate::log::Level::Warn,
            "push.journal_write_failed",
            Some(&identity.batch_ref),
            Some(&e.code),
            &format!("恢复日志写不进去（状态库里的记录仍然有效）：{}", e.message),
            &[
                ("row_uid", serde_json::json!(identity.row_uid)),
                ("idempotency_key", serde_json::json!(identity.idempotency_key)),
            ],
        );
    }

    Ok(())
}

/// 一批推进的落库报告（可核对产物）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersistReport {
    /// 处理的行数
    pub rows: usize,
    /// 落库的回执数
    pub receipts: usize,
    /// 已上报行数
    pub accepted: usize,
    /// 被退回行数
    pub rejected: usize,
    /// 待重试行数
    pub retryable: usize,
    /// 回执待人工核对行数
    pub invalid_receipt: usize,
    /// 未发出行数
    pub blocked: usize,
}

impl PersistReport {
    /// 一句话摘要（终端用）
    pub fn summary_line(&self) -> String {
        format!(
            "回执落库：行 {} · 回执 {} · 已上报 {} · 被退回 {} · 待重试 {} · 回执待核对 {} · 未发出 {}",
            self.rows,
            self.receipts,
            self.accepted,
            self.rejected,
            self.retryable,
            self.invalid_receipt,
            self.blocked,
        )
    }
}

/// 一批推进一起落库（逐行落，撞了约束就停下并报错 —— 不覆盖已有回执）。
pub fn persist_batch(
    repo: &mut Repo,
    items: &[(SubmitIdentity, RowAdvance)],
) -> Result<PersistReport, EngineError> {
    let mut report = PersistReport::default();
    for (identity, adv) in items {
        persist(repo, identity, adv)?;
        report.rows += 1;
        if adv.receipt.is_some() {
            report.receipts += 1;
        }
        match adv.status.as_str() {
            STATUS_ACCEPTED => report.accepted += 1,
            STATUS_REJECTED => report.rejected += 1,
            STATUS_RETRYABLE => report.retryable += 1,
            STATUS_INVALID_RECEIPT => report.invalid_receipt += 1,
            STATUS_BLOCKED => report.blocked += 1,
            _ => {}
        }
    }
    Ok(report)
}

/// 库里这一批的行级上报状态（写回原表「上报状态」列用）。
///
/// 每一行取**最新修订号**的那次提交 —— 订正后报上去的才算数。
pub fn row_push_states(repo: &Repo, batch_ref: &str) -> Result<Vec<RowPushState>, EngineError> {
    repo.list_row_push_states(batch_ref)
}

/// 把被退回的行做成一条问题（写进「问题说明（含依据）」列）。
///
/// 单独抽出来是为了让落库那一步只管写库，问题怎么写在这里说清楚。
pub fn push_issue(
    row_uid: &str,
    idempotency_key: &str,
    note: &str,
    error_code: &str,
    error_field: &str,
) -> Issue {
    Issue {
        row_uid: row_uid.to_string(),
        rule_id: PUSH_ISSUE_RULE_ID.to_string(),
        issue_kind: PUSH_ISSUE_KIND.to_string(),
        severity: "阻断".to_string(),
        message: note.to_string(),
        basis: PUSH_ISSUE_BASIS.to_string(),
        issue_key: idempotency_key.to_string(),
        missing_columns: if error_field.is_empty() {
            Vec::new()
        } else {
            vec![error_field.to_string()]
        },
        error_code: error_code.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::SubmissionIdentity;
    use crate::push::client::GatewayRejection;

    fn identity() -> SubmitIdentity {
        SubmissionIdentity {
            batch_ref: "batch-a".to_string(),
            row_uid: "row-1".to_string(),
            idempotency_key: "idem-1".to_string(),
            payload_digest: "digest-1".to_string(),
            revision: 1,
        }
    }

    #[test]
    fn 受理时间格式校验认两种写法() {
        assert!(is_valid_accepted_at("2026-09-06 14:30:00"));
        assert!(is_valid_accepted_at("2026-09-06T14:30:00Z"));
        assert!(is_valid_accepted_at("2026-09-06T14:30:00+08:00"));
        assert!(is_valid_accepted_at("2026-09-06 14:30:00.123"));
        assert!(!is_valid_accepted_at("2026-09-06"));
        assert!(!is_valid_accepted_at("昨天下午"));
        assert!(!is_valid_accepted_at("2026/09/06 14:30:00"));
        assert!(!is_valid_accepted_at(""));
    }

    #[test]
    fn 受理即已上报且带回执() {
        let verdict = Verdict::Accepted {
            receipt_id: "RCPT-1".to_string(),
            accepted_at: "2026-09-06 14:30:00".to_string(),
            status: "accepted".to_string(),
        };
        let adv = advance(&identity(), &verdict);
        assert_eq!(adv.status, STATUS_ACCEPTED);
        assert!(adv.is_accepted());
        let receipt = adv.receipt.unwrap();
        assert_eq!(receipt.receipt_id, "RCPT-1");
        assert_eq!(receipt.idempotency_key, "idem-1");
        assert_eq!(receipt.revision, 1);
        assert!(adv.note.is_empty());
    }

    #[test]
    fn 受理时间非法时不认成功() {
        let verdict = Verdict::Accepted {
            receipt_id: "RCPT-1".to_string(),
            accepted_at: "昨天下午".to_string(),
            status: "accepted".to_string(),
        };
        let adv = advance(&identity(), &verdict);
        assert_eq!(adv.status, STATUS_INVALID_RECEIPT);
        assert!(adv.receipt.is_none());
        assert!(adv.message.contains("受理时间格式不合法"));
    }

    #[test]
    fn 退回带错误码与字段名() {
        let outcome = RowOutcome {
            row_uid: "row-1".to_string(),
            kind: GatewayOutcomeKind::Rejected(GatewayRejection {
                http_status: 422,
                error_code: "E_REPORTING_PERIOD".to_string(),
                error_field: "reporting_period".to_string(),
                message: "报告期格式不合法".to_string(),
            }),
        };
        let adv = advance(&identity(), &Verdict::from_row_outcome(&outcome));
        assert_eq!(adv.status, STATUS_REJECTED);
        assert!(adv.note.contains("E_REPORTING_PERIOD"));
        assert!(adv.note.contains("reporting_period"));
        assert!(adv.note.contains("报告期格式不合法"));
        assert_eq!(adv.error_code, "GATEWAY_REJECTED");
        assert!(adv.receipt.is_none());
    }

    #[test]
    fn 可重试的行停在待重试且没有回执() {
        let outcome = RowOutcome {
            row_uid: "row-1".to_string(),
            kind: GatewayOutcomeKind::Retryable {
                error_code: ErrorCode::GatewayServerError,
                message: "网关 500".to_string(),
            },
        };
        let adv = advance(&identity(), &Verdict::from_row_outcome(&outcome));
        assert_eq!(adv.status, STATUS_RETRYABLE);
        assert!(adv.receipt.is_none());
        assert_eq!(adv.error_code, "GATEWAY_SERVER_ERROR");
    }

    #[test]
    fn 回执结构不合法时写人工核对而不是空状态() {
        let outcome = RowOutcome {
            row_uid: "row-1".to_string(),
            kind: GatewayOutcomeKind::Invalid {
                error_code: ErrorCode::GatewayResponseInvalid,
                message: "回执缺字段".to_string(),
            },
        };
        let adv = advance(&identity(), &Verdict::from_row_outcome(&outcome));
        assert_eq!(adv.status, STATUS_INVALID_RECEIPT);
        assert!(adv.receipt.is_none());
        assert!(!adv.message.is_empty());
    }

    #[test]
    fn 幂等命中按已上报处理且不新落回执() {
        let adv = advance(
            &identity(),
            &Verdict::from_step_outcome(&StepOutcome::IdempotentHit {
                receipt_id: "RCPT-1".to_string(),
                revision: 1,
            }),
        );
        // 幂等命中：这一行此前已上报，回执在库里已有 —— 不再落第二条回执
        assert_eq!(adv.status, STATUS_ACCEPTED);
        assert!(adv.receipt.is_none());
        assert!(adv.idempotent_hit);
        assert!(adv.message.contains("幂等命中"));
    }

    #[test]
    fn 退回问题带错误码与字段名() {
        let issue = push_issue(
            "row-1",
            "idem-1",
            "上报被退回：错误码 E_X · 字段 reporting_period",
            "GATEWAY_REJECTED",
            "reporting_period",
        );
        assert_eq!(issue.rule_id, PUSH_ISSUE_RULE_ID);
        assert_eq!(issue.issue_kind, PUSH_ISSUE_KIND);
        assert_eq!(issue.issue_key, "idem-1");
        assert_eq!(issue.missing_columns, vec!["reporting_period".to_string()]);
        assert_eq!(issue.error_code, "GATEWAY_REJECTED");
        assert!(issue.basis.contains("网关"));
    }
}
