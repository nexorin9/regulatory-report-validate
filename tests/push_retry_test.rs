//! 退避重试与断点续跑的验收测试。
//!
//! 验收点（Task 33）：
//! 1. **退避次数上限生效**：可重试的失败（5xx / 超时）按策略退避重试，
//!    次数用完就停（不无限重试）；**4xx 不重试**；
//! 2. **已成功的行未被重推**：台账里已有回执的行不重复推送；
//! 3. **断点续跑不重复推送已成功行**：批次中断后从最后一个已确认成功的步骤继续；
//! 4. **重试复用同一幂等键**：同一幂等键的重试，`Idempotency-Key` 头逐次同值
//!    （断言落在协议替身**实际收到的报文**上）；
//! 5. **重试耗尽进待办清单**：该行置「待重试」，可由经办人手工重推；
//! 6. **每次尝试的错误码与耗时写审计链**。
//!
//! 对端是 [`gateway_stub`] 里的协议替身（协议兼容测试，**不是**最终对接方式）；
//! 院内真实上报网关本轮未实连（地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::collections::BTreeMap;

use gateway_stub::{StubResponse, StubServer};
use regval::config::MapEnv;
use regval::db::repo::Repo;
use regval::errors::{EngineError, ErrorCode};
use regval::push::client::{GatewayClient, GatewayConfig};
use regval::push::executor::{
    BatchLedger, FailureClass, StepOutcome, SubmissionTransport, STATUS_ACCEPTED,
    STATUS_RETRYABLE,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput, Program};
use regval::push::retry::{
    execute_with_retry, plan_resume, resolve_policy, restore_ledger, retry_todo, retry_todo_csv,
    RecordingSleeper, RetryPolicy, RetryTodoItem, ENV_BASE_DELAY_MS, ENV_MAX_ATTEMPTS,
    ENV_MAX_DELAY_MS,
};

/// 造一行权威候选
fn candidate(row_uid: &str, business_key: &str) -> PlanCandidate {
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref: "batch-2026-Q3",
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key,
        numerator: NumericField::Number(980.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
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

/// 造一个 N 步程序（每步一行权威行）
fn program_of(n: usize) -> Program {
    let candidates: Vec<PlanCandidate> = (1..=n)
        .map(|i| candidate(&format!("r{}", i), &format!("BK-{}", i)))
        .collect();
    compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates,
        existing: BTreeMap::new(),
    })
}

/// 假通道：按**调用次序**回裁决，并记录被调用了哪些行
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

    /// 全部受理的通道（默认裁决）
    fn accepting() -> ScriptedTransport {
        ScriptedTransport::new(Vec::new())
    }
}

impl SubmissionTransport for ScriptedTransport {
    fn submit(&mut self, step: &regval::push::program::Step) -> Result<StepOutcome, EngineError> {
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
        message: "上报网关返回服务端错误（HTTP 500）".to_string(),
    }
}

fn timeout() -> StepOutcome {
    StepOutcome::Failed {
        class: FailureClass::RetryableTimeout,
        error_code: ErrorCode::GatewayTimeout,
        error_field: String::new(),
        message: "调用上报网关超时".to_string(),
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
fn 退避次数上限生效_可重试失败按策略重试到上限为止() {
    let policy = RetryPolicy::with_delays(3, 10, 100);
    let program = program_of(1);
    let mut transport = ScriptedTransport::new(vec![server_error(), server_error(), server_error()]);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());

    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(
        transport.calls.len(),
        3,
        "总尝试次数上限 3 生效：第 4 次不该发出去"
    );
    assert_eq!(
        sleeper.waits,
        vec![10, 20],
        "退避序列按基数翻倍：10 → 20"
    );
    assert_eq!(report.run_counts.retryable, 1);
    assert!(!report.completed);
    let status = ledger.row_status("r1").expect("失败行要有状态");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_eq!(status.error_code, "GATEWAY_SERVER_ERROR");
}

#[test]
fn 四xx不重试() {
    let policy = RetryPolicy::with_delays(3, 10, 100);
    let program = program_of(1);
    let mut transport = ScriptedTransport::new(vec![rejected(), accepted()]);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());

    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(transport.calls.len(), 1, "4xx 只发一次（重试还是被退回）");
    assert!(sleeper.waits.is_empty(), "4xx 不退避");
    assert_eq!(report.run_counts.rejected, 1);
    assert_eq!(report.run_counts.retryable, 0);
}

#[test]
fn 超时也按策略重试且耗尽后停在待重试() {
    let policy = RetryPolicy::with_delays(2, 5, 50);
    let program = program_of(1);
    let mut transport = ScriptedTransport::new(vec![timeout(), timeout()]);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());

    execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(transport.calls.len(), 2);
    assert_eq!(sleeper.waits, vec![5]);
    let status = ledger.row_status("r1").expect("失败行要有状态");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_eq!(status.error_code, "GATEWAY_TIMEOUT");
}

#[test]
fn 断点续跑从最后一个已确认成功的步骤继续且不重复推送已成功行() {
    let policy = RetryPolicy::with_delays(1, 1, 10);
    let program = program_of(3);

    // 第一轮：第 1 行受理，第 2 行可重试失败（重试耗尽），第 3 行不执行
    let mut transport1 = ScriptedTransport::new(vec![accepted(), server_error()]);
    let mut sleeper1 = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report1 = execute_with_retry(&program, &mut transport1, &mut ledger, &policy, &mut sleeper1);
    assert_eq!(transport1.calls, vec!["r1".to_string(), "r2".to_string()]);
    assert_eq!(report1.failed_step, Some(2));
    assert!(!report1.steps[2].executed, "第 3 行因前一步失败而未执行");
    assert_eq!(ledger.receipts().len(), 1, "第 1 行已确认成功，回执在");

    // 续跑点：第 1 行已有回执 → 从第 2 步接着跑
    let plan = plan_resume(&program, &ledger);
    assert_eq!(plan.resume_step, 2);
    assert_eq!(plan.completed, 1);
    assert_eq!(
        plan.pending_row_uids,
        vec!["r2".to_string(), "r3".to_string()]
    );

    // 第二轮：第 1 行不重推，第 2、3 行接着推
    let mut transport2 = ScriptedTransport::accepting();
    let mut sleeper2 = RecordingSleeper::new();
    let report2 = execute_with_retry(&program, &mut transport2, &mut ledger, &policy, &mut sleeper2);
    assert_eq!(
        transport2.calls,
        vec!["r2".to_string(), "r3".to_string()],
        "已确认成功的行不重复推送"
    );
    assert_eq!(report2.resumed_from_step, Some(2));
    assert_eq!(report2.run_counts.resume_skipped, 1);
    assert!(report2.steps[0].resume_skipped);
    assert!(report2.completed);
    assert_eq!(report2.run_counts.accepted, 2);
}

#[test]
fn 续跑时条件链仍然成立_不会把第一步判成未执行() {
    let policy = RetryPolicy::with_delays(1, 1, 10);
    let program = program_of(3);

    let mut transport1 = ScriptedTransport::new(vec![accepted(), server_error()]);
    let mut sleeper1 = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    execute_with_retry(&program, &mut transport1, &mut ledger, &policy, &mut sleeper1);

    // 第二轮全部受理：第 2 步的条件是「第 1 步成功」—— 第 1 步是续跑前已成功的
    let mut transport2 = ScriptedTransport::accepting();
    let mut sleeper2 = RecordingSleeper::new();
    let report2 = execute_with_retry(&program, &mut transport2, &mut ledger, &policy, &mut sleeper2);

    assert!(report2.steps[1].executed, "第 2 步要执行");
    assert!(report2.steps[2].executed, "第 3 步要执行");
    assert_eq!(report2.run_counts.not_executed, 0);
    assert!(report2.completed);
}

#[test]
fn 整批都已确认成功时续跑不推送任何行() {
    let policy = RetryPolicy::with_delays(2, 1, 10);
    let program = program_of(2);

    let mut transport1 = ScriptedTransport::accepting();
    let mut sleeper1 = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    execute_with_retry(&program, &mut transport1, &mut ledger, &policy, &mut sleeper1);

    let mut transport2 = ScriptedTransport::accepting();
    let mut sleeper2 = RecordingSleeper::new();
    let report2 = execute_with_retry(&program, &mut transport2, &mut ledger, &policy, &mut sleeper2);

    assert!(transport2.calls.is_empty(), "整批已完成 → 一行都不推");
    assert_eq!(report2.run_counts.resume_skipped, 2);
    assert!(report2.completed);
}

#[test]
fn 每次尝试的错误码与耗时写审计链() {
    let policy = RetryPolicy::with_delays(2, 1, 10);
    let program = program_of(1);
    let mut transport = ScriptedTransport::new(vec![server_error(), accepted()]);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());

    execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    let attempts: Vec<_> = ledger
        .audit_for("r1")
        .into_iter()
        .filter(|a| a.event == "push.retry.attempt")
        .collect();
    assert_eq!(attempts.len(), 2, "两次尝试各留一笔");
    assert_eq!(attempts[0].error_code, "GATEWAY_SERVER_ERROR");
    assert!(attempts[0].detail.contains("耗时"), "审计要写耗时");
    assert!(attempts[0].detail.contains("退避"), "审计要写退避时长");
    assert!(attempts[0].detail.contains("第 1 次尝试"));
    assert!(attempts[1].detail.contains("第 2 次尝试"));
}

#[test]
fn 重试复用同一幂等键头_协议级断言() {
    // 替身：前两次 500，第三次 200 受理
    let stub = StubServer::start(vec![
        StubResponse::json(500, r#"{"error":{"message":"网关内部错误"}}"#),
        StubResponse::json(500, r#"{"error":{"message":"网关内部错误"}}"#),
        StubResponse::json(
            200,
            r#"{"receipt_id":"RCPT-900","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
        ),
    ]);
    let client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .expect("客户端应可建");

    let program = program_of(1);
    let policy = RetryPolicy::with_delays(3, 1, 10);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let mut transport = client;
    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(report.run_counts.accepted, 1, "第三次尝试受理");

    // 三次请求的幂等头逐次同值，且与 body 里的幂等键同值
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..3 {
        let req = stub.next_request();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/v1/regulatory-submissions");
        let header = req.header("idempotency-key").expect("幂等头要在");
        let body_key = req.body_json()["idempotency_key"]
            .as_str()
            .expect("body 要带幂等键")
            .to_string();
        assert_eq!(header, body_key, "幂等头与 body 同值");
        seen.push(header.to_string());
    }
    assert_eq!(
        seen[0], seen[1],
        "同一幂等键的重试：幂等头逐次同值"
    );
    assert_eq!(seen[1], seen[2]);
}

#[test]
fn 超时按退避重试_协议级断言() {
    // 替身：每次都拖 2.5 秒不回（客户端 1 秒超时）→ 真实 HTTP 超时路径
    let stub = StubServer::start(vec![
        StubResponse::json(200, r#"{"receipt_id":"RCPT-1","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#)
            .with_delay(2500),
        StubResponse::json(200, r#"{"receipt_id":"RCPT-2","accepted_at":"2026-11-06 09:12:01","status":"accepted"}"#)
            .with_delay(2500),
    ]);
    let client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 1,
    })
    .expect("客户端应可建");

    let program = program_of(1);
    let policy = RetryPolicy::with_delays(2, 1, 10);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let mut transport = client;
    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(report.run_counts.retryable, 1, "两次尝试都超时 → 该行待重试");
    let status = ledger.row_status("r1").expect("失败行要有状态");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_eq!(status.error_code, "GATEWAY_TIMEOUT");
    assert!(ledger.receipts().is_empty(), "超时不产出行级回执");
}

#[test]
fn 重试耗尽的行进待办清单且可手工重推() {
    // 本机库：落一条「待重试」的提交
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let identity = regval::db::repo::SubmissionIdentity {
        batch_ref: "batch-2026-Q3".to_string(),
        row_uid: "r2".to_string(),
        idempotency_key: "idem-abc".to_string(),
        payload_digest: "digest-abc".to_string(),
        revision: 1,
    };
    repo.upsert_submission(&identity, STATUS_RETRYABLE, "GATEWAY_SERVER_ERROR", "", true)
        .unwrap();

    let pending = repo.list_pending_retry_rows("batch-2026-Q3").unwrap();
    assert_eq!(pending.len(), 1, "待重试的行进未上报名单的数据来源");
    assert_eq!(pending[0].row_uid, "r2");
    assert_eq!(pending[0].idempotency_key, "idem-abc");
    assert_eq!(pending[0].error_code, "GATEWAY_SERVER_ERROR");
    assert_eq!(pending[0].attempt_count, 1);

    let todo: Vec<RetryTodoItem> = retry_todo(&pending);
    assert_eq!(todo.len(), 1);
    assert_eq!(todo[0].idempotency_key, "idem-abc");
    assert!(
        todo[0].next_action.contains("复用同一幂等键"),
        "待办要点名复用幂等键，避免重复上报：{}",
        todo[0].next_action
    );

    let csv = retry_todo_csv(&todo);
    assert!(csv.starts_with("行键,幂等键,修订号,错误码"));
    assert!(csv.contains("idem-abc"));
    assert_eq!(csv.lines().count(), 2, "表头 + 一行");
}

#[test]
fn 退避参数从环境变量读且写坏时报错点名变量() {
    let mut env = MapEnv::new();
    env.set(ENV_MAX_ATTEMPTS, "5");
    env.set(ENV_BASE_DELAY_MS, "200");
    env.set(ENV_MAX_DELAY_MS, "1000");
    let policy = resolve_policy(&env).unwrap();
    assert_eq!(policy.max_attempts, 5);
    assert_eq!(policy.base_delay_ms, 200);
    assert_eq!(policy.max_delay_ms, 1000);
    assert!(policy.source.contains(ENV_MAX_ATTEMPTS));

    let mut bad = MapEnv::new();
    bad.set(ENV_MAX_ATTEMPTS, "abc");
    let err = resolve_policy(&bad).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
    assert!(err.message.contains(ENV_MAX_ATTEMPTS), "要点名变量");

    let mut zero = MapEnv::new();
    zero.set(ENV_MAX_ATTEMPTS, "0");
    let err = resolve_policy(&zero).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
}

#[test]
fn 退避序列按基数翻倍且封顶() {
    let policy = RetryPolicy::with_delays(6, 100, 350);
    assert_eq!(policy.delay_for(1), 0, "首发不等");
    assert_eq!(policy.delay_for(2), 100);
    assert_eq!(policy.delay_for(3), 200);
    assert_eq!(policy.delay_for(4), 350, "超过上限封顶");
    assert_eq!(policy.delay_for(9), 350);
}

#[test]
fn 重试耗尽的行状态是待重试不是已上报() {
    let policy = RetryPolicy::with_delays(1, 1, 10);
    let program = program_of(1);
    let mut transport = ScriptedTransport::new(vec![server_error()]);
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());

    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    let status = ledger.row_status("r1").expect("失败行要有状态");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_ne!(status.status, STATUS_ACCEPTED, "失败绝不改写成已上报");
    assert!(ledger.receipts().is_empty(), "失败不产出行级回执");
    assert!(!report.completed);
}

// —— 跨进程续跑（批次中断 → 进程重启 → 从最后一个已确认成功的步骤继续）——

/// 造一行权威候选（可指定填报值 —— 值变了载荷摘要就变了）
fn candidate_with_value(row_uid: &str, business_key: &str, value: f64) -> PlanCandidate {
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref: "batch-2026-Q3",
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key,
        numerator: NumericField::Number(value * 10.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(value),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
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

/// 在本机库里落一条「已上报」的提交 + 回执（模拟上一轮真的报上去了）
fn persist_accepted(
    repo: &mut Repo,
    batch_ref: &str,
    row_uid: &str,
    idempotency_key: &str,
    revision: i64,
) {
    let identity = regval::db::repo::SubmissionIdentity {
        batch_ref: batch_ref.to_string(),
        row_uid: row_uid.to_string(),
        idempotency_key: idempotency_key.to_string(),
        payload_digest: format!("digest-{}", row_uid),
        revision,
    };
    repo.upsert_submission(&identity, STATUS_ACCEPTED, "", "", true)
        .unwrap();
    repo.insert_receipt(&regval::db::repo::ReceiptRow {
        receipt_id: format!("RCPT-{}", row_uid),
        idempotency_key: idempotency_key.to_string(),
        revision,
        batch_ref: batch_ref.to_string(),
        row_uid: row_uid.to_string(),
        status: "accepted".to_string(),
        accepted_at: "2026-11-06T09:12:00+08:00".to_string(),
        error_code: String::new(),
        error_field: String::new(),
        message: String::new(),
    })
    .unwrap();
}

#[test]
fn 跨进程续跑_从本机库恢复台账后已上报的行不重推() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let program = program_of(3);

    // 上一轮：第 1 行真的报上去了（回执已落库），第 2 行没推成
    let key1 = program.steps()[0].idempotency_key().to_string();
    persist_accepted(&mut repo, "batch-2026-Q3", "r1", &key1, 1);

    // 新进程：台账是空的 → 先从本机库把「已上报」恢复回来
    let ledger = restore_ledger(&repo, "batch-2026-Q3", program.len()).unwrap();
    assert_eq!(ledger.receipts().len(), 1, "已上报那行的回执要恢复进台账");
    assert_eq!(
        ledger.receipt_of("r1").map(|r| r.receipt_id.clone()),
        Some("RCPT-r1".to_string())
    );

    let plan = plan_resume(&program, &ledger);
    assert_eq!(plan.resume_step, 2, "从第 2 步接着跑（第 1 步已确认成功）");
    assert_eq!(plan.completed, 1);
    assert_eq!(
        plan.pending_row_uids,
        vec!["r2".to_string(), "r3".to_string()]
    );

    // 真跑一遍：已上报的行一个请求都不该再发
    let mut transport = ScriptedTransport::accepting();
    let mut sleeper = RecordingSleeper::new();
    let mut ledger = ledger;
    let policy = RetryPolicy::with_delays(1, 1, 10);
    let report = execute_with_retry(&program, &mut transport, &mut ledger, &policy, &mut sleeper);

    assert_eq!(
        transport.calls,
        vec!["r2".to_string(), "r3".to_string()],
        "已确认成功的行不重复推送"
    );
    assert_eq!(report.resumed_from_step, Some(2));
    assert_eq!(report.run_counts.resume_skipped, 1);
    assert!(report.completed);
}

#[test]
fn 跨进程续跑_订正后重报的行不会被旧修订的回执跳过() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    // 这一行在修订 1 时报上去了（值 96）；后来数据改了（值 75）→ 订正，修订 2 要重报
    let old = candidate_with_value("r1", "BK-1", 96.0);
    let new = candidate_with_value("r1", "BK-1", 75.0);
    let key = old.payload.idempotency_key.clone();
    assert_eq!(key, new.payload.idempotency_key, "改值不改幂等键");

    let mut existing = BTreeMap::new();
    existing.insert(
        key.clone(),
        vec![regval::push::idempotency::ExistingSubmission {
            revision: 1,
            payload_digest: old.payload.payload_digest.clone(),
            status: STATUS_ACCEPTED.to_string(),
        }],
    );
    let program = compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates: vec![new],
        existing,
    });
    assert_eq!(program.steps().len(), 1);
    assert_eq!(
        program.steps()[0].revision(),
        2,
        "载荷变了 → 订正，修订号 +1"
    );

    // 本机库里留着修订 1 的回执（上一版真的报上去了）
    persist_accepted(&mut repo, "batch-2026-Q3", "r1", &key, 1);

    let ledger = restore_ledger(&repo, "batch-2026-Q3", program.len()).unwrap();
    let plan = plan_resume(&program, &ledger);
    assert_eq!(
        plan.resume_step, 1,
        "修订 2 还没报上去 —— 拿修订 1 的回执跳过它等于把订正吞了"
    );
    assert_eq!(plan.pending_row_uids, vec!["r1".to_string()]);
}

#[test]
fn 恢复台账时上一轮的行状态也被读回_不被抹成空白() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    // 上一轮第 2 行退避重试耗尽 → 留在「待重试」
    let identity = regval::db::repo::SubmissionIdentity {
        batch_ref: "batch-2026-Q3".to_string(),
        row_uid: "r2".to_string(),
        idempotency_key: "idem-r2".to_string(),
        payload_digest: "digest-r2".to_string(),
        revision: 1,
    };
    repo.upsert_submission(&identity, STATUS_RETRYABLE, "GATEWAY_TIMEOUT", "", true)
        .unwrap();

    let ledger = restore_ledger(&repo, "batch-2026-Q3", 3).unwrap();
    let status = ledger.row_status("r2").expect("上一轮的行状态要恢复");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_eq!(status.error_code, "GATEWAY_TIMEOUT");
}

#[test]
fn 上次待重试的行会重新编进程序_不会被当成已上报跳过() {
    let c = candidate("r1", "BK-1");
    let key = c.payload.idempotency_key.clone();
    let digest = c.payload.payload_digest.clone();

    // 上次 5xx 没报上去（待重试）→ 这一行还得再推一次，沿用同一修订号与幂等键
    let mut existing = BTreeMap::new();
    existing.insert(
        key.clone(),
        vec![regval::push::idempotency::ExistingSubmission {
            revision: 1,
            payload_digest: digest.clone(),
            status: STATUS_RETRYABLE.to_string(),
        }],
    );
    let p = compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates: vec![c.clone()],
        existing,
    });
    assert_eq!(p.steps().len(), 1, "待重试的行要回到上报程序里");
    assert_eq!(p.steps()[0].revision(), 1, "重试沿用同一修订号");
    assert_eq!(p.steps()[0].idempotency_key(), key);
    assert!(p.skipped().is_empty());

    // 上次**已上报** → 幂等命中，不重复推送（回归）
    let mut existing = BTreeMap::new();
    existing.insert(
        key.clone(),
        vec![regval::push::idempotency::ExistingSubmission {
            revision: 1,
            payload_digest: digest.clone(),
            status: STATUS_ACCEPTED.to_string(),
        }],
    );
    let p = compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates: vec![c.clone()],
        existing,
    });
    assert!(p.is_empty(), "已上报的行不重复推送");
    assert_eq!(p.skipped().len(), 1);

    // 上次**被退回**且载荷没变 → 不重复推送（退回是数据问题，改了数据摘要才变）
    let mut existing = BTreeMap::new();
    existing.insert(
        key.clone(),
        vec![regval::push::idempotency::ExistingSubmission {
            revision: 1,
            payload_digest: digest,
            status: regval::push::status::REJECTED.to_string(),
        }],
    );
    let p = compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates: vec![c],
        existing,
    });
    assert!(p.is_empty(), "载荷没变就不重复推被退回的行");
    assert_eq!(p.skipped().len(), 1);
    assert!(
        p.skipped()[0].detail.contains("退回"),
        "跳过理由要点明上次被退回：{}",
        p.skipped()[0].detail
    );
}
