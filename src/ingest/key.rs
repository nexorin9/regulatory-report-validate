//! 行键、业务主键、跨表匹配键与权威键。
//!
//! 四层键各管一件事；少一层，某个业务环节就会串行：
//!
//! | 键 | 由什么拼成 | 干什么用 |
//! |---|---|---|
//! | `row_uid` | 来源命名空间 + 文件主干 + 业务主键 + 同键行序 | 定位到**一行**：口径问题、人工确认、回执都以它为锚 |
//! | `business_key` | 指标编号 + 科室代码 + 报告期 + 业务主键 | 定位到**一条业务记录**（跨表对齐的第一层） |
//! | `match_key` | 指标编号 + 科室代码 + 报告期 | 跨表对齐的第二层：同匹配键下再按业务主键分记录 |
//! | `authority_key` | 机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键 | **权威键**：一条记录只能有一行权威数据（本机库唯一索引约束的就是它） |
//!
//! # 行键为什么带「同键行序」
//!
//! 同一份表里同一个业务主键可能出现多行（重复填报）。这些行必须各有各的行键，
//! 否则口径问题、人工确认、回执会串到别的行上去。行序只取「同一业务主键下第几行」，
//! **不取行在表里的绝对位置** —— 表里插进别的指标的行，各行行键不变。
//!
//! # 跨表对齐为什么不看行键
//!
//! 行键含来源命名空间，跨来源表天然不同；跨表对齐一律走 `match_key` + `business_key`。
//!
//! # 键值不做归一
//!
//! 这里只去首尾空白，不做全半角、单位别名、指标名别名等归一 —— 那是重复检测漏斗
//! 第二级（归一化候选）的活。键层要的是「同一份数据两次算出来一样」，不是「写法不同也认作一样」。

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{file_stem, Field, SourceRow};

/// 键的字段分隔符。
///
/// 科室代码、指标编号、报告期、业务主键都不含这个字符，用它拼键不会歧义。
const SEP: &str = "|";

/// 一行的全部键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowKeys {
    /// 行键：定位到一行
    pub row_uid: String,
    /// 业务主键：定位到一条业务记录
    pub business_key: String,
    /// 跨表匹配键：指标编号 + 科室代码 + 报告期
    pub match_key: String,
    /// 权威键：一条业务记录只能有一行权威数据
    pub authority_key: String,
}

/// 业务主键：指标编号 + 科室代码 + 报告期 + 业务主键。
pub fn business_key(row: &SourceRow) -> String {
    join(&[
        text(row, Field::IndicatorCode),
        text(row, Field::DeptCode),
        text(row, Field::ReportingPeriod),
        text(row, Field::BusinessKey),
    ])
}

/// 跨表匹配键：指标编号 + 科室代码 + 报告期。
pub fn match_key(row: &SourceRow) -> String {
    join(&[
        text(row, Field::IndicatorCode),
        text(row, Field::DeptCode),
        text(row, Field::ReportingPeriod),
    ])
}

/// 权威键：机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键。
///
/// 段序与本机库 `rows` 表唯一索引 `idx_rows_authority` 的列序一致 ——
/// 改这里就要同步改索引与迁移。
pub fn authority_key(row: &SourceRow) -> String {
    join(&[
        text(row, Field::OrgCode),
        text(row, Field::ReportingPeriod),
        text(row, Field::IndicatorCode),
        text(row, Field::DeptCode),
        text(row, Field::BusinessKey),
    ])
}

/// 行键：来源命名空间 + 文件主干 + 业务主键 + 同键行序。
///
/// 同一份输入两次 ingest 必得同一 `row_uid`（纯函数，不含时间与随机数）。
pub fn row_uid(row: &SourceRow) -> String {
    let raw = join(&[
        row.source_ns.clone(),
        file_stem(&row.source_file),
        business_key(row),
        row.dup_ordinal.to_string(),
    ]);
    sha256_hex(raw.as_bytes())
}

/// 一行的全部键。
pub fn keys(row: &SourceRow) -> RowKeys {
    RowKeys {
        row_uid: row_uid(row),
        business_key: business_key(row),
        match_key: match_key(row),
        authority_key: authority_key(row),
    }
}

/// 给同一张来源表里的行编号：同一业务主键下按出现次序编号。
///
/// 只依赖「同键行在表内的相对次序」，不依赖行的绝对位置 —— 表里插进别的指标的行，
/// 各行行键不变（行序变化后仍能按业务主键对齐）。
pub fn assign_dup_ordinals(rows: &mut [SourceRow]) {
    let mut counter: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows.iter_mut() {
        let bk = business_key(row);
        let next = counter.entry(bk).or_insert(0);
        row.dup_ordinal = *next;
        *next += 1;
    }
}

/// 键的四要素是否齐。
///
/// 缺要素的行**不参与绑定**：它们的「必填缺失」问题由口径规则报，
/// 这里再绑一次只会把同一个错报两遍。
pub fn is_bindable(row: &SourceRow) -> bool {
    !text(row, Field::IndicatorCode).is_empty()
        && !text(row, Field::DeptCode).is_empty()
        && !text(row, Field::ReportingPeriod).is_empty()
        && !text(row, Field::BusinessKey).is_empty()
}

/// 取值（去首尾空白；空串算空）
fn text(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

fn join(parts: &[String]) -> String {
    parts.join(SEP)
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(source_ns: &str, row_no: usize, business_key: &str, value: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-HR-01".to_string());
        values.insert(Field::IndicatorName, "病案首页完整率".to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, business_key.to_string());
        values.insert(Field::Value, value.to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    #[test]
    fn 四层键的段序固定() {
        let r = row("表一", 1, "BK-01", "50");
        assert_eq!(business_key(&r), "MER-HR-01|0301|2026-Q3|BK-01");
        assert_eq!(match_key(&r), "MER-HR-01|0301|2026-Q3");
        assert_eq!(authority_key(&r), "ORG-001|2026-Q3|MER-HR-01|0301|BK-01");
    }

    #[test]
    fn 同键多行各有各的行键() {
        let mut rows = vec![row("表一", 1, "BK-01", "50"), row("表一", 2, "BK-01", "60")];
        assign_dup_ordinals(&mut rows);
        assert_ne!(row_uid(&rows[0]), row_uid(&rows[1]));
        assert_eq!(business_key(&rows[0]), business_key(&rows[1]));
    }

    #[test]
    fn 行序变化不影响行键() {
        let mut a = vec![row("表一", 1, "BK-01", "50"), row("表一", 2, "BK-02", "60")];
        // 同一份数据换个行序再来一遍
        let mut b = vec![row("表一", 1, "BK-02", "60"), row("表一", 2, "BK-01", "50")];
        assign_dup_ordinals(&mut a);
        assign_dup_ordinals(&mut b);
        let uid_a = |bk: &str| {
            a.iter()
                .find(|r| r.get_trimmed(Field::BusinessKey) == Some(bk))
                .map(|r| row_uid(r))
                .unwrap()
        };
        let uid_b = |bk: &str| {
            b.iter()
                .find(|r| r.get_trimmed(Field::BusinessKey) == Some(bk))
                .map(|r| row_uid(r))
                .unwrap()
        };
        assert_eq!(uid_a("BK-01"), uid_b("BK-01"));
        assert_eq!(uid_a("BK-02"), uid_b("BK-02"));
    }

    #[test]
    fn 缺要素的行不参与绑定() {
        let mut r = row("表一", 1, "BK-01", "50");
        r.values.insert(Field::BusinessKey, "  ".to_string());
        assert!(!is_bindable(&r));
    }
}
