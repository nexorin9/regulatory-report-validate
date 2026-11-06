//! 写闸门的执行点：把 [`crate::state::confirm`] 的判定接到上报通道上。
//!
//! # 这一层解决什么
//!
//! 判定规则住在状态层（[`crate::state::confirm::write_gate`]）——「谁确认过、
//! 是不是被撤销了、行状态到哪一步」这些事实归状态层管。**执行**归上报层管：
//! 在真正把请求发出去之前，先问一次闸门。
//!
//! 拦下来的是**请求本身**：闸门不过，**一个字节都不往网关发**。
//! 不是「发了再看看」—— 发了就是报上去了，网关那边已经生效。
//!
//! # 拦下时怎么表现
//!
//! 拦下不算「传输失败」，算**阻断**（[`FailureClass::NonRetryableBlocked`]）：
//! 错误码 `UNCONFIRMED_ROW`，人话说明写清为什么被拦。
//! 执行器按失败即停批处理 —— 一行没过闸门，后面的行不接着推。
//!
//! # 留痕
//!
//! 每次拦下都往审计链追加一条 `push_blocked`：谁在什么时候想推了一行没确认的行。
//! 判定本身是纯读（[`crate::state::confirm::write_gate`] 不写库），
//! 留痕只发生在这个执行点上。

use rusqlite::Connection;

use crate::errors::EngineError;
use crate::push::executor::{FailureClass, StepOutcome, SubmissionTransport};
use crate::push::program::Step;
use crate::state::audit::{self, AuditRecord};
use crate::state::confirm::{self, GateVerdict};

/// 带写闸门的上报通道：包在真实出站客户端外面，推之前先问闸门。
///
/// 用法：
///
/// ```text
/// let mut gated = GatedTransport::new(&mut client, repo.conn(), &batch_ref);
/// let report = execute(&program, &mut gated, &mut ledger);
/// ```
pub struct GatedTransport<'a> {
    /// 真正发请求的通道（院内卫健上报网关出站客户端）
    inner: &'a mut dyn SubmissionTransport,
    /// 本机校验库（闸门查确认记录与行状态用）
    conn: &'a Connection,
    /// 批次号
    batch_ref: String,
    /// 被闸门拦下的行键（按拦下顺序）
    blocked: Vec<String>,
}

impl<'a> GatedTransport<'a> {
    /// 包一层闸门。
    pub fn new(
        inner: &'a mut dyn SubmissionTransport,
        conn: &'a Connection,
        batch_ref: &str,
    ) -> GatedTransport<'a> {
        GatedTransport {
            inner,
            conn,
            batch_ref: batch_ref.trim().to_string(),
            blocked: Vec::new(),
        }
    }

    /// 被闸门拦下的行键
    pub fn blocked(&self) -> &[String] {
        &self.blocked
    }

    /// 这一批里还有没有没确认就行的（收尾自检用）
    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }
}

impl SubmissionTransport for GatedTransport<'_> {
    fn submit(&mut self, step: &Step) -> Result<StepOutcome, EngineError> {
        let verdict = confirm::write_gate(self.conn, &self.batch_ref, &step.row_uid)?;
        match verdict {
            GateVerdict::Allowed { .. } => self.inner.submit(step),
            GateVerdict::Rejected { reason, detail } => {
                self.blocked.push(step.row_uid.clone());
                // 留痕：谁在什么时候想推了一行没过闸门的行
                let _ = audit::append(
                    self.conn,
                    &AuditRecord::new(
                        &self.batch_ref,
                        &step.row_uid,
                        audit::EVENT_PUSH_BLOCKED,
                        "push",
                        &format!("{}（{}）", reason.zh(), detail),
                    ),
                );
                Ok(StepOutcome::Failed {
                    class: FailureClass::NonRetryableBlocked,
                    error_code: crate::errors::ErrorCode::UnconfirmedRow,
                    error_field: String::new(),
                    message: format!("写闸门拦下：{}（{}）", reason.zh(), detail),
                })
            }
        }
    }

    fn begin_step(&mut self, step_no: usize, step: &Step) {
        self.inner.begin_step(step_no, step);
    }
}
