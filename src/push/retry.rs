//! 退避重试与断点续跑 —— 网关临时不可用时**不丢行、不重复上报**。
//!
//! # 这层解决什么
//!
//! 院内卫健上报网关是**别人家的系统**：它会 5xx、会超时、会连不上。
//! 经办人不想看到的是「推了 40 行，第 3 行报错了，后面的行全没推，
//! 而且不知道哪几行已经推上去了」。
//!
//! 这一层管三件事：
//!
//! 1. **退避重试**：5xx / 超时 / 连不上 → 指数退避后再试；**4xx 不重试**
//!    （载荷本身不合格，重试还是被退回，重试只是把退回推迟）。
//! 2. **断点续跑**：批次中断后重跑，**从最后一个已确认成功的步骤继续**，
//!    已成功的行**不重复推送**（幂等键不变，网关侧也不会重复收）。
//! 3. **重试耗尽进待办**：退避次数用完仍失败 → 该行置「待重试」，
//!    进**待办清单**（未上报名单的数据来源），由经办人手工重推。
//!
//! # 三条不变量
//!
//! | 不变量 | 怎么保证 |
//! |---|---|
//! | **已确认成功的行不重推** | 台账里**这一版**已有回执的行 → 判幂等命中，**不发请求**；续跑时前置步骤直接跳过。跨进程续跑时先 [`restore_ledger`] 把本机库里的回执与行级状态读回台账 |
//! | **重试复用同一幂等键** | 重试的是**同一个 `Step`**（同一个幂等键与载荷摘要），`Idempotency-Key` 头逐次同值 |
//! | **失败不改写成成功** | 重试耗尽仍是「待重试」；4xx 是「被退回」；每次尝试的错误码都进审计链 |
//!
//! # 续跑按**修订号**判，不按「有没有回执」判
//!
//! 一行订正后（改了值 → 载荷摘要变 → 修订号 +1）要重报。
//! 拿旧修订的回执把它跳掉，等于**把订正吞了** —— 上一版报上去了，这一版永远发不出去。
//! 所以判定口径是「回执的修订号 ≥ 这一步的修订号」才算已报上去。
//!
//! # 上次没报上去的行不会被当成已上报跳过
//!
//! 编译上报计划时要同时看**载荷摘要**与**上次那次提交的状态**：
//! 上次是「待重试」（5xx / 超时 / 连不上）且载荷没变 → 这一行**回到上报程序里**，
//! 沿用同一修订号与同一幂等键（网关侧若其实已受理，会返回幂等命中，不会变成两条上报）；
//! 上次是「已上报」→ 幂等命中，不重复推送；上次是「被退回」且载荷没变 → 不重复推送
//! （退回是数据问题，改完数据摘要就变了，会自动走订正重报）。
//!
//! # 退避参数从环境变量读（可配）
//!
//! | 变量 | 默认 | 说明 |
//! |---|---|---|
//! | `REGVAL_PUSH_RETRY_MAX_ATTEMPTS` | 3 | **总**尝试次数（含第一次） |
//! | `REGVAL_PUSH_RETRY_BASE_MS` | 500 | 退避基数（第一次重试等多久） |
//! | `REGVAL_PUSH_RETRY_MAX_DELAY_MS` | 5000 | 单次退避上限 |
//!
//! 退避序列：`base`、`base × 2`、`base × 4`…（封顶 `max_delay_ms`）。
//! 写成非整数 → 报 `CONFIG_INVALID` 并点名变量（**不静默用默认值** ——
//! 运维以为改了重试次数、其实没生效，这种事比报错难查得多）。
//!
//! # 源产品能力 → 融合后行为
//!
//! | 参考机制 | 本产品 |
//! |---|---|
//! | `execute_batch_or_rollback`：批内某条查询失败 → 剩余查询**忽略**（不重试、接着往下走） | 本产品**先退避重试**同一行；重试耗尽才停批 —— 但停批后**可续跑**，不用从头再来 |
//! | 批末条件 `ROLLBACK`（`Cond::Not { cond: Cond::Ok { step: 末步 } }`） | 语义复用（见 [`crate::push::executor`]）：回滚范围收窄到批次汇总 |
//!
//! 参考实现没有「退避重试」这一层（它的重试在更下面的存储引擎里），
//! 本产品把它显式做在**上报这一层**：这一层面对的是「网关临时不可用」
//! 这种**对方**的问题，规则与口径都得自己定。

use std::time::Instant;

use crate::config::{optional_usize, EnvSource};
use crate::db::repo::{PendingRetryRow, Repo};
use crate::errors::{EngineError, ErrorCode};
use crate::push::executor::{
    BatchLedger, ExecutionReport, FailureClass, RowReceipt, RowStatus, StepOutcome,
    SubmissionTransport, STATUS_RETRYABLE,
};
use crate::push::program::{Program, Step};

/// 环境变量：总尝试次数（含第一次）
pub const ENV_MAX_ATTEMPTS: &str = "REGVAL_PUSH_RETRY_MAX_ATTEMPTS";
/// 环境变量：退避基数（毫秒）
pub const ENV_BASE_DELAY_MS: &str = "REGVAL_PUSH_RETRY_BASE_MS";
/// 环境变量：单次退避上限（毫秒）
pub const ENV_MAX_DELAY_MS: &str = "REGVAL_PUSH_RETRY_MAX_DELAY_MS";

/// 默认总尝试次数：首发 1 次 + 重试 2 次
pub const DEFAULT_MAX_ATTEMPTS: usize = 3;
/// 默认退避基数：500 毫秒
pub const DEFAULT_BASE_DELAY_MS: u64 = 500;
/// 默认单次退避上限：5 秒
pub const DEFAULT_MAX_DELAY_MS: u64 = 5_000;
/// 默认退避倍数：每次翻倍
pub const DEFAULT_MULTIPLIER: u32 = 2;

/// 退避策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// 总尝试次数（含第一次）。`1` = 不重试。
    pub max_attempts: usize,
    /// 退避基数（毫秒）
    pub base_delay_ms: u64,
    /// 单次退避上限（毫秒）
    pub max_delay_ms: u64,
    /// 退避倍数
    pub multiplier: u32,
    /// 参数来源（人话：这轮用的是哪套退避参数）
    pub source: String,
}

impl Default for RetryPolicy {
    fn default() -> RetryPolicy {
        RetryPolicy {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay_ms: DEFAULT_BASE_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            multiplier: DEFAULT_MULTIPLIER,
            source: format!(
                "内置默认值（总尝试 {} 次 · 基数 {} ms · 上限 {} ms）",
                DEFAULT_MAX_ATTEMPTS, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_DELAY_MS
            ),
        }
    }
}

impl RetryPolicy {
    /// 指定总尝试次数（单测与内部调用用）
    pub fn with_max_attempts(max_attempts: usize) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            ..RetryPolicy::default()
        }
    }

    /// 指定退避参数（单测用：退避基数与上限都压到毫秒级，跑得快）
    pub fn with_delays(max_attempts: usize, base_delay_ms: u64, max_delay_ms: u64) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay_ms,
            max_delay_ms,
            ..RetryPolicy::default()
        }
    }

    /// 第 `attempt` 次尝试**之前**要等多久（毫秒）。
    ///
    /// `attempt` 是 1 起的尝试序号：第 1 次不等（首发不延迟），
    /// 第 2 次等 `base`，第 3 次等 `base × 2`…… 封顶 `max_delay_ms`。
    pub fn delay_for(&self, attempt: usize) -> u64 {
        if attempt <= 1 {
            return 0;
        }
        let mut delay = self.base_delay_ms;
        for _ in 1..(attempt - 1) {
            delay = delay.saturating_mul(self.multiplier as u64);
            if delay >= self.max_delay_ms {
                return self.max_delay_ms;
            }
        }
        delay.min(self.max_delay_ms)
    }

    /// 这一类失败要不要再试（4xx 永远不重试）
    pub fn should_retry(&self, class: FailureClass, attempt: usize) -> bool {
        class.is_retryable() && attempt < self.max_attempts
    }
}

/// 取退避参数：环境变量 → 内置默认值。
pub fn resolve_policy(src: &dyn EnvSource) -> Result<RetryPolicy, EngineError> {
    let mut sources: Vec<String> = Vec::new();
    let mut policy = RetryPolicy::default();

    if let Some(v) = optional_usize(src, ENV_MAX_ATTEMPTS)? {
        if v == 0 {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                format!("环境变量 {} 至少要是 1（1 = 不重试）", ENV_MAX_ATTEMPTS),
                ENV_MAX_ATTEMPTS.to_string(),
            ));
        }
        policy.max_attempts = v;
        sources.push(format!("环境变量 {}", ENV_MAX_ATTEMPTS));
    }
    if let Some(v) = optional_usize(src, ENV_BASE_DELAY_MS)? {
        policy.base_delay_ms = v as u64;
        sources.push(format!("环境变量 {}", ENV_BASE_DELAY_MS));
    }
    if let Some(v) = optional_usize(src, ENV_MAX_DELAY_MS)? {
        policy.max_delay_ms = v as u64;
        sources.push(format!("环境变量 {}", ENV_MAX_DELAY_MS));
    }
    if policy.max_delay_ms < policy.base_delay_ms {
        policy.max_delay_ms = policy.base_delay_ms;
    }
    if !sources.is_empty() {
        policy.source = format!(
            "{}（总尝试 {} 次 · 基数 {} ms · 上限 {} ms）",
            sources.join(" + "),
            policy.max_attempts,
            policy.base_delay_ms,
            policy.max_delay_ms
        );
    }
    Ok(policy)
}

/// 睡觉 —— 抽成接口，单测里**不真的等**。
pub trait Sleeper {
    /// 等这么久（毫秒）
    fn sleep_ms(&mut self, ms: u64);
}

/// 真等（生产用）
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep_ms(&mut self, ms: u64) {
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// 只记不等（单测用）：把每次退避时长记下来，断言退避序列
#[derive(Debug, Clone, Default)]
pub struct RecordingSleeper {
    /// 每次退避的时长（毫秒，按发生顺序）
    pub waits: Vec<u64>,
}

impl RecordingSleeper {
    /// 新建一个记录器
    pub fn new() -> RecordingSleeper {
        RecordingSleeper::default()
    }
}

impl Sleeper for RecordingSleeper {
    fn sleep_ms(&mut self, ms: u64) {
        self.waits.push(ms);
    }
}

/// 一次尝试的留痕 —— **每次尝试都记一笔**（错误码 + 耗时）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    /// 步骤号（1 起）
    pub step: usize,
    /// 行键
    pub row_uid: String,
    /// 第几次尝试（1 起）
    pub attempt: usize,
    /// 这一次的结果（accepted / idempotent_hit / failed）
    pub outcome: String,
    /// 机器可读错误码（成功时为空串）
    pub error_code: String,
    /// 这一次耗时（毫秒）
    pub duration_ms: u64,
    /// 这一次**之前**退避等了多久（首发为 0）
    pub wait_before_ms: u64,
    /// 人话说明
    pub message: String,
}

impl AttemptRecord {
    /// JSON 产物（可核对）
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "step": self.step,
            "row_uid": self.row_uid,
            "attempt": self.attempt,
            "outcome": self.outcome,
            "error_code": self.error_code,
            "duration_ms": self.duration_ms,
            "wait_before_ms": self.wait_before_ms,
            "message": self.message,
        })
    }
}

/// 带退避重试的出站通道。
///
/// 包装一个真实通道（[`crate::push::client::GatewayClient`] 或测试替身）：
/// 可重试的失败按 [`RetryPolicy`] 退避后再试，**不可重试的失败立刻返回**。
///
/// 重试的是**同一个 `Step`** —— 幂等键与载荷摘要逐次相同，
/// 所以 `Idempotency-Key` 头逐次同值，网关侧不会收到两条不同的上报。
pub struct RetryingTransport<'a> {
    inner: &'a mut dyn SubmissionTransport,
    policy: RetryPolicy,
    sleeper: &'a mut dyn Sleeper,
    attempts: Vec<AttemptRecord>,
    current_step: usize,
}

impl<'a> RetryingTransport<'a> {
    /// 包装一个通道
    pub fn new(
        inner: &'a mut dyn SubmissionTransport,
        policy: RetryPolicy,
        sleeper: &'a mut dyn Sleeper,
    ) -> RetryingTransport<'a> {
        RetryingTransport {
            inner,
            policy,
            sleeper,
            attempts: Vec::new(),
            current_step: 0,
        }
    }

    /// 退避策略（人话：这轮用的是哪套参数）
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// 每次尝试的留痕
    pub fn attempts(&self) -> &[AttemptRecord] {
        &self.attempts
    }

    /// 取走每次尝试的留痕（审计链写入用）
    pub fn take_attempts(&mut self) -> Vec<AttemptRecord> {
        std::mem::take(&mut self.attempts)
    }
}

impl SubmissionTransport for RetryingTransport<'_> {
    fn begin_step(&mut self, step_no: usize, _step: &Step) {
        self.current_step = step_no;
    }

    fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError> {
        let mut attempt = 1usize;
        loop {
            // 退避：第 1 次不等，第 2 次等基数，之后翻倍
            let wait = self.policy.delay_for(attempt);
            if wait > 0 {
                self.sleeper.sleep_ms(wait);
            }

            let started = Instant::now();
            let raw = self.inner.submit(step);
            let duration_ms = started.elapsed().as_millis() as u64;

            let outcome = match raw {
                Ok(outcome) => outcome,
                Err(err) => StepOutcome::Failed {
                    class: FailureClass::of_code(err.code),
                    error_code: err.code,
                    error_field: err.location.unwrap_or_default(),
                    message: err.message,
                },
            };

            let error_code = outcome
                .error_code()
                .map(|c| c.as_str().to_string())
                .unwrap_or_default();
            self.attempts.push(AttemptRecord {
                step: self.current_step,
                row_uid: step.row_uid.clone(),
                attempt,
                outcome: outcome_kind(&outcome).to_string(),
                error_code,
                duration_ms,
                wait_before_ms: wait,
                message: outcome.zh(),
            });

            // 不是可重试的失败 → 立刻返回（4xx 重试还是被退回）
            let class = match outcome.failure_class() {
                Some(class) => class,
                None => return Ok(outcome),
            };
            if !self.policy.should_retry(class, attempt) {
                // 退避次数用完 → 这一行留在「待重试」，进待办清单
                return Ok(outcome);
            }
            attempt += 1;
        }
    }
}

/// 裁决 → 结果标签
fn outcome_kind(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Accepted { .. } => "accepted",
        StepOutcome::IdempotentHit { .. } => "idempotent_hit",
        StepOutcome::Failed { .. } => "failed",
    }
}

/// 续跑计划 —— 从第几步接着跑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// 从第几步开始跑（1 起）；`len + 1` 表示**整批都已确认成功**，没有要跑的
    pub resume_step: usize,
    /// 此前已确认成功的步数
    pub completed: usize,
    /// 还要推的行键（按步骤序）
    pub pending_row_uids: Vec<String>,
}

/// 算续跑点：台账里**这一版已经报上去**的行**不重推**，从第一个没报上去的步骤接着跑。
///
/// 按**修订号**判，不按「有没有回执」判：一行订正后（修订号 +1）要重报，
/// 拿旧修订的回执把它跳掉，等于把订正吞了。
pub fn plan_resume(program: &Program, ledger: &BatchLedger) -> ResumePlan {
    let mut resume_step = program.len() + 1;
    let mut pending: Vec<String> = Vec::new();
    for (i, step) in program.steps().iter().enumerate() {
        if ledger.has_receipt_for(step) {
            continue;
        }
        if resume_step == program.len() + 1 {
            resume_step = i + 1;
        }
        pending.push(step.row_uid.clone());
    }
    ResumePlan {
        resume_step,
        completed: resume_step - 1,
        pending_row_uids: pending,
    }
}

/// 从本机库恢复台账 —— **跨进程续跑**的前提。
///
/// 批次中断后进程重启，新进程里的台账是空的。
/// 不把「已经报上去的行」读回来，续跑就会把已成功的行再推一遍：
/// 网关侧靠幂等键挡住（不会变成两条上报），但本机白白多发一次请求，
/// 而且「已上报」会被写成「已上报」两次 —— 台账里看不出哪一行是真推上去的。
///
/// 恢复两样东西：
/// - **行级回执**（`receipts` 表）→ 这一行哪一版已经报上去了；
/// - **行级上报状态**（`submissions` 表）→ 上一轮是被退回了还是退避重试耗尽了，
///   续跑时这些结论不该被抹成一片空白。
pub fn restore_ledger(
    repo: &Repo,
    batch_ref: &str,
    planned_steps: usize,
) -> Result<BatchLedger, EngineError> {
    let mut ledger = BatchLedger::new(batch_ref, planned_steps);
    for r in repo.list_receipts(batch_ref)? {
        ledger.add_receipt(RowReceipt {
            row_uid: r.row_uid.clone(),
            receipt_id: r.receipt_id.clone(),
            accepted_at: r.accepted_at.clone(),
            status: r.status.clone(),
            revision: r.revision,
            idempotency_key: r.idempotency_key.clone(),
            // 回执表不存载荷摘要（摘要在 submissions 表里）；续跑判定用不到
            payload_digest: String::new(),
        });
    }
    for s in repo.list_row_push_states(batch_ref)? {
        ledger.restore_row_status(RowStatus {
            row_uid: s.row_uid.clone(),
            status: s.status.clone(),
            error_code: s.error_code.clone(),
            error_field: s.error_field.clone(),
            message: s.message.clone(),
            receipt_id: s.receipt_id.clone(),
        });
    }
    Ok(ledger)
}

/// 退避重试 + 断点续跑地执行一段上报程序。
///
/// - **先算续跑点**：台账里已有回执的步骤**不执行、不推送**（记「续跑跳过」）；
/// - **可重试的失败退避重试**：5xx / 超时 / 连不上按 [`RetryPolicy`] 退避；
///   4xx **不重试**；
/// - **每次尝试都留痕**：错误码与耗时写进审计链（`push.retry.attempt`）；
/// - **重试耗尽**：该行留在「待重试」，由 [`retry_todo`] 汇成待办清单。
pub fn execute_with_retry(
    program: &Program,
    transport: &mut dyn SubmissionTransport,
    ledger: &mut BatchLedger,
    policy: &RetryPolicy,
    sleeper: &mut dyn Sleeper,
) -> ExecutionReport {
    let plan = plan_resume(program, ledger);
    let start = plan.resume_step.saturating_sub(1);

    let mut retrying = RetryingTransport::new(transport, policy.clone(), sleeper);
    let report = crate::push::executor::execute_with_start(program, &mut retrying, ledger, start);

    // 每次尝试的错误码与耗时 → 审计链
    for attempt in retrying.attempts() {
        ledger.record_audit_event(
            &attempt.row_uid,
            "push.retry.attempt",
            &attempt.error_code,
            &format!(
                "第 {} 步 · 第 {} 次尝试 · 耗时 {} ms · 退避 {} ms · {}",
                attempt.step, attempt.attempt, attempt.duration_ms, attempt.wait_before_ms, attempt.message
            ),
        );
    }

    report
}

/// 待办清单的一条 —— **重试耗尽的行进这里**（未上报名单的数据来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryTodoItem {
    /// 行键
    pub row_uid: String,
    /// 幂等键（手工重推时**复用同一个**，避免重复上报）
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
    /// 最后一次的错误码
    pub error_code: String,
    /// 对端点名的字段（没有则空串）
    pub error_field: String,
    /// 已经推过几次
    pub attempts: i64,
    /// 最后一次尝试时间
    pub last_attempt_at: String,
    /// 下一步该做什么（人话）
    pub next_action: String,
}

impl RetryTodoItem {
    /// JSON 产物（可核对）
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "row_uid": self.row_uid,
            "idempotency_key": self.idempotency_key,
            "revision": self.revision,
            "error_code": self.error_code,
            "error_field": self.error_field,
            "attempts": self.attempts,
            "last_attempt_at": self.last_attempt_at,
            "next_action": self.next_action,
        })
    }
}

/// 把「待重试」的行汇成待办清单。
///
/// 输入来自本机库 `submissions` 表里状态为「待重试」的行
/// （[`crate::db::repo::Repo::list_pending_retry_rows`]）。
pub fn retry_todo(rows: &[PendingRetryRow]) -> Vec<RetryTodoItem> {
    rows.iter()
        .map(|r| RetryTodoItem {
            row_uid: r.row_uid.clone(),
            idempotency_key: r.idempotency_key.clone(),
            revision: r.revision,
            error_code: r.error_code.clone(),
            error_field: r.error_field.clone(),
            attempts: r.attempt_count,
            last_attempt_at: r.updated_at.clone(),
            next_action: format!(
                "可由经办人手工重推：复用同一幂等键 {}（网关侧若已受理会返回幂等命中，不会重复上报）；\
                 先看错误码 {} 再决定改数据还是直接重推",
                r.idempotency_key, r.error_code
            ),
        })
        .collect()
}

/// 待办清单的表头（导出用）
pub const RETRY_TODO_HEADERS: &[&str] = &[
    "行键",
    "幂等键",
    "修订号",
    "错误码",
    "字段",
    "已尝试次数",
    "最后尝试时间",
    "下一步",
];

/// 待办清单 → CSV 行（脱敏：只带标识与错误码，不带业务明细）
pub fn retry_todo_csv(items: &[RetryTodoItem]) -> String {
    let mut out = String::new();
    out.push_str(&RETRY_TODO_HEADERS.join(","));
    out.push('\n');
    for item in items {
        let fields = [
            item.row_uid.clone(),
            item.idempotency_key.clone(),
            item.revision.to_string(),
            item.error_code.clone(),
            item.error_field.clone(),
            item.attempts.to_string(),
            item.last_attempt_at.clone(),
            item.next_action.clone(),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| csv_cell(f))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// CSV 单元格转义
fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// 「待重试」状态取值（与契约「上报状态」列同一套说法）
pub const PENDING_RETRY_STATUS: &str = STATUS_RETRYABLE;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::executor::{StepOutcome, SubmissionTransport};
    use crate::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
    use crate::push::program::{PlanCandidate, PlanInput};
    use std::collections::BTreeMap;

    /// 造一个候选行
    fn candidate(row_uid: &str, key: &str) -> PlanCandidate {
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
            authority_state: "authoritative".to_string(),
            match_key: "MER-TL-04|0301|2026-Q3".to_string(),
            payload,
        }
    }

    /// 造一个 N 步程序
    fn program_of(n: usize) -> Program {
        let candidates: Vec<PlanCandidate> = (1..=n)
            .map(|i| candidate(&format!("r{}", i), &format!("BK-{}", i)))
            .collect();
        crate::push::program::compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates,
            existing: BTreeMap::new(),
        })
    }

    /// 假通道：按行键给出裁决序列（第几次调用给第几个裁决），并记录调用次数
    struct ScriptedTransport {
        outcomes: Vec<StepOutcome>,
        calls: Vec<String>,
    }

    impl ScriptedTransport {
        fn new(outcomes: Vec<StepOutcome>) -> ScriptedTransport {
            ScriptedTransport {
                outcomes,
                calls: Vec::new(),
            }
        }
    }

    impl SubmissionTransport for ScriptedTransport {
        fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError> {
            self.calls.push(step.row_uid.clone());
            let idx = self.calls.len().saturating_sub(1);
            Ok(self
                .outcomes
                .get(idx)
                .cloned()
                .unwrap_or_else(|| StepOutcome::Accepted {
                    receipt_id: format!("RCPT-{}", step.row_uid),
                    accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
                    status: "accepted".to_string(),
                }))
        }
    }

    fn accepted() -> StepOutcome {
        StepOutcome::Accepted {
            receipt_id: "RCPT-x".to_string(),
            accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
            status: "accepted".to_string(),
        }
    }

    fn server_error() -> StepOutcome {
        StepOutcome::Failed {
            class: FailureClass::RetryableServer,
            error_code: ErrorCode::GatewayServerError,
            error_field: String::new(),
            message: "网关返回服务端错误（HTTP 500）".to_string(),
        }
    }

    fn rejected() -> StepOutcome {
        StepOutcome::Failed {
            class: FailureClass::NonRetryableRejected,
            error_code: ErrorCode::GatewayRejected,
            error_field: "reporting_period".to_string(),
            message: "网关退回：报告期不合规".to_string(),
        }
    }

    #[test]
    fn 退避序列按基数翻倍且封顶() {
        let policy = RetryPolicy::with_delays(5, 100, 350);
        assert_eq!(policy.delay_for(1), 0);
        assert_eq!(policy.delay_for(2), 100);
        assert_eq!(policy.delay_for(3), 200);
        assert_eq!(policy.delay_for(4), 350, "超过上限就封顶");
        assert_eq!(policy.delay_for(9), 350);
    }

    #[test]
    fn 可重试的失败按策略重试到上限为止() {
        let policy = RetryPolicy::with_delays(3, 10, 100);
        let program = program_of(1);
        let mut transport = ScriptedTransport::new(vec![
            server_error(),
            server_error(),
            server_error(),
            accepted(),
        ]);
        let mut sleeper = RecordingSleeper::new();
        let mut ledger = BatchLedger::new("b-1", program.len());
        let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

        assert_eq!(transport.calls.len(), 3, "总尝试次数上限生效（不发起第 4 次）");
        assert_eq!(sleeper.waits, vec![10, 20], "退避序列 10 → 20");
        assert_eq!(report.run_counts.retryable, 1);
        assert!(!report.completed);
    }

    #[test]
    fn 四xx不重试() {
        let policy = RetryPolicy::with_delays(3, 10, 100);
        let program = program_of(1);
        let mut transport = ScriptedTransport::new(vec![rejected(), accepted()]);
        let mut sleeper = RecordingSleeper::new();
        let mut ledger = BatchLedger::new("b-1", program.len());
        let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

        assert_eq!(transport.calls.len(), 1, "4xx 只发一次");
        assert!(sleeper.waits.is_empty(), "4xx 不退避");
        assert_eq!(report.run_counts.rejected, 1);
    }

    #[test]
    fn 每次尝试的错误码与耗时进审计链() {
        let policy = RetryPolicy::with_delays(2, 10, 100);
        let program = program_of(1);
        let mut transport = ScriptedTransport::new(vec![server_error(), accepted()]);
        let mut sleeper = RecordingSleeper::new();
        let mut ledger = BatchLedger::new("b-1", program.len());
        execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

        let audit = ledger.audit_for("r1");
        let attempts: Vec<_> = audit
            .iter()
            .filter(|a| a.event == "push.retry.attempt")
            .collect();
        assert_eq!(attempts.len(), 2, "两次尝试各留一笔");
        assert_eq!(attempts[0].error_code, "GATEWAY_SERVER_ERROR");
        assert!(attempts[0].detail.contains("耗时"));
        assert!(attempts[0].detail.contains("退避"));
    }
}
