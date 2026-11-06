//! 口径差异清单的产物（CSV 表格 / JSON）。
//!
//! 差异清单是**换版前**给质控岗与科室看的东西：两版口径到底哪几条不一样、
//! 每一处的前后取值是什么。表格一行一处变化，列定义固定，便于在 Excel 里
//! 逐条核；JSON 保留完整结构，便于机器比对。

use crate::rules::version::CaliberDiff;

/// 差异清单的列定义（CSV 表头，固定顺序）
pub const COLUMNS: [&str; 7] = [
    "适用指标",
    "检查类型",
    "检查项",
    "变化",
    "变化项",
    "前值",
    "后值",
];

/// 差异清单 → CSV 文本
///
/// 取值里的换行与逗号要转义（`"..."` + 双写引号），否则表格会错行 ——
/// 口径取值里带逗号是常事（枚举清单、单位别名表）。
pub fn to_csv(diff: &CaliberDiff) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(&COLUMNS.join(","));
    out.push('\n');
    for change in &diff.changes {
        let row = [
            change.indicator_code.clone(),
            change.check_kind.clone(),
            change.check_column.clone(),
            change.change.clone(),
            change.field.clone(),
            change.before.clone(),
            change.after.clone(),
        ];
        let cells: Vec<String> = row.iter().map(|c| escape_csv(c)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(out)
}

/// 差异清单 → JSON 文本
pub fn to_json(diff: &CaliberDiff) -> Result<String, String> {
    serde_json::to_string_pretty(diff).map_err(|e| format!("口径差异清单序列化失败：{}", e))
}

/// CSV 单元格转义
fn escape_csv(raw: &str) -> String {
    let needs_quote =
        raw.contains(',') || raw.contains('"') || raw.contains('\n') || raw.contains('\r');
    if !needs_quote {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::version::CaliberChange;

    fn diff_with(before: &str, after: &str) -> CaliberDiff {
        CaliberDiff {
            from_version: "2021.1".to_string(),
            to_version: "2022.1".to_string(),
            changes: vec![CaliberChange {
                indicator_code: "MER-TL-04".to_string(),
                check_kind: "单位不符".to_string(),
                check_column: "unit".to_string(),
                change: "修改".to_string(),
                field: "期望单位".to_string(),
                before: before.to_string(),
                after: after.to_string(),
            }],
            compared_slots: 1,
            unchanged_slots: 0,
        }
    }

    #[test]
    fn csv_表头固定且每处变化一行() {
        let csv = to_csv(&diff_with("%", "例")).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("适用指标,检查类型,检查项,变化,变化项,前值,后值"));
        assert!(lines[1].contains("MER-TL-04"));
        assert!(lines[1].contains("期望单位"));
    }

    #[test]
    fn csv_取值里的逗号与引号会转义() {
        let csv = to_csv(&diff_with("%, 例", "例")).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "取值里的换行会分行，但逗号不该分行：{}",
            csv
        );
        assert!(lines[1].contains("\"%, 例\""));
    }

    #[test]
    fn json_保留完整结构() {
        let json = to_json(&diff_with("%", "例")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["from_version"], "2021.1");
        assert_eq!(parsed["changes"][0]["after"], "例");
    }
}
