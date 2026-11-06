//! 未上报名单与催报对照：经办人催办、科室逐条改表用的两份产物。
//!
//! # 这份产物回答两个问题
//!
//! 1. **还剩哪些行没报上去** —— 按科室与报告期列出，每行带**具体原因**
//!    （错误码 / 缺失列 / 规则编号 / 未确认标记），不写「待处理」这类无信息量文案。
//! 2. **谁还没交表** —— 「已收到 / 未收到」两列对照，按报告期分组，
//!    供报告期截止前催办（依据《国家卫生统计信息网络直报管理规定（试行）》
//!    报告期截止 5 日前催报公告）。
//!
//! # 为什么「未收到」必须有名册
//!
//! 「没交表」是**相对一份应交名单**才成立的说法：没有名单就只能列出「谁交了」，
//! 说不清「谁没交」。所以未收到一列取自**应交科室名册**，名册来源写在产物里
//! （`roster_source`）：
//!
//! - **名册文件**（首选）：`$REGVAL_RULE_DIR/roster.json`，医院自己维护的
//!   「本报告期应交科室清单」；
//! - **院内 HIS 科室目录缓存**（兜底）：院内科室全量 —— 目录里没有的科室不在名单内，
//!   名单**可能宽于**实际应交范围，产物里写明这一点。
//!
//! 两者都取不到时，未收到一列为空并说明为什么空 ——
//! **不拿本地见过的科室凑一份名册**：那是猜的，猜出来的「未交」会冤枉科室。
//!
//! # 状态与待办原因不是一回事
//!
//! 行状态（待校验 / 校验通过 / 待确认 / 已上报 / 被退回 / 待重试）来自
//! [`crate::state::machine::RowState`]；待办原因（未校验 / 有问题待修 / 待确认 / 待重试）
//! 是**给人看的归类**，由「行状态 + 本行命中的问题 + 上报回执」三者合起来判。
//!
//! 「已上报」只由**网关回执**驱动 —— 本模块只读，不推进任何状态、不改任何数据。
//!
//! # 字段边界
//!
//! 产物只有指标级与科室级字段（指标编号 / 科室代码 / 报告期 / 业务主键 / 行键），
//! **不含患者可识别明细**。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::repo::{RowPushState, StoredRow};
use crate::errors::{EngineError, ErrorCode};
use crate::rules::model::Issue;
use crate::state::machine::RowState;

use super::summary::{IssueIndex, SEVERITY_BLOCKING};

/// 未上报名单版本。改动产物结构时升版本。
pub const PENDING_SCHEMA_VERSION: &str = "regval.pending/1.0.0";

/// 待办原因：还没过口径校验
pub const REASON_NOT_VALIDATED: &str = "未校验";
/// 待办原因：有口径问题或有退回问题，要改数据
pub const REASON_NEEDS_FIX: &str = "有问题待修";
/// 待办原因：口径已过，等人工确认或等推送
pub const REASON_AWAITING_CONFIRM: &str = "待确认";
/// 待办原因：上次上报失败，原样可以再试
pub const REASON_RETRY_LATER: &str = "待重试";

/// 原因来源：口径校验（本轮规则算出来的）
pub const SOURCE_CALIBER: &str = "口径校验";
/// 原因来源：上报网关（回执带回来的）
pub const SOURCE_GATEWAY: &str = "上报网关";
/// 原因来源：人工确认（等经办人勾选）
pub const SOURCE_CONFIRM: &str = "人工确认";
/// 原因来源：状态库（行还没进过校验流程）
pub const SOURCE_STATE: &str = "状态库";

/// 催报对照：收报状态 —— 已收到
pub const RECEIPT_RECEIVED: &str = "已收到";
/// 催报对照：收报状态 —— 未收到
pub const RECEIPT_MISSING: &str = "未收到";

/// 未上报名单的列定义（CSV 表头，顺序固定）
pub const CSV_HEADER: [&str; 17] = [
    "来源表",
    "行号",
    "行键",
    "指标编号",
    "科室代码",
    "科室名称",
    "报告期",
    "业务主键",
    "行状态",
    "待办原因",
    "原因来源",
    "原因编号",
    "原因明细",
    "错误码",
    "缺失列",
    "问题条数",
    "已确认",
];

/// 催报对照的列定义（CSV 表头，顺序固定；「已收到 / 未收到」两列对照）
pub const REMINDER_HEADER: [&str; 4] = ["报告期", "已收到", "未收到", "说明"];

/// 名册文件在规则目录下的文件名
pub const ROSTER_FILE_NAME: &str = "roster.json";

// ————————————————————————————————————————————————————
// 应交科室名册
// ————————————————————————————————————————————————————

/// 名册文件里一个报告期的应交科室
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterPeriod {
    /// 报告期（与表里的「报告期」列同一写法）
    pub reporting_period: String,
    /// 应交科室代码
    #[serde(default)]
    pub dept_codes: Vec<String>,
    /// 科室名称（代码 → 名称；缺了就只显示代码）
    #[serde(default)]
    pub dept_names: BTreeMap<String, String>,
}

/// 名册文件（一份文件装多个报告期）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RosterFile {
    /// 各报告期的应交科室
    #[serde(default)]
    pub reporting_periods: Vec<RosterPeriod>,
}

/// 应交科室名册。
///
/// 名册是「谁该交表」的**唯一依据** —— 没有它，「未收到」无从谈起。
/// 名册的来源与局限都记在产物里（`source` / `note`），经办人一眼看得出
/// 这一列是按什么名单比出来的。
#[derive(Debug, Clone, Default)]
pub struct Roster {
    /// 名册来源（名册文件 / 院内 HIS 科室目录缓存 / 未取到名册）
    pub source: String,
    /// 来源说明与局限
    pub note: String,
    /// 报告期 → （科室代码 → 科室名称）
    pub periods: BTreeMap<String, BTreeMap<String, String>>,
}

impl Roster {
    /// 空名册（没取到名册时用；未收到一列因此为空）
    pub fn empty(note: &str) -> Roster {
        Roster {
            source: "未取到名册".to_string(),
            note: note.to_string(),
            periods: BTreeMap::new(),
        }
    }

    /// 从名册文件装载（`$REGVAL_RULE_DIR/roster.json`）。
    ///
    /// 文件读不出、解析失败、字段不对都**报错** —— 名册读错会算出一张
    /// 冤枉科室的催报表，比导不出来危险得多。
    pub fn from_file(path: &Path) -> Result<Roster, EngineError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            EngineError::at(
                ErrorCode::InputUnreadable,
                format!("应交科室名册读不出来：{}", e),
                path.display().to_string(),
            )
        })?;
        let file: RosterFile = serde_json::from_str(&text).map_err(|e| {
            EngineError::at(
                ErrorCode::InputUnreadable,
                format!("应交科室名册解析失败：{}", e),
                path.display().to_string(),
            )
        })?;
        let mut periods: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for period in file.reporting_periods {
            let period_key = period.reporting_period.trim().to_string();
            if period_key.is_empty() {
                continue;
            }
            let entry = periods.entry(period_key).or_default();
            for code in period.dept_codes {
                let code = code.trim().to_string();
                if code.is_empty() {
                    continue;
                }
                let name = period
                    .dept_names
                    .get(&code)
                    .cloned()
                    .unwrap_or_default();
                entry.entry(code).or_insert(name);
            }
        }
        Ok(Roster {
            source: format!("名册文件 {}", path.display()),
            note: "按医院维护的应交科室清单比对（名册里没有的科室不催）".to_string(),
            periods,
        })
    }

    /// 从院内 HIS 科室目录缓存兜底（目录里没有的科室不在名单内）。
    ///
    /// 目录是**科室全量**，未必等于「本报告期应交科室」—— 所以产物里写明
    /// 名单可能宽于实际应交范围，别拿它直接当催办依据。
    pub fn from_dept_entries(entries: &[crate::push::dept::DeptEntry], fetched_at: &str) -> Roster {
        let mut periods: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for entry in entries {
            let code = entry.dept_code.trim().to_string();
            if code.is_empty() {
                continue;
            }
            // 目录兜底对所有报告期都适用 —— 报告期这一层由调用方按批次取
            periods
                .entry(String::new())
                .or_default()
                .entry(code)
                .or_insert_with(|| entry.dept_name.trim().to_string());
        }
        let mut roster = Roster {
            source: "院内 HIS 科室目录缓存".to_string(),
            note: format!(
                "按院内科室目录全量比对（目录数据时间 {}）；目录里没有的科室不在名单内，名单可能宽于实际应交范围",
                if fetched_at.is_empty() {
                    "未记录"
                } else {
                    fetched_at
                }
            ),
            periods,
        };
        // 空报告期这一层作废：兜底名册对任何报告期都适用
        let all = roster.periods.remove("").unwrap_or_default();
        roster.periods.insert("*".to_string(), all);
        roster
    }

    /// 某个报告期的应交科室（代码 + 名称，按代码排序）
    pub fn for_period(&self, period: &str) -> Vec<(String, String)> {
        let exact = self.periods.get(period);
        let wildcard = self.periods.get("*");
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        // 兜底名册（对所有报告期适用）先铺，报告期专属名册后铺并覆盖
        if let Some(all) = wildcard {
            for (code, name) in all {
                out.insert(code.clone(), name.clone());
            }
        }
        if let Some(list) = exact {
            for (code, name) in list {
                out.insert(code.clone(), name.clone());
            }
        }
        out.into_iter().collect()
    }

    /// 名册里一个科室都没列
    pub fn is_empty(&self) -> bool {
        self.periods
            .values()
            .all(|m| m.is_empty())
    }
}

// ————————————————————————————————————————————————————
// 产物结构
// ————————————————————————————————————————————————————

/// 未上报名单里的一行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRow {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: String,
    pub dept_code: String,
    pub dept_name: String,
    pub reporting_period: String,
    pub business_key: String,
    /// 行状态（待校验 / 校验通过 / 待确认 / 被退回 / 待重试）
    pub row_state: String,
    /// 待办原因（未校验 / 有问题待修 / 待确认 / 待重试）
    pub todo_reason: String,
    /// 原因来源（口径校验 / 上报网关 / 人工确认 / 状态库）
    pub reason_source: String,
    /// 原因明细：具体到问题类型、规则编号、缺失列、错误码或未确认标记
    pub reason_detail: String,
    /// 造成该原因的口径规则编号（顿号分隔；上报侧原因无规则编号）
    pub rule_ids: String,
    /// 机器可读错误码（无则空串）
    pub error_code: String,
    /// 缺失列清单（顿号分隔）
    pub missing_columns: String,
    /// 本行命中的问题条数
    pub issue_count: usize,
    /// 是否已有有效的人工确认
    pub confirmed: bool,
}

/// 未上报名单的计数
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PendingCounts {
    /// 本批行数
    pub rows_total: usize,
    /// 未上报行数（清单里的行数）
    pub rows_pending: usize,
    /// 已上报行数（网关受理或幂等命中）
    pub rows_reported: usize,
    /// 按待办原因计数
    pub by_reason: BTreeMap<String, usize>,
    /// 按行状态计数（全部行，不只是未上报行）
    pub by_state: BTreeMap<String, usize>,
    /// 按科室计数（未上报行）
    pub by_dept: BTreeMap<String, usize>,
    /// 按报告期计数（未上报行）
    pub by_period: BTreeMap<String, usize>,
}

/// 催报对照里的一个科室
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderDept {
    pub dept_code: String,
    pub dept_name: String,
    /// 收报状态（已收到 / 未收到）
    pub receipt_state: String,
    /// 本批次收到的行数
    pub rows: usize,
    /// 其中未上报的行数
    pub pending_rows: usize,
    /// 说明（未收到的行写明「本报告期未收到该科室上报表」）
    pub note: String,
}

/// 一个报告期的催报对照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodReminder {
    pub reporting_period: String,
    /// 已收到
    pub received: Vec<ReminderDept>,
    /// 未收到
    pub missing: Vec<ReminderDept>,
    /// 这一组的说明（名册里没有这个报告期时写明「未收到一列不可用」）
    pub note: String,
}

impl PeriodReminder {
    /// 已收到的科室数
    pub fn received_count(&self) -> usize {
        self.received.len()
    }
    /// 未收到的科室数
    pub fn missing_count(&self) -> usize {
        self.missing.len()
    }
}

/// 催报对照（按报告期分组）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderComparison {
    /// 名册来源
    pub roster_source: String,
    /// 名册来源说明与局限
    pub roster_note: String,
    /// 名册覆盖的报告期数
    pub roster_periods: usize,
    /// 报告期写法不在枚举内、没计入分组的行数
    pub rows_without_valid_period: usize,
    /// 各报告期的对照
    pub periods: Vec<PeriodReminder>,
}

/// 未上报名单 + 催报对照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReport {
    pub schema_version: String,
    pub generated_at: String,
    pub batch_ref: String,
    pub counts: PendingCounts,
    /// 未上报行（按来源表、行号排序）
    pub rows: Vec<PendingRow>,
    /// 催报对照
    pub reminder: ReminderComparison,
}

impl PendingReport {
    /// 未上报行数
    pub fn pending_count(&self) -> usize {
        self.rows.len()
    }

    /// 某个报告期的催报对照
    pub fn period(&self, reporting_period: &str) -> Option<&PeriodReminder> {
        self.reminder
            .periods
            .iter()
            .find(|p| p.reporting_period == reporting_period)
    }
}

// ————————————————————————————————————————————————————
// 组装
// ————————————————————————————————————————————————————

/// 按批次组装未上报名单与催报对照。
///
/// `states` 是行键 → 行状态字面量（取值来自 [`crate::state::machine`]）；
/// 状态字面量**不认识时报错**，不猜也不丢行 —— 猜出来的待办原因会催错人。
///
/// `push_states` 是行键 → 上报状态（网关回执驱动）。**只有**网关受理或幂等命中
/// 才算「已上报」；被退回、待重试、未发出都还在清单里。
pub fn build(
    batch_ref: &str,
    rows: &[StoredRow],
    issues: &[Issue],
    states: &BTreeMap<String, String>,
    push_states: &[RowPushState],
    confirmed: &std::collections::BTreeSet<String>,
    roster: &Roster,
) -> Result<PendingReport, EngineError> {
    let index = IssueIndex::build(issues);
    let push_by_row: BTreeMap<&str, &RowPushState> = push_states
        .iter()
        .map(|p| (p.row_uid.as_str(), p))
        .collect();

    let mut pending: Vec<PendingRow> = Vec::new();
    let mut counts = PendingCounts {
        rows_total: rows.len(),
        ..Default::default()
    };

    for row in rows {
        let state = match states.get(&row.row_uid) {
            Some(raw) => RowState::parse(raw).ok_or_else(|| {
                EngineError::at(
                    ErrorCode::InvalidStateTransition,
                    format!(
                        "状态库里的行状态取值不认识（{}），未上报名单拒绝按猜测归类（不猜、不丢行）",
                        raw
                    ),
                    format!("row_uid={}", row.row_uid),
                )
            })?,
            None => RowState::PendingValidation,
        };
        *counts
            .by_state
            .entry(state.as_str().to_string())
            .or_insert(0) += 1;

        let push = push_by_row.get(row.row_uid.as_str()).copied();
        if let Some(p) = push {
            if crate::push::status::is_submitted(&p.status) {
                counts.rows_reported += 1;
                continue;
            }
        }

        let row_issues = index.of_row(&row.row_uid);
        let cls = classify(state, push, row_issues);

        counts.rows_pending += 1;
        *counts
            .by_reason
            .entry(cls.reason.clone())
            .or_insert(0) += 1;
        *counts
            .by_dept
            .entry(dept_key(&row.dept_code, &row.dept_name))
            .or_insert(0) += 1;
        *counts
            .by_period
            .entry(row.reporting_period.clone())
            .or_insert(0) += 1;

        pending.push(PendingRow {
            row_uid: row.row_uid.clone(),
            source_ns: row.source_ns.clone(),
            source_file: row.source_file.clone(),
            row_no: row.row_no,
            indicator_code: row.indicator_code.clone(),
            dept_code: row.dept_code.clone(),
            dept_name: row.dept_name.clone(),
            reporting_period: row.reporting_period.clone(),
            business_key: row.business_key.clone(),
            row_state: state.as_str().to_string(),
            todo_reason: cls.reason,
            reason_source: cls.source,
            reason_detail: cls.detail,
            rule_ids: cls.rule_ids,
            error_code: cls.error_code,
            missing_columns: cls.missing_columns,
            issue_count: row_issues.len(),
            confirmed: confirmed.contains(&row.row_uid),
        });
    }

    let reminder = build_reminder(rows, &pending, roster);

    Ok(PendingReport {
        schema_version: PENDING_SCHEMA_VERSION.to_string(),
        generated_at: crate::db::repo::now_iso8601(),
        batch_ref: batch_ref.to_string(),
        counts,
        rows: pending,
        reminder,
    })
}

/// 一行的待办归类结果
#[derive(Debug, Clone, Default)]
struct Classification {
    /// 待办原因
    reason: String,
    /// 原因来源
    source: String,
    /// 原因明细
    detail: String,
    /// 造成该原因的口径规则编号（顿号分隔）
    rule_ids: String,
    /// 机器可读错误码
    error_code: String,
    /// 缺失列清单
    missing_columns: String,
}

/// 判一行属于哪一类待办、原因是什么。
fn classify(state: RowState, push: Option<&RowPushState>, issues: &[Issue]) -> Classification {
    // 上报侧的状态优先：这一行已经推过一次，原因按回执说
    if let Some(p) = push {
        match p.status.as_str() {
            crate::push::status::RETRYABLE => {
                return Classification {
                    reason: REASON_RETRY_LATER.to_string(),
                    source: SOURCE_GATEWAY.to_string(),
                    detail: format!(
                        "上次上报失败可重试：错误码 {}，字段 {}，已尝试 {} 次（幂等键 {}）",
                        or_none(&p.error_code),
                        or_none(&p.error_field),
                        p.revision,
                        short_key(&p.idempotency_key)
                    ),
                    rule_ids: String::new(),
                    error_code: p.error_code.clone(),
                    missing_columns: String::new(),
                };
            }
            crate::push::status::REJECTED => {
                return Classification {
                    reason: REASON_NEEDS_FIX.to_string(),
                    source: SOURCE_GATEWAY.to_string(),
                    detail: format!(
                        "上报被网关退回：要改数据才能再报 —— 错误码 {}，字段 {}，网关说明 {}",
                        or_none(&p.error_code),
                        or_none(&p.error_field),
                        or_none(&p.message)
                    ),
                    rule_ids: String::new(),
                    error_code: p.error_code.clone(),
                    missing_columns: String::new(),
                };
            }
            crate::push::status::BLOCKED => {
                // 写闸门拦下的（未确认）与数据本身不合格的，**待办原因不一样**：
                // 前者要的是「勾选确认」，后者要的是「改数据」。混成一条，
                // 经办人会去改数据 —— 改完了还是推不上去。
                let unconfirmed = p.error_code == crate::errors::ErrorCode::UnconfirmedRow.as_str();
                return Classification {
                    reason: if unconfirmed {
                        REASON_AWAITING_CONFIRM.to_string()
                    } else {
                        REASON_NEEDS_FIX.to_string()
                    },
                    source: if unconfirmed {
                        SOURCE_CONFIRM.to_string()
                    } else {
                        SOURCE_GATEWAY.to_string()
                    },
                    detail: if unconfirmed {
                        "写闸门拦下：这一行还没人工确认，请求未发出（确认后才推）".to_string()
                    } else {
                        "本行数据不合格，请求未发出（改数据后重新校验）".to_string()
                    },
                    rule_ids: String::new(),
                    error_code: p.error_code.clone(),
                    missing_columns: String::new(),
                };
            }
            crate::push::status::INVALID_RECEIPT => {
                return Classification {
                    reason: REASON_NEEDS_FIX.to_string(),
                    source: SOURCE_GATEWAY.to_string(),
                    detail: format!(
                        "回执不可用，不能当成功：需人工核对 —— 网关说明 {}",
                        or_none(&p.message)
                    ),
                    rule_ids: String::new(),
                    error_code: p.error_code.clone(),
                    missing_columns: String::new(),
                };
            }
            crate::push::status::NOT_EXECUTED => {
                return Classification {
                    reason: REASON_RETRY_LATER.to_string(),
                    source: SOURCE_GATEWAY.to_string(),
                    detail: "本批前面的步骤失败，没执行到这一行（重跑一次 regval push 即可续跑补报）"
                        .to_string(),
                    rule_ids: String::new(),
                    error_code: p.error_code.clone(),
                    missing_columns: String::new(),
                };
            }
            _ => {}
        }
    }

    // 口径侧：阻断级问题优先
    let blocking: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.severity == SEVERITY_BLOCKING)
        .collect();
    if !blocking.is_empty() {
        let (detail, rule_ids, codes, missing) = caliber_detail(&blocking);
        return Classification {
            reason: REASON_NEEDS_FIX.to_string(),
            source: SOURCE_CALIBER.to_string(),
            detail,
            rule_ids,
            error_code: codes,
            missing_columns: missing,
        };
    }

    // 口径已过：等确认 / 等推送
    match state {
        RowState::Validated => Classification {
            reason: REASON_AWAITING_CONFIRM.to_string(),
            source: SOURCE_CONFIRM.to_string(),
            detail: format!(
                "口径校验通过，等待经办人勾选确认（未确认标记）{}",
                dupe_hint(issues)
            ),
            rule_ids: rule_ids_of(issues),
            error_code: String::new(),
            missing_columns: String::new(),
        },
        RowState::PendingConfirmation => Classification {
            reason: REASON_AWAITING_CONFIRM.to_string(),
            source: SOURCE_CONFIRM.to_string(),
            detail: format!("已人工确认，等待推送上报{}", dupe_hint(issues)),
            rule_ids: rule_ids_of(issues),
            error_code: String::new(),
            missing_columns: String::new(),
        },
        _ => Classification {
            reason: REASON_NOT_VALIDATED.to_string(),
            source: SOURCE_STATE.to_string(),
            detail: format!(
                "本行尚未完成口径校验（行状态：{}）{}",
                state.as_str(),
                hint_note(issues)
            ),
            rule_ids: rule_ids_of(issues),
            error_code: String::new(),
            missing_columns: String::new(),
        },
    }
}

/// 口径问题的明细：逐条写清问题类型、规则编号、缺失列与错误码。
///
/// 返回：(明细, 规则编号串, 错误码串, 缺失列串)
fn caliber_detail(blocking: &[&Issue]) -> (String, String, String, String) {
    let mut parts: Vec<String> = Vec::new();
    let mut rule_ids: Vec<String> = Vec::new();
    let mut codes: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for issue in blocking {
        let mut part = format!("{}（规则 {}", issue.issue_kind, issue.rule_id);
        if !issue.missing_columns.is_empty() {
            part.push_str(&format!("，缺失列：{}", issue.missing_columns.join("、")));
            for col in &issue.missing_columns {
                if !missing.contains(col) {
                    missing.push(col.clone());
                }
            }
        }
        if !issue.error_code.is_empty() {
            part.push_str(&format!("，错误码：{}", issue.error_code));
            if !codes.contains(&issue.error_code) {
                codes.push(issue.error_code.clone());
            }
        }
        part.push('）');
        parts.push(part);
        if !rule_ids.contains(&issue.rule_id) {
            rule_ids.push(issue.rule_id.clone());
        }
    }
    (
        format!("口径问题 {} 项：{}", blocking.len(), parts.join("；")),
        rule_ids.join("、"),
        codes.join("、"),
        missing.join("、"),
    )
}

/// 某一行的规则编号清单（去重，按首次出现顺序）
fn rule_ids_of(issues: &[Issue]) -> String {
    let mut out: Vec<String> = Vec::new();
    for issue in issues {
        if !issue.rule_id.is_empty() && !out.contains(&issue.rule_id) {
            out.push(issue.rule_id.clone());
        }
    }
    out.join("、")
}

/// 含疑似重复时补一句（这类行要人工判定保留哪一行）
fn dupe_hint(issues: &[Issue]) -> String {
    if issues.iter().any(|i| i.issue_kind == "疑似重复") {
        "；含疑似重复，待人工判定保留哪一行（本轮不自动合并）".to_string()
    } else {
        String::new()
    }
}

/// 只有提示级问题时补一句
fn hint_note(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return String::new();
    }
    let kinds: Vec<&str> = issues.iter().map(|i| i.issue_kind.as_str()).collect();
    format!("；口径提示：{}", kinds.join("、"))
}

/// 空值写成人话（不留空串让人猜）
fn or_none(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "（网关未点名）".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 幂等键只显示前 12 位（够核对，不必占满一列）
fn short_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return "（未生成）".to_string();
    }
    if trimmed.len() <= 12 {
        return trimmed.to_string();
    }
    format!("{}…", &trimmed[..12])
}

/// 科室计数键：代码 + 名称（代码为空时退回名称）
fn dept_key(dept_code: &str, dept_name: &str) -> String {
    let code = dept_code.trim();
    if !code.is_empty() {
        return code.to_string();
    }
    let name = dept_name.trim();
    if !name.is_empty() {
        return format!("（科室代码为空）{}", name);
    }
    "（科室代码与名称均为空）".to_string()
}

/// 按报告期分组做催报对照。
///
/// 「已收到」= 本批次里出现过的科室；「未收到」= 应交名册里、本批次没出现的科室。
///
/// **报告期写法不合法的行不计入分组**：`2026Q3` 这种写法属于枚举越界，
/// 它到底算哪个报告期还不知道 —— 猜一个分组会把「谁没交」算到别的报告期头上。
/// 这些行照样在未上报名单里（带枚举越界问题），计数写在
/// [`ReminderComparison::rows_without_valid_period`] 里，不悄悄少掉。
fn build_reminder(
    rows: &[StoredRow],
    pending: &[PendingRow],
    roster: &Roster,
) -> ReminderComparison {
    // 报告期 → 科室 → (收到行数, 未上报行数, 科室名称)
    let mut by_period: BTreeMap<String, BTreeMap<String, (usize, usize, String)>> = BTreeMap::new();
    let mut without_valid_period = 0usize;
    for row in rows {
        if !crate::rules::checks::enum_::is_valid_reporting_period(&row.reporting_period) {
            without_valid_period += 1;
            continue;
        }
        let entry = by_period
            .entry(row.reporting_period.clone())
            .or_default()
            .entry(row.dept_code.trim().to_string())
            .or_insert((0, 0, row.dept_name.trim().to_string()));
        entry.0 += 1;
    }
    for row in pending {
        if !crate::rules::checks::enum_::is_valid_reporting_period(&row.reporting_period) {
            continue;
        }
        if let Some(entry) = by_period
            .get_mut(&row.reporting_period)
            .and_then(|m| m.get_mut(row.dept_code.trim()))
        {
            entry.1 += 1;
        }
    }

    let mut periods: Vec<PeriodReminder> = Vec::new();
    for (period, depts) in by_period {
        let expected = roster.for_period(&period);
        let mut received: Vec<ReminderDept> = Vec::new();
        let mut missing: Vec<ReminderDept> = Vec::new();

        for (code, (rows_n, pending_n, name)) in &depts {
            received.push(ReminderDept {
                dept_code: code.clone(),
                dept_name: if name.is_empty() {
                    expected
                        .iter()
                        .find(|(c, _)| c == code)
                        .map(|(_, n)| n.clone())
                        .unwrap_or_default()
                } else {
                    name.clone()
                },
                receipt_state: RECEIPT_RECEIVED.to_string(),
                rows: *rows_n,
                pending_rows: *pending_n,
                note: format!("本报告期收到该科室上报表 {} 行", rows_n),
            });
        }

        for (code, name) in &expected {
            if depts.contains_key(code) {
                continue;
            }
            missing.push(ReminderDept {
                dept_code: code.clone(),
                dept_name: name.clone(),
                receipt_state: RECEIPT_MISSING.to_string(),
                rows: 0,
                pending_rows: 0,
                note: "本报告期未收到该科室上报表".to_string(),
            });
        }

        // 名册里没有这个报告期 → 未收到一列不可用，写明而不是留个空列让人误读
        let note = if expected.is_empty() {
            format!(
                "名册里没有 {} 的应交科室条目，未收到一列不可用（名册来源：{}）",
                period, roster.source
            )
        } else {
            format!(
                "名册列出 {} 个应交科室，已收到 {} 个",
                expected.len(),
                received.len()
            )
        };

        periods.push(PeriodReminder {
            reporting_period: period,
            received,
            missing,
            note,
        });
    }

    ReminderComparison {
        roster_source: roster.source.clone(),
        roster_note: roster.note.clone(),
        roster_periods: roster
            .periods
            .values()
            .filter(|m| !m.is_empty())
            .count(),
        rows_without_valid_period: without_valid_period,
        periods,
    }
}

// ————————————————————————————————————————————————————
// 产物
// ————————————————————————————————————————————————————

/// 未上报名单 → CSV 文本
pub fn to_csv(report: &PendingReport) -> Result<String, csv::Error> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(CSV_HEADER)?;
    for row in &report.rows {
        wtr.write_record([
            row.source_ns.clone(),
            row.row_no.to_string(),
            row.row_uid.clone(),
            row.indicator_code.clone(),
            row.dept_code.clone(),
            row.dept_name.clone(),
            row.reporting_period.clone(),
            row.business_key.clone(),
            row.row_state.clone(),
            row.todo_reason.clone(),
            row.reason_source.clone(),
            row.rule_ids.clone(),
            row.reason_detail.clone(),
            row.error_code.clone(),
            row.missing_columns.clone(),
            row.issue_count.to_string(),
            if row.confirmed { "是" } else { "否" }.to_string(),
        ])?;
    }
    wtr.flush()?;
    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 催报对照 → CSV 文本（「已收到 / 未收到」两列对照，一行一个报告期）
pub fn to_reminder_csv(report: &PendingReport) -> Result<String, csv::Error> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(REMINDER_HEADER)?;
    for period in &report.reminder.periods {
        wtr.write_record([
            period.reporting_period.clone(),
            join_depts(&period.received),
            join_depts(&period.missing),
            format!(
                "已收到 {} 个科室 · 未收到 {} 个科室 · 名册来源：{}",
                period.received.len(),
                period.missing.len(),
                report.reminder.roster_source
            ),
        ])?;
    }
    wtr.flush()?;
    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 科室列表拼成一个单元格（代码 + 名称，顿号分隔）
fn join_depts(depts: &[ReminderDept]) -> String {
    depts
        .iter()
        .map(|d| {
            if d.dept_name.is_empty() {
                d.dept_code.clone()
            } else {
                format!("{} {}", d.dept_code, d.dept_name)
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 未上报名单 + 催报对照 → JSON 文本
pub fn to_json(report: &PendingReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(uid: &str, ns: &str, row_no: usize, dept: &str, period: &str) -> StoredRow {
        StoredRow {
            row_uid: uid.into(),
            source_ns: ns.into(),
            source_file: format!("{}.csv", ns),
            row_no,
            org_code: String::new(),
            authority_state: "authoritative".into(),
            indicator_code: "MER-TL-04".into(),
            indicator_name: "病案首页24小时内完成率".into(),
            dept_code: dept.into(),
            dept_name: format!("科室{}", dept),
            reporting_period: period.into(),
            business_key: format!("BK-{}", row_no),
            numerator: Some(98.0),
            denominator: Some(100.0),
            value: Some(98.0),
            unit: "%".into(),
            numeric_raw: String::new(),
            rule_version: "2021.1".into(),
        }
    }

    fn issue(row: &str, kind: &str, severity: &str, code: &str, missing: &[&str]) -> Issue {
        Issue {
            row_uid: row.into(),
            rule_id: format!("R-{}", kind),
            issue_kind: kind.into(),
            severity: severity.into(),
            message: format!("{}说明", kind),
            basis: "国卫办医函〔2021〕28号 附件".into(),
            issue_key: String::new(),
            missing_columns: missing.iter().map(|s| s.to_string()).collect(),
            error_code: code.into(),
        }
    }

    fn push_state(uid: &str, status: &str, code: &str) -> RowPushState {
        RowPushState {
            row_uid: uid.into(),
            status: status.into(),
            error_code: code.into(),
            error_field: "value".into(),
            message: "网关原话".into(),
            receipt_id: String::new(),
            accepted_at: String::new(),
            idempotency_key: "0123456789abcdef0123456789abcdef".into(),
            revision: 1,
        }
    }

    fn roster_with(period: &str, codes: &[(&str, &str)]) -> Roster {
        let mut roster = Roster {
            source: "名册文件".into(),
            note: "测试名册".into(),
            periods: BTreeMap::new(),
        };
        let mut map = BTreeMap::new();
        for (code, name) in codes {
            map.insert(code.to_string(), name.to_string());
        }
        roster.periods.insert(period.to_string(), map);
        roster
    }

    #[test]
    fn 未上报行数与状态计数一致() {
        let rows = vec![
            stored("u1", "科室一表", 2, "0301", "2026-Q3"),
            stored("u2", "科室一表", 3, "0301", "2026-Q3"),
            stored("u3", "科室二表", 2, "0302", "2026-Q3"),
        ];
        let mut states = BTreeMap::new();
        states.insert("u1".to_string(), "校验通过".to_string());
        states.insert("u2".to_string(), "待校验".to_string());
        states.insert("u3".to_string(), "已上报".to_string());
        let push = vec![push_state("u3", crate::push::status::ACCEPTED, "")];
        let confirmed = std::collections::BTreeSet::new();
        let roster = roster_with("2026-Q3", &[("0301", "病案室"), ("0302", "质控科")]);

        let report = build("b1", &rows, &[], &states, &push, &confirmed, &roster).unwrap();
        assert_eq!(report.counts.rows_total, 3);
        assert_eq!(report.counts.rows_pending, 2);
        assert_eq!(report.counts.rows_reported, 1);
        assert_eq!(report.rows.len(), 2);
        // 状态计数覆盖全部行
        let by_state: usize = report.counts.by_state.values().sum();
        assert_eq!(by_state, 3);
        assert_eq!(report.counts.by_state.get("已上报"), Some(&1));
        // 未上报 + 已上报 == 总行数
        assert_eq!(report.counts.rows_pending + report.counts.rows_reported, 3);
    }

    #[test]
    fn 每行都有具体原因不留空话() {
        let rows = vec![
            stored("u1", "科室一表", 2, "0301", "2026-Q3"),
            stored("u2", "科室一表", 3, "0301", "2026-Q3"),
        ];
        let mut states = BTreeMap::new();
        states.insert("u1".to_string(), "待校验".to_string());
        states.insert("u2".to_string(), "校验通过".to_string());
        let issues = vec![issue("u1", "必填缺失", "阻断", "MISSING_COLUMN", &["指标名称"])];
        let roster = roster_with("2026-Q3", &[("0301", "病案室")]);
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &issues, &states, &[], &confirmed, &roster).unwrap();
        for row in &report.rows {
            assert!(!row.reason_detail.trim().is_empty(), "行 {} 没有原因明细", row.row_uid);
            assert!(
                !row.reason_detail.contains("待处理"),
                "行 {} 的原因是无信息量文案",
                row.row_uid
            );
            // 追得到具体原因：要么有错误码，要么是未确认标记
            let traceable = !row.error_code.is_empty()
                || row.reason_detail.contains("未确认标记")
                || row.reason_detail.contains("规则 ");
            assert!(traceable, "行 {} 追不到具体原因：{}", row.row_uid, row.reason_detail);
        }
        let u1 = report.rows.iter().find(|r| r.row_uid == "u1").unwrap();
        assert_eq!(u1.todo_reason, REASON_NEEDS_FIX);
        assert_eq!(u1.error_code, "MISSING_COLUMN");
        assert_eq!(u1.missing_columns, "指标名称");
        let u2 = report.rows.iter().find(|r| r.row_uid == "u2").unwrap();
        assert_eq!(u2.todo_reason, REASON_AWAITING_CONFIRM);
        assert!(u2.reason_detail.contains("未确认标记"));
    }

    #[test]
    fn 待办原因四类各归各的() {
        let rows = vec![
            stored("u1", "t", 1, "0301", "2026-Q3"),
            stored("u2", "t", 2, "0301", "2026-Q3"),
            stored("u3", "t", 3, "0301", "2026-Q3"),
            stored("u4", "t", 4, "0301", "2026-Q3"),
        ];
        let mut states = BTreeMap::new();
        states.insert("u2".to_string(), "校验通过".to_string());
        states.insert("u3".to_string(), "待确认".to_string());
        states.insert("u4".to_string(), "待重试".to_string());
        let push = vec![push_state("u4", crate::push::status::RETRYABLE, "GATEWAY_TIMEOUT")];
        let issues = vec![issue("u1", "必填缺失", "阻断", "", &["报告期"])];
        let roster = roster_with("2026-Q3", &[("0301", "病案室")]);
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &issues, &states, &push, &confirmed, &roster).unwrap();
        let reason_of = |uid: &str| {
            report
                .rows
                .iter()
                .find(|r| r.row_uid == uid)
                .unwrap()
                .todo_reason
                .clone()
        };
        assert_eq!(reason_of("u1"), REASON_NEEDS_FIX);
        assert_eq!(reason_of("u2"), REASON_AWAITING_CONFIRM);
        assert_eq!(reason_of("u3"), REASON_AWAITING_CONFIRM);
        assert_eq!(reason_of("u4"), REASON_RETRY_LATER);
        assert_eq!(report.counts.by_reason.get(REASON_NEEDS_FIX), Some(&1));
        assert_eq!(report.counts.by_reason.get(REASON_AWAITING_CONFIRM), Some(&2));
        assert_eq!(report.counts.by_reason.get(REASON_RETRY_LATER), Some(&1));
    }

    #[test]
    fn 网关退回落进待修并带回错误码与字段() {
        let rows = vec![stored("u1", "t", 1, "0301", "2026-Q3")];
        let mut states = BTreeMap::new();
        states.insert("u1".to_string(), "被退回".to_string());
        let push = vec![push_state("u1", crate::push::status::REJECTED, "GATEWAY_REJECTED")];
        let roster = roster_with("2026-Q3", &[("0301", "病案室")]);
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &[], &states, &push, &confirmed, &roster).unwrap();
        let row = &report.rows[0];
        assert_eq!(row.todo_reason, REASON_NEEDS_FIX);
        assert_eq!(row.reason_source, SOURCE_GATEWAY);
        assert_eq!(row.error_code, "GATEWAY_REJECTED");
        assert!(row.reason_detail.contains("value"));
    }

    #[test]
    fn 催报对照按报告期给出已收到与未收到() {
        let rows = vec![stored("u1", "科室一表", 2, "0301", "2026-Q3")];
        let states = BTreeMap::new();
        let roster = roster_with("2026-Q3", &[("0301", "病案室"), ("0302", "质控科")]);
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap();
        let period = report.period("2026-Q3").unwrap();
        assert_eq!(period.received_count(), 1);
        assert_eq!(period.missing_count(), 1);
        assert_eq!(period.received[0].dept_code, "0301");
        assert_eq!(period.received[0].receipt_state, RECEIPT_RECEIVED);
        assert_eq!(period.missing[0].dept_code, "0302");
        assert_eq!(period.missing[0].receipt_state, RECEIPT_MISSING);
        assert!(period.missing[0].note.contains("未收到"));
    }

    #[test]
    fn 报告期写法不合法的行不计入分组但计数留痕() {
        let mut rows = vec![stored("u1", "t", 1, "0301", "2026-Q3")];
        rows.push(stored("u2", "t", 2, "0301", "2026Q3"));
        let states = BTreeMap::new();
        let roster = roster_with("2026-Q3", &[("0301", "病案室"), ("0302", "质控科")]);
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap();
        assert_eq!(report.reminder.periods.len(), 1, "只按合法报告期分组");
        assert_eq!(report.reminder.rows_without_valid_period, 1, "计数要留痕");
        // 那一行仍在未上报名单里（未被分组不等于不用改）
        assert_eq!(report.rows.len(), 2);
        assert!(report
            .rows
            .iter()
            .any(|r| r.reporting_period == "2026Q3"));
    }

    #[test]
    fn 没有名册时未收到一列为空并说明为什么() {
        let rows = vec![stored("u1", "科室一表", 2, "0301", "2026-Q3")];
        let states = BTreeMap::new();
        let roster = Roster::empty("未取到应交科室名册");
        let confirmed = std::collections::BTreeSet::new();

        let report = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap();
        let period = report.period("2026-Q3").unwrap();
        assert_eq!(period.received_count(), 1);
        assert_eq!(period.missing_count(), 0, "没名册就不能说谁没交");
        assert_eq!(report.reminder.roster_source, "未取到名册");
        assert!(report.reminder.roster_note.contains("未取到"));
    }

    #[test]
    fn 状态取值不认识时报错不猜不丢行() {
        let rows = vec![stored("u1", "t", 1, "0301", "2026-Q3")];
        let mut states = BTreeMap::new();
        states.insert("u1".to_string(), "不知道什么状态".to_string());
        let roster = Roster::empty("未取到名册");
        let confirmed = std::collections::BTreeSet::new();

        let err = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidStateTransition);
        assert!(err.message.contains("不知道什么状态"));
    }

    #[test]
    fn csv_列定义固定且每行一行() {
        let rows = vec![stored("u1", "科室一表", 2, "0301", "2026-Q3")];
        let states = BTreeMap::new();
        let roster = roster_with("2026-Q3", &[("0301", "病案室"), ("0302", "质控科")]);
        let confirmed = std::collections::BTreeSet::new();
        let report = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap();

        let csv = to_csv(&report).unwrap();
        let lines: Vec<&str> = csv.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "表头 + 1 行：\n{}", csv);
        assert!(lines[0].starts_with("来源表,行号,行键,指标编号,科室代码,科室名称,报告期,业务主键,行状态,待办原因"));
        assert!(lines[1].contains("0301"));

        let reminder = to_reminder_csv(&report).unwrap();
        let rlines: Vec<&str> = reminder.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(rlines.len(), 2, "表头 + 1 个报告期：\n{}", reminder);
        assert!(rlines[0].starts_with("报告期,已收到,未收到,说明"));
        assert!(rlines[1].contains("已收到") || rlines[1].contains("0301"));
    }

    #[test]
    fn csv_不含患者可识别字段() {
        let rows = vec![stored("u1", "科室一表", 2, "0301", "2026-Q3")];
        let states = BTreeMap::new();
        let roster = roster_with("2026-Q3", &[("0301", "病案室")]);
        let confirmed = std::collections::BTreeSet::new();
        let report = build("b1", &rows, &[], &states, &[], &confirmed, &roster).unwrap();
        let csv = to_csv(&report).unwrap();
        for banned in ["姓名", "身份证", "住院号", "病案号", "处方号", "患者"] {
            assert!(
                !csv.contains(banned),
                "未上报名单里出现了患者可识别字段名：{}",
                banned
            );
        }
    }

    #[test]
    fn 名册文件读得出且按报告期取() {
        let dir = std::env::temp_dir().join("regval-roster-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ROSTER_FILE_NAME);
        std::fs::write(
            &path,
            r#"{"reporting_periods":[{"reporting_period":"2026-Q3","dept_codes":["0301","0302"],"dept_names":{"0301":"病案室"}}]}"#,
        )
        .unwrap();
        let roster = Roster::from_file(&path).unwrap();
        let list = roster.for_period("2026-Q3");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], ("0301".to_string(), "病案室".to_string()));
        assert_eq!(list[1], ("0302".to_string(), String::new()));
        assert!(roster.for_period("2026-Q4").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 名册文件坏了报错不静默() {
        let dir = std::env::temp_dir().join("regval-roster-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ROSTER_FILE_NAME);
        std::fs::write(&path, "{ 这不是 JSON").unwrap();
        let err = Roster::from_file(&path).unwrap_err();
        assert_eq!(err.code, ErrorCode::InputUnreadable);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
