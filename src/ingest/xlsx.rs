//! Excel（xlsx/xlsm）读取路径。
//!
//! 一个工作簿里的每个工作表算一张来源表，工作表名即来源命名空间 ——
//! 信息科把各科室填好的表并进同一个模板后，各表互不污染。

use std::path::Path;

use calamine::{open_workbook_auto, Data, Reader};

use super::SourceTable;
use crate::errors::{EngineError, ErrorCode};

/// 读工作簿里的全部工作表。
pub fn read_all(path: &Path) -> Result<Vec<SourceTable>, EngineError> {
    let mut workbook = open_workbook_auto(path).map_err(|e| {
        EngineError::at(
            ErrorCode::InputUnreadable,
            format!("Excel 打开失败：{}", e),
            path.display().to_string(),
        )
    })?;

    let sheets = workbook.worksheets();
    if sheets.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            "工作簿里没有工作表",
            path.display().to_string(),
        ));
    }

    let mut out = Vec::with_capacity(sheets.len());
    for (name, range) in sheets {
        let mut header: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<String>> = Vec::new();
        for (i, row) in range.rows().enumerate() {
            let cells: Vec<String> = row.iter().map(cell_to_string).collect();
            if i == 0 {
                header = cells;
            } else {
                rows.push(cells);
            }
        }
        out.push(SourceTable {
            source_ns: name.clone(),
            source_file: path.display().to_string(),
            header,
            rows,
            format: super::FormatInfo {
                encoding: "xlsx（工作簿原生）".to_string(),
                delimiter: "xlsx（单元格原生）".to_string(),
                note: String::new(),
            },
        });
    }
    Ok(out)
}

/// 单元格取值一律转成文本；空单元格转空串（判定交给规则层，不在这里丢行）。
fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERR:{:?}", e),
        Data::Empty => String::new(),
    }
}
