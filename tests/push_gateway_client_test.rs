//! 院内卫健上报网关出站客户端的协议兼容测试。
//!
//! 断言落在**实际发出去的报文**上：请求路径、鉴权头、幂等头、body 字段全集，
//! 以及网关三类响应（200 / 4xx / 5xx）各自的裁决分类。
//!
//! 对端是 [`gateway_stub`] 里的协议替身（本机可控进程）与「端口无人监听」情形；
//! 产品代码走的是 `spec.md`「出站报文（正式对接规范）」里的真实协议，
//! 院内真实上报网关**本轮未实连**（地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::net::TcpListener;

use gateway_stub::{StubResponse, StubServer};
use regval::config::MapEnv;
use regval::errors::ErrorCode;
use regval::push::client::{
    GatewayClient, GatewayConfig, GatewayOutcomeKind, SubmissionRequest, OUTBOUND_FIELDS,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};

/// 一条待上报的载荷
fn payload() -> SubmissionPayload {
    SubmissionPayload::new(PayloadInput {
        batch_ref: "mini-941b47da51bd",
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key: "BK-SAMPLE-01",
        numerator: NumericField::Number(980.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    })
}

/// 一次提交请求（行键不进报文，只用来把裁决挂回这一行）
fn request() -> SubmissionRequest {
    SubmissionRequest {
        row_uid: "row-58b9b50a".to_string(),
        payload: payload(),
    }
}

/// 给替身配一个客户端
fn client_for(stub: &StubServer) -> GatewayClient {
    GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .expect("客户端应可建")
}

/// 找一个没人监听的端口（连它必被拒）
fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("端口绑定失败");
    let port = listener.local_addr().expect("读端口失败").port();
    drop(listener);
    port
}

#[test]
fn 请求路径鉴权头幂等头与报文字段全集() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-001","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
    )]);
    let client = client_for(&stub);

    let outcome = client.submit(&request());
    assert!(outcome.is_accepted());

    let req = stub.next_request();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/v1/regulatory-submissions");
    assert_eq!(req.header("authorization"), Some("Bearer stub-token"));
    assert_eq!(
        req.header("content-type"),
        Some("application/json")
    );

    // 幂等头与 body 里的幂等键同值
    let body = req.body_json();
    let idem_in_body = body["idempotency_key"].as_str().unwrap().to_string();
    assert_eq!(req.header("idempotency-key"), Some(idem_in_body.as_str()));
    assert_eq!(idem_in_body.len(), 64);

    // body 字段全集：固定 15 项，少一多一都不行
    let mut names: Vec<String> = body
        .as_object()
        .unwrap()
        .keys()
        .map(|s| s.to_string())
        .collect();
    names.sort();
    let mut expected: Vec<String> = OUTBOUND_FIELDS.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(names.len(), 15);

    // 字段取值逐个对上
    assert_eq!(body["batch_ref"], serde_json::json!("mini-941b47da51bd"));
    assert_eq!(body["reporting_period"], serde_json::json!("2026-Q3"));
    assert_eq!(body["org_code"], serde_json::json!("ORG-001"));
    assert_eq!(body["indicator_code"], serde_json::json!("MER-TL-04"));
    assert_eq!(body["dept_code"], serde_json::json!("0301"));
    assert_eq!(body["business_key"], serde_json::json!("BK-SAMPLE-01"));
    assert_eq!(body["numerator"], serde_json::json!(980.0));
    assert_eq!(body["denominator"], serde_json::json!(1000.0));
    assert_eq!(body["value"], serde_json::json!(98.0));
    assert_eq!(body["unit"], serde_json::json!("%"));
    assert_eq!(body["revision"], serde_json::json!(1));

    // 报文里不含患者可识别明细
    let raw = req.body.to_lowercase();
    for banned in ["住院号", "身份证", "患者姓名", "病案号", "row_uid", "row-58b9b50a"] {
        assert!(!raw.contains(banned), "报文里不该出现 {}", banned);
    }
}

#[test]
fn 两百返回解析出受理回执() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-77","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
    )]);
    let outcome = client_for(&stub).submit(&request());

    assert!(outcome.is_accepted());
    assert!(!outcome.is_rejected());
    assert!(!outcome.is_retryable());
    assert_eq!(outcome.row_uid, "row-58b9b50a");
    match outcome.kind {
        GatewayOutcomeKind::Accepted(a) => {
            assert_eq!(a.receipt_id, "RCPT-77");
            assert_eq!(a.accepted_at, "2026-11-06 09:12:00");
            assert_eq!(a.status, "accepted");
        }
        other => panic!("应是受理，实际 {:?}", other),
    }
}

#[test]
fn 四百返回提取错误码与字段名并可写回该行() {
    let stub = StubServer::start(vec![StubResponse::json(
        422,
        r#"{"error_code":"E_REPORTING_PERIOD","error_field":"reporting_period","message":"报告期格式不合法"}"#,
    )]);
    let outcome = client_for(&stub).submit(&request());

    assert!(outcome.is_rejected());
    assert!(!outcome.is_accepted());
    assert!(!outcome.is_retryable());
    match outcome.kind {
        GatewayOutcomeKind::Rejected(r) => {
            assert_eq!(r.http_status, 422);
            assert_eq!(r.error_code, "E_REPORTING_PERIOD");
            assert_eq!(r.error_field, "reporting_period");
            assert_eq!(r.message, "报告期格式不合法");
            // 写回这一行的说明文本：错误码与字段名都在
            let note = r.row_note();
            assert!(note.contains("E_REPORTING_PERIOD"));
            assert!(note.contains("reporting_period"));
            assert!(note.contains("报告期格式不合法"));
        }
        other => panic!("应是退回，实际 {:?}", other),
    }
}

#[test]
fn 五百返回归为可重试且不改成成功() {
    let stub = StubServer::start(vec![StubResponse::json(
        500,
        r#"{"error":{"message":"网关内部错误"}}"#,
    )]);
    let outcome = client_for(&stub).submit(&request());

    assert!(outcome.is_retryable());
    assert!(!outcome.is_accepted(), "5xx 绝不能被改写成已上报");
    assert!(!outcome.is_rejected());
    match outcome.kind {
        GatewayOutcomeKind::Retryable { error_code, message } => {
            assert_eq!(error_code, ErrorCode::GatewayServerError);
            assert_eq!(error_code.as_str(), "GATEWAY_SERVER_ERROR");
            assert!(message.contains("500"));
        }
        other => panic!("应可重试，实际 {:?}", other),
    }
}

#[test]
fn 超时归为可重试且不带成功结论() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-SLOW","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
    )
    .with_delay(2500)]);
    let client = GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 1,
    })
    .expect("客户端应可建");

    let outcome = client.submit(&request());
    assert!(outcome.is_retryable());
    assert!(!outcome.is_accepted());
    match outcome.kind {
        GatewayOutcomeKind::Retryable { error_code, .. } => {
            assert_eq!(error_code, ErrorCode::GatewayTimeout);
            assert_eq!(error_code.as_str(), "GATEWAY_TIMEOUT");
        }
        other => panic!("应可重试，实际 {:?}", other),
    }
}

#[test]
fn 连不上时归为可重试且不静默改成成功() {
    let port = unused_port();
    let client = GatewayClient::new(GatewayConfig {
        base_url: format!("http://127.0.0.1:{}", port),
        token: "stub-token".to_string(),
        timeout_secs: 3,
    })
    .expect("客户端应可建");

    let outcome = client.submit(&request());
    assert!(outcome.is_retryable());
    assert!(!outcome.is_accepted());
    match outcome.kind {
        GatewayOutcomeKind::Retryable { error_code, .. } => {
            assert_eq!(error_code, ErrorCode::GatewayConnectionFailed);
            assert_eq!(error_code.as_str(), "GATEWAY_CONNECTION_FAILED");
        }
        other => panic!("应可重试，实际 {:?}", other),
    }
}

#[test]
fn 两百但回执缺字段时不当成成功() {
    let stub = StubServer::start(vec![StubResponse::json(200, r#"{"receipt_id":"RCPT-001"}"#)]);
    let outcome = client_for(&stub).submit(&request());

    assert!(outcome.is_invalid());
    assert!(!outcome.is_accepted(), "回执缺字段绝不能当成已上报");
    assert!(!outcome.is_retryable());
    match outcome.kind {
        GatewayOutcomeKind::Invalid { error_code, message } => {
            assert_eq!(error_code, ErrorCode::GatewayResponseInvalid);
            assert!(message.contains("receipt_id"));
        }
        other => panic!("应判回执不可用，实际 {:?}", other),
    }
}

#[test]
fn 缺凭据时报错且一个包都没发出去() {
    let mut env = MapEnv::new();
    env.set("REGULATORY_REPORT_BASE_URL", "https://gateway.internal.example");
    let err = GatewayConfig::load(&env).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigMissing);
    assert!(err.message.contains("REGULATORY_REPORT_TOKEN"));

    let err = GatewayClient::from_env(&env).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigMissing);
}

#[test]
fn 数值填坏时请求根本没发出去() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-001","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
    )]);
    let client = client_for(&stub);

    let mut payload = payload();
    payload.numerator = NumericField::Raw("填坏了".to_string());
    let outcome = client.submit(&SubmissionRequest {
        row_uid: "row-58b9b50a".to_string(),
        payload,
    });

    assert!(outcome.is_blocked());
    assert!(!outcome.is_accepted());
    assert!(!outcome.is_retryable());
    match outcome.kind {
        GatewayOutcomeKind::Blocked { error_code, message } => {
            assert_eq!(error_code, ErrorCode::InvalidNumeric);
            assert!(message.contains("分子"));
            assert!(message.contains("填坏了"));
        }
        other => panic!("应被拦下，实际 {:?}", other),
    }

    // 一个包都没发出去（替身没收到请求）
    assert!(
        stub.try_next_request(300).is_none(),
        "数值填坏的行不该发出请求"
    );
}

#[test]
fn 凭据只来自环境变量不写死在代码里() {
    // 客户端源码里不出现任何字面凭据，也不出现院内真实地址
    let source = include_str!("../src/push/client.rs");
    assert!(!source.contains("Bearer sk-"));
    assert!(!source.contains("REGULATORY_REPORT_TOKEN="));
    // 端点与凭据一律从配置来
    let cfg = GatewayConfig {
        base_url: "https://gateway.internal.example".to_string(),
        token: "from-env".to_string(),
        timeout_secs: 5,
    };
    assert!(cfg.submissions_endpoint().starts_with("https://gateway.internal.example"));
}

#[test]
fn 上报侧只有网关客户端碰网络() {
    // 幂等身份（本地计算）与网络无关
    let identity = include_str!("../src/push/idempotency.rs");
    assert!(!identity.contains("reqwest"));

    // 上报侧除网关客户端外都不碰 HTTP
    let module = include_str!("../src/push/mod.rs");
    assert!(!module.contains("reqwest"));
}
