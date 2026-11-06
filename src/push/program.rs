//! 上报提交计划编译器 —— 把「一批要上报的行」编成**有条件的批量提交程序**。
//!
//! # 这层解决什么
//!
//! 一次上报要推几十行。哪几行能推、哪几行不能推、推的顺序是什么 —— 都要**先编译出来、
//! 给经办人核对过**再执行。编译器只做两件事：
//!
//! 1. **挑**：哪些行进程序，哪些行不进（幂等命中的不重推、跨表同一笔只编一行、
//!    同键异值的**不编**，标「待人工判定」交给人）；
//! 2. **串**：把挑出来的行串成条件链 —— 第 i 步只在第 i-1 步成功后执行。
//!
//! # 条件链的形状
//!
//! ```text
//! 步骤 1  行键 a…  条件 无（首批执行）
//! 步骤 2  行键 b…  条件 第 1 步成功
//! 步骤 3  行键 c…  条件 第 2 步成功
//! ```
//!
//! 上报是**串行**的：网关上一行没受理，后面的行就不该接着推 —— 推了也只是多一批要被退回的行。
//! 条件链把这条「串行」写进程序本身，而不是靠执行器临场决定。
//!
//! # 源产品能力 → 融合后行为
//!
//! 源仓库的远端执行层把一批查询编成**条件批量程序**：
//! `Program = Vec<Step{query, cond}>`、`Cond::Ok{step}`（前一步成功才走下一步），
//! 批末再挂一条 `Cond::Not{Cond::Ok{末步}}` 的条件 `ROLLBACK`（末步没成功就整批回滚）。
//!
//! 本产品**复用语义、改写对象**：
//! - 复用的是「**程序 = 步骤 + 条件**」这套装载方式与 `Cond::Ok{step}` 的链式依赖；
//! - 改写的是**步骤的内容**：源产品的步骤是一条 SQL，本产品的步骤是**上报一行**
//!   （行键 + 幂等键 + 载荷）；
//! - 批末回滚的**范围被收窄**：源产品回滚整个事务，本产品只回滚**批次汇总** ——
//!   **已在网关生效的行级回执不回滚**（网关侧已受理的事实不可撤销）。
//!   回滚那一步由执行器（批末任务）挂上，本模块只负责把前置步骤编好。
//!
//! # 编译规则
//!
//! | 行的处境 | 去向 | 理由 |
//! |---|---|---|
//! | 权威行 + 载荷没变（幂等命中） | **已跳过**清单 | 已上报过，不重复推送 |
//! | 权威行 + 跨表同一笔记录（取值一致） | 只编一行，其余**已跳过** | 同一笔记录只报一次 |
//! | 同一笔记录跨表取值**不同** | **待人工判定**，一行都不编 | 不二选一 |
//! | 权威归属还没判定（raw） | **已跳过**清单 | 不拿没判定的行去上报 |
//! | 其余权威行 | 编进步骤 | — |
//!
//! 顺序按**行键**排 —— 同一批输入两次编译出来的程序必须逐字一样，
//! 经办人核对的是同一份东西，执行器重跑也是同一份东西。

use std::collections::BTreeMap;

use crate::db::repo::StoredRow;
use crate::push::idempotency::{
    decide_revision, latest_status, ExistingSubmission, NumericField, PayloadInput,
    SubmissionPayload,
};
use crate::rules::indicators;

/// 跨表匹配键的分隔符（与 `ingest::key` 同一套，段序也一样）
const SEP: &str = "|";

/// 权威归属状态：可上报的行
const AUTHORITY_AUTHORITATIVE: &str = "authoritative";
/// 权威归属状态：跨表同一笔记录的重复行
const AUTHORITY_DUPLICATE: &str = "duplicate";
/// 权威归属状态：同键异值，待人工判定
const AUTHORITY_PENDING_MANUAL: &str = "pending_manual";

/// 条件：这一步要不要执行，取决于前面某一步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    /// 第 `step` 步**成功**才执行本步（源产品 `Cond::Ok { step }`）
    Ok {
        /// 依赖的步骤序号（从 0 起）
        step: usize,
    },
    /// 取反（源产品 `Cond::Not { cond }`）—— 批末条件回滚挂的是它
    Not {
        /// 被取反的条件
        cond: Box<Cond>,
    },
}

impl Cond {
    /// 这条条件依赖第几步（只有 [`Cond::Ok`] 有）
    pub fn depends_on(&self) -> Option<usize> {
        match self {
            Cond::Ok { step } => Some(*step),
            Cond::Not { cond } => cond.depends_on(),
        }
    }

    /// 取反：把「前一步成功才做」变成「前一步没成功才做」
    pub fn negate(self) -> Cond {
        Cond::Not {
            cond: Box::new(self),
        }
    }
}

/// 计划里的一步 = 上报一行。
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 执行条件；`None` = 无条件（第一步）
    pub cond: Option<Cond>,
    /// 这一步上报的是哪一行（行键）
    pub row_uid: String,
    /// 这一行来自哪张来源表（经办人核对时要看：是哪张科室表里的第几行）
    pub source_file: String,
    /// 这一行在来源表里的行号
    pub row_no: usize,
    /// 上报载荷（含幂等键、载荷摘要、修订号）
    pub payload: SubmissionPayload,
}

impl Step {
    /// 这一步的幂等键
    pub fn idempotency_key(&self) -> &str {
        &self.payload.idempotency_key
    }

    /// 这一步的载荷摘要
    pub fn payload_digest(&self) -> &str {
        &self.payload.payload_digest
    }

    /// 这一步的修订号
    pub fn revision(&self) -> i64 {
        self.payload.revision
    }

    /// 条件的说法（人话）
    pub fn condition_text(&self) -> String {
        match &self.cond {
            None => "无条件（首批执行）".to_string(),
            Some(Cond::Ok { step }) => format!("第 {} 步成功", step + 1),
            Some(Cond::Not { cond }) => format!("非（{}）", cond.depends_on().map(|s| s + 1).unwrap_or(0)),
        }
    }
}

/// 这行为什么没编进程序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 幂等命中：载荷没变，已经上报过（不重复推送）
    IdempotentHit,
    /// 跨表同一笔记录，取值一致 —— 编一行就够
    CrossTableDuplicate,
    /// 权威归属还没判定（raw），不拿没判定的行去上报
    NotAuthoritative,
}

impl SkipReason {
    /// 中文说法（导出与终端用）
    pub fn zh(&self) -> &'static str {
        match self {
            SkipReason::IdempotentHit => "幂等命中（载荷未变，不重复推送）",
            SkipReason::CrossTableDuplicate => "跨表同一笔记录（取值一致，只报一次）",
            SkipReason::NotAuthoritative => "权威归属未判定，不编入上报程序",
        }
    }
}

/// 一行没进程序 —— 进「已跳过」清单，供经办人核对「为什么这行没推」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRow {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: String,
    pub dept_code: String,
    pub business_key: String,
    pub idempotency_key: String,
    pub payload_digest: String,
    pub revision: i64,
    pub reason: SkipReason,
    /// 具体说明（点名对照行等）
    pub detail: String,
}

/// 一行待人工判定 —— **不进程序**，等人判。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingManualRow {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: String,
    pub dept_code: String,
    pub business_key: String,
    /// 跨表匹配键（指标编号｜科室代码｜报告期）
    pub match_key: String,
    pub idempotency_key: String,
    pub payload_digest: String,
    /// 为什么要人判
    pub detail: String,
}

/// 待编译的一行（载荷已经算好）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanCandidate {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    /// 权威归属状态（authoritative / duplicate / pending_manual / raw）
    pub authority_state: String,
    /// 跨表匹配键（指标编号｜科室代码｜报告期）
    pub match_key: String,
    /// 上报载荷（含幂等键与载荷摘要；修订号由编译器定）
    pub payload: SubmissionPayload,
}

/// 编译输入。
#[derive(Debug, Clone, Default)]
pub struct PlanInput {
    /// 批次号
    pub batch_ref: String,
    /// 待编译的行
    pub candidates: Vec<PlanCandidate>,
    /// 本机库里同一个幂等键下已存在的提交（键 → 历次提交）
    pub existing: BTreeMap<String, Vec<ExistingSubmission>>,
}

/// 编译结果：一段条件批量提交程序 + 两张清单。
///
/// - [`Program::steps`]：要执行的步骤（条件链）；
/// - [`Program::skipped`]：没编进去的行（幂等命中 / 跨表重复 / 未判定）；
/// - [`Program::pending_manual`]：待人工判定的行（**一行都没编**）。
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    steps: Vec<Step>,
    skipped: Vec<SkippedRow>,
    pending_manual: Vec<PendingManualRow>,
}

impl Program {
    /// 新建一个空程序（源产品 `Program::new`）
    pub fn new(steps: Vec<Step>) -> Program {
        Program {
            steps,
            skipped: Vec::new(),
            pending_manual: Vec::new(),
        }
    }

    /// 程序里的步骤
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// 已跳过的行
    pub fn skipped(&self) -> &[SkippedRow] {
        &self.skipped
    }

    /// 待人工判定的行
    pub fn pending_manual(&self) -> &[PendingManualRow] {
        &self.pending_manual
    }

    /// 程序是否为空（没有要执行的步骤）
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// 步骤数
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// 可读计划：每步一行（行键、幂等键、载荷摘要、修订号 + 核对用的业务字段）
    pub fn plan_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.steps.len());
        for (i, step) in self.steps.iter().enumerate() {
            lines.push(format!(
                "步骤 {}/{}  行键 {}  来源 {} 第 {} 行  指标 {}  科室 {}  业务主键 {}  \
                 幂等键 {}  载荷摘要 {}  修订 {}  条件 {}",
                i + 1,
                self.steps.len(),
                step.row_uid,
                step.source_file,
                step.row_no,
                step.payload.indicator_code,
                step.payload.dept_code,
                step.payload.business_key,
                step.idempotency_key(),
                step.payload_digest(),
                step.revision(),
                step.condition_text(),
            ));
        }
        lines
    }

    /// 计划 CSV（表头 + 每步一行 + 已跳过 + 待人工判定三段）
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        out.push_str("段,步骤,行键,来源表,行号,指标编号,科室代码,业务主键,幂等键,载荷摘要,修订,说明\n");
        for (i, step) in self.steps.iter().enumerate() {
            out.push_str(&csv_row(&[
                "待上报",
                &(i + 1).to_string(),
                &step.row_uid,
                &step.source_file,
                &step.row_no.to_string(),
                &step.payload.indicator_code,
                &step.payload.dept_code,
                &step.payload.business_key,
                step.idempotency_key(),
                step.payload_digest(),
                &step.revision().to_string(),
                &step.condition_text(),
            ]));
        }
        for s in &self.skipped {
            out.push_str(&csv_row(&[
                "已跳过",
                "",
                &s.row_uid,
                &s.source_file,
                &s.row_no.to_string(),
                &s.indicator_code,
                &s.dept_code,
                &s.business_key,
                &s.idempotency_key,
                &s.payload_digest,
                &s.revision.to_string(),
                &format!("{}（{}）", s.reason.zh(), s.detail),
            ]));
        }
        for p in &self.pending_manual {
            out.push_str(&csv_row(&[
                "待人工判定",
                "",
                &p.row_uid,
                &p.source_file,
                &p.row_no.to_string(),
                &p.indicator_code,
                &p.dept_code,
                &p.business_key,
                &p.idempotency_key,
                &p.payload_digest,
                "",
                &p.detail,
            ]));
        }
        out
    }

    /// 计划 JSON（可核对产物；步骤与两张清单都在里面）
    pub fn to_json(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                serde_json::json!({
                    "step": i + 1,
                    "row_uid": s.row_uid,
                    "source_file": s.source_file,
                    "row_no": s.row_no,
                    "indicator_code": s.payload.indicator_code,
                    "dept_code": s.payload.dept_code,
                    "business_key": s.payload.business_key,
                    "idempotency_key": s.idempotency_key(),
                    "payload_digest": s.payload_digest(),
                    "revision": s.revision(),
                    "condition": s.condition_text(),
                    "depends_on": s.cond.as_ref().and_then(|c| c.depends_on()).map(|x| x + 1),
                })
            })
            .collect();
        let skipped: Vec<serde_json::Value> = self
            .skipped
            .iter()
            .map(|s| {
                serde_json::json!({
                    "row_uid": s.row_uid,
                    "source_file": s.source_file,
                    "row_no": s.row_no,
                    "indicator_code": s.indicator_code,
                    "dept_code": s.dept_code,
                    "business_key": s.business_key,
                    "idempotency_key": s.idempotency_key,
                    "payload_digest": s.payload_digest,
                    "revision": s.revision,
                    "reason": s.reason.zh(),
                    "detail": s.detail,
                })
            })
            .collect();
        let pending: Vec<serde_json::Value> = self
            .pending_manual
            .iter()
            .map(|p| {
                serde_json::json!({
                    "row_uid": p.row_uid,
                    "source_file": p.source_file,
                    "row_no": p.row_no,
                    "indicator_code": p.indicator_code,
                    "dept_code": p.dept_code,
                    "business_key": p.business_key,
                    "match_key": p.match_key,
                    "idempotency_key": p.idempotency_key,
                    "payload_digest": p.payload_digest,
                    "detail": p.detail,
                })
            })
            .collect();
        serde_json::json!({
            "schema_version": "regval.plan/1.0.0",
            "steps": steps,
            "skipped": skipped,
            "pending_manual": pending,
            "summary": {
                "step_count": self.steps.len(),
                "skipped_count": self.skipped.len(),
                "pending_manual_count": self.pending_manual.len(),
            },
        })
        .to_string()
    }
}

/// 编译上报提交计划。
///
/// 顺序按**行键**排（同一批输入两次编译逐字一样）；挑行规则见模块文档的表。
///
/// **不报错**：没有可上报的行就是空程序（不是错误 —— 全都幂等命中是正常结局）；
/// 同键异值的行进「待人工判定」清单而不是报错（要人判，不是机器能定的事）。
pub fn compile(input: &PlanInput) -> Program {
    // 确定性排序：同一批输入两次编译出来的程序必须逐字一样
    let mut candidates = input.candidates.clone();
    candidates.sort_by(|a, b| a.row_uid.cmp(&b.row_uid));

    let mut steps: Vec<Step> = Vec::new();
    let mut skipped: Vec<SkippedRow> = Vec::new();
    let mut pending_manual: Vec<PendingManualRow> = Vec::new();

    // 先按权威归属与幂等命中挑一遍
    let mut eligible: Vec<PlanCandidate> = Vec::new();
    for c in candidates {
        match c.authority_state.as_str() {
            AUTHORITY_AUTHORITATIVE => {
                let existing = input
                    .existing
                    .get(&c.payload.idempotency_key)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let decision = decide_revision(existing, &c.payload.payload_digest);
                if decision.is_duplicate() {
                    // 跳过理由要说清「上次到底成没成」——
                    // 被退回的行和已上报的行，续跑时该做的事不一样
                    let why = match latest_status(existing) {
                        crate::push::status::REJECTED => format!(
                            "幂等键 {} 上次被网关退回，载荷摘要与那一版一致（修订号 {}）；\
                             改完数据再报会走订正（修订号 +1），载荷没变就不重复推送",
                            c.payload.idempotency_key,
                            decision.revision()
                        ),
                        _ => format!(
                            "幂等键 {} 的载荷摘要与已提交的那一版一致（修订号 {}），不重复推送",
                            c.payload.idempotency_key,
                            decision.revision()
                        ),
                    };
                    skipped.push(skipped_of(
                        &c,
                        SkipReason::IdempotentHit,
                        why,
                        decision.revision(),
                    ));
                    continue;
                }
                let mut c = c;
                c.payload.revision = decision.revision();
                eligible.push(c);
            }
            AUTHORITY_DUPLICATE => {
                skipped.push(skipped_of(
                    &c,
                    SkipReason::CrossTableDuplicate,
                    format!(
                        "跨表匹配键 {} 下与另一行是同一笔记录（取值一致），只报一次",
                        c.match_key
                    ),
                    c.payload.revision,
                ));
            }
            AUTHORITY_PENDING_MANUAL => {
                pending_manual.push(pending_of(
                    &c,
                    format!(
                        "跨表匹配键 {} 下有多行且取值不同，待人工判定（不编入上报程序）",
                        c.match_key
                    ),
                ));
            }
            _ => {
                skipped.push(skipped_of(
                    &c,
                    SkipReason::NotAuthoritative,
                    format!("权威归属状态为 {}，未判定不编入上报程序", c.authority_state),
                    c.payload.revision,
                ));
            }
        }
    }

    // 同一笔记录跨表出现：取值一致就只编一行，取值不同就**都不编**（不二选一）
    let mut by_key: BTreeMap<String, Vec<PlanCandidate>> = BTreeMap::new();
    for c in eligible {
        by_key
            .entry(c.payload.idempotency_key.clone())
            .or_default()
            .push(c);
    }
    for (idempotency_key, group) in by_key {
        if group.len() == 1 {
            let c = group.into_iter().next().expect("分组非空");
            steps.push(Step {
                cond: None, // 条件链在下面统一串
                row_uid: c.row_uid,
                source_file: c.source_file,
                row_no: c.row_no,
                payload: c.payload,
            });
            continue;
        }
        let digests: std::collections::BTreeSet<&str> = group
            .iter()
            .map(|c| c.payload.payload_digest.as_str())
            .collect();
        if digests.len() == 1 {
            // 取值一致：编第一行（行键序最小的那行），其余进已跳过清单
            let mut iter = group.into_iter();
            let keep = iter.next().expect("分组非空");
            steps.push(Step {
                cond: None,
                row_uid: keep.row_uid.clone(),
                source_file: keep.source_file,
                row_no: keep.row_no,
                payload: keep.payload,
            });
            for other in iter {
                skipped.push(skipped_of(
                    &other,
                    SkipReason::CrossTableDuplicate,
                    format!(
                        "与行 {} 是同一笔记录（跨表匹配键 {}，取值一致），只报一次",
                        keep.row_uid, other.match_key
                    ),
                    other.payload.revision,
                ));
            }
        } else {
            // 取值不同：**一行都不编**，全部待人工判定
            for c in group {
                pending_manual.push(pending_of(
                    &c,
                    format!(
                        "跨表匹配键 {} 下有多行且取值不同（幂等键 {}），待人工判定 —— \
                         不自动二选一，也不编入上报程序",
                        c.match_key, idempotency_key
                    ),
                ));
            }
        }
    }

    // 串条件链：第 i 步只在第 i-1 步成功后执行（源产品 `Cond::Ok { step: i - 1 }`）
    //
    // 分组那一步是按幂等键归并的，出来时不是行键序 —— 这里再按**行键**排回去：
    // 待推送行的确定顺序是「批次内按 row_uid 排序」（任务 30 的编译规则）。
    steps.sort_by(|a, b| a.row_uid.cmp(&b.row_uid));
    let mut chained: Vec<Step> = Vec::with_capacity(steps.len());
    for (i, mut s) in steps.into_iter().enumerate() {
        s.cond = if i == 0 {
            None
        } else {
            Some(Cond::Ok { step: i - 1 })
        };
        chained.push(s);
    }

    skipped.sort_by(|a, b| a.row_uid.cmp(&b.row_uid));
    pending_manual.sort_by(|a, b| a.row_uid.cmp(&b.row_uid));

    Program {
        steps: chained,
        skipped,
        pending_manual,
    }
}

/// 把库里的一行编成候选（含载荷）。
///
/// 载荷的数值字段取**原文**优先（`numeric_raw`）—— 填坏了就保留原文，
/// 不拿库里那个已解析成 `f64` 的值把「填坏了」抹平。
pub fn candidate_for_row(
    row: &StoredRow,
    batch_ref: &str,
    bases: &BTreeMap<String, String>,
) -> PlanCandidate {
    let (numerator, denominator, value) = numeric_fields(row);
    let basis = bases
        .get(row.indicator_code.trim())
        .cloned()
        .unwrap_or_else(|| indicators::BASIS.to_string());
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref,
        org_code: &row.org_code,
        reporting_period: &row.reporting_period,
        indicator_code: &row.indicator_code,
        dept_code: &row.dept_code,
        business_key: &row.business_key,
        numerator,
        denominator,
        value,
        unit: &row.unit,
        basis: &basis,
        rule_version: &row.rule_version,
    });
    PlanCandidate {
        row_uid: row.row_uid.clone(),
        source_ns: row.source_ns.clone(),
        source_file: row.source_file.clone(),
        row_no: row.row_no,
        authority_state: row.authority_state.clone(),
        match_key: match_key_of(row),
        payload,
    }
}

/// 跨表匹配键：指标编号｜科室代码｜报告期（段序与 `ingest::key::match_key` 一致）
pub fn match_key_of(row: &StoredRow) -> String {
    [
        row.indicator_code.trim(),
        row.dept_code.trim(),
        row.reporting_period.trim(),
    ]
    .join(SEP)
}

/// 数值字段的取法：原文优先，取不到才回落到库里已解析的值。
///
/// 原文优先的理由：填坏了的数值（「abc」）在库里没有 `f64`，
/// 回落会把「填坏了」变成「空」—— 两回事，报错码都不一样。
pub fn numeric_fields(row: &StoredRow) -> (NumericField, NumericField, NumericField) {
    let raw: serde_json::Value =
        serde_json::from_str(&row.numeric_raw).unwrap_or(serde_json::Value::Null);
    let pick = |key: &str, fallback: Option<f64>| -> NumericField {
        let text = raw.get(key).and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            match fallback {
                Some(v) => NumericField::Number(v),
                None => NumericField::Empty,
            }
        } else {
            NumericField::from_raw(text)
        }
    };
    (
        pick("numerator", row.numerator),
        pick("denominator", row.denominator),
        pick("value", row.value),
    )
}

/// 造一条「已跳过」记录
fn skipped_of(c: &PlanCandidate, reason: SkipReason, detail: String, revision: i64) -> SkippedRow {
    SkippedRow {
        row_uid: c.row_uid.clone(),
        source_ns: c.source_ns.clone(),
        source_file: c.source_file.clone(),
        row_no: c.row_no,
        indicator_code: c.payload.indicator_code.clone(),
        dept_code: c.payload.dept_code.clone(),
        business_key: c.payload.business_key.clone(),
        idempotency_key: c.payload.idempotency_key.clone(),
        payload_digest: c.payload.payload_digest.clone(),
        revision,
        reason,
        detail,
    }
}

/// 造一条「待人工判定」记录
fn pending_of(c: &PlanCandidate, detail: String) -> PendingManualRow {
    PendingManualRow {
        row_uid: c.row_uid.clone(),
        source_ns: c.source_ns.clone(),
        source_file: c.source_file.clone(),
        row_no: c.row_no,
        indicator_code: c.payload.indicator_code.clone(),
        dept_code: c.payload.dept_code.clone(),
        business_key: c.payload.business_key.clone(),
        match_key: c.match_key.clone(),
        idempotency_key: c.payload.idempotency_key.clone(),
        payload_digest: c.payload.payload_digest.clone(),
        detail,
    }
}

/// CSV 一行（值里的逗号与引号按 CSV 规则转义）
fn csv_row(values: &[&str]) -> String {
    let mut parts = Vec::with_capacity(values.len());
    for v in values {
        if v.contains(',') || v.contains('"') || v.contains('\n') {
            parts.push(format!("\"{}\"", v.replace('"', "\"\"")));
        } else {
            parts.push((*v).to_string());
        }
    }
    format!("{}\n", parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个候选行
    fn candidate(row_uid: &str, state: &str, key: &str, digest: &str) -> PlanCandidate {
        let mut payload = SubmissionPayload::new(PayloadInput {
            batch_ref: "b-1",
            org_code: "ORG-1",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: key,
            numerator: NumericField::Number(96.0),
            denominator: NumericField::Number(100.0),
            value: NumericField::Number(96.0),
            unit: "%",
            basis: "国卫办医函〔2021〕28号 附件",
            rule_version: "2021.1",
        });
        payload.payload_digest = digest.to_string();
        PlanCandidate {
            row_uid: row_uid.to_string(),
            source_ns: "科室一表".to_string(),
            source_file: "科室一表.csv".to_string(),
            row_no: 1,
            authority_state: state.to_string(),
            match_key: "MER-TL-04|0301|2026-Q3".to_string(),
            payload,
        }
    }

    #[test]
    fn 条件链_第二步起都依赖前一步() {
        let input = PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![
                candidate("r3", AUTHORITY_AUTHORITATIVE, "BK-3", "d3"),
                candidate("r1", AUTHORITY_AUTHORITATIVE, "BK-1", "d1"),
                candidate("r2", AUTHORITY_AUTHORITATIVE, "BK-2", "d2"),
            ],
            existing: BTreeMap::new(),
        };
        let p = compile(&input);
        assert_eq!(p.len(), 3);
        // 顺序按行键
        assert_eq!(p.steps()[0].row_uid, "r1");
        assert_eq!(p.steps()[1].row_uid, "r2");
        assert_eq!(p.steps()[2].row_uid, "r3");
        // 第一步无条件，后面每一步依赖前一步
        assert!(p.steps()[0].cond.is_none());
        assert_eq!(p.steps()[1].cond, Some(Cond::Ok { step: 0 }));
        assert_eq!(p.steps()[2].cond, Some(Cond::Ok { step: 1 }));
        assert_eq!(p.steps()[2].cond.as_ref().unwrap().depends_on(), Some(1));
    }

    #[test]
    fn 幂等命中的行不进程序但进已跳过清单() {
        let mut existing = BTreeMap::new();
        existing.insert(
            "idk-1".to_string(),
            vec![ExistingSubmission {
                revision: 1,
                payload_digest: "d1".to_string(),
                status: crate::push::status::ACCEPTED.to_string(),
            }],
        );
        let mut c = candidate("r1", AUTHORITY_AUTHORITATIVE, "BK-1", "d1");
        c.payload.idempotency_key = "idk-1".to_string();
        let input = PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![c],
            existing,
        };
        let p = compile(&input);
        assert!(p.is_empty());
        assert_eq!(p.skipped().len(), 1);
        assert_eq!(p.skipped()[0].reason, SkipReason::IdempotentHit);
        assert_eq!(p.skipped()[0].revision, 1);
    }

    #[test]
    fn 空待推送集合编译为空程序且不报错() {
        let p = compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![],
            existing: BTreeMap::new(),
        });
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
        assert!(p.skipped().is_empty());
        assert!(p.pending_manual().is_empty());
    }

    #[test]
    fn 同键异值两行都不编_进待人工判定() {
        let mut a = candidate("r1", AUTHORITY_AUTHORITATIVE, "BK-1", "d1");
        a.payload.idempotency_key = "idk-1".to_string();
        let mut b = candidate("r2", AUTHORITY_AUTHORITATIVE, "BK-1", "d2");
        b.payload.idempotency_key = "idk-1".to_string();
        b.source_file = "科室二表.csv".to_string();
        let p = compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![a, b],
            existing: BTreeMap::new(),
        });
        assert!(p.is_empty(), "同键异值不该编进程序");
        assert_eq!(p.pending_manual().len(), 2);
        assert!(p.pending_manual()[0].detail.contains("待人工判定"));
    }

    #[test]
    fn 同键同值只编一行() {
        let mut a = candidate("r1", AUTHORITY_AUTHORITATIVE, "BK-1", "d1");
        a.payload.idempotency_key = "idk-1".to_string();
        let mut b = candidate("r2", AUTHORITY_AUTHORITATIVE, "BK-1", "d1");
        b.payload.idempotency_key = "idk-1".to_string();
        b.source_file = "科室二表.csv".to_string();
        let p = compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![a, b],
            existing: BTreeMap::new(),
        });
        assert_eq!(p.len(), 1);
        assert_eq!(p.skipped().len(), 1);
        assert_eq!(p.skipped()[0].reason, SkipReason::CrossTableDuplicate);
    }

    #[test]
    fn 非权威行不进程序() {
        let p = compile(&PlanInput {
            batch_ref: "b-1".to_string(),
            candidates: vec![candidate("r1", "raw", "BK-1", "d1")],
            existing: BTreeMap::new(),
        });
        assert!(p.is_empty());
        assert_eq!(p.skipped()[0].reason, SkipReason::NotAuthoritative);
    }

    #[test]
    fn 条件取反后依赖不变() {
        let c = Cond::Ok { step: 2 }.negate();
        assert_eq!(c.depends_on(), Some(2));
    }
}
