//! CSV 读取路径。
//!
//! CSV 是「企业组策略禁宏 / 专网报送机没有宏环境」时的降级入口（见 Task 42）：
//! 与 Excel 走同一套表头映射、同一套规则、同一份契约。
//!
//! # 现场导出出来的 CSV 长什么样
//!
//! 专网报送机上的表多半不是「干净的 UTF-8 半角逗号 CSV」，本模块对三样常见脏数据
//! 做**归一或明确报错**，不静默丢行、不静默丢字：
//!
//! | 脏数据 | 现场来由 | 本模块怎么处理 |
//! |--------|---------|--------------|
//! | GBK 编码 | 中文 Windows 的 WPS/Excel「CSV(逗号分隔)」默认存 GBK | 逐行解码；全表 GBK 记 `gbk`，部分行 GBK 记 `mixed` |
//! | UTF-8 与 GBK 混编 | 多表并入时从不同系统各粘了一段 | **逐行**判定：UTF-8 行原样留，GBK 行按 GBK 解 |
//! | 全角逗号分隔 | 从文档/网页里抄出来的表 | 表头里只有全角逗号时，引号外的全角逗号归一成半角 |
//!
//! 列名带空格（含全角空格）由表头归一处理（[`super::normalize_header`]），不在这里重复一遍。
//!
//! # 解不出来就报错，不猜
//!
//! 某一行既不是 UTF-8 也不是 GBK → 直接报错并点名**第几行**，
//! 让人去改那一行；不拿替换字符糊过去 —— 糊过去的话，这一行的中文会变成乱码，
//! 后面按名称匹配的规则会给出错误的「名称写法不一致」提示。

use std::path::Path;

use super::{FormatInfo, SourceTable};
use crate::errors::{EngineError, ErrorCode};

/// 读一张 CSV 表。表头在第一行；行宽不齐不报错（`flexible`），缺列由表头映射层判定。
pub fn read(path: &Path) -> Result<SourceTable, EngineError> {
    let bytes = std::fs::read(path).map_err(|e| {
        EngineError::at(
            ErrorCode::InputUnreadable,
            format!("CSV 打开失败：{}", e),
            path.display().to_string(),
        )
    })?;

    // 1. 解码：UTF-8 / GBK / 两者混编
    let (text, encoding) = decode_text(&bytes, path)?;
    // 2. 分隔符：表头里只有全角逗号时归一成半角
    let (text, delimiter) = normalize_delimiter(text, path)?;

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(text.as_bytes());

    let header: Vec<String> = reader
        .headers()
        .map_err(|e| {
            EngineError::at(
                ErrorCode::InputUnreadable,
                format!("CSV 表头读取失败：{}", e),
                path.display().to_string(),
            )
        })?
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| {
            EngineError::at(
                ErrorCode::InputUnreadable,
                format!("CSV 数据行解析失败：{}", e),
                path.display().to_string(),
            )
        })?;
        rows.push(record.iter().map(|s| s.to_string()).collect());
    }

    // 空行补回来：csv crate 会跳过整行空白的行，不补的话同一张表经 CSV 与
    // Excel 读进来的行数与行号会对不上（「第 7 行」指的是不同的行）。
    let physical = split_physical_lines(&text);
    let (rows, aligned) = align_rows(&physical, &rows, header.len());
    let mut format_note = String::new();
    if !aligned {
        // 物理行与解析器对不齐（畸形引号之类）：以解析器为准，一条数据都不丢
        format_note = "空行识别未对齐（按 csv 解析结果逐行读入）".to_string();
    }

    Ok(SourceTable {
        source_ns: super::file_stem(&path.display().to_string()),
        source_file: path.display().to_string(),
        header,
        rows,
        format: FormatInfo {
            encoding,
            delimiter,
            note: format_note,
        },
    })
}

/// 物理行的一行
struct PhysicalLine {
    /// 空行（整行零长度 —— 与 csv crate 跳过的那种行一致）
    blank: bool,
    /// 续行（上一行的引号没闭合，这一行是那个带换行字段的后半截）
    continuation: bool,
}

/// 按物理行切分文本，标出空行与续行。
///
/// **为什么自己切一遍**：csv crate 会跳过整行空白的行，也不把带换行的引号字段
/// 的续行单独算一条记录。同一张表经 CSV 与 Excel 读进来，空行要是在 CSV 这边
/// 被吞了，后面的行号就整体错位 —— 「第 7 行」指的是不同的行，报给科室就没意义了。
/// 所以这里按物理行把空行补回来，让两条入口的行数与行号完全一致。
///
/// 判定口径与 csv crate 对齐：
///
/// - **空行** = 整行零长度（只有行终止符）—— crate 跳过它；
/// - **只有空格的行** = crate 会当成一条记录（一个字段），不补，由判定层按
///   「所有格都是空的」归成空行 —— 两条入口的结论因此一致；
/// - **续行** = 引号里的换行，跟着上一条记录走，不单独算一行。
fn split_physical_lines(text: &str) -> Vec<PhysicalLine> {
    let mut out: Vec<PhysicalLine> = Vec::new();
    let mut in_quotes = false;
    let mut line_has_content = false;
    let mut line_continuation = false;

    for ch in text.chars() {
        match ch {
            '\n' => {
                out.push(PhysicalLine {
                    blank: !line_has_content && !line_continuation,
                    continuation: line_continuation,
                });
                line_continuation = in_quotes;
                line_has_content = false;
            }
            // 行尾的 \r 是行终止符的一部分，不算内容
            '\r' => {}
            '"' => {
                in_quotes = !in_quotes;
                line_has_content = true;
            }
            _ => {
                line_has_content = true;
            }
        }
    }
    // 末尾没有换行时补上最后一行
    if line_has_content {
        out.push(PhysicalLine {
            blank: false,
            continuation: line_continuation,
        });
    }
    out
}

/// 把物理行与解析出来的记录对齐：把空行补回原位。
///
/// 返回 `(行, 是否对齐)`。对不齐时以解析器为准（一条数据都不丢），
/// 由调用方记一笔格式说明 —— 不静默丢行，也不静默丢字。
fn align_rows(
    physical: &[PhysicalLine],
    records: &[Vec<String>],
    width: usize,
) -> (Vec<Vec<String>>, bool) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut ri = 0usize;
    let mut header_seen = false;
    for line in physical {
        if line.continuation {
            continue;
        }
        if !header_seen {
            // 第一条非空物理行是表头（csv crate 已经吃掉了它）
            if line.blank {
                continue;
            }
            header_seen = true;
            continue;
        }
        if line.blank {
            rows.push(vec![String::new(); width]);
        } else if ri < records.len() {
            rows.push(records[ri].clone());
            ri += 1;
        } else {
            return (records.to_vec(), false);
        }
    }
    if ri < records.len() {
        return (records.to_vec(), false);
    }
    (rows, true)
}

/// 解码：整表 UTF-8 → 逐行救（UTF-8 行原样、GBK 行按 GBK 解）→ 都不行就报错。
///
/// 返回 `(文本, 编码标签)`。编码标签取值：`utf-8` / `utf-8-bom` / `gbk` / `mixed`。
fn decode_text(bytes: &[u8], path: &Path) -> Result<(String, String), EngineError> {
    // UTF-16 带 BOM，中文 Windows 少见但确实有。本轮不支持 —— 明说，不糊。
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(EngineError::at(
            ErrorCode::InputUnreadable,
            "检测到 UTF-16 编码的 CSV，本轮只支持 UTF-8 与 GBK；请在 WPS/Excel 里另存为「CSV UTF-8」或「CSV(逗号分隔)」后重试",
            path.display().to_string(),
        ));
    }

    // UTF-8 BOM：去掉 BOM，其余按 UTF-8 读
    let (body, had_bom) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };
    if let Ok(text) = std::str::from_utf8(body) {
        let label = if had_bom { "utf-8-bom" } else { "utf-8" };
        return Ok((text.to_string(), label.to_string()));
    }

    // 逐行救：UTF-8 行原样留，GBK 行按 GBK 解。
    // 按行切是安全的 —— GBK 的第二字节范围是 0x40..=0xFE，不含 0x0A，
    // 所以多字节字符不会跨行被切断。
    let mut lines: Vec<&[u8]> = body.split(|b| *b == b'\n').collect();
    // 末尾换行会切出一个空段，去掉它（否则计数会多一行）
    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let mut out = String::with_capacity(body.len());
    let mut gbk_lines = 0usize;
    let mut utf8_nonascii_lines = 0usize;
    for (i, raw) in lines.iter().enumerate() {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        match std::str::from_utf8(raw) {
            Ok(text) => {
                // 纯 ASCII 的行 UTF-8 与 GBK 解出来一模一样 —— 不算「这一行是 UTF-8」。
                // 只把「带中文的 UTF-8 行」记进 utf8_nonascii_lines，
                // 否则「表头是 ASCII + 数据是 GBK」会被误判成混编。
                if !raw.is_ascii() {
                    utf8_nonascii_lines += 1;
                }
                out.push_str(text);
            }
            Err(_) => {
                let (decoded, _, had_errors) = encoding_rs::GBK.decode(raw);
                if had_errors {
                    return Err(EngineError::at(
                        ErrorCode::InputUnreadable,
                        format!(
                            "第 {} 行既不是 UTF-8 也不是 GBK，无法解码；请把该行另存为 UTF-8 后重试",
                            i + 1
                        ),
                        path.display().to_string(),
                    ));
                }
                gbk_lines += 1;
                out.push_str(&decoded);
            }
        }
        out.push('\n');
    }
    let label = if gbk_lines == 0 {
        "utf-8"
    } else if utf8_nonascii_lines == 0 {
        // 全表 GBK（ASCII 行在两种编码下解出来一样，不算混编）
        "gbk"
    } else {
        "mixed"
    };
    Ok((out, label.to_string()))
}

/// 分隔符归一：表头里只有全角逗号时，把引号外的全角逗号换成半角。
///
/// 只在「表头里一个半角逗号都没有、却有全角逗号」时才动 —— 这时候全角逗号
/// 只可能是分隔符。要是表头本来就是半角逗号分隔（值里恰好有个全角逗号），
/// 就一个字都不改：改了会把一个正常的值劈成两半。
fn normalize_delimiter(text: String, path: &Path) -> Result<(String, String), EngineError> {
    let first_line = text.lines().next().unwrap_or("");
    let ascii = count_outside_quotes(first_line, ',');
    let fullwidth = count_outside_quotes(first_line, '，');
    if fullwidth > 0 && ascii == 0 {
        let normalized = replace_outside_quotes(&text, '，', ',');
        return Ok((normalized, "全角逗号（已归一为半角）".to_string()));
    }
    let _ = path;
    Ok((text, "半角逗号".to_string()))
}

/// 引号外的目标字符个数（引号内的不算 —— 那是值的一部分）
fn count_outside_quotes(text: &str, target: char) -> usize {
    let mut count = 0usize;
    let mut in_quotes = false;
    for ch in text.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if !in_quotes && ch == target {
            count += 1;
        }
    }
    count
}

/// 把引号外的 target 换成 replacement（引号内的原样保留）
fn replace_outside_quotes(text: &str, target: char, replacement: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_quotes = false;
    for ch in text.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            out.push(ch);
            continue;
        }
        if !in_quotes && ch == target {
            out.push(replacement);
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("regval-csv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录应可建");
        dir.join(name)
    }

    #[test]
    fn utf8表照常读() {
        let path = tmp_path("utf8.csv");
        std::fs::write(&path, "指标编号,科室代码\nMER-HR-01,0101\n").expect("应可写");
        let table = read(&path).expect("应可读");
        assert_eq!(table.format.encoding, "utf-8");
        assert_eq!(table.format.delimiter, "半角逗号");
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn gbk表按gbk解码_中文不乱码() {
        let path = tmp_path("gbk.csv");
        let (text, _, _) = encoding_rs::GBK.encode("指标编号,科室代码\nMER-HR-01,0101\n");
        std::fs::write(&path, text.as_ref()).expect("应可写");
        let table = read(&path).expect("应可读");
        assert_eq!(table.format.encoding, "gbk");
        assert_eq!(table.header[0], "指标编号");
    }

    #[test]
    fn 混编表逐行解码_每行都对() {
        let path = tmp_path("mixed.csv");
        let (gbk_line, _, _) = encoding_rs::GBK.encode("MER-HR-01,病案室");
        let mut bytes = "指标编号,科室名称\n".as_bytes().to_vec();
        // 一行是 UTF-8 的中文，一行是 GBK 的中文 —— 真正意义的混编
        bytes.extend_from_slice("MER-HR-02,急诊科\n".as_bytes());
        bytes.extend_from_slice(gbk_line.as_ref());
        bytes.extend_from_slice(b"\n");
        std::fs::write(&path, &bytes).expect("应可写");
        let table = read(&path).expect("应可读");
        assert_eq!(table.format.encoding, "mixed");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][1], "急诊科");
        assert_eq!(table.rows[1][1], "病案室");
    }

    #[test]
    fn 全角逗号分隔归一成半角() {
        let path = tmp_path("fullwidth.csv");
        std::fs::write(&path, "指标编号，科室代码\nMER-HR-01，0101\n").expect("应可写");
        let table = read(&path).expect("应可读");
        assert_eq!(table.format.delimiter, "全角逗号（已归一为半角）");
        assert_eq!(table.header.len(), 2);
        assert_eq!(table.rows[0][1], "0101");
    }

    #[test]
    fn 半角逗号表里的全角逗号值不动() {
        let path = tmp_path("keep.csv");
        std::fs::write(&path, "指标编号,备注\nMER-HR-01,甲，乙\n").expect("应可写");
        let table = read(&path).expect("应可读");
        assert_eq!(table.format.delimiter, "半角逗号");
        assert_eq!(table.rows[0][1], "甲，乙");
    }

    #[test]
    fn 解不出来的行报错并点名第几行() {
        let path = tmp_path("bad.csv");
        let mut bytes = "指标编号,科室代码\n".as_bytes().to_vec();
        bytes.extend_from_slice(&[0x81, 0x20, 0x81]); // 孤立的 0x81：GBK 也解不了
        bytes.extend_from_slice(b"\n");
        std::fs::write(&path, &bytes).expect("应可写");
        let err = read(&path).expect_err("应报错");
        assert_eq!(err.code, ErrorCode::InputUnreadable);
        assert!(err.message.contains("第 2 行"), "应点名行号：{}", err.message);
    }

    #[test]
    fn utf16文件明确说不支持() {
        let path = tmp_path("utf16.csv");
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice("指标编号".as_bytes());
        std::fs::write(&path, &bytes).expect("应可写");
        let err = read(&path).expect_err("应报错");
        assert_eq!(err.code, ErrorCode::InputUnreadable);
        assert!(err.message.contains("UTF-16"), "应说清不支持：{}", err.message);
    }
}
