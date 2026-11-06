//! WPS JSA 宏的装配：把手写模板与引擎侧的契约常量、token、文案拼成一份
//! **可独立加载**的宏文件。
//!
//! # 为什么要拼
//!
//! WPS JSA 的宏是一个单独加载的 `.js` 文件，运行时没有模块加载器可依赖
//! （不像 Node 有 `require`）。所以 token 与文案常量必须**内联**在这一个文件里。
//!
//! 但内联不等于手抄：常量仍然只在 [`crate::ui::tokens`] 与
//! [`crate::ui::messages`] 里定义一次，由本模块注入到手写模板的标记区里。
//! 改样式、改文案各改一处，Excel 侧（`regval tokens --vba`）与 WPS 侧同时跟着变。
//!
//! # 三入口同构
//!
//! 引擎的结果契约 JSON 是三条入口（Excel 宏 / WPS JSA 宏 / CSV 直跑）唯一的
//! 交接面。契约版本与 5 个新列名也由本模块从 [`crate::contract`] 注入，
//! 所以 WPS 侧不可能与引擎的契约漂移 —— 版本闸门拦的是「引擎二进制比宏旧」
//! 或「契约文件是上一轮留下的」这两种真实场景。

use crate::contract::{NEW_COLUMNS, SCHEMA_VERSION};

/// 契约常量注入区（模板里的起止标记）。
pub const CONTRACT_BEGIN: &str = "// @regval:begin contract";
pub const CONTRACT_END: &str = "// @regval:end contract";
/// 视觉 token 注入区。
pub const TOKENS_BEGIN: &str = "// @regval:begin tokens";
pub const TOKENS_END: &str = "// @regval:end tokens";
/// 提示文案注入区。
pub const MESSAGES_BEGIN: &str = "// @regval:begin messages";
pub const MESSAGES_END: &str = "// @regval:end messages";

/// 契约常量块：契约版本与 5 个新列名（与引擎同源，宏侧不可能漂移）。
pub fn contract_block() -> String {
    format!(
        "// 契约常量（与引擎 src/contract.rs 同源；改动须同步升 schema_version）\nvar REGVAL_SCHEMA_VERSION = \"{}\";\nvar REGVAL_COLUMN_NAMES = \"{}\";\n",
        SCHEMA_VERSION,
        NEW_COLUMNS.join("|")
    )
}

/// 把模板装配成可加载的 WPS 宏。
///
/// 模板里必须有三对标记（`contract` / `tokens` / `messages`）；缺任何一对
/// 都直接报错 —— 少注入一块，宏就会带着半套常量去写回，静默跑偏比报错更糟。
pub fn assemble(template: &str) -> Result<String, String> {
    let mut out = template.to_string();
    out = inject(&out, CONTRACT_BEGIN, CONTRACT_END, &contract_block())?;
    out = inject(
        &out,
        TOKENS_BEGIN,
        TOKENS_END,
        &crate::ui::tokens::render_jsa_module(),
    )?;
    out = inject(
        &out,
        MESSAGES_BEGIN,
        MESSAGES_END,
        &crate::ui::messages::render_jsa_module(),
    )?;
    Ok(out)
}

/// 用 `block` 替换 `begin` 与 `end` 两个标记之间的内容（标记本身保留）。
fn inject(text: &str, begin: &str, end: &str, block: &str) -> Result<String, String> {
    let start = text
        .find(begin)
        .ok_or_else(|| format!("模板里找不到注入起点标记：{}", begin))?
        + begin.len();
    let end_pos = text[start..]
        .find(end)
        .ok_or_else(|| format!("模板里找不到注入终点标记：{}", end))?
        + start;

    let mut out = String::with_capacity(text.len() + block.len());
    out.push_str(&text[..start]);
    out.push('\n');
    out.push_str(block);
    if !block.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&text[end_pos..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 三块都注进去了() {
        let template = format!(
            "// head\n{}\n// mid\n{}\n// tail\n{}\n",
            CONTRACT_BEGIN, TOKENS_BEGIN, MESSAGES_BEGIN
        );
        let template = template.replace(CONTRACT_BEGIN, &format!("{}\n{}", CONTRACT_BEGIN, CONTRACT_END));
        let template = template.replace(TOKENS_BEGIN, &format!("{}\n{}", TOKENS_BEGIN, TOKENS_END));
        let template = template.replace(MESSAGES_BEGIN, &format!("{}\n{}", MESSAGES_BEGIN, MESSAGES_END));

        let out = assemble(&template).expect("装配应成功");
        assert!(out.contains(&format!("var REGVAL_SCHEMA_VERSION = \"{}\"", SCHEMA_VERSION)));
        assert!(out.contains(&format!("var REGVAL_COLUMN_NAMES = \"{}\"", NEW_COLUMNS.join("|"))));
        assert!(out.contains("var REGVAL_TOKENS = {"));
        assert!(out.contains("var REGVAL_MESSAGES = {"));
        // 标记本身保留，便于下次重新装配时仍能定位
        assert!(out.contains(CONTRACT_BEGIN) && out.contains(TOKENS_END));
    }

    #[test]
    fn 缺标记时报错而不是静默少注入一块() {
        let err = assemble("// 没有标记的模板").unwrap_err();
        assert!(err.contains(CONTRACT_BEGIN), "报错要点名缺的标记：{}", err);
    }

    #[test]
    fn 契约常量与引擎一致() {
        let block = contract_block();
        assert!(block.contains(SCHEMA_VERSION));
        for name in NEW_COLUMNS {
            assert!(block.contains(name), "契约常量块少了列名 {}", name);
        }
    }
}
