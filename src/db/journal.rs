//! 恢复日志（append-only 边车文件）—— 状态库损坏时**唯一**还能指望的东西。
//!
//! # 为什么需要它
//!
//! 状态库是**单文件 SQLite**。文件一旦损坏（磁盘掉电、文件被截断、被别的程序写坏），
//! 库里的一切都读不出来 —— 包括「哪些行已经报给网关了」这条事实。
//! 少了这条事实的后果很具体：经办人重跑一遍上报，引擎以为这些行没报过，
//! **又推了一遍** —— 网关那边就成了两条上报。
//!
//! 所以上报这条线上的关键事实（幂等键、回执 ID、批次提交标记）除了落进状态库，
//! 还要**另写一份到状态库旁边**。这份就是恢复日志：纯追加、每行一条 JSON、
//! 不依赖 SQLite，因此状态库坏掉时它通常还活着。
//!
//! # 它记什么
//!
//! 只记**上报侧的事实**（恢复时最要紧的那部分）与**批次提交标记**：
//!
//! | 记录种类 | 记什么 | 为什么 |
//! |---|---|---|
//! | `receipt` | 幂等键 + 修订号 + 回执 ID + 行键 + 批次 | **幂等键 ↔ 回执 ID 一一对应**的原始凭据 |
//! | `submission` | 幂等键 + 修订号 + 状态（被退回 / 待重试） | 没拿到回执的行也要留痕，重跑才知道重推哪几行 |
//! | `batch_commit` | 批次号 + 提交时刻 | **批次的一致性标记**：这批跑到最后了 |
//!
//! **不记**：患者可识别明细、指标取值、任何一行的业务内容。
//! 审计链（`audit_log` 表）记的是「谁在何时确认了哪一行」，
//! 恢复日志记的是「哪些幂等键对应哪个回执」—— 两者都只到行级与科室级。
//!
//! # 「最后一个一致状态」怎么定
//!
//! 一批的上报可能跑到一半就断了。判断「这批到底算不算跑完」的依据是
//! **`batch_commit` 标记**：标记在，批次就是完整的；标记不在，这批就是**半截的**。
//! 恢复时只恢复到最后一条 `batch_commit` 所属的批次 ——
//! 那之后的记录属于「跑了但没跑完」的批次，报告里单独列为**未恢复**（丢了什么）。
//!
//! # 只追加
//!
//! 打开方式是 `append(true)`：写不进已有内容，只能往后接。
//! 崩溃时最后一行可能是半截的 JSON —— 读的时候**跳过并计数**，不当成致命错误：
//! 断电时最后一行没写完是常事，为此整份恢复日志都不可信才是真事故。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{EngineError, ErrorCode};

/// 记录种类：拿到回执（已上报）
pub const KIND_RECEIPT: &str = "receipt";
/// 记录种类：一次提交（被退回 / 待重试等没有回执的结局）
pub const KIND_SUBMISSION: &str = "submission";
/// 记录种类：批次提交标记（这批跑到最后了）
pub const KIND_BATCH_COMMIT: &str = "batch_commit";

/// 一条恢复日志记录（一行 JSON）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord {
    /// 序号（从 1 起，追加顺序）
    #[serde(default)]
    pub seq: u64,
    /// 记录时间（`YYYY-MM-DD HH:mm:ss`）
    pub ts: String,
    /// 记录种类（见本模块的 KIND_* 常量）
    pub kind: String,
    /// 批次号
    #[serde(default)]
    pub batch_ref: String,
    /// 行键
    #[serde(default)]
    pub row_uid: String,
    /// 幂等键
    #[serde(default)]
    pub idempotency_key: String,
    /// 修订号
    #[serde(default)]
    pub revision: i64,
    /// 回执 ID
    #[serde(default)]
    pub receipt_id: String,
    /// 行状态（已上报 / 被退回 / 待重试 …）
    #[serde(default)]
    pub status: String,
    /// 补充说明
    #[serde(default)]
    pub detail: String,
}

impl JournalRecord {
    /// 造一条「拿到回执」的记录。
    pub fn receipt(
        batch_ref: &str,
        row_uid: &str,
        idempotency_key: &str,
        revision: i64,
        receipt_id: &str,
        status: &str,
    ) -> JournalRecord {
        JournalRecord {
            seq: 0,
            ts: crate::db::repo::now_iso8601(),
            kind: KIND_RECEIPT.to_string(),
            batch_ref: batch_ref.trim().to_string(),
            row_uid: row_uid.trim().to_string(),
            idempotency_key: idempotency_key.trim().to_string(),
            revision,
            receipt_id: receipt_id.trim().to_string(),
            status: status.trim().to_string(),
            detail: String::new(),
        }
    }

    /// 造一条「提交但没有回执」的记录（被退回 / 待重试）。
    pub fn submission(
        batch_ref: &str,
        row_uid: &str,
        idempotency_key: &str,
        revision: i64,
        status: &str,
        detail: &str,
    ) -> JournalRecord {
        JournalRecord {
            seq: 0,
            ts: crate::db::repo::now_iso8601(),
            kind: KIND_SUBMISSION.to_string(),
            batch_ref: batch_ref.trim().to_string(),
            row_uid: row_uid.trim().to_string(),
            idempotency_key: idempotency_key.trim().to_string(),
            revision,
            receipt_id: String::new(),
            status: status.trim().to_string(),
            detail: detail.trim().to_string(),
        }
    }

    /// 造一条「批次提交标记」。
    pub fn batch_commit(batch_ref: &str, detail: &str) -> JournalRecord {
        JournalRecord {
            seq: 0,
            ts: crate::db::repo::now_iso8601(),
            kind: KIND_BATCH_COMMIT.to_string(),
            batch_ref: batch_ref.trim().to_string(),
            row_uid: String::new(),
            idempotency_key: String::new(),
            revision: 0,
            receipt_id: String::new(),
            status: String::new(),
            detail: detail.trim().to_string(),
        }
    }
}

/// 恢复日志的路径：**状态库旁边**的同名 `.journal` 文件。
///
/// 放在旁边而不是放进库里 —— 放进去就一起坏掉了，恢复就没有意义了。
pub fn journal_path(state_db: &Path) -> PathBuf {
    let mut name = state_db
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "state.db".to_string());
    name.push_str(".journal");
    state_db.with_file_name(name)
}

/// 读回来的恢复日志（含读不出来的行数）。
#[derive(Debug, Clone, Default)]
pub struct JournalContents {
    /// 能解析的记录（按追加顺序）
    pub records: Vec<JournalRecord>,
    /// 读不出来 / 被跳过的行数（崩溃时最后一行没写完是常事）
    pub skipped_lines: usize,
}

/// 追加一条记录（**只追加**）。
///
/// 打开方式是 `append(true)`：写不进已有内容，只能往后接。
/// 写完立刻 `flush` —— 崩溃时这条记录要么完整在，要么一行都没有。
pub fn append(path: &Path, record: &JournalRecord) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EngineError::at(
                    ErrorCode::Io,
                    format!("恢复日志目录创建失败：{}", e),
                    parent.display().to_string(),
                )
            })?;
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("恢复日志打开失败：{}", e),
                path.display().to_string(),
            )
        })?;
    let mut line = serde_json::to_string(record).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("恢复日志序列化失败：{}", e),
            path.display().to_string(),
        )
    })?;
    line.push('\n');
    file.write_all(line.as_bytes()).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("恢复日志写入失败：{}", e),
            path.display().to_string(),
        )
    })?;
    file.flush().map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("恢复日志落盘失败：{}", e),
            path.display().to_string(),
        )
    })?;
    Ok(())
}

/// 读回整份恢复日志。
///
/// 读不出来的行**跳过并计数**，不当成致命错误 —— 断电时最后一行没写完是常事。
pub fn read_all(path: &Path) -> Result<JournalContents, EngineError> {
    let mut out = JournalContents::default();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(EngineError::at(
                ErrorCode::Io,
                format!("恢复日志读取失败：{}", e),
                path.display().to_string(),
            ))
        }
    };
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                out.skipped_lines += 1;
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalRecord>(trimmed) {
            Ok(rec) => out.records.push(rec),
            // 半截的 JSON（崩溃时最后一行）→ 跳过并计数，不让整份日志作废
            Err(_) => out.skipped_lines += 1,
        }
    }
    Ok(out)
}

/// 最后一个**一致批次** —— 最后一条 `batch_commit` 标记所属的批次。
///
/// 标记之后���记录属于「跑了但没跑完」的批次，恢复时不算。
pub fn last_consistent_batch(records: &[JournalRecord]) -> Option<String> {
    records
        .iter()
        .rev()
        .find(|r| r.kind == KIND_BATCH_COMMIT && !r.batch_ref.trim().is_empty())
        .map(|r| r.batch_ref.trim().to_string())
}

/// 一致批次之后、还没提交的批次（这些是**丢了**的部分）。
pub fn uncommitted_batches(records: &[JournalRecord]) -> Vec<String> {
    let Some(last) = last_consistent_batch(records) else {
        return records
            .iter()
            .map(|r| r.batch_ref.trim().to_string())
            .filter(|b| !b.is_empty())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    };
    let mut out: Vec<String> = Vec::new();
    for rec in records.iter().rev() {
        if rec.kind == KIND_BATCH_COMMIT && rec.batch_ref.trim() == last {
            break;
        }
        let batch = rec.batch_ref.trim();
        if !batch.is_empty() && !out.iter().any(|b| b == batch) {
            out.push(batch.to_string());
        }
    }
    out.reverse();
    out
}

/// 下一条记录的序号（读一遍已有的，取最大序号 + 1）。
pub fn next_seq(records: &[JournalRecord]) -> u64 {
    records.iter().map(|r| r.seq).max().unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 追加后能读回且序号递增() {
        let dir = tempfile::tempdir().unwrap();
        let path = journal_path(&dir.path().join("state.db"));
        assert_eq!(path.file_name().unwrap(), "state.db.journal");

        let mut rec = JournalRecord::receipt("b-1", "r-1", "idem-1", 1, "RCPT-1", "已上报");
        rec.seq = next_seq(&[]);
        append(&path, &rec).unwrap();
        let mut rec2 = JournalRecord::batch_commit("b-1", "这批跑完了");
        rec2.seq = rec.seq + 1;
        append(&path, &rec2).unwrap();

        let contents = read_all(&path).unwrap();
        assert_eq!(contents.records.len(), 2);
        assert_eq!(contents.skipped_lines, 0);
        assert_eq!(contents.records[0].receipt_id, "RCPT-1");
        assert_eq!(contents.records[1].kind, KIND_BATCH_COMMIT);
    }

    #[test]
    fn 半截的行被跳过并计数不当致命错误() {
        let dir = tempfile::tempdir().unwrap();
        let path = journal_path(&dir.path().join("state.db"));
        let mut rec = JournalRecord::batch_commit("b-1", "跑完了");
        rec.seq = 1;
        append(&path, &rec).unwrap();
        // 模拟断电：最后一行只写了一半
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"seq\":2,\"kind\":\"receipt\",\"receipt").unwrap();
        drop(file);

        let contents = read_all(&path).unwrap();
        assert_eq!(contents.records.len(), 1, "完整的那条仍读得到");
        assert_eq!(contents.skipped_lines, 1, "半截的那条被计数");
    }

    #[test]
    fn 一致批次取最后一条提交标记() {
        let recs = vec![
            JournalRecord::receipt("b-1", "r-1", "i-1", 1, "RCPT-1", "已上报"),
            JournalRecord::batch_commit("b-1", ""),
            JournalRecord::receipt("b-2", "r-2", "i-2", 1, "RCPT-2", "已上报"),
        ];
        assert_eq!(last_consistent_batch(&recs), Some("b-1".to_string()));
        assert_eq!(uncommitted_batches(&recs), vec!["b-2".to_string()]);
    }

    #[test]
    fn 文件不存在时读回空内容而不是报错() {
        let dir = tempfile::tempdir().unwrap();
        let contents = read_all(&journal_path(&dir.path().join("nope.db"))).unwrap();
        assert!(contents.records.is_empty());
        assert_eq!(contents.skipped_lines, 0);
    }
}
