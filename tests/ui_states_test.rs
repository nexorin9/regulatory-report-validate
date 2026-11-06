//! 表格内插件的三态与提示层级（UI-3）。
//!
//! 这一组用例守的是「表里那三句话」：
//!
//! 1. **加载态**：调引擎之前先把「校验中…」写上状态条并按住按钮 ——
//!    宏阻塞执行引擎时界面不刷新，用户面对「按了没反应」只会再点一次。
//! 2. **空结果态**：两种空要分得开 ——「没有行」与「没有问题」不是同一句话。
//! 3. **错误态**：引擎退出码非 0 时状态条写「错误码 + 中文说明」，
//!    中文说明与 `src/errors.rs` 的 `ErrorCode::zh()` 逐条一致。
//! 4. **提示层级**：结果级问题用单元格填充 + 状态条摘要（不弹框），
//!    阻断级错误才用对话框 —— 同一件事不叠三种提示。
//! 5. **文案统一**：宏里面向用户的中文一个字都不写死，全部取自
//!    `RegValMessages.bas`（由 `src/ui/messages.rs` 生成）。
//!
//! 宏是 VBA，跑不了单元测试，所以分两头验收：**源码层面**核对宏与引擎的口径，
//! **行为层面**用引擎真跑三态，看状态条上那句话到底长什么样。

use std::path::{Path, PathBuf};
use std::process::Command;

use regval::contract::Contract;
use regval::errors::ErrorCode;
use regval::ui::{messages, statusbar};

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

fn messages_module() -> String {
    let path = project_dir().join("macro/vba/RegValMessages.bas");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到宏常量模块 {}：{}", path.display(), e))
}

/// 取出某个子过程 / 函数的正文（`Public Sub X(...)`、`Private Function X(...)` 都算）。
fn proc_body(source: &str, name: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            let rest = ["Public Sub ", "Private Sub ", "Public Function ", "Private Function "]
                .iter()
                .find_map(|kw| t.strip_prefix(kw).map(|r| r.to_string()));
            let rest = match rest {
                Some(r) => r,
                None => return false,
            };
            let head = rest.split('(').next().unwrap_or("").trim();
            head == name
        })
        .unwrap_or_else(|| panic!("宏里找不到子过程 {}", name));

    let mut body = String::new();
    for line in &lines[start..] {
        let t = line.trim();
        if t == "End Sub" || t == "End Function" {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    body
}

/// 去掉 VBA 注释（`'` 起的部分，但引号里的不算注释）。
fn strip_vba_comment(line: &str) -> &str {
    let bytes: Vec<char> = line.chars().collect();
    let mut in_string = false;
    for (i, ch) in bytes.iter().enumerate() {
        if *ch == '"' {
            in_string = !in_string;
        } else if *ch == '\'' && !in_string {
            let byte_idx: usize = bytes[..i].iter().map(|c| c.len_utf8()).sum();
            return &line[..byte_idx];
        }
    }
    line
}

/// 一行里的字符串字面量（去注释后）。
fn string_literals(line: &str) -> Vec<String> {
    let code = strip_vba_comment(line);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_string = false;
    let mut chars = code.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_string = false;
                    out.push(std::mem::take(&mut cur));
                }
            } else {
                cur.push(ch);
            }
        } else if ch == '"' {
            in_string = true;
            cur.clear();
        }
    }
    out
}

fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

// ─────────────────────────────────────────────────────────────────────
// 0. 文案唯一出处：生成物与引擎逐字一致
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 生成的文案常量模块与引擎逐字一致() {
    let generated = messages::render_vba_module();
    let on_disk = messages_module();
    assert_eq!(
        generated, on_disk,
        "macro/vba/RegValMessages.bas 与引擎生成物不一致 —— \
         跑 cargo run -- messages --vba --out macro/vba/RegValMessages.bas 重新生成"
    );
}

#[test]
fn 每个错误码的中文说明逐条对应() {
    let module = messages_module();
    for code in ErrorCode::ALL {
        let const_line = format!(
            "Public Const {} As String = \"{}\"",
            messages::vba_error_const_name(code.as_str()),
            code.zh()
        );
        assert!(
            module.contains(&const_line),
            "宏常量模块里 {} 的中文说明与 src/errors.rs 不一致",
            code.as_str()
        );
    }
    // 查表函数也把每个错误码都覆盖了（不在表里的原样返回错误码，不吞掉）
    for code in ErrorCode::ALL {
        assert!(
            module.contains(&format!("Case \"{}\"", code.as_str())),
            "宏的查表函数里缺错误码：{}",
            code.as_str()
        );
    }
}

#[test]
fn 契约版本不一致的中文说明就是那一句() {
    let module = messages_module();
    assert!(module.contains(
        "Public Const REGVAL_ERR_CONTRACT_VERSION_MISMATCH As String = \"引擎与宏版本不一致，请升级\""
    ));
    assert_eq!(
        ErrorCode::ContractVersionMismatch.zh(),
        "引擎与宏版本不一致，请升级"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 1. 加载态
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 加载态_调引擎之前先写状态条() {
    let source = macro_source();
    let begin = proc_body(&source, "BeginRun");
    assert!(
        begin.contains("SetStatusLine"),
        "开跑前应先把状态条写上"
    );
    // 加载态的文案由调用处传入，取自文案常量表
    let validate = proc_body(&source, "RegVal_Validate");
    assert!(
        validate.contains("BeginRun ws, REGVAL_MSG_LOADING"),
        "校验按钮应在调引擎前进入加载态，文案取自文案常量表"
    );
    let push = proc_body(&source, "RegVal_Push");
    assert!(
        push.contains("BeginRun ws, REGVAL_MSG_LOADING_PUSH"),
        "上报按钮应在调引擎前进入加载态，文案取自文案常量表"
    );
    assert_eq!(
        statusbar::LOADING_LINE, "校验中…",
        "加载态那句话要与引擎侧一致"
    );
    // 顺序：先写状态条，再禁用按钮，最后才是引擎
    let status_at = begin.find("SetStatusLine").expect("应有 SetStatusLine");
    let busy_at = begin.find("SetButtonsEnabled").expect("应有 SetButtonsEnabled");
    assert!(
        status_at < busy_at,
        "先写状态条再按住按钮：按钮先灰了、状态条还没写，用户看到的还是「没反应」"
    );
}

#[test]
fn 加载态_按钮被按住_宏有防重复提交的守卫() {
    let source = macro_source();
    let enable = proc_body(&source, "SetButtonsEnabled");
    assert!(
        enable.contains("SetOneButtonEnabled"),
        "两个按钮都要能被按住"
    );
    let one = proc_body(&source, "SetOneButtonEnabled");
    assert!(
        one.contains("shp.OnAction = \"\"") || one.contains("shp.OnAction = \"\""),
        "按住按钮的写法应是解掉 OnAction（点了没反应）"
    );
    assert!(
        one.contains("shp.OnAction = actionName"),
        "放开按钮时要把 OnAction 绑回去"
    );

    // 两个入口都要有「上一轮没跑完就别再点」的守卫
    for entry in ["RegVal_Validate", "RegVal_Push"] {
        let body = proc_body(&source, entry);
        assert!(
            body.contains("If gBusy Then"),
            "{} 应有重复提交守卫",
            entry
        );
        assert!(
            body.contains("REGVAL_MSG_BUSY"),
            "{} 拦下重复提交时要给一句提示",
            entry
        );
    }
}

#[test]
fn 加载态_引擎跑完要把按钮放开() {
    let source = macro_source();
    let end = proc_body(&source, "EndRun");
    assert!(end.contains("gBusy = False"), "跑完要清掉忙标记");
    assert!(
        end.contains("SetButtonsEnabled") && end.contains("True"),
        "跑完要把按钮放开"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 2. 空结果态：两种空要分得开
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 空结果态_零问题是未发现口径问题() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let contract = run_validate(&fixture("clean.csv"), &out, &dir.path().join("state.db"));

    assert_eq!(contract.summary.issue_count, 0, "样例表本身不该有口径问题");
    assert_eq!(
        contract.summary.status_line, "已校验 5 行 · 未发现口径问题",
        "零问题要说「未发现口径问题」，不是「0 项口径问题」"
    );
    assert_eq!(contract.summary.total_rows, 5);
}

#[test]
fn 空结果态_没有行是另一句空() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let contract = run_validate(
        &fixture("header_only.csv"),
        &out,
        &dir.path().join("state.db"),
    );

    assert_eq!(contract.summary.total_rows, 0);
    assert_eq!(
        contract.summary.status_line, "表里没有数据行 · 未做校验",
        "没有行要说「没有数据行」，不能与「没有问题」混成一句"
    );
    assert_ne!(
        contract.summary.status_line, "已校验 5 行 · 未发现口径问题",
        "两种空必须能一眼分开"
    );
}

#[test]
fn 空结果态_引擎侧与宏侧是同一句话() {
    assert_eq!(
        statusbar::status_line(5, 0, 0),
        "已校验 5 行 · 未发现口径问题"
    );
    assert_eq!(statusbar::status_line(0, 0, 0), "表里没有数据行 · 未做校验");
    assert_eq!(
        statusbar::status_line(6, 2, 0),
        "已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认"
    );
}

#[test]
fn 空结果态_宏不自行判断空_只搬引擎算好的那句话() {
    let source = macro_source();
    let body = proc_body(&source, "RegVal_Validate");
    assert!(
        body.contains("\"status_line\""),
        "宏应读引擎算好的状态条句子"
    );
    assert!(
        !body.contains("未发现口径问题") && !body.contains("没有数据行"),
        "宏里不该自己判断「空」并拼句子 —— 空不空由引擎说了算"
    );
    // 宏里没有行时也要说话：状态条要落到当前表上，不能因为没东西可写就不说
    let apply = proc_body(&source, "ApplyStatusLineToWrittenSheets");
    assert!(
        apply.contains("ActiveSheet"),
        "一行都没写回时（空表），状态条仍要落到当前表上"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 3. 错误态
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 错误态_契约版本不符时引擎非零退出并写出错误报告() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let err_out = dir.path().join("error.json");
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("mini.csv"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(dir.path().join("state.db"))
        .arg("--expect-schema-version")
        .arg("regval.contract/9.9.9")
        .arg("--error-out")
        .arg(&err_out)
        .output()
        .expect("regval 应可执行");

    assert!(
        !output.status.success(),
        "契约版本不符必须非 0 退出（宏据此中止写回）"
    );
    assert!(
        !out.exists(),
        "版本不符时不该产出契约文件（不做部分写回）"
    );

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&err_out).expect("应有错误报告"))
            .expect("错误报告应是 JSON");
    assert_eq!(report["code"], "CONTRACT_VERSION_MISMATCH");
    assert_eq!(report["code_zh"], "引擎与宏版本不一致，请升级");
    assert_eq!(report["code_zh"], ErrorCode::ContractVersionMismatch.zh());
}

#[test]
fn 错误态_状态条写错误码加中文说明() {
    let source = macro_source();
    let status = proc_body(&source, "ErrorStatusText");
    assert!(
        status.contains("REGVAL_MSG_ERROR_LINE"),
        "错误行应取自文案常量表"
    );
    assert!(
        status.contains("RegValFmt"),
        "错误行要把错误码与中文说明填进模板"
    );

    // 引擎侧算出来的错误行与宏侧模板拼出来的是同一句
    let engine_line = statusbar::error_line(
        "CONTRACT_VERSION_MISMATCH",
        ErrorCode::ContractVersionMismatch.zh(),
    );
    assert_eq!(
        engine_line,
        "错误码：CONTRACT_VERSION_MISMATCH · 引擎与宏版本不一致，请升级"
    );
    let template = messages::ERROR_LINE;
    let macro_line = template
        .replace("{code}", "CONTRACT_VERSION_MISMATCH")
        .replace("{code_zh}", ErrorCode::ContractVersionMismatch.zh());
    assert_eq!(engine_line, macro_line, "宏侧与引擎侧的错误行必须逐字相同");
}

#[test]
fn 错误态_不吞异常_错误报告四个字段都读() {
    let source = macro_source();
    let read = proc_body(&source, "ReadErrorReport");
    for key in ["\"code\"", "\"code_zh\"", "\"message\"", "\"advice\""] {
        assert!(read.contains(key), "错误报告的 {} 应被读到", key);
    }
    // 中文说明优先取本地表（与 src/errors.rs 同源），本地没有才退回引擎给的那句
    assert!(
        read.contains("RegValErrorText"),
        "中文说明应查本地的错误码表"
    );
    assert!(
        read.contains("NO_ERROR_REPORT"),
        "引擎没留错误报告时要有一句兜底，不能静默"
    );
}

#[test]
fn 错误态_引擎没产出契约也要报错不静默() {
    let source = macro_source();
    let body = proc_body(&source, "RunValidateAndWriteBack");
    assert!(
        body.contains("CONTRACT_NOT_PRODUCED"),
        "引擎跑完没产出契约要报错"
    );
    assert!(
        body.contains("SetErrorInfo"),
        "本地闸门也要走同一套错误信息"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 4. 提示层级：同一件事不叠三种提示
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 提示层级_结果级不弹框_阻断级错误才弹() {
    let source = macro_source();

    // 结果级：只填单元格 + 状态条摘要
    assert!(
        source.contains("Private Const HINT_RESULT_FILL As Boolean = True"),
        "结果级要有单元格填充"
    );
    assert!(
        source.contains("Private Const HINT_RESULT_DIALOG As Boolean = False"),
        "结果级不弹框"
    );
    assert!(
        source.contains("Private Const HINT_ERROR_FILL As Boolean = False"),
        "阻断级错误不碰单元格"
    );
    assert!(
        source.contains("Private Const HINT_ERROR_DIALOG As Boolean = True"),
        "阻断级错误才弹框"
    );

    // 两个入口的正文里不能有直接的 MsgBox：弹框只能走那两个具名子过程
    for entry in ["RegVal_Validate", "RegVal_Push"] {
        let body = proc_body(&source, entry);
        assert!(
            !body.contains("MsgBox"),
            "{} 的正文里不该有 MsgBox —— 结果级问题用填充 + 状态条就够了，\
             弹框只留给阻断级错误与未保存确认",
            entry
        );
        assert!(
            body.contains("ShowErrorDialog"),
            "{} 遇到阻断级错误要调 ShowErrorDialog",
            entry
        );
    }
}

#[test]
fn 提示层级_错误态走对话框且不写回单元格() {
    let source = macro_source();
    let write_back = proc_body(&source, "WriteBackNewColumns");
    assert!(
        write_back.contains("HINT_RESULT_FILL"),
        "写回后的填色受提示层级控制"
    );

    let dialog = proc_body(&source, "ShowErrorDialog");
    assert!(dialog.contains("MsgBox"), "阻断级错误要弹框");
    assert!(dialog.contains("ErrorStatusText"), "弹框里要带上错误码与中文说明");
    assert!(
        dialog.contains("REGVAL_MSG_ADVICE_PREFIX"),
        "弹框里要带上处理建议"
    );

    // 错误态写状态条，但不碰单元格
    let write_err = proc_body(&source, "WriteErrorStatusLine");
    assert!(write_err.contains("SetStatusLine"), "错误态要写状态条");
    assert!(
        !write_err.contains("ApplyResultColumnStyles"),
        "错误态不填单元格（数据还没写回，没什么可填）"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 5. 文案统一：宏里不写死面向用户的中文
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 文案统一_宏里没有写死的中文提示() {
    let source = macro_source();
    let module = messages_module();

    let mut offenders: Vec<String> = Vec::new();
    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        // 契约列名常量是例外：它与 src/contract.rs 对齐，是契约的一部分
        if trimmed.starts_with("Public Const REGVAL_COLUMN_NAMES")
            || trimmed.starts_with("Public Const REGVAL_SCHEMA_VERSION")
            || trimmed.starts_with("Public Const REGVAL_ENGINE_PATH")
            || trimmed.starts_with("Public Const REGVAL_STATE_DB")
        {
            continue;
        }
        for literal in string_literals(line) {
            if has_cjk(&literal) {
                offenders.push(format!("第 {} 行：{}", idx + 1, literal));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "宏里不该写死面向用户的中文（都取自 RegValMessages.bas）：\n{}",
        offenders.join("\n")
    );

    // 宏引用的每个 REGVAL_MSG_* / REGVAL_ERR_* 常量都要在生成模块里存在
    for line in source.lines() {
        for literal in string_literals(line) {
            if literal.starts_with("REGVAL_MSG_") || literal.starts_with("REGVAL_ERR_") {
                assert!(
                    module.contains(&format!("Public Const {} As String", literal)),
                    "宏引用了文案常量 {}，但生成模块里没有它",
                    literal
                );
            }
        }
    }
}

#[test]
fn 文案统一_宏引用的样式与文案函数都在生成模块里() {
    let source = macro_source();
    let module = messages_module();
    for fname in ["RegValErrorText", "RegValFmt"] {
        assert!(
            source.contains(fname),
            "宏应调用生成模块里的 {}",
            fname
        );
        assert!(
            module.contains(&format!("Function {}", fname)),
            "生成模块里应定义 {}",
            fname
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// 6. 三态各有可复现的触发样例与截图
// ─────────────────────────────────────────────────────────────────────

#[test]
fn 三态各有可复现的触发样例() {
    // 正常
    let dir = tempfile::tempdir().unwrap();
    let contract = run_validate(
        &fixture("mini.csv"),
        &dir.path().join("contract.json"),
        &dir.path().join("state.db"),
    );
    assert_eq!(
        contract.summary.status_line,
        "已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认"
    );
    assert_eq!(contract.summary.issue_count, 2);

    // 空结果（零问题）
    let dir2 = tempfile::tempdir().unwrap();
    let clean = run_validate(
        &fixture("clean.csv"),
        &dir2.path().join("contract.json"),
        &dir2.path().join("state.db"),
    );
    assert_eq!(clean.summary.status_line, "已校验 5 行 · 未发现口径问题");

    // 没有行
    let dir3 = tempfile::tempdir().unwrap();
    let empty = run_validate(
        &fixture("header_only.csv"),
        &dir3.path().join("contract.json"),
        &dir3.path().join("state.db"),
    );
    assert_eq!(empty.summary.status_line, "表里没有数据行 · 未做校验");

    // 契约版本不符（见上面那条用例）
}

#[test]
fn 三态各有截图() {
    let shots = project_dir().join("docs/screenshots");
    for name in [
        "ui-state-normal.png",
        "ui-state-empty.png",
        "ui-state-version-mismatch.png",
    ] {
        let path = shots.join(name);
        assert!(path.exists(), "缺截图：{}", path.display());
        let size = std::fs::metadata(&path).expect("截图应有大小").len();
        assert!(size > 5_000, "截图太小，可能不是真实渲染：{}", path.display());
    }

    // 截图由脚本生成，脚本要在仓里（可重跑）
    let script = project_dir().join("scripts/capture_ui_states.py");
    assert!(
        script.exists(),
        "截图采集脚本应在仓里（可重跑）：scripts/capture_ui_states.py"
    );
    let text = std::fs::read_to_string(&script).expect("脚本应可读");
    assert!(
        text.contains("ui-state-version-mismatch.png"),
        "脚本应产出三态截图"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 辅助
// ─────────────────────────────────────────────────────────────────────

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
