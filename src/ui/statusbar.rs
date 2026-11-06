//! 状态条文案 —— 表格插件与命令行共用的同一句话。
//!
//! # 这一层为什么存在
//!
//! 状态条上有三句话：「还没校验时的一句」「校验完的一句」「上报完的一句」。
//! 这三句话如果在宏里拼、在命令行里再拼一遍，迟早会各说各的 ——
//! 表里写着「已校验 200 行 · 12 项口径问题」，终端里写着「已校验 200 行 · 10 项口径问题」，
//! 经办人该信哪个？没人说得清。
//!
//! 所以句子的**唯一出处**在这里：
//!
//! - 引擎把成品句子写进结果契约的 `summary.status_line` / `summary.submission_line`；
//! - 表格插件（Excel 宏 / WPS JSA）把这两句**原样**写进表里的状态条 ——
//!   宏不自行统计、不自行拼句子；
//! - 命令行用同一批函数打印，两边不会各说一套。
//!
//! 句式取自 `docs/concepts/visual-goal.md` 的视觉目标卡（起始态 / 首个可用瞬间 / 成形态）。

/// 状态条与表格之间的呼吸区：数据末行往下空几行再写状态条。
///
/// 留白是为了让状态条不贴着最后一行数据 —— 视觉目标卡要求的「8px 呼吸区」。
pub const STATUS_ROW_GAP: usize = 2;

/// 还没校验时状态条上写的话（静态文案，插件直接用）。
///
/// 这一句不来自任何统计结果，所以它**不是**契约里的字段 ——
/// 插件在装好按钮时先写上这句话，等第一次校验跑完再被覆盖。
pub const IDLE_LINE: &str = "本机校验库：未加载 — 点「校验」开始";

/// 加载态：引擎在跑的那一瞬间状态条上写的话。
///
/// 宏在调引擎**之前**就把这句写上，引擎跑完再覆盖。
/// 宏阻塞执行引擎时界面是不刷新的，所以这句话必须先写、先刷出来 ——
/// 否则用户面对的是「按了没反应」，那不叫加载态，叫卡死。
pub const LOADING_LINE: &str = crate::ui::messages::LOADING;

/// 加载态：上报在跑。
pub const LOADING_PUSH_LINE: &str = crate::ui::messages::LOADING_PUSH;

/// 校验完成后的状态条文案。
///
/// 三个数都取自契约的 `summary` 段：校验行数、口径问题项数、疑似重复候选对数。
///
/// 三档说法（**两种空要分得开**，不能都写成「0 项」）：
///
/// | 情形 | 状态条 |
/// |------|--------|
/// | 表里一行都没有 | 「表里没有数据行 · 未做校验」 |
/// | 校验过了、一项问题都没发现 | 「已校验 N 行 · 未发现口径问题」 |
/// | 有问题或疑似重复 | 「已校验 N 行 · M 项口径问题 · K 项疑似重复待确认」 |
pub fn status_line(rows: usize, issues: usize, duplicates: usize) -> String {
    if rows == 0 {
        return crate::ui::messages::EMPTY_ROWS.to_string();
    }
    if issues == 0 && duplicates == 0 {
        return crate::ui::messages::format(
            crate::ui::messages::EMPTY_ISSUES,
            &[("rows", &rows.to_string())],
        );
    }
    format!(
        "已校验 {} 行 · {} 项口径问题 · {} 项疑似重复待确认",
        rows, issues, duplicates
    )
}

/// 错误态：状态条上写的那一句（错误码 + 中文说明）。
///
/// 中文说明取自 [`crate::ui::messages::error_text`]（即 `ErrorCode::zh()`），
/// 与宏侧的 `RegValErrorText` 查的是同一张表 —— 表里与终端里不会各说一套。
pub fn error_line(code: &str, code_zh: &str) -> String {
    crate::ui::messages::error_line(code, code_zh)
}

/// 上报完成后的状态条文案。
///
/// `submitted_rows` 是**回执驱动**的行数（网关已受理 + 幂等命中）——
/// 不是「本轮推了几行」，也不是「本地以为推了几行」。
pub fn submission_line(submitted_rows: usize) -> String {
    format!(
        "本批 {} 行已提交 · 幂等键见状态库 · 回执已回填",
        submitted_rows
    )
}

/// 这一行算不算「已报上去」。
///
/// 判定规则在 `src/push/status.rs`（**唯一出处**）—— 宏侧状态条、未上报名单、
/// 界面三态都调那一处，避免「状态条说报过了、清单里还挂着待办」。
pub fn is_submitted(status: &str) -> bool {
    crate::push::status::is_submitted(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 校验状态条按三个数出句子() {
        assert_eq!(
            status_line(200, 12, 5),
            "已校验 200 行 · 12 项口径问题 · 5 项疑似重复待确认"
        );
    }

    #[test]
    fn 零问题是空结果态_说清校验了多少行() {
        assert_eq!(
            status_line(6, 0, 0),
            "已校验 6 行 · 未发现口径问题",
            "零问题要说「未发现口径问题」，不是「0 项口径问题」"
        );
    }

    #[test]
    fn 没有行与没有问题是两种空_说法不能一样() {
        let no_rows = status_line(0, 0, 0);
        let no_issues = status_line(6, 0, 0);
        assert_eq!(no_rows, "表里没有数据行 · 未做校验");
        assert_ne!(
            no_rows, no_issues,
            "「没有行」与「没有问题」必须能一眼分开"
        );
    }

    #[test]
    fn 有疑似重复时不算空_仍把重复数说出来() {
        assert_eq!(
            status_line(6, 0, 2),
            "已校验 6 行 · 0 项口径问题 · 2 项疑似重复待确认"
        );
    }

    #[test]
    fn 加载态与错误态各有各的一句话() {
        assert_eq!(LOADING_LINE, "校验中…");
        assert_eq!(LOADING_PUSH_LINE, "上报中…");
        assert_eq!(
            error_line("CONTRACT_VERSION_MISMATCH", "引擎与宏版本不一致，请升级"),
            "错误码：CONTRACT_VERSION_MISMATCH · 引擎与宏版本不一致，请升级"
        );
    }

    #[test]
    fn 上报状态条按回执行数出句子() {
        assert_eq!(
            submission_line(183),
            "本批 183 行已提交 · 幂等键见状态库 · 回执已回填"
        );
    }

    #[test]
    fn 起始态文案是静态的一句() {
        assert_eq!(IDLE_LINE, "本机校验库：未加载 — 点「校验」开始");
    }

    #[test]
    fn 已上报与幂等命中都算已提交() {
        assert!(is_submitted(crate::push::status::ACCEPTED));
        assert!(is_submitted(crate::push::status::IDEMPOTENT_HIT));
        assert!(!is_submitted(crate::push::status::REJECTED));
        assert!(!is_submitted(crate::push::status::RETRYABLE));
        assert!(!is_submitted(""));
    }
}
