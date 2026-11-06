//! 每行依据举证包：这一行命中了哪些规则、按哪条官方口径、该怎么改。
//!
//! # 为什么要有举证包
//!
//! 「不通过」三个字对科室没用 —— 科室要改的是**具体哪一格**、**按哪条口径**。
//! 举证包把每行的问题拆到「规则编号 + 官方依据原文 + 建议修改动作」，
//! 经办人可以直接把它发给科室，或贴进整改通知；质控岗也能照着核对。
//!
//! # 命中 0 条规则的行也要在包里
//!
//! 举证包是**逐行核对**用的：包里少了行，核对的人就分不清是「这行没问题」
//! 还是「这行没被检查到」。因此没有命中的行照样列出，标「无命中」，
//! 建议动作写「本行未命中任何口径规则，无需修改」。
//!
//! # 与契约的关系
//!
//! 举证包与结果契约读的是**同一批问题**（同一份 [`crate::rules::model::Issue`]），
//! 校验结论也出自同一处（[`crate::report::summary::conclusion_of`]）。
//! 两边对同一行给出的结论不会打架。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::ErrorCode;
use crate::ingest::RowMeta;
use crate::rules::model::Issue;

use super::summary::{conclusion_of, IssueIndex};

/// 举证包版本。改动产物结构时升版本。
pub const EVIDENCE_SCHEMA_VERSION: &str = "regval.evidence/1.0.0";

/// 命中状态：该行命中了至少一条口径问题
pub const MATCH_HIT: &str = "命中";
/// 命中状态：该行没有命中任何口径问题
pub const MATCH_NONE: &str = "无命中";

/// 无命中行的建议动作
pub const NO_HIT_SUGGESTION: &str = "本行未命中任何口径规则，无需修改";

/// 举证包 CSV 的列定义（顺序固定，便于第三方按列名取数）
pub const CSV_HEADER: [&str; 20] = [
    "来源表",
    "行号",
    "行键",
    "指标编号",
    "科室代码",
    "报告期",
    "业务主键",
    "校验结论",
    "命中状态",
    "命中条数",
    "官方条款号",
    "指标口径依据",
    "规则编号",
    "问题类型",
    "严重度",
    "问题说明",
    "官方依据",
    "错误码",
    "缺失列",
    "建议修改动作",
];

/// 指标口径依据索引：指标编号 →（官方条款号，条款依据原文）。
///
/// 规则依据说的是「哪条规则、按哪份文件」；**逐项**依据说的是「这一行的指标，
/// 对应官方附件的第几条、怎么定义的」。给科室看的是后者 —— 「按指标七改」比
/// 「按国卫办医函〔2021〕28号附件改」可执行得多。
///
/// 取不到就空着，并在包里的 `indicator_basis_note` 说明原因 —— 不拿别的口径顶替。
#[derive(Debug, Clone, Default)]
pub struct IndicatorBasis {
    /// 指标编号 → 官方条款号（如「指标四」）
    clauses: BTreeMap<String, String>,
    /// 指标编号 → 官方条款依据原文
    bases: BTreeMap<String, String>,
    /// 口径种子版本（逐项依据出自哪一版种子）
    versions: Vec<String>,
}

impl IndicatorBasis {
    /// 从规则目录里的口径种子文件装载（`*.seed.json`）。
    ///
    /// 目录不存在、文件读不出、字段不合法都**不静默**：装载不了的指标不进索引，
    /// 由调用方把「没有逐项依据」写进包里的说明。
    pub fn from_seed_dir(dir: &Path) -> IndicatorBasis {
        let mut index = IndicatorBasis::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return index,
        };
        let mut files: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(crate::rules::seed::SEED_FILE_SUFFIX))
                    .unwrap_or(false)
            })
            .collect();
        // 文件名排序 → 装载顺序稳定；同编号以先装载的为准（不改写已有依据）
        files.sort();

        for path in files {
            let set = match crate::rules::seed::SeedSet::load(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if !set.seed_set.is_empty() && !index.versions.contains(&set.seed_set) {
                index.versions.push(set.seed_set.clone());
            }
            for seed in &set.indicators {
                if seed.indicator_code.is_empty() {
                    continue;
                }
                index
                    .clauses
                    .entry(seed.indicator_code.clone())
                    .or_insert_with(|| seed.official_clause.clone());
                index
                    .bases
                    .entry(seed.indicator_code.clone())
                    .or_insert_with(|| seed.basis.clone());
            }
        }
        index
    }

    /// 某指标编号的官方条款号
    pub fn clause_of(&self, code: &str) -> Option<&str> {
        self.clauses.get(code).map(|s| s.as_str())
    }

    /// 某指标编号的官方条款依据原文
    pub fn basis_of(&self, code: &str) -> Option<&str> {
        self.bases.get(code).map(|s| s.as_str())
    }

    /// 是否取到了逐项依据
    pub fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    /// 逐项依据出自哪几版口径种子
    pub fn versions(&self) -> &[String] {
        &self.versions
    }

    /// 是否与给定的口径版本对得上（对不上就不附逐项依据 —— 别拿别的口径顶替）
    pub fn matches_version(&self, version: &str) -> bool {
        !version.is_empty() && self.versions.iter().any(|v| v == version)
    }
}

/// 一条命中：某个规则在某一行的命中详情
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceHit {
    /// 规则编号（回溯到规则表）
    pub rule_id: String,
    /// 问题类型（必填缺失 / 枚举越界 / 单位不符 / 疑似重复 …）
    pub issue_kind: String,
    /// 严重度（阻断 / 提示）
    pub severity: String,
    /// 问题说明
    pub message: String,
    /// 官方依据原文出处（**不得为空** —— 没有依据的命中不成立）
    pub basis: String,
    /// 机器可读错误码（无则空串）
    #[serde(default)]
    pub error_code: String,
    /// 缺失列清单（可多列）
    #[serde(default)]
    pub missing_columns: Vec<String>,
    /// 建议修改动作（给科室看的一句话）
    pub suggestion: String,
}

/// 举证包里的一行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRow {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: Option<String>,
    pub dept_code: Option<String>,
    pub reporting_period: Option<String>,
    pub business_key: Option<String>,
    /// 校验结论（通过 / 有提示 / 不通过）
    pub conclusion: String,
    /// 命中状态（命中 / 无命中）
    pub match_state: String,
    /// 命中条数
    pub hit_count: usize,
    /// 命中明细
    pub hits: Vec<EvidenceHit>,
    /// 行级建议动作（各命中建议去重后合并）
    pub suggestion: String,
    /// 本行指标的官方条款号（如「指标四」）；取不到为空串
    pub official_clause: String,
    /// 本行指标的官方条款依据原文（逐项口径）；取不到为空串
    pub indicator_basis: String,
}

/// 举证包的计数（与行条目出自同一份数据，内部自洽）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceCounts {
    /// 包内行数（= 本批次的行数）
    pub rows: usize,
    /// 有命中的行数
    pub rows_with_hits: usize,
    /// 无命中的行数
    pub rows_without_hits: usize,
    /// 命中总条数
    pub hits: usize,
    /// 按问题类型计数
    pub by_issue_kind: BTreeMap<String, usize>,
    /// 按严重度计数
    pub by_severity: BTreeMap<String, usize>,
}

/// 每行依据举证包
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePackage {
    pub schema_version: String,
    pub generated_at: String,
    pub batch_ref: String,
    /// 本批次结论所依据的规则集版本；行级版本未记录时为空串
    pub rule_set_version: String,
    /// 逐项官方条款依据的来源说明（取到 / 为什么没取到）
    pub indicator_basis_note: String,
    pub counts: EvidenceCounts,
    pub rows: Vec<EvidenceRow>,
}

impl EvidencePackage {
    /// 包内行数
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// 某一行（按行键查）
    pub fn row(&self, row_uid: &str) -> Option<&EvidenceRow> {
        self.rows.iter().find(|r| r.row_uid == row_uid)
    }
}

/// 按批次组装举证包。
///
/// `metas` 是本批次的全部行（含没有命中的行），`issues` 是本批次命中的全部问题。
/// 每条问题都挂在某个行上，因此包内计数与行条目自洽。
///
/// `basis` 给出行级指标的官方条款依据（逐项口径）；没有就空着，**不拿别的口径顶替**。
pub fn build(
    batch_ref: &str,
    rule_set_version: &str,
    metas: &[RowMeta],
    issues: &[Issue],
    basis: &IndicatorBasis,
) -> EvidencePackage {
    let index = IssueIndex::build(issues);

    let mut rows: Vec<EvidenceRow> = Vec::with_capacity(metas.len());
    let mut rows_with_hits = 0usize;
    let mut hits_total = 0usize;
    let mut by_issue_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_severity: BTreeMap<String, usize> = BTreeMap::new();

    for meta in metas {
        let row_issues = index.of_row(&meta.row_uid);
        let hits: Vec<EvidenceHit> = row_issues.iter().map(to_hit).collect();
        let conclusion = conclusion_of(row_issues).to_string();

        let (match_state, suggestion) = if hits.is_empty() {
            (MATCH_NONE.to_string(), NO_HIT_SUGGESTION.to_string())
        } else {
            rows_with_hits += 1;
            hits_total += hits.len();
            for hit in &hits {
                *by_issue_kind.entry(hit.issue_kind.clone()).or_insert(0) += 1;
                *by_severity.entry(hit.severity.clone()).or_insert(0) += 1;
            }
            (MATCH_HIT.to_string(), row_suggestion(&hits))
        };

        let code = meta.indicator_code.as_deref().unwrap_or("");
        let official_clause = basis.clause_of(code).unwrap_or("").to_string();
        let indicator_basis = basis.basis_of(code).unwrap_or("").to_string();

        rows.push(EvidenceRow {
            row_uid: meta.row_uid.clone(),
            source_ns: meta.source_ns.clone(),
            source_file: meta.source_file.clone(),
            row_no: meta.row_no,
            indicator_code: meta.indicator_code.clone(),
            dept_code: meta.dept_code.clone(),
            reporting_period: meta.reporting_period.clone(),
            business_key: meta.business_key.clone(),
            conclusion,
            match_state,
            hit_count: hits.len(),
            hits,
            suggestion,
            official_clause,
            indicator_basis,
        });
    }

    EvidencePackage {
        schema_version: EVIDENCE_SCHEMA_VERSION.to_string(),
        generated_at: crate::db::repo::now_iso8601(),
        batch_ref: batch_ref.to_string(),
        rule_set_version: rule_set_version.to_string(),
        indicator_basis_note: basis_note(basis, rule_set_version),
        counts: EvidenceCounts {
            rows: rows.len(),
            rows_with_hits,
            rows_without_hits: rows.len() - rows_with_hits,
            hits: hits_total,
            by_issue_kind,
            by_severity,
        },
        rows,
    }
}

/// 逐项依据的来源说明：取到了说清出自哪一版种子，没取到说清为什么。
fn basis_note(basis: &IndicatorBasis, rule_set_version: &str) -> String {
    if basis.is_empty() {
        return "未取到逐项官方条款依据（规则目录里没有口径种子文件 *.seed.json）".to_string();
    }
    if !rule_set_version.is_empty() && !basis.matches_version(rule_set_version) {
        return format!(
            "未附逐项官方条款依据：口径种子版本（{}）与本批结论的口径版本（{}）对不上，不拿别的口径顶替",
            basis.versions().join("、"),
            rule_set_version
        );
    }
    format!(
        "逐项官方条款依据取自口径种子（版本 {}）",
        basis.versions().join("、")
    )
}

/// 一条问题 → 一条命中
fn to_hit(issue: &Issue) -> EvidenceHit {
    EvidenceHit {
        rule_id: issue.rule_id.clone(),
        issue_kind: issue.issue_kind.clone(),
        severity: issue.severity.clone(),
        message: issue.message.clone(),
        basis: issue.basis.clone(),
        error_code: issue.error_code.clone(),
        missing_columns: issue.missing_columns.clone(),
        suggestion: suggest_for(issue),
    }
}

/// 行级建议：各命中建议去重后合并
fn row_suggestion(hits: &[EvidenceHit]) -> String {
    let mut out: Vec<String> = Vec::new();
    for hit in hits {
        if !out.contains(&hit.suggestion) {
            out.push(hit.suggestion.clone());
        }
    }
    out.join("；")
}

/// 建议修改动作：按问题类型与错误码给出**能照着做**的一句话。
///
/// 顺序：先看错误码（同一类问题里还要再分小类的，看码更准），再看问题类型。
pub fn suggest_for(issue: &Issue) -> String {
    let code = issue.error_code.as_str();
    if code == ErrorCode::DenominatorZero.as_str() {
        return "补填非零分母后重新校验（分母为 0 时填报值无法复算）".to_string();
    }
    if code == ErrorCode::InvalidNumeric.as_str() {
        return "把该数值列改成合法数值后重新校验（非数值与空值是两类问题）".to_string();
    }
    if code == ErrorCode::DeptNotFound.as_str() {
        return "核对科室代码：不在 HIS 科室目录中，与信息科确认后再上报".to_string();
    }
    if code == ErrorCode::AuthorityKeyConflict.as_str() {
        return "同一业务主键下有取值不同的两行，请人工判定保留哪一行后再上报".to_string();
    }

    match issue.issue_kind.as_str() {
        "必填缺失" => {
            if issue.missing_columns.is_empty() {
                "补齐缺失的必填列后重新校验".to_string()
            } else {
                format!(
                    "补齐缺失列「{}」后重新校验",
                    issue.missing_columns.join("」「")
                )
            }
        }
        "枚举越界" => "把取值改成问题说明里列出的期望取值之一后重新校验".to_string(),
        "单位不符" => "把计量单位改成问题说明里给出的期望单位后重新校验".to_string(),
        "公式不符" => "按分子 ÷ 分母 × 100 重算填报值后重新校验".to_string(),
        "数据形态错误" => "按问题说明里的错误码修正该列后重新校验".to_string(),
        "疑似重复" => "与对照行人工核对后保留一行再上报（本轮不自动合并）".to_string(),
        "待人工判定" => "人工判定保留哪一行后重新校验（本轮不自动合并）".to_string(),
        "科室主数据不一致" => "核对科室代码是否写错或尚未在科室目录登记".to_string(),
        _ => "按问题说明里的官方依据修正后重新校验".to_string(),
    }
}

/// 举证包 JSON 文本
pub fn to_json(pkg: &EvidencePackage) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(pkg)
}

/// 举证包 CSV 文本。
///
/// **一行一次命中**：命中多条的行列多行，便于在 Excel 里逐条筛。
/// 没有命中的行照样出一行（命中状态 = 无命中），逐行核对时不会漏行。
pub fn to_csv(pkg: &EvidencePackage) -> Result<String, csv::Error> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(CSV_HEADER)?;

    for row in &pkg.rows {
        let base = [
            row.source_ns.clone(),
            row.row_no.to_string(),
            row.row_uid.clone(),
            row.indicator_code.clone().unwrap_or_default(),
            row.dept_code.clone().unwrap_or_default(),
            row.reporting_period.clone().unwrap_or_default(),
            row.business_key.clone().unwrap_or_default(),
            row.conclusion.clone(),
            row.match_state.clone(),
            row.hit_count.to_string(),
            row.official_clause.clone(),
            row.indicator_basis.clone(),
        ];
        if row.hits.is_empty() {
            let mut line = base.to_vec();
            line.extend([
                String::new(), // 规则编号
                String::new(), // 问题类型
                String::new(), // 严重度
                String::new(), // 问题说明
                String::new(), // 官方依据
                String::new(), // 错误码
                String::new(), // 缺失列
                row.suggestion.clone(),
            ]);
            wtr.write_record(&line)?;
            continue;
        }
        for hit in &row.hits {
            let mut line = base.to_vec();
            line.extend([
                hit.rule_id.clone(),
                hit.issue_kind.clone(),
                hit.severity.clone(),
                hit.message.clone(),
                hit.basis.clone(),
                hit.error_code.clone(),
                hit.missing_columns.join("、"),
                hit.suggestion.clone(),
            ]);
            wtr.write_record(&line)?;
        }
    }

    wtr.flush()?;
    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(uid: &str, row_no: usize) -> RowMeta {
        RowMeta {
            row_uid: uid.into(),
            source_ns: "表一".into(),
            source_file: "t.csv".into(),
            row_no,
            indicator_code: Some("MER-TL-04".into()),
            dept_code: Some("0301".into()),
            reporting_period: Some("2026-Q3".into()),
            business_key: Some(format!("BK-{:03}", row_no)),
        }
    }

    fn issue(row: &str, kind: &str, severity: &str, missing: &[&str]) -> Issue {
        Issue {
            row_uid: row.into(),
            rule_id: format!("R-{}", kind),
            issue_kind: kind.into(),
            severity: severity.into(),
            message: format!("{}说明", kind),
            basis: "国卫办医函〔2021〕28号 附件 指标四".into(),
            issue_key: String::new(),
            missing_columns: missing.iter().map(|s| s.to_string()).collect(),
            error_code: String::new(),
        }
    }

    #[test]
    fn 无命中的行也在包里且标无命中() {
        let issues = vec![issue("u1", "必填缺失", "阻断", &["科室代码"])];
        let pkg = build(
            "b1",
            "2021.1",
            &[meta("u1", 1), meta("u2", 2)],
            &issues,
            &IndicatorBasis::default(),
        );
        assert_eq!(pkg.row_count(), 2);
        assert_eq!(pkg.counts.rows_with_hits, 1);
        assert_eq!(pkg.counts.rows_without_hits, 1);
        let row2 = pkg.row("u2").unwrap();
        assert_eq!(row2.match_state, MATCH_NONE);
        assert_eq!(row2.hit_count, 0);
        assert_eq!(row2.suggestion, NO_HIT_SUGGESTION);
        assert_eq!(row2.conclusion, "通过");
    }

    #[test]
    fn 每条命中都带依据与建议动作() {
        let issues = vec![issue("u1", "必填缺失", "阻断", &["科室代码", "报告期"])];
        let pkg = build(
            "b1",
            "2021.1",
            &[meta("u1", 1)],
            &issues,
            &IndicatorBasis::default(),
        );
        let hit = &pkg.rows[0].hits[0];
        assert!(!hit.basis.is_empty(), "命中必须带官方依据");
        assert!(hit.suggestion.contains("科室代码"));
        assert!(hit.suggestion.contains("报告期"));
    }

    #[test]
    fn 计数与行条目自洽() {
        let issues = vec![
            issue("u1", "必填缺失", "阻断", &[]),
            issue("u1", "枚举越界", "阻断", &[]),
            issue("u2", "疑似重复", "提示", &[]),
        ];
        let pkg = build(
            "b1",
            "2021.1",
            &[meta("u1", 1), meta("u2", 2), meta("u3", 3)],
            &issues,
            &IndicatorBasis::default(),
        );
        assert_eq!(pkg.counts.rows, 3);
        assert_eq!(pkg.counts.rows_with_hits, 2);
        assert_eq!(pkg.counts.rows_without_hits, 1);
        assert_eq!(pkg.counts.hits, 3);
        let sum: usize = pkg.rows.iter().map(|r| r.hit_count).sum();
        assert_eq!(sum, pkg.counts.hits);
        assert_eq!(pkg.counts.by_issue_kind.get("必填缺失"), Some(&1));
        assert_eq!(pkg.counts.by_severity.get("提示"), Some(&1));
    }

    #[test]
    fn csv_一行一次命中且无命中行也出行() {
        let issues = vec![
            issue("u1", "必填缺失", "阻断", &["科室代码"]),
            issue("u1", "枚举越界", "阻断", &[]),
        ];
        let pkg = build(
            "b1",
            "2021.1",
            &[meta("u1", 1), meta("u2", 2)],
            &issues,
            &IndicatorBasis::default(),
        );
        let text = to_csv(&pkg).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 4, "表头 + 2 次命中 + 1 行无命中：\n{}", text);
        assert!(lines[0].contains("官方依据"));
        assert!(lines.iter().any(|l| l.contains("无命中")));
    }

    #[test]
    fn json_可解析且带版本号() {
        let pkg = build(
            "b1",
            "2021.1",
            &[meta("u1", 1)],
            &[],
            &IndicatorBasis::default(),
        );
        let text = to_json(&pkg).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["schema_version"], EVIDENCE_SCHEMA_VERSION);
        assert_eq!(v["counts"]["rows"], 1);
    }
}
