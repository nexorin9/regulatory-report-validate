//! CSV 直跑降级路径 —— 专网报送机没有宏环境时的等价用法。
//!
//! # 这条测试在证明什么
//!
//! 产品的第三个入口不是宏，是**一条命令**：企业组策略把 VBA/JSA 禁了、
//! 或者报送机上根本没装 Excel，科室还是得把表校验一遍。CSV 直跑就是这条路。
//!
//! 它要成立，得同时满足三件事：
//!
//! 1. **同一份判定**：CSV 走的是同一套表头映射、同一套口径规则、同一份契约 ——
//!    不是「另写一份简化版逻辑」。同一张表从 CSV 进去和从 Excel 进去，
//!    结论、问题数、状态条那句话都得一样。
//! 2. **脏数据不丢人**：专网报送机上的表多半不是干净的 UTF-8 半角逗号 CSV。
//!    GBK 编码、UTF-8/GBK 混编、列名带空格、全角逗号分隔、空行、重复表头 ——
//!    这些要么被归一，要么**给出明确错误码**，不许静默丢行、静默丢字。
//! 3. **不静默丢行**：空行与完全无效行不参与校验（它们不是数据行），
//!    但**必须计数、必须留痕**：行号与原因进契约，经办人看得见「表里 8 行、
//!    校验了 5 行，另外 3 行是什么、为什么不算」。
//!
//! # 与三入口一致性测试的关系
//!
//! `tests/three_entry_consistency_test.rs` 盯的是「CSV 与表格入口产出同一份契约」；
//! 这一份盯的是「CSV 入口自己站得住」—— 脏数据、丢行计数、错误码、
//! 以及 CSV 与 xlsx 两路读入后逐字段一致。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn regval_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_regval"))
}

struct RunOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

/// 跑一次 `regval validate`（只算不发）
fn run_validate(input: &Path, out: &Path, state_db: &Path) -> RunOutput {
    let o = Command::new(regval_bin())
        .arg("validate")
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .output()
        .expect("引擎应可执行");
    RunOutput {
        status: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("产物应可读");
    serde_json::from_str(&text).expect("产物应是合法 JSON")
}

/// 插件与经办人真正消费的那几段（不含「跟文件/入口有关」的字段）
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
            "source_rows": c["summary"]["source_rows"],
            "blank_rows": c["summary"]["blank_rows"],
            "invalid_rows": c["summary"]["invalid_rows"],
            "skipped_rows": c["summary"]["skipped_rows"],
            "skipped_note": c["summary"]["skipped_note"],
        },
        "rows": rows,
        "issues": c["issues"],
        "suspected_duplicates": c["suspected_duplicates"],
        // 读表记录里与「内容」有关的部分（编码与分隔符是格式属性，与入口有关）
        "ingest": {
            "source_rows": c["ingest"]["source_rows"],
            "valid_rows": c["ingest"]["valid_rows"],
            "blank_rows": c["ingest"]["blank_rows"],
            "invalid_rows": c["ingest"]["invalid_rows"],
            "skipped_rows": c["ingest"]["skipped_rows"],
            "skipped_detail": c["ingest"]["skipped_detail"],
        },
    })
}

// ======================================================================
// 写 xlsx 的小工具（同一张表、两种格式）
// ======================================================================

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

/// 写一份最小 xlsx（单工作表，单元格一律写成内联字符串）
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

    use std::io::Write;
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

// ======================================================================
// 用例
// ======================================================================

#[test]
fn csv直跑产出与宏路径同一份契约() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(
        &project_dir().join("tests/fixtures/mini.csv"),
        &out,
        &state_db,
    );
    assert_eq!(r.status, 0, "CSV 直跑应成功：{}", r.stderr);
    let c = read_json(&out);

    // 契约版本与 5 个新列：与宏路径同一份（宏只是把这份契约原样写回）
    assert_eq!(c["schema_version"], "regval.contract/1.0.0");
    let cols: Vec<&str> = c["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        cols,
        vec!["校验结论", "问题类型", "问题说明（含依据）", "疑似重复项", "上报状态"]
    );
    // 行数与状态条句子：与宏路径同一套算法算出来的
    assert_eq!(c["rows"].as_array().unwrap().len(), 6);
    let status_line = c["summary"]["status_line"].as_str().unwrap();
    assert_eq!(status_line, "已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认");
    // 终端里说的与表里写的是同一句
    assert!(
        r.stdout.contains(status_line),
        "终端首行应含状态条句子：{}",
        r.stdout
    );
    // 这一批没有跳过行 → 计数为 0，note 为空（不是「跳过 0 行」这种废话）
    assert_eq!(c["summary"]["source_rows"], 6);
    assert_eq!(c["summary"]["skipped_rows"], 0);
    assert_eq!(c["summary"]["skipped_note"], "");
}

#[test]
fn 脏数据样例跑出丢行计数_不静默丢行() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(
        &project_dir().join("tests/fixtures/csv_dirty.csv"),
        &out,
        &state_db,
    );
    assert_eq!(r.status, 0, "脏数据样例应能跑完：{}", r.stderr);
    let c = read_json(&out);

    // 样例表：8 行数据 = 5 行有效 + 1 空行 + 1 重复表头 + 1 分隔线
    assert_eq!(c["summary"]["source_rows"], 8, "读到的数据行总数");
    assert_eq!(c["summary"]["total_rows"], 5, "参与校验的有效行数");
    assert_eq!(c["summary"]["blank_rows"], 1, "空行数");
    assert_eq!(c["summary"]["invalid_rows"], 2, "完全无效行数");
    assert_eq!(c["summary"]["skipped_rows"], 3, "被跳过的行数");
    // 关系自洽：有效 + 跳过 = 读到的总数（一行都没少，也没多）
    assert_eq!(
        c["summary"]["total_rows"].as_u64().unwrap() + c["summary"]["skipped_rows"].as_u64().unwrap(),
        c["summary"]["source_rows"].as_u64().unwrap()
    );
    // 说明句带具体数字，不写「跳过了一些行」
    let note = c["summary"]["skipped_note"].as_str().unwrap();
    assert!(note.contains("空行 1 行"), "说明句应点名空行数：{}", note);
    assert!(note.contains("无效行 2 行"), "说明句应点名无效行数：{}", note);
    assert!(note.contains("未参与校验"), "说明句应讲清这些行没参与校验：{}", note);
    // 终端也照实打这一行（经办人在命令行里就能看见少了哪几行）
    assert!(
        r.stdout.contains(note),
        "终端应打出跳过行的说明：{}",
        r.stdout
    );

    // 跳过的行留痕：行号 + 原因，一个不落
    let detail = c["ingest"]["skipped_detail"].as_array().unwrap();
    assert_eq!(detail.len(), 3, "三条跳过记录");
    let reasons: Vec<&str> = detail
        .iter()
        .map(|d| d["reason"].as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"空行"), "应有空行：{:?}", reasons);
    assert!(reasons.contains(&"重复表头"), "应有重复表头：{:?}", reasons);
    assert!(reasons.contains(&"分隔线"), "应有分隔线：{:?}", reasons);
    // 行号能照着回原表找：第 5 行是空行，第 6 行是重复表头，第 7 行是分隔线
    let row_nos: Vec<u64> = detail
        .iter()
        .map(|d| d["row_no"].as_u64().unwrap())
        .collect();
    assert_eq!(row_nos, vec![5, 6, 7], "跳过的行号应指原表里的位置");
    for d in detail {
        assert_eq!(d["source_ns"], "csv_dirty", "留痕要指明是哪张表");
    }

    // 跳过的行不参与校验：没有为它们产生任何口径问题
    let rows = c["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    for r in rows {
        assert!(
            r["conclusion"] != "不通过" || !r["issue_types"].as_str().unwrap().is_empty(),
            "有效行的结论与问题类型要配套"
        );
        assert!(
            !r["business_key"].as_str().unwrap_or("").is_empty(),
            "被跳过的行不该混进结果：{:?}",
            r["business_key"]
        );
    }
}

#[test]
fn gbk编码的表能读出中文_编码记进契约() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("gbk.csv");
    let (text, _, _) = encoding_rs::GBK.encode(
        "指标编号,指标名称,科室代码,报告期,业务主键\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-GBK-01\n",
    );
    std::fs::write(&path, text.as_ref()).expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_eq!(r.status, 0, "GBK 表应能跑完：{}", r.stderr);
    let c = read_json(&out);

    assert_eq!(c["ingest"]["encoding"], "gbk", "契约里应记下这张表是 GBK");
    let rows = c["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["indicator_code"], "MER-TL-04");
    assert_eq!(rows[0]["business_key"], "BK-GBK-01");
    // 中文没变成乱码：整份契约里不该出现替换字符（U+FFFD）
    assert!(
        !c.to_string().contains('\u{FFFD}'),
        "GBK 表读出来不该有替换字符（乱码）"
    );
    // 终端里点明做过格式归一
    assert!(
        r.stdout.contains("编码 gbk"),
        "终端应说明这张表是 GBK：{}",
        r.stdout
    );
}

#[test]
fn utf8与gbk混编逐行读对() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("mixed.csv");
    let (gbk_line, _, _) = encoding_rs::GBK.encode(
        "MER-TQ-01,出院患者病历2日归档率,0301,2026-Q3,BK-MIX-02,病案统计室",
    );
    let mut bytes = "指标编号,指标名称,科室代码,报告期,业务主键,科室名称\n"
        .as_bytes()
        .to_vec();
    bytes.extend_from_slice(
        "MER-TL-04,病案首页24小时内完成率,0302,2026-Q3,BK-MIX-01,急诊科\n".as_bytes(),
    );
    bytes.extend_from_slice(gbk_line.as_ref());
    bytes.extend_from_slice(b"\n");
    std::fs::write(&path, &bytes).expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_eq!(r.status, 0, "混编表应能跑完：{}", r.stderr);
    let c = read_json(&out);

    assert_eq!(c["ingest"]["encoding"], "mixed", "契约里应记下这张表是混编");
    let rows = c["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "一行都不能少");
    assert_eq!(rows[0]["indicator_code"], "MER-TL-04");
    assert_eq!(rows[1]["indicator_code"], "MER-TQ-01");
    // 两行的中文都要读对（UTF-8 行与 GBK 行各一行）
    assert_eq!(rows[0]["dept_code"], "0302");
    assert_eq!(rows[1]["dept_code"], "0301");
    assert_eq!(rows[1]["business_key"], "BK-MIX-02");
}

#[test]
fn 列名带空格与全角空格能映射() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("spaced.csv");
    std::fs::write(
        &path,
        "指 标 编 号,指标名称\u{3000},科室代码,报告期,业务主键\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-SPACE-01\n",
    )
    .expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_eq!(r.status, 0, "列名带空格应能跑完：{}", r.stderr);
    let c = read_json(&out);
    assert_eq!(c["rows"].as_array().unwrap().len(), 1, "不该因为列名空格丢行");
    assert_eq!(c["rows"][0]["indicator_code"], "MER-TL-04");
    assert_eq!(c["rows"][0]["dept_code"], "0301");
}

#[test]
fn 全角逗号分隔能读_并记下做过归一() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("fullwidth.csv");
    std::fs::write(
        &path,
        "指标编号，指标名称，科室代码，报告期，业务主键\n\
         MER-TL-04，病案首页24小时内完成率，0301，2026-Q3，BK-FW-01\n",
    )
    .expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_eq!(r.status, 0, "全角逗号表应能跑完：{}", r.stderr);
    let c = read_json(&out);
    assert_eq!(c["ingest"]["delimiter"], "全角逗号（已归一为半角）");
    let rows = c["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["business_key"], "BK-FW-01");
    assert_eq!(rows[0]["dept_code"], "0301");
}

#[test]
fn 解不出来的行报明确错误码并点名行号() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("broken.csv");
    let mut bytes = "指标编号,科室代码\n".as_bytes().to_vec();
    bytes.extend_from_slice(&[0x81, 0x20, 0x81]); // 孤立的 0x81：GBK 也解不了
    bytes.extend_from_slice(b"\n");
    std::fs::write(&path, &bytes).expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_ne!(r.status, 0, "解不出来应非 0 退出");
    assert!(
        r.stderr.contains("INPUT_UNREADABLE"),
        "应报明确的错误码：{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("第 2 行"),
        "应点名是第几行：{}",
        r.stderr
    );
    assert!(!out.exists(), "失败时不该产出契约文件");
}

#[test]
fn utf16文件明确说不支持() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    let path = dir.path().join("utf16.csv");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend_from_slice("指标编号".as_bytes());
    std::fs::write(&path, &bytes).expect("应可写");

    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let r = run_validate(&path, &out, &state_db);
    assert_ne!(r.status, 0, "UTF-16 应非 0 退出");
    assert!(r.stderr.contains("INPUT_UNREADABLE"), "{}", r.stderr);
    assert!(
        r.stderr.contains("UTF-16"),
        "应说清是 UTF-16 的问题：{}",
        r.stderr
    );
    assert!(!out.exists(), "失败时不该产出契约文件");
}

#[test]
fn 同一份数据经csv与xlsx两路读入_契约业务内容一致() {
    let dir = tempfile::tempdir().expect("临时目录应可建");
    // 同一张表（含空行与重复表头），写成两种格式
    let src = project_dir().join("tests/fixtures/csv_dirty.csv");
    let text = std::fs::read_to_string(&src).expect("样例应可读");
    let rows: Vec<Vec<String>> = text
        .lines()
        .map(|l| l.split(',').map(|s| s.to_string()).collect())
        .collect();

    let csv_path = dir.path().join("csv_dirty.csv");
    std::fs::write(&csv_path, &text).expect("CSV 应可写");
    let xlsx_path = dir.path().join("csv_dirty.xlsx");
    write_minimal_xlsx(&xlsx_path, "csv_dirty", &rows);

    let state_db = dir.path().join("state.db");
    let csv_out = dir.path().join("contract_csv.json");
    let book_out = dir.path().join("contract_book.json");
    let r1 = run_validate(&csv_path, &csv_out, &state_db);
    assert_eq!(r1.status, 0, "CSV 入口应成功：{}", r1.stderr);
    let r2 = run_validate(&xlsx_path, &book_out, &state_db);
    assert_eq!(r2.status, 0, "表格入口应成功：{}", r2.stderr);

    let a = read_json(&csv_out);
    let b = read_json(&book_out);
    assert_eq!(
        plugin_view(&a),
        plugin_view(&b),
        "同一份数据经两路读入，契约的业务内容必须逐字段相同"
    );
    // 丢行计数也一样（判定口径只有一处）
    assert_eq!(a["summary"]["skipped_rows"], 3);
    assert_eq!(b["summary"]["skipped_rows"], 3);
    assert_eq!(a["summary"]["blank_rows"], b["summary"]["blank_rows"]);
    assert_eq!(a["summary"]["invalid_rows"], b["summary"]["invalid_rows"]);
    // 只有格式属性与文件属性不同
    assert_eq!(a["ingest"]["encoding"], "utf-8");
    assert_eq!(b["ingest"]["encoding"], "xlsx（工作簿原生）");
}
