//! 协议替身：最小 HTTP 服务端，只回放固定响应并**记录收到的请求**。
//!
//! # 它是什么，不是什么
//!
//! 这是**测试数据层**的替身 —— 把对端换成本机一个可控进程，好断言
//! 「引擎发出去的到底是什么」。它**不是**最终对接方式：
//!
//! - 产品代码走的是真实 OpenAI-compatible 协议（`POST {base}/chat/completions`
//!   + `Authorization: Bearer` + 固定请求体字段全集）；
//! - 替身只换了对端地址与响应内容，不改协议形状 —— 请求路径、请求头、
//!   请求体字段都由同一个客户端发出，断言落在**实际发出去的那份报文**上；
//! - 院内真实模型端点（凭据与地址走环境变量）**本轮未实连**，见 progress.txt。
//!
//! 只用标准库（`std::net`），不引第三方 mock 依赖。

// 同一个替身包被多个测试目标共用（模型端点、上报网关），各目标用到的部分不同 ——
// 没用到的那半边在本目标里会报「未使用」，这里统一压掉（只是告警，不影响行为）。
#![allow(dead_code)]

/// 院内卫健上报网关的协议兼容替身（按 `spec.md`「出站报文（正式对接规范）」实现：
/// 自己校验路径 / 头 / 报文字段全集，按幂等键记回执台账）。
pub mod reporting;

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 替身收到的一次请求
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    /// 头名统一小写
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl CapturedRequest {
    /// 取一个请求头（大小写不敏感）
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// 请求体解析成 JSON（请求体不是合法 JSON 时测试直接失败）
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("请求体不是合法 JSON")
    }
}

/// 替身要回的一口气
#[derive(Debug, Clone)]
pub struct StubResponse {
    pub status: u16,
    pub body: String,
    pub content_type: String,
    /// 回包前先睡这么久（测超时用）
    pub delay_ms: u64,
}

impl StubResponse {
    /// 回一坨 JSON
    pub fn json(status: u16, body: impl Into<String>) -> StubResponse {
        StubResponse {
            status,
            body: body.into(),
            content_type: "application/json".to_string(),
            delay_ms: 0,
        }
    }

    /// 回包前先睡一会儿
    pub fn with_delay(mut self, ms: u64) -> StubResponse {
        self.delay_ms = ms;
        self
    }
}

/// 包一层 OpenAI-compatible 的 chat completion 外壳
#[allow(dead_code)]
pub fn chat_completion(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-stub",
        "object": "chat.completion",
        "model": "stub-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 128, "completion_tokens": 42, "total_tokens": 170}
    })
    .to_string()
}

/// 替身服务
pub struct StubServer {
    /// 端点根地址（引擎的 `OPENAI_BASE_URL` 填它）
    pub base_url: String,
    requests: Receiver<CapturedRequest>,
    running: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

impl StubServer {
    /// 起一个替身；`responses` 按请求顺序依次回（一个请求对一次响应）
    pub fn start(responses: Vec<StubResponse>) -> StubServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("替身端口绑定失败");
        let addr = listener.local_addr().expect("读替身端口失败");
        let (tx, rx) = mpsc::channel();
        let queue = Arc::new(Mutex::new(responses.into_iter().collect::<VecDeque<_>>()));
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);

        thread::spawn(move || {
            for stream in listener.incoming() {
                if !flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { break };
                let tx = tx.clone();
                let queue = Arc::clone(&queue);
                thread::spawn(move || {
                    let response = {
                        let mut q = queue.lock().expect("替身队列锁坏了");
                        q.pop_front()
                    };
                    // 队列空了还来请求：回 503（比不回更利于排障）
                    let response = response.unwrap_or_else(|| {
                        StubResponse::json(503, r#"{"error":{"message":"替身队列已空"}}"#)
                    });
                    handle_connection(stream, &response, tx);
                });
            }
        });

        StubServer {
            base_url: format!("http://{}", addr),
            requests: rx,
            running,
            addr,
        }
    }

    /// 取下一次收到的请求（最多等 5 秒；没收到就 panic）
    pub fn next_request(&self) -> CapturedRequest {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("替身没有收到请求")
    }

    /// 取下一次收到的请求（最多等 millis 毫秒；没收到返回 None）
    #[allow(dead_code)]
    pub fn try_next_request(&self, millis: u64) -> Option<CapturedRequest> {
        self.requests
            .recv_timeout(Duration::from_millis(millis))
            .ok()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // 唤醒 accept 循环，让监听线程收工
        let _ = TcpStream::connect(self.addr);
    }
}

/// 处理一次连接：读请求 → 记录 → 回包
fn handle_connection(stream: TcpStream, response: &StubResponse, tx: Sender<CapturedRequest>) {
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

    let _ = tx.send(CapturedRequest {
        method,
        path,
        headers,
        body,
    });

    if response.delay_ms > 0 {
        thread::sleep(Duration::from_millis(response.delay_ms));
    }

    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason_phrase(response.status),
        response.content_type,
        response.body.as_bytes().len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(response.body.as_bytes());
    let _ = stream.flush();
}

/// 状态码的英文短语（只求 HTTP 报文完整，客户端不看这行）
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}
