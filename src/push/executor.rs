//! 条件批量提交程序的执行器 —— 逐步执行、失败分类、批末条件回滚。
//!
//! # 这层解决什么
//!
//! 计划（[`crate::push::program`]）把一批要上报的行编成**有条件的批量提交程序**：
//! 第 i 步只在第 i-1 步成功后执行。执行器负责把这套程序真的跑起来，并且回答
//! 三个问题：
//!
//! 1. **每一步成没成？成不了是哪一类？** —— 5xx 可重试、4xx 不可重试、幂等命中
//!    不重推、网络超时可重试。失败**绝不被改写成成功**。
//! 2. **第 k 步挂了，后面还跑不跑？** —— 不跑。上报是串行的：网关上一行没受理，
//!    后面的行推过去也只是多一批要被退回的行。
//! 3. **这一批算不算数？** —— 整批没跑完，**批次汇总回到批前值**；
//!    但**已在网关生效的行级回执不回滚** —— 网关侧已经受理的事实不可撤销。
//!
//! # 批末条件回滚：范围被收窄的那一步
//!
//! 源产品的批末 `ROLLBACK` 回滚的是**整个事务**：批里前几步写进去的数据一并撤销。
//! 本产品**只回滚批次汇总**（批次计数与汇总值回到批前状态），理由是：
//!
//! - 批次汇总说的是「这批数据整体算不算数」—— 整批没跑完就不落账，汇总回批前值，
//!   下次重跑还是从零算起，不会把半批当成一批；
//! - 行级回执说的是「网关已经受理了这一行」—— 这是**已经发生的外部事实**，
//!   本机库改不掉，回滚它等于假装没上报过。网关那边真撤不回来，撤的只能是本地账。
//!
//! 这条区分是源产品语义与本产品业务事实的**分歧点**：回滚范围收窄到汇总，
//! 行级回执只增不减。
//!
//! # 续跑：不重复推送
//!
//! 批次中断后重跑同一台账：已经拿到回执的行**不再发请求**（幂等命中），
//! 从断掉的那一步继续。幂等键与载荷摘要不变，网关侧的幂等判定也就不会重复收。
//!
//! # 源产品能力 → 融合后行为
//!
//! | 参考机制 | 本产品 |
//! |---|---|
//! | `execute_batch_or_rollback`：批里某条查询失败 → 剩余查询忽略 + 事务回滚 | 第 k 步失败 → 第 k+1..N 步**不执行**；批末条件 `ROLLBACK` **只回滚批次汇总** |
//! | 条件 `ROLLBACK` 步：`Cond::Not { cond: Cond::Ok { step: 末步 } }` | 同构：末步没成功 → 挂条件回滚步（[`crate::push::program::Cond::Not`]） |
//! | `make_batch_program`：第 i 步挂 `Cond::Ok { step: i - 1 }` | 条件链由编译器给出，执行器**按 `step.cond` 判定**（不假定一定是线性链） |
//! | 回滚步的结果被丢弃（`result_builder.take(batch_len)`） | 回滚步不产出行级回执：它只动汇总 |
//!
//! 源产品的步骤是一条 SQL，本产品的步骤是**上报一行**（行键 + 幂等键 + 载荷）。
//!
//! # 失败不静默
//!
//! 每一步的失败都写两处：**该行的状态**（含机器可读错误码）与**审计链**
//! （追加写，不修改历史）。失败不会被吞掉，也不会被改写成成功 —— 上报接上去的
//! 必须是网关的裁决，不是本地的时间，也不是执行器的乐观假设。

use std::collections::BTreeMap;

use crate::db::repo::now_iso8601;
use crate::errors::{EngineError, ErrorCode};
use crate::push::client::{GatewayClient, GatewayOutcomeKind, SubmissionRequest};
use crate::push::program::{Program, Step};

// —— 行状态取值（与契约「上报状态」列同一套说法）——
// 取值本体在 `push::status` 里定义一次，这里只是换个名字对外暴露，
// 免得同一串字面量在两处各写一份（写岔了就出现「静默跳过」这种难查的毛病）。
pub use crate::push::status::{
    ACCEPTED as STATUS_ACCEPTED, BLOCKED as STATUS_BLOCKED,
    IDEMPOTENT_HIT as STATUS_IDEMPOTENT_HIT, INVALID_RECEIPT as STATUS_INVALID_RECEIPT,
    NOT_EXECUTED as STATUS_NOT_EXECUTED, REJECTED as STATUS_REJECTED,
    RETRYABLE as STATUS_RETRYABLE,
};

/// 执行产物版本
pub const EXECUTION_SCHEMA_VERSION: &str = "regval.execution/1.0.0";

/// 失败分类 —— 决定这一行要不要重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// 可重试：网关服务端错误（5xx）
    RetryableServer,
    /// 可重试：调用超时
    RetryableTimeout,
    /// 可重试：连不上网关
    RetryableConnection,
    /// 不可重试：网关退回（4xx）—— 载荷本身不合格，重试还是被退回
    NonRetryableRejected,
    /// 不可重试：本行数据不合格，请求根本没发出去
    NonRetryableBlocked,
    /// 不可重试：回执不可用（200 但结构不合法）—— 不当成功
    NonRetryableInvalidReceipt,
}

impl FailureClass {
    /// 中文说法
    pub fn zh(&self) -> &'static str {
        match self {
            FailureClass::RetryableServer => "可重试（网关服务端错误）",
            FailureClass::RetryableTimeout => "可重试（调用超时）",
            FailureClass::RetryableConnection => "可重试（连不上网关）",
            FailureClass::NonRetryableRejected => "不可重试（网关退回）",
            FailureClass::NonRetryableBlocked => "不可重试（本行数据不合格）",
            FailureClass::NonRetryableInvalidReceipt => "不可重试（回执不可用）",
        }
    }

    /// 是否可退避重试
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            FailureClass::RetryableServer
                | FailureClass::RetryableTimeout
                | FailureClass::RetryableConnection
        )
    }

    /// 由机器可读错误码推出分类（错误码体系是唯一出处，不另写一套映射）
    pub fn of_code(code: ErrorCode) -> FailureClass {
        match code {
            ErrorCode::GatewayServerError => FailureClass::RetryableServer,
            ErrorCode::GatewayTimeout => FailureClass::RetryableTimeout,
            ErrorCode::GatewayConnectionFailed => FailureClass::RetryableConnection,
            ErrorCode::GatewayRejected => FailureClass::NonRetryableRejected,
            ErrorCode::GatewayResponseInvalid => FailureClass::NonRetryableInvalidReceipt,
            _ => FailureClass::NonRetryableBlocked,
        }
    }

    /// 该分类对应的行状态取值
    pub fn status(&self) -> &'static str {
        match self {
            FailureClass::RetryableServer
            | FailureClass::RetryableTimeout
            | FailureClass::RetryableConnection => STATUS_RETRYABLE,
            FailureClass::NonRetryableRejected => STATUS_REJECTED,
            FailureClass::NonRetryableBlocked => STATUS_BLOCKED,
            FailureClass::NonRetryableInvalidReceipt => STATUS_INVALID_RECEIPT,
        }
    }
}

/// 一步的裁决结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// 网关受理（200）—— 这一行已上报
    Accepted {
        /// 回执号
        receipt_id: String,
        /// 受理时间
        accepted_at: String,
        /// 网关给的状态
        status: String,
    },
    /// 幂等命中 —— 这一行此前已上报过，**本轮不重复推送**
    IdempotentHit {
        /// 此前那一版的回执号
        receipt_id: String,
        /// 此前那一版的修订号
        revision: i64,
    },
    /// 失败（**绝不改写成成功**）
    Failed {
        /// 失败分类
        class: FailureClass,
        /// 机器可读错误码
        error_code: ErrorCode,
        /// 对端点名的字段（哪一列不合格）
        error_field: String,
        /// 人话说明
        message: String,
    },
}

impl StepOutcome {
    /// 这一步算不算成功（受理 + 幂等命中都算：行已经是「已上报」的状态）
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            StepOutcome::Accepted { .. } | StepOutcome::IdempotentHit { .. }
        )
    }

    /// 是否失败
    pub fn is_failure(&self) -> bool {
        matches!(self, StepOutcome::Failed { .. })
    }

    /// 失败分类（成功时为 `None`）
    pub fn failure_class(&self) -> Option<FailureClass> {
        match self {
            StepOutcome::Failed { class, .. } => Some(*class),
            _ => None,
        }
    }

    /// 机器可读错误码（成功时为 `None`）
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            StepOutcome::Failed { error_code, .. } => Some(*error_code),
            _ => None,
        }
    }

    /// 对端点名的字段（没有则空串）
    pub fn error_field(&self) -> &str {
        match self {
            StepOutcome::Failed { error_field, .. } => error_field,
            _ => "",
        }
    }

    /// 一句话结论（终端与导出用）
    pub fn zh(&self) -> String {
        match self {
            StepOutcome::Accepted {
                receipt_id,
                accepted_at,
                status,
            } => format!("已上报 · 回执 {} · 受理时间 {} · {}", receipt_id, accepted_at, status),
            StepOutcome::IdempotentHit {
                receipt_id,
                revision,
            } => format!("幂等命中 · 回执 {} · 修订 {}（不重复推送）", receipt_id, revision),
            StepOutcome::Failed {
                class,
                error_code,
                error_field,
                message,
            } => {
                let mut text = format!("{}（{}）", class.zh(), error_code.as_str());
                if !error_field.is_empty() {
                    text.push_str(&format!(" · 字段 {}", error_field));
                }
                if !message.is_empty() {
                    text.push_str(&format!("：{}", message));
                }
                text
            }
        }
    }
}

/// 行级回执 —— **网关侧已生效的事实**。
///
/// 批末回滚**不动**这份数据：网关已经受理了这一行，本机库改不掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowReceipt {
    /// 行键
    pub row_uid: String,
    /// 回执号
    pub receipt_id: String,
    /// 受理时间
    pub accepted_at: String,
    /// 网关给的状态
    pub status: String,
    /// 修订号
    pub revision: i64,
    /// 幂等键
    pub idempotency_key: String,
    /// 载荷摘要
    pub payload_digest: String,
}

/// 行级状态 —— 这一行现在是什么处境（失败时带机器可读错误码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowStatus {
    /// 行键
    pub row_uid: String,
    /// 状态取值（[`STATUS_ACCEPTED`] 等）
    pub status: String,
    /// 机器可读错误码（没有则空串）
    pub error_code: String,
    /// 对端点名的字段（没有则空串）
    pub error_field: String,
    /// 人话说明
    pub message: String,
    /// 回执号（拿到回执才有）
    pub receipt_id: String,
}

/// 审计链条目（追加写，不修改历史）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// 时间
    pub ts: String,
    /// 批次号
    pub batch_ref: String,
    /// 行键（批次级事件为空串）
    pub row_uid: String,
    /// 事件名
    pub event: String,
    /// 机器可读错误码（没有则空串）
    pub error_code: String,
    /// 详情
    pub detail: String,
}

/// 本轮执行的明细计数 —— **不受批末回滚影响**。
///
/// 批次汇总说的是「这批整体算不算数」（整批没跑完就回到批前值）；
/// 本轮计数说的是「这一轮发生了什么」（第几步失败、几行没执行）。
/// 后者不被回滚 —— 回滚的是账，不是事实。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunCounts {
    /// 真正发过请求的步数
    pub attempted: usize,
    /// 网关受理的行数
    pub accepted: usize,
    /// 幂等命中的行数
    pub idempotent_hit: usize,
    /// 被退回的行数（4xx）
    pub rejected: usize,
    /// 待重试的行数（5xx / 超时 / 连不上）
    pub retryable: usize,
    /// 未发出的行数（本行数据不合格）
    pub blocked: usize,
    /// 回执待人工核对的行数
    pub invalid_receipt: usize,
    /// 未执行的行数（因前一步失败）
    pub not_executed: usize,
    /// 续跑跳过的行数（此前已确认成功，本轮**未重复推送**）
    pub resume_skipped: usize,
}

impl RunCounts {
    /// 「已上报行数」口径（本次受理 + 幂等命中）
    pub fn reported(&self) -> usize {
        self.accepted + self.idempotent_hit
    }
}

/// 批次汇总 —— **可以回滚的那部分**。
///
/// 汇总说的是「这批整体算不算数」；行级回执说的是「网关受理了哪几行」。
/// 批末条件回滚只动这份汇总。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchSummary {
    /// 计划里的总步数
    pub planned_steps: usize,
    /// 真正发过请求的步数
    pub attempted: usize,
    /// 网关受理的行数
    pub accepted: usize,
    /// 幂等命中的行数（此前已上报，本轮没推）
    pub idempotent_hit: usize,
    /// 被退回的行数（4xx）
    pub rejected: usize,
    /// 待重试的行数（5xx / 超时 / 连不上）
    pub retryable: usize,
    /// 未发出的行数（本行数据不合格）
    pub blocked: usize,
    /// 回执待人工核对的行数
    pub invalid_receipt: usize,
    /// 未执行的行数（因前一步失败）
    pub not_executed: usize,
    /// 已上报行的填报值合计
    pub sum_value: f64,
    /// 已上报行的分子合计
    pub sum_numerator: f64,
    /// 已上报行的分母合计
    pub sum_denominator: f64,
}

impl BatchSummary {
    /// 「已上报行数」口径：本次受理 + 幂等命中（两者都是「这一行已上报」）
    pub fn reported(&self) -> usize {
        self.accepted + self.idempotent_hit
    }

    /// 「待重试」口径：可重试的失败行数
    pub fn pending_retry(&self) -> usize {
        self.retryable
    }

    /// 是否为空汇总（一步都没跑）
    pub fn is_empty(&self) -> bool {
        self.planned_steps == 0
            && self.attempted == 0
            && self.reported() == 0
            && self.rejected == 0
            && self.retryable == 0
            && self.blocked == 0
            && self.invalid_receipt == 0
            && self.not_executed == 0
    }
}

/// 批次台账 —— 执行器写它，回滚只动其中的汇总。
///
/// - **汇总**：可回滚（批末条件回滚）；
/// - **行级回执**：只增不减（网关侧已生效的事实不可撤销）；
/// - **行状态**：每一行现在是什么处境，失败带错误码；
/// - **审计链**：追加写，每一步的成败都留痕。
pub struct BatchLedger {
    batch_ref: String,
    summary: BatchSummary,
    receipts: Vec<RowReceipt>,
    row_status: BTreeMap<String, RowStatus>,
    audit: Vec<AuditEntry>,
}

impl BatchLedger {
    /// 新建台账（批次号 + 计划步数）
    pub fn new(batch_ref: impl Into<String>, planned_steps: usize) -> BatchLedger {
        BatchLedger {
            batch_ref: batch_ref.into(),
            summary: BatchSummary {
                planned_steps,
                ..BatchSummary::default()
            },
            receipts: Vec::new(),
            row_status: BTreeMap::new(),
            audit: Vec::new(),
        }
    }

    /// 批次号
    pub fn batch_ref(&self) -> &str {
        &self.batch_ref
    }

    /// 当前批次汇总
    pub fn summary(&self) -> &BatchSummary {
        &self.summary
    }

    /// 取当前汇总的快照（批前值）
    pub fn summary_snapshot(&self) -> BatchSummary {
        self.summary.clone()
    }

    /// **批末条件回滚**：把批次汇总恢复到批前状态。
    ///
    /// 只动汇总 —— 行级回执、行状态、审计链**一律不动**。
    pub fn restore_summary(&mut self, snapshot: &BatchSummary) {
        self.summary = snapshot.clone();
    }

    /// 全部行级回执（只增不减）
    pub fn receipts(&self) -> &[RowReceipt] {
        &self.receipts
    }

    /// 某一行的回执
    pub fn receipt_of(&self, row_uid: &str) -> Option<&RowReceipt> {
        self.receipts.iter().find(|r| r.row_uid == row_uid)
    }

    /// 某一行的**最新**回执（按修订号取最大；同修订号取后落的那条）。
    ///
    /// 同一行可能被上报过多次（订正重报），续跑判定要看**最新修订**那一条，
    /// 不是先落的那条。
    pub fn latest_receipt_of(&self, row_uid: &str) -> Option<&RowReceipt> {
        self.receipts
            .iter()
            .filter(|r| r.row_uid == row_uid)
            .max_by_key(|r| r.revision)
    }

    /// 这一步是不是**已经上报过**（有回执，且回执的修订号 ≥ 这一步的修订号）。
    ///
    /// 续跑判定用这个，而不是「有没有回执」：
    /// 一行订正后（修订号 +1）要重报，拿旧修订的回执把它跳掉，
    /// 等于**把订正吞了** —— 上一版报上去了，这一版却永远发不出去。
    pub fn has_receipt_for(&self, step: &Step) -> bool {
        self.latest_receipt_of(&step.row_uid)
            .map(|r| r.revision >= step.revision())
            .unwrap_or(false)
    }

    /// 某一行的状态
    pub fn row_status(&self, row_uid: &str) -> Option<&RowStatus> {
        self.row_status.get(row_uid)
    }

    /// 全部行状态
    pub fn row_statuses(&self) -> Vec<&RowStatus> {
        self.row_status.values().collect()
    }

    /// 审计链
    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }

    /// 某一行的审计条目
    pub fn audit_for(&self, row_uid: &str) -> Vec<&AuditEntry> {
        self.audit.iter().filter(|a| a.row_uid == row_uid).collect()
    }

    /// 记一条受理（拿到回执）
    fn record_accepted(&mut self, step_no: usize, step: &Step, outcome: &StepOutcome) {
        if let StepOutcome::Accepted {
            receipt_id,
            accepted_at,
            status,
        } = outcome
        {
            self.receipts.push(RowReceipt {
                row_uid: step.row_uid.clone(),
                receipt_id: receipt_id.clone(),
                accepted_at: accepted_at.clone(),
                status: status.clone(),
                revision: step.revision(),
                idempotency_key: step.idempotency_key().to_string(),
                payload_digest: step.payload_digest().to_string(),
            });
            self.summary.accepted += 1;
            self.summary.attempted += 1;
            self.add_sums(step);
        }
        self.set_status(
            &step.row_uid,
            RowStatus {
                row_uid: step.row_uid.clone(),
                status: STATUS_ACCEPTED.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: outcome.zh(),
                receipt_id: match outcome {
                    StepOutcome::Accepted { receipt_id, .. } => receipt_id.clone(),
                    _ => String::new(),
                },
            },
        );
        self.push_audit(
            &step.row_uid,
            "push.step.accepted",
            "",
            &format!("第 {} 步上报成功：{}", step_no, outcome.zh()),
        );
    }

    /// 记一次幂等命中（不重复推送）
    fn record_idempotent_hit(&mut self, step_no: usize, step: &Step, outcome: &StepOutcome) {
        self.summary.idempotent_hit += 1;
        self.add_sums(step);
        self.set_status(
            &step.row_uid,
            RowStatus {
                row_uid: step.row_uid.clone(),
                status: STATUS_IDEMPOTENT_HIT.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: outcome.zh(),
                receipt_id: match outcome {
                    StepOutcome::IdempotentHit { receipt_id, .. } => receipt_id.clone(),
                    _ => String::new(),
                },
            },
        );
        self.push_audit(
            &step.row_uid,
            "push.step.idempotent_hit",
            "",
            &format!("第 {} 步幂等命中，不重复推送", step_no),
        );
    }

    /// 记一次失败（错误码写进行状态与审计链）
    fn record_failure(&mut self, step_no: usize, step: &Step, outcome: &StepOutcome) {
        let (class, code, field) = match outcome {
            StepOutcome::Failed {
                class,
                error_code,
                error_field,
                ..
            } => (*class, *error_code, error_field.clone()),
            _ => return,
        };
        match class {
            FailureClass::NonRetryableRejected => self.summary.rejected += 1,
            FailureClass::NonRetryableBlocked => self.summary.blocked += 1,
            FailureClass::NonRetryableInvalidReceipt => self.summary.invalid_receipt += 1,
            _ => self.summary.retryable += 1,
        }
        self.summary.attempted += 1;
        self.set_status(
            &step.row_uid,
            RowStatus {
                row_uid: step.row_uid.clone(),
                status: class.status().to_string(),
                error_code: code.as_str().to_string(),
                error_field: field,
                message: outcome.zh(),
                receipt_id: String::new(),
            },
        );
        self.push_audit(
            &step.row_uid,
            "push.step.failed",
            code.as_str(),
            &format!("第 {} 步失败：{}", step_no, outcome.zh()),
        );
    }

    /// 记一次「续跑跳过」（此前已确认成功，本轮**未重复推送**）。
    ///
    /// 计入批次汇总的「幂等命中」—— 这一行确实已经上报了，只是这一轮没再推。
    fn record_resume_skipped(&mut self, step_no: usize, step: &Step, reason: &str) {
        self.summary.idempotent_hit += 1;
        self.add_sums(step);
        let receipt_id = self
            .receipt_of(&step.row_uid)
            .map(|r| r.receipt_id.clone())
            .unwrap_or_default();
        self.set_status(
            &step.row_uid,
            RowStatus {
                row_uid: step.row_uid.clone(),
                status: STATUS_IDEMPOTENT_HIT.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: reason.to_string(),
                receipt_id,
            },
        );
        self.push_audit(
            &step.row_uid,
            "push.step.resume_skipped",
            "",
            &format!("第 {} 步（续跑跳过）：{}", step_no, reason),
        );
    }

    /// 记一次「未执行」（因前一步失败）
    fn record_not_executed(&mut self, step_no: usize, step: &Step, reason: &str) {
        self.summary.not_executed += 1;
        self.set_status(
            &step.row_uid,
            RowStatus {
                row_uid: step.row_uid.clone(),
                status: STATUS_NOT_EXECUTED.to_string(),
                error_code: String::new(),
                error_field: String::new(),
                message: reason.to_string(),
                receipt_id: String::new(),
            },
        );
        self.push_audit(
            &step.row_uid,
            "push.step.not_executed",
            "",
            &format!("第 {} 步未执行：{}", step_no, reason),
        );
    }

    /// 记一次批末条件回滚（只回滚汇总）
    fn record_rollback(&mut self, reason: &str) {
        self.push_audit("", "push.batch.rollback", "", reason);
    }

    /// 累加已上报行的汇总值
    fn add_sums(&mut self, step: &Step) {
        if let Some(v) = step.payload.value.as_number() {
            self.summary.sum_value += v;
        }
        if let Some(v) = step.payload.numerator.as_number() {
            self.summary.sum_numerator += v;
        }
        if let Some(v) = step.payload.denominator.as_number() {
            self.summary.sum_denominator += v;
        }
    }

    /// 写行状态
    fn set_status(&mut self, row_uid: &str, status: RowStatus) {
        self.row_status.insert(row_uid.to_string(), status);
    }

    /// 追加一条审计（对外可用：退避重试层把**每次尝试**的错误码与耗时记在这里）
    pub fn record_audit_event(
        &mut self,
        row_uid: &str,
        event: &str,
        error_code: &str,
        detail: &str,
    ) {
        self.push_audit(row_uid, event, error_code, detail);
    }

    /// 把一条已有回执放进台账（**续跑用**：从本机库读回的回执先灌进台账，
    /// 执行器才知道哪几行已经上报过、不该再推）
    pub fn add_receipt(&mut self, receipt: RowReceipt) {
        self.receipts.push(receipt);
    }

    /// 恢复一行状态（**续跑用**：从本机库读回的行级上报状态先灌进台账）。
    ///
    /// 续跑时这些行不是「没发生过」—— 上次是被网关退回了，还是退避重试耗尽了，
    /// 台账里都得留着，别让续跑把上一轮的结论抹成一片空白。
    pub fn restore_row_status(&mut self, status: RowStatus) {
        let row_uid = status.row_uid.clone();
        self.set_status(&row_uid, status);
    }

    /// 追加一条审计
    fn push_audit(&mut self, row_uid: &str, event: &str, error_code: &str, detail: &str) {
        self.audit.push(AuditEntry {
            ts: now_iso8601(),
            batch_ref: self.batch_ref.clone(),
            row_uid: row_uid.to_string(),
            event: event.to_string(),
            error_code: error_code.to_string(),
            detail: detail.to_string(),
        });
    }
}

/// 提交通道 —— 把一行发出去，拿到裁决。
///
/// 业务层只依赖这个抽象，不直接依赖 HTTP 客户端；测试里用替身注入。
pub trait SubmissionTransport {
    /// 提交一行，返回裁决。
    ///
    /// `Err` 只表示**通道内部**的意外错误（不是网关的裁决）；
    /// 网关的裁决一律走 `Ok(StepOutcome::…)`，且失败**不会**被改写成成功。
    fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError>;

    /// 执行器在每一步开始前告诉通道「现在是第几步、这一行是谁」。
    ///
    /// 默认什么都不做 —— 通道不关心步骤号也能用。
    /// **退避重试层**用它把「第几步第几次尝试」记进审计链。
    fn begin_step(&mut self, _step_no: usize, _step: &Step) {}
}

/// 真实的出站客户端实现：把网关裁决翻译成执行器的裁决分类。
impl SubmissionTransport for GatewayClient {
    fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError> {
        let request = SubmissionRequest {
            row_uid: step.row_uid.clone(),
            payload: step.payload.clone(),
        };
        let outcome = GatewayClient::submit(self, &request);
        Ok(outcome.into())
    }
}

/// 网关裁决 → 执行器裁决分类。
impl From<crate::push::client::RowOutcome> for StepOutcome {
    fn from(outcome: crate::push::client::RowOutcome) -> StepOutcome {
        match outcome.kind {
            GatewayOutcomeKind::Accepted(a) => StepOutcome::Accepted {
                receipt_id: a.receipt_id,
                accepted_at: a.accepted_at,
                status: a.status,
            },
            GatewayOutcomeKind::Rejected(r) => {
                let note = r.row_note();
                StepOutcome::Failed {
                    class: FailureClass::NonRetryableRejected,
                    error_code: ErrorCode::GatewayRejected,
                    error_field: r.error_field,
                    message: note,
                }
            }
            GatewayOutcomeKind::Retryable { error_code, message } => StepOutcome::Failed {
                class: FailureClass::of_code(error_code),
                error_code,
                error_field: String::new(),
                message,
            },
            GatewayOutcomeKind::Invalid { error_code, message } => StepOutcome::Failed {
                class: FailureClass::NonRetryableInvalidReceipt,
                error_code,
                error_field: String::new(),
                message,
            },
            GatewayOutcomeKind::Blocked { error_code, message } => StepOutcome::Failed {
                class: FailureClass::NonRetryableBlocked,
                error_code,
                error_field: String::new(),
                message,
            },
        }
    }
}

/// 一步的执行结果（可核对产物的一部分）。
#[derive(Debug, Clone, PartialEq)]
pub struct StepReport {
    /// 步骤号（1 起）
    pub step: usize,
    /// 行键
    pub row_uid: String,
    /// 来源表文件名
    pub source_file: String,
    /// 来源表行号
    pub row_no: usize,
    /// 指标编号
    pub indicator_code: String,
    /// 科室代码
    pub dept_code: String,
    /// 业务主键
    pub business_key: String,
    /// 幂等键
    pub idempotency_key: String,
    /// 载荷摘要
    pub payload_digest: String,
    /// 修订号
    pub revision: i64,
    /// 条件说法（人话）
    pub condition_text: String,
    /// 是否真的执行了（false = 因前一步失败而未执行，或续跑跳过）
    pub executed: bool,
    /// 是否**续跑跳过**（此前已确认成功，本轮未重复推送）
    pub resume_skipped: bool,
    /// 裁决
    pub outcome: StepOutcome,
    /// 人话说明
    pub note: String,
}

/// 执行报告 —— 一次执行的可核对产物。
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    /// 批次号
    pub batch_ref: String,
    /// 逐步结果
    pub steps: Vec<StepReport>,
    /// 批前汇总
    pub summary_before: BatchSummary,
    /// 批后汇总（回滚后就是批前值）
    pub summary_after: BatchSummary,
    /// 本轮执行的明细计数（**不受回滚影响** —— 这一轮发生了什么）
    pub run_counts: RunCounts,
    /// 是否触发了批末条件回滚
    pub rollback_applied: bool,
    /// 回滚原因（人话）
    pub rollback_reason: String,
    /// 整批是否跑完（每一步都成功）
    pub completed: bool,
    /// 本轮从第几步开始跑（1 起；`None` = 从头跑）
    pub resumed_from_step: Option<usize>,
    /// 第一个失败的步骤号（1 起）
    pub failed_step: Option<usize>,
    /// 行级回执（只增不减）
    pub receipts: Vec<RowReceipt>,
}

impl ExecutionReport {
    /// 可读摘要行（终端用）
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for s in &self.steps {
            lines.push(format!(
                "第 {} 步  行键 {}  {}  {}",
                s.step,
                s.row_uid,
                if s.executed { "已执行" } else { "未执行" },
                s.note
            ));
        }
        let c = &self.run_counts;
        lines.push(format!(
            "本轮：计划 {} 步 · 尝试 {} 步 · 已上报 {} 行（受理 {} · 幂等命中 {}）· 被退回 {} 行 · \
             待重试 {} 行 · 未发出 {} 行 · 回执待核对 {} 行 · 未执行 {} 行 · 续跑跳过 {} 行",
            self.summary_after.planned_steps,
            c.attempted,
            c.reported(),
            c.accepted,
            c.idempotent_hit,
            c.rejected,
            c.retryable,
            c.blocked,
            c.invalid_receipt,
            c.not_executed,
            c.resume_skipped,
        ));
        if self.rollback_applied {
            lines.push(format!("批末条件回滚：{}", self.rollback_reason));
        }
        lines
    }

    /// JSON 产物（可核对）
    pub fn to_json(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "step": s.step,
                    "row_uid": s.row_uid,
                    "source_file": s.source_file,
                    "row_no": s.row_no,
                    "indicator_code": s.indicator_code,
                    "dept_code": s.dept_code,
                    "business_key": s.business_key,
                    "idempotency_key": s.idempotency_key,
                    "payload_digest": s.payload_digest,
                    "revision": s.revision,
                    "condition": s.condition_text,
                    "executed": s.executed,
                    "resume_skipped": s.resume_skipped,
                    "outcome": outcome_json(&s.outcome),
                    "note": s.note,
                })
            })
            .collect();
        let receipts: Vec<serde_json::Value> = self
            .receipts
            .iter()
            .map(|r| {
                serde_json::json!({
                    "row_uid": r.row_uid,
                    "receipt_id": r.receipt_id,
                    "accepted_at": r.accepted_at,
                    "status": r.status,
                    "revision": r.revision,
                    "idempotency_key": r.idempotency_key,
                    "payload_digest": r.payload_digest,
                })
            })
            .collect();
        serde_json::json!({
            "schema_version": EXECUTION_SCHEMA_VERSION,
            "batch_ref": self.batch_ref,
            "steps": steps,
            "receipts": receipts,
            "summary_before": summary_json(&self.summary_before),
            "summary_after": summary_json(&self.summary_after),
            "run_counts": run_counts_json(&self.run_counts),
            "rollback_applied": self.rollback_applied,
            "rollback_reason": self.rollback_reason,
            "completed": self.completed,
            "failed_step": self.failed_step,
            "resumed_from_step": self.resumed_from_step,
        })
        .to_string()
    }
}

/// 裁决 → JSON
fn outcome_json(outcome: &StepOutcome) -> serde_json::Value {
    match outcome {
        StepOutcome::Accepted {
            receipt_id,
            accepted_at,
            status,
        } => serde_json::json!({
            "kind": "accepted",
            "receipt_id": receipt_id,
            "accepted_at": accepted_at,
            "status": status,
        }),
        StepOutcome::IdempotentHit {
            receipt_id,
            revision,
        } => serde_json::json!({
            "kind": "idempotent_hit",
            "receipt_id": receipt_id,
            "revision": revision,
        }),
        StepOutcome::Failed {
            class,
            error_code,
            error_field,
            message,
        } => serde_json::json!({
            "kind": "failed",
            "failure_class": class.zh(),
            "retryable": class.is_retryable(),
            "error_code": error_code.as_str(),
            "error_field": error_field,
            "message": message,
        }),
    }
}

/// 汇总 → JSON
fn summary_json(summary: &BatchSummary) -> serde_json::Value {
    serde_json::json!({
        "planned_steps": summary.planned_steps,
        "attempted": summary.attempted,
        "accepted": summary.accepted,
        "idempotent_hit": summary.idempotent_hit,
        "reported": summary.reported(),
        "rejected": summary.rejected,
        "retryable": summary.retryable,
        "blocked": summary.blocked,
        "invalid_receipt": summary.invalid_receipt,
        "not_executed": summary.not_executed,
        "sum_value": summary.sum_value,
        "sum_numerator": summary.sum_numerator,
        "sum_denominator": summary.sum_denominator,
    })
}

/// 本轮计数 → JSON
fn run_counts_json(counts: &RunCounts) -> serde_json::Value {
    serde_json::json!({
        "attempted": counts.attempted,
        "accepted": counts.accepted,
        "idempotent_hit": counts.idempotent_hit,
        "reported": counts.reported(),
        "rejected": counts.rejected,
        "retryable": counts.retryable,
        "blocked": counts.blocked,
        "invalid_receipt": counts.invalid_receipt,
        "not_executed": counts.not_executed,
        "resume_skipped": counts.resume_skipped,
    })
}

/// 执行条件批量提交程序。
///
/// # 执行规则
///
/// 1. **按条件判定**：第 i 步的 `Cond::Ok { step: j }` 要求第 j 步**成功**；
///    条件不满足的步骤**不执行**（记「未执行」）。
/// 2. **幂等命中不重推**：台账里已有这一行的回执 → 直接判幂等命中，**不发请求**。
/// 3. **失败即停批**：第 k 步失败 → 第 k+1..N 步不执行。
/// 4. **失败不静默**：每步失败都写进行状态（含错误码）与审计链。
/// 5. **批末条件回滚**：末步没成功 → 批次汇总回到批前值；
///    **行级回执不回滚**（网关侧已生效的事实不可撤销）。
///
/// 空程序不执行任何步，也不触发回滚（没有末步可条件）。
pub fn execute(
    program: &Program,
    transport: &mut dyn SubmissionTransport,
    ledger: &mut BatchLedger,
) -> ExecutionReport {
    execute_with_start(program, transport, ledger, 0)
}

/// 从第 `start` 步开始执行（**断点续跑**）。
///
/// `start` 是 0 起的步序：`0` = 从头跑；`k` = 前 k 步**此前已确认成功**，
/// 本轮**不执行、不推送**（只记「续跑跳过」留痕）。
///
/// 续跑时条件链仍然成立：依赖「续跑前已成功」的那一步，条件按**台账里已有的回执**
/// 判定为满足 —— 否则续跑会把第一步就判成「未执行」，批次永远推不完。
pub fn execute_with_start(
    program: &Program,
    transport: &mut dyn SubmissionTransport,
    ledger: &mut BatchLedger,
    start: usize,
) -> ExecutionReport {
    let summary_before = ledger.summary_snapshot();
    let mut results: Vec<Option<StepOutcome>> = Vec::with_capacity(program.len());
    let mut reports: Vec<StepReport> = Vec::with_capacity(program.len());
    let mut failed_step: Option<usize> = None;

    for (i, step) in program.steps().iter().enumerate() {
        // —— 断点续跑：前 start 步此前已确认成功，本轮不重复推送 ——
        if i < start {
            let reason = "此前已确认成功，本轮不重复推送".to_string();
            ledger.record_resume_skipped(i + 1, step, &reason);
            results.push(None);
            reports.push(report_of(
                i + 1,
                step,
                false,
                true,
                StepOutcome::IdempotentHit {
                    receipt_id: ledger
                        .receipt_of(&step.row_uid)
                        .map(|r| r.receipt_id.clone())
                        .unwrap_or_default(),
                    revision: step.revision(),
                },
                reason.clone(),
            ));
            continue;
        }

        transport.begin_step(i + 1, step);
        let condition_ok = match step.cond.as_ref().and_then(|c| c.depends_on()) {
            Some(dep) => {
                if dep < start {
                    // 依赖的是续跑前已确认成功的步骤 → 条件成立
                    // （按修订号判：那一版确实报上去了，条件才成立）
                    ledger.has_receipt_for(&program.steps()[dep])
                } else {
                    step_succeeded(&results, dep)
                }
            }
            None => true,
        };
        if !condition_ok {
            let reason = format!(
                "第 {} 步未执行：依赖的第 {} 步没有成功（上报是串行的，前一行没受理就不接着推）",
                i + 1,
                dep_step_text(&results, step)
            );
            ledger.record_not_executed(i + 1, step, &reason);
            results.push(None);
            reports.push(report_of(i + 1, step, false, false, StepOutcome::Failed {
                class: FailureClass::NonRetryableBlocked,
                error_code: ErrorCode::UnconfirmedRow,
                error_field: String::new(),
                message: reason.clone(),
            }, reason));
            continue;
        }

        // 幂等命中：台账里已有这一行的回执 → 不重复推送
        let outcome = match ledger.receipt_of(&step.row_uid) {
            Some(receipt) => StepOutcome::IdempotentHit {
                receipt_id: receipt.receipt_id.clone(),
                revision: receipt.revision,
            },
            None => match transport.submit(step) {
                Ok(outcome) => outcome,
                Err(err) => StepOutcome::Failed {
                    class: FailureClass::of_code(err.code),
                    error_code: err.code,
                    error_field: err.location.unwrap_or_default(),
                    message: err.message,
                },
            },
        };

        match &outcome {
            StepOutcome::Accepted { .. } => ledger.record_accepted(i + 1, step, &outcome),
            StepOutcome::IdempotentHit { .. } => {
                ledger.record_idempotent_hit(i + 1, step, &outcome)
            }
            StepOutcome::Failed { .. } => ledger.record_failure(i + 1, step, &outcome),
        }

        if outcome.is_failure() && failed_step.is_none() {
            failed_step = Some(i + 1);
        }
        let note = outcome.zh();
        results.push(Some(outcome.clone()));
        reports.push(report_of(i + 1, step, true, false, outcome, note));
    }

    // —— 批末条件 ROLLBACK ——
    // 条件：Cond::Not { cond: Cond::Ok { step: 末步 } } —— 末步没成功就回滚。
    // 源产品回滚整个事务；本产品**只回滚批次汇总**，行级回执不动。
    let last_succeeded = match program.len() {
        0 => true, // 空程序：没有末步可条件，不回滚
        n => {
            if n - 1 < start {
                // 末步在续跑点之前 —— 此前已确认成功
                true
            } else {
                step_succeeded(&results, n - 1)
            }
        }
    };
    let rollback_applied = program.len() > 0 && !last_succeeded;
    let rollback_reason = if rollback_applied {
        let reason = match failed_step {
            Some(k) => format!(
                "第 {} 步失败，其后的步骤未执行；批次汇总回到批前值（行级回执保留 —— 网关侧已受理的事实不可撤销）",
                k
            ),
            None => "整批未跑完；批次汇总回到批前值（行级回执保留 —— 网关侧已受理的事实不可撤销）"
                .to_string(),
        };
        ledger.restore_summary(&summary_before);
        ledger.record_rollback(&reason);
        reason
    } else {
        String::new()
    };

    let run_counts = count_runs(&reports);
    ExecutionReport {
        batch_ref: ledger.batch_ref().to_string(),
        steps: reports,
        summary_before,
        summary_after: ledger.summary_snapshot(),
        run_counts,
        rollback_applied,
        rollback_reason,
        completed: failed_step.is_none(),
        failed_step,
        resumed_from_step: if start > 0 {
            Some(start + 1)
        } else {
            None
        },
        receipts: ledger.receipts().to_vec(),
    }
}

/// 本轮执行的明细计数（**不受批末回滚影响**）
fn count_runs(reports: &[StepReport]) -> RunCounts {
    let mut counts = RunCounts::default();
    for r in reports {
        if r.resume_skipped {
            counts.resume_skipped += 1;
            continue;
        }
        if !r.executed {
            counts.not_executed += 1;
            continue;
        }
        counts.attempted += 1;
        match &r.outcome {
            StepOutcome::Accepted { .. } => counts.accepted += 1,
            StepOutcome::IdempotentHit { .. } => counts.idempotent_hit += 1,
            StepOutcome::Failed { class, .. } => match class {
                FailureClass::NonRetryableRejected => counts.rejected += 1,
                FailureClass::NonRetryableBlocked => counts.blocked += 1,
                FailureClass::NonRetryableInvalidReceipt => counts.invalid_receipt += 1,
                _ => counts.retryable += 1,
            },
        }
    }
    counts
}

/// 第 `index` 步是否成功（`None` = 该步没执行 / 失败）
fn step_succeeded(results: &[Option<StepOutcome>], index: usize) -> bool {
    matches!(results.get(index), Some(Some(outcome)) if outcome.is_success())
}

/// 依赖步的说法（人话）
fn dep_step_text(results: &[Option<StepOutcome>], step: &Step) -> String {
    match step.cond.as_ref().and_then(|c| c.depends_on()) {
        Some(dep) => {
            let done = matches!(results.get(dep), Some(Some(o)) if o.is_success());
            if done {
                format!("第 {} 步（已成功）", dep + 1)
            } else {
                format!("第 {} 步（未成功）", dep + 1)
            }
        }
        None => "无".to_string(),
    }
}

/// 造一条步骤报告
fn report_of(
    step_no: usize,
    step: &Step,
    executed: bool,
    resume_skipped: bool,
    outcome: StepOutcome,
    note: String,
) -> StepReport {
    StepReport {
        step: step_no,
        row_uid: step.row_uid.clone(),
        source_file: step.source_file.clone(),
        row_no: step.row_no,
        indicator_code: step.payload.indicator_code.clone(),
        dept_code: step.payload.dept_code.clone(),
        business_key: step.payload.business_key.clone(),
        idempotency_key: step.idempotency_key().to_string(),
        payload_digest: step.payload_digest().to_string(),
        revision: step.revision(),
        condition_text: step.condition_text(),
        executed,
        resume_skipped,
        outcome,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
    use crate::push::program::{Cond, PlanCandidate, PlanInput};

    /// 造一个候选行
    fn candidate(row_uid: &str, state: &str, key: &str) -> PlanCandidate {
        let payload = SubmissionPayload::new(PayloadInput {
            batch_ref: "b-1",
            org_code: "ORG-001",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: key,
            numerator: NumericField::Number(980.0),
            denominator: NumericField::Number(1000.0),
            value: NumericField::Number(98.0),
            unit: "%",
            basis: "国卫办医函〔2021〕28号 附件",
            rule_version: "2021.1",
        });
        PlanCandidate {
            row_uid: row_uid.to_string(),
            source_ns: "科室一表".to_string(),
            source_file: "科室一表.csv".to_string(),
            row_no: 1,
            authority_state: state.to_string(),
            match_key: "MER-TL-04|0301|2026-Q3".to_string(),
            payload,
        }
    }

    /// 造一个 N 步程序（每步一行权威行）
    fn program_of(n: usize) -> Program {
        let candidates: Vec<PlanCandidate> = (1..=n)
            .map(|i| candidate(&format!("r{}", i), "authoritative", &format!("BK-{}", i)))
            .collect();
        crate::push::program::compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates,
            existing: BTreeMap::new(),
        })
    }

    /// 假通道：按行键返回裁决，并记录被调用了哪些行
    struct FakeTransport {
        outcomes: BTreeMap<String, StepOutcome>,
        called: Vec<String>,
    }

    impl FakeTransport {
        fn new(outcomes: Vec<(&str, StepOutcome)>) -> FakeTransport {
            FakeTransport {
                outcomes: outcomes
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
                called: Vec::new(),
            }
        }
    }

    impl SubmissionTransport for FakeTransport {
        fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError> {
            self.called.push(step.row_uid.clone());
            Ok(self
                .outcomes
                .get(&step.row_uid)
                .cloned()
                .unwrap_or_else(|| StepOutcome::Accepted {
                    receipt_id: format!("RCPT-{}", step.row_uid),
                    accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
                    status: "accepted".to_string(),
                }))
        }
    }

    fn accepted(row: &str) -> StepOutcome {
        StepOutcome::Accepted {
            receipt_id: format!("RCPT-{}", row),
            accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
            status: "accepted".to_string(),
        }
    }

    fn failed(class: FailureClass, code: ErrorCode) -> StepOutcome {
        StepOutcome::Failed {
            class,
            error_code: code,
            error_field: String::new(),
            message: "网关没受理".to_string(),
        }
    }

    #[test]
    fn 全部成功时汇总不回滚() {
        let program = program_of(3);
        let mut transport = FakeTransport::new(vec![
            ("r1", accepted("r1")),
            ("r2", accepted("r2")),
            ("r3", accepted("r3")),
        ]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        let report = execute(&program, &mut transport, &mut ledger);
        assert!(report.completed);
        assert!(!report.rollback_applied);
        assert_eq!(report.summary_after.accepted, 3);
        assert_eq!(report.summary_after.reported(), 3);
        assert_eq!(ledger.receipts().len(), 3);
    }

    #[test]
    fn 第k步失败则后续步骤不执行() {
        let program = program_of(4);
        let mut transport = FakeTransport::new(vec![(
            "r2",
            failed(FailureClass::RetryableServer, ErrorCode::GatewayServerError),
        )]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        let report = execute(&program, &mut transport, &mut ledger);
        assert_eq!(transport.called, vec!["r1".to_string(), "r2".to_string()]);
        assert!(!report.steps[2].executed);
        assert!(!report.steps[3].executed);
        assert_eq!(report.failed_step, Some(2));
        assert_eq!(report.run_counts.not_executed, 2);
    }

    #[test]
    fn 批末失败时汇总回到批前值但行级回执保留() {
        let program = program_of(3);
        let mut transport = FakeTransport::new(vec![(
            "r2",
            failed(FailureClass::NonRetryableRejected, ErrorCode::GatewayRejected),
        )]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        let before = ledger.summary_snapshot();
        let report = execute(&program, &mut transport, &mut ledger);
        assert!(report.rollback_applied);
        assert_eq!(report.summary_after, before, "批次汇总应回到批前值");
        assert_eq!(ledger.summary(), &before);
        // 行级回执不回滚：第 1 行的回执仍在
        assert_eq!(ledger.receipts().len(), 1);
        assert!(ledger.receipt_of("r1").is_some());
        assert_eq!(ledger.receipt_of("r1").unwrap().receipt_id, "RCPT-r1");
    }

    #[test]
    fn 失败不静默_行状态与审计链都带错误码() {
        let program = program_of(2);
        let mut transport = FakeTransport::new(vec![(
            "r1",
            failed(FailureClass::RetryableTimeout, ErrorCode::GatewayTimeout),
        )]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        execute(&program, &mut transport, &mut ledger);
        let status = ledger.row_status("r1").expect("失败行要有状态");
        assert_eq!(status.status, STATUS_RETRYABLE);
        assert_eq!(status.error_code, "GATEWAY_TIMEOUT");
        let audit = ledger.audit_for("r1");
        assert!(!audit.is_empty());
        assert_eq!(audit[0].error_code, "GATEWAY_TIMEOUT");
    }

    #[test]
    fn 续跑不重复推送已成功的行() {
        let program = program_of(3);
        let mut transport = FakeTransport::new(vec![(
            "r2",
            failed(FailureClass::RetryableServer, ErrorCode::GatewayServerError),
        )]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        execute(&program, &mut transport, &mut ledger);
        assert_eq!(transport.called.len(), 2);

        // 第二轮：通道全部成功，第 1 行应命中幂等（不重推）
        let mut transport2 = FakeTransport::new(vec![]);
        let report2 = execute(&program, &mut transport2, &mut ledger);
        assert_eq!(transport2.called, vec!["r2".to_string(), "r3".to_string()]);
        assert_eq!(report2.summary_after.idempotent_hit, 1);
        assert_eq!(report2.summary_after.accepted, 2);
    }

    #[test]
    fn 空程序不执行也不回滚() {
        let program = program_of(0);
        let mut transport = FakeTransport::new(vec![]);
        let mut ledger = BatchLedger::new("b-1", 0);
        let report = execute(&program, &mut transport, &mut ledger);
        assert!(transport.called.is_empty());
        assert!(!report.rollback_applied);
        assert!(report.completed);
    }

    #[test]
    fn 条件链按前一步成败判定() {
        // 手工造一个两步程序：第 2 步依赖第 1 步
        let program = program_of(2);
        assert_eq!(program.steps()[1].cond, Some(Cond::Ok { step: 0 }));
        let mut transport = FakeTransport::new(vec![(
            "r1",
            failed(FailureClass::NonRetryableBlocked, ErrorCode::InvalidNumeric),
        )]);
        let mut ledger = BatchLedger::new("b-1", program.len());
        let report = execute(&program, &mut transport, &mut ledger);
        assert!(!report.steps[1].executed);
        assert!(report.rollback_applied);
    }

    #[test]
    fn 失败分类可重试与不可重试分得开() {
        assert!(FailureClass::RetryableServer.is_retryable());
        assert!(FailureClass::RetryableTimeout.is_retryable());
        assert!(FailureClass::RetryableConnection.is_retryable());
        assert!(!FailureClass::NonRetryableRejected.is_retryable());
        assert!(!FailureClass::NonRetryableBlocked.is_retryable());
        assert!(!FailureClass::NonRetryableInvalidReceipt.is_retryable());
        assert_eq!(
            FailureClass::of_code(ErrorCode::GatewayTimeout),
            FailureClass::RetryableTimeout
        );
        assert_eq!(
            FailureClass::of_code(ErrorCode::GatewayRejected),
            FailureClass::NonRetryableRejected
        );
    }
}
