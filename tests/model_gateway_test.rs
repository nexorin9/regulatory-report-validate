//! ModelGateway 协议兼容测试。
//!
//! 断言落在**实际发出去的报文**上：请求路径、鉴权头、请求体字段全集、
//! 响应解析、错误语义分层。对端是 [`gateway_stub`] 里的协议替身
//! （本机可控进程）；产品代码走的是真实 OpenAI-compatible 协议，
//! 院内真实模型端点**本轮未实连**（凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gateway_stub::{chat_completion, StubResponse, StubServer};
use regval::config::MapEnv;
use regval::errors::ErrorCode;
use regval::model::gateway::{MergeCandidate, MergePair, MergeRequest, ModelGateway};
use regval::model::openai::{ModelConfig, OpenAiModelGateway};
use regval::model::schema::MergeKind;

/// 一对候选行（问模型「这两个说法是不是同一件事」）
fn pair() -> MergePair {
    MergePair {
        left: MergeCandidate {
            row_key: "row-left-001".to_string(),
            indicator_name: "医院感染例数".to_string(),
            dept_name: "院感科".to_string(),
            reporting_period: "2026-Q3".to_string(),
        },
        right: MergeCandidate {
            row_key: "row-right-002".to_string(),
            indicator_name: "院感例数".to_string(),
            dept_name: "院感科".to_string(),
            reporting_period: "2026-Q3".to_string(),
        },
    }
}

/// 合规的归并回答
fn good_content() -> String {
    serde_json::json!({
        "items": [{
            "left_row_key": "row-left-001",
            "right_row_key": "row-right-002",
            "kind": "same_indicator",
            "confidence": 0.83,
            "reason": "两处指标名指的都是医院感染例数，只是写法不同",
            "evidence": ["左侧含『医院感染』", "右侧含『院感』"]
        }]
    })
    .to_string()
}

/// 给替身起一个端点，配好客户端
fn gateway_for(stub: &StubServer) -> OpenAiModelGateway {
    let cfg = ModelConfig {
        base_url: stub.base_url.clone(),
        api_key: "stub-token".to_string(),
        model: "stub-model".to_string(),
        timeout_secs: 5,
    };
    OpenAiModelGateway::new(cfg).expect("构造模型客户端失败")
}

/// 源码目录（静态分层断言用）
fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 递归列出目录下的 .rs 文件
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn 请求路径_鉴权头_请求体字段全集() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&good_content()),
    )]);
    let gw = gateway_for(&stub);

    let resp = gw
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect("归并调用应当成功");

    // 响应解析
    assert_eq!(resp.items.len(), 1);
    assert_eq!(resp.items[0].kind, MergeKind::SameIndicator);
    assert_eq!(resp.items[0].confidence, 0.83);
    assert_eq!(resp.items[0].evidence.len(), 2);
    assert_eq!(resp.served_model.as_deref(), Some("stub-model"));
    assert_eq!(resp.usage.as_ref().unwrap().total_tokens, Some(170));

    // 实际发出去的报文
    let req = stub.next_request();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/chat/completions");
    assert_eq!(req.header("authorization"), Some("Bearer stub-token"));
    assert_eq!(
        req.header("content-type"),
        Some("application/json"),
        "请求应当带 JSON 内容类型"
    );

    let body = req.body_json();
    let obj = body.as_object().expect("请求体应是对象");
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "messages",
            "model",
            "response_format",
            "stream",
            "temperature"
        ],
        "请求体字段全集固定，多一个少一个都不行"
    );
    assert_eq!(body["model"], serde_json::json!("stub-model"));
    assert_eq!(body["stream"], serde_json::json!(false));
    assert_eq!(body["temperature"], serde_json::json!(0.0));
    assert_eq!(
        body["response_format"]["type"],
        serde_json::json!("json_object")
    );

    let messages = body["messages"].as_array().expect("messages 应是数组");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], serde_json::json!("system"));
    assert_eq!(messages[1]["role"], serde_json::json!("user"));
}

#[test]
fn 载荷只含四项最小必要字段且不含患者明细() {
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&good_content()),
    )]);
    let gw = gateway_for(&stub);
    gw.merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect("归并调用应当成功");

    let req = stub.next_request();
    let user_content = req.body_json()["messages"][1]["content"]
        .as_str()
        .expect("用户消息应是文本")
        .to_string();
    let payload: serde_json::Value = serde_json::from_str(&user_content).expect("载荷应是 JSON");

    let left = &payload["pairs"][0]["left"];
    let mut ckeys: Vec<&str> = left
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    ckeys.sort();
    assert_eq!(
        ckeys,
        vec!["dept_name", "indicator_name", "reporting_period", "row_key"],
        "候选行只送四项最小必要字段"
    );
    assert_eq!(left["row_key"], serde_json::json!("row-left-001"));
    assert_eq!(left["indicator_name"], serde_json::json!("医院感染例数"));

    // 整份报文里不得出现数值口径字段与患者可识别字段
    let whole = req.body.to_ascii_lowercase();
    for banned in [
        "numerator",
        "denominator",
        "business_key",
        "住院号",
        "身份证",
        "患者姓名",
        "病案号",
    ] {
        assert!(
            !whole.contains(banned),
            "请求体里不该出现 {}：{}",
            banned,
            req.body
        );
    }
}

#[test]
fn 响应未通过结构校验_整次调用不可用() {
    let bad_contents = [
        // 不是 JSON
        "这不是 JSON",
        // 缺 items
        r#"{"nope":[]}"#,
        // items 不是数组
        r#"{"items":{}}"#,
        // kind 取值不认识
        r#"{"items":[{"left_row_key":"row-left-001","right_row_key":"row-right-002","kind":"maybe","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
        // 置信度越界
        r#"{"items":[{"left_row_key":"row-left-001","right_row_key":"row-right-002","kind":"same_indicator","confidence":1.4,"reason":"r","evidence":["a"]}]}"#,
        // 没有依据
        r#"{"items":[{"left_row_key":"row-left-001","right_row_key":"row-right-002","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":[]}]}"#,
        // 模型编了个没问过的行键
        r#"{"items":[{"left_row_key":"row-left-001","right_row_key":"row-ghost-999","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
        // 左右是同一行
        r#"{"items":[{"left_row_key":"row-left-001","right_row_key":"row-left-001","kind":"same_indicator","confidence":0.5,"reason":"r","evidence":["a"]}]}"#,
    ];

    for content in bad_contents {
        let stub = StubServer::start(vec![StubResponse::json(200, chat_completion(content))]);
        let gw = gateway_for(&stub);
        let err = gw
            .merge_candidates(&MergeRequest::new(vec![pair()]))
            .expect_err("不合规的响应必须让整次调用不可用");
        assert_eq!(
            err.code,
            ErrorCode::ModelResponseInvalid,
            "内容：{}",
            content
        );
    }
}

#[test]
fn 端点回的不是_chat_completions_结构_算响应不合法() {
    let stub = StubServer::start(vec![StubResponse::json(200, r#"{"unexpected":true}"#)]);
    let gw = gateway_for(&stub);
    let err = gw
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect_err("非 chat/completions 结构必须报错");
    assert_eq!(err.code, ErrorCode::ModelResponseInvalid);
}

#[test]
fn 错误语义分层_四xx_五xx_连不上_超时_互不混淆() {
    // 4xx：端点拒绝请求
    let stub = StubServer::start(vec![StubResponse::json(
        400,
        r#"{"error":{"message":"bad request: temperature unsupported"}}"#,
    )]);
    let err = gateway_for(&stub)
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect_err("4xx 必须报错");
    assert_eq!(err.code, ErrorCode::ModelBadRequest);
    assert!(
        err.message.contains("400"),
        "错误里要带状态码：{}",
        err.message
    );

    // 5xx：端点服务端错误
    let stub = StubServer::start(vec![StubResponse::json(500, "upstream boom")]);
    let err = gateway_for(&stub)
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect_err("5xx 必须报错");
    assert_eq!(err.code, ErrorCode::ModelServerError);
    assert!(
        err.message.contains("500"),
        "错误里要带状态码：{}",
        err.message
    );

    // 连不上：端点端口没人在听
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("占端口失败");
        let addr = listener.local_addr().expect("读端口失败");
        drop(listener);
        format!("http://{}", addr)
    };
    let cfg = ModelConfig {
        base_url: dead,
        api_key: "stub-token".to_string(),
        model: "stub-model".to_string(),
        timeout_secs: 3,
    };
    let gw = OpenAiModelGateway::new(cfg).unwrap();
    let err = gw
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect_err("连不上必须报错");
    assert_eq!(err.code, ErrorCode::ModelConnectionFailed);

    // 超时：端点睡够久，客户端先超时
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&good_content()),
    )
    .with_delay(2500)]);
    let cfg = ModelConfig {
        base_url: stub.base_url.clone(),
        api_key: "stub-token".to_string(),
        model: "stub-model".to_string(),
        timeout_secs: 1,
    };
    let gw = OpenAiModelGateway::new(cfg).unwrap();
    let err = gw
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect_err("超时必须报错");
    assert_eq!(err.code, ErrorCode::ModelTimeout);
}

#[test]
fn 端点不支持_json_模式时去掉该字段重试一次() {
    let stub = StubServer::start(vec![
        StubResponse::json(
            400,
            r#"{"error":{"message":"response_format is not supported"}}"#,
        ),
        StubResponse::json(200, chat_completion(&good_content())),
    ]);
    let gw = gateway_for(&stub);
    let resp = gw
        .merge_candidates(&MergeRequest::new(vec![pair()]))
        .expect("去掉 response_format 后应当成功");
    assert_eq!(resp.items.len(), 1);

    let first = stub.next_request();
    assert!(first
        .body_json()
        .as_object()
        .unwrap()
        .contains_key("response_format"));
    let second = stub.next_request();
    assert!(!second
        .body_json()
        .as_object()
        .unwrap()
        .contains_key("response_format"));
}

#[test]
fn 凭据缺失_在发起请求前就报错() {
    let mut env = MapEnv::new();
    env.set("OPENAI_BASE_URL", "https://model.internal.example/v1");
    env.set("OPENAI_MODEL", "some-model");
    env.set("OPENAI_TIMEOUT_SECS", "20");
    // 故意不给 OPENAI_API_KEY
    let err = ModelConfig::load(&env).expect_err("缺凭据必须报错");
    assert_eq!(err.code, ErrorCode::ConfigMissing);
    assert!(err.message.contains("OPENAI_API_KEY"));
}

#[test]
fn 业务层不依赖厂商sdk_只有模型层客户端碰网络() {
    let src = src_dir();
    let mut http_users = Vec::new();
    for path in rust_files(&src) {
        let text = std::fs::read_to_string(&path).expect("读源码失败");
        if text.contains("reqwest::") {
            http_users.push(
                path.strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    assert_eq!(
        http_users,
        vec![
            "model/openai.rs".to_string(),
            "push/client.rs".to_string(),
            "push/dept.rs".to_string(),
        ],
        "HTTP 客户端只允许出现在三个适配层：模型端点客户端、上报网关客户端与 HIS 科室目录客户端"
    );

    // 业务层（重复检测漏斗）既不碰 HTTP，也不出现厂商名
    for path in rust_files(&src.join("dupe")) {
        let text = std::fs::read_to_string(&path).expect("读源码失败");
        let lower = text.to_ascii_lowercase();
        assert!(
            !lower.contains("reqwest"),
            "{} 里不该出现 HTTP 客户端",
            path.display()
        );
        assert!(
            !lower.contains("openai"),
            "{} 里不该出现厂商名",
            path.display()
        );
    }

    // provider 中立的抽象与结构校验层也不碰 HTTP
    for rel in ["model/gateway.rs", "model/schema.rs"] {
        let text = std::fs::read_to_string(src.join(rel)).expect("读源码失败");
        assert!(
            !text.contains("reqwest::"),
            "{} 是 provider 中立层，不该碰 HTTP 客户端",
            rel
        );
    }
}

#[test]
fn 模型层不写业务表() {
    let src = src_dir().join("model");
    for path in rust_files(&src) {
        let text = std::fs::read_to_string(&path).expect("读源码失败");
        for banned in ["rusqlite::", "Repo::", "INSERT INTO", "conn.execute"] {
            assert!(
                !text.contains(banned),
                "{} 里不该出现 {}：模型层不得写任何业务表",
                path.display(),
                banned
            );
        }
    }
}

#[test]
fn 已知行键集合只含本次问过的行() {
    let req = MergeRequest::new(vec![pair()]);
    let keys: BTreeSet<String> = req.known_row_keys();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains("row-left-001"));
    assert!(keys.contains("row-right-002"));
    assert_eq!(req.row_count(), 2);
}
