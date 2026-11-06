//! 读表阶段的质量判定与格式归一。
//!
//! # 为什么读表就要分「有效 / 空行 / 完全无效」
//!
//! 科室从 WPS、Excel、旧系统导出的表，五花八门：
//!
//! - 末尾多一行空行（导出时按留的空行）；
//! - 表中间又抄了一遍表头（多表并入时手滑粘进来的）；
//! - 分隔线、`合计`、`以上汇总`这类没有业务字段的行。
//!
//! 这些行**不是**数据质量问题 —— 把它们当成「必填缺失」报上去，
//! 状态条上就会多出一堆和填报无关的口径问题，科室看着懵；
//! 但把它们**悄悄丢掉**更糟：表里 200 行，引擎只校验了 197 行，
//! 谁也不知道少了 3 行。所以本模块的规矩是：
//!
//! - 空行与完全无效行**不参与校验**（不是「合格」，是「不是数据行」）；
//! - 但**必须计数、必须留痕**：行号 + 原因进契约的 `ingest` 段，
//!   计数进契约 `summary` 段，终端也照实打一行 —— **不静默丢行**。
//!
//! # 判定口径只有一处
//!
//! CSV 直跑与 Excel/WPS 走的是**同一段判定代码**（[`crate::ingest::map_table_with_report`]），
//! 所以同一张表从哪条入口进去，「跳过了几行、为什么跳」的答案都一样 ——
//! 否则信息科拿 CSV 校验过的表，到 Excel 机器上看到行数对不上，就得逐行对账。

use serde::{Deserialize, Serialize};

use super::Field;

/// 跳过的原因：空行
pub const REASON_BLANK: &str = "空行";
/// 跳过的原因：表中间又抄了一遍表头
pub const REASON_DUP_HEADER: &str = "重复表头";
/// 跳过的原因：有内容但没有一个业务字段落得上（分隔线、合计行、随手的备注）
pub const REASON_NO_CONTENT: &str = "无有效内容";
/// 跳过的原因：表里用一行横线把上下两段隔开
pub const REASON_SEPARATOR: &str = "分隔线";

/// 一行的判定结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowVerdict {
    /// 有效行：参与校验
    Valid,
    /// 空行：所有格都是空的
    Blank,
    /// 完全无效行：有内容，但不是数据行（带原因）
    Invalid(&'static str),
}

/// 被跳过的行（留痕用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedRow {
    /// 来源命名空间（CSV 取文件主干，Excel 取工作表名）
    pub source_ns: String,
    /// 原表里的行号（数据行序，从 1 起，不含表头）—— 人能照着这个号回原表找
    pub row_no: usize,
    /// 跳过的原因
    pub reason: String,
}

/// 读表阶段的记录：行数怎么分的、格式怎么归一的。
///
/// 这一段进结果契约的 `ingest` 段（**追加**段，契约版本不升，插件读法不变），
/// 也进 `summary` 段的四个计数 —— 状态条与终端都能看见「这一批跳过了几行」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestSection {
    /// 读到的数据行总数（含空行与完全无效行）
    #[serde(default)]
    pub source_rows: usize,
    /// 参与校验的有效行数
    #[serde(default)]
    pub valid_rows: usize,
    /// 空行数
    #[serde(default)]
    pub blank_rows: usize,
    /// 完全无效行数
    #[serde(default)]
    pub invalid_rows: usize,
    /// 被跳过的行数（空行 + 完全无效行）
    #[serde(default)]
    pub skipped_rows: usize,
    /// 跳过的行明细（行号 + 原因）
    #[serde(default)]
    pub skipped_detail: Vec<SkippedRow>,
    /// 读到的编码（utf-8 / utf-8-bom / gbk / mixed / xlsx）
    #[serde(default)]
    pub encoding: String,
    /// 分隔符（半角逗号 / 全角逗号（已归一为半角） / xlsx）
    #[serde(default)]
    pub delimiter: String,
    /// 读表时遇到的问题说明（正常为空串）
    #[serde(default)]
    pub format_note: String,
    /// 一句话说明（终端与状态条用；没有跳过行时为空串）
    #[serde(default)]
    pub skipped_note: String,
}

impl IngestSection {
    /// 汇总一段读表结果。
    pub fn build(
        source_rows: usize,
        valid_rows: usize,
        blank_rows: usize,
        invalid_rows: usize,
        skipped_detail: Vec<SkippedRow>,
        encoding: &str,
        delimiter: &str,
        format_note: &str,
    ) -> IngestSection {
        let skipped_rows = blank_rows + invalid_rows;
        IngestSection {
            source_rows,
            valid_rows,
            blank_rows,
            invalid_rows,
            skipped_rows,
            skipped_detail,
            encoding: encoding.to_string(),
            delimiter: delimiter.to_string(),
            format_note: format_note.to_string(),
            skipped_note: skipped_note(skipped_rows, blank_rows, invalid_rows),
        }
    }

    /// 读表时做过格式归一没有（编码不是 UTF-8、或者分隔符被归一过）
    pub fn normalized(&self) -> bool {
        self.encoding.starts_with("gbk")
            || self.encoding == "mixed"
            || self.encoding == "utf-8-bom"
            || self.delimiter.starts_with("全角")
    }
}

/// 跳过行的说明句。
///
/// 「空行 1 行 · 无效行 1 行」这种说法要有具体数字 ——
/// 写「跳过了一些行」等于没写，看的人还得回去数表。
pub fn skipped_note(skipped_rows: usize, blank_rows: usize, invalid_rows: usize) -> String {
    if skipped_rows == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if blank_rows > 0 {
        parts.push(format!("空行 {} 行", blank_rows));
    }
    if invalid_rows > 0 {
        parts.push(format!("无效行 {} 行", invalid_rows));
    }
    format!(
        "跳过 {} 行：{}（已计数，未参与校验）",
        skipped_rows,
        parts.join(" · ")
    )
}

/// 一个格子是不是「空」。
///
/// 空串、纯空白、全角空格、不间断空格、零宽不换行空格都算空 ——
/// 中文 Windows 的表里这几种空格混着出现，不当成空就会凭空多出一批「必填缺失」。
pub fn is_blank_cell(cell: &str) -> bool {
    cell.chars()
        .all(|c| c.is_whitespace() || c == '\u{00A0}' || c == '\u{3000}' || c == '\u{FEFF}')
}

/// 这一行是不是「表头又抄了一遍」。
///
/// 逐格比较（归一后）：列名一字不差才算。多表并入时手滑粘进来的表头行
/// 长得跟表头一样，靠这条认出来 —— 认成数据行会报一堆「必填缺失」。
pub fn is_repeated_header(header: &[String], row: &[String]) -> bool {
    if header.is_empty() || row.is_empty() {
        return false;
    }
    let n = header.len().min(row.len());
    if n == 0 {
        return false;
    }
    header[..n]
        .iter()
        .zip(row[..n].iter())
        .all(|(h, r)| super::normalize_header(h) == super::normalize_header(r))
}

/// 这一行有没有落得上**必填**业务字段。
///
/// 「落得上」= 指标编号 / 指标名称 / 科室代码 / 报告期 / 业务主键里至少一格有值。
/// 这五格是行的身份证 —— 一格都填不上，这行就不是「一条指标记录」，
/// 而是表里多出来的一行杂字（备注行、合计行标题）。
pub fn has_required_content(
    row: &[String],
    column_of: &std::collections::BTreeMap<Field, usize>,
) -> bool {
    Field::all()
        .iter()
        .filter(|f| f.is_required_header())
        .filter_map(|f| column_of.get(f))
        .any(|&col| row.get(col).map(|v| !is_blank_cell(v)).unwrap_or(false))
}

/// 这一行是不是「分隔线」。
///
/// 表里用一行 `————` 或 `====` 把上下两段隔开 —— 这种行不是数据，
/// 报成「指标编号不在册」纯属噪声。
pub fn is_separator_row(row: &[String]) -> bool {
    let mut any = false;
    for cell in row {
        let t = cell.trim();
        if t.is_empty() {
            continue;
        }
        any = true;
        if !t.chars().all(|c| {
            matches!(
                c,
                '-' | '=' | '_' | '.' | '~' | '*' | '/' | '\\' | '—' | '－' | '﹣' | '·'
            )
        }) {
            return false;
        }
    }
    any
}

/// 判一行是什么（判定口径只有这一处，CSV 与 Excel 共用）。
pub fn classify(
    header: &[String],
    row: &[String],
    column_of: &std::collections::BTreeMap<Field, usize>,
) -> RowVerdict {
    if row.iter().all(|c| is_blank_cell(c)) {
        return RowVerdict::Blank;
    }
    if is_repeated_header(header, row) {
        return RowVerdict::Invalid(REASON_DUP_HEADER);
    }
    if is_separator_row(row) {
        return RowVerdict::Invalid(REASON_SEPARATOR);
    }
    if !has_required_content(row, column_of) {
        return RowVerdict::Invalid(REASON_NO_CONTENT);
    }
    RowVerdict::Valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cols() -> BTreeMap<Field, usize> {
        let mut m = BTreeMap::new();
        m.insert(Field::IndicatorCode, 0);
        m.insert(Field::IndicatorName, 1);
        m.insert(Field::DeptCode, 2);
        m.insert(Field::ReportingPeriod, 3);
        m.insert(Field::BusinessKey, 4);
        m
    }

    fn header() -> Vec<String> {
        ["指标编号", "指标名称", "科室代码", "报告期", "业务主键"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn 空行与全角空格行都判为空行() {
        let c = cols();
        assert_eq!(classify(&header(), &["".into(), "   ".into()], &c), RowVerdict::Blank);
        assert_eq!(
            classify(
                &header(),
                &["".into(), "\u{3000}".into(), "\u{00A0}".into(), "\t".into()],
                &c
            ),
            RowVerdict::Blank
        );
    }

    #[test]
    fn 重复表头判为无效行并给出原因() {
        let c = cols();
        let row: Vec<String> = header();
        assert_eq!(
            classify(&header(), &row, &c),
            RowVerdict::Invalid(REASON_DUP_HEADER)
        );
    }

    #[test]
    fn 有内容但必填业务列全空的行判为无效行() {
        let c = cols();
        // 备注行：内容落在表头之外的列上，必填列一格没填
        let row = vec![
            "".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
            "以上数据由信息科汇总".to_string(),
        ];
        assert_eq!(
            classify(&header(), &row, &c),
            RowVerdict::Invalid(REASON_NO_CONTENT)
        );
    }

    #[test]
    fn 分隔线行判为无效行() {
        let c = cols();
        let row = vec![
            "————".to_string(),
            "----".to_string(),
            "----".to_string(),
            "----".to_string(),
            "----".to_string(),
        ];
        assert_eq!(
            classify(&header(), &row, &c),
            RowVerdict::Invalid(REASON_SEPARATOR)
        );
    }

    #[test]
    fn 必填列有值但别列是横线的行仍是有效行() {
        let c = cols();
        let row = vec![
            "MER-HR-01".to_string(),
            "病案首页完整率".to_string(),
            "0101".to_string(),
            "2026-Q3".to_string(),
            "BK-001".to_string(),
        ];
        assert_eq!(classify(&header(), &row, &c), RowVerdict::Valid);
    }

    #[test]
    fn 正常数据行判为有效行() {
        let c = cols();
        let row = vec![
            "MER-HR-01".to_string(),
            "病案首页完整率".to_string(),
            "0101".to_string(),
            "2026-Q3".to_string(),
            "BK-001".to_string(),
        ];
        assert_eq!(classify(&header(), &row, &c), RowVerdict::Valid);
    }

    #[test]
    fn 跳过说明句带具体数字_没有跳过时为空() {
        assert_eq!(skipped_note(0, 0, 0), "");
        assert_eq!(skipped_note(1, 1, 0), "跳过 1 行：空行 1 行（已计数，未参与校验）");
        assert_eq!(
            skipped_note(3, 1, 2),
            "跳过 3 行：空行 1 行 · 无效行 2 行（已计数，未参与校验）"
        );
    }

    #[test]
    fn 归一过就标出来_没归一不标() {
        let plain = IngestSection::build(6, 6, 0, 0, vec![], "utf-8", "半角逗号", "");
        assert!(!plain.normalized());
        let gbk = IngestSection::build(6, 6, 0, 0, vec![], "gbk", "半角逗号", "");
        assert!(gbk.normalized());
        let full =
            IngestSection::build(6, 6, 0, 0, vec![], "utf-8", "全角逗号（已归一为半角）", "");
        assert!(full.normalized());
    }
}
