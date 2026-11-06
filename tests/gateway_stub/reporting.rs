//! 院内卫健上报网关的**协议兼容替身** —— 按 `spec.md`「出站报文（正式对接规范）」实现。
//!
//! # 它是什么，不是什么
//!
//! 这是**测试数据层**的替身：把对端换成本机一个可控进程，好逐条断言
//! 「引擎发出去的报文长什么样」「网关回什么」「引擎怎么分类」。
//! 它**不是**最终对接方式：
//!
//! - 产品代码走的是真实协议（`POST {base}/api/v1/regulatory-submissions`
//!   + `Authorization: Bearer` + `Idempotency-Key` + 固定 15 项报文字段全集）；
//! - 替身只换了对端地址与响应内容，**协议形状一点没改** ——
//!   断言落在**实际发出去的那一份报文**上；
//! - 院内真实上报网关**本轮未实连**（地址与凭据走环境变量），见 progress.txt。
//!
//! # 这个替身比「回放固定响应」多做了什么
//!
//! 它**按 spec 自己校验报文**（方法 / 路径 / 鉴权头 / 幂等头 / 报文字段全集 /
//! 字段类型 / 幂等键与头同值），不合规就按 spec 的 `4xx` 形状退回并点名是哪一处：
//!
//! | 不合规处 | 状态码 | 错误码 |
//! |---|---|---|
//! | 方法不是 POST | 405 | `E_METHOD_NOT_ALLOWED` |
//! | 路径不对 | 404 | `E_PATH_NOT_FOUND` |
//! | 缺 / 错鉴权头 | 401 | `E_UNAUTHORIZED` |
//! | 缺幂等头 | 400 | `E_IDEMPOTENCY_KEY_MISSING` |
//! | 幂等头与 body 不一致 | 400 | `E_IDEMPOTENCY_KEY_MISMATCH` |
//! | body 不是 JSON | 400 | `E_BODY_NOT_JSON` |
//! | **少传字段** | 400 | `E_FIELD_MISSING`（`error_field` 点名缺哪个） |
//! | **多传字段** | 400 | `E_FIELD_UNEXPECTED`（`error_field` 点名多哪个） |
//! | 字段类型不对 | 400 | `E_FIELD_TYPE_INVALID` |
//!
//! # 幂等语义
//!
//! 替身按 `Idempotency-Key` 记台账：**同一个键再来一次，回的是同一张回执**
//! （`receipt_id` 与 `accepted_at` 逐字相同），**不新建回执** ——
//! `receipts_created` 计数不涨。这一条正是「重复提交不新建」的判据。
//!
//! 只用标准库（`std::net`），不引第三方 mock 依赖。

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// 上报路径（`spec.md` 出站报文规范）
pub const SUBMISSIONS_PATH: &str = "/api/v1/regulatory-submissions";

/// 报文字段全集（`spec.md` 出站报文规范，15 项；少一多一都不合规）
pub const SPEC_FIELDS: [&str; 15] = [
    "batch_ref",
    "reporting_period",
    "org_code",
    "indicator_code",
    "dept_code",
    "business_key",
    "numerator",
    "denominator",
    "value",
    "unit",
    "basis",
    "rule_version",
    "idempotency_key",
    "payload_digest",
    "revision",
];

/// 必须是字符串的字段
pub const STRING_FIELDS: [&str; 11] = [
    "batch_ref",
    "reporting_period",
    "org_code",
    "indicator_code",
    "dept_code",
    "business_key",
    "unit",
    "basis",
    "rule_version",
    "idempotency_key",
    "payload_digest",
];

/// 必须是数字或 null 的字段（没填给 null，**不推断成 0**）
pub const NUMERIC_FIELDS: [&str; 3] = ["numerator", "denominator", "value"];

/// 必须是整数的字段
pub const INTEGER_FIELDS: [&str; 1] = ["revision"];

/// 替身默认期待的 Bearer 凭据
pub const DEFAULT_TOKEN: &str = "stub-token";

/// 受理时间（固定值：幂等重放要逐字相同）
pub const ACCEPTED_AT: &str = "2026-11-06 09:12:00";

/// 替身要回的一口气 —— 按场景构造，便于逐条造用例。
#[derive(Debug, Clone)]
pub enum Scenario {
    /// `200` + 合规回执（首次受理时**新建**回执并按幂等键记账）
    Accepted,
    /// `4xx` + `spec` 形状的退回正文（`error_code` / `error_field` / `message`）
    Rejected {
        status: u16,
        error_code: String,
        error_field: String,
        message: String,
    },
    /// `5xx`（服务端侧问题 → 可退避重试）
    ServerError { status: u16 },
    /// 回包前先睡这么久（客户端超时会先到）
    Timeout { delay_ms: u64 },
    /// `200` 但回执缺字段（引擎不该把它当成成功）
    InvalidReceipt,
    /// 任意状态码 + 任意响应体（用于构造 spec 之外的情形）
    Raw { status: u16, body: String },
}

impl Scenario {
    /// 一次 `4xx` 退回
    pub fn rejected(status: u16, error_code: &str, error_field: &str, message: &str) -> Scenario {
        Scenario::Rejected {
            status,
            error_code: error_code.to_string(),
            error_field: error_field.to_string(),
            message: message.to_string(),
        }
    }
}

/// 报文不合规的一处。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    MethodNotPost { got: String },
    PathMismatch { got: String },
    MissingAuthorization,
    AuthorizationMismatch,
    MissingIdempotencyKey,
    IdempotencyKeyMismatch { header: String, body: String },
    BodyNotJson { detail: String },
    FieldMissing { field: String },
    FieldUnexpected { field: String },
    FieldTypeWrong { field: String, expected: String },
}

impl Violation {
    /// 机器可读错误码
    pub fn error_code(&self) -> &'static str {
        match self {
            Violation::MethodNotPost { .. } => "E_METHOD_NOT_ALLOWED",
            Violation::PathMismatch { .. } => "E_PATH_NOT_FOUND",
            Violation::MissingAuthorization | Violation::AuthorizationMismatch => "E_UNAUTHORIZED",
            Violation::MissingIdempotencyKey => "E_IDEMPOTENCY_KEY_MISSING",
            Violation::IdempotencyKeyMismatch { .. } => "E_IDEMPOTENCY_KEY_MISMATCH",
            Violation::BodyNotJson { .. } => "E_BODY_NOT_JSON",
            Violation::FieldMissing { .. } => "E_FIELD_MISSING",
            Violation::FieldUnexpected { .. } => "E_FIELD_UNEXPECTED",
            Violation::FieldTypeWrong { .. } => "E_FIELD_TYPE_INVALID",
        }
    }

    /// 网关点名的字段（字段级的不合规才有；其余为空串）
    pub fn error_field(&self) -> String {
        match self {
            Violation::FieldMissing { field }
            | Violation::FieldUnexpected { field }
            | Violation::FieldTypeWrong { field, .. } => field.clone(),
            _ => String::new(),
        }
    }

    /// 人话说明
    pub fn zh(&self) -> String {
        match self {
            Violation::MethodNotPost { got } => format!("上报只接受 POST，收到的是 {}", got),
            Violation::PathMismatch { got } => format!("上报路径不对：收到 {}", got),
            Violation::MissingAuthorization => "缺少 Authorization 头".to_string(),
            Violation::AuthorizationMismatch => "Authorization 头与凭据不匹配".to_string(),
            Violation::MissingIdempotencyKey => "缺少 Idempotency-Key 头".to_string(),
            Violation::IdempotencyKeyMismatch { header, body } => format!(
                "Idempotency-Key 头与报文里的 idempotency_key 不一致（头 {} / 报文 {}）",
                header, body
            ),
            Violation::BodyNotJson { detail } => format!("报文不是合法 JSON：{}", detail),
            Violation::FieldMissing { field } => format!("报文缺字段 {}", field),
            Violation::FieldUnexpected { field } => format!("报文多了字段 {}", field),
            Violation::FieldTypeWrong { field, expected } => {
                format!("报文字段 {} 的取值类型不对，应为 {}", field, expected)
            }
        }
    }

    /// 该不合规处对应的 HTTP 状态码
    pub fn http_status(&self) -> u16 {
        match self {
            Violation::MethodNotPost { .. } => 405,
            Violation::PathMismatch { .. } => 404,
            Violation::MissingAuthorization | Violation::AuthorizationMismatch => 401,
            _ => 400,
        }
    }
}

/// 替身收到的一次请求（含协议校验结论）。
#[derive(Debug, Clone)]
pub struct ReceivedRequest {
    pub method: String,
    pub path: String,
    /// 头名统一小写
    pub headers: BTreeMap<String, String>,
    pub body: String,
    /// 协议校验出来的不合规处（空 = 完全合规）
    pub violations: Vec<Violation>,
    /// 取到的幂等键（头里的；没有则空串）
    pub idempotency_key: String,
    /// 报文里实际出现的字段名（排序）
    pub fields: Vec<String>,
}

impl ReceivedRequest {
    /// 取一个请求头（大小写不敏感）
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// 请求体解析成 JSON（请求体不是合法 JSON 时测试直接失败）
    pub fn body_json(&self) -> Value {
        serde_json::from_str(&self.body).expect("请求体不是合法 JSON")
    }

    /// 是否完全符合 spec 的出站报文规范
    pub fn conforms(&self) -> bool {
        self.violations.is_empty()
    }
}

/// 替身记下的一张回执
#[derive(Debug, Clone)]
struct ReceiptRecord {
    receipt_id: String,
    body: String,
}

/// 替身的共享状态
#[derive(Debug)]
struct StubState {
    scenarios: VecDeque<Scenario>,
    last_scenario: Option<Scenario>,
    /// 幂等键 → 回执（**同一键再来一次回的是同一张，不新建**）
    receipts: HashMap<String, ReceiptRecord>,
    counter: usize,
    receipts_created: usize,
    requests_received: usize,
}

impl StubState {
    fn new(scenarios: Vec<Scenario>) -> StubState {
        let mut queue = VecDeque::new();
        let mut last = None;
        for s in scenarios {
            last = Some(s.clone());
            queue.push_back(s);
        }
        StubState {
            scenarios: queue,
            last_scenario: last,
            receipts: HashMap::new(),
            counter: 0,
            receipts_created: 0,
            requests_received: 0,
        }
    }

    /// 取下一个场景（队列空了就把最后一个再来一遍）
    fn take_scenario(&mut self) -> Scenario {
        match self.scenarios.pop_front() {
            Some(s) => {
                self.last_scenario = Some(s.clone());
                s
            }
            None => self
                .last_scenario
                .clone()
                .unwrap_or(Scenario::Accepted),
        }
    }
}

/// 院内卫健上报网关的协议兼容替身。
pub struct ReportingGatewayStub {
    /// 端点根地址（引擎的 `REGULATORY_REPORT_BASE_URL` 填它）
    pub base_url: String,
    /// 替身期待的 Bearer 凭据
    pub expected_token: String,
    requests: Receiver<ReceivedRequest>,
    running: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    state: Arc<Mutex<StubState>>,
}

impl ReportingGatewayStub {
    /// 起一个替身；`scenarios` 按请求顺序依次用，用完了重复最后一个
    pub fn start(scenarios: Vec<Scenario>) -> ReportingGatewayStub {
        ReportingGatewayStub::start_with_token(scenarios, DEFAULT_TOKEN)
    }

    /// 起一个替身，并指定它期待的 Bearer 凭据
    pub fn start_with_token(scenarios: Vec<Scenario>, token: &str) -> ReportingGatewayStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("替身端口绑定失败");
        let addr = listener.local_addr().expect("读替身端口失败");
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(StubState::new(scenarios)));
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        // 凭据与状态都要进 'static 的线程 —— 各自留一份给监听线程
        let token = token.to_string();
        let expected_token = token.clone();
        let state_for_thread = Arc::clone(&state);

        thread::spawn(move || {
            for stream in listener.incoming() {
                if !flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { break };
                let tx = tx.clone();
                let state = Arc::clone(&state_for_thread);
                let token = token.clone();
                thread::spawn(move || {
                    handle_connection(stream, &state, &token, &tx);
                });
            }
        });

        ReportingGatewayStub {
            base_url: format!("http://{}", addr),
            expected_token,
            requests: rx,
            running,
            addr,
            state,
        }
    }

    /// 取下一次收到的请求（最多等 5 秒；没收到就 panic）
    pub fn next_request(&self) -> ReceivedRequest {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("替身没有收到请求")
    }

    /// 取下一次收到的请求（最多等 millis 毫秒；没收到返回 None）
    pub fn try_next_request(&self, millis: u64) -> Option<ReceivedRequest> {
        self.requests.recv_timeout(Duration::from_millis(millis)).ok()
    }

    /// 收到过的请求数
    pub fn requests_received(&self) -> usize {
        self.state.lock().expect("替身状态锁坏了").requests_received
    }

    /// **新建**的回执数（幂等重放不涨 —— 这就是「重复提交不新建」的判据）
    pub fn receipts_created(&self) -> usize {
        self.state.lock().expect("替身状态锁坏了").receipts_created
    }

    /// 某个幂等键对应的回执号（没有则 None）
    pub fn receipt_id_for(&self, idempotency_key: &str) -> Option<String> {
        self.state
            .lock()
            .expect("替身状态锁坏了")
            .receipts
            .get(idempotency_key)
            .map(|r| r.receipt_id.clone())
    }
}

impl Drop for ReportingGatewayStub {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // 唤醒 accept 循环，让监听线程收工
        let _ = TcpStream::connect(self.addr);
    }
}

/// 处理一次连接：读请求 → 校验 → 记请求 → 回包
fn handle_connection(
    stream: TcpStream,
    state: &Arc<Mutex<StubState>>,
    expected_token: &str,
    tx: &Sender<ReceivedRequest>,
) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 && reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let (violations, idempotency_key, fields) =
        validate(&method, &path, &headers, &body, expected_token);
    let received = ReceivedRequest {
        method,
        path,
        headers,
        body,
        violations,
        idempotency_key,
        fields,
    };

    // 先把请求记下来（超时场景下客户端已经走了，测试仍能查这份报文）
    let _ = tx.send(received.clone());

    let (status, response_body, delay_ms) = respond(state, &received);
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }

    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason_phrase(status),
        response_body.as_bytes().len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(response_body.as_bytes());
    let _ = stream.flush();
}

/// 按 `spec.md` 校验一份报文，返回（不合规处、幂等键、报文字段名）。
fn validate(
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &str,
    expected_token: &str,
) -> (Vec<Violation>, String, Vec<String>) {
    let mut violations = Vec::new();

    if method != "POST" {
        violations.push(Violation::MethodNotPost {
            got: method.to_string(),
        });
    }
    if path != SUBMISSIONS_PATH {
        violations.push(Violation::PathMismatch {
            got: path.to_string(),
        });
    }

    match headers.get("authorization") {
        None => violations.push(Violation::MissingAuthorization),
        Some(v) => {
            let want = format!("Bearer {}", expected_token);
            if v.trim() != want {
                violations.push(Violation::AuthorizationMismatch);
            }
        }
    }

    let idem_header = headers.get("idempotency-key").cloned().unwrap_or_default();
    if !headers.contains_key("idempotency-key") {
        violations.push(Violation::MissingIdempotencyKey);
    }

    let mut fields: Vec<String> = Vec::new();
    let mut parsed: Option<Value> = None;
    match serde_json::from_str::<Value>(body) {
        Err(e) => violations.push(Violation::BodyNotJson { detail: e.to_string() }),
        Ok(Value::Object(map)) => {
            fields = map.keys().cloned().collect();
            fields.sort();
            parsed = Some(Value::Object(map));
        }
        Ok(other) => violations.push(Violation::BodyNotJson {
            detail: format!("报文顶层不是对象（是 {}）", type_name(&other)),
        }),
    }

    if let Some(Value::Object(map)) = &parsed {
        for field in SPEC_FIELDS {
            if !map.contains_key(field) {
                violations.push(Violation::FieldMissing {
                    field: field.to_string(),
                });
            }
        }
        for key in map.keys() {
            if !SPEC_FIELDS.contains(&key.as_str()) {
                violations.push(Violation::FieldUnexpected {
                    field: key.clone(),
                });
            }
        }
        for field in STRING_FIELDS {
            if let Some(value) = map.get(field) {
                if !value.is_string() {
                    violations.push(Violation::FieldTypeWrong {
                        field: field.to_string(),
                        expected: "字符串".to_string(),
                    });
                }
            }
        }
        for field in NUMERIC_FIELDS {
            if let Some(value) = map.get(field) {
                if !(value.is_number() || value.is_null()) {
                    violations.push(Violation::FieldTypeWrong {
                        field: field.to_string(),
                        expected: "数字或 null".to_string(),
                    });
                }
            }
        }
        for field in INTEGER_FIELDS {
            if let Some(value) = map.get(field) {
                if !value.is_i64() && !value.is_u64() {
                    violations.push(Violation::FieldTypeWrong {
                        field: field.to_string(),
                        expected: "整数".to_string(),
                    });
                }
            }
        }
        // 幂等键：头与报文必须同值（网关按头判幂等，报文里带的是同一份身份）
        if let Some(body_idem) = map.get("idempotency_key").and_then(Value::as_str) {
            if !idem_header.is_empty() && body_idem != idem_header {
                violations.push(Violation::IdempotencyKeyMismatch {
                    header: idem_header.clone(),
                    body: body_idem.to_string(),
                });
            }
        }
    }

    (violations, idem_header, fields)
}

/// 网关怎么回这一份报文。
///
/// 返回（状态码、响应体、回包前的等待毫秒数）。
fn respond(state: &Arc<Mutex<StubState>>, received: &ReceivedRequest) -> (u16, String, u64) {
    // 不合规 → 按 spec 的 4xx 形状退回，并点名是哪一处
    if let Some(violation) = received.violations.first() {
        let body = json!({
            "error_code": violation.error_code(),
            "error_field": violation.error_field(),
            "message": violation.zh(),
        })
        .to_string();
        return (violation.http_status(), body, 0);
    }

    let mut st = state.lock().expect("替身状态锁坏了");
    st.requests_received += 1;

    // 幂等命中：同一键再来一次 → 回同一张回执，**不新建**
    if let Some(record) = st.receipts.get(&received.idempotency_key) {
        return (200, record.body.clone(), 0);
    }

    match st.take_scenario() {
        Scenario::Accepted => {
            st.counter += 1;
            st.receipts_created += 1;
            let receipt_id = format!("RCPT-{:04}", st.counter);
            let body = json!({
                "receipt_id": receipt_id,
                "accepted_at": ACCEPTED_AT,
                "status": "accepted",
            })
            .to_string();
            st.receipts.insert(
                received.idempotency_key.clone(),
                ReceiptRecord {
                    receipt_id,
                    body: body.clone(),
                },
            );
            (200, body, 0)
        }
        Scenario::Rejected {
            status,
            error_code,
            error_field,
            message,
        } => {
            let body = json!({
                "error_code": error_code,
                "error_field": error_field,
                "message": message,
            })
            .to_string();
            (status, body, 0)
        }
        Scenario::ServerError { status } => {
            let body = json!({"error": {"message": "网关内部错误"}}).to_string();
            (status, body, 0)
        }
        Scenario::Timeout { delay_ms } => {
            // 客户端超时会先到；这份回包发出去也没人收
            let body = json!({
                "receipt_id": "RCPT-TIMEOUT",
                "accepted_at": ACCEPTED_AT,
                "status": "accepted",
            })
            .to_string();
            (200, body, delay_ms)
        }
        Scenario::InvalidReceipt => {
            // 200 但回执缺字段：引擎不该把它当成成功
            let body = json!({"receipt_id": "RCPT-BROKEN"}).to_string();
            (200, body, 0)
        }
        Scenario::Raw { status, body } => (status, body, 0),
    }
}

/// JSON 值的类型名（报错时给人话）
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

/// 状态码的英文短语（只求 HTTP 报文完整，客户端不看这行）
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
