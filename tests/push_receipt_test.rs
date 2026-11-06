//! 回执解析与行级状态推进的验收测试。
//!
//! 验收点（Task 32）：
//! 1. `200` / `4xx` / `5xx` 三类响应**各自**的行状态正确；
//! 2. **幂等键 ↔ 回执 ID 一一对应**约束成立（撞了就报错，不覆盖）；
//! 3. 退回的错误码与字段名写回原表「问题说明（含依据）」列；
//! 4. 边界：回执缺字段 / `accepted_at` 格式非法 → 该行标「回执待人工核对」，
//!    **不写空状态**、不落空回执。
//!
//! 对端是 [`gateway_stub`] 里的协议替身（协议兼容测试，**不是**最终对接方式）；
//! 院内真实上报网关本轮未实连（地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::collections::BTreeMap;

use gateway_stub::{StubResponse, StubServer};
use regval::db::repo::{Repo, SubmissionIdentity};
use regval::errors::{EngineError, ErrorCode};
use regval::push::client::{
    GatewayClient, GatewayConfig, GatewayOutcomeKind, GatewayRejection, RowOutcome,
};
use regval::push::executor::{
    execute, BatchLedger, FailureClass, StepOutcome, SubmissionTransport, STATUS_ACCEPTED,
    STATUS_INVALID_RECEIPT, STATUS_REJECTED, STATUS_RETRYABLE,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput};
use regval::push::receipt::{
    advance, is_valid_accepted_at, persist, persist_batch, push_issue, SubmitIdentity, Verdict,
    PUSH_ISSUE_RULE_ID,
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

/// 上报身份
fn identity(row_uid: &str, idempotency_key: &str, revision: i64) -> SubmitIdentity {
    SubmissionIdentity {
        batch_ref: "batch-2026-Q3".to_string(),
        row_uid: row_uid.to_string(),
        idempotency_key: idempotency_key.to_string(),
        payload_digest: format!("digest-{}", row_uid),
        revision,
    }
}

/// 受理裁决
fn accepted(receipt_id: &str) -> Verdict {
    Verdict::Accepted {
        receipt_id: receipt_id.to_string(),
        accepted_at: "2026-11-06 09:12:00".to_string(),
        status: "accepted".to_string(),
    }
}

/// 退回裁决
fn rejected(error_code: &str, error_field: &str, message: &str) -> Verdict {
    Verdict::Rejected {
        error_code: error_code.to_string(),
        error_field: error_field.to_string(),
        message: message.to_string(),
    }
}

/// 落库后查这一行的状态
fn status_of(repo: &Repo, row_uid: &str) -> String {
    repo.list_row_push_states("batch-2026-Q3")
        .unwrap()
        .into_iter()
        .find(|s| s.row_uid == row_uid)
        .map(|s| s.status)
        .unwrap_or_else(|| "（库里没有这一行）".to_string())
}

#[test]
fn 二百受理则行状态为已上报且回执落库() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r01", "idem-r01", 1);
    let adv = advance(&id, &accepted("RCPT-001"));
    assert_eq!(adv.status, STATUS_ACCEPTED);
    persist(&mut repo, &id, &adv).unwrap();

    assert_eq!(status_of(&repo, "r01"), STATUS_ACCEPTED);

    let receipts = repo.list_receipts("batch-2026-Q3").unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].receipt_id, "RCPT-001");
    assert_eq!(receipts[0].idempotency_key, "idem-r01");
    assert_eq!(receipts[0].revision, 1);
    assert_eq!(receipts[0].accepted_at, "2026-11-06 09:12:00");

    // 幂等键留在提交行里（续跑靠它）
    let submissions = repo.list_submissions("batch-2026-Q3").unwrap();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].idempotency_key, "idem-r01");
    assert_eq!(submissions[0].status, STATUS_ACCEPTED);
    assert_eq!(submissions[0].attempt_count, 1);
}

#[test]
fn 四百退回则行状态为被退回且错误码与字段名进问题说明列() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r02", "idem-r02", 1);
    let adv = advance(
        &id,
        &rejected(
            "E_REPORTING_PERIOD",
            "reporting_period",
            "报告期格式不合法",
        ),
    );
    assert_eq!(adv.status, STATUS_REJECTED);
    persist(&mut repo, &id, &adv).unwrap();

    assert_eq!(status_of(&repo, "r02"), STATUS_REJECTED);

    // 被退回**没有**回执号 —— 落一条空回执等于凭空造一个上报事实
    assert!(repo.list_receipts("batch-2026-Q3").unwrap().is_empty());

    // 错误码与字段名写回「问题说明（含依据）」列
    let issues = repo.list_issues("batch-2026-Q3").unwrap();
    let push_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.rule_id == PUSH_ISSUE_RULE_ID)
        .collect();
    assert_eq!(push_issues.len(), 1);
    assert!(push_issues[0].message.contains("E_REPORTING_PERIOD"));
    assert!(push_issues[0].message.contains("reporting_period"));
    assert!(push_issues[0].message.contains("报告期格式不合法"));
    assert_eq!(push_issues[0].error_code, "GATEWAY_REJECTED");
    assert_eq!(push_issues[0].missing_columns, vec!["reporting_period".to_string()]);

    // 提交行上留了错误码与字段名
    let submissions = repo.list_submissions("batch-2026-Q3").unwrap();
    assert_eq!(submissions[0].status, STATUS_REJECTED);
    assert_eq!(submissions[0].last_error_code, "GATEWAY_REJECTED");
    assert_eq!(submissions[0].last_error_field, "reporting_period");
}

#[test]
fn 五百则行停在待重试并保留幂等键且不落回执() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r03", "idem-r03", 1);
    let adv = advance(
        &id,
        &Verdict::Retryable {
            error_code: ErrorCode::GatewayServerError,
            message: "上报网关返回服务端错误（HTTP 500）".to_string(),
        },
    );
    assert_eq!(adv.status, STATUS_RETRYABLE);
    persist(&mut repo, &id, &adv).unwrap();

    assert_eq!(status_of(&repo, "r03"), STATUS_RETRYABLE);
    // 回执缺失 → 没有回执行，但幂等键留着，续跑时还认得这一行
    assert!(repo.list_receipts("batch-2026-Q3").unwrap().is_empty());
    let submissions = repo.list_submissions("batch-2026-Q3").unwrap();
    assert_eq!(submissions[0].idempotency_key, "idem-r03");
    assert_eq!(submissions[0].last_error_code, "GATEWAY_SERVER_ERROR");
}

#[test]
fn 回执缺字段或受理时间非法则标回执待人工核对且不写空状态() {
    // 受理时间格式非法
    let bad_time = advance(
        &identity("r04", "idem-r04", 1),
        &Verdict::Accepted {
            receipt_id: "RCPT-004".to_string(),
            accepted_at: "昨天下午".to_string(),
            status: "accepted".to_string(),
        },
    );
    assert_eq!(bad_time.status, STATUS_INVALID_RECEIPT);
    assert!(bad_time.receipt.is_none());

    // 协议层：200 但受理时间格式非法 → 判「回执待人工核对」，不当成功
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-005","accepted_at":"昨天下午","status":"accepted"}"#,
    )]);
    let client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .unwrap();
    let outcome = client.submit(&regval::push::client::SubmissionRequest {
        row_uid: "r05".to_string(),
        payload: SubmissionPayload::new(PayloadInput {
            batch_ref: "batch-2026-Q3",
            org_code: "ORG-001",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: "BK-05",
            numerator: NumericField::Number(980.0),
            denominator: NumericField::Number(1000.0),
            value: NumericField::Number(98.0),
            unit: "%",
            basis: "国卫办医函〔2021〕28号 附件 指标七",
            rule_version: "2021.1",
        }),
    });
    assert!(outcome.is_invalid(), "受理时间非法不得当成成功");

    // 落库：不落空回执，状态写「回执待人工核对」
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();
    let id = identity("r05", "idem-r05", 1);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    persist(&mut repo, &id, &adv).unwrap();
    assert_eq!(status_of(&repo, "r05"), STATUS_INVALID_RECEIPT);
    assert!(repo.list_receipts("batch-2026-Q3").unwrap().is_empty());
}

#[test]
fn 幂等键与回执号一一对应撞了就报错不覆盖() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r06", "idem-r06", 1);
    persist(&mut repo, &id, &advance(&id, &accepted("RCPT-006"))).unwrap();

    // 同一幂等键 + 同一修订号再落一条回执 → 冲突
    let err = persist(&mut repo, &id, &advance(&id, &accepted("RCPT-007"))).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReceiptConflict);
    assert!(err.message.contains("idem-r06"));

    // 同一回执号挂到另一个幂等键上 → 冲突
    let other = identity("r07", "idem-r07", 1);
    let err = persist(&mut repo, &other, &advance(&other, &accepted("RCPT-006"))).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReceiptConflict);
    assert!(err.message.contains("RCPT-006"));

    // 已有回执没被覆盖
    let receipts = repo.list_receipts("batch-2026-Q3").unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].receipt_id, "RCPT-006");
}

#[test]
fn 订正后重报是新修订号且旧回执仍可查() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let first = identity("r08", "idem-r08", 1);
    persist(&mut repo, &first, &advance(&first, &accepted("RCPT-008"))).unwrap();
    // 载荷变了 → 修订号 +1，业务键（幂等键）不变
    let revised = identity("r08", "idem-r08", 2);
    persist(&mut repo, &revised, &advance(&revised, &accepted("RCPT-009"))).unwrap();

    let receipts = repo.list_receipts("batch-2026-Q3").unwrap();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].revision, 1);
    assert_eq!(receipts[0].receipt_id, "RCPT-008");
    assert_eq!(receipts[1].revision, 2);
    assert_eq!(receipts[1].receipt_id, "RCPT-009");

    // 行状态取最新修订号那次
    let states = repo.list_row_push_states("batch-2026-Q3").unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].revision, 2);
    assert_eq!(states[0].status, STATUS_ACCEPTED);
}

#[test]
fn 报上去之后这一行之前退回留下的说明清掉() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r09", "idem-r09", 1);
    persist(
        &mut repo,
        &id,
        &advance(&id, &rejected("E_X", "reporting_period", "报告期格式不合法")),
    )
    .unwrap();
    assert_eq!(
        repo.list_issues("batch-2026-Q3")
            .unwrap()
            .iter()
            .filter(|i| i.rule_id == PUSH_ISSUE_RULE_ID)
            .count(),
        1
    );

    // 修正后重报（修订号 +1）→ 受理 → 退回说明作废
    let revised = identity("r09", "idem-r09", 2);
    persist(&mut repo, &revised, &advance(&revised, &accepted("RCPT-009"))).unwrap();
    assert_eq!(
        repo.list_issues("batch-2026-Q3")
            .unwrap()
            .iter()
            .filter(|i| i.rule_id == PUSH_ISSUE_RULE_ID)
            .count(),
        0
    );
}

#[test]
fn 重跑校验不会抹掉上报退回留下的说明() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let id = identity("r10", "idem-r10", 1);
    persist(
        &mut repo,
        &id,
        &advance(&id, &rejected("E_X", "reporting_period", "报告期格式不合法")),
    )
    .unwrap();

    // 模拟「重新跑一次校验」：口径问题整批重算，但退回说明要留着
    repo.insert_issues("batch-2026-Q3", &[]).unwrap();
    let issues = repo.list_issues("batch-2026-Q3").unwrap();
    assert_eq!(
        issues
            .iter()
            .filter(|i| i.rule_id == PUSH_ISSUE_RULE_ID)
            .count(),
        1,
        "上报退回的说明不能被重跑校验抹掉"
    );
}

/// 假通道：按行键给裁决
struct FakeTransport {
    outcomes: BTreeMap<String, StepOutcome>,
}

impl SubmissionTransport for FakeTransport {
    fn submit(&mut self, step: &regval::push::program::Step) -> Result<StepOutcome, EngineError> {
        Ok(self.outcomes.get(&step.row_uid).cloned().unwrap_or_else(|| {
            StepOutcome::Accepted {
                receipt_id: format!("RCPT-{}", step.row_uid),
                accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
                status: "accepted".to_string(),
            }
        }))
    }
}

#[test]
fn 端到端三类响应各归各的行状态() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    // 三类响应各走一批单步程序（每批一步，没有条件依赖，三类都跑得到）
    let cases: Vec<(&str, &str, StepOutcome)> = vec![
        (
            "r01",
            "BK-01",
            StepOutcome::Accepted {
                receipt_id: "RCPT-r01".to_string(),
                accepted_at: "2026-11-06 10:00:00".to_string(),
                status: "accepted".to_string(),
            },
        ),
        (
            "r02",
            "BK-02",
            StepOutcome::Failed {
                class: FailureClass::NonRetryableRejected,
                error_code: ErrorCode::GatewayRejected,
                error_field: "reporting_period".to_string(),
                message: "上报被退回：错误码 E_REPORTING_PERIOD · 字段 reporting_period".to_string(),
            },
        ),
        (
            "r03",
            "BK-03",
            StepOutcome::Failed {
                class: FailureClass::RetryableServer,
                error_code: ErrorCode::GatewayServerError,
                error_field: String::new(),
                message: "上报网关返回服务端错误（HTTP 500）".to_string(),
            },
        ),
    ];

    let mut persist_report = regval::push::receipt::PersistReport::default();
    for (row_uid, business_key, outcome) in cases {
        let program = compile(&PlanInput {
            batch_ref: "batch-2026-Q3".to_string(),
            candidates: vec![candidate(row_uid, business_key)],
            existing: BTreeMap::new(),
        });
        assert_eq!(program.len(), 1);

        let mut transport = FakeTransport {
            outcomes: BTreeMap::from([(row_uid.to_string(), outcome.clone())]),
        };
        let mut ledger = BatchLedger::new("batch-2026-Q3", program.len());
        let report = execute(&program, &mut transport, &mut ledger);

        let step = program.steps()[0].clone();
        let id = SubmissionIdentity {
            batch_ref: "batch-2026-Q3".to_string(),
            row_uid: step.row_uid.clone(),
            idempotency_key: step.idempotency_key().to_string(),
            payload_digest: step.payload_digest().to_string(),
            revision: step.revision(),
        };
        let adv = advance(&id, &Verdict::from_step_outcome(&report.steps[0].outcome));
        let one = persist_batch(&mut repo, &[(id, adv)]).unwrap();
        persist_report.rows += one.rows;
        persist_report.receipts += one.receipts;
        persist_report.accepted += one.accepted;
        persist_report.rejected += one.rejected;
        persist_report.retryable += one.retryable;
        persist_report.invalid_receipt += one.invalid_receipt;
        persist_report.blocked += one.blocked;
    }

    assert_eq!(persist_report.accepted, 1);
    assert_eq!(persist_report.rejected, 1);
    assert_eq!(persist_report.retryable, 1);
    assert_eq!(persist_report.receipts, 1);

    assert_eq!(status_of(&repo, "r01"), STATUS_ACCEPTED);
    assert_eq!(status_of(&repo, "r02"), STATUS_REJECTED);
    assert_eq!(status_of(&repo, "r03"), STATUS_RETRYABLE);

    // 只有受理那一行有回执
    let receipts = repo.list_receipts("batch-2026-Q3").unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].receipt_id, "RCPT-r01");

    // 退回那一行的错误码与字段名进了问题说明列
    let issues = repo.list_issues("batch-2026-Q3").unwrap();
    let note = issues
        .iter()
        .find(|i| i.rule_id == PUSH_ISSUE_RULE_ID)
        .expect("被退回的行应有说明");
    assert!(note.message.contains("E_REPORTING_PERIOD"));
    assert!(note.message.contains("reporting_period"));
}

#[test]
fn 真实协议走一遍网关替身_受理与退回() {
    // 对端是协议替身：请求路径 / 鉴权头 / 幂等头 / 报文字段全集由 Task 28 的
    // 契约测试守着，这里只关心「回执解析 + 行级状态推进」这一段
    let stub = StubServer::start(vec![
        StubResponse::json(
            200,
            r#"{"receipt_id":"RCPT-100","accepted_at":"2026-11-06 10:05:00","status":"accepted"}"#,
        ),
        StubResponse::json(
            422,
            r#"{"error_code":"E_REPORTING_PERIOD","error_field":"reporting_period","message":"报告期格式不合法"}"#,
        ),
    ]);
    let client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repo::open(&dir.path().join("state.db")).unwrap();

    let mut statuses = Vec::new();
    for (row_uid, key) in [("r11", "idem-r11"), ("r12", "idem-r12")] {
        let business_key = format!("BK-{}", row_uid);
        let request = regval::push::client::SubmissionRequest {
            row_uid: row_uid.to_string(),
            payload: SubmissionPayload::new(PayloadInput {
                batch_ref: "batch-2026-Q3",
                org_code: "ORG-001",
                reporting_period: "2026-Q3",
                indicator_code: "MER-TL-04",
                dept_code: "0301",
                business_key: &business_key,
                numerator: NumericField::Number(980.0),
                denominator: NumericField::Number(1000.0),
                value: NumericField::Number(98.0),
                unit: "%",
                basis: "国卫办医函〔2021〕28号 附件 指标七",
                rule_version: "2021.1",
            }),
        };
        let outcome: RowOutcome = client.submit(&request);
        let id = identity(row_uid, key, 1);
        let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
        persist(&mut repo, &id, &adv).unwrap();
        statuses.push(adv.status);
    }

    assert_eq!(statuses[0], STATUS_ACCEPTED);
    assert_eq!(statuses[1], STATUS_REJECTED);
    assert_eq!(status_of(&repo, "r11"), STATUS_ACCEPTED);
    assert_eq!(status_of(&repo, "r12"), STATUS_REJECTED);
}

#[test]
fn 退回裁决的错误码来自网关而不是引擎() {
    // 协议层的退回裁决：网关给的错误码与字段名要原样带到行上
    let outcome = RowOutcome {
        row_uid: "r13".to_string(),
        kind: GatewayOutcomeKind::Rejected(GatewayRejection {
            http_status: 422,
            error_code: "E_UNIT_MISMATCH".to_string(),
            error_field: "unit".to_string(),
            message: "计量单位应为 %".to_string(),
        }),
    };
    let adv = advance(&identity("r13", "idem-r13", 1), &Verdict::from_row_outcome(&outcome));
    assert_eq!(adv.status, STATUS_REJECTED);
    assert_eq!(adv.error_field, "unit");
    assert_eq!(adv.error_code, "GATEWAY_REJECTED");
    assert!(adv.note.contains("E_UNIT_MISMATCH"));
}

#[test]
fn 受理时间格式两种写法都认() {
    assert!(is_valid_accepted_at("2026-11-06 10:05:00"));
    assert!(is_valid_accepted_at("2026-11-06T10:05:00+08:00"));
    assert!(!is_valid_accepted_at("2026-11-06"));
    assert!(!is_valid_accepted_at("2026-11-06 10:05"));
}

#[test]
fn 退回问题带错误码与字段名落库() {
    let issue = push_issue(
        "r14",
        "idem-r14",
        "上报被退回：错误码 E_X · 字段 reporting_period",
        "GATEWAY_REJECTED",
        "reporting_period",
    );
    assert_eq!(issue.rule_id, PUSH_ISSUE_RULE_ID);
    assert_eq!(issue.issue_key, "idem-r14");
    assert_eq!(issue.error_code, "GATEWAY_REJECTED");
}

#[test]
fn 端到端_退回的错误码与字段名进原表问题说明列() {
    // 走真实 CLI：先校验出契约 → 模拟网关退回并落库 → 再校验一次
    // 「上报状态」与「问题说明（含依据）」两列都要变
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out = dir.path().join("result.json");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mini.csv");

    let run = |out: &std::path::Path| -> serde_json::Value {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
            .args(["validate", "--input"])
            .arg(&fixture)
            .arg("--out")
            .arg(out)
            .arg("--state-db")
            .arg(&state_db)
            .output()
            .expect("regval 应可执行");
        assert!(
            output.status.success(),
            "validate 应成功退出：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let raw = std::fs::read_to_string(out).unwrap();
        serde_json::from_str(&raw).expect("契约 JSON 应可解析")
    };

    // 第一轮：还没上报，状态列全是「未上报」
    let first = run(&out);
    let rows = first["rows"].as_array().expect("rows 应为数组");
    assert!(rows
        .iter()
        .all(|r| r["submission_status"] == "未上报"));
    let batch = first["batch_ref"].as_str().unwrap().to_string();
    let row_uid = rows[0]["row_uid"].as_str().unwrap().to_string();

    // 模拟网关退回：落一条提交（被退回）+ 一条退回说明
    {
        let mut repo = Repo::open(&state_db).unwrap();
        let id = SubmissionIdentity {
            batch_ref: batch.clone(),
            row_uid: row_uid.clone(),
            idempotency_key: "idem-cli-01".to_string(),
            payload_digest: "digest-cli-01".to_string(),
            revision: 1,
        };
        let adv = advance(
            &id,
            &rejected(
                "E_REPORTING_PERIOD",
                "reporting_period",
                "报告期格式不合法",
            ),
        );
        persist(&mut repo, &id, &adv).unwrap();
    }

    // 第二轮：那一行的状态列变「被退回」，问题说明列写明错误码与字段名
    let second = run(&out);
    let rows = second["rows"].as_array().expect("rows 应为数组");
    let target = rows
        .iter()
        .find(|r| r["row_uid"] == row_uid.as_str())
        .expect("应能找到那一行");
    assert_eq!(target["submission_status"], "被退回");
    assert_eq!(target["issue_types"], "上报退回");
    let detail = target["issue_detail"].as_str().unwrap();
    assert!(
        detail.contains("E_REPORTING_PERIOD"),
        "问题说明应带网关给的错误码：{}",
        detail
    );
    assert!(
        detail.contains("reporting_period"),
        "问题说明应带网关点的字段：{}",
        detail
    );
    assert_eq!(target["conclusion"], "不通过");

    // 别的行不受影响
    let others: Vec<&str> = rows
        .iter()
        .filter(|r| r["row_uid"] != row_uid.as_str())
        .map(|r| r["submission_status"].as_str().unwrap())
        .collect();
    assert!(others.iter().all(|s| *s == "未上报"));
}
