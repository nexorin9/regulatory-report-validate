//! 结果契约：引擎与表格插件（Excel 宏 / WPS JSA / CSV 直跑）之间的唯一交接面。
//!
//! 契约带 `schema_version`：插件按此版本判断能否写回，版本不符必须拒绝写回
//! （错误码 `CONTRACT_VERSION_MISMATCH`，Task 3 落地）。
//!
//! 契约里写回原表的 5 个新列取值，全部由引擎算好，宏不自行统计、不猜口径。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dupe::distance::DistanceStatusSection;
use crate::dupe::merge::SemanticStatusSection;
use crate::dupe::SuspectedDuplicate;
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::RowMeta;
use crate::ingest::IngestSection;
use crate::model::audit::ModelStatusSection;
use crate::push::dept::DeptDirectorySection;
use crate::report::summary::IssueIndex;
use crate::rules::model::Issue;

/// 规则清单段（本次校验用的是哪一套口径、逐条规则是什么状态）。
///
/// 与 `regval rules --out` 写出的清单是同一份结构 —— 表格插件与运维看的是同一套说法。
pub use crate::rules::loader::{RuleManifest, RuleStatusEntry, RuleStatusSection};

/// 校验结论取值。**唯一出处**在 [`crate::report::summary`] —— 契约写回与依据举证包共用。
pub use crate::report::summary::{CONCLUSION_BLOCKED, CONCLUSION_HINT, CONCLUSION_PASS};

/// 契约版本。改动列定义或行结果结构时必须升版本。
pub const SCHEMA_VERSION: &str = "regval.contract/1.0.0";

/// 写回原表的 5 个新列（顺序即写回顺序，接在原表已有列之后）
pub const NEW_COLUMNS: [&str; 5] = [
    "校验结论",
    "问题类型",
    "问题说明（含依据）",
    "疑似重复项",
    "上报状态",
];

/// 建议列宽（Excel 字符宽）—— 宽度的唯一出处是视觉 token（`src/ui/tokens.rs`），
/// 宏与文档用的是同一份，改一处两边一起变。
const COLUMN_WIDTHS: [f64; 5] = crate::ui::tokens::column_widths();

/// 行未上报时的状态取值
pub const STATUS_NOT_SUBMITTED: &str = "未上报";

/// 一个新列的写回定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractColumn {
    /// 列名
    pub name: String,
    /// 写回序号（1 起，接在原表已有列之后）
    pub index: usize,
    /// 建议列宽
    pub width: f64,
}

/// 运行摘要（宏的状态条直接取这里的数，不自己统计）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// 来源表张数（Excel 多工作表时 >1）
    pub tables: usize,
    /// 校验行数
    pub total_rows: usize,
    /// 有问题的行数
    pub rows_with_issues: usize,
    /// 问题总数
    pub issue_count: usize,
    /// 按问题类型计数
    pub issue_count_by_kind: BTreeMap<String, usize>,
    /// 疑似重复候选对数（重复检测漏斗产出；与口径问题分开计数 —— 候选要人确认，不是不合格）
    #[serde(default)]
    pub suspected_duplicate_count: usize,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// 科室主数据是否核对过（false = 目录不可用，未核对）
    pub dept_master_checked: bool,
    /// 未核对时的提示（核对过时为空串）
    pub dept_master_note: String,
    /// 状态条一行摘要（**成品句子**，表格插件原样显示）
    ///
    /// 句子的唯一出处是 [`crate::ui::statusbar`]：插件不自行统计、不自行拼句子，
    /// 否则表里与终端里会各说一套。
    #[serde(default)]
    pub status_line: String,
    /// 上报完成后的状态条一行摘要（同样是成品句子）
    #[serde(default)]
    pub submission_line: String,
    /// 已报上去的行数（网关已受理 + 幂等命中；**回执驱动**，不是本地自述）
    #[serde(default)]
    pub submitted_rows: usize,
    /// 读到的数据行总数（含空行与完全无效行）
    ///
    /// 跟 `total_rows` 的差就是「这一批跳过了几行」—— 差得不明不白时，
    /// 经办人就得回原表数一遍，所以这个数必须摆出来。
    #[serde(default)]
    pub source_rows: usize,
    /// 空行数（导出时多出来的空行，不参与校验但计数留痕）
    #[serde(default)]
    pub blank_rows: usize,
    /// 完全无效行数（重复表头、分隔线、合计行等；不参与校验但计数留痕）
    #[serde(default)]
    pub invalid_rows: usize,
    /// 被跳过的行数（空行 + 完全无效行）
    #[serde(default)]
    pub skipped_rows: usize,
    /// 跳过行的说明句（没有跳过行时为空串）
    #[serde(default)]
    pub skipped_note: String,
}

/// 一行的校验结果（对应原表一个新行）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowResult {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: Option<String>,
    pub dept_code: Option<String>,
    pub reporting_period: Option<String>,
    pub business_key: Option<String>,
    /// 「校验结论」列取值：通过 / 有提示 / 不通过
    pub conclusion: String,
    /// 「问题类型」列取值（多类以「、」连接）
    pub issue_types: String,
    /// 「问题说明（含依据）」列取值
    pub issue_detail: String,
    /// 「疑似重复项」列取值（重复检测漏斗在 Task 20 起填）
    pub suspected_duplicate: String,
    /// 「上报状态」列取值（只由网关回执驱动，模型永不推进）
    pub submission_status: String,
}

/// 一条口径问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueEntry {
    pub row_uid: String,
    pub rule_id: String,
    pub issue_kind: String,
    pub severity: String,
    pub message: String,
    pub basis: String,
    pub missing_columns: Vec<String>,
    /// 机器可读错误码；口径规则命中时为空串
    #[serde(default)]
    pub error_code: String,
}

/// 组装契约时的附加信息（实体绑定层与重复检测漏斗的结果）
#[derive(Debug, Clone, Default)]
pub struct Extra {
    /// 行键 → 「疑似重复项」列取值
    pub suspected: BTreeMap<String, String>,
    /// 疑似重复候选对（重复检测漏斗产出；只标疑似，不合并不推送）
    pub duplicates: Vec<SuspectedDuplicate>,
    /// 科室主数据是否核对过
    pub dept_master_checked: bool,
    /// 未核对时的提示
    pub dept_master_note: String,
    /// 重复检测第三级（编辑距离候选）的运行参数、计数与距离分布直方
    pub dupe_distance: DistanceStatusSection,
    /// 重复检测第四级（模型语义归并）的状态与计数
    pub dupe_semantic: SemanticStatusSection,
    /// 行键 → 「上报状态」列取值（只由网关回执驱动，模型永不推进）
    ///
    /// 没有这一项的行 = 还没上报过（`未上报`）。回执说已上报就是已上报，
    /// 回执说被退回就是被退回 —— 本地时钟与模型都不推进这一列。
    pub submission_status: BTreeMap<String, String>,
    /// 读表阶段的记录：跳过了几行、为什么跳、这张表是什么编码与分隔符
    pub ingest: IngestSection,
}

/// 结果契约
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub schema_version: String,
    pub generated_at: String,
    pub batch_ref: String,
    /// 写回原表的 5 个新列定义
    pub columns: Vec<ContractColumn>,
    pub summary: Summary,
    pub rows: Vec<RowResult>,
    pub issues: Vec<IssueEntry>,
    /// 疑似重复候选对（重复检测漏斗产出）。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。行级标注在 `rows[].suspected_duplicate`，
    /// 这一段给的是成对的明细（谁和谁像、命中哪一级、依据是什么）。
    #[serde(default)]
    pub suspected_duplicates: Vec<SuspectedDuplicate>,
    /// 本次用的口径规则清单（规则集版本、指纹、逐条规则状态）。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。留这一段是为了让「这批结论按哪套口径算的」有据可查。
    #[serde(default)]
    pub rule_status: RuleStatusSection,
    /// 重复检测第三级（编辑距离候选）的运行参数、计数与**距离分布直方**。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。直方是「距离落在哪一档的行对最多」的可核对产物 ——
    /// 阈值调松调紧，效果一眼看得见。
    #[serde(default)]
    pub dupe_distance: DistanceStatusSection,
    /// 重复检测第四级（模型语义归并）的状态与计数。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。段里写清「模型提了几条 · 几条够置信度 · 几条降级为
    /// 疑似待人工判定」—— 读的人一眼就知道第四级这一轮做了什么。
    #[serde(default)]
    pub dupe_semantic: SemanticStatusSection,
    /// 本轮模型端点的状态（有没有接、成了没成、降级了没有、模型提了什么）。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。留这一段是为了让「这一批的疑似项里模型掺和了没有」
    /// 有据可查 —— 模型不可用时它说 `degraded`，疑似项来源标 `deterministic`，
    /// 读的人一眼就知道这批结论没经过模型那一关。
    #[serde(default)]
    pub model: ModelStatusSection,
    /// 本轮科室主数据目录的状态（有没有核对过、目录多少条、数据时间、来源）。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。留这一段是为了让「这批结论的科室维度到底查没查过、
    /// 用的是哪一份主数据、那份主数据是什么时候的」有据可查 ——
    /// 目录拉不到时 `checked = false`，这一批的科室维度**没查**，不能当成查过了。
    #[serde(default)]
    pub dept_directory: DeptDirectorySection,
    /// 本轮读表的记录：跳过了几行、为什么跳、这张表是什么编码与分隔符。
    ///
    /// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
    /// 因此契约版本不升。留这一段是为了让「表里 200 行、校验了 197 行，
    /// 另外 3 行去哪了」有据可查 —— 不静默丢行。
    #[serde(default)]
    pub ingest: IngestSection,
}

/// 契约的**可写回视图**：表格插件（Excel 宏 / WPS JSA）真正消费的三段 —— 版本、5 个新列、行结果。
///
/// 插件按 `schema_version` 判断能否写回；版本不符即中止，不做部分写回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSchema {
    pub schema_version: String,
    /// 写回原表的 5 个新列定义（顺序即写回顺序）
    pub columns: Vec<ContractColumn>,
    /// 行结果（按来源表分组，`source_ns` 为工作表名）
    pub rows: Vec<RowResult>,
}

impl ContractSchema {
    /// 5 个新列的列名（插件与引擎共用同一份列名定义）
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }
}

impl Contract {
    /// 取可写回视图
    pub fn schema(&self) -> ContractSchema {
        ContractSchema {
            schema_version: self.schema_version.clone(),
            columns: self.columns.clone(),
            rows: self.rows.clone(),
        }
    }
}

/// 5 个新列的列名（插件与引擎共用同一份定义）
pub fn column_names() -> Vec<String> {
    NEW_COLUMNS.iter().map(|s| s.to_string()).collect()
}

/// 校验契约版本。
///
/// 版本不符一律拒绝写回（错误码 `CONTRACT_VERSION_MISMATCH`）——
/// 宁可让经办人升级宏，也不能让半张表按旧口径写下去。
pub fn check_schema_version(actual: &str) -> Result<(), EngineError> {
    if actual == SCHEMA_VERSION {
        return Ok(());
    }
    Err(EngineError::at(
        ErrorCode::ContractVersionMismatch,
        format!(
            "契约版本不一致：引擎为 {}，收到的为 {}（请升级表格插件或引擎）",
            SCHEMA_VERSION, actual
        ),
        format!("schema_version={}", actual),
    ))
}

/// 5 个新列的写回定义
pub fn columns() -> Vec<ContractColumn> {
    NEW_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, name)| ContractColumn {
            name: (*name).to_string(),
            index: i + 1,
            width: COLUMN_WIDTHS[i],
        })
        .collect()
}

/// 由行标识与问题集合组装契约。
///
/// 三列取值（校验结论 / 问题类型 / 问题说明（含依据））由 [`IssueIndex`] 算出：
/// 行维度与类型维度出自**同一份索引**，与依据举证包同源，两边不会给出两套说法。
pub fn build(
    batch_ref: &str,
    metas: &[RowMeta],
    issues: Vec<Issue>,
    tables: usize,
    elapsed_ms: u64,
    extra: &Extra,
) -> Contract {
    // 按行键归集问题（行维度与类型维度共用这一份索引）
    let index = IssueIndex::build(&issues);

    let mut rows: Vec<RowResult> = Vec::with_capacity(metas.len());
    let mut rows_with_issues = 0usize;
    let issue_count_by_kind: BTreeMap<String, usize> = index.count_by_kind();

    for meta in metas {
        let row_issues = index.of_row(&meta.row_uid);
        let (conclusion, issue_types, issue_detail) = if row_issues.is_empty() {
            (CONCLUSION_PASS.to_string(), String::new(), String::new())
        } else {
            rows_with_issues += 1;
            (
                crate::report::summary::conclusion_of(row_issues).to_string(),
                index.kinds_of_row(&meta.row_uid),
                index.detail_of_row(&meta.row_uid),
            )
        };

        rows.push(RowResult {
            row_uid: meta.row_uid.clone(),
            source_ns: meta.source_ns.clone(),
            source_file: meta.source_file.clone(),
            row_no: meta.row_no,
            indicator_code: meta.indicator_code.clone(),
            dept_code: meta.dept_code.clone(),
            reporting_period: meta.reporting_period.clone(),
            business_key: meta.business_key.clone(),
            conclusion,
            issue_types,
            issue_detail,
            suspected_duplicate: extra
                .suspected
                .get(&meta.row_uid)
                .cloned()
                .unwrap_or_default(),
            submission_status: extra
                .submission_status
                .get(&meta.row_uid)
                .cloned()
                .unwrap_or_else(|| STATUS_NOT_SUBMITTED.to_string()),
        });
    }

    let issue_entries: Vec<IssueEntry> = issues
        .into_iter()
        .map(|i| IssueEntry {
            row_uid: i.row_uid,
            rule_id: i.rule_id,
            issue_kind: i.issue_kind,
            severity: i.severity,
            message: i.message,
            basis: i.basis,
            missing_columns: i.missing_columns,
            error_code: i.error_code,
        })
        .collect();

    // 状态条两句成品话：插件原样显示，宏不自行统计（见 ui::statusbar）
    let submitted_rows = rows
        .iter()
        .filter(|r| crate::ui::statusbar::is_submitted(&r.submission_status))
        .count();
    let status_line = crate::ui::statusbar::status_line(
        rows.len(),
        issue_entries.len(),
        extra.duplicates.len(),
    );
    let submission_line = crate::ui::statusbar::submission_line(submitted_rows);

    Contract {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: crate::db::repo::now_iso8601(),
        batch_ref: batch_ref.to_string(),
        columns: columns(),
        summary: Summary {
            tables,
            total_rows: rows.len(),
            rows_with_issues,
            issue_count: issue_entries.len(),
            issue_count_by_kind,
            suspected_duplicate_count: extra.duplicates.len(),
            elapsed_ms,
            dept_master_checked: extra.dept_master_checked,
            dept_master_note: extra.dept_master_note.clone(),
            status_line,
            submission_line,
            submitted_rows,
            // 读表阶段的计数：跳过了几行、为什么跳（不静默丢行）
            source_rows: extra.ingest.source_rows,
            blank_rows: extra.ingest.blank_rows,
            invalid_rows: extra.ingest.invalid_rows,
            skipped_rows: extra.ingest.skipped_rows,
            skipped_note: extra.ingest.skipped_note.clone(),
        },
        rows,
        issues: issue_entries,
        suspected_duplicates: extra.duplicates.clone(),
        rule_status: RuleStatusSection::default(),
        dupe_distance: extra.dupe_distance.clone(),
        dupe_semantic: extra.dupe_semantic.clone(),
        // 模型段由调用方在装配后填（validate 里跑完模型端点才知道这一轮什么状态）；
        // 这里先给「未接入」的缺省值 —— 契约永远有这一段，插件读法不变。
        model: ModelStatusSection::not_attempted(),
        // 科室主数据目录段同理：调用方在解析完目录后填，这里先给「不可用」的缺省值。
        dept_directory: DeptDirectorySection::unavailable("科室主数据未校验"),
        // 读表记录：调用方读完表后填（跳过了几行、什么编码）
        ingest: extra.ingest.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(uid: &str) -> RowMeta {
        RowMeta {
            row_uid: uid.to_string(),
            source_ns: "t".into(),
            source_file: "t.csv".into(),
            row_no: 1,
            indicator_code: Some("MER-HR-01".into()),
            dept_code: Some("0101".into()),
            reporting_period: Some("2026-Q3".into()),
            business_key: Some("BK-001".into()),
        }
    }

    #[test]
    fn 五个新列按固定顺序给出() {
        let cols = columns();
        assert_eq!(cols.len(), 5);
        assert_eq!(cols[0].name, "校验结论");
        assert_eq!(cols[4].name, "上报状态");
        assert_eq!(cols[2].index, 3);
    }

    #[test]
    fn 阻断问题判不通过且带依据() {
        let issue = Issue {
            row_uid: "u1".into(),
            rule_id: "R-REQUIRED-001".into(),
            issue_kind: "必填缺失".into(),
            severity: "阻断".into(),
            message: "缺少必填列「科室代码」".into(),
            basis: "依据X".into(),
            issue_key: "科室代码".into(),
            missing_columns: vec!["科室代码".into()],
            error_code: String::new(),
        };
        let c = build("b1", &[meta("u1")], vec![issue], 1, 3, &Extra::default());
        assert_eq!(c.rows[0].conclusion, CONCLUSION_BLOCKED);
        assert!(c.rows[0].issue_detail.contains("依据X"));
        assert_eq!(c.summary.issue_count, 1);
        assert_eq!(c.summary.rows_with_issues, 1);
    }

    #[test]
    fn 无问题行判通过且仍在结果里() {
        let c = build("b1", &[meta("u1")], vec![], 1, 1, &Extra::default());
        assert_eq!(c.rows.len(), 1);
        assert_eq!(c.rows[0].conclusion, CONCLUSION_PASS);
        assert_eq!(c.rows[0].issue_types, "");
    }

    #[test]
    fn 状态条句子由引擎算好_插件原样显示() {
        let c = build("b1", &[meta("u1"), meta("u2")], vec![], 1, 1, &Extra::default());
        assert_eq!(
            c.summary.status_line,
            "已校验 2 行 · 未发现口径问题",
            "插件显示的就是这句，宏不自行统计（零问题走空结果态的说法）"
        );
        assert_eq!(c.summary.submitted_rows, 0);
        assert!(c.summary.submission_line.contains("0 行已提交"));
    }

    #[test]
    fn 疑似重复项按行键写进对应行() {
        let mut suspected = BTreeMap::new();
        suspected.insert(
            "u2".to_string(),
            "疑似重复：与 表一 第 1 行取值完全一致".to_string(),
        );
        let extra = Extra {
            suspected,
            duplicates: Vec::new(),
            dept_master_checked: false,
            dept_master_note: "科室主数据未校验".to_string(),
            dupe_distance: DistanceStatusSection::default(),
            dupe_semantic: SemanticStatusSection::default(),
            submission_status: BTreeMap::new(),
            ingest: IngestSection::default(),
        };
        let c = build("b1", &[meta("u1"), meta("u2")], vec![], 1, 1, &extra);
        assert_eq!(c.rows[0].suspected_duplicate, "");
        assert!(c.rows[1].suspected_duplicate.contains("疑似重复"));
        assert!(!c.summary.dept_master_checked);
        assert_eq!(c.summary.dept_master_note, "科室主数据未校验");
    }
}
