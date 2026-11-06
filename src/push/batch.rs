//! 批次汇总：**口径定义 + 汇总构造 + 落库**。
//!
//! # 汇总是什么
//!
//! 批次汇总回答一个问题：**这批数据整体算不算数**。
//!
//! 它不是另一个报表系统 —— 它就四行口径 + 几个批次计数，
//! 供两处使用：
//!
//! - `regval status`：经办人打开状态库第一眼看到「这批报到哪一步了」；
//! - **未上报名单**（催报对照）：哪些行还没报上去、为什么，等网关恢复的先排前面。
//!
//! # 汇总口径（固定四项，写死在这里）
//!
//! | 口径 | 定义 | 出处 |
//! |---|---|---|
//! | 已上报行数 | 网关已受理的行数（本次受理 + 幂等命中） | `submissions.status = 已上报` |
//! | 被退回行数 | 网关退回（4xx）的行数 | `submissions.status = 被退回` |
//! | 待重试行数 | 可重试失败（5xx / 超时 / 连不上）的行数 | `submissions.status = 待重试` |
//! | 问题项数 | 该批次问题条数（口径问题 + 上报退回问题） | `issues` 表 |
//!
//! **口径只在这里定义一次**，代码注释、`docs/architecture.md`、README 都引这一份。
//! 库层（[`crate::db::batch_summary`]）只负责把算好的值原样存下来，不另算一遍 ——
//! 两处各算一遍就会岔开，岔开了就出现「状态说报了 3 行、汇总说报了 2 行」。
//!
//! # 批末回滚：汇总回批前值，行级回执不回滚
//!
//! 上报是条件批量程序：第 i 步只在第 i-1 步成功后执行。
//! 第 k 步失败 → 第 k+1..N 步不执行 → 批末条件回滚触发 →
//! **批次汇总回到批前值**（批次计数与汇总值一起回），批次状态记「批末回滚」。
//!
//! 但**行级回执不回滚**：网关已经受理了那一行，那是**已经发生的外部事实**，
//! 本机库改不掉。回滚它等于假装没上报过 —— 而网关那边真撤不回来。
//!
//! 这条区分（回滚账，不回滚事实）在执行器里已经定了口径，
//! 这一层只是把它**落到库里**：批末回滚时落的汇总就是批前值。
//!
//! # 一次事务
//!
//! 批次计数、汇总值与批次状态在**一个事务**里写进本机库
//! （见 [`crate::db::batch_summary::Repo::save_batch_summary`]）。
//! 中途失败不留半截状态 —— 快照回到批前值。
//! 「半截的汇总比没有汇总更坏」：半截的汇总看着像一批干净的账，
//! 经办人会照着它去催科室。

use crate::db::batch_summary::BatchSummaryRecord;
use crate::db::repo::{Repo, RowPushState};
use crate::errors::EngineError;
use crate::push::executor::ExecutionReport;
use crate::push::status;

/// 批次状态：校验完成，还没上报。
pub const BATCH_STATUS_VALIDATED: &str = "已校验";
/// 批次状态：上报整批跑完（无回滚）。
pub const BATCH_STATUS_COMPLETED: &str = "上报完成";
/// 批次状态：批末条件回滚 —— 汇总回到批前值。
pub const BATCH_STATUS_ROLLED_BACK: &str = "批末回滚";
/// 批次状态：批次没整批跑完（有失败或未执行），但没触发回滚（续跑场景）。
pub const BATCH_STATUS_PARTIAL: &str = "部分完成";

/// 汇总口径的四项 —— 数字本身，全部来自本机库的权威状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchTotals {
    /// 已上报行数（受理 + 幂等命中）
    pub reported: usize,
    /// 被退回行数（4xx）
    pub rejected: usize,
    /// 待重试行数（5xx / 超时 / 连不上）
    pub retryable: usize,
    /// 问题项数（口径问题 + 上报退回问题）
    pub issue_count: usize,
}

impl BatchTotals {
    /// 一行可读说法（终端用）
    pub fn to_line(&self) -> String {
        format!(
            "已上报 {} 行 · 被退回 {} 行 · 待重试 {} 行 · 问题 {} 项",
            self.reported, self.rejected, self.retryable, self.issue_count
        )
    }
}

/// 按**行级上报状态**算四项口径。
///
/// 数据来自本机库 `submissions`（每行取最新修订号的那次提交）——
/// 这是**已经发生的事实**，不是执行器的自述。执行器说「我受理了 3 行」
/// 与库里「3 行的状态是已上报」对不上时，以库里的为准。
///
/// 被退回、待重试之外的状态（未发出 / 回执待人工核对 / 未执行）**不计入**这四项：
/// 它们既没报上去、也不在等网关恢复，混进来会让「还差多少」算不清。
///
/// 「幂等命中」算**已上报**：那一行此前确实报上去了，只是本轮没再推 ——
/// 库里的行状态落的是「已上报」（`src/push/receipt.rs`），
/// 执行器台账里的行状态落的是「幂等命中（已上报，不重复推送）」，两处都算已上报。
pub fn totals_from_rows(states: &[RowPushState], issue_count: usize) -> BatchTotals {
    let mut totals = BatchTotals {
        issue_count,
        ..BatchTotals::default()
    };
    for state in states {
        if status::is_accepted(&state.status) || state.status == status::IDEMPOTENT_HIT {
            totals.reported += 1;
        } else if status::is_rejected(&state.status) {
            totals.rejected += 1;
        } else if status::is_retryable(&state.status) {
            totals.retryable += 1;
        }
    }
    totals
}

/// 从已落库的汇总记录读回四项口径（未上报名单等产物用）。
pub fn totals_of(record: &BatchSummaryRecord) -> BatchTotals {
    BatchTotals {
        reported: record.reported.max(0) as usize,
        rejected: record.rejected.max(0) as usize,
        retryable: record.retryable.max(0) as usize,
        issue_count: record.issue_count.max(0) as usize,
    }
}

/// 按库里**当前**的行级状态重算汇总并落库（一次事务）。
///
/// 用在「这批的行级状态变了，但没跑完整套上报程序」的时候 ——
/// 比如刚跑完一次 `regval validate`：口径问题重算了，批次汇总也得跟着更新
/// （问题项数变了），否则 `regval status` 里的汇总与库里对不上。
pub fn refresh(
    repo: &mut Repo,
    batch_ref: &str,
    batch_status: &str,
    rule_version: &str,
) -> Result<BatchSummaryRecord, EngineError> {
    let states = repo.list_row_push_states(batch_ref)?;
    let issue_count = repo.count_issues(batch_ref)?;
    let totals = totals_from_rows(&states, issue_count as usize);

    let mut record = match repo.batch_summary(batch_ref)? {
        Some(prev) => prev,
        None => BatchSummaryRecord::empty(batch_ref),
    };
    record.reported = totals.reported as i64;
    record.rejected = totals.rejected as i64;
    record.retryable = totals.retryable as i64;
    record.issue_count = totals.issue_count as i64;
    record.batch_status = batch_status.to_string();
    if !rule_version.is_empty() {
        record.rule_version = rule_version.to_string();
    }
    let revision = repo.save_batch_summary(&record)?;
    record.revision = revision;
    Ok(record)
}

/// 上报批次跑完后落汇总（一次事务）。
///
/// # 两种结局
///
/// - **整批跑完**（`report.rollback_applied == false`）：批次计数与汇总值取本轮执行结果，
///   四项口径按**库里当前行级状态**算（权威），批次状态记「上报完成」或「部分完成」。
/// - **批末回滚**（`report.rollback_applied == true`）：汇总**回到批前值**
///   （`before` 给的就是批前快照；没有批前快照就是空汇总），
///   批次状态记「批末回滚」。行级回执不受影响 —— 那是已发生的外部事实。
///
/// `before` 是**批前快照**（跑这批之前从库里取的）；调用方负责在执行前取。
pub fn persist_after_run(
    repo: &mut Repo,
    batch_ref: &str,
    report: &ExecutionReport,
    before: Option<&BatchSummaryRecord>,
) -> Result<BatchSummaryRecord, EngineError> {
    if report.rollback_applied {
        // 批末回滚：汇总回到批前值，一项都不动
        let mut record = match before {
            Some(prev) => prev.clone(),
            None => BatchSummaryRecord::empty(batch_ref),
        };
        record.batch_ref = batch_ref.to_string();
        record.batch_status = BATCH_STATUS_ROLLED_BACK.to_string();
        let revision = repo.save_batch_summary(&record)?;
        record.revision = revision;
        return Ok(record);
    }

    // 正常结局：四项口径按行级状态算，批次计数与汇总值取本轮执行结果
    let states = repo.list_row_push_states(batch_ref)?;
    let issue_count = repo.count_issues(batch_ref)?;
    let totals = totals_from_rows(&states, issue_count as usize);
    let summary = &report.summary_after;

    let mut record = BatchSummaryRecord::empty(batch_ref);
    record.planned_steps = summary.planned_steps as i64;
    record.attempted = summary.attempted as i64;
    record.accepted = summary.accepted as i64;
    record.idempotent_hit = summary.idempotent_hit as i64;
    record.reported = totals.reported as i64;
    record.rejected = totals.rejected as i64;
    record.retryable = totals.retryable as i64;
    record.blocked = summary.blocked as i64;
    record.invalid_receipt = summary.invalid_receipt as i64;
    record.not_executed = summary.not_executed as i64;
    record.issue_count = totals.issue_count as i64;
    record.sum_value = summary.sum_value;
    record.sum_numerator = summary.sum_numerator;
    record.sum_denominator = summary.sum_denominator;
    record.batch_status = if report.completed {
        BATCH_STATUS_COMPLETED.to_string()
    } else {
        BATCH_STATUS_PARTIAL.to_string()
    };
    record.rule_version = repo.batch_rule_version(batch_ref).unwrap_or_default();

    let revision = repo.save_batch_summary(&record)?;
    record.revision = revision;
    // 批次提交标记：这批跑到最后了。
    //
    // 标记写在恢复日志里（状态库旁边的 append-only 边车文件）——
    // 状态库坏掉时，「哪一批算跑完了」这条信息靠它找回来。
    // 写不进去不判本批失败（汇总已经落库了），记一条告警让人知道备份没写上。
    let mut marker = crate::db::journal::JournalRecord::batch_commit(
        batch_ref,
        &format!(
            "批次状态 {} · 修订 {} · 已上报 {} · 被退回 {} · 待重试 {}",
            record.batch_status, revision, record.reported, record.rejected, record.retryable
        ),
    );
    if let Err(e) = repo.append_journal(&mut marker) {
        crate::log::log(
            crate::log::Level::Warn,
            "push.batch_marker_write_failed",
            Some(batch_ref),
            Some(&e.code),
            &format!("批次提交标记写不进恢复日志（批次汇总已落库）：{}", e.message),
            &[],
        );
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::status::{ACCEPTED, REJECTED, RETRYABLE};

    fn state(row_uid: &str, st: &str) -> RowPushState {
        RowPushState {
            row_uid: row_uid.to_string(),
            status: st.to_string(),
            error_code: String::new(),
            error_field: String::new(),
            message: String::new(),
            receipt_id: String::new(),
            accepted_at: String::new(),
            idempotency_key: String::new(),
            revision: 1,
        }
    }

    #[test]
    fn 四项口径按行级状态分得开() {
        let states = vec![
            state("r1", ACCEPTED),
            state("r2", ACCEPTED),
            state("r3", REJECTED),
            state("r4", RETRYABLE),
            state("r5", "未发出（数据不合格）"),
            state("r6", "回执待人工核对"),
        ];
        let totals = totals_from_rows(&states, 4);
        assert_eq!(totals.reported, 2, "已上报 = 受理的行");
        assert_eq!(totals.rejected, 1);
        assert_eq!(totals.retryable, 1);
        assert_eq!(
            totals.issue_count, 4,
            "未发出 / 回执待核对不算已报也不算等重试"
        );
    }

    #[test]
    fn 幂等命中也算已上报() {
        // 幂等命中在库里落的状态是「已上报」—— 那一行确实已经报上去了
        let states = vec![state("r1", status::IDEMPOTENT_HIT)];
        let totals = totals_from_rows(&states, 0);
        assert_eq!(totals.reported, 1);
    }

    #[test]
    fn 空汇总不报任何数() {
        let totals = totals_from_rows(&[], 0);
        assert_eq!(totals.reported, 0);
        assert_eq!(totals.rejected, 0);
        assert_eq!(totals.retryable, 0);
        assert_eq!(totals.issue_count, 0);
    }
}
