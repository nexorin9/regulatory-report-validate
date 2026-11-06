//! 行级上报状态取值 —— **单一出处**。
//!
//! 同一串字面量同时出现在五处：本机库 `submissions.status`、行级「上报状态」列、
//! 幂等判定（上次到底成没成）、待办清单、导出产物。
//!
//! **两处各写一份**就会出这种静默跳过：本机库写着「待重试」，
//! 编译计划时却按「已上报」处理 → 那一行永远不重推，经办人也催不动。
//! 所以状态取值只在这里定义一次，别处一律引用。

/// 网关受理 —— 这一行已上报，幂等键已生效
pub const ACCEPTED: &str = "已上报";
/// 幂等命中 —— 这一行此前已上报过，本轮不重复推送
pub const IDEMPOTENT_HIT: &str = "幂等命中（已上报，不重复推送）";
/// 网关退回（4xx）—— 不可重试，改数据才能再报
pub const REJECTED: &str = "被退回";
/// 可重试的失败（5xx / 超时 / 连不上）—— 上次没报上去，原样可以再试
pub const RETRYABLE: &str = "待重试";
/// 本行数据不合格，请求根本没发出去
pub const BLOCKED: &str = "未发出（数据不合格）";
/// 回执不可用（200 但结构不合法）—— 不当成功
pub const INVALID_RECEIPT: &str = "回执待人工核对";
/// 因前一步失败而未执行
///
/// 说明写「前面的步骤没成功」而不是「上一批未执行」—— 上报是**同一批内**串行的：
/// 第 k 步失败，第 k+1..N 步就停在这儿。写「上一批」会让人以为是上一份表的事。
pub const NOT_EXECUTED: &str = "未执行（前面的步骤没成功）";

/// 这一行是不是「网关已经受理了」。
///
/// 受理过的行**不重复推送** —— 幂等键在网关侧已经生效。
pub fn is_accepted(status: &str) -> bool {
    status == ACCEPTED
}

/// 这一行是不是「上次没报上去、原样可以再试」。
///
/// 5xx / 超时 / 连不上属于这一类：网关**没有**受理这一行，
/// 原样再推一次是安全的（网关侧若其实已受理，会返回幂等命中，不会变成两条上报）。
pub fn is_retryable(status: &str) -> bool {
    status == RETRYABLE
}

/// 这一行是不是「网关退回了」（不可重试，改数据才能再报）。
pub fn is_rejected(status: &str) -> bool {
    status == REJECTED
}

/// 这一行算不算「已经报上去了」。
///
/// 网关受理（`已上报`）与幂等命中（`幂等命中（已上报，不重复推送）`）都算 ——
/// 幂等命中说明这一行**确实**报过，只是本轮没再推。
///
/// 状态串后面可能带补充说明，所以按前缀判。
///
/// **唯一出处**：宏侧状态条、未上报名单、界面三态都调这里 ——
/// 各写一份「已上报」的判定，迟早出现「状态条说报过了、清单里还挂着待办」。
pub fn is_submitted(status: &str) -> bool {
    let trimmed = status.trim();
    trimmed.starts_with(ACCEPTED) || trimmed.starts_with(IDEMPOTENT_HIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 状态判定各归各的() {
        assert!(is_accepted(ACCEPTED));
        assert!(!is_accepted(RETRYABLE));
        assert!(is_retryable(RETRYABLE));
        assert!(!is_retryable(ACCEPTED));
        assert!(is_rejected(REJECTED));
        assert!(!is_rejected(RETRYABLE));
    }

    #[test]
    fn 已上报判定含幂等命中且不含待办状态() {
        assert!(is_submitted(ACCEPTED));
        assert!(is_submitted(IDEMPOTENT_HIT));
        assert!(is_submitted("已上报（回执 R-001）"));
        assert!(!is_submitted(RETRYABLE));
        assert!(!is_submitted(REJECTED));
        assert!(!is_submitted(BLOCKED));
        assert!(!is_submitted(""));
    }

    #[test]
    fn 状态取值互不相等() {
        let all = [
            ACCEPTED,
            IDEMPOTENT_HIT,
            REJECTED,
            RETRYABLE,
            BLOCKED,
            INVALID_RECEIPT,
            NOT_EXECUTED,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "两个状态取值撞了：{} / {}", a, b);
            }
        }
    }
}
