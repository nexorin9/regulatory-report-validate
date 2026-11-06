//! 写回不动原表：带公式的样例表写回后，原表已有列与公式列一律原样。
//!
//! 样例表 `tests/fixtures/formula_sample.xlsx` 是带真公式的 xlsx（末列 `=F+G`）。
//! 本测试：
//!   1. 用引擎读这张表 → 拿到结果契约；
//!   2. 按写回计划把 5 个新列写进一份「表模型」（值取自真实文件，公式取自
//!      xlsx 内部的 sheet XML）；
//!   3. 逐格核对：计划没碰过的格子（含公式格）必须与写回前完全一致；
//!   4. 把写回后的表导成 CSV 再跑一遍校验 —— 结论必须与写回前一致，
//!      说明写回没有把数据写坏。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use calamine::{open_workbook_auto, Data, Reader};
use regval::contract::Contract;
use regval::writeback;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 表模型里的一个格子
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Value(String),
    Formula { formula: String, cached: String },
}

type Sheet = BTreeMap<(usize, usize), Cell>;

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

/// 读 xlsx 的全部单元格值（1 起的行列号）
fn read_values(path: &Path) -> Sheet {
    let mut wb = open_workbook_auto(path).expect("样例表应可读");
    let mut sheet: Sheet = BTreeMap::new();
    for (_name, range) in wb.worksheets() {
        for (i, row) in range.rows().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                let value = cell_to_string(cell);
                if !value.is_empty() {
                    sheet.insert((i + 1, j + 1), Cell::Value(value));
                }
            }
        }
    }
    sheet
}

/// 从 xlsx 的 sheet XML 里抠出公式格（地址 → 公式文本）
fn read_formulas(path: &Path) -> BTreeMap<String, String> {
    let file = std::fs::File::open(path).expect("样例表应可打开");
    let mut archive = zip::ZipArchive::new(file).expect("xlsx 应是合法 zip");
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("条目应可读");
        let name = entry.name().to_string();
        if !(name.starts_with("xl/worksheets/") && name.ends_with(".xml")) {
            continue;
        }
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .expect("sheet XML 应是 UTF-8");
        for (addr, formula) in scan_formula_cells(&xml) {
            out.insert(addr, formula);
        }
    }
    out
}

/// 扫描 sheet XML 里的公式格
fn scan_formula_cells(xml: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = xml[pos..].find("<c ") {
        let start = pos + rel;
        let tail = &xml[start..];
        let close = tail.find("</c>").unwrap_or(tail.len());
        let next_cell = tail[3..].find("<c ").map(|i| i + 3).unwrap_or(usize::MAX);
        let end = close.min(next_cell);
        let cell = &tail[..end];

        let addr = cell.find("r=\"").and_then(|i| {
            let rest = &cell[i + 3..];
            rest.find('"').map(|j| rest[..j].to_string())
        });
        let formula = cell.find("<f>").and_then(|i| {
            let rest = &cell[i + 3..];
            let end = rest.find("</f>").unwrap_or(rest.len());
            Some(rest[..end].to_string())
        });

        if let (Some(addr), Some(formula)) = (addr, formula) {
            out.push((addr, formula));
        }
        pos = start + 4;
    }
    out
}

/// "L2" → (行 2, 列 12)
fn parse_addr(addr: &str) -> (usize, usize) {
    let split = addr
        .find(|c: char| c.is_ascii_digit())
        .expect("地址应含行号");
    let (letters, digits) = addr.split_at(split);
    let col = letters
        .bytes()
        .fold(0usize, |acc, b| acc * 26 + (b - b'A' + 1) as usize);
    (digits.parse::<usize>().unwrap(), col)
}

/// 跑一次 `regval validate`，返回契约
fn run_validate(input: &Path, out: &Path, state_db: &Path) -> Contract {
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = std::fs::read_to_string(out).unwrap();
    serde_json::from_str(&raw).expect("契约 JSON 应可解析")
}

/// 按写回计划把新列写进表模型（镜像宏的写回：只写新列那几格）
fn apply_plan(sheet: &mut Sheet, plan: &writeback::WriteBackPlan) {
    for (addr, name) in plan.header_cells() {
        let (_, col) = parse_addr(&addr);
        sheet.insert((plan.header_row, col), Cell::Value(name));
    }
    for (row_idx, values) in plan.values.iter().enumerate() {
        let row = plan.first_data_row + row_idx;
        for (col_idx, value) in values.iter().enumerate() {
            let col = plan.first_new_col + col_idx;
            sheet.insert((row, col), Cell::Value(value.clone()));
        }
    }
}

/// 把表模型导成 CSV（写回后的表就是这样被人接手的）
fn sheet_to_csv(sheet: &Sheet, rows: usize, cols: usize, path: &Path) {
    let mut wtr = csv::Writer::from_path(path).expect("CSV 应可写");
    for row in 1..=rows {
        let mut record: Vec<String> = Vec::with_capacity(cols);
        for col in 1..=cols {
            let text = match sheet.get(&(row, col)) {
                Some(Cell::Value(v)) => v.clone(),
                Some(Cell::Formula { cached, .. }) => cached.clone(),
                None => String::new(),
            };
            record.push(text);
        }
        wtr.write_record(&record).unwrap();
    }
    wtr.flush().unwrap();
}

#[test]
fn 写回只碰新列_原表列与公式一律不动() {
    let dir = tempfile::tempdir().unwrap();
    let sample = fixture("formula_sample.xlsx");
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let contract = run_validate(&sample, &out, &state_db);
    assert_eq!(contract.rows.len(), 5, "样例表 5 行");

    // 原表 12 列（11 个业务列 + 1 个公式列）
    let original_col_count = 12usize;
    let plan = writeback::plan_for_sheet(&contract, "上报模板", original_col_count);

    // 计划本身就不许压到原表列上
    for col in plan.touched_columns() {
        assert!(col > original_col_count, "写回计划碰到了原表第 {} 列", col);
    }

    // 建表模型：值来自真实文件，公式来自 xlsx 内部 XML
    let mut sheet = read_values(&sample);
    let formulas = read_formulas(&sample);
    assert!(
        !formulas.is_empty(),
        "样例表必须真的带公式，否则这条测试是空转"
    );
    for (addr, formula) in &formulas {
        let (row, col) = parse_addr(addr);
        assert!(col <= original_col_count, "公式列应在原表范围内：{}", addr);
        let cached = match sheet.get(&(row, col)) {
            Some(Cell::Value(v)) => v.clone(),
            _ => String::new(),
        };
        sheet.insert(
            (row, col),
            Cell::Formula {
                formula: formula.clone(),
                cached,
            },
        );
    }
    let before = sheet.clone();

    // 写回
    apply_plan(&mut sheet, &plan);

    // 1) 计划没碰过的格子必须一模一样（含公式格）
    let mut checked = 0usize;
    for ((row, col), cell) in before.iter() {
        if plan.touched_columns().contains(col)
            && (row == &plan.header_row
                || (row >= &plan.first_data_row && row <= &plan.last_data_row))
        {
            continue; // 新列区域，允许变化
        }
        assert_eq!(
            sheet.get(&(*row, *col)),
            Some(cell),
            "格子 ({}, {}) 被写回改动了",
            row,
            col
        );
        checked += 1;
    }
    assert!(
        checked >= 12 * 5,
        "应逐格核对过原表区域，实际 {} 格",
        checked
    );

    // 2) 公式格原样保留
    for (addr, formula) in &formulas {
        let (row, col) = parse_addr(addr);
        match sheet.get(&(row, col)) {
            Some(Cell::Formula { formula: f, .. }) => {
                assert_eq!(f, formula, "公式 {} 被改写", addr)
            }
            other => panic!("公式格 {} 变成了 {:?}", addr, other),
        }
    }

    // 3) 新列写进去的值与契约逐字段一致
    for (row_idx, values) in plan.values.iter().enumerate() {
        let row = plan.first_data_row + row_idx;
        for (col_idx, expected) in values.iter().enumerate() {
            let col = plan.first_new_col + col_idx;
            assert_eq!(
                sheet.get(&(row, col)),
                Some(&Cell::Value(expected.clone())),
                "新列 ({}, {}) 的值与契约不一致",
                row,
                col
            );
        }
    }
    // 列头落在原表首行之后
    for (i, name) in regval::contract::NEW_COLUMNS.iter().enumerate() {
        assert_eq!(
            sheet.get(&(1, original_col_count + 1 + i)),
            Some(&Cell::Value(name.to_string())),
            "列头应写在原表已有列之后"
        );
    }
}

#[test]
fn 写回后再校验_结论与写回前一致() {
    let dir = tempfile::tempdir().unwrap();
    let sample = fixture("formula_sample.xlsx");
    let out1 = dir.path().join("c1.json");
    let state_db = dir.path().join("state.db");
    let contract_before = run_validate(&sample, &out1, &state_db);

    let plan = writeback::plan_for_sheet(&contract_before, "上报模板", 12);
    let mut sheet = read_values(&sample);
    for (addr, formula) in read_formulas(&sample) {
        let (row, col) = parse_addr(&addr);
        let cached = match sheet.get(&(row, col)) {
            Some(Cell::Value(v)) => v.clone(),
            _ => String::new(),
        };
        sheet.insert((row, col), Cell::Formula { formula, cached });
    }
    apply_plan(&mut sheet, &plan);

    // 写回后的表（12 个原列 + 5 个新列）导成 CSV 再校验
    let csv_path = dir.path().join("after_writeback.csv");
    sheet_to_csv(&sheet, 6, 17, &csv_path);

    let out2 = dir.path().join("c2.json");
    let state_db2 = dir.path().join("state2.db");
    let contract_after = run_validate(&csv_path, &out2, &state_db2);

    assert_eq!(
        contract_after.summary.issue_count, contract_before.summary.issue_count,
        "写回后再校验的问题数应与写回前一致"
    );
    assert_eq!(
        contract_after.rows.len(),
        contract_before.rows.len(),
        "写回后再校验的行数应与写回前一致"
    );
    for (before, after) in contract_before.rows.iter().zip(contract_after.rows.iter()) {
        assert_eq!(before.conclusion, after.conclusion, "行的校验结论变了");
        assert_eq!(before.issue_types, after.issue_types, "行的问题类型变了");
    }
}

#[test]
fn 契约版本不符时引擎拒绝写出结果文件() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .arg("--expect-schema-version")
        .arg("regval.contract/9.9.9")
        .output()
        .expect("regval 应可执行");

    assert!(!output.status.success(), "版本不符时应以非 0 退出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CONTRACT_VERSION_MISMATCH"),
        "应报出错误码：{}",
        stderr
    );
    assert!(
        !out.exists(),
        "版本不符时不应产出契约文件（宏据此不做部分写回）"
    );
}
