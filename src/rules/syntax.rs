//! 口径规则 SQL 的语法预校验与出错定位。
//!
//! # 为什么规则要先过语法这一关
//!
//! 口径规则表是业务人员手工维护的 JSON 文件，规则 SQL 是**数据**（取值走命名参数，
//! 不拼字符串）。SQL 写错时若放行，这条规则会在校验阶段才炸 —— 表现是「跑到一半
//! 报错」，而不是「装载时就告诉你第几个字符写错了」。
//!
//! 所以规则**装载即预校验**：语法不通过的规则标为「不生效」，不参与执行，
//! 并在结果契约的 `rule_status` 段里逐条列出出错 token 与偏移 —— **不静默通过**。
//!
//! # 三级降级链
//!
//! 预校验按三级降级链走，每一级都产出**同一种**出错定位结构
//! [`RuleSyntaxError`]（`token_type` / `found` / `offset`），
//! 上层拿到的信息与「是哪一级报出来的」无关：
//!
//! | 级别 | 检测方式 | 定位能力 |
//! |------|----------|----------|
//! | 一级 | SQLite 语法解析器（先解析成语法树，再执行） | 出错 token 类型 + 原文 + 字节偏移 |
//! | 二级 | SQLite 自身的预编译（与实际执行同一个解析器） | 同上 |
//! | 三级 | 括号与关键字配平自检（不依赖任何外部解析器） | 出错字符 + 字节偏移 |
//!
//! 一级覆盖整段 SQL；二级只编译**首条**语句（SQLite 的预编译接口按语句逐条编译），
//! 多语句规则的整段覆盖由一级负责；三级是最后一道保险 —— 前两级都不可用时，
//! 至少能报出括号没闭合、字符串没闭合这类硬伤。
//!
//! # 缺表缺列不算语法错
//!
//! 「表不存在」「列不存在」是**语义**问题，不是语法问题：它们由执行器报
//! `MISSING_COLUMN`（哪条规则、缺哪一列）。预校验只管语法 —— 把语义问题
//! 混进语法错会让口径问题清单里出现误导性的条目。
//!
//! # 源产品机制迁移
//!
//! 「SQL 先经解析器得到语法树再执行、解析错误带出错 token」这一套取自参考实现的
//! **解析器与词法器分层**：解析错误按种类分（语法错 / 意外 EOF / 自定义），每种都带
//! 出错 token 的类型与原文；词法层另有一份 tokenizer 与关键字表（关键字逐个登记，
//! 可反查回原文）。
//! 移植到本产品：口径规则表的**装载即预校验**，语法错 → 报出错 token 与偏移，
//! 该规则不生效。溯源见 round 根 `spec.md` → `## 开源对照`。

use std::fmt;

use rusqlite::Connection;
use sqlite3_parser::dialect::TokenType;
use sqlite3_parser::lexer::sql::Tokenizer;
use sqlite3_parser::lexer::sql::{Error as SqlError, Parser as SqlParser, ParserError};
use sqlite3_parser::lexer::Scanner;
use sqlite3_parser::{Bump, FallibleIterator};

use crate::db::schema::ROWS_TABLE;

/// 出错定位（三级降级链统一产出这一种结构）。
///
/// 三个字段与来源无关：一级、二级、三级报出来的都是这一种。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSyntaxError {
    /// 出错 token 的类型（业务可读：`SELECT`、`,`、`(`、`字符串`……）
    pub token_type: String,
    /// 出错 token 原文（解析器只给位置不给原文时为空串）
    pub found: String,
    /// 出错 token 在规则 SQL 文本中的字节偏移（0 起）
    pub offset: usize,
}

impl fmt::Display for RuleSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "出错 token 类型「{}」，原文「{}」，在规则 SQL 第 {} 个字节处",
            self.token_type, self.found, self.offset
        )
    }
}

/// 检测级别（降级链里是哪一级报出来的）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxDetector {
    /// 一级：SQLite 语法解析器
    Sqlite3Parser,
    /// 二级：SQLite 自身的预编译
    RusqlitePrepare,
    /// 三级：括号与关键字配平自检
    BalanceCheck,
}

impl SyntaxDetector {
    /// 中文名（写进规则清单，让运维看得懂是哪一级拦下的）
    pub fn as_str(self) -> &'static str {
        match self {
            SyntaxDetector::Sqlite3Parser => "语法解析器",
            SyntaxDetector::RusqlitePrepare => "SQLite 预编译",
            SyntaxDetector::BalanceCheck => "括号配平自检",
        }
    }
}

impl fmt::Display for SyntaxDetector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条规则的语法预校验结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxReport {
    /// 哪一级检测出来的
    pub detector: SyntaxDetector,
    /// 统一的出错定位
    pub error: RuleSyntaxError,
    /// 检测器给的原始说明（给信息科工程师看，业务人员看 `error` 即可）
    pub detail: String,
}

/// 预校验一条规则 SQL：不通过返回出错定位，通过返回 `None`。
///
/// 走完整的三级降级链 —— 一级报了就直接返回；一级放过但二级（SQLite 自己）
/// 不认，说明方言有差异，也算不通过；两级都放过才走三级兜底。
pub fn check_sql(sql: &str) -> Option<SyntaxReport> {
    if let Some(report) = check_sql_with(sql, SyntaxDetector::Sqlite3Parser) {
        return Some(report);
    }
    if let Some(report) = check_sql_with(sql, SyntaxDetector::RusqlitePrepare) {
        return Some(report);
    }
    check_sql_with(sql, SyntaxDetector::BalanceCheck)
}

/// 只用指定的一级检测（供降级链逐级验证与回归测试使用）
pub fn check_sql_with(sql: &str, detector: SyntaxDetector) -> Option<SyntaxReport> {
    match detector {
        SyntaxDetector::Sqlite3Parser => detect_with_parser(sql),
        SyntaxDetector::RusqlitePrepare => detect_with_prepare(sql),
        SyntaxDetector::BalanceCheck => detect_with_balance(sql),
    }
}

/// 字节偏移 → 行列（1 起；列按字节计，与解析器一致）
///
/// 给业务人员看的报错里，字节偏移不好读 —— 换成「第几行第几列」才看得懂改哪儿。
pub fn line_column(sql: &str, offset: usize) -> (usize, usize) {
    let bytes = sql.as_bytes();
    let end = offset.min(bytes.len());
    let mut line = 1usize;
    let mut column = 1usize;
    for byte in &bytes[..end] {
        if *byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

// ——————————————————————— 一级：SQLite 语法解析器 ———————————————————————

/// 一级检测：用解析器把 SQL 解析成语法树，报错带出错 token 与位置。
fn detect_with_parser(sql: &str) -> Option<SyntaxReport> {
    let bump = Bump::new();
    let mut parser = SqlParser::new(&bump, sql.as_bytes());
    loop {
        match parser.next() {
            Ok(None) => return None,
            Ok(Some(_)) => continue,
            Err(err) => {
                let detail = err.to_string();
                let (token_type, found, offset) = describe_parser_error(sql, &err);
                return Some(SyntaxReport {
                    detector: SyntaxDetector::Sqlite3Parser,
                    error: RuleSyntaxError {
                        token_type,
                        found,
                        offset,
                    },
                    detail,
                });
            }
        }
    }
}

/// 把解析器的报错翻译成统一的出错定位
///
/// 解析器的位置类型不对外暴露，所以位置从报错文本里取
/// （`near "X": syntax error at line: 1, column: 8`）——
/// 文本格式是解析器自己的 `Display`/`Debug` 输出，位置就在里面。
fn describe_parser_error(sql: &str, err: &SqlError) -> (String, String, usize) {
    let detail = err.to_string();
    let position = parse_position(&detail).map(|(line, column)| offset_of_pos(sql, line, column));
    match err {
        SqlError::ParserError(ParserError::SyntaxError(msg), _) => {
            let found = extract_near_lexeme(msg);
            let offset = position.unwrap_or_else(|| locate_lexeme(sql, &found));
            let token_type = token_type_at(sql, offset).unwrap_or_else(|| "标识符".to_string());
            (token_type, found, offset)
        }
        SqlError::ParserError(ParserError::UnexpectedEof, _) => {
            let offset = position.unwrap_or_else(|| sql.len());
            ("输入结束".to_string(), String::new(), offset)
        }
        SqlError::ParserError(ParserError::Custom(_), _) => (
            "语法不匹配".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::UnrecognizedToken(_) => (
            "无法识别的字符".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::UnterminatedLiteral(_) => (
            "字符串未闭合".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::UnterminatedBracket(_) => (
            "中括号未闭合".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::UnterminatedBlockComment(_) => (
            "块注释未闭合".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::BadVariableName(_) => (
            "参数名不合法".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::BadNumber(_) => (
            "数字格式不合法".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::ExpectedEqualsSign(_) => (
            "!= 写法不完整".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::MalformedBlobLiteral(_) => (
            "二进制字面量不合法".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        SqlError::MalformedHexInteger(_) => (
            "十六进制整数不合法".to_string(),
            String::new(),
            position.unwrap_or_default(),
        ),
        // 非穷尽匹配（解析器 crate 预留了新错误类型）
        _ => ("语法不匹配".to_string(), String::new(), 0),
    }
}

/// 从解析器报错文本里取行列位置（`line: 1, column: 8`）
fn parse_position(text: &str) -> Option<(usize, usize)> {
    let line = parse_number_after(text, "line: ")?;
    let column = parse_number_after(text, "column: ")?;
    Some((line, column))
}

/// 取标记后面的整数
fn parse_number_after(text: &str, marker: &str) -> Option<usize> {
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse::<usize>().ok()
}

/// 解析器位置（行列）→ 字节偏移
fn offset_of_pos(sql: &str, line: usize, column: usize) -> usize {
    let bytes = sql.as_bytes();
    let (mut cur_line, mut cur_col) = (1usize, 1usize);
    for (offset, byte) in bytes.iter().enumerate() {
        if cur_line == line && cur_col == column {
            return offset;
        }
        if *byte == b'\n' {
            cur_line += 1;
            cur_col = 1;
        } else {
            cur_col += 1;
        }
    }
    bytes.len()
}

// ——————————————————————— 二级：SQLite 自身预编译 ———————————————————————

/// 二级检测：让 SQLite 自己预编译这条 SQL —— 与实际执行走的是同一个解析器。
///
/// 缺表 / 缺列不算语法错（那是执行器报 `MISSING_COLUMN` 的事），这里放过。
fn detect_with_prepare(sql: &str) -> Option<SyntaxReport> {
    let conn = Connection::open_in_memory().ok()?;
    // 建出规则运行时的目标表：语法检查要判的是「这条 SQL 能不能跑」，
    // 不是「表在不在」。
    let _ = conn.execute_batch(ROWS_TABLE);
    let outcome: Result<(), String> = match conn.prepare(sql) {
        Ok(stmt) => {
            drop(stmt);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    };
    match outcome {
        Ok(()) => None,
        Err(detail) => {
            if is_semantic_error(&detail) {
                return None;
            }
            let (token_type, found, offset) = describe_prepare_error(sql, &detail);
            Some(SyntaxReport {
                detector: SyntaxDetector::RusqlitePrepare,
                error: RuleSyntaxError {
                    token_type,
                    found,
                    offset,
                },
                detail,
            })
        }
    }
}

/// 「表不存在」「列不存在」是语义问题，不是语法问题
fn is_semantic_error(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("no such table") || lower.contains("no such column")
}

/// SQLite 预编译报错 → 统一出错定位
fn describe_prepare_error(sql: &str, detail: &str) -> (String, String, usize) {
    let found = extract_near_lexeme(detail);
    let offset = locate_lexeme(sql, &found);
    let token_type = if found.is_empty() {
        "语法不匹配".to_string()
    } else {
        token_type_at(sql, offset).unwrap_or_else(|| "标识符".to_string())
    };
    (token_type, found, offset)
}

// ——————————————————————— 三级：括号与关键字配平 ———————————————————————

/// 三级检测：括号、字符串、块注释、CASE…END 的配平自检。
///
/// 不依赖任何解析器 —— 前两级都不可用时的最后一道保险。
fn detect_with_balance(sql: &str) -> Option<SyntaxReport> {
    balance_check(sql).map(|(token_type, found, offset, detail)| SyntaxReport {
        detector: SyntaxDetector::BalanceCheck,
        error: RuleSyntaxError {
            token_type,
            found,
            offset,
        },
        detail,
    })
}

/// 配平检查：返回 (出错类型, 出错原文, 字节偏移, 说明)
fn balance_check(sql: &str) -> Option<(String, String, usize, String)> {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    // 未闭合的左括号（记偏移，便于报「第几个字符没合上」）
    let mut paren_stack: Vec<usize> = Vec::new();
    // 未闭合的 CASE
    let mut case_stack: Vec<usize> = Vec::new();

    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[i];
                let start = i;
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        // 两个相同的引号是转义，不是结束
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Some((
                        "字符串未闭合".to_string(),
                        (quote as char).to_string(),
                        start,
                        format!("第 {} 字节处开始的引号没有配对的结束引号", start),
                    ));
                }
                continue;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let start = i;
                i += 2;
                let mut closed = false;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Some((
                        "块注释未闭合".to_string(),
                        "/*".to_string(),
                        start,
                        format!("第 {} 字节处开始的块注释没有 */ 结束", start),
                    ));
                }
                continue;
            }
            b'(' => {
                paren_stack.push(i);
                i += 1;
                continue;
            }
            b')' => {
                if paren_stack.pop().is_none() {
                    return Some((
                        "括号不配对".to_string(),
                        ")".to_string(),
                        i,
                        format!("第 {} 字节处多出一个右括号，没有与之配对的左括号", i),
                    ));
                }
                i += 1;
                continue;
            }
            _ => {}
        }

        // 关键字（只认 ASCII 字母开头的词；中文与其它字符按字节跳过）
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &sql[start..i];
            match word.to_ascii_uppercase().as_str() {
                "CASE" => case_stack.push(start),
                "END" => {
                    if case_stack.pop().is_none() {
                        return Some((
                            "关键字不配对".to_string(),
                            "END".to_string(),
                            start,
                            format!("第 {} 字节处出现 END，但没有与之配对的 CASE", start),
                        ));
                    }
                }
                _ => {}
            }
            continue;
        }
        i += 1;
    }

    if let Some(&offset) = paren_stack.last() {
        return Some((
            "括号未闭合".to_string(),
            "(".to_string(),
            offset,
            format!("第 {} 字节处的左括号没有配对的右括号", offset),
        ));
    }
    if let Some(&offset) = case_stack.last() {
        return Some((
            "关键字未闭合".to_string(),
            "CASE".to_string(),
            offset,
            format!("第 {} 字节处的 CASE 没有配对的 END", offset),
        ));
    }
    None
}

// ——————————————————————— 共用小工具 ———————————————————————

/// 从报错文本里取出出错 token 原文
///
/// 两种格式都要认：SQLite 自己的 `near "X": syntax error`，
/// 以及解析器给的裸 token（`X`）。
fn extract_near_lexeme(message: &str) -> String {
    let trimmed = message.trim();
    if let Some(start) = trimmed.find('"') {
        if let Some(rel_end) = trimmed[start + 1..].find('"') {
            return trimmed[start + 1..start + 1 + rel_end].to_string();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("near ") {
        let end = rest.find(':').unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    trimmed.to_string()
}

/// 在 SQL 文本里定位出错 token 原文（先精确匹配，再忽略大小写）
fn locate_lexeme(sql: &str, lexeme: &str) -> usize {
    if lexeme.is_empty() {
        return 0;
    }
    if let Some(i) = sql.find(lexeme) {
        return i;
    }
    let lower = lexeme.to_lowercase();
    sql.to_lowercase().find(&lower).unwrap_or(0)
}

/// 某个字节偏移处 token 的类型名（业务可读）
fn token_type_at(sql: &str, offset: usize) -> Option<String> {
    let bytes = sql.as_bytes();
    let mut scanner = Scanner::new(Tokenizer::new());
    let mut cursor = 0usize;
    loop {
        let (start, token, end) = scanner.scan(bytes).ok()?;
        if start >= bytes.len() {
            return None;
        }
        if let Some((lexeme, token_type)) = token {
            if offset >= start && offset < end {
                return Some(friendly_token_name(token_type, lexeme));
            }
        }
        if end <= cursor {
            return None;
        }
        cursor = end;
    }
}

/// token 类型 → 业务可读的名字
fn friendly_token_name(token_type: TokenType, lexeme: &[u8]) -> String {
    if let Some(name) = token_type.as_str() {
        return name.to_string();
    }
    match token_type {
        TokenType::TK_ID => "标识符".to_string(),
        TokenType::TK_STRING => "字符串".to_string(),
        TokenType::TK_VARIABLE => "命名参数".to_string(),
        TokenType::TK_INTEGER => "整数".to_string(),
        TokenType::TK_FLOAT => "小数".to_string(),
        TokenType::TK_BLOB => "二进制串".to_string(),
        TokenType::TK_ANY => "通配符".to_string(),
        TokenType::TK_EOF => "输入结束".to_string(),
        _ => String::from_utf8_lossy(lexeme).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROKEN: &str = "SELEC row_uid FROM rows WHERE (indicator_code = :scope";
    const GOOD: &str = "SELECT row_uid AS row_uid, :label_1 AS missing_column FROM rows \
                         WHERE batch_ref = :batch_ref AND indicator_code = :scope";

    #[test]
    fn 三级降级链在同一条坏规则上都能报出错位置() {
        for detector in [
            SyntaxDetector::Sqlite3Parser,
            SyntaxDetector::RusqlitePrepare,
            SyntaxDetector::BalanceCheck,
        ] {
            let report = check_sql_with(BROKEN, detector)
                .unwrap_or_else(|| panic!("{} 级别没能报出坏规则", detector));
            assert_eq!(report.detector, detector);
            assert!(
                !report.error.token_type.is_empty(),
                "{} 级别的出错 token 类型为空",
                detector
            );
            assert!(
                report.error.offset <= BROKEN.len(),
                "{} 级别的偏移越界：{}",
                detector,
                report.error.offset
            );
        }
    }

    #[test]
    fn 一级与二级对同一条坏规则给出一致的定位() {
        let one = check_sql_with(BROKEN, SyntaxDetector::Sqlite3Parser).unwrap();
        let two = check_sql_with(BROKEN, SyntaxDetector::RusqlitePrepare).unwrap();
        assert_eq!(one.error, two.error, "两个 SQLite 解析器应给出一致的定位");
        assert_eq!(one.error.offset, 0, "出错 token 在句首");
        assert_eq!(one.error.found, "SELEC");
    }

    #[test]
    fn 好规则三级都放过() {
        for detector in [
            SyntaxDetector::Sqlite3Parser,
            SyntaxDetector::RusqlitePrepare,
            SyntaxDetector::BalanceCheck,
        ] {
            assert!(
                check_sql_with(GOOD, detector).is_none(),
                "{} 级别误报了好规则",
                detector
            );
        }
        assert!(check_sql(GOOD).is_none());
    }

    #[test]
    fn 缺表缺列不算语法错() {
        // 表不存在是语义问题，交给执行器报 MISSING_COLUMN
        let sql = "SELECT row_uid FROM no_such_table";
        assert!(check_sql_with(sql, SyntaxDetector::RusqlitePrepare).is_none());
    }

    #[test]
    fn 行列换算与字节偏移对得上() {
        let sql = "SELECT 1\nFROM rows";
        let offset = sql.find("rows").unwrap();
        assert_eq!(line_column(sql, offset), (2, 6));
    }
}
