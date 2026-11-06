//! 写回计划：把结果契约的 5 个新列落成「写哪些格子、写什么值」的明确计划。
//!
//! 引擎与表格宏共用同一套列算术 —— 宏只按计划写，不自行统计、不猜口径。
//!
//! **写回只碰新列**：计划覆盖的单元格全部落在原表已有列之后（表头行写列名，
//! 数据行写行结果），原表已有列、单元格格式与公式都不在计划覆盖范围内。
//! 这条不变量由 `tests/writeback_test.rs` 拿真实带公式的样例表逐格核对。

use std::collections::{BTreeMap, BTreeSet};

use crate::contract::{Contract, ContractColumn, NEW_COLUMNS};

/// 一张来源表（Excel 工作表）的写回计划。
#[derive(Debug, Clone)]
pub struct WriteBackPlan {
    /// 来源表名（Excel 工作表名 / CSV 文件名主干）
    pub sheet: String,
    /// 原表已有列数（新列从第 `original_col_count + 1` 列开始）
    pub original_col_count: usize,
    /// 表头所在行（1 起）
    pub header_row: usize,
    /// 5 个新列的写回定义
    pub columns: Vec<ContractColumn>,
    /// 新列首列号（1 起）
    pub first_new_col: usize,
    /// 数据首行行号（表头行 + 1）
    pub first_data_row: usize,
    /// 数据末行行号
    pub last_data_row: usize,
    /// 写回值：`values[行序][新列序]`，行序从 0 起
    pub values: Vec<Vec<String>>,
}

impl WriteBackPlan {
    /// 本表没有待写回的行
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// 新列的列名（顺序即写回顺序）
    pub fn column_names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.name.clone()).collect()
    }

    /// 表头行要写的单元格：`(单元格地址, 列名)`
    pub fn header_cells(&self) -> Vec<(String, String)> {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    cell_address(self.header_row, self.first_new_col + i),
                    c.name.clone(),
                )
            })
            .collect()
    }

    /// 数据区的区域地址（如 `M2:Q7`）；无数据时返回空串
    pub fn data_range(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let last_new_col = self.first_new_col + self.columns.len() - 1;
        format!(
            "{}:{}",
            cell_address(self.first_data_row, self.first_new_col),
            cell_address(self.last_data_row, last_new_col)
        )
    }

    /// 计划触碰到的列号集合（应全部大于 `original_col_count`）
    pub fn touched_columns(&self) -> BTreeSet<usize> {
        let mut set: BTreeSet<usize> = BTreeSet::new();
        for i in 0..self.columns.len() {
            set.insert(self.first_new_col + i);
        }
        set
    }

    /// 计划触碰到的行号集合（表头行 + 数据行）
    pub fn touched_rows(&self) -> BTreeSet<usize> {
        let mut set: BTreeSet<usize> = BTreeSet::new();
        set.insert(self.header_row);
        for r in self.first_data_row..=self.last_data_row {
            set.insert(r);
        }
        set
    }
}

/// 列号 → 列字母（1 → A，27 → AA）
pub fn column_letter(col: usize) -> String {
    let mut n = col;
    let mut out = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        out.push((b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    out.iter().rev().collect()
}

/// 单元格地址（1 起的行列号 → A1 式地址）
pub fn cell_address(row: usize, col: usize) -> String {
    format!("{}{}", column_letter(col), row)
}

/// 为一张来源表编写回计划。
///
/// `original_col_count` = 该表**已有**列数（表头宽度）。新列一律接在它后面，
/// 因此计划不会覆盖原表任何一格。
pub fn plan_for_sheet(
    contract: &Contract,
    sheet: &str,
    original_col_count: usize,
) -> WriteBackPlan {
    let rows: Vec<&crate::contract::RowResult> = contract
        .rows
        .iter()
        .filter(|r| r.source_ns == sheet)
        .collect();

    let values: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.conclusion.clone(),
                r.issue_types.clone(),
                r.issue_detail.clone(),
                r.suspected_duplicate.clone(),
                r.submission_status.clone(),
            ]
        })
        .collect();

    let first_new_col = original_col_count + 1;
    let first_data_row = 2usize;
    let last_data_row = first_data_row + values.len().saturating_sub(1);

    WriteBackPlan {
        sheet: sheet.to_string(),
        original_col_count,
        header_row: 1,
        columns: contract.columns.clone(),
        first_new_col,
        first_data_row,
        last_data_row,
        values,
    }
}

/// 按来源表分组编计划。`col_counts` 给出每张表的已有列数。
pub fn plans_by_sheet(
    contract: &Contract,
    col_counts: &BTreeMap<String, usize>,
) -> BTreeMap<String, WriteBackPlan> {
    let mut sheets: Vec<String> = Vec::new();
    for row in &contract.rows {
        if !sheets.contains(&row.source_ns) {
            sheets.push(row.source_ns.clone());
        }
    }
    let mut out = BTreeMap::new();
    for sheet in sheets {
        let cols = col_counts.get(&sheet).copied().unwrap_or(0);
        out.insert(sheet.clone(), plan_for_sheet(contract, &sheet, cols));
    }
    out
}

/// 5 个新列的列名（与 `contract::NEW_COLUMNS` 同源，便于插件侧对齐）
pub fn new_column_names() -> Vec<String> {
    NEW_COLUMNS.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{build, Extra};
    use crate::ingest::RowMeta;

    fn meta(uid: &str, ns: &str) -> RowMeta {
        RowMeta {
            row_uid: uid.to_string(),
            source_ns: ns.to_string(),
            source_file: "t.csv".into(),
            row_no: 1,
            indicator_code: Some("MER-HR-01".into()),
            dept_code: Some("0101".into()),
            reporting_period: Some("2026-Q3".into()),
            business_key: Some("BK-001".into()),
        }
    }

    #[test]
    fn 列号字母换算跨过二十六与二十七() {
        assert_eq!(column_letter(1), "A");
        assert_eq!(column_letter(26), "Z");
        assert_eq!(column_letter(27), "AA");
        assert_eq!(cell_address(2, 13), "M2");
    }

    #[test]
    fn 计划只覆盖新列_原表列不动() {
        let c = build(
            "b1",
            &[meta("u1", "表一"), meta("u2", "表一")],
            vec![],
            1,
            1,
            &Extra::default(),
        );
        let plan = plan_for_sheet(&c, "表一", 11);
        assert_eq!(plan.first_new_col, 12);
        assert_eq!(plan.first_data_row, 2);
        assert_eq!(plan.last_data_row, 3);
        assert_eq!(plan.data_range(), "L2:P3");
        for col in plan.touched_columns() {
            assert!(col > plan.original_col_count, "新列不应压在原表列上");
        }
    }

    #[test]
    fn 表头单元格地址与列名一致() {
        let c = build("b1", &[meta("u1", "表一")], vec![], 1, 1, &Extra::default());
        let plan = plan_for_sheet(&c, "表一", 11);
        let cells = plan.header_cells();
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[0], ("L1".to_string(), "校验结论".to_string()));
        assert_eq!(cells[4], ("P1".to_string(), "上报状态".to_string()));
    }

    #[test]
    fn 空表编译为空计划且不报错() {
        let c = build("b1", &[], vec![], 0, 1, &Extra::default());
        let plan = plan_for_sheet(&c, "表一", 11);
        assert!(plan.is_empty());
        assert_eq!(plan.data_range(), "");
    }
}
