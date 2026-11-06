//! 提示文案常量表 —— 表格插件侧所有面向用户的文案，**只有这一处出处**。
//!
//! # 这一层为什么存在
//!
//! 宏里有三处会说话：状态条、对话框、错误行。同一件事在三处各写一遍中文，
//! 迟早会各说各的 —— 引擎的 `ErrorCode::zh()` 写「引擎与宏版本不一致，请升级」，
//! 宏里手写一遍「版本对不上」，科室截图发到群里，两边说法对不上，
//! 还得打电话问一句「到底哪个是准的」。
//!
//! 所以文案的规矩是：
//!
//! - **错误码的中文说明**只有一处出处：[`crate::errors::ErrorCode::zh`]。
//!   本模块不重抄，只把它翻译成 VBA 常量（`REGVAL_ERR_*`）与查表函数
//!   （`RegValErrorText`）。抄第二份就等于埋雷。
//! - **其余提示文案**（加载态、空结果态、对话框、状态条）集中在 [`MESSAGES`]，
//!   由 [`render_vba_module`] 生成 `macro/vba/RegValMessages.bas`。
//! - 宏里**不写死任何面向用户的中文**（`tests/ui_states_test.rs` 会盯着这件事）：
//!   要么引用 `REGVAL_MSG_*` 常量，要么查 `RegValErrorText`。
//!
//! 改文案的正确做法：改这里 → `cargo run -- messages --vba --out macro/vba/RegValMessages.bas`
//! → 跑测试。生成物与源文件逐字比对，改歪了会被测出来。

use serde_json::{json, Value};

use crate::errors::ErrorCode;

/// 文案表版本（宏侧据此判断能不能读这份清单）。
pub const MESSAGES_SCHEMA_VERSION: &str = "regval.messages/1.0.0";

// ─────────────────────────────────────────────────────────────────────
// 文案常量：每个字符串在本文件里只出现一次
// ─────────────────────────────────────────────────────────────────────

/// 加载态：引擎在跑。
pub const LOADING: &str = "校验中…";
/// 加载态：上报在跑。
pub const LOADING_PUSH: &str = "上报中…";
/// 空结果态之一：表里一行数据都没有（与「没有问题」分开说）。
pub const EMPTY_ROWS: &str = "表里没有数据行 · 未做校验";
/// 空结果态之二：校验过了，一项口径问题都没发现。
pub const EMPTY_ISSUES: &str = "已校验 {rows} 行 · 未发现口径问题";
/// 错误态：状态条上那一句（错误码 + 中文说明）。
pub const ERROR_LINE: &str = "错误码：{code} · {code_zh}";
/// 错误态：错误码不在表里时（不吞掉错误码，只是不给中文说明）。
pub const ERROR_LINE_UNKNOWN: &str = "错误码：{code}";

/// 对话框标题。
pub const DIALOG_TITLE: &str = "卫健上报校验";
/// 按钮标题：校验。
pub const BUTTON_VALIDATE: &str = "校验";
/// 按钮标题：上报。
pub const BUTTON_PUSH: &str = "上报";
/// 没打开表。
pub const NO_WORKBOOK: &str = "请先打开要校验的上报表。";
/// 表没保存到磁盘。
pub const NOT_SAVED: &str = "请先把表格保存到磁盘（引擎读的是磁盘上的文件）。";
/// 装界面成功。
pub const INSTALLED: &str = "已装好「校验」「上报」两个按钮。";
/// 装界面成功后的用法提示。
pub const INSTALLED_HINT: &str =
    "点「校验」把结果写回右侧 5 个新列；点「上报」把已确认的行推到院内上报网关。";
/// 引擎二进制找不到。
pub const ENGINE_MISSING: &str = "找不到引擎程序：{path}";
/// 引擎二进制找不到时怎么办。
pub const ENGINE_MISSING_HINT: &str =
    "请修改 RegVal.bas 顶部的 REGVAL_ENGINE_PATH，或参照 macro/README.md 的安装说明。";
/// 引擎跑完了但没产出契约。
pub const NO_CONTRACT: &str = "引擎没有产出结果契约文件，原表未被改动。";
/// 版本闸门：宏要求什么。
pub const EXPECT_VALUE: &str = "宏要求：{value}";
/// 版本闸门：引擎给的是什么。
pub const ACTUAL_VALUE: &str = "引擎产出：{value}";
/// 列名闸门：宏期望的列。
pub const COLUMNS_EXPECTED: &str = "宏期望的列：{value}";
/// 列名闸门：引擎产出的列。
pub const COLUMNS_ACTUAL: &str = "引擎产出的列：{value}";
/// 原表没被动过。
pub const UNTOUCHED: &str = "原表未被改动。";
/// 校验这一轮失败。
pub const VALIDATE_FAILED: &str = "校验未完成，原表未被改动。";
/// 上报这一轮失败。
pub const PUSH_FAILED: &str = "上报未完成，原表未被改动。";
/// 没有批次号。
pub const NO_BATCH: &str = "这一批没有批次号，上报无从谈起。请检查引擎版本是否与宏一致。";
/// 上报完成但没取到 summary 里的句子。
pub const SUBMIT_DONE_FALLBACK: &str = "上报完成，回执已回填";
/// 有未保存改动（第一句）。
pub const UNSAVED_CHANGES: &str = "表格有未保存的改动。";
/// 有未保存改动（第二句）。
pub const UNSAVED_CHANGES_HINT: &str = "引擎读的是磁盘上的文件，未保存的改动它看不到。";
/// 有未保存改动（问句）。
pub const UNSAVED_CHANGES_QUESTION: &str = "要现在保存再继续吗？";
/// 引擎没留下错误报告。
pub const NO_ERROR_REPORT: &str = "（引擎没有留下错误报告，请到命令行窗口查看详情）";
/// 处理建议前缀。
pub const ADVICE_PREFIX: &str = "处理建议：";
/// 样式没套上（数据是好的，但外观没到位 —— 如实说）。
pub const STYLE_ERROR: &str = "样式未完全套上：{detail}";
/// 状态条没写上。
pub const STATUSBAR_FAILED: &str = "状态条未写上：{detail}";
/// 契约里没取到 summary 状态句。
pub const SUMMARY_MISSING: &str = "已校验（未取到摘要）";
/// 上一轮还没跑完（重复提交被拦下）。
pub const BUSY: &str = "上一轮还没跑完，请等它结束再点。";
/// 校验完成后的一句补充（写进状态条，不弹框）。
pub const WRITEBACK_NOTE: &str = "结果已写回原表右侧 5 个新列（原表格式与公式未动）。";
/// 上报完成后的一句补充（写进状态条，不弹框）。
pub const PUSH_NOTE: &str = "「上报状态」列已按回执回填（绿=已上报 / 橙=被退回 / 灰=待重试）。";

/// 一条文案。
#[derive(Debug, Clone, Copy)]
pub struct MessageToken {
    /// 稳定的 ASCII 键（宏里是 `REGVAL_MSG_<KEY>`）。
    pub key: &'static str,
    /// 中文文案（`{name}` 是占位符）。
    pub text: &'static str,
    /// 这句话在什么时候用（一行注释，宏里也看得到）。
    pub note: &'static str,
}

/// 提示文案常量表 —— **宏侧文案的唯一出处**。
pub const MESSAGES: &[MessageToken] = &[
    MessageToken { key: "loading", text: LOADING, note: "加载态：引擎在跑" },
    MessageToken { key: "loading_push", text: LOADING_PUSH, note: "加载态：上报在跑" },
    MessageToken { key: "empty_rows", text: EMPTY_ROWS, note: "空结果态：表里没有行" },
    MessageToken { key: "empty_issues", text: EMPTY_ISSUES, note: "空结果态：没有问题" },
    MessageToken { key: "error_line", text: ERROR_LINE, note: "错误态：状态条那一句" },
    MessageToken { key: "error_line_unknown", text: ERROR_LINE_UNKNOWN, note: "错误态：错误码不在表里" },
    MessageToken { key: "dialog_title", text: DIALOG_TITLE, note: "对话框标题" },
    MessageToken { key: "button_validate", text: BUTTON_VALIDATE, note: "按钮标题：校验" },
    MessageToken { key: "button_push", text: BUTTON_PUSH, note: "按钮标题：上报" },
    MessageToken { key: "no_workbook", text: NO_WORKBOOK, note: "没打开表" },
    MessageToken { key: "not_saved", text: NOT_SAVED, note: "表没存盘" },
    MessageToken { key: "installed", text: INSTALLED, note: "装界面成功" },
    MessageToken { key: "installed_hint", text: INSTALLED_HINT, note: "装界面成功后的用法" },
    MessageToken { key: "engine_missing", text: ENGINE_MISSING, note: "引擎二进制找不到" },
    MessageToken { key: "engine_missing_hint", text: ENGINE_MISSING_HINT, note: "引擎二进制找不到怎么办" },
    MessageToken { key: "no_contract", text: NO_CONTRACT, note: "引擎没产出契约" },
    MessageToken { key: "expect_value", text: EXPECT_VALUE, note: "版本闸门：宏要求" },
    MessageToken { key: "actual_value", text: ACTUAL_VALUE, note: "版本闸门：引擎产出" },
    MessageToken { key: "columns_expected", text: COLUMNS_EXPECTED, note: "列名闸门：宏期望" },
    MessageToken { key: "columns_actual", text: COLUMNS_ACTUAL, note: "列名闸门：引擎产出" },
    MessageToken { key: "untouched", text: UNTOUCHED, note: "原表没被动过" },
    MessageToken { key: "validate_failed", text: VALIDATE_FAILED, note: "校验失败" },
    MessageToken { key: "push_failed", text: PUSH_FAILED, note: "上报失败" },
    MessageToken { key: "no_batch", text: NO_BATCH, note: "没有批次号" },
    MessageToken { key: "submit_done_fallback", text: SUBMIT_DONE_FALLBACK, note: "上报完成兜底句" },
    MessageToken { key: "unsaved_changes", text: UNSAVED_CHANGES, note: "有未保存改动" },
    MessageToken { key: "unsaved_changes_hint", text: UNSAVED_CHANGES_HINT, note: "有未保存改动" },
    MessageToken { key: "unsaved_changes_question", text: UNSAVED_CHANGES_QUESTION, note: "有未保存改动" },
    MessageToken { key: "no_error_report", text: NO_ERROR_REPORT, note: "引擎没留错误报告" },
    MessageToken { key: "advice_prefix", text: ADVICE_PREFIX, note: "处理建议前缀" },
    MessageToken { key: "style_error", text: STYLE_ERROR, note: "样式没套上" },
    MessageToken { key: "statusbar_failed", text: STATUSBAR_FAILED, note: "状态条没写上" },
    MessageToken { key: "summary_missing", text: SUMMARY_MISSING, note: "没取到摘要" },
    MessageToken { key: "busy", text: BUSY, note: "重复提交被拦下" },
    MessageToken { key: "writeback_note", text: WRITEBACK_NOTE, note: "写回完成后的补充句" },
    MessageToken { key: "push_note", text: PUSH_NOTE, note: "上报完成后的补充句" },
];

/// 按键取文案（键不认识返回空串，不 panic —— 宏里传错键不该把引擎带崩）。
pub fn text(key: &str) -> &'static str {
    MESSAGES
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.text)
        .unwrap_or("")
}

/// 错误码 → 中文说明（**不重抄**：直接取 [`ErrorCode::zh`]）。
pub fn error_text(code: &str) -> Option<&'static str> {
    ErrorCode::ALL
        .iter()
        .find(|c| c.as_str() == code)
        .map(|c| c.zh())
}

/// 按错误码取中文说明；表里没有就返回空串（调用方决定怎么兜底）。
pub fn error_text_for(code: &str) -> &'static str {
    error_text(code).unwrap_or("")
}

/// 文案模板占位符替换：`{rows}` 这类标记换成实值。
///
/// 键不认识就原样留着（不静默吞掉占位符 —— 宏里看到 `{rows}` 残在界面上
/// 就知道模板与调用对不上）。
pub fn format(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in args {
        out = out.replace(&format!("{{{}}}", key), value);
    }
    out
}

/// 错误态状态条那句话：`错误码：XXX · 中文说明`。
///
/// 错误码不在表里时不编造中文说明，只把错误码原样带出去 ——
/// 「不知道就说不知道」比「猜一句」好。
pub fn error_line(code: &str, code_zh: &str) -> String {
    if code_zh.is_empty() {
        format(ERROR_LINE_UNKNOWN, &[("code", code)])
    } else {
        format(ERROR_LINE, &[("code", code), ("code_zh", code_zh)])
    }
}

/// 宏侧常量名（`REGVAL_MSG_LOADING` / `REGVAL_ERR_<CODE>`）。
pub fn vba_const_name(key: &str) -> String {
    format!("REGVAL_MSG_{}", key.to_uppercase())
}

/// 错误码的宏侧常量名。
pub fn vba_error_const_name(code: &str) -> String {
    format!("REGVAL_ERR_{}", code.to_uppercase())
}

/// JSON 清单（文档、doctor 与测试共用）。
pub fn manifest_json() -> Value {
    let messages: Vec<Value> = MESSAGES
        .iter()
        .map(|m| {
            json!({
                "key": m.key,
                "text": m.text,
                "note": m.note,
                "vba_const": vba_const_name(m.key),
            })
        })
        .collect();

    let errors: Vec<Value> = ErrorCode::ALL
        .iter()
        .map(|c| {
            json!({
                "code": c.as_str(),
                "code_zh": c.zh(),
                "advice": c.advice(),
                "vba_const": vba_error_const_name(c.as_str()),
            })
        })
        .collect();

    json!({
        "schema_version": MESSAGES_SCHEMA_VERSION,
        "message_count": MESSAGES.len(),
        "error_code_count": ErrorCode::ALL.len(),
        "messages": messages,
        "error_codes": errors,
    })
}

/// 生成宏侧常量模块（`RegValMessages.bas`）。
///
/// 宏里所有面向用户的中文都从这里来 —— `RegVal.bas` 自己一个字都不写死，
/// `tests/ui_states_test.rs` 会盯着这件事。
pub fn render_vba_module() -> String {
    let mut s = String::new();
    s.push_str("'======================================================================\n");
    s.push_str("' RegValMessages —— 表格内插件的提示文案常量表（由 regval messages --vba 生成）\n");
    s.push_str("'\n");
    s.push_str("' 唯一出处：src/ui/messages.rs（提示文案）+ src/errors.rs（错误码中文说明）。\n");
    s.push_str("' 改文案请改那里，再重新生成本文件：\n");
    s.push_str("'   cargo run -- messages --vba --out macro/vba/RegValMessages.bas\n");
    s.push_str("' tests/ui_states_test.rs 会把本文件与引擎逐字比对，改歪了会被测出来。\n");
    s.push_str("'\n");
    s.push_str("' 编码：本文件是 UTF-8。中文 Windows 的 VBE 按系统 ANSI 代码页读模块文件，\n");
    s.push_str("' 直接导入可能出现乱码 —— 按 macro/README.md 的说明另存为 ANSI/GBK 再导入。\n");
    s.push_str("'======================================================================\n");
    s.push_str("Option Explicit\n\n");
    s.push_str(&format!(
        "Public Const MESSAGES_SCHEMA_VERSION As String = \"{}\"\n\n",
        MESSAGES_SCHEMA_VERSION
    ));

    s.push_str("' ---- 提示文案（宏里只引用这些常量，不写死中文）----\n");
    for m in MESSAGES {
        s.push_str(&format!(
            "Public Const {} As String = \"{}\"\n",
            vba_const_name(m.key),
            m.text.replace('"', "\"\"")
        ));
    }
    s.push('\n');

    s.push_str("' ---- 错误码中文说明（与引擎 src/errors.rs 的 ErrorCode::zh() 同源）----\n");
    for c in ErrorCode::ALL {
        s.push_str(&format!(
            "Public Const {} As String = \"{}\"\n",
            vba_error_const_name(c.as_str()),
            c.zh().replace('"', "\"\"")
        ));
    }
    s.push('\n');

    s.push_str("' ---- 按错误码取中文说明（表里没有就原样返回错误码，不吞掉）----\n");
    s.push_str("Public Function RegValErrorText(ByVal code As String) As String\n");
    s.push_str("    Select Case UCase$(Trim$(code))\n");
    for c in ErrorCode::ALL {
        s.push_str(&format!(
            "    Case \"{}\"\n        RegValErrorText = {}\n",
            c.as_str(),
            vba_error_const_name(c.as_str())
        ));
    }
    s.push_str("    Case Else\n        RegValErrorText = code\n    End Select\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 文案占位符替换（{rows} 这类标记换成实值）----\n");
    s.push_str("Public Function RegValFmt(ByVal template As String, ByVal key As String, ByVal value As String) As String\n");
    s.push_str("    RegValFmt = Replace(template, \"{\" & key & \"}\", value)\n");
    s.push_str("End Function\n");
    s
}

/// JS 字符串字面量转义（反斜杠 / 双引号 / 换行 / 制表符）。
fn js_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// 生成 WPS JSA 侧的文案块（注入 `macro/wps/regval.js`）。
///
/// 与 [`render_vba_module`] **同源**：文案与错误码中文说明都只在这一处定义，
/// 只是载体从「VBA 常量」换成「JS 对象字面量」。函数名与 VBA 侧刻意取成一样的。
pub fn render_jsa_module() -> String {
    let mut s = String::new();
    s.push_str("// ----------------------------------------------------------------------\n");
    s.push_str("// 提示文案常量表（唯一出处：src/ui/messages.rs + src/errors.rs，由 `regval macro` 注入）\n");
    s.push_str("// 与 Excel 侧的 RegValMessages.bas 同源：同一批文案，同一批错误码中文说明。\n");
    s.push_str("// ----------------------------------------------------------------------\n");
    s.push_str("var REGVAL_MESSAGES = {\n");
    for m in MESSAGES {
        s.push_str(&format!(
            "  \"{}\": \"{}\",\n",
            m.key,
            js_escape(m.text)
        ));
    }
    s.push_str("};\n\n");

    s.push_str("var REGVAL_ERROR_ZH = {\n");
    for c in ErrorCode::ALL {
        s.push_str(&format!(
            "  \"{}\": \"{}\",\n",
            c.as_str(),
            js_escape(c.zh())
        ));
    }
    s.push_str("};\n\n");

    s.push_str("// 按错误码取中文说明（表里没有就原样返回错误码，不吞掉）\n");
    s.push_str("function RegValErrorText(code) {\n");
    s.push_str("  var c = RegValAsText(code).replace(/^\\s+|\\s+$/g, \"\").toUpperCase();\n");
    s.push_str("  return Object.prototype.hasOwnProperty.call(REGVAL_ERROR_ZH, c) ? REGVAL_ERROR_ZH[c] : code;\n");
    s.push_str("}\n\n");

    s.push_str("// 文案占位符替换（{rows} 这类标记换成实值）\n");
    s.push_str("function RegValFmt(template, key, value) {\n");
    s.push_str("  return String(template).split(\"{\" + key + \"}\").join(value);\n");
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文案键不重复() {
        let mut seen = std::collections::BTreeSet::new();
        for m in MESSAGES {
            assert!(seen.insert(m.key), "文案键重复：{}", m.key);
        }
    }

    #[test]
    fn 文案键都能取到且非空() {
        for m in MESSAGES {
            assert_eq!(text(m.key), m.text, "文案键取不到：{}", m.key);
            assert!(!m.text.is_empty(), "文案不能为空：{}", m.key);
        }
        assert_eq!(text("没有这个键"), "");
    }

    #[test]
    fn 错误码中文说明取自错误码体系_不另抄一份() {
        for c in ErrorCode::ALL {
            assert_eq!(
                error_text(c.as_str()),
                Some(c.zh()),
                "错误码 {} 的中文说明与 ErrorCode::zh() 不一致",
                c.as_str()
            );
        }
        assert_eq!(error_text("NOT_A_REAL_CODE"), None);
        assert_eq!(error_text_for("NOT_A_REAL_CODE"), "");
    }

    #[test]
    fn 占位符替换_认不出的键原样留着() {
        assert_eq!(
            format(EMPTY_ISSUES, &[("rows", "12")]),
            "已校验 12 行 · 未发现口径问题"
        );
        assert_eq!(
            format(EMPTY_ISSUES, &[("nope", "12")]),
            "已校验 {rows} 行 · 未发现口径问题",
            "占位符对不上要看得见，不能静默吞掉"
        );
    }

    #[test]
    fn 错误行_错误码不在表里就只带错误码() {
        assert_eq!(
            error_line("CONTRACT_VERSION_MISMATCH", "引擎与宏版本不一致，请升级"),
            "错误码：CONTRACT_VERSION_MISMATCH · 引擎与宏版本不一致，请升级"
        );
        assert_eq!(
            error_line("SOMETHING_NEW", ""),
            "错误码：SOMETHING_NEW",
            "不认识的中文说明不要编"
        );
    }

    #[test]
    fn 生成的宏模块里每个错误码都有常量() {
        let vba = render_vba_module();
        for c in ErrorCode::ALL {
            assert!(
                vba.contains(&format!("Public Const {} As String", vba_error_const_name(c.as_str()))),
                "宏模块里缺错误码常量：{}",
                c.as_str()
            );
            assert!(
                vba.contains(&format!("Case \"{}\"", c.as_str())),
                "宏模块的查表函数里缺错误码：{}",
                c.as_str()
            );
        }
    }

    #[test]
    fn 生成的宏模块里每条文案都有常量() {
        let vba = render_vba_module();
        for m in MESSAGES {
            assert!(
                vba.contains(&format!("Public Const {} As String", vba_const_name(m.key))),
                "宏模块里缺文案常量：{}",
                m.key
            );
        }
    }

    #[test]
    fn jsa文案块与vba常量块同源() {
        let jsa = render_jsa_module();
        let vba = render_vba_module();

        for m in MESSAGES {
            assert!(
                jsa.contains(&format!("\"{}\": \"{}\"", m.key, m.text)),
                "JSA 文案块里缺文案：{}",
                m.key
            );
            assert!(
                vba.contains(&format!(
                    "Public Const {} As String = \"{}\"",
                    vba_const_name(m.key),
                    m.text.replace('"', "\"\"")
                )),
                "VBA 常量块里缺文案：{}",
                m.key
            );
        }

        for c in ErrorCode::ALL {
            assert!(
                jsa.contains(&format!("\"{}\": \"{}\"", c.as_str(), c.zh())),
                "JSA 文案块里缺错误码中文说明：{}",
                c.as_str()
            );
        }

        // 两侧的查表与占位符函数同名同义
        assert!(jsa.contains("function RegValErrorText(code)"));
        assert!(jsa.contains("function RegValFmt(template, key, value)"));
        assert!(vba.contains("Public Function RegValErrorText("));
        assert!(vba.contains("Public Function RegValFmt("));
    }

    #[test]
    fn jsa文案块里的字符串都转义了() {
        let jsa = render_jsa_module();
        // 每条文案都应能原样取回（转义后再解析回来还是那句话）
        for m in MESSAGES {
            let needle = format!("\"{}\": \"{}\"", m.key, js_escape(m.text));
            assert!(
                jsa.contains(&needle),
                "JSA 文案块里 {} 的转义不对",
                m.key
            );
        }
    }
}
