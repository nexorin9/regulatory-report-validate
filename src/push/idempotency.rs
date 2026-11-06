//! 上报幂等键与载荷摘要 —— 上报身份。
//!
//! # 两个值分别回答什么问题
//!
//! | 值 | 怎么算 | 它变了说明 |
//! |---|---|---|
//! | [`idempotency_key`] | `sha256(机构代码｜报告期｜指标编号｜科室代码｜业务主键)` | **换了一条业务记录** |
//! | 载荷摘要 | `sha256(被上报内容的规范化 JSON)` | **同一条记录的取值变了（订正）** |
//!
//! 两个值**分开存**：只存一个就分不清「同一行的新版本」与「另一个行」。
//! 同一个幂等键下摘要变了 = 订正 → 修订号 +1、业务键不变；
//! 摘要没变 = 重复提交 → 不产生新的推送请求。
//!
//! # 幂等键为什么不含时间戳与重试次数
//!
//! 幂等键标识的是**哪一条业务记录**，不是**哪一次提交**：
//! 带了时间戳，同一行重跑一次就变成两个键，网关会收两遍；
//! 带了重试次数，一次超时重试又变成另一个键。
//! 时间与重试次数记在 `submissions` 表的 `created_at` / `attempt_count` 上。
//!
//! # 摘要覆盖哪一段
//!
//! 摘要只覆盖**被上报的那一行内容**（见 [`DIGEST_FIELDS`]），不覆盖：
//! - `batch_ref` —— 运行上下文。同一份数据下一期再报，不该因为批次号变了就算新版本；
//! - `revision` —— 修订号由摘要推出（摘要变了才 +1），写进摘要会自指；
//! - `payload_digest` —— 摘要自身。
//!
//! # 规范化规则（写死，测试固定）
//!
//! - **字符串字段**：去首尾空白；空串就是空串（不是「没填」）。
//! - **数值字段**：空 → 空值；能解析成有限数值 → **定形**后的数值
//!   （`96` / `96.0` / `96.00` / 全角 `９６` 都归成 `96`）；解析不了 → **保留原文**
//!   （填坏了就是填坏了，不推断成 0，也不静默丢掉）。
//! - **字段顺序**：按 [`DIGEST_FIELDS`] 的固定顺序落，不看结构体里的声明顺序 ——
//!   载荷字段顺序变了，摘要不变。
//!
//! # 源产品能力 → 融合后行为
//!
//! 参考实现里的**帧级完整性校验**（对定长帧做填充加密后，按帧长分母校验完整性）
//! 借的是「这份数据有没有被改过」这份语义。
//! 本产品**改写**为幂等键 + 载荷 sha256 摘要，**不做加密**：上报对象是监管指标行与
//! 科室级数据，不是患者明细，密文反而让科室对不上数。
//! 定长帧的处理换成「固定字段顺序 + 浮点定形」的规范化 JSON —— 同一份载荷两次算出来
//! 的摘要必须逐字相同。

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::ingest::normalize_digits;

/// 幂等键与摘要的字段分隔符。
///
/// 与 `ingest::key` 同一套：机构代码、报告期、指标编号、科室代码、业务主键都不含这个字符。
const SEP: &str = "|";

/// 幂等键覆盖的字段（顺序写死）。
pub const IDEMPOTENCY_FIELDS: [&str; 5] = [
    "org_code",
    "reporting_period",
    "indicator_code",
    "dept_code",
    "business_key",
];

/// 载荷摘要覆盖的字段（顺序写死）。
///
/// 不含 `batch_ref` / `revision` / `payload_digest`，理由见模块文档。
pub const DIGEST_FIELDS: [&str; 11] = [
    "reporting_period",
    "org_code",
    "indicator_code",
    "dept_code",
    "business_key",
    "numerator",
    "denominator",
    "value",
    "unit",
    "basis",
    "rule_version",
];

/// 幂等键：sha256(机构代码｜报告期｜指标编号｜科室代码｜业务主键)。
///
/// 与时间戳、重试次数、载荷内容**都无关** —— 它标识的是「哪一条业务记录」。
///
/// 段序与 `ingest::key::authority_key` 一致（同一条权威记录），只是这里再取一次哈希：
/// 权威键是**可读**的（库里要能看出冲突的是谁），幂等键是**对外**的（当 `Idempotency-Key`
/// 头发给网关）。同一段输入，两处算出来的东西必须对得上。
pub fn idempotency_key(
    org_code: &str,
    reporting_period: &str,
    indicator_code: &str,
    dept_code: &str,
    business_key: &str,
) -> String {
    sha256_hex(idempotency_source(
        org_code,
        reporting_period,
        indicator_code,
        dept_code,
        business_key,
    )
    .as_bytes())
}

/// 幂等键的**原文**（哈希之前）：机构代码｜报告期｜指标编号｜科室代码｜业务主键。
///
/// 与 `ingest::key::authority_key` 逐字一致 —— 权威键是「库里认得出是谁」的可读形式，
/// 幂等键是「对外发得出去」的哈希形式；两者指的是同一条记录，段序不能各写一套。
pub fn idempotency_source(
    org_code: &str,
    reporting_period: &str,
    indicator_code: &str,
    dept_code: &str,
    business_key: &str,
) -> String {
    [
        org_code.trim(),
        reporting_period.trim(),
        indicator_code.trim(),
        dept_code.trim(),
        business_key.trim(),
    ]
    .join(SEP)
}

/// 规范化后的字段值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalValue {
    /// 空 —— 这一格没填
    Null,
    /// 合法数值（已定形）
    Number(String),
    /// 字符串字段的取值（可能是空串）
    Text(String),
    /// 数值字段填坏了：保留原文，不推断成 0，也不静默丢掉
    Raw(String),
}

impl CanonicalValue {
    /// 数值字段的规范化：空 → 空；合法数值 → 定形；其余 → 原文
    pub fn from_number_text(raw: &str) -> CanonicalValue {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return CanonicalValue::Null;
        }
        match parse_number(trimmed) {
            Some(v) => CanonicalValue::Number(canonical_number(v)),
            None => CanonicalValue::Raw(trimmed.to_string()),
        }
    }

    /// 字符串字段的规范化：去首尾空白（空串仍是空串，不是「没填」）
    pub fn from_text(raw: &str) -> CanonicalValue {
        CanonicalValue::Text(raw.trim().to_string())
    }

    /// JSON 片段
    fn to_json_fragment(&self) -> String {
        match self {
            CanonicalValue::Null => "null".to_string(),
            // 数值文本由 `canonical_number` 生成（有限十进制），可直接当 JSON 数字落
            CanonicalValue::Number(n) => n.clone(),
            CanonicalValue::Text(s) | CanonicalValue::Raw(s) => {
                serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
            }
        }
    }
}

/// 摘要覆盖的内容：字段名 → 规范化取值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalContent {
    fields: BTreeMap<String, CanonicalValue>,
}

impl CanonicalContent {
    /// 空内容
    pub fn new() -> Self {
        Self {
            fields: BTreeMap::new(),
        }
    }

    /// 放一个字段
    pub fn set(&mut self, name: &str, value: CanonicalValue) -> &mut Self {
        self.fields.insert(name.to_string(), value);
        self
    }

    /// 放一个字符串字段
    pub fn set_text(&mut self, name: &str, raw: &str) -> &mut Self {
        self.set(name, CanonicalValue::from_text(raw))
    }

    /// 从任意顺序的字段对建内容 —— **顺序不影响摘要**（摘要按固定字段表落）
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, CanonicalValue)>) -> Self {
        let mut content = Self::new();
        for (name, value) in pairs {
            content.set(&name, value);
        }
        content
    }

    /// 取一个字段的规范化取值
    pub fn get(&self, name: &str) -> Option<&CanonicalValue> {
        self.fields.get(name)
    }

    /// 规范化 JSON：字段顺序固定为 [`DIGEST_FIELDS`]，没放的字段按空值落
    pub fn canonical_json(&self) -> String {
        let mut parts = Vec::with_capacity(DIGEST_FIELDS.len());
        for name in DIGEST_FIELDS {
            let value = self
                .fields
                .get(name)
                .cloned()
                .unwrap_or(CanonicalValue::Null);
            parts.push(format!("\"{}\":{}", name, value.to_json_fragment()));
        }
        format!("{{{}}}", parts.join(","))
    }

    /// 载荷摘要 = sha256(规范化 JSON)
    pub fn digest(&self) -> String {
        sha256_hex(self.canonical_json().as_bytes())
    }
}

/// 数值字段的三种形态。
#[derive(Debug, Clone, PartialEq)]
pub enum NumericField {
    /// 没填
    Empty,
    /// 合法数值
    Number(f64),
    /// 填坏了的原文（保留原样，不推断、不静默丢）
    Raw(String),
}

impl NumericField {
    /// 从原文建数值字段
    pub fn from_raw(raw: &str) -> NumericField {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return NumericField::Empty;
        }
        match parse_number(trimmed) {
            Some(v) => NumericField::Number(v),
            None => NumericField::Raw(trimmed.to_string()),
        }
    }

    /// 作为数值取；空与填坏了都取不到（**不静默当成 0**）
    pub fn as_number(&self) -> Option<f64> {
        match self {
            NumericField::Number(v) => Some(*v),
            _ => None,
        }
    }

    /// 是否是合法数值（空与填坏了都不算）
    pub fn is_number(&self) -> bool {
        matches!(self, NumericField::Number(_))
    }

    /// 原文（空 → 空串）
    pub fn raw_text(&self) -> String {
        match self {
            NumericField::Empty => String::new(),
            NumericField::Number(v) => canonical_number(*v),
            NumericField::Raw(s) => s.clone(),
        }
    }

    /// 规范化取值
    fn canonical(&self) -> CanonicalValue {
        match self {
            NumericField::Empty => CanonicalValue::Null,
            NumericField::Number(v) => CanonicalValue::Number(canonical_number(*v)),
            NumericField::Raw(s) => CanonicalValue::Raw(s.clone()),
        }
    }
}

/// 构造载荷所需的输入。
#[derive(Debug, Clone)]
pub struct PayloadInput<'a> {
    /// 批次号（不参与摘要）
    pub batch_ref: &'a str,
    pub org_code: &'a str,
    pub reporting_period: &'a str,
    pub indicator_code: &'a str,
    pub dept_code: &'a str,
    pub business_key: &'a str,
    pub numerator: NumericField,
    pub denominator: NumericField,
    pub value: NumericField,
    pub unit: &'a str,
    /// 官方依据出处
    pub basis: &'a str,
    /// 这批结论按哪套口径算的
    pub rule_version: &'a str,
}

/// 一条上报载荷。
///
/// 字段与 `spec.md`「出站报文（正式对接规范）」一一对应；出站客户端（后续任务）按
/// 这里的取值组装 body，本模块只管**身份**：幂等键与载荷摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionPayload {
    /// 批次号（不参与摘要）
    pub batch_ref: String,
    pub reporting_period: String,
    pub org_code: String,
    pub indicator_code: String,
    pub dept_code: String,
    pub business_key: String,
    pub numerator: NumericField,
    pub denominator: NumericField,
    pub value: NumericField,
    pub unit: String,
    /// 官方依据出处
    pub basis: String,
    /// 这批结论按哪套口径算的
    pub rule_version: String,
    /// 幂等键（由本模块算，不接受外部传入）
    pub idempotency_key: String,
    /// 载荷摘要（由本模块算，不接受外部传入）
    pub payload_digest: String,
    /// 修订号（订正后 +1，业务键不变）
    pub revision: i64,
}

impl SubmissionPayload {
    /// 用输入构造载荷：自动算幂等键与载荷摘要。
    ///
    /// 修订号先给 1 —— 真正的修订号由 [`decide_revision`] 按本机库里已有的提交决定。
    pub fn new(input: PayloadInput<'_>) -> SubmissionPayload {
        let idempotency_key = idempotency_key(
            input.org_code,
            input.reporting_period,
            input.indicator_code,
            input.dept_code,
            input.business_key,
        );
        let content = content_of(
            input.reporting_period,
            input.org_code,
            input.indicator_code,
            input.dept_code,
            input.business_key,
            &input.numerator,
            &input.denominator,
            &input.value,
            input.unit,
            input.basis,
            input.rule_version,
        );
        let payload_digest = content.digest();
        SubmissionPayload {
            batch_ref: input.batch_ref.to_string(),
            reporting_period: input.reporting_period.trim().to_string(),
            org_code: input.org_code.trim().to_string(),
            indicator_code: input.indicator_code.trim().to_string(),
            dept_code: input.dept_code.trim().to_string(),
            business_key: input.business_key.trim().to_string(),
            numerator: input.numerator,
            denominator: input.denominator,
            value: input.value,
            unit: input.unit.trim().to_string(),
            basis: input.basis.trim().to_string(),
            rule_version: input.rule_version.trim().to_string(),
            idempotency_key,
            payload_digest,
            revision: 1,
        }
    }

    /// 摘要覆盖的那一段内容（可核对：重算出的摘要应与 `payload_digest` 一致）
    pub fn content(&self) -> CanonicalContent {
        content_of(
            &self.reporting_period,
            &self.org_code,
            &self.indicator_code,
            &self.dept_code,
            &self.business_key,
            &self.numerator,
            &self.denominator,
            &self.value,
            &self.unit,
            &self.basis,
            &self.rule_version,
        )
    }

    /// 重算载荷摘要（与 `payload_digest` 一致 —— 不一致说明载荷被改过）
    pub fn recomputed_digest(&self) -> String {
        self.content().digest()
    }

    /// 换修订号（摘要不变 —— 修订号不参与摘要）
    pub fn with_revision(mut self, revision: i64) -> SubmissionPayload {
        self.revision = revision;
        self
    }
}

/// 本机库里同一个幂等键下已存在的提交。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingSubmission {
    /// 修订号
    pub revision: i64,
    /// 那次提交的载荷摘要
    pub payload_digest: String,
    /// 那次提交的**状态**（已上报 / 被退回 / 待重试 …）
    ///
    /// 这一项决定「上次到底成没成」：光看摘要会把**上次没报上去**的行
    /// 当成「已上报」跳过 —— 那一行就永远不会再推一次，经办人也催不动。
    pub status: String,
}

/// 本次提交该怎么走。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionDecision {
    /// 载荷没变**且上次已上报** → 重复提交：幂等键命中，**不产生新的推送请求**
    Duplicate { revision: i64 },
    /// 载荷变了 → 订正：业务键不变，修订号 +1
    Revised { revision: i64 },
    /// 载荷没变**但上次没报上去**（5xx / 超时 / 连不上）→ 重试同一修订号
    ///
    /// 沿用**同一个幂等键与同一个修订号**：网关侧若其实已受理，会返回幂等命中，
    /// 不会变成两条上报；若确实没受理，这一行就还有机会报上去。
    Retry { revision: i64 },
}

impl RevisionDecision {
    /// 该用哪个修订号
    pub fn revision(&self) -> i64 {
        match self {
            RevisionDecision::Duplicate { revision } => *revision,
            RevisionDecision::Revised { revision } => *revision,
            RevisionDecision::Retry { revision } => *revision,
        }
    }

    /// 是否是「重复提交」
    pub fn is_duplicate(&self) -> bool {
        matches!(self, RevisionDecision::Duplicate { .. })
    }

    /// 是否是「重试上次没成功的那一次」
    pub fn is_retry(&self) -> bool {
        matches!(self, RevisionDecision::Retry { .. })
    }

    /// 中文说法（终端与导出用）
    pub fn zh(&self) -> &'static str {
        match self {
            RevisionDecision::Duplicate { .. } => "重复提交（载荷未变，不重复推送）",
            RevisionDecision::Revised { .. } => "订正后重报（修订号 +1）",
            RevisionDecision::Retry { .. } => "重试上次未成功的提交（沿用同一修订号与幂等键）",
        }
    }
}

/// 同一个幂等键下**最近一次**提交的状态（没有则空串）。
pub fn latest_status(existing: &[ExistingSubmission]) -> &str {
    existing
        .iter()
        .max_by_key(|s| s.revision)
        .map(|s| s.status.as_str())
        .unwrap_or_default()
}

/// 决定这次提交用哪个修订号。
///
/// 拿**最新一次**提交比，**摘要与状态一起看**：
/// - 摘要一样 **且上次已上报** → [`RevisionDecision::Duplicate`]：幂等键命中，不产生新的推送请求；
/// - 摘要一样 **但上次是待重试**（5xx / 超时 / 连不上）→ [`RevisionDecision::Retry`]：
///   网关**没有**受理那一行，原样再推一次是安全的，沿用同一修订号与幂等键；
/// - 摘要一样 **但上次被退回**（4xx）→ 按重复提交处理：退回是数据问题，
///   改完数据摘要就变了，会自动走订正重报；载荷没变就不重复推；
/// - 摘要一样 **且上次既没报上去、也不该重推**（未发出 / 未执行 / 回执待人工核对）
///   → [`RevisionDecision::Retry`]：这一行**根本没报上去**，不能当成重复提交跳过
///   —— 跳过了就等于把这一行永远钉在没报上去的状态里，经办人也催不动；
/// - 摘要不一样 → [`RevisionDecision::Revised`]：业务键不变，修订号 +1。
///
/// 改回旧值（摘要等于更早那次）也算订正 —— 产生新修订号，不复用旧修订号：
/// 旧修订号对应的是网关侧已经收到过的那一版。
pub fn decide_revision(existing: &[ExistingSubmission], digest: &str) -> RevisionDecision {
    match existing.iter().max_by_key(|s| s.revision) {
        Some(latest) if latest.payload_digest == digest => {
            if crate::push::status::is_submitted(&latest.status) {
                RevisionDecision::Duplicate {
                    revision: latest.revision,
                }
            } else if crate::push::status::is_rejected(&latest.status) {
                // 被退回：数据没改就别再推（改了摘要就变，自动走订正重报）
                RevisionDecision::Duplicate {
                    revision: latest.revision,
                }
            } else {
                // 待重试 / 未发出 / 未执行 / 回执待人工核对 —— 这一行**没报上去**，
                // 原样再推一次（幂等键不变，网关侧若其实已受理会返回幂等命中）
                RevisionDecision::Retry {
                    revision: latest.revision,
                }
            }
        }
        Some(latest) => RevisionDecision::Revised {
            revision: latest.revision + 1,
        },
        None => RevisionDecision::Revised { revision: 1 },
    }
}

/// 摘要覆盖的内容（11 项，顺序固定）
#[allow(clippy::too_many_arguments)]
fn content_of(
    reporting_period: &str,
    org_code: &str,
    indicator_code: &str,
    dept_code: &str,
    business_key: &str,
    numerator: &NumericField,
    denominator: &NumericField,
    value: &NumericField,
    unit: &str,
    basis: &str,
    rule_version: &str,
) -> CanonicalContent {
    let mut content = CanonicalContent::new();
    content.set_text("reporting_period", reporting_period);
    content.set_text("org_code", org_code);
    content.set_text("indicator_code", indicator_code);
    content.set_text("dept_code", dept_code);
    content.set_text("business_key", business_key);
    content.set("numerator", numerator.canonical());
    content.set("denominator", denominator.canonical());
    content.set("value", value.canonical());
    content.set_text("unit", unit);
    content.set_text("basis", basis);
    content.set_text("rule_version", rule_version);
    content
}

/// 解析数值：全角数字先归半角；非有限值（inf / NaN）不算合法数值
fn parse_number(raw: &str) -> Option<f64> {
    let normalized = normalize_digits(raw.trim());
    let value: f64 = normalized.parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

/// 数值定形：同一个数的不同写法归成同一串。
///
/// `96` / `96.0` / `96.00` / 全角 `９６` → `96`；`0.1` 与 `0.10` → `0.1`。
fn canonical_number(value: f64) -> String {
    if value == 0.0 {
        // -0.0 与 0.0 是同一个数，别让摘要因为负零而变
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    format!("{}", value)
}

/// sha256 十六进制
fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(batch_ref: &'a str) -> PayloadInput<'a> {
        PayloadInput {
            batch_ref,
            org_code: "ORG-001",
            reporting_period: "2026-Q3",
            indicator_code: "MER-TL-04",
            dept_code: "0301",
            business_key: "BK-01",
            numerator: NumericField::Number(980.0),
            denominator: NumericField::Number(1000.0),
            value: NumericField::Number(98.0),
            unit: "%",
            basis: "国卫办医函〔2021〕28号 附件 指标七",
            rule_version: "2021.1",
        }
    }

    #[test]
    fn 幂等键只由五个要素决定() {
        let a = idempotency_key("ORG-001", "2026-Q3", "MER-TL-04", "0301", "BK-01");
        let b = idempotency_key("ORG-001", "2026-Q3", "MER-TL-04", "0301", "BK-01");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn 幂等键与时间戳重试次数无关() {
        // 幂等键的输入里没有时间戳与重试次数这两个东西
        assert_eq!(IDEMPOTENCY_FIELDS.len(), 5);
        assert!(!IDEMPOTENCY_FIELDS.contains(&"timestamp"));
        assert!(!IDEMPOTENCY_FIELDS.contains(&"attempt_count"));
    }

    #[test]
    fn 改值不改幂等键但改摘要() {
        let first = SubmissionPayload::new(input("batch-a"));
        let mut changed = input("batch-a");
        changed.value = NumericField::Number(75.0);
        let second = SubmissionPayload::new(changed);
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_ne!(first.payload_digest, second.payload_digest);
    }

    #[test]
    fn 载荷字段顺序变化不改摘要() {
        let ordered = CanonicalContent::from_pairs(vec![
            ("reporting_period".to_string(), CanonicalValue::Text("2026-Q3".into())),
            ("org_code".to_string(), CanonicalValue::Text("ORG-001".into())),
            ("value".to_string(), CanonicalValue::Number("98".into())),
        ]);
        let shuffled = CanonicalContent::from_pairs(vec![
            ("value".to_string(), CanonicalValue::Number("98".into())),
            ("org_code".to_string(), CanonicalValue::Text("ORG-001".into())),
            ("reporting_period".to_string(), CanonicalValue::Text("2026-Q3".into())),
        ]);
        assert_eq!(ordered.canonical_json(), shuffled.canonical_json());
        assert_eq!(ordered.digest(), shuffled.digest());
    }

    #[test]
    fn 浮点定形让同值同摘要() {
        let a = NumericField::from_raw("96.0");
        let b = NumericField::from_raw("96.00");
        let c = NumericField::from_raw("９６");
        assert_eq!(a, NumericField::Number(96.0));
        assert_eq!(b, NumericField::Number(96.0));
        assert_eq!(c, NumericField::Number(96.0));
        assert_eq!(a.canonical(), b.canonical());
        assert_eq!(a.canonical(), c.canonical());
    }

    #[test]
    fn 空值与填坏了分得开() {
        assert_eq!(NumericField::from_raw("").raw_text(), "");
        assert_eq!(NumericField::from_raw("   ").raw_text(), "");
        let broken = NumericField::from_raw("abc");
        assert!(!broken.is_number());
        assert_eq!(broken.raw_text(), "abc");
        assert_eq!(broken.as_number(), None);
    }

    #[test]
    fn 摘要不含批次号与修订号() {
        let a = SubmissionPayload::new(input("batch-a"));
        let b = SubmissionPayload::new(input("batch-b")).with_revision(7);
        assert_eq!(a.payload_digest, b.payload_digest);
        assert_eq!(a.idempotency_key, b.idempotency_key);
    }

    #[test]
    fn 载荷能重算出同一个摘要() {
        let payload = SubmissionPayload::new(input("batch-a"));
        assert_eq!(payload.recomputed_digest(), payload.payload_digest);
    }

    #[test]
    fn 同键同摘要判重复提交() {
        let payload = SubmissionPayload::new(input("batch-a"));
        let existing = vec![ExistingSubmission {
            revision: 1,
            payload_digest: payload.payload_digest.clone(),
            status: crate::push::status::ACCEPTED.to_string(),
        }];
        let decision = decide_revision(&existing, &payload.payload_digest);
        assert!(decision.is_duplicate());
        assert_eq!(decision.revision(), 1);
    }

    #[test]
    fn 上次待重试且载荷没变_判重试而不是重复提交() {
        // 上次 5xx 没报上去 → 那一行还得再推一次（沿用同一修订号与幂等键）
        let payload = SubmissionPayload::new(input("batch-a"));
        let existing = vec![ExistingSubmission {
            revision: 1,
            payload_digest: payload.payload_digest.clone(),
            status: crate::push::status::RETRYABLE.to_string(),
        }];
        let decision = decide_revision(&existing, &payload.payload_digest);
        assert!(decision.is_retry(), "上次没报上去，不能当成已上报跳过");
        assert!(!decision.is_duplicate());
        assert_eq!(decision.revision(), 1, "重试沿用同一修订号");
    }

    #[test]
    fn 上次被退回且载荷没变_不重复推送() {
        // 退回是数据问题：载荷没改就没必要再推一次（改了摘要就变了，会走订正）
        let payload = SubmissionPayload::new(input("batch-a"));
        let existing = vec![ExistingSubmission {
            revision: 1,
            payload_digest: payload.payload_digest.clone(),
            status: crate::push::status::REJECTED.to_string(),
        }];
        let decision = decide_revision(&existing, &payload.payload_digest);
        assert!(decision.is_duplicate());
        assert_eq!(decision.revision(), 1);
    }

    #[test]
    fn 最近一次提交的状态取修订号最大的那条() {
        let existing = vec![
            ExistingSubmission {
                revision: 1,
                payload_digest: "d1".to_string(),
                status: crate::push::status::RETRYABLE.to_string(),
            },
            ExistingSubmission {
                revision: 2,
                payload_digest: "d2".to_string(),
                status: crate::push::status::ACCEPTED.to_string(),
            },
        ];
        assert_eq!(latest_status(&existing), crate::push::status::ACCEPTED);
    }

    #[test]
    fn 改值后修订号加一且业务键不变() {
        let payload = SubmissionPayload::new(input("batch-a"));
        let existing = vec![ExistingSubmission {
            revision: 1,
            payload_digest: "旧摘要".to_string(),
            status: crate::push::status::ACCEPTED.to_string(),
        }];
        let decision = decide_revision(&existing, &payload.payload_digest);
        assert!(!decision.is_duplicate());
        assert_eq!(decision.revision(), 2);
    }
}
