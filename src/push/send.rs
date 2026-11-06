//! 上报发送：把编译好的条件批量程序真的发出去，并把网关的裁决落回本机库。
//!
//! # 这一层解决什么
//!
//! 前面几层各自把一件事做扎实：计划编译（[`crate::push::program`]）、条件执行
//! （[`crate::push::executor`]）、退避重试与断点续跑（[`crate::push::retry`]）、
//! 写闸门（[`crate::push::gate`]）、回执与行级状态推进（[`crate::push::receipt`]）。
//! 这一层是**把它们接成一条链**：编译 → 恢复台账 → 过闸门 → 逐步执行 → 落库 → 交回产物。
//!
//! # 执行与落库分两段（为什么不能合成一段）
//!
//! 写闸门要**读**状态库（确认记录、行状态），落库要**写**状态库。
//! 同一个 `Repo` 上不能同时持有一个可变借用和一个不可变借用 —— 拆成两段：
//!
//! - [`execute`]：只读状态库，跑完程序，产出逐行裁决与行级推进结果；
//! - [`persist`]：拿可变借用，把回执、提交记录、行状态、审计链与批次汇总写进去。
//!
//! 两段之间没有共享的可变状态：执行段的全部产物都在 [`SendOutcome`] 里，
//! 落库段只读它。
//!
//! # 三条不变量
//!
//! | 不变量 | 怎么保证 |
//! |---|---|
//! | **已上报的行不重复推送** | 执行前先 [`crate::push::retry::restore_ledger`]：把本机库里的回执与行级状态读回台账，续跑点据此算；已上报的行本轮**不发请求** |
//! | **失败不改写成成功** | 裁决逐条走 [`crate::push::receipt::advance`]：回执不可用就是「回执待人工核对」，不写空状态；4xx 就是「被退回」，不重试 |
//! | **批末回滚只动汇总** | 汇总落库由 [`crate::push::batch::persist_after_run`] 决定（回滚时回到批前值），行级回执只增不减 |

use crate::db::batch_summary::BatchSummaryRecord;
use crate::db::repo::Repo;
use crate::errors::EngineError;
use crate::push::batch::{self, BatchTotals};
use crate::push::executor::{ExecutionReport, StepReport, SubmissionTransport};
use crate::push::program::Program;
use crate::push::receipt::{self, PersistReport, RowAdvance, SubmitIdentity, Verdict};
use crate::push::retry::{self, RetryPolicy, Sleeper};

/// 一轮发送的产物（执行段的全部输出）。
#[derive(Debug, Clone)]
pub struct SendOutcome {
    /// 执行报告（逐步结果、批前批后汇总、行级回执）
    pub report: ExecutionReport,
    /// 每一行的行级推进结果（落库段读它）
    pub advances: Vec<(SubmitIdentity, RowAdvance)>,
}

impl SendOutcome {
    /// 本轮真正发过请求的步数
    pub fn attempted(&self) -> usize {
        self.report.run_counts.attempted
    }

    /// 本轮续跑跳过的步数（此前已确认成功，**未重复推送**）
    pub fn resume_skipped(&self) -> usize {
        self.report.run_counts.resume_skipped
    }
}

/// 落库段的产物。
#[derive(Debug, Clone)]
pub struct PersistedSend {
    /// 回执与行级状态的落库报告
    pub rows: PersistReport,
    /// 批次汇总（落库后的那一版）
    pub batch_summary: BatchSummaryRecord,
    /// 批次汇总的四项口径
    pub totals: BatchTotals,
}

/// 执行一段上报程序（**只读状态库**）。
///
/// 写闸门要查确认记录与行状态，所以执行段拿的是 `&Repo`；
/// 落库段在 [`persist`] 里另拿 `&mut Repo` —— 两段不重叠。
///
/// # 参数
///
/// - `program`：编译好的条件批量提交程序（[`crate::push::program::compile`] 的产物）；
/// - `transport`：出站通道（生产是写闸门包着的网关客户端，测试里是替身）；
/// - `policy`：退避重试参数（5xx / 超时退避，4xx 不退避）。
///
/// # 返回
///
/// 逐行的行级裁决与推进结果。**执行段不写库** —— 落库由 [`persist`] 负责，
/// 这样「跑完了但落库失败」时，本机库里不会留下半截的汇总。
pub fn execute(
    repo: &Repo,
    batch_ref: &str,
    program: &Program,
    transport: &mut dyn SubmissionTransport,
    policy: &RetryPolicy,
    sleeper: &mut dyn Sleeper,
) -> Result<SendOutcome, EngineError> {
    // 台账从本机库恢复 —— 跨进程续跑的前提：
    // 上一轮已经拿到回执的行，这一轮**不再发请求**。
    let mut ledger = retry::restore_ledger(repo, batch_ref, program.len())?;
    let report = retry::execute_with_retry(program, transport, &mut ledger, policy, sleeper);

    let mut advances = Vec::with_capacity(report.steps.len());
    for step in &report.steps {
        let identity = SubmitIdentity {
            batch_ref: batch_ref.to_string(),
            row_uid: step.row_uid.clone(),
            idempotency_key: step.idempotency_key.clone(),
            payload_digest: step.payload_digest.clone(),
            revision: step.revision,
        };
        let verdict = verdict_of(step);
        let advance = receipt::advance(&identity, &verdict);
        advances.push((identity, advance));
    }

    Ok(SendOutcome { report, advances })
}

/// 把执行结果落进本机库（**拿可变借用**）。
///
/// 落三样东西：
/// 1. **回执与行级状态**（[`crate::push::receipt::persist_batch`]）：受理的回执落库，
///    退回的错误码与字段名写回原表说明列，行状态按回执推进；
/// 2. **批次汇总**（[`crate::push::batch::persist_after_run`]）：批末回滚时回到批前值，
///    行级回执不受影响；
/// 3. **审计链与恢复日志**（在 `persist_batch` 内部落）—— 「报过没有」这条事实留痕。
pub fn persist(
    repo: &mut Repo,
    batch_ref: &str,
    outcome: &SendOutcome,
    before: Option<&BatchSummaryRecord>,
) -> Result<PersistedSend, EngineError> {
    let rows = receipt::persist_batch(repo, &outcome.advances)?;
    let batch_summary = batch::persist_after_run(repo, batch_ref, &outcome.report, before)?;
    let totals = batch::totals_of(&batch_summary);
    Ok(PersistedSend {
        rows,
        batch_summary,
        totals,
    })
}

/// 步骤 → 行级裁决。
///
/// **没执行的步骤单独归类**：因前一步失败而没发出去的那一行，状态是
/// 「未执行（上一批未执行）」而不是「未发出（数据不合格）」——
/// 两回事：前者是这一批停在这儿，后者是这一行自己的数据不合格。
/// 混成一条，经办人就分不清「这批卡住了」与「这行该改数据」。
fn verdict_of(step: &StepReport) -> Verdict {
    if !step.executed && !step.resume_skipped {
        return Verdict::NotExecuted {
            message: step.note.clone(),
        };
    }
    Verdict::from_step_outcome(&step.outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repo::Repo;
    use crate::errors::ErrorCode;
    use crate::push::executor::{FailureClass, StepOutcome};
    use crate::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
    use crate::push::program::{PlanCandidate, PlanInput};
    use std::collections::BTreeMap;

    const BATCH: &str = "batch-2026-Q3";

    fn candidate(row_uid: &str, key: &str) -> PlanCandidate {
        let payload = SubmissionPayload::new(PayloadInput {
            batch_ref: BATCH,
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

    fn program_of(n: usize) -> Program {
        let candidates: Vec<PlanCandidate> = (1..=n)
            .map(|i| candidate(&format!("r{}", i), &format!("BK-{}", i)))
            .collect();
        crate::push::program::compile(&PlanInput {
            batch_ref: BATCH.to_string(),
            candidates,
            existing: BTreeMap::new(),
        })
    }

    /// 假通道：按行键给裁决，并记录被调用过哪些行
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
        fn submit(&mut self, step: &crate::push::program::Step) -> Result<StepOutcome, EngineError> {
            self.called.push(step.row_uid.clone());
            Ok(self
                .outcomes
                .get(&step.row_uid)
                .cloned()
                .unwrap_or_else(|| StepOutcome::Accepted {
                    receipt_id: format!("RCPT-{}", step.row_uid),
                    accepted_at: "2026-11-06 10:00:00".to_string(),
                    status: "accepted".to_string(),
                }))
        }
    }

    fn accepted() -> StepOutcome {
        StepOutcome::Accepted {
            receipt_id: "RCPT-x".to_string(),
            accepted_at: "2026-11-06 10:00:00".to_string(),
            status: "accepted".to_string(),
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
    fn 没执行的步骤归为未执行而不是数据不合格() {
        let program = program_of(3);
        let mut transport = FakeTransport::new(vec![("r1", rejected())]);
        let mut sleeper = crate::push::retry::RecordingSleeper::new();
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::open(&dir.path().join("state.db")).unwrap();
        let outcome = execute(
            &repo,
            BATCH,
            &program,
            &mut transport,
            &RetryPolicy::with_delays(1, 10, 100),
            &mut sleeper,
        )
        .unwrap();

        assert_eq!(transport.called, vec!["r1".to_string()]);
        let statuses: Vec<&str> = outcome
            .advances
            .iter()
            .map(|(_, a)| a.status.as_str())
            .collect();
        assert_eq!(
            statuses,
            vec![
                crate::push::status::REJECTED,
                crate::push::status::NOT_EXECUTED,
                crate::push::status::NOT_EXECUTED,
            ],
            "第 1 行被退回，后两行是「未执行」—— 不能写成「未发出（数据不合格）」"
        );
    }

    #[test]
    fn 受理的行拿到回执且身份带幂等键() {
        let program = program_of(1);
        let mut transport = FakeTransport::new(vec![]);
        let mut sleeper = crate::push::retry::RecordingSleeper::new();
        let dir = tempfile::tempdir().unwrap();
        let repo = Repo::open(&dir.path().join("state.db")).unwrap();
        let outcome = execute(
            &repo,
            BATCH,
            &program,
            &mut transport,
            &RetryPolicy::with_delays(1, 10, 100),
            &mut sleeper,
        )
        .unwrap();

        assert_eq!(outcome.advances.len(), 1);
        let (identity, advance) = &outcome.advances[0];
        assert!(!identity.idempotency_key.is_empty());
        assert_eq!(advance.status, crate::push::status::ACCEPTED);
        assert!(advance.receipt.is_some(), "受理的行要带回执");
        assert_eq!(outcome.attempted(), 1);
    }
}
