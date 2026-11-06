//! 结构化日志与运行摘要。
//!
//! - **JSON 行日志**：一行一条 JSON（时间 / 级别 / 事件 / 批次 / 错误码 / 附加字段），
//!   写到 **stderr**；stdout 留给给人读的摘要与契约内容，两条通道互不污染。
//! - **运行摘要**：每次命令跑完把「表数 / 行数 / 问题数 / 耗时」落状态库，
//!   `regval status` 直接可查 —— 经办人下期打开同一台机器就知道上次跑过什么。
//!
//! 摘要落库只含指标级监管数据与科室级数据，不落患者可识别明细。

use std::collections::BTreeMap;
use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::errors::ErrorCode;

/// 日志级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// 从文本解析级别（`debug` / `info` / `warn` / `error`，大小写不敏感）
    pub fn parse(raw: &str) -> Option<Level> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// 等级门槛：由 `REGVAL_LOG_LEVEL` 决定，缺省 `info`。
pub fn level_threshold() -> Level {
    std::env::var("REGVAL_LOG_LEVEL")
        .ok()
        .and_then(|v| Level::parse(&v))
        .unwrap_or(Level::Info)
}

/// 写一条结构化 JSON 行日志。
///
/// 附加字段按固定顺序输出（`BTreeMap` 保证同输入同输出，便于比对与回归）。
pub fn log(
    level: Level,
    event: &str,
    batch_ref: Option<&str>,
    code: Option<&ErrorCode>,
    message: &str,
    extra: &[(&str, Value)],
) {
    if level < level_threshold() {
        return;
    }
    let mut obj = Map::new();
    obj.insert("ts".to_string(), json!(crate::db::repo::now_iso8601()));
    obj.insert("level".to_string(), json!(level.as_str()));
    obj.insert("event".to_string(), json!(event));
    if let Some(b) = batch_ref {
        obj.insert("batch_ref".to_string(), json!(b));
    }
    if let Some(c) = code {
        obj.insert("error_code".to_string(), json!(c.as_str()));
        obj.insert("error_zh".to_string(), json!(c.zh()));
    }
    if !message.is_empty() {
        obj.insert("message".to_string(), json!(message));
    }
    for (k, v) in extra {
        obj.insert((*k).to_string(), v.clone());
    }
    let line = serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|e| {
        format!(
            "{{\"level\":\"error\",\"event\":\"log.serialize_failed\",\"message\":\"{}\"}}",
            e
        )
    });
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{}", line);
    let _ = stderr.flush();
}

/// 运行摘要：一次命令跑完的计量结果，落状态库、`regval status` 可见。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    /// 跑的是哪个子命令
    pub command: String,
    /// 批次号
    pub batch_ref: String,
    /// 来源表张数
    pub tables: usize,
    /// 校验行数
    pub total_rows: usize,
    /// 有问题的行数
    pub rows_with_issues: usize,
    /// 问题总数
    pub issue_count: usize,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// 开始时间
    pub started_at: String,
    /// 结束时间
    pub finished_at: String,
}

impl RunSummary {
    /// 摘要的日志字段（写进 JSON 行日志的附加位）
    pub fn log_fields(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("command", json!(self.command)),
            ("tables", json!(self.tables)),
            ("total_rows", json!(self.total_rows)),
            ("rows_with_issues", json!(self.rows_with_issues)),
            ("issue_count", json!(self.issue_count)),
            ("elapsed_ms", json!(self.elapsed_ms)),
        ]
    }

    /// 一行可读摘要（给人看：`regval status` 用）
    pub fn to_line(&self) -> String {
        format!(
            "{} · {} · 表 {} 张 · 行 {} · 问题 {} 项 · 耗时 {} ms · {}",
            self.finished_at,
            self.command,
            self.tables,
            self.total_rows,
            self.issue_count,
            self.elapsed_ms,
            self.batch_ref
        )
    }
}

/// 一组日志附加字段（`BTreeMap` 保序）
pub type Extra = BTreeMap<String, Value>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 级别解析大小写不敏感() {
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert!(Level::parse("verbose").is_none());
    }

    #[test]
    fn 运行摘要可序列化且字段齐() {
        let s = RunSummary {
            command: "validate".to_string(),
            batch_ref: "mini-abc".to_string(),
            tables: 1,
            total_rows: 6,
            rows_with_issues: 2,
            issue_count: 2,
            elapsed_ms: 12,
            started_at: "2026-11-06 08:00:00".to_string(),
            finished_at: "2026-11-06 08:00:12".to_string(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["command"], "validate");
        assert_eq!(v["total_rows"], 6);
        assert_eq!(v["issue_count"], 2);
        let line = s.to_line();
        assert!(line.contains("mini-abc"));
        assert!(line.contains("问题 2 项"));
    }

    #[test]
    fn 日志附加字段保序可复现() {
        let mut extra = Extra::new();
        extra.insert("b".to_string(), json!(2));
        extra.insert("a".to_string(), json!(1));
        let keys: Vec<&String> = extra.keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
    }
}
