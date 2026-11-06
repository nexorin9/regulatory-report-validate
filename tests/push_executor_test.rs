//! 条件批量执行器的验收测试。
//!
//! 验收点（Task 31）：
//! 1. 构造 N 步程序使第 k 步失败 → 第 k+1..N 步**未执行**；
//! 2. 批次汇总**回到批前值**；
//! 3. **已成功行的回执仍在**（行级回执不回滚 —— 网关侧已生效的事实不可撤销）。
//!
//! 三项断言同时通过，才算这一格做完。

mod gateway_stub;

use std::collections::BTreeMap;

use gateway_stub::{StubResponse, StubServer};
use regval::errors::{EngineError, ErrorCode};
use regval::push::client::{GatewayClient, GatewayConfig, OUTBOUND_FIELDS};
use regval::push::executor::{
    execute, BatchLedger, ExecutionReport, FailureClass, StepOutcome, SubmissionTransport,
    STATUS_ACCEPTED, STATUS_IDEMPOTENT_HIT, STATUS_NOT_EXECUTED, STATUS_REJECTED,
    STATUS_RETRYABLE,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput, Program};

/// 造一行权威候选
fn candidate(row_uid: &str, business_key: &str, value: f64) -> PlanCandidate {
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

/// 编一个 N 步程序（每步一行权威行，按行键排序）
fn program_of(values: &[f64]) -> Program {
    let candidates: Vec<PlanCandidate> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            candidate(
                &format!("r{:02}", i + 1),
                &format!("BK-{:02}", i + 1),
                *v,
            )
        })
        .collect();
    compile(&PlanInput {
        batch_ref: "batch-2026-Q3".to_string(),
        candidates,
        existing: BTreeMap::new(),
    })
}

/// 假通道：按行键给裁决，并记录**被调用过**的行（用来断言「没有重复推送」）
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
    fn submit(&mut self, step: &regval::push::program::Step) -> Result<StepOutcome, EngineError> {
        self.called.push(step.row_uid.clone());
        Ok(self.outcomes.get(&step.row_uid).cloned().unwrap_or_else(|| {
            StepOutcome::Accepted {
                receipt_id: format!("RCPT-{}", step.row_uid),
                accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
                status: "accepted".to_string(),
            }
        }))
    }
}

/// 网关受理
fn accepted(row: &str) -> StepOutcome {
    StepOutcome::Accepted {
        receipt_id: format!("RCPT-{}", row),
        accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
        status: "accepted".to_string(),
    }
}

/// 网关退回（4xx）
fn rejected(error_code: &str, error_field: &str) -> StepOutcome {
    StepOutcome::Failed {
        class: FailureClass::NonRetryableRejected,
        error_code: ErrorCode::GatewayRejected,
        error_field: error_field.to_string(),
        message: format!("上报被退回：错误码 {}", error_code),
    }
}

/// 网关 5xx
fn server_error() -> StepOutcome {
    StepOutcome::Failed {
        class: FailureClass::RetryableServer,
        error_code: ErrorCode::GatewayServerError,
        error_field: String::new(),
        message: "上报网关返回服务端错误（HTTP 500）".to_string(),
    }
}

/// 网络超时
fn timeout() -> StepOutcome {
    StepOutcome::Failed {
        class: FailureClass::RetryableTimeout,
        error_code: ErrorCode::GatewayTimeout,
        error_field: String::new(),
        message: "上报网关调用超时".to_string(),
    }
}

// ————————————————————————————————————————————————————————————
// 验收三项：第 k 步失败 → 后续未执行 / 汇总回批前值 / 已成功行回执仍在
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_第k步失败则后续步骤未执行且汇总回批前值且已成功行回执仍在() {
    let program = program_of(&[10.0, 20.0, 30.0, 40.0]);
    assert_eq!(program.len(), 4, "程序应有 4 步");

    let mut transport = FakeTransport::new(vec![("r02", server_error())]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let before = ledger.summary_snapshot();

    let report: ExecutionReport = execute(&program, &mut transport, &mut ledger);

    // ① 第 k+1..N 步未执行：只有 r01、r02 两步真的发了请求
    assert_eq!(
        transport.called,
        vec!["r01".to_string(), "r02".to_string()],
        "第 3、4 步不该发请求"
    );
    assert!(report.steps[0].executed, "第 1 步应已执行");
    assert!(report.steps[1].executed, "第 2 步应已执行");
    assert!(!report.steps[2].executed, "第 3 步应未执行");
    assert!(!report.steps[3].executed, "第 4 步应未执行");
    assert_eq!(report.failed_step, Some(2));
    assert_eq!(report.run_counts.not_executed, 2, "本轮计数记着两行没执行");

    // ② 批次汇总回到批前值（计数与汇总值都回到批前状态）
    assert!(report.rollback_applied, "批末条件回滚应已触发");
    assert_eq!(
        report.summary_after, before,
        "批次汇总（计数与汇总值）应回到批前状态"
    );
    assert_eq!(ledger.summary(), &before);
    // 本轮计数不受回滚影响 —— 这一轮发生了什么不能被抹掉
    assert_eq!(report.run_counts.accepted, 1);
    assert_eq!(report.run_counts.retryable, 1);
    assert_eq!(report.run_counts.not_executed, 2);
    assert_eq!(report.run_counts.attempted, 2);

    // ③ 已成功行的回执仍在（行级回执不回滚）
    assert_eq!(ledger.receipts().len(), 1, "第 1 行的回执应保留");
    let receipt = ledger.receipt_of("r01").expect("第 1 行应有回执");
    assert_eq!(receipt.receipt_id, "RCPT-r01");
    assert_eq!(receipt.revision, 1);
    assert!(!receipt.idempotency_key.is_empty());
    assert!(!receipt.payload_digest.is_empty());
    assert_eq!(report.receipts.len(), 1);
}

#[test]
fn 验收_失败分类_可重试与不可重试分得开() {
    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut transport = FakeTransport::new(vec![("r01", timeout())]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report = execute(&program, &mut transport, &mut ledger);

    let outcome = &report.steps[0].outcome;
    assert_eq!(outcome.failure_class(), Some(FailureClass::RetryableTimeout));
    assert!(outcome.failure_class().unwrap().is_retryable());
    assert_eq!(outcome.error_code(), Some(ErrorCode::GatewayTimeout));

    let status = ledger.row_status("r01").expect("失败行要有状态");
    assert_eq!(status.status, STATUS_RETRYABLE);
    assert_eq!(status.error_code, "GATEWAY_TIMEOUT");
}

#[test]
fn 验收_网关退回不可重试且错误码与字段名进该行状态() {
    let program = program_of(&[10.0, 20.0]);
    let mut transport = FakeTransport::new(vec![(
        "r01",
        rejected("E_REPORTING_PERIOD", "reporting_period"),
    )]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report = execute(&program, &mut transport, &mut ledger);

    assert_eq!(
        report.steps[0].outcome.failure_class(),
        Some(FailureClass::NonRetryableRejected)
    );
    assert!(!report.steps[0]
        .outcome
        .failure_class()
        .unwrap()
        .is_retryable());
    let status = ledger.row_status("r01").expect("被退回的行要有状态");
    assert_eq!(status.status, STATUS_REJECTED);
    assert_eq!(status.error_code, "GATEWAY_REJECTED");
    assert_eq!(status.error_field, "reporting_period");
    assert_eq!(report.run_counts.rejected, 1);
    assert_eq!(report.run_counts.retryable, 0);
}

#[test]
fn 验收_失败不静默_审计链留痕且错误码可查() {
    let program = program_of(&[10.0, 20.0]);
    let mut transport = FakeTransport::new(vec![("r01", server_error())]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    execute(&program, &mut transport, &mut ledger);

    let audit = ledger.audit_for("r01");
    assert!(!audit.is_empty(), "失败行要有审计条目");
    assert_eq!(audit[0].event, "push.step.failed");
    assert_eq!(audit[0].error_code, "GATEWAY_SERVER_ERROR");
    assert_eq!(audit[0].batch_ref, "batch-2026-Q3");

    // 未执行的行也要留痕（不是静默跳过）
    let not_executed = ledger.audit_for("r02");
    assert!(!not_executed.is_empty());
    assert_eq!(not_executed[0].event, "push.step.not_executed");
    assert_eq!(
        ledger.row_status("r02").map(|s| s.status.as_str()),
        Some(STATUS_NOT_EXECUTED)
    );

    // 批末回滚也有审计条目
    assert!(ledger
        .audit()
        .iter()
        .any(|a| a.event == "push.batch.rollback"));
}

#[test]
fn 验收_续跑不重复推送已成功的行() {
    let program = program_of(&[10.0, 20.0, 30.0]);

    // 第一轮：第 2 步 5xx 失败
    let mut transport = FakeTransport::new(vec![("r02", server_error())]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    execute(&program, &mut transport, &mut ledger);
    assert_eq!(transport.called.len(), 2);

    // 第二轮：通道全部成功 —— 第 1 行应命中幂等（不重推），从第 2 步继续
    let mut transport2 = FakeTransport::new(vec![]);
    let report2 = execute(&program, &mut transport2, &mut ledger);
    assert_eq!(
        transport2.called,
        vec!["r02".to_string(), "r03".to_string()],
        "已成功的第 1 行不该再发一次"
    );
    assert_eq!(report2.summary_after.idempotent_hit, 1);
    assert_eq!(report2.summary_after.accepted, 2);
    assert_eq!(ledger.receipts().len(), 3, "三行都该有回执");
    assert_eq!(
        ledger.row_status("r01").map(|s| s.status.as_str()),
        Some(STATUS_IDEMPOTENT_HIT)
    );
}

#[test]
fn 验收_全部成功时不回滚且汇总与行状态计数一致() {
    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut transport = FakeTransport::new(vec![
        ("r01", accepted("r01")),
        ("r02", accepted("r02")),
        ("r03", accepted("r03")),
    ]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report = execute(&program, &mut transport, &mut ledger);

    assert!(report.completed);
    assert!(!report.rollback_applied);
    assert_eq!(report.summary_after.accepted, 3);
    assert_eq!(report.summary_after.reported(), 3);
    assert_eq!(report.summary_after.attempted, 3);
    assert_eq!(report.summary_after.not_executed, 0);
    // 汇总值：已上报行的填报值合计
    assert!((report.summary_after.sum_value - 60.0).abs() < 1e-9);
    assert!((report.summary_after.sum_numerator - 600.0).abs() < 1e-9);
    assert!((report.summary_after.sum_denominator - 3000.0).abs() < 1e-9);
    // 汇总计数与台账里的行状态计数一致
    let accepted_rows = ledger
        .row_statuses()
        .iter()
        .filter(|s| s.status == STATUS_ACCEPTED)
        .count();
    assert_eq!(accepted_rows, report.summary_after.accepted);
}

#[test]
fn 验收_空程序不执行也不回滚() {
    let program = program_of(&[]);
    let mut transport = FakeTransport::new(vec![]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", 0);
    let report = execute(&program, &mut transport, &mut ledger);
    assert!(transport.called.is_empty());
    assert!(!report.rollback_applied);
    assert!(report.completed);
    assert!(ledger.receipts().is_empty());
}

#[test]
fn 验收_末步是幂等命中时不算失败也不回滚() {
    let program = program_of(&[10.0, 20.0]);
    let mut transport = FakeTransport::new(vec![]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    // 先造一条已有回执（模拟上一轮跑过第一行）
    execute(&program, &mut transport, &mut ledger);

    // 清掉通道记录，重新跑：两行都命中幂等
    transport.called.clear();
    let report = execute(&program, &mut transport, &mut ledger);
    assert!(transport.called.is_empty(), "两行都不该再发请求");
    assert_eq!(report.summary_after.idempotent_hit, 2);
    assert!(!report.rollback_applied, "幂等命中不是失败，不该回滚");
    assert!(report.completed);
}

#[test]
fn 验收_执行报告是可核对产物_含逐步结果与回执() {
    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut transport = FakeTransport::new(vec![("r02", server_error())]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report = execute(&program, &mut transport, &mut ledger);

    let raw = report.to_json();
    let v: serde_json::Value = serde_json::from_str(&raw).expect("执行报告应是合法 JSON");
    assert_eq!(
        v["schema_version"],
        "regval.execution/1.0.0",
        "执行产物也要带版本号"
    );
    assert_eq!(v["batch_ref"], "batch-2026-Q3");
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);
    assert_eq!(v["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(v["rollback_applied"], serde_json::json!(true));
    assert_eq!(v["failed_step"], serde_json::json!(2));
    // 失败步骤带机器可读错误码与可重试标记
    assert_eq!(v["steps"][1]["outcome"]["error_code"], "GATEWAY_SERVER_ERROR");
    assert_eq!(v["steps"][1]["outcome"]["retryable"], serde_json::json!(true));
    // 未执行的步骤标记出来
    assert_eq!(v["steps"][2]["executed"], serde_json::json!(false));
    // 批前批后汇总都在产物里，可核对
    assert_eq!(v["summary_before"]["accepted"], serde_json::json!(0));
    assert_eq!(v["summary_after"]["accepted"], serde_json::json!(0));
}

#[test]
fn 验收_同一份输入两次执行产出同样的报告() {
    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut t1 = FakeTransport::new(vec![("r02", server_error())]);
    let mut l1 = BatchLedger::new("batch-2026-Q3", program.len());
    let r1 = execute(&program, &mut t1, &mut l1);

    let mut t2 = FakeTransport::new(vec![("r02", server_error())]);
    let mut l2 = BatchLedger::new("batch-2026-Q3", program.len());
    let r2 = execute(&program, &mut t2, &mut l2);

    assert_eq!(r1.summary_after, r2.summary_after);
    assert_eq!(r1.rollback_applied, r2.rollback_applied);
    assert_eq!(r1.failed_step, r2.failed_step);
    assert_eq!(r1.receipts, r2.receipts);
}

// ————————————————————————————————————————————————————————————
// 端到端：执行器 + **真实出站客户端**（协议替身只在对端）
// ————————————————————————————————————————————————————————————

#[test]
fn 端到端_真实出站客户端被执行器按条件链驱动_网关退回时后续步骤不执行() {
    // 对端依次回：200 受理 → 4xx 退回 → （本不该有第三次请求）
    let stub = StubServer::start(vec![
        StubResponse::json(
            200,
            r#"{"receipt_id":"RCPT-E2E-01","accepted_at":"2026-11-06T10:00:00+08:00","status":"accepted"}"#,
        ),
        StubResponse::json(
            422,
            r#"{"error_code":"E_REPORTING_PERIOD","error_field":"reporting_period","message":"报告期格式不合法"}"#,
        ),
    ]);
    let mut client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .expect("客户端应可建");

    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let before = ledger.summary_snapshot();

    let report = execute(&program, &mut client, &mut ledger);

    // 只发了两次请求：第 3 步因第 2 步被退回而未执行
    let first = stub.next_request();
    assert_eq!(first.method, "POST");
    assert_eq!(first.path, "/api/v1/regulatory-submissions");
    assert_eq!(first.header("authorization"), Some("Bearer stub-token"));
    assert!(first.header("idempotency-key").is_some());
    // 报文走的是正式规范：字段全集 15 项，幂等键与头同值
    let body = first.body_json();
    let mut fields: Vec<String> = body.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    let mut expected: Vec<String> = OUTBOUND_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(fields, expected, "出站报文字段全集应固定 15 项");
    assert_eq!(
        body["idempotency_key"],
        first.header("idempotency-key").unwrap()
    );
    let second = stub.next_request();
    assert!(second.header("idempotency-key").is_some());
    assert!(stub.try_next_request(300).is_none(), "第 3 步不该发请求");

    // 第 1 行受理 → 行级回执保留；第 2 行被退回 → 错误码与字段名进该行状态
    assert_eq!(ledger.receipts().len(), 1);
    assert_eq!(ledger.receipt_of("r01").unwrap().receipt_id, "RCPT-E2E-01");
    let rejected_status = ledger.row_status("r02").expect("被退回的行要有状态");
    assert_eq!(rejected_status.status, STATUS_REJECTED);
    assert_eq!(rejected_status.error_code, "GATEWAY_REJECTED");
    assert_eq!(rejected_status.error_field, "reporting_period");

    // 批末条件回滚：批次汇总回到批前值
    assert!(report.rollback_applied);
    assert_eq!(report.summary_after, before);
    assert_eq!(report.run_counts.accepted, 1);
    assert_eq!(report.run_counts.rejected, 1);
    assert_eq!(report.run_counts.not_executed, 1);
}

#[test]
fn 端到端_网关超时归可重试且不会被改写成已上报() {
    // 替身回包前睡 2500ms，客户端超时 1s → 超时
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-LATE","accepted_at":"2026-11-06T10:00:00+08:00","status":"accepted"}"#,
    )
    .with_delay(2500)]);
    let mut client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 1,
    })
    .expect("客户端应可建");

    let program = program_of(&[10.0]);
    let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
    let report = execute(&program, &mut client, &mut ledger);

    // 超时 ≠ 已上报：分类为可重试，行状态是「待重试」，没有回执
    assert_eq!(
        report.steps[0].outcome.failure_class(),
        Some(FailureClass::RetryableTimeout)
    );
    assert_eq!(
        ledger.row_status("r01").map(|s| s.status.as_str()),
        Some(STATUS_RETRYABLE)
    );
    assert!(ledger.receipts().is_empty(), "超时不产生回执");
    assert!(!report.completed);
    assert!(report.rollback_applied);
}
