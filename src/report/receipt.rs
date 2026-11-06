//! 回执台账：留档与第三方核对用。
//!
//! # 这份产物回答什么问题
//!
//! 「这一批到底报上去了没有、网关是怎么说的、凭哪条身份报的」——
//! 一行**一次提交**（按 `幂等键 + 修订号` 计一条），带上网关给的回执号、受理时间、
//! 错误码与错误字段。
//!
//! 它和未上报名单（[`super::pending`]）是**并排**的两份：
//! 未上报名单回答「还剩谁没报」，回执台账回答「已经报的那部分，凭哪条身份、
//! 报的是哪一版数据、网关认了没有」。
//!
//! # 为什么按「幂等键 + 修订号」一行一条
//!
//! 幂等键是**业务身份**（机构｜报告期｜指标｜科室｜业务主键），
//! 修订号是**这次报的是哪一版数据**。同一个业务行订正后重报：
//! 幂等键不变、修订号 +1、载荷摘要变 —— 网关那边是两次受理、两个回执号。
//! 按「幂等键 + 修订号」记一条，两边都对得上；
//! 只按幂等键记，会把两次受理并成一条，回执号对不上。
//!
//! # 交叉核对怎么做
//!
//! `cross_check` 段给出第三方核对要的结论（**不是**「看起来没问题」）：
//!
//! | 字段 | 含义 | 期望 |
//! |---|---|---|
//! | `receipt_pairs_one_to_one` | `(幂等键, 修订号) ↔ 回执号` 是否一一对应 | `true` |
//! | `accepted_without_receipt` | 已上报却没有回执号的条目数 | `0` |
//! | `receipt_without_submission` | 有回执号却找不到对应提交记录的条目数 | `0` |
//! | `state_disagreements` | 上报状态与行状态对不上的行数 | `0` |
//! | `conflicts` | 逐条列出对不上的地方（哪一行、哪一头） | 空 |
//!
//! # 只读，不推进任何状态
//!
//! 本模块**不写任何表、不改任何状态**。行状态只由网关回执驱动 ——
//! 这里只是把已经落库的事实摆出来给人核对。
//!
//! # 字段边界
//!
//! 产物只有指标级与科室级字段（行键 / 幂等键 / 载荷摘要 / 回执号 / 错误码 …），
//! **不含患者可识别明细**。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::db::repo::{ReceiptRow, SubmissionRecord};
use crate::push::receipt::PUSH_ISSUE_RULE_ID;
use crate::push::status;
use crate::rules::model::Issue;

/// 回执台账版本。改动产物结构时升版本。
pub const RECEIPT_SCHEMA_VERSION: &str = "regval.receipts/1.0.0";

/// CSV 列定义（**表头与顺序固定** —— 第三方按这套列核对，顺序变了就核不上了）。
pub const CSV_HEADER: [&str; 17] = [
    "批次",
    "行键",
    "幂等键",
    "修订号",
    "载荷摘要",
    "上报状态",
    "回执号",
    "网关状态",
    "受理时间",
    "错误码",
    "错误字段",
    "网关说明",
    "尝试次数",
    "提交时间",
    "更新时间",
    "行状态",
    "口径版本",
];

/// 这一行在行状态机里没有状态记录。
///
/// 不是「数据丢了」，是「这一行没进过状态机」（合成测试数据、或推的行没落进
/// `rows` 表）。写出来而不是留空 —— 留空会被读成「状态是空的」。
pub const ROW_STATE_UNRECORDED: &str = "未记录";

/// 回执台账里的一条（一次提交一条）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptExportRow {
    /// 批次号
    pub batch_ref: String,
    /// 行键
    pub row_uid: String,
    /// 幂等键（业务身份）
    pub idempotency_key: String,
    /// 修订号（这次报的是哪一版数据）
    pub revision: i64,
    /// 载荷摘要（这一版数据的指纹）
    pub payload_digest: String,
    /// 上报状态（已上报 / 被退回 / 待重试 …；取值见 [`crate::push::status`]）
    pub push_status: String,
    /// 网关回执号（没有受理则空串 —— 退回与待重试都没有回执）
    pub receipt_id: String,
    /// 网关给的状态（受理时是网关原话，如 `accepted`）
    pub gateway_status: String,
    /// 受理时间
    pub accepted_at: String,
    /// 机器可读错误码（无则空串）
    pub error_code: String,
    /// 网关点名的字段（无则空串）
    pub error_field: String,
    /// 人话说明（退回时是网关原话 + 错误码 + 字段名）
    pub message: String,
    /// 推送尝试次数
    pub attempt_count: i64,
    /// 提交时间
    pub submitted_at: String,
    /// 最近更新时间
    pub updated_at: String,
    /// 行状态机里的当前状态（没有记录时是「未记录」）
    pub row_state: String,
    /// 这一批结论所依据的口径版本（行级版本未记录时为空串）
    pub rule_version: String,
}

impl ReceiptExportRow {
    /// 这一条是不是「网关已受理」
    pub fn is_accepted(&self) -> bool {
        status::is_accepted(&self.push_status)
    }
}

/// 回执台账的计数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiptCounts {
    /// 条目数（一次提交一条；订正重报算两条）
    pub entries: usize,
    /// 涉及的行数（行键去重）
    pub rows: usize,
    /// 有回执号的条目数
    pub receipts: usize,
    /// 订正后重报的行数（同一行有多个修订号）
    pub revised_rows: usize,
    /// 没有行状态记录的条目数
    pub entries_without_row_state: usize,
    /// 按上报状态计数（按条目）
    pub by_status: BTreeMap<String, usize>,
    /// 按行状态计数（按行，一行只计一次，取该行最新修订的状态）
    pub by_row_state: BTreeMap<String, usize>,
}

/// 一处对不上的地方（交叉核对的明细）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCheckConflict {
    /// 冲突类别
    pub kind: String,
    /// 行键（能定位到行时）
    pub row_uid: String,
    /// 幂等键
    pub idempotency_key: String,
    /// 修订号
    pub revision: i64,
    /// 说明（人话）
    pub detail: String,
}

/// 交叉核对结论。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrossCheck {
    /// `(幂等键, 修订号)` 的个数
    pub identity_pairs: usize,
    /// 不重复的回执号个数
    pub distinct_receipt_ids: usize,
    /// `(幂等键, 修订号) ↔ 回执号` 是否一一对应
    pub receipt_pairs_one_to_one: bool,
    /// 已上报却没有回执号的条目数（应恒为 0）
    pub accepted_without_receipt: usize,
    /// 有回执号却找不到对应提交记录的条目数（应恒为 0）
    pub receipt_without_submission: usize,
    /// 上报状态与行状态对不上的行数（应恒为 0）
    pub state_disagreements: usize,
    /// 逐条冲突
    pub conflicts: Vec<CrossCheckConflict>,
}

impl CrossCheck {
    /// 有没有对不上的地方
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
            && self.accepted_without_receipt == 0
            && self.receipt_without_submission == 0
            && self.state_disagreements == 0
    }
}

/// 回执台账。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptExport {
    pub schema_version: String,
    pub generated_at: String,
    pub batch_ref: String,
    pub counts: ReceiptCounts,
    pub cross_check: CrossCheck,
    /// 台账条目（按幂等键、修订号排序）
    pub rows: Vec<ReceiptExportRow>,
}

impl ReceiptExport {
    /// 条目数
    pub fn entry_count(&self) -> usize {
        self.rows.len()
    }

    /// 某一行（按行键查最新修订的那条）
    pub fn row(&self, row_uid: &str) -> Option<&ReceiptExportRow> {
        self.rows
            .iter()
            .filter(|r| r.row_uid == row_uid)
            .max_by_key(|r| r.revision)
    }
}

/// 按批次组装回执台账。
///
/// - `submissions`：本批次的提交记录（一次提交一条，含订正后的历次修订）；
/// - `receipts`：本批次的网关回执（只有受理才有）；
/// - `states`：行键 → 行状态机里的当前状态（**由状态机模块读出**后传进来 ——
///   这一层不碰状态表，状态推进只有一个入口）；
/// - `push_notes`：行键 → 退回说明（从上报退回问题里取，网关原话）；
/// - `rule_versions`：行键 → 这一批结论所依据的口径版本。
///
/// 本函数**不碰数据库**，只拼装 —— 读数据的事由调用方做。
pub fn build(
    batch_ref: &str,
    submissions: &[SubmissionRecord],
    receipts: &[ReceiptRow],
    states: &BTreeMap<String, String>,
    push_notes: &BTreeMap<String, String>,
    rule_versions: &BTreeMap<String, String>,
) -> ReceiptExport {
    // 回执索引：(幂等键, 修订号) → 回执
    let mut receipt_index: BTreeMap<(String, i64), &ReceiptRow> = BTreeMap::new();
    let mut receipt_ids: BTreeSet<String> = BTreeSet::new();
    for r in receipts {
        receipt_index.insert((r.idempotency_key.clone(), r.revision), r);
        receipt_ids.insert(r.receipt_id.clone());
    }

    let mut rows: Vec<ReceiptExportRow> = Vec::new();
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut entries_without_row_state = 0usize;
    let mut identity_pairs: BTreeSet<(String, i64)> = BTreeSet::new();
    let mut accepted_without_receipt = 0usize;
    let mut conflicts: Vec<CrossCheckConflict> = Vec::new();

    for s in submissions {
        if s.batch_ref != batch_ref {
            continue;
        }
        let receipt = receipt_index.get(&(s.idempotency_key.clone(), s.revision)).copied();
        let row_state = states
            .get(&s.row_uid)
            .cloned()
            .unwrap_or_else(|| ROW_STATE_UNRECORDED.to_string());
        if row_state == ROW_STATE_UNRECORDED {
            entries_without_row_state += 1;
        }
        *by_status.entry(s.status.clone()).or_insert(0) += 1;
        identity_pairs.insert((s.idempotency_key.clone(), s.revision));

        let receipt_id = receipt.map(|r| r.receipt_id.clone()).unwrap_or_default();
        if receipt.is_none() && status::is_accepted(&s.status) {
            accepted_without_receipt += 1;
            conflicts.push(CrossCheckConflict {
                kind: "已上报但没有回执号".to_string(),
                row_uid: s.row_uid.clone(),
                idempotency_key: s.idempotency_key.clone(),
                revision: s.revision,
                detail: format!(
                    "这一条的上报状态是「{}」，但库里找不到对应的回执记录 —— 回执台账与网关对不上",
                    s.status
                ),
            });
        }

        rows.push(ReceiptExportRow {
            batch_ref: s.batch_ref.clone(),
            row_uid: s.row_uid.clone(),
            idempotency_key: s.idempotency_key.clone(),
            revision: s.revision,
            payload_digest: s.payload_digest.clone(),
            push_status: s.status.clone(),
            gateway_status: receipt.map(|r| r.status.clone()).unwrap_or_default(),
            accepted_at: receipt.map(|r| r.accepted_at.clone()).unwrap_or_default(),
            error_code: if s.last_error_code.is_empty() {
                receipt.map(|r| r.error_code.clone()).unwrap_or_default()
            } else {
                s.last_error_code.clone()
            },
            error_field: if s.last_error_field.is_empty() {
                receipt.map(|r| r.error_field.clone()).unwrap_or_default()
            } else {
                s.last_error_field.clone()
            },
            message: push_notes.get(&s.row_uid).cloned().unwrap_or_else(|| {
                receipt
                    .map(|r| r.message.clone())
                    .unwrap_or_default()
            }),
            attempt_count: s.attempt_count,
            submitted_at: s.created_at.clone(),
            updated_at: s.updated_at.clone(),
            receipt_id,
            row_state,
            rule_version: rule_versions.get(&s.row_uid).cloned().unwrap_or_default(),
        });
    }

    // 回执 ↔ 提交：双向对账
    let mut receipt_without_submission = 0usize;
    for r in receipts {
        let matched = identity_pairs.contains(&(r.idempotency_key.clone(), r.revision));
        if !matched {
            receipt_without_submission += 1;
            conflicts.push(CrossCheckConflict {
                kind: "有回执但找不到对应提交记录".to_string(),
                row_uid: r.row_uid.clone(),
                idempotency_key: r.idempotency_key.clone(),
                revision: r.revision,
                detail: format!("回执 {} 在提交记录里找不到同身份的那一条", r.receipt_id),
            });
        }
    }

    // 同一个回执号对应多条提交 / 同一个身份对应多个回执号 —— 一一对应不成立
    let mut receipt_by_id: BTreeMap<&str, Vec<&ReceiptRow>> = BTreeMap::new();
    for r in receipts {
        receipt_by_id.entry(r.receipt_id.as_str()).or_default().push(r);
    }
    // 同一个回执号挂在两条不同提交上 —— 一一对应不成立。
    // （同一幂等键的不同修订号各有各的回执号 —— 那是订正重报，不算冲突。）
    for (receipt_id, list) in &receipt_by_id {
        if list.len() > 1 {
            conflicts.push(CrossCheckConflict {
                kind: "同一个回执号挂在多条提交上".to_string(),
                row_uid: list[0].row_uid.clone(),
                idempotency_key: list[0].idempotency_key.clone(),
                revision: list[0].revision,
                detail: format!("回执 {} 对应了 {} 条提交记录", receipt_id, list.len()),
            });
        }
    }

    let receipt_pairs_one_to_one = !conflicts.iter().any(|c| {
        c.kind == "同一个回执号挂在多条提交上"
            || c.kind == "有回执但找不到对应提交记录"
            || c.kind == "已上报但没有回执号"
    });

    // 行状态 ↔ 上报状态：同一行最新修订的上报状态应与行状态机一致
    let mut latest_by_row: BTreeMap<&str, &ReceiptExportRow> = BTreeMap::new();
    for r in &rows {
        let replace = match latest_by_row.get(r.row_uid.as_str()) {
            Some(prev) => r.revision >= prev.revision,
            None => true,
        };
        if replace {
            latest_by_row.insert(r.row_uid.as_str(), r);
        }
    }
    let mut state_disagreements = 0usize;
    let mut by_row_state: BTreeMap<String, usize> = BTreeMap::new();
    for (row_uid, r) in &latest_by_row {
        *by_row_state
            .entry(r.row_state.clone())
            .or_insert(0) += 1;
        if r.row_state == ROW_STATE_UNRECORDED {
            continue;
        }
        let agree = match r.push_status.as_str() {
            s if status::is_accepted(s) => r.row_state == crate::state::machine::REPORTED,
            s if status::is_rejected(s) => r.row_state == crate::state::machine::REJECTED,
            s if status::is_retryable(s) => r.row_state == crate::state::machine::RETRY_PENDING,
            _ => true,
        };
        if !agree {
            state_disagreements += 1;
            conflicts.push(CrossCheckConflict {
                kind: "上报状态与行状态对不上".to_string(),
                row_uid: (*row_uid).to_string(),
                idempotency_key: r.idempotency_key.clone(),
                revision: r.revision,
                detail: format!(
                    "上报状态是「{}」，行状态是「{}」—— 两处对不上，台账不能直接留档",
                    r.push_status, r.row_state
                ),
            });
        }
    }

    // 涉及的行数与订正重报的行数
    let mut row_uids: BTreeSet<&str> = BTreeSet::new();
    let mut revisions_by_row: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    for r in &rows {
        row_uids.insert(r.row_uid.as_str());
        revisions_by_row
            .entry(r.row_uid.as_str())
            .or_default()
            .insert(r.revision);
    }
    let revised_rows = revisions_by_row
        .values()
        .filter(|revs| revs.len() > 1)
        .count();

    let counts = ReceiptCounts {
        entries: rows.len(),
        rows: row_uids.len(),
        receipts: rows.iter().filter(|r| !r.receipt_id.is_empty()).count(),
        revised_rows,
        entries_without_row_state,
        by_status,
        by_row_state,
    };

    let cross_check = CrossCheck {
        identity_pairs: identity_pairs.len(),
        distinct_receipt_ids: receipt_ids.len(),
        receipt_pairs_one_to_one,
        accepted_without_receipt,
        receipt_without_submission,
        state_disagreements,
        conflicts,
    };

    ReceiptExport {
        schema_version: RECEIPT_SCHEMA_VERSION.to_string(),
        generated_at: crate::db::repo::now_iso8601(),
        batch_ref: batch_ref.to_string(),
        counts,
        cross_check,
        rows,
    }
}

/// 从上报退回问题里取每行的退回说明（行键 → 网关原话）。
///
/// 退回原因落在 `R-PUSH-GATEWAY` 那条问题上（见 [`crate::push::receipt::push_issue`]）——
/// 校验规则的问题每次重跑都要重算，上报退回留下的不能被重跑抹掉。
pub fn push_notes_by_row(issues: &[Issue]) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for issue in issues {
        if issue.rule_id != PUSH_ISSUE_RULE_ID {
            continue;
        }
        out.insert(issue.row_uid.clone(), issue.message.clone());
    }
    out
}

/// 回执台账 → CSV 文本。
///
/// **一行一次提交**（订正重报出两行），列名与顺序固定 —— 第三方按这套列核对。
pub fn to_csv(export: &ReceiptExport) -> Result<String, csv::Error> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(CSV_HEADER)?;
    for row in &export.rows {
        wtr.write_record([
            row.batch_ref.clone(),
            row.row_uid.clone(),
            row.idempotency_key.clone(),
            row.revision.to_string(),
            row.payload_digest.clone(),
            row.push_status.clone(),
            row.receipt_id.clone(),
            row.gateway_status.clone(),
            row.accepted_at.clone(),
            row.error_code.clone(),
            row.error_field.clone(),
            row.message.clone(),
            row.attempt_count.to_string(),
            row.submitted_at.clone(),
            row.updated_at.clone(),
            row.row_state.clone(),
            row.rule_version.clone(),
        ])?;
    }
    wtr.flush()?;
    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 回执台账 → JSON 文本
pub fn to_json(export: &ReceiptExport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(export)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submission(
        row_uid: &str,
        idem: &str,
        revision: i64,
        status: &str,
    ) -> SubmissionRecord {
        SubmissionRecord {
            batch_ref: "batch-a".to_string(),
            row_uid: row_uid.into(),
            idempotency_key: idem.into(),
            payload_digest: format!("digest-{}-{}", idem, revision),
            revision,
            status: status.into(),
            attempt_count: 1,
            last_error_code: String::new(),
            last_error_field: String::new(),
            created_at: "2026-09-06 10:00:00".to_string(),
            updated_at: "2026-09-06 10:00:05".to_string(),
        }
    }

    fn receipt(idem: &str, revision: i64, receipt_id: &str, row_uid: &str) -> ReceiptRow {
        ReceiptRow {
            receipt_id: receipt_id.into(),
            idempotency_key: idem.into(),
            revision,
            batch_ref: "batch-a".to_string(),
            row_uid: row_uid.into(),
            status: "accepted".to_string(),
            accepted_at: "2026-09-06 10:00:03".to_string(),
            error_code: String::new(),
            error_field: String::new(),
            message: String::new(),
        }
    }

    #[test]
    fn 台账按一次提交一条且字段齐全() {
        let subs = vec![
            submission("row-1", "idem-1", 1, "已上报"),
            submission("row-2", "idem-2", 1, "被退回"),
        ];
        let receipts = vec![receipt("idem-1", 1, "RCPT-1", "row-1")];
        let mut states = BTreeMap::new();
        states.insert("row-1".to_string(), "已上报".to_string());
        states.insert("row-2".to_string(), "被退回".to_string());
        let export = build("batch-a", &subs, &receipts, &states, &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.counts.entries, 2);
        assert_eq!(export.counts.rows, 2);
        assert_eq!(export.counts.receipts, 1);
        assert_eq!(export.counts.by_status.get("已上报"), Some(&1));
        assert_eq!(export.counts.by_status.get("被退回"), Some(&1));
        assert_eq!(export.counts.by_row_state.get("已上报"), Some(&1));
        // 批次 / 回执号 / 幂等键 / 状态 / 时间 / 行键 六样都要有
        assert_eq!(export.rows[0].batch_ref, "batch-a");
        assert_eq!(export.rows[0].receipt_id, "RCPT-1");
        assert_eq!(export.rows[0].idempotency_key, "idem-1");
        assert_eq!(export.rows[0].push_status, "已上报");
        assert_eq!(export.rows[0].accepted_at, "2026-09-06 10:00:03");
        assert_eq!(export.rows[0].row_uid, "row-1");
        // 被退回的那条没有回执号
        assert!(export.rows[1].receipt_id.is_empty());
        assert!(export.cross_check.is_clean());
    }

    #[test]
    fn 订正重报按两条记且回执号各归各的() {
        let subs = vec![
            submission("row-1", "idem-1", 1, "已上报"),
            submission("row-1", "idem-1", 2, "已上报"),
        ];
        let receipts = vec![
            receipt("idem-1", 1, "RCPT-1", "row-1"),
            receipt("idem-1", 2, "RCPT-2", "row-1"),
        ];
        let mut states = BTreeMap::new();
        states.insert("row-1".to_string(), "已上报".to_string());
        let export = build("batch-a", &subs, &receipts, &states, &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.counts.entries, 2);
        assert_eq!(export.counts.rows, 1);
        assert_eq!(export.counts.revised_rows, 1);
        assert_eq!(export.counts.receipts, 2);
        assert_eq!(export.cross_check.distinct_receipt_ids, 2);
        assert!(export.cross_check.receipt_pairs_one_to_one);
        // 幂等键相同、修订号不同、回执号不同 —— 三者各归各的
        assert_eq!(export.rows[0].idempotency_key, export.rows[1].idempotency_key);
        assert_ne!(export.rows[0].receipt_id, export.rows[1].receipt_id);
        assert_ne!(export.rows[0].payload_digest, export.rows[1].payload_digest);
    }

    #[test]
    fn 已上报却没有回执号时报出来() {
        let subs = vec![submission("row-1", "idem-1", 1, "已上报")];
        let mut states = BTreeMap::new();
        states.insert("row-1".to_string(), "已上报".to_string());
        let export = build("batch-a", &subs, &[], &states, &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.cross_check.accepted_without_receipt, 1);
        assert!(!export.cross_check.is_clean());
        assert_eq!(export.cross_check.conflicts.len(), 1);
        assert!(export.cross_check.conflicts[0].detail.contains("找不到对应的回执"));
    }

    #[test]
    fn 有回执却找不到提交记录时报出来() {
        let subs: Vec<SubmissionRecord> = Vec::new();
        let receipts = vec![receipt("idem-9", 1, "RCPT-9", "row-9")];
        let export = build("batch-a", &subs, &receipts, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.cross_check.receipt_without_submission, 1);
        assert!(!export.cross_check.is_clean());
    }

    #[test]
    fn 上报状态与行状态对不上时报出来() {
        let subs = vec![submission("row-1", "idem-1", 1, "已上报")];
        let receipts = vec![receipt("idem-1", 1, "RCPT-1", "row-1")];
        let mut states = BTreeMap::new();
        // 行状态还停在「待确认」，上报状态却是「已上报」—— 两处对不上
        states.insert("row-1".to_string(), "待确认".to_string());
        let export = build("batch-a", &subs, &receipts, &states, &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.cross_check.state_disagreements, 1);
        assert!(!export.cross_check.is_clean());
        assert!(export.cross_check.conflicts[0].detail.contains("行状态"));
    }

    #[test]
    fn 别的批次的提交不混进来() {
        let mut other = submission("row-9", "idem-9", 1, "已上报");
        other.batch_ref = "batch-b".to_string();
        let subs = vec![submission("row-1", "idem-1", 1, "已上报"), other];
        let receipts = vec![receipt("idem-1", 1, "RCPT-1", "row-1")];
        let export = build("batch-a", &subs, &receipts, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());

        assert_eq!(export.counts.entries, 1);
        assert_eq!(export.rows[0].row_uid, "row-1");
    }

    #[test]
    fn csv_列定义固定且每行一条() {
        let subs = vec![
            submission("row-1", "idem-1", 1, "已上报"),
            submission("row-2", "idem-2", 1, "被退回"),
        ];
        let receipts = vec![receipt("idem-1", 1, "RCPT-1", "row-1")];
        let export = build("batch-a", &subs, &receipts, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        let csv_text = to_csv(&export).unwrap();

        let mut lines = csv_text.lines();
        let header = lines.next().unwrap();
        assert_eq!(header, CSV_HEADER.join(","));
        assert_eq!(lines.count(), 2);
        // 关键字段在行里
        assert!(csv_text.contains("idem-1"));
        assert!(csv_text.contains("RCPT-1"));
    }

    #[test]
    fn 退回说明从上报退回问题里取() {
        let issue = Issue {
            row_uid: "row-2".to_string(),
            rule_id: PUSH_ISSUE_RULE_ID.to_string(),
            issue_kind: "上报退回".to_string(),
            severity: "阻断".to_string(),
            message: "上报被退回：错误码 E_X · 字段 reporting_period".to_string(),
            basis: "院内卫健上报网关回执".to_string(),
            issue_key: "idem-2".to_string(),
            missing_columns: vec!["reporting_period".to_string()],
            error_code: "GATEWAY_REJECTED".to_string(),
        };
        let notes = push_notes_by_row(&[issue]);
        assert_eq!(notes.get("row-2").unwrap(), "上报被退回：错误码 E_X · 字段 reporting_period");

        let subs = vec![submission("row-2", "idem-2", 1, "被退回")];
        let export = build("batch-a", &subs, &[], &BTreeMap::new(), &notes, &BTreeMap::new());
        assert_eq!(export.rows[0].message, "上报被退回：错误码 E_X · 字段 reporting_period");
    }
}
