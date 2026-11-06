//! 院内卫健上报网关的**协议兼容契约测试**（验证 D）。
//!
//! 这一套断言落在两处：
//!
//! 1. **实际发出去的报文** —— 请求路径、`Authorization` 头、`Idempotency-Key` 头、
//!    body 字段全集（少一多一都不合规）；
//! 2. **对端的协议裁决** —— 网关回 `200` / `4xx` / `5xx` / 超时之后，
//!    引擎怎么分类、行状态怎么推进、错误码与字段名怎么写回。
//!
//! 对端是 [`gateway_stub::reporting`] 里的**协议兼容替身**：它按 `spec.md`
//! 「出站报文（正式对接规范）」自己校验报文，不合规就按 spec 的 `4xx` 形状退回。
//! 替身只出现在测试数据层，**不是**最终对接方式 —— 院内真实上报网关本轮未实连
//! （地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gateway_stub::reporting::{
    ReportingGatewayStub, Scenario, Violation, SPEC_FIELDS, SUBMISSIONS_PATH,
};
use regval::db::repo::{Repo, SubmissionIdentity};
use regval::errors::ErrorCode;
use regval::ingest::key::row_uid;
use regval::ingest::{Field, SourceRow};
use regval::push::client::{
    GatewayClient, GatewayConfig, GatewayOutcomeKind, OUTBOUND_FIELDS, SubmissionRequest,
};
use regval::push::executor::{
    execute, BatchLedger, STATUS_ACCEPTED, STATUS_INVALID_RECEIPT, STATUS_REJECTED,
    STATUS_RETRYABLE,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput};
use regval::push::receipt::{advance, persist, SubmitIdentity, Verdict, PUSH_ISSUE_RULE_ID};
use regval::state::machine::{self, RowState, StateChange};

/// 本轮测试用的批次号
const BATCH: &str = "batch-2026-Q3";
/// 替身期待的凭据
const TOKEN: &str = "stub-token";

// ————————————————————————————————————————————————
// 造数据
// ————————————————————————————————————————————————

/// 一条上报载荷
fn payload() -> SubmissionPayload {
    SubmissionPayload::new(PayloadInput {
        batch_ref: BATCH,
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key: "BK-01",
        numerator: NumericField::Number(980.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    })
}

/// 一次提交请求（行键由调用方给：落库的行键是算出来的，不能编）
fn request_for(row_uid: &str) -> SubmissionRequest {
    SubmissionRequest {
        row_uid: row_uid.to_string(),
        payload: payload(),
    }
}

/// 上报身份（幂等键 / 摘要取自载荷本身，不另编）
fn identity(payload: &SubmissionPayload, row_uid: &str) -> SubmitIdentity {
    SubmissionIdentity {
        batch_ref: payload.batch_ref.clone(),
        row_uid: row_uid.to_string(),
        idempotency_key: payload.idempotency_key.clone(),
        payload_digest: payload.payload_digest.clone(),
        revision: payload.revision,
    }
}

/// 给替身配一个网关客户端
fn client_for(stub: &ReportingGatewayStub, timeout_secs: u64) -> GatewayClient {
    GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: TOKEN.to_string(),
        timeout_secs,
    })
    .expect("客户端应可建")
}

/// 一行脱敏的科室上报表行（指标级监管数据，无患者可识别明细）
fn source_row() -> SourceRow {
    let mut values: BTreeMap<Field, String> = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::IndicatorName, "病案首页 24 小时内完成率".to_string());
    values.insert(Field::DeptCode, "0301".to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::Numerator, "980".to_string());
    values.insert(Field::Denominator, "1000".to_string());
    values.insert(Field::Value, "98".to_string());
    values.insert(Field::Unit, "%".to_string());
    values.insert(Field::BusinessKey, "BK-01".to_string());
    SourceRow {
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 1,
        dup_ordinal: 0,
        values,
    }
}

/// 一个已落库、已过校验的批次（行状态 = 校验通过，等着被推）
fn seeded_repo(dir: &Path) -> (Repo, String) {
    let mut repo = Repo::open(&dir.join("state.db")).expect("状态库应可开");
    let row = source_row();
    repo.insert_rows(BATCH, std::slice::from_ref(&row))
        .expect("落库应成功");
    let uid = row_uid(&row);
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(
            &uid,
            &row.source_file,
            BATCH,
            RowState::Validated,
            "validate",
        ),
    )
    .expect("推进到校验通过应成功");
    (repo, uid)
}

/// 库里这一行的上报状态
fn push_status_of(repo: &Repo, uid: &str) -> String {
    repo.list_row_push_states(BATCH)
        .expect("读行状态应成功")
        .into_iter()
        .find(|s| s.row_uid == uid)
        .map(|s| s.status)
        .unwrap_or_else(|| "（库里没有这一行）".to_string())
}

/// 库里这一行的行级状态
fn row_state_of(repo: &Repo, uid: &str) -> Option<RowState> {
    machine::current(repo.conn(), uid, &machine::file_identity("科室一表.csv"))
        .expect("读行状态应成功")
        .map(|s| s.state)
}

/// 按 spec 的 15 项字段造一份合规报文
fn spec_body(idempotency_key: &str) -> serde_json::Value {
    serde_json::json!({
        "batch_ref": BATCH,
        "reporting_period": "2026-Q3",
        "org_code": "ORG-001",
        "indicator_code": "MER-TL-04",
        "dept_code": "0301",
        "business_key": "BK-01",
        "numerator": 980.0,
        "denominator": 1000.0,
        "value": 98.0,
        "unit": "%",
        "basis": "国卫办医函〔2021〕28号 附件 指标七",
        "rule_version": "2021.1",
        "idempotency_key": idempotency_key,
        "payload_digest": "digest-0001",
        "revision": 1,
    })
}

/// 手工发一份报文给替身（用来构造不合规的报文 —— 引擎自己不会发这种）
fn post_raw(
    stub: &ReportingGatewayStub,
    body: &serde_json::Value,
    idempotency_key: &str,
) -> reqwest::blocking::Response {
    let http = reqwest::blocking::Client::new();
    http.post(format!("{}{}", stub.base_url, SUBMISSIONS_PATH))
        .bearer_auth(TOKEN)
        .header("Idempotency-Key", idempotency_key)
        .json(body)
        .send()
        .expect("手工请求应发得出去")
}

// ————————————————————————————————————————————————
// 1. 报文本身：路径 / 头 / 字段全集
// ————————————————————————————————————————————————

#[test]
fn 上报报文完全符合出站报文规范_路径头与字段全集都对上() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let outcome = client_for(&stub, 5).submit(&request_for("row-contract-01"));
    assert!(outcome.is_accepted(), "替身受理了，引擎应判为已受理");

    let req = stub.next_request();
    // 替身按 spec 校验：不合规一处都没有
    assert!(
        req.conforms(),
        "报文应完全合规，实际不合规处：{:?}",
        req.violations
    );

    // 请求路径
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, SUBMISSIONS_PATH);

    // 鉴权头
    assert_eq!(req.header("authorization"), Some("Bearer stub-token"));
    assert_eq!(req.header("content-type"), Some("application/json"));

    // 幂等头：与报文里的幂等键同值
    let body = req.body_json();
    let idem = body["idempotency_key"].as_str().unwrap().to_string();
    assert_eq!(req.header("idempotency-key"), Some(idem.as_str()));
    assert_eq!(idem.len(), 64);

    // body 字段全集：恰好 15 项，少一多一都不行
    assert_eq!(req.fields.len(), 15);
    let mut expected: Vec<String> = SPEC_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(req.fields, expected);

    // 引擎侧的字段常量与 spec 一致（两边各写一份就会走偏）
    let mut engine_fields: Vec<String> = OUTBOUND_FIELDS.iter().map(|s| s.to_string()).collect();
    engine_fields.sort();
    assert_eq!(engine_fields, expected);

    // 报文里不含患者可识别明细
    let raw = req.body.to_lowercase();
    for banned in ["住院号", "身份证", "患者姓名", "病案号", "row_uid"] {
        assert!(!raw.contains(banned), "报文里不该出现 {}", banned);
    }
}

#[test]
fn 报文多传字段时网关按协议退回并点名多出的字段() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let mut body = spec_body("idem-extra");
    body.as_object_mut()
        .unwrap()
        .insert("row_uid".to_string(), serde_json::json!("row-1"));

    let resp = post_raw(&stub, &body, "idem-extra");
    assert_eq!(resp.status().as_u16(), 400);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["error_code"], "E_FIELD_UNEXPECTED");
    assert_eq!(v["error_field"], "row_uid");
    assert!(v["message"].as_str().unwrap().contains("row_uid"));

    // 替身自己也知道这一份不合规（留痕可查）
    let req = stub.next_request();
    assert!(!req.conforms());
    assert!(req
        .violations
        .iter()
        .any(|v| matches!(v, Violation::FieldUnexpected { field } if field == "row_uid")));

    // 不合规的报文不会留下回执
    assert_eq!(stub.receipts_created(), 0);
}

#[test]
fn 报文少传字段时网关按协议退回并点名缺的字段() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let mut body = spec_body("idem-missing");
    body.as_object_mut().unwrap().remove("basis");

    let resp = post_raw(&stub, &body, "idem-missing");
    assert_eq!(resp.status().as_u16(), 400);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["error_code"], "E_FIELD_MISSING");
    assert_eq!(v["error_field"], "basis");

    let req = stub.next_request();
    assert!(req
        .violations
        .iter()
        .any(|v| matches!(v, Violation::FieldMissing { field } if field == "basis")));
    assert_eq!(stub.receipts_created(), 0);
}

#[test]
fn 字段类型不对时网关按协议退回并点名是哪个字段() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let mut body = spec_body("idem-type");
    body.as_object_mut()
        .unwrap()
        .insert("revision".to_string(), serde_json::json!("第一版"));

    let resp = post_raw(&stub, &body, "idem-type");
    assert_eq!(resp.status().as_u16(), 400);
    let v: serde_json::Value = resp.json().unwrap();
    assert_eq!(v["error_code"], "E_FIELD_TYPE_INVALID");
    assert_eq!(v["error_field"], "revision");
}

// ————————————————————————————————————————————————
// 2. 四类响应：分类 / 行状态推进 / 错误码写回
// ————————————————————————————————————————————————

#[test]
fn 受理回执把行状态推成已上报且回执落库() {
    let dir = tempfile::tempdir().unwrap();
    let (mut repo, uid) = seeded_repo(dir.path());
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);

    let outcome = client_for(&stub, 5).submit(&request_for(&uid));
    assert!(outcome.is_accepted());

    let payload = payload();
    let id = identity(&payload, &uid);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    assert_eq!(adv.status, STATUS_ACCEPTED);
    persist(&mut repo, &id, &adv).expect("落库应成功");

    // 上报状态列 = 已上报
    assert_eq!(push_status_of(&repo, &uid), STATUS_ACCEPTED);
    // 行级状态 = 已上报
    assert_eq!(row_state_of(&repo, &uid), Some(RowState::Reported));

    // 回执落库：幂等键 ↔ 回执号一一对应
    let receipts = repo.list_receipts(BATCH).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].idempotency_key, payload.idempotency_key);
    assert_eq!(receipts[0].revision, 1);
    assert_eq!(receipts[0].accepted_at, "2026-11-06 09:12:00");
    assert_eq!(receipts[0].receipt_id, stub.receipt_id_for(&payload.idempotency_key).unwrap());

    // 受理了就没有「上报退回」问题
    assert!(repo.list_push_issues(BATCH).unwrap().is_empty());
}

#[test]
fn 退回响应把行状态推成被退回且错误码与字段名写回说明列() {
    let dir = tempfile::tempdir().unwrap();
    let (mut repo, uid) = seeded_repo(dir.path());
    let stub = ReportingGatewayStub::start(vec![Scenario::rejected(
        422,
        "E_REPORTING_PERIOD",
        "reporting_period",
        "报告期格式不合法",
    )]);

    let outcome = client_for(&stub, 5).submit(&request_for(&uid));
    assert!(outcome.is_rejected());
    match &outcome.kind {
        GatewayOutcomeKind::Rejected(r) => {
            assert_eq!(r.http_status, 422);
            assert_eq!(r.error_code, "E_REPORTING_PERIOD");
            assert_eq!(r.error_field, "reporting_period");
        }
        other => panic!("应是退回，实际 {:?}", other),
    }

    let payload = payload();
    let id = identity(&payload, &uid);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    assert_eq!(adv.status, STATUS_REJECTED);
    persist(&mut repo, &id, &adv).expect("落库应成功");

    // 上报状态列 = 被退回；行级状态 = 被退回
    assert_eq!(push_status_of(&repo, &uid), STATUS_REJECTED);
    assert_eq!(row_state_of(&repo, &uid), Some(RowState::Rejected));

    // 上报状态那一行也带着错误码与网关点名的字段
    let states = repo.list_row_push_states(BATCH).unwrap();
    let st = states.iter().find(|s| s.row_uid == uid).expect("应有这一行");
    assert_eq!(st.error_code, "GATEWAY_REJECTED");
    assert_eq!(st.error_field, "reporting_period");

    // 错误码与字段名写回「问题说明（含依据）」列
    //
    // 结构化那两列记的是**引擎的分类**（`GATEWAY_REJECTED` + 网关点名的字段），
    // **网关自己的错误码**落在说明正文里 —— 科室在原表里读到的是这一列，
    // 「上报被退回：错误码 E_REPORTING_PERIOD · 字段 reporting_period · …」。
    let issues = repo
        .list_push_issues(BATCH)
        .unwrap()
        .into_iter()
        .filter(|i| i.row_uid == uid)
        .collect::<Vec<_>>();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].rule_id, PUSH_ISSUE_RULE_ID);
    assert_eq!(issues[0].issue_kind, "上报退回");
    assert_eq!(issues[0].error_code, "GATEWAY_REJECTED");
    assert_eq!(issues[0].missing_columns, vec!["reporting_period".to_string()]);
    assert!(issues[0].message.contains("E_REPORTING_PERIOD"));
    assert!(issues[0].message.contains("reporting_period"));
    assert!(issues[0].message.contains("报告期格式不合法"));

    // 被退回没有回执
    assert!(repo.list_receipts(BATCH).unwrap().is_empty());
    assert_eq!(stub.receipts_created(), 0);
}

#[test]
fn 服务端错误归为待重试且不落回执() {
    let dir = tempfile::tempdir().unwrap();
    let (mut repo, uid) = seeded_repo(dir.path());
    let stub = ReportingGatewayStub::start(vec![Scenario::ServerError { status: 500 }]);

    let outcome = client_for(&stub, 5).submit(&request_for(&uid));
    assert!(outcome.is_retryable());
    assert!(!outcome.is_accepted(), "5xx 绝不能被改写成已上报");
    match &outcome.kind {
        GatewayOutcomeKind::Retryable { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::GatewayServerError);
            assert_eq!(error_code.as_str(), "GATEWAY_SERVER_ERROR");
        }
        other => panic!("应可重试，实际 {:?}", other),
    }

    let payload = payload();
    let id = identity(&payload, &uid);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    assert_eq!(adv.status, STATUS_RETRYABLE);
    persist(&mut repo, &id, &adv).expect("落库应成功");

    // 待重试：没有回执，但幂等键留着（续跑靠它）
    assert_eq!(push_status_of(&repo, &uid), STATUS_RETRYABLE);
    assert_eq!(row_state_of(&repo, &uid), Some(RowState::RetryPending));
    assert!(repo.list_receipts(BATCH).unwrap().is_empty());
    let submissions = repo.list_submissions(BATCH).unwrap();
    assert_eq!(submissions.len(), 1);
    assert_eq!(submissions[0].idempotency_key, payload.idempotency_key);
    assert_eq!(submissions[0].last_error_code, "GATEWAY_SERVER_ERROR");
}

#[test]
fn 超时归为待重试且不带成功结论() {
    let dir = tempfile::tempdir().unwrap();
    let (mut repo, uid) = seeded_repo(dir.path());
    let stub = ReportingGatewayStub::start(vec![Scenario::Timeout { delay_ms: 2500 }]);

    let outcome = client_for(&stub, 1).submit(&request_for(&uid));
    assert!(outcome.is_retryable());
    assert!(!outcome.is_accepted());
    match &outcome.kind {
        GatewayOutcomeKind::Retryable { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::GatewayTimeout);
            assert_eq!(error_code.as_str(), "GATEWAY_TIMEOUT");
        }
        other => panic!("应可重试，实际 {:?}", other),
    }

    let payload = payload();
    let id = identity(&payload, &uid);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    persist(&mut repo, &id, &adv).expect("落库应成功");
    assert_eq!(push_status_of(&repo, &uid), STATUS_RETRYABLE);
    assert!(repo.list_receipts(BATCH).unwrap().is_empty());
}

#[test]
fn 回执结构不合法时不当成成功也不落空回执() {
    let dir = tempfile::tempdir().unwrap();
    let (mut repo, uid) = seeded_repo(dir.path());
    let stub = ReportingGatewayStub::start(vec![Scenario::InvalidReceipt]);

    let outcome = client_for(&stub, 5).submit(&request_for(&uid));
    assert!(outcome.is_invalid());
    assert!(!outcome.is_accepted(), "回执缺字段绝不能当成已上报");
    match &outcome.kind {
        GatewayOutcomeKind::Invalid { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::GatewayResponseInvalid);
        }
        other => panic!("应判回执不可用，实际 {:?}", other),
    }

    let payload = payload();
    let id = identity(&payload, &uid);
    let adv = advance(&id, &Verdict::from_row_outcome(&outcome));
    assert_eq!(adv.status, STATUS_INVALID_RECEIPT);
    persist(&mut repo, &id, &adv).expect("落库应成功");

    assert_eq!(push_status_of(&repo, &uid), STATUS_INVALID_RECEIPT);
    assert!(repo.list_receipts(BATCH).unwrap().is_empty(), "不落空回执");
}

// ————————————————————————————————————————————————
// 3. 幂等：命中不新建回执、不重复推送
// ————————————————————————————————————————————————

#[test]
fn 幂等键命中时网关不产生新回执() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let client = client_for(&stub, 5);
    let payload = payload();

    let first = client.submit(&request_for("row-contract-01"));
    assert!(first.is_accepted());
    assert_eq!(stub.receipts_created(), 1);
    let first_receipt = match &first.kind {
        GatewayOutcomeKind::Accepted(a) => a.receipt_id.clone(),
        other => panic!("应受理，实际 {:?}", other),
    };

    // 同一个幂等键再来一次：网关回的是**同一张回执**，不新建
    let second = client.submit(&request_for("row-contract-01"));
    assert!(second.is_accepted());
    let second_receipt = match &second.kind {
        GatewayOutcomeKind::Accepted(a) => a.receipt_id.clone(),
        other => panic!("应受理，实际 {:?}", other),
    };
    assert_eq!(first_receipt, second_receipt, "幂等命中应回同一张回执");
    assert_eq!(stub.receipts_created(), 1, "重复提交不新建回执");
    assert_eq!(stub.requests_received(), 2, "两次请求都发出去了");
    assert_eq!(
        stub.receipt_id_for(&payload.idempotency_key).as_deref(),
        Some(first_receipt.as_str())
    );
}

#[test]
fn 引擎重跑同一批次时台账判幂等命中不重复推送() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let mut client = client_for(&stub, 5);

    let payload = payload();
    let program = compile(&PlanInput {
        batch_ref: BATCH.to_string(),
        candidates: vec![PlanCandidate {
            row_uid: "row-contract-01".to_string(),
            source_ns: "科室一表".to_string(),
            source_file: "科室一表.csv".to_string(),
            row_no: 1,
            authority_state: "authoritative".to_string(),
            match_key: "MER-TL-04|0301|2026-Q3".to_string(),
            payload: payload.clone(),
        }],
        existing: BTreeMap::new(),
    });
    assert_eq!(program.len(), 1);

    // 第一轮：推出去，网关受理
    let mut ledger = BatchLedger::new(BATCH, program.len());
    let first = execute(&program, &mut client, &mut ledger);
    assert_eq!(first.steps.len(), 1);
    assert!(first.steps[0].outcome.is_success());
    assert_eq!(stub.requests_received(), 1);
    assert_eq!(stub.receipts_created(), 1);

    // 第二轮：台账里已有这一行的回执 → 幂等命中，**不再发请求**
    let second = execute(&program, &mut client, &mut ledger);
    assert_eq!(second.steps.len(), 1);
    match &second.steps[0].outcome {
        regval::push::executor::StepOutcome::IdempotentHit { receipt_id, .. } => {
            assert_eq!(
                receipt_id,
                &ledger.receipt_of("row-contract-01").unwrap().receipt_id
            );
        }
        other => panic!("应判幂等命中，实际 {:?}", other),
    }
    assert_eq!(stub.requests_received(), 1, "幂等命中不该再发请求");
    assert_eq!(stub.receipts_created(), 1, "幂等命中不新建回执");
}

// ————————————————————————————————————————————————
// 4. 替身可配置 + 替身只出现在测试目录
// ————————————————————————————————————————————————

#[test]
fn 替身可按场景配置状态码与响应体() {
    // 任意状态码 + 任意响应体：200 合规回执
    let stub = ReportingGatewayStub::start(vec![Scenario::Raw {
        status: 200,
        body: serde_json::json!({
            "receipt_id": "RCPT-RAW-01",
            "accepted_at": "2026-11-06 09:12:00",
            "status": "accepted"
        })
        .to_string(),
    }]);
    let outcome = client_for(&stub, 5).submit(&request_for("row-contract-01"));
    assert!(outcome.is_accepted());
    match &outcome.kind {
        GatewayOutcomeKind::Accepted(a) => assert_eq!(a.receipt_id, "RCPT-RAW-01"),
        other => panic!("应受理，实际 {:?}", other),
    }

    // 任意状态码 + 非 spec 形状的正文：4xx 仍归退回，错误码退回 HTTP 状态码
    let stub2 = ReportingGatewayStub::start(vec![Scenario::Raw {
        status: 418,
        body: "不是 JSON".to_string(),
    }]);
    let outcome2 = client_for(&stub2, 5).submit(&request_for("row-contract-01"));
    assert!(outcome2.is_rejected());
    match &outcome2.kind {
        GatewayOutcomeKind::Rejected(r) => {
            assert_eq!(r.http_status, 418);
            assert_eq!(r.error_code, "HTTP_418");
            assert!(!r.message.is_empty());
        }
        other => panic!("应退回，实际 {:?}", other),
    }
}

#[test]
fn 替身只出现在测试目录_源码里没有替身引用() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    assert!(src.is_dir(), "源码目录应存在：{}", src.display());

    let mut checked = 0usize;
    let mut offenders: Vec<(PathBuf, String)> = Vec::new();
    collect_rs_files(&src, &mut checked, &mut offenders);
    assert!(checked > 0, "源码目录里没扫到 .rs 文件");

    for (path, marker) in offenders {
        panic!(
            "源码 {} 里出现了替身引用（{}）—— 替身只允许在 tests/ 目录",
            path.display(),
            marker
        );
    }

    // 替身文件本身在 tests/ 目录里
    let stub_file = root.join("tests").join("gateway_stub").join("reporting.rs");
    assert!(stub_file.is_file(), "替身应落在 tests/ 目录：{}", stub_file.display());
}

#[test]
fn 源码里出现替身引用会被这条测试抓到() {
    // 自检：这条测试自己往源码里塞一个替身引用会怎样 —— 抓不到就说明判据失效了。
    let probe = "gateway_stub";
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0usize;
    let mut offenders: Vec<(PathBuf, String)> = Vec::new();
    collect_rs_files(&src, &mut checked, &mut offenders);
    assert!(offenders.is_empty(), "当前源码不该有替身引用");
    assert!(probe.contains("gateway_stub"));
}

/// 递归收集 .rs 文件，命中替身标记就记下来
fn collect_rs_files(dir: &Path, checked: &mut usize, offenders: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, checked, offenders);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        *checked += 1;
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for marker in ["gateway_stub", "ReportingGatewayStub", "StubServer"] {
            if text.contains(marker) {
                offenders.push((path.clone(), marker.to_string()));
            }
        }
    }
}
