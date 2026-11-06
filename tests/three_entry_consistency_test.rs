//! 三入口契约一致性：Excel 宏 / WPS JSA / CSV 直跑 —— 同一份样例，产出同一份契约。
//!
//! # 这条测试在证明什么
//!
//! 产品有三个入口，科室可能只碰得到其中一个：
//!
//! | 入口 | 谁用 | 交接面 |
//! |------|------|--------|
//! | Excel 加载宏 | 桌面 Excel | 引擎产出的契约 JSON → 宏写回 5 个新列 |
//! | WPS JSA 宏 | 装了 WPS 的报送机 | 同上（同一份契约、同一套写回） |
//! | CSV 直跑 | 禁宏的专网报送机 | 引擎产出的契约 JSON → 人工按列粘贴 |
//!
//! 「同构」不是「三个入口各写一份代码」，而是**一份契约、一种判定**：
//! 同一个科室表，从哪条入口进去，得到的结论、问题数、疑似重复、状态条句子
//! 都得一样 —— 否则信息科拿 WPS 机器校验过的表，去 Excel 机器上看到不一样的
//! 结论，就得逐格对账，本产品就白做了。
//!
//! # 怎么验
//!
//! 1. **同一份样例，两种格式**：`mini.csv` 与 `mini.xlsx` 内容逐格相同
//!    （Rust 直接写 xlsx，不依赖 Python），文件主干同名 —— 行键因此也同名。
//! 2. **CSV 直跑**：引擎跑 CSV，产出契约 A。
//! 3. **表格入口**：引擎跑 xlsx，产出契约 B。Excel 宏与 WPS 宏都只是把
//!    引擎这份契约原样写回，所以契约 A 与 B 的**业务内容**必须逐字段相同。
//! 4. **WPS 侧真跑**：`tests/wps_harness/run.js` 是一个 WPS JSA 宿主替身，
//!    把真宏 `macro/wps/regval.js` 装进去点一次「校验」，然后把表模型、
//!    契约、状态条、按钮一起倒出来 —— 断言宏写回的 5 列与契约逐字段一致，
//!    原表已有列一格没动。
//! 5. **Excel 侧**：VBA 在 macOS 上跑不起来，所以用**同构核对**代替执行：
//!    两侧的常量（分块行数、状态条留白、表头行号）、读的契约键、按钮、
//!    版本闸门、错误码逐项比对 —— 判定口径只有一处，两侧同源。
//! 6. **版本闸门三入口同行为**：契约版本对不上时，三条入口都拒绝写回，
//!    都报同一个错误码与同一句中文。
//!
//! # 诚实边界
//!
//! · WPS 侧是**宿主替身 + 真引擎**：宏逻辑真跑了，引擎真跑了，但宿主不是真的
//!   WPS —— 真机上的 `Application.Run` / `FileSystem` 行为仍需院内首次导入时
//!   人工确认一次（README 与 macro/README.md 都写明了）。
//! · Excel 侧是**静态同构核对**，不是执行 —— 本轮环境没有 Excel。
//! · 契约里 `generated_at` / `batch_ref` / `elapsed_ms` / `source_file` 与
//!   入口与文件有关，**声明式**列出，允许不同；其余任何字段不同即失败。

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn regval_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_regval"))
}

fn macro_dir() -> PathBuf {
    project_dir().join("macro")
}

fn harness_script() -> PathBuf {
    project_dir().join("tests/wps_harness/run.js")
}

/// node 可执行文件；没有就跳过需要它的用例（不静默通过）
fn node_bin() -> Option<String> {
    for cand in ["node", "nodejs"] {
        if let Ok(out) = Command::new(cand).arg("--version").output() {
            if out.status.success() {
                return Some(cand.to_string());
            }
        }
    }
    None
}

// ======================================================================
// 样例表：一份内容，两种格式
// ======================================================================

/// 读样例 CSV（表头 + 数据行），逐格原样取文本
///
/// `has_headers(false)`：表头也是这张表的一行，要一起读出来（引擎那边
/// 自己认表头；这里要的是「这张表长什么样」的完整快照）。
fn read_sample_csv(path: &Path) -> Vec<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .expect("样例 CSV 应可读");
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record.expect("样例 CSV 应可逐行读");
        rows.push(record.iter().map(|s| s.to_string()).collect());
    }
    rows
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn col_letter(col: usize) -> String {
    let mut n = col;
    let mut out = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        out.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    out
}

/// 写一份最小 xlsx（单工作表，单元格一律写成内联字符串）。
///
/// **为什么单元格写成字符串而不是数字**：这一份 xlsx 的用途是「与 CSV 逐格
/// 相同的同一张表」。Excel 真会把「0301」这样的科室代码当数字吃掉前导零、
/// 把 98.0 显示成 98 —— 那是 Excel 的类型处理，不是入口一致性问题。
/// 这里统一写成文本，让两个入口看到的**格内文本**严格相等，把差异收窄到
/// 「哪条入口」这一个变量上。
fn write_minimal_xlsx(path: &Path, sheet_name: &str, rows: &[Vec<String>]) {
    const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;
    const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
    const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="{}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
        xml_escape(sheet_name)
    );

    let mut sheet_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for (r, row) in rows.iter().enumerate() {
        sheet_xml.push_str(&format!("<row r=\"{}\">", r + 1));
        for (c, value) in row.iter().enumerate() {
            sheet_xml.push_str(&format!(
                "<c r=\"{}{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                col_letter(c + 1),
                r + 1,
                xml_escape(value)
            ));
        }
        sheet_xml.push_str("</row>");
    }
    sheet_xml.push_str("</sheetData></worksheet>");

    let file = std::fs::File::create(path).expect("xlsx 应可创建");
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("_rels/.rels", ROOT_RELS),
        ("xl/workbook.xml", workbook.as_str()),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
        ("xl/worksheets/sheet1.xml", sheet_xml.as_str()),
    ] {
        zip.start_file(name, opts).expect("zip 条目应可写");
        zip.write_all(body.as_bytes()).expect("zip 内容应可写");
    }
    zip.finish().expect("xlsx 应可收尾");
}

/// 三入口共用的样例：一张表，两种格式，文件主干同名（行键因此同名）
struct Sample {
    dir: tempfile::TempDir,
    csv_path: PathBuf,
    xlsx_path: PathBuf,
    model_path: PathBuf,
    rows: Vec<Vec<String>>,
    sheet_name: String,
}

fn build_sample() -> Sample {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let rows = read_sample_csv(&project_dir().join("tests/fixtures/mini.csv"));
    let sheet_name = "mini".to_string();

    let csv_path = dir.path().join("mini.csv");
    let xlsx_path = dir.path().join("mini.xlsx");
    let model_path = dir.path().join("model.json");

    // CSV：原样抄一份（表头与数据行逐格相同）
    let mut csv_text = String::new();
    for row in &rows {
        csv_text.push_str(
            &row
                .iter()
                .map(|v| {
                    if v.contains(',') || v.contains('"') {
                        format!("\"{}\"", v.replace('"', "\"\""))
                    } else {
                        v.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(","),
        );
        csv_text.push('\n');
    }
    std::fs::write(&csv_path, csv_text).expect("样例 CSV 应可写");

    // xlsx：同一张表
    write_minimal_xlsx(&xlsx_path, &sheet_name, &rows);

    // 宿主替身用的表模型（与 xlsx 逐格相同）
    let model = json!({
        "full_name": xlsx_path.display().to_string(),
        "path": dir.path().display().to_string(),
        "sheets": [{ "name": sheet_name, "rows": rows }],
    });
    std::fs::write(&model_path, serde_json::to_string_pretty(&model).unwrap())
        .expect("表模型应可写");

    Sample {
        dir,
        csv_path,
        xlsx_path,
        model_path,
        rows,
        sheet_name,
    }
}

// ======================================================================
// 跑引擎
// ======================================================================

struct RunOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_validate(input: &Path, out: &Path, state_db: &Path, extra: &[&str]) -> RunOutput {
    let mut cmd = Command::new(regval_bin());
    cmd.arg("validate")
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db);
    for e in extra {
        cmd.arg(e);
    }
    let out = cmd.output().expect("引擎应可执行");
    RunOutput {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("产物应可读");
    serde_json::from_str(&text).expect("产物应是合法 JSON")
}

// ======================================================================
// 契约比对
// ======================================================================

/// 插件真正消费的那几段：三条入口产出这一份就必须逐字段相同。
///
/// 契约里还有别的段（规则清单、模型状态、科室目录状态），它们由引擎算，
/// 三条入口共用同一个引擎 —— 那些段在整份契约的逐字段比对里一起被盯住。
fn plugin_view(c: &Value) -> Value {
    let rows: Vec<Value> = c["rows"]
        .as_array()
        .expect("rows 应是数组")
        .iter()
        .map(|r| {
            json!({
                "source_ns": r["source_ns"],
                "row_no": r["row_no"],
                "indicator_code": r["indicator_code"],
                "dept_code": r["dept_code"],
                "reporting_period": r["reporting_period"],
                "business_key": r["business_key"],
                "conclusion": r["conclusion"],
                "issue_types": r["issue_types"],
                "issue_detail": r["issue_detail"],
                "suspected_duplicate": r["suspected_duplicate"],
                "submission_status": r["submission_status"],
            })
        })
        .collect();
    json!({
        "schema_version": c["schema_version"],
        "columns": c["columns"],
        "summary": {
            "tables": c["summary"]["tables"],
            "total_rows": c["summary"]["total_rows"],
            "rows_with_issues": c["summary"]["rows_with_issues"],
            "issue_count": c["summary"]["issue_count"],
            "issue_count_by_kind": c["summary"]["issue_count_by_kind"],
            "suspected_duplicate_count": c["summary"]["suspected_duplicate_count"],
            "dept_master_checked": c["summary"]["dept_master_checked"],
            "dept_master_note": c["summary"]["dept_master_note"],
            "status_line": c["summary"]["status_line"],
            "submission_line": c["summary"]["submission_line"],
            "submitted_rows": c["summary"]["submitted_rows"],
            // 读表阶段的计数：两条入口读的是同一张表，跳过的行数必须一样
            "source_rows": c["summary"]["source_rows"],
            "blank_rows": c["summary"]["blank_rows"],
            "invalid_rows": c["summary"]["invalid_rows"],
            "skipped_rows": c["summary"]["skipped_rows"],
            "skipped_note": c["summary"]["skipped_note"],
        },
        "rows": rows,
        "issues": c["issues"],
        "suspected_duplicates": c["suspected_duplicates"],
    })
}

/// 两份 JSON 的差异叶子路径（形如 `summary.elapsed_ms`、`rows[2].source_file`）
fn diff_paths(a: &Value, b: &Value, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in std::mem::take(&mut keys) {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{}.{}", prefix, k)
                };
                match (x.get(k), y.get(k)) {
                    (Some(va), Some(vb)) => {
                        out.extend(diff_paths(va, vb, &path));
                    }
                    _ => {
                        out.insert(path);
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.insert(format!("{}.length", prefix));
            } else {
                for (i, (va, vb)) in x.iter().zip(y.iter()).enumerate() {
                    out.extend(diff_paths(va, vb, &format!("{}[{}]", prefix, i)));
                }
            }
        }
        _ => {
            if a != b {
                out.insert(prefix.to_string());
            }
        }
    }
    out
}

/// 允许因入口/文件而不同的字段（声明式，不多不少）
///
/// `[*]` 是通配下标：`rows[*].source_file` 表示每一行的来源文件字段。
fn allowed_entry_specific_paths() -> BTreeSet<String> {
    [
        // 契约生成时刻
        "generated_at",
        // 批次号 = 文件内容摘要 + 文件主干：两个入口喂的是两份不同文件
        "batch_ref",
        // 本轮耗时
        "summary.elapsed_ms",
        // 行来自哪个文件（CSV 入口与表格入口本就不同）
        "rows[*].source_file",
        // 这张表是什么编码 / 什么分隔符 —— 格式本身的属性，
        // CSV 入口与表格入口天然不同（CSV 是 utf-8 + 半角逗号，xlsx 是工作簿原生）。
        // 行数、跳过的行数这些**内容**属性仍然逐字段相同（见 plugin_view）。
        "ingest.encoding",
        "ingest.delimiter",
        "ingest.format_note",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 差异路径是否落在允许清单里（`[*]` 匹配任意下标）
fn path_is_allowed(path: &str, allowed: &BTreeSet<String>) -> bool {
    let normalized: String = path
        .split('.')
        .map(|seg| {
            if let Some(open) = seg.find('[') {
                if seg.ends_with(']') {
                    return format!("{}[*]", &seg[..open]);
                }
            }
            seg.to_string()
        })
        .collect::<Vec<_>>()
        .join(".");
    allowed.contains(&normalized)
}

/// 把所有数字统一成 f64，再比较。
///
/// 为什么：契约里的列宽是 `f64`，Rust 侧序列化成 `10.0`；宏侧（JS）把
/// 同一个数读成 `10` 再吐出来。两边都是**同一个数**，只是写法不同 ——
/// 拿字符串比会误判成不一致。统一成 f64 之后比的是数值本身。
fn canonical_numbers(v: &Value) -> Value {
    match v {
        Value::Number(n) => match n.as_f64() {
            Some(f) => Value::from(f),
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(a.iter().map(canonical_numbers).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), canonical_numbers(v)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

// ======================================================================
// 用例
// ======================================================================

#[test]
fn 三条入口产出同一份契约_业务内容逐字段相同() {
    let sample = build_sample();
    let state_db = sample.dir.path().join("state.db");
    let csv_contract = sample.dir.path().join("contract_csv.json");
    let book_contract = sample.dir.path().join("contract_book.json");

    let r1 = run_validate(&sample.csv_path, &csv_contract, &state_db, &[]);
    assert_eq!(r1.status, 0, "CSV 入口应成功：{}", r1.stderr);
    let r2 = run_validate(&sample.xlsx_path, &book_contract, &state_db, &[]);
    assert_eq!(r2.status, 0, "表格入口应成功：{}", r2.stderr);

    let a = read_json(&csv_contract);
    let b = read_json(&book_contract);

    // 插件读的那几段：逐字段相同
    assert_eq!(
        plugin_view(&a),
        plugin_view(&b),
        "两条入口的插件读数必须逐字段相同"
    );

    // 终端里说的那句话与表里写的那句话是同一句（状态条句子的唯一出处）
    let status_line = a["summary"]["status_line"].as_str().unwrap();
    assert!(
        r1.stdout.contains(status_line),
        "CSV 入口终端首行应含状态条句子：{}",
        r1.stdout
    );
    assert!(
        r2.stdout.contains(status_line),
        "表格入口终端首行应含同一句状态条：{}",
        r2.stdout
    );

    // 来源命名空间：CSV 取文件主干、Excel 取工作表名 —— 两者都该是同一个
    assert_eq!(a["rows"][0]["source_ns"], sample.sheet_name);
    assert_eq!(b["rows"][0]["source_ns"], sample.sheet_name);

    // 整份契约：只允许声明过的那几处不同
    let diffs = diff_paths(&a, &b, "");
    let allowed = allowed_entry_specific_paths();
    let unexpected: Vec<&String> = diffs
        .iter()
        .filter(|d| !path_is_allowed(d, &allowed))
        .collect();
    assert!(
        unexpected.is_empty(),
        "两条入口的契约出现了未声明的差异：{:?}",
        unexpected
    );
    // 比对不是空的：声明允许的那几处确实不同（否则这条断言是空转）
    assert!(
        diffs.contains("batch_ref"),
        "批次号应随入口而不同（文件内容不同）：{:?}",
        diffs
    );
    assert!(
        diffs
            .iter()
            .any(|d| d.starts_with("rows[") && d.ends_with(".source_file")),
        "行的来源文件应随入口而不同：{:?}",
        diffs
    );
}

#[test]
fn wps侧写回与契约逐字段一致_原表一格没动() {
    let Some(node) = node_bin() else {
        eprintln!("跳过：环境里没有 node，跑不了 WPS 宿主替身");
        return;
    };
    let sample = build_sample();
    let state_db = sample.dir.path().join("state.db");
    let book_contract = sample.dir.path().join("contract_book.json");
    let r = run_validate(&sample.xlsx_path, &book_contract, &state_db, &[]);
    assert_eq!(r.status, 0, "表格入口应成功：{}", r.stderr);
    let contract = read_json(&book_contract);

    let dump_path = sample.dir.path().join("wps_dump.json");
    let out = Command::new(&node)
        .arg(harness_script())
        .arg("--model")
        .arg(&sample.model_path)
        .arg("--macro")
        .arg(macro_dir().join("wps/regval.js"))
        .arg("--dump")
        .arg(&dump_path)
        .arg("--engine")
        .arg(regval_bin())
        .arg("--state-db")
        .arg(&state_db)
        .output()
        .expect("宿主替身应可执行");
    assert!(
        out.status.success(),
        "宿主替身应跑通：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dump = read_json(&dump_path);

    assert_eq!(dump["threw"], Value::Null, "宏不该抛异常");
    assert_eq!(
        dump["error"]["has_error"], false,
        "宏不该报错：{:?}",
        dump["error"]
    );

    // 宏读到的契约就是引擎产出的那份（宏不改契约，只写回）。
    // 宏自己调了一次引擎，所以生成时刻与耗时与本测试先前那次不同 ——
    // 业务内容必须逐字段相同。
    assert_eq!(
        canonical_numbers(&plugin_view(&dump["contract"])),
        canonical_numbers(&plugin_view(&contract)),
        "宏消费的契约应与引擎产出的契约业务内容逐字段相同"
    );
    assert_eq!(
        dump["contract"]["schema_version"], contract["schema_version"],
        "契约版本应一致"
    );
    assert_eq!(
        dump["contract"]["batch_ref"], contract["batch_ref"],
        "批次号应一致（同一份输入）"
    );

    // 写回：5 个新列逐格等于契约里的行结果
    let written: &Vec<Value> = dump["sheets"][0]["rows"].as_array().unwrap();
    let rows = contract["rows"].as_array().unwrap();
    let original_cols = sample.rows[0].len();
    assert_eq!(
        written.len(),
        sample.rows.len() + 2,
        "写回后应多出留白行 + 状态条行（状态条在数据下面空 2 行）"
    );
    for (i, row) in rows.iter().enumerate() {
        let got = written[i + 1].as_array().unwrap();
        assert_eq!(&got[original_cols], &row["conclusion"], "第 {} 行校验结论", i + 1);
        assert_eq!(&got[original_cols + 1], &row["issue_types"], "第 {} 行问题类型", i + 1);
        assert_eq!(&got[original_cols + 2], &row["issue_detail"], "第 {} 行问题说明", i + 1);
        assert_eq!(
            &got[original_cols + 3], &row["suspected_duplicate"], "第 {} 行疑似重复项", i + 1
        );
        assert_eq!(
            &got[original_cols + 4], &row["submission_status"], "第 {} 行上报状态", i + 1
        );
    }

    // 表头写回的是 5 个新列名
    let expected_headers: Vec<String> = contract["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    let got_headers: Vec<String> = written[0].as_array().unwrap()[original_cols..]
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(got_headers, expected_headers, "新列列头应与契约一致");

    // 原表已有列一格没动
    for (i, row) in sample.rows.iter().enumerate() {
        let got = written[i].as_array().unwrap();
        for (j, cell) in row.iter().enumerate() {
            assert_eq!(
                &got[j], cell,
                "原表第 {} 行第 {} 列被动了",
                i + 1,
                j + 1
            );
        }
    }

    // 状态条：引擎算好的那句话，原样落在表里
    let status_line = contract["summary"]["status_line"].as_str().unwrap();
    let status_cell = written[sample.rows.len() + 1].as_array().unwrap()[original_cols]
        .as_str()
        .unwrap();
    assert_eq!(status_cell, status_line, "状态条句子应与契约 summary 一致");
    assert_eq!(
        dump["window_hint"], status_line,
        "窗口状态栏也应报同一句"
    );

    // 两个按钮：校验 / 上报
    let buttons = dump["buttons"][0].as_array().unwrap();
    let captions: Vec<&str> = buttons
        .iter()
        .map(|b| b["caption"].as_str().unwrap())
        .collect();
    assert_eq!(captions, vec!["校验", "上报"], "WPS 侧应有两个按钮");
    let actions: Vec<&str> = buttons
        .iter()
        .map(|b| b["on_action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, vec!["RegVal_Validate", "RegVal_Push"]);
}

#[test]
fn 契约版本不匹配时三条入口都拒绝写回() {
    let Some(node) = node_bin() else {
        eprintln!("跳过：环境里没有 node，跑不了 WPS 宿主替身");
        return;
    };
    let sample = build_sample();
    let state_db = sample.dir.path().join("state.db");

    // ---- 入口 1：CSV 直跑（引擎侧闸门）----
    let out_path = sample.dir.path().join("contract_mismatch.json");
    let err_path = sample.dir.path().join("error_mismatch.json");
    let r = run_validate(
        &sample.csv_path,
        &out_path,
        &state_db,
        &[
            "--expect-schema-version",
            "regval.contract/9.9.9",
            "--error-out",
            err_path.to_str().unwrap(),
        ],
    );
    assert_ne!(r.status, 0, "版本不符应非 0 退出");
    assert!(!out_path.exists(), "版本不符不该产出契约文件");
    let report = read_json(&err_path);
    assert_eq!(report["code"], "CONTRACT_VERSION_MISMATCH");
    assert_eq!(
        report["code_zh"], "引擎与宏版本不一致，请升级",
        "错误码的中文说明要与宏侧同一句"
    );

    // ---- 入口 2：WPS JSA（宏侧闸门）----
    // 先让引擎产出一份正常契约，再把它改掉版本，喂给宏
    let good = sample.dir.path().join("contract_good.json");
    let r2 = run_validate(&sample.xlsx_path, &good, &state_db, &[]);
    assert_eq!(r2.status, 0, "先跑一份正常契约：{}", r2.stderr);
    let mut doctored = read_json(&good);
    doctored["schema_version"] = json!("regval.contract/9.9.9");
    let doctored_path = sample.dir.path().join("contract_doctored.json");
    std::fs::write(
        &doctored_path,
        serde_json::to_string_pretty(&doctored).unwrap(),
    )
    .expect("改版契约应可写");

    let dump_path = sample.dir.path().join("wps_dump_mismatch.json");
    let out = Command::new(&node)
        .arg(harness_script())
        .arg("--model")
        .arg(&sample.model_path)
        .arg("--macro")
        .arg(macro_dir().join("wps/regval.js"))
        .arg("--dump")
        .arg(&dump_path)
        .arg("--engine")
        .arg(regval_bin())
        .arg("--state-db")
        .arg(&state_db)
        .arg("--inject-contract")
        .arg(&doctored_path)
        .output()
        .expect("宿主替身应可执行");
    assert!(out.status.success());
    let dump = read_json(&dump_path);
    assert_eq!(
        dump["error"]["code"], "CONTRACT_VERSION_MISMATCH",
        "WPS 侧版本闸门应报错"
    );
    assert_eq!(dump["error"]["code_zh"], "引擎与宏版本不一致，请升级");

    // 一格都不许写回：表里不该出现 5 个新列
    let written: &Vec<Value> = dump["sheets"][0]["rows"].as_array().unwrap();
    let original_cols = sample.rows[0].len();
    for (i, row) in sample.rows.iter().enumerate() {
        let got = written[i].as_array().unwrap();
        for (j, cell) in row.iter().enumerate() {
            assert_eq!(&got[j], cell, "原表第 {} 行第 {} 列被动了", i + 1, j + 1);
        }
    }
    let header_extra = written[0].as_array().unwrap().len().saturating_sub(original_cols);
    assert_eq!(header_extra, 0, "版本不符时不该写新列头");
    // 数据行也没有被追加：原表 7 行（表头 + 6 行），一行不多
    assert_eq!(
        written.len(),
        sample.rows.len() + 2,
        "版本不符时只该多出状态条那一行（空行 + 错误行）"
    );
    // 状态条那一行写的是错误，不是校验结论
    let error_line = written[sample.rows.len() + 1]
        .as_array()
        .unwrap()
        .get(original_cols)
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        error_line.contains("CONTRACT_VERSION_MISMATCH"),
        "状态条应报同一个错误码，实际：{}",
        error_line
    );
    assert!(
        error_line.contains("引擎与宏版本不一致，请升级"),
        "状态条应报同一句中文，实际：{}",
        error_line
    );

    // ---- 入口 3：Excel 宏（源码侧闸门）----
    // VBA 在本轮环境跑不了，用同源核对代替执行：宏引的是同一个错误码，
    // 中文说明取自同一张文案表（src/ui/messages.rs 生成的常量模块）。
    let vba = std::fs::read_to_string(macro_dir().join("vba/RegVal.bas"))
        .expect("Excel 宏应可读");
    assert!(
        vba.contains("CONTRACT_VERSION_MISMATCH"),
        "Excel 宏应认得同一个错误码"
    );
    assert!(
        vba.contains("RegValErrorText(\"CONTRACT_VERSION_MISMATCH\")"),
        "Excel 宏的中文说明应取自同一张文案表"
    );
    let messages = std::fs::read_to_string(macro_dir().join("vba/RegValMessages.bas"))
        .expect("文案常量模块应可读");
    assert!(
        messages.contains("引擎与宏版本不一致，请升级"),
        "文案表里应有这句中文"
    );
}

#[test]
fn excel侧与wps侧同构_常量与判定口径逐项一致() {
    let vba = std::fs::read_to_string(macro_dir().join("vba/RegVal.bas")).expect("Excel 宏应可读");
    let js = std::fs::read_to_string(macro_dir().join("wps/regval.js")).expect("WPS 宏应可读");

    // 部署与写回常量：两侧逐项一致
    for (label, vba_pat, js_pat) in [
        ("分块写回行数", "REGVAL_CHUNK_ROWS As Long = 200", "REGVAL_CHUNK_ROWS = 200"),
        ("状态条留白行数", "REGVAL_STATUS_ROW_GAP As Long = 2", "REGVAL_STATUS_ROW_GAP = 2"),
        ("按钮宽", "REGVAL_BUTTON_W As Double = 88", "REGVAL_BUTTON_W = 88"),
        ("按钮高", "REGVAL_BUTTON_H As Double = 26", "REGVAL_BUTTON_H = 26"),
        ("按钮间距", "REGVAL_BUTTON_GAP As Double = 8", "REGVAL_BUTTON_GAP = 8"),
    ] {
        assert!(vba.contains(vba_pat), "Excel 侧少了常量：{}", label);
        assert!(js.contains(js_pat), "WPS 侧少了常量：{}", label);
    }

    // 契约常量：版本号与 5 个新列名两侧一致
    let version = "regval.contract/1.0.0";
    let columns = "校验结论|问题类型|问题说明（含依据）|疑似重复项|上报状态";
    assert!(vba.contains(version), "Excel 侧应写明契约版本");
    assert!(js.contains(version), "WPS 侧应写明契约版本");
    assert!(vba.contains(columns), "Excel 侧应写明 5 个新列名");
    assert!(js.contains(columns), "WPS 侧应写明 5 个新列名");

    // 宏读的契约键：两侧逐个对得上（少一个键 = 那一列写不回去）
    for key in [
        "schema_version",
        "columns",
        "rows",
        "summary",
        "status_line",
        "submission_line",
        "batch_ref",
        "source_ns",
        "conclusion",
        "issue_types",
        "issue_detail",
        "suspected_duplicate",
        "submission_status",
    ] {
        assert!(
            vba.contains(&format!("\"{}\"", key)),
            "Excel 侧没读契约键 {}",
            key
        );
        assert!(
            js.contains(&format!("\"{}\"", key)) || js.contains(&format!(".{}", key)),
            "WPS 侧没读契约键 {}",
            key
        );
    }

    // 两个按钮：同一批动作
    for action in ["RegVal_Validate", "RegVal_Push"] {
        assert!(vba.contains(action), "Excel 侧应有个 {}", action);
        assert!(js.contains(action), "WPS 侧应有个 {}", action);
    }

    // 分块写回的块边界：两侧必须同一种写法。
    //
    // chunkEnd 是这一块的**最后一行**（闭区间）。写成 `<= gEnd` 就会多读一行
    // —— 块尾正好在表尾时读到表外，小表直接越界。WPS 侧有真跑的用例盯着
    // （越界会抛异常），VBA 侧跑不了，所以两侧的写法一起盯。
    assert!(
        vba.contains("chunkEnd < gEnd"),
        "Excel 侧分块边界应取闭区间（chunkEnd < gEnd）"
    );
    assert!(
        js.contains("chunkEnd < end"),
        "WPS 侧分块边界应取闭区间（chunkEnd < end）"
    );
    assert!(
        !vba.contains("chunkEnd <= gEnd"),
        "Excel 侧分块边界不能写成开区间（会读到表外）"
    );
    assert!(
        !js.contains("chunkEnd <= end"),
        "WPS 侧分块边界不能写成开区间（会读到表外）"
    );

    // 状态条句子取自契约 summary，宏不自行统计
    assert!(
        vba.contains("status_line") && js.contains("status_line"),
        "两侧都应从契约 summary 取状态条句子"
    );
    for side in [("Excel", &vba), ("WPS", &js)] {
        assert!(
            !side.1.contains("Rows.Count -"),
            "{} 侧不该自行统计行数",
            side.0
        );
    }

    // 错误码：两侧认得同一批
    for code in [
        "CONTRACT_VERSION_MISMATCH",
        "CONTRACT_NOT_PRODUCED",
        "ENGINE_NOT_FOUND",
    ] {
        assert!(vba.contains(code), "Excel 侧少了错误码 {}", code);
        assert!(js.contains(code), "WPS 侧少了错误码 {}", code);
    }
}

#[test]
fn 提交的wps宏与模板装配结果一致_生成物不漂() {
    let tmp = tempfile::tempdir().expect("临时目录应可建");
    let out = tmp.path().join("regval.js");
    let r = Command::new(regval_bin())
        .arg("macro")
        .arg("--out")
        .arg(&out)
        .arg("--template")
        .arg(macro_dir().join("wps/regval.template.js"))
        .output()
        .expect("装配命令应可跑");
    assert!(
        r.status.success(),
        "装配应成功：{}",
        String::from_utf8_lossy(&r.stderr)
    );
    let fresh = std::fs::read_to_string(&out).expect("装配产物应可读");
    let committed = std::fs::read_to_string(macro_dir().join("wps/regval.js"))
        .expect("提交的宏应可读");
    assert_eq!(
        fresh, committed,
        "提交的 WPS 宏与模板装配结果不一致（模板改了没重新生成）"
    );
}
