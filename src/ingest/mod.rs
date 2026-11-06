//! 读表与表头映射。
//!
//! 表头可映射：只认登记过的列名与别名，缺必填列就报错并点名缺哪一列 —— 不猜列、不静默丢行。
//! 数值列的「空」与「非数值」都按原文保留，是否算问题交给口径规则层判定。

pub mod csv;
pub mod key;
pub mod quality;
pub mod xlsx;

pub use quality::{IngestSection, SkippedRow};

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::errors::{EngineError, ErrorCode};

/// 行键的定义在 [`key`] 模块（与业务主键、跨表匹配键、权威键同源）。
pub use key::row_uid;

/// 业务字段（与 spec.md「最小数据格式与字段要求」一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    OrgCode,
    IndicatorCode,
    IndicatorName,
    DeptCode,
    DeptName,
    ReportingPeriod,
    /// 报告期类型（可选列：年报 / 半年报 / 季报 / 月报 / 实时报）
    PeriodType,
    Numerator,
    Denominator,
    Value,
    Unit,
    BusinessKey,
    Remark,
}

impl Field {
    /// 规范表头名（写回原表与文档口径一致）
    pub fn header(self) -> &'static str {
        match self {
            Field::OrgCode => "机构代码",
            Field::IndicatorCode => "指标编号",
            Field::IndicatorName => "指标名称",
            Field::DeptCode => "科室代码",
            Field::DeptName => "科室名称",
            Field::ReportingPeriod => "报告期",
            Field::PeriodType => "报告期类型",
            Field::Numerator => "分子",
            Field::Denominator => "分母",
            Field::Value => "填报值",
            Field::Unit => "计量单位",
            Field::BusinessKey => "业务主键",
            Field::Remark => "备注",
        }
    }

    /// 本机库列名
    pub fn column(self) -> &'static str {
        match self {
            Field::OrgCode => "org_code",
            Field::IndicatorCode => "indicator_code",
            Field::IndicatorName => "indicator_name",
            Field::DeptCode => "dept_code",
            Field::DeptName => "dept_name",
            Field::ReportingPeriod => "reporting_period",
            Field::PeriodType => "period_type",
            Field::Numerator => "numerator",
            Field::Denominator => "denominator",
            Field::Value => "value",
            Field::Unit => "unit",
            Field::BusinessKey => "business_key",
            Field::Remark => "remark",
        }
    }

    /// 可接受的表头写法。只认登记过的别名，不做模糊猜测。
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Field::OrgCode => &["机构代码", "机构编码", "医院代码", "院区代码"],
            Field::IndicatorCode => &["指标编号", "指标编码", "指标代码"],
            Field::IndicatorName => &["指标名称", "指标名"],
            Field::DeptCode => &["科室代码", "科室编码", "科室号"],
            Field::DeptName => &["科室名称", "科室"],
            Field::ReportingPeriod => &["报告期", "报表期间", "统计期间"],
            Field::PeriodType => &["报告期类型", "报表类型", "上报类型", "报告类型"],
            Field::Numerator => &["分子"],
            Field::Denominator => &["分母"],
            Field::Value => &["填报值", "填报数值", "数值"],
            Field::Unit => &["计量单位", "单位"],
            Field::BusinessKey => &["业务主键", "业务键", "主键"],
            Field::Remark => &["备注"],
        }
    }

    /// 是否为表头必现列。条件必填列（分子/分母/填报值/计量单位）由口径规则按指标类别判定，
    /// 表头可以没有；必填列缺了就是表结构问题，直接报错。
    pub fn is_required_header(self) -> bool {
        matches!(
            self,
            Field::IndicatorCode
                | Field::IndicatorName
                | Field::DeptCode
                | Field::ReportingPeriod
                | Field::BusinessKey
        )
    }

    /// 全部业务字段
    pub fn all() -> [Field; 13] {
        [
            Field::OrgCode,
            Field::IndicatorCode,
            Field::IndicatorName,
            Field::DeptCode,
            Field::DeptName,
            Field::ReportingPeriod,
            Field::PeriodType,
            Field::Numerator,
            Field::Denominator,
            Field::Value,
            Field::Unit,
            Field::BusinessKey,
            Field::Remark,
        ]
    }
}

/// 一张来源表：原始表头 + 原始数据行（值一律按原样保留）。
#[derive(Debug, Clone)]
pub struct SourceTable {
    /// 来源命名空间：CSV 取文件名主干，Excel 取工作表名（多来源互不污染）
    pub source_ns: String,
    pub source_file: String,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 读表时的格式归一记录（编码 / 分隔符）—— 进了契约的 `ingest` 段
    pub format: FormatInfo,
}

/// 读表时的格式信息。
///
/// CSV 直跑时这里记的是「这张表实际是什么编码、什么分隔符」——
/// 专网报送机上的表常是 GBK + 全角逗号，不记下来，
/// 经办人看到表里的乱码会以为引擎读错了。
#[derive(Debug, Clone, Default)]
pub struct FormatInfo {
    /// 编码标签（utf-8 / utf-8-bom / gbk / mixed / xlsx）
    pub encoding: String,
    /// 分隔符标签（半角逗号 / 全角逗号（已归一为半角） / xlsx）
    pub delimiter: String,
    /// 读表时遇到的问题说明（正常为空串）
    pub note: String,
}

/// 一行已映射的业务字段
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub source_ns: String,
    pub source_file: String,
    /// 文件内数据行序（从 1 起，不含表头）
    pub row_no: usize,
    /// 同一业务主键下本行在表内的次序（从 0 起）。行键靠它区分同键多行，
    /// 由 [`key::assign_dup_ordinals`] 在表映射后统一编号。
    pub dup_ordinal: usize,
    pub values: BTreeMap<Field, String>,
}

/// 一行的标识信息（写进契约与本机库时用）
#[derive(Debug, Clone)]
pub struct RowMeta {
    pub row_uid: String,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    pub indicator_code: Option<String>,
    pub dept_code: Option<String>,
    pub reporting_period: Option<String>,
    pub business_key: Option<String>,
}

impl SourceRow {
    /// 原值
    pub fn get(&self, field: Field) -> Option<&str> {
        self.values.get(&field).map(|s| s.as_str())
    }

    /// 去空白后的取值；空串与纯空白视为 None
    pub fn get_trimmed(&self, field: Field) -> Option<&str> {
        self.get(field).map(|s| s.trim()).filter(|s| !s.is_empty())
    }

    /// 数值取值：空 → None；非数值 → None（原文另存 `numeric_raw`，由规则层区分「空」与「非数值」）
    pub fn number(&self, field: Field) -> Option<f64> {
        let raw = self.get_trimmed(field)?;
        let v = normalize_digits(raw).parse::<f64>().ok()?;
        if v.is_finite() {
            Some(v)
        } else {
            None
        }
    }

    /// 行标识信息
    pub fn meta(&self) -> RowMeta {
        RowMeta {
            row_uid: row_uid(self),
            source_ns: self.source_ns.clone(),
            source_file: self.source_file.clone(),
            row_no: self.row_no,
            indicator_code: self.get_trimmed(Field::IndicatorCode).map(str::to_string),
            dept_code: self.get_trimmed(Field::DeptCode).map(str::to_string),
            reporting_period: self.get_trimmed(Field::ReportingPeriod).map(str::to_string),
            business_key: self.get_trimmed(Field::BusinessKey).map(str::to_string),
        }
    }
}

/// 行键：来源命名空间 + 文件主干 + 业务主键 + 同键行序的稳定摘要。
///
/// 定义在 [`key::row_uid`]，与业务主键、跨表匹配键、权威键同源。
/// 同一份输入两次 ingest 必得同一 `row_uid`。

/// 批次号：同一份输入内容对应同一批次号，便于重跑对账。
pub fn batch_ref(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let digest = sha256_hex(&bytes);
    format!(
        "{}-{}",
        file_stem(&path.display().to_string()),
        &digest[..12]
    )
}

/// 文件名主干（不含目录与扩展名）
pub fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "table".to_string())
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

/// 按扩展名分派读取器，返回该文件里的全部来源表（Excel 一个工作表一张表）。
pub fn read_tables(path: &Path) -> Result<Vec<SourceTable>, EngineError> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "csv" | "txt" => Ok(vec![csv::read(path)?]),
        "xlsx" | "xlsm" => xlsx::read_all(path),
        _ => Err(EngineError::at(
            ErrorCode::UnsupportedFormat,
            format!(
                "不支持的输入格式「.{}」，本轮支持 .csv / .xlsx / .xlsm",
                ext
            ),
            path.display().to_string(),
        )),
    }
}

/// 把原始表映射成业务行。缺必填表头列 → 报错并点名缺哪一列。
///
/// 只回有效行 —— 空行与完全无效行不参与校验（判定口径见 [`quality`]）。
/// 要看跳过了几行、为什么跳，用 [`map_table_with_report`]。
pub fn map_table(table: &SourceTable) -> Result<Vec<SourceRow>, EngineError> {
    Ok(map_table_with_report(table)?.rows)
}

/// 映射结果：有效行 + 跳过的行留痕。
#[derive(Debug, Clone, Default)]
pub struct MappedTable {
    /// 参与校验的有效行
    pub rows: Vec<SourceRow>,
    /// 被跳过的行（空行 / 完全无效行）—— 行号与原因都留痕
    pub skipped: Vec<SkippedRow>,
    /// 空行数
    pub blank_rows: usize,
    /// 完全无效行数
    pub invalid_rows: usize,
}

/// 把原始表映射成业务行，并记录被跳过的行。
///
/// **空行与完全无效行不参与校验，但必须计数留痕**（不静默丢行）：
/// 跳过的行号与原因进契约 `ingest` 段，计数进 `summary` 段。
/// 判定口径在 [`quality::classify`] 一处，CSV 与 Excel 共用 —— 同一张表
/// 从哪条入口进去，跳过了几行都一样。
pub fn map_table_with_report(table: &SourceTable) -> Result<MappedTable, EngineError> {
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (i, header) in table.header.iter().enumerate() {
        index.entry(normalize_header(header)).or_insert(i);
    }

    let mut missing: Vec<&'static str> = Vec::new();
    let mut column_of: BTreeMap<Field, usize> = BTreeMap::new();
    for field in Field::all() {
        let mut hit: Option<usize> = None;
        for alias in field.aliases() {
            if let Some(&i) = index.get(&normalize_header(alias)) {
                hit = Some(i);
                break;
            }
        }
        match hit {
            Some(i) => {
                column_of.insert(field, i);
            }
            None if field.is_required_header() => missing.push(field.header()),
            None => {}
        }
    }
    if !missing.is_empty() {
        return Err(EngineError::at(
            ErrorCode::MissingColumn,
            format!(
                "表头缺少必填列：{}（请检查表头是否写错或多表并入时列被挤掉）",
                missing.join("、")
            ),
            format!("{} :: 表头 {:?}", table.source_file, table.header),
        ));
    }

    let mut out = MappedTable::default();
    for (idx, raw) in table.rows.iter().enumerate() {
        // 行号用原表里的行序（从 1 起，不含表头）—— 跳过的行也占号，
        // 报出来的时候人能照着这个号回原表找。
        let row_no = idx + 1;
        match quality::classify(&table.header, raw, &column_of) {
            quality::RowVerdict::Valid => {
                let mut values = BTreeMap::new();
                for (field, &col) in &column_of {
                    let v = raw.get(col).map(|s| s.to_string()).unwrap_or_default();
                    values.insert(*field, v);
                }
                out.rows.push(SourceRow {
                    source_ns: table.source_ns.clone(),
                    source_file: table.source_file.clone(),
                    row_no,
                    dup_ordinal: 0,
                    values,
                });
            }
            quality::RowVerdict::Blank => {
                out.blank_rows += 1;
                out.skipped.push(SkippedRow {
                    source_ns: table.source_ns.clone(),
                    row_no,
                    reason: quality::REASON_BLANK.to_string(),
                });
            }
            quality::RowVerdict::Invalid(reason) => {
                out.invalid_rows += 1;
                out.skipped.push(SkippedRow {
                    source_ns: table.source_ns.clone(),
                    row_no,
                    reason: reason.to_string(),
                });
            }
        }
    }
    // 同键多行编号：行键靠它区分同业务主键下的多行（见 key::assign_dup_ordinals）
    key::assign_dup_ordinals(&mut out.rows);
    Ok(out)
}

/// 表头归一：去空白（含全角空格、不间断空格）、全角转半角、转小写。
pub fn normalize_header(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{00A0}' && *c != '\u{3000}')
        .map(|c| {
            let code = c as u32;
            if (0xFF01..=0xFF5E).contains(&code) {
                char::from_u32(code - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 数值归一：全角数字与全角小数点转半角，其余不动。
///
/// 上报侧的载荷摘要也用这条规则（`push::idempotency`）—— 同一个数在读表层和在
/// 上报层必须是同一个数，不能各写一套。
pub(crate) fn normalize_digits(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            let code = c as u32;
            if (0xFF10..=0xFF19).contains(&code) {
                char::from_u32(code - 0xFEE0).unwrap_or(c)
            } else if code == 0xFF0E {
                '.'
            } else if code == 0xFF0D {
                '-'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(header: &[&str], rows: &[&[&str]]) -> SourceTable {
        SourceTable {
            source_ns: "t".into(),
            source_file: "t.csv".into(),
            header: header.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
            format: FormatInfo {
                encoding: "utf-8".into(),
                delimiter: "半角逗号".into(),
                note: String::new(),
            },
        }
    }

    #[test]
    fn 缺必填表头列时报错并点名缺哪一列() {
        let t = table(&["指标编号", "指标名称", "报告期", "业务主键"], &[]);
        let err = map_table(&t).unwrap_err();
        assert_eq!(err.code, ErrorCode::MissingColumn);
        assert!(
            err.message.contains("科室代码"),
            "错误说明应点名缺哪一列：{}",
            err.message
        );
    }

    #[test]
    fn 表头带空格与全角也能映射() {
        let t = table(
            &[
                "指 标 编 号",
                "指标名称\u{3000}",
                "科室代码",
                "报告期",
                "业务主键",
            ],
            &[&["MER-HR-01", "病案首页完整率", "0101", "2026-Q3", "BK-001"]],
        );
        let rows = map_table(&t).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get_trimmed(Field::IndicatorName),
            Some("病案首页完整率")
        );
    }

    #[test]
    fn 同一份输入两次映射得到同一行键() {
        let t = table(
            &["指标编号", "指标名称", "科室代码", "报告期", "业务主键"],
            &[&["MER-HR-01", "病案首页完整率", "0101", "2026-Q3", "BK-001"]],
        );
        let a = map_table(&t).unwrap();
        let b = map_table(&t).unwrap();
        assert_eq!(row_uid(&a[0]), row_uid(&b[0]));
    }

    #[test]
    fn 空串与纯空白在数值列上不当作零() {
        let t = table(
            &[
                "指标编号",
                "指标名称",
                "科室代码",
                "报告期",
                "业务主键",
                "分子",
            ],
            &[&["MER-HR-01", "n", "0101", "2026-Q3", "BK-001", "   "]],
        );
        let rows = map_table(&t).unwrap();
        assert_eq!(rows[0].number(Field::Numerator), None);
    }
}
