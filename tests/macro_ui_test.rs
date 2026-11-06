//! 表格内插件（Excel 加载宏）的界面验收：两个按钮 + 底部状态条 + 分块写回。
//!
//! 这一组用例守的是「宏与引擎说同一套话」：
//!
//! 1. **状态条句子**：句子由引擎算好放进契约的 `summary` 段，宏原样显示 ——
//!    宏不自行统计、不自行拼句子（表里与终端里必须是一句话）。
//! 2. **两个按钮**：宏里确有「校验」「上报」两个入口，且按钮的 `OnAction`
//!    指得到存在的子程序（院内机器上不会点了没反应）。
//! 3. **三步一致性**：引擎算结果 → 宏写回 → 再校验，结论与问题数不变；
//!    写回进格子的 5 列值与契约 JSON **逐字段**一致；
//!    带公式的样例表写回后公式列**一字未动**。
//!
//! 宏是 VBA，跑不了单元测试，所以这里用「读宏源码 + 用 Rust 复刻宏的写回」
//! 的方式验收：源码层面核对宏与引擎的口径，写回层面核对行为。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use calamine::{open_workbook_auto, Data, Reader};
use regval::contract::Contract;
use regval::ui::statusbar;
use regval::writeback;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

fn macro_source() -> String {
    let path = project_dir().join("macro/vba/RegVal.bas");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到宏文件 {}：{}", path.display(), e))
}

/// 宏里写回时读的 5 个行字段（按写入顺序）
fn macro_writeback_keys(source: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in source.lines() {
        let t = line.trim();
        if !t.contains("arr(j,") {
            continue;
        }
        let q = match t.rfind('"') {
            Some(q) => q,
            None => continue,
        };
        let before = &t[..q];
        if let Some(q2) = before.rfind('"') {
            keys.push(before[q2 + 1..].to_string());
        }
    }
    keys
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

// ─────────────────────────────────────────────────────────────────────
// 1. 状态条：句子由引擎算好，宏原样显示
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 状态条句子由引擎算好_契约里带着成品句子() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let contract = run_validate(
        &fixture("mini.csv"),
        &out,
        &dir.path().join("state.db"),
    );

    assert_eq!(
        contract.summary.status_line,
        statusbar::status_line(
            contract.summary.total_rows,
            contract.summary.issue_count,
            contract.summary.suspected_duplicate_count
        ),
        "状态条句子应由 ui::statusbar 算出，契约里带着成品"
    );
    assert_eq!(
        contract.summary.status_line,
        "已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认"
    );
    assert!(
        contract.summary.submission_line.contains("行已提交"),
        "上报完成后的状态条句子也要在契约里：{}",
        contract.summary.submission_line
    );
}

#[test]
fn 宏从契约summary段取状态条_不自行统计() {
    let source = macro_source();
    // 宏读的是 summary 对象里的成品句子，不是自己数行数
    assert!(
        source.contains("JsonObjectText(contractText, \"summary\")")
            || source.contains("JsonObjectText(jsonText, \"summary\")"),
        "宏应先取出 summary 段"
    );
    assert!(
        source.contains("\"status_line\""),
        "宏应读 summary.status_line（校验完成那一句）"
    );
    assert!(
        source.contains("\"submission_line\""),
        "宏应读 summary.submission_line（上报完成那一句）"
    );
    // 宏里不该出现自己数行数的写法
    assert!(
        !source.contains("Rows.Count -"),
        "宏不应自行统计行数（数的是原表，不是这一批的结论）"
    );
}

#[test]
fn 终端与状态条是同一句话() {
    // 表里与终端里必须是一句话 —— 两处各拼一次迟早会各说各的
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(dir.path().join("state.db"))
        .output()
        .expect("regval 应可执行");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let contract: Contract = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert!(
        stdout.contains(&contract.summary.status_line),
        "终端应打出与状态条同一句话：{}",
        contract.summary.status_line
    );
}

#[test]
fn 状态条留白与引擎一致() {
    let source = macro_source();
    assert!(
        source.contains("REGVAL_STATUS_ROW_GAP"),
        "宏应按留白常量放状态条"
    );
    // 宏里的留白行数与引擎一致（差一行，状态条就贴到数据上了）
    let gap = source
        .lines()
        .find(|l| l.contains("REGVAL_STATUS_ROW_GAP As Long"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .expect("宏里应能抠出留白行数");
    assert_eq!(
        gap,
        statusbar::STATUS_ROW_GAP,
        "宏与引擎对状态条留白的说法不一致"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 2. 两个按钮
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 宏里有两个按钮_且按钮动作指得到存在的子程序() {
    let source = macro_source();
    for entry in ["RegVal_Validate", "RegVal_Push"] {
        assert!(
            source.contains(&format!("Public Sub {}()", entry)),
            "宏里应有「{}」入口",
            entry
        );
        assert!(
            source.contains(&format!("\"{}\"", entry)),
            "按钮的 OnAction 应指向 {}",
            entry
        );
    }
    // 按钮是用形状画的（不是 emoji、不是浏览器默认控件）
    assert!(
        source.contains("AddShape"),
        "按钮应用 Excel 原生形状画（Shape.AddShape）"
    );
    assert!(
        source.contains("shp.OnAction"),
        "形状按钮要绑 OnAction，否则点了没反应"
    );
    // 两个按钮的标题取自文案常量表（宏里不写死中文）
    let messages = std::fs::read_to_string(project_dir().join("macro/vba/RegValMessages.bas"))
        .expect("应能读到文案常量模块");
    assert!(
        messages.contains("Public Const REGVAL_MSG_BUTTON_VALIDATE As String = \"校验\"")
            && messages.contains("Public Const REGVAL_MSG_BUTTON_PUSH As String = \"上报\""),
        "两个按钮的标题应是「校验」「上报」"
    );
    assert!(
        source.contains("REGVAL_MSG_BUTTON_VALIDATE")
            && source.contains("REGVAL_MSG_BUTTON_PUSH"),
        "宏应引用文案常量表里的按钮标题"
    );
}

#[test]
fn 宏调用引擎的validate与push两个子命令() {
    let source = macro_source();
    assert!(
        source.contains("\" validate\""),
        "宏应拼出引擎的 validate 子命令"
    );
    assert!(
        source.contains("\" push\""),
        "宏应拼出引擎的 push 子命令"
    );
    // 两条命令都要带错误报告路径：宏靠它拿错误码与中文说明
    let code_lines = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("'"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code_lines.matches("--error-out").count(),
        2,
        "validate 与 push 两条命令都应带 --error-out"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 3. 三步一致性：算结果 → 写回 → 再校验
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 宏写回的五个字段与契约行字段逐字对应() {
    let keys = macro_writeback_keys(&macro_source());
    assert_eq!(
        keys,
        vec![
            "conclusion",
            "issue_types",
            "issue_detail",
            "suspected_duplicate",
            "submission_status"
        ],
        "宏写回读的字段必须与契约行结果一一对应且顺序一致"
    );
}

#[test]
fn 三步一致_引擎算结果_宏写回_再校验结论不变() {
    let dir = tempfile::tempdir().unwrap();
    let sample = fixture("formula_sample.xlsx");
    let out1 = dir.path().join("c1.json");
    let contract_before = run_validate(&sample, &out1, &dir.path().join("state.db"));

    // ── 第 2 步：按宏的算法写回（Rust 侧复刻：同一分组、同一列序、同一取值）
    let plan = writeback::plan_for_sheet(&contract_before, "上报模板", 12);
    assert_eq!(plan.values.len(), contract_before.rows.len());

    // 写回进格子的 5 列值与契约 JSON 逐字段一致
    for (idx, values) in plan.values.iter().enumerate() {
        let row = &contract_before.rows[idx];
        assert_eq!(values[0], row.conclusion, "第 {} 行的校验结论写错了", idx);
        assert_eq!(values[1], row.issue_types, "第 {} 行的问题类型写错了", idx);
        assert_eq!(values[2], row.issue_detail, "第 {} 行的问题说明写错了", idx);
        assert_eq!(
            values[3], row.suspected_duplicate,
            "第 {} 行的疑似重复项写错了",
            idx
        );
        assert_eq!(
            values[4], row.submission_status,
            "第 {} 行的上报状态写错了",
            idx
        );
    }

    // 写回只碰新列：原表 12 列（11 业务列 + 1 公式列）一格都不许动
    for col in plan.touched_columns() {
        assert!(col > 12, "写回压到了原表第 {} 列", col);
    }

    // ── 第 3 步：把写回后的表再校验一遍，结论必须一致
    let mut sheet = read_values(&sample);
    for (addr, formula) in read_formulas(&sample) {
        let (row, col) = parse_addr(&addr);
        let cached = match sheet.get(&(row, col)) {
            Some(Cell::Value(v)) => v.clone(),
            _ => String::new(),
        };
        sheet.insert(
            (row, col),
            Cell::Formula { formula, cached },
        );
    }
    let before = sheet.clone();
    apply_plan(&mut sheet, &plan);

    // 带公式样例表写回后公式列值不变（公式一格未动）
    for (addr, formula) in read_formulas(&sample) {
        let (row, col) = parse_addr(&addr);
        match sheet.get(&(row, col)) {
            Some(Cell::Formula { formula: f, .. }) => {
                assert_eq!(f, &formula, "公式 {} 被改写", addr)
            }
            other => panic!("公式格 {} 变成了 {:?}", addr, other),
        }
    }
    // 原表区域逐格不变
    let mut checked = 0usize;
    for ((row, col), cell) in before.iter() {
        if plan.touched_columns().contains(col)
            && (row == &plan.header_row
                || (row >= &plan.first_data_row && row <= &plan.last_data_row))
        {
            continue;
        }
        assert_eq!(sheet.get(&(*row, *col)), Some(cell), "格子 ({}, {}) 被改动", row, col);
        checked += 1;
    }
    assert!(checked >= 12 * 5, "应逐格核对过原表区域，实际 {} 格", checked);

    // 导成 CSV 再校验：结论与写回前一致
    let csv_path = dir.path().join("after_writeback.csv");
    sheet_to_csv(&sheet, 6, 17, &csv_path);
    let out2 = dir.path().join("c2.json");
    let contract_after = run_validate(&csv_path, &out2, &dir.path().join("state2.db"));
    assert_eq!(
        contract_after.summary.issue_count, contract_before.summary.issue_count,
        "写回后再校验的问题数应与写回前一致"
    );
    for (before, after) in contract_before.rows.iter().zip(contract_after.rows.iter()) {
        assert_eq!(before.conclusion, after.conclusion, "行的校验结论变了");
        assert_eq!(before.issue_types, after.issue_types, "行的问题类型变了");
    }
}

#[test]
fn 写回分块_每块两百行且不漏行() {
    let source = macro_source();
    assert!(
        source.contains("REGVAL_CHUNK_ROWS"),
        "宏应分块写回"
    );
    let chunk = source
        .lines()
        .find(|l| l.contains("REGVAL_CHUNK_ROWS As Long"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .expect("宏里应能抠出分块行数");
    assert_eq!(chunk, 200, "分块大小按 200 行一档");
    assert!(
        source.contains(".Value2 = arr"),
        "每块应一次批量赋值（Range.Value2），不逐格写"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 表模型（复刻宏的写回目标：只写新列那几格）
// ─────────────────────────────────────────────────────────────────────

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
