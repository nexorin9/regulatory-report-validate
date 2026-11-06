//! 规则与问题的数据结构。
//!
//! 规则表的口径与源仓库「SQL 先解析再执行」的用法一致：规则 SQL 是数据不是代码，
//! 执行时以命名参数绑定，不拼字符串；解析/准备失败时该规则不生效并报错，不静默通过。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::checks;

/// 规则类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleKind {
    /// 必填缺失（五列必填：指标编号 / 指标名称 / 科室代码 / 报告期 / 业务主键）
    RequiredField,
    /// 条件必填缺失（比率型指标的分子 / 分母 / 填报值 / 计量单位）
    ///
    /// 与 [`RuleKind::RequiredField`] 分开，是因为两者**口径范围不同**：
    /// 五列必填适用于全部指标，条件必填只适用于比率型指标。
    /// 口径槽位按（检查类型, 适用指标）划分，两者必须占不同槽位 ——
    /// 否则换版时新口径会把旧口径顶掉（另一条还在，另一条被标「已被取代」）。
    ConditionalRequired,
    /// 枚举越界
    EnumBound,
    /// 单位不符
    UnitCheck,
    /// 分子分母公式复算
    FormulaRecalc,
    /// 数据形态错误（分母为 0 / 数值列非数值 / 数值列为负）
    ///
    /// 与 [`RuleKind::FormulaRecalc`] 分开，是因为「算错了」与「填坏了」要改的地方不同：
    /// 公式不符要重算填报值；数据形态错误要改那一格本身或补分母口径。
    /// 混成一类，报错的地方就和真正要改的地方对不上。
    DataShapeError,
    /// 疑似重复（表内业务主键撞键；最终判定交重复检测漏斗）
    SuspectedDuplicate,
}

impl RuleKind {
    /// 规则清单里的检查类型名（给运维看：这条规则管什么）
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::RequiredField => "必填缺失",
            RuleKind::ConditionalRequired => "条件必填缺失",
            RuleKind::EnumBound => "枚举越界",
            RuleKind::UnitCheck => "单位不符",
            RuleKind::FormulaRecalc => "公式不符",
            RuleKind::DataShapeError => "数据形态错误",
            RuleKind::SuspectedDuplicate => "疑似重复",
        }
    }

    /// 写进「问题类型」列的中文名
    ///
    /// 与 [`RuleKind::as_str`] 的差别只在「条件必填缺失」：它归入**必填缺失**一类
    /// （四类口径问题：必填缺失 / 枚举越界 / 单位不符 / 疑似重复），
    /// 而规则清单里仍按「条件必填缺失」显示，便于运维看清这条规则管的是哪一段。
    pub fn as_issue_kind(self) -> &'static str {
        match self {
            RuleKind::ConditionalRequired => "必填缺失",
            other => other.as_str(),
        }
    }

    /// 规则表文件里用的机器可读代号（口径目录由业务人员手工维护，代号稳定不随中文名变）
    pub fn as_token(self) -> &'static str {
        match self {
            RuleKind::RequiredField => "required",
            RuleKind::ConditionalRequired => "required_conditional",
            RuleKind::EnumBound => "enum",
            RuleKind::UnitCheck => "unit",
            RuleKind::FormulaRecalc => "formula",
            RuleKind::DataShapeError => "data_shape",
            RuleKind::SuspectedDuplicate => "duplicate",
        }
    }

    /// 由代号还原；未知代号返回 `None`（调用方据此拒绝装载，不猜）
    pub fn from_token(token: &str) -> Option<RuleKind> {
        match token {
            "required" => Some(RuleKind::RequiredField),
            "required_conditional" => Some(RuleKind::ConditionalRequired),
            "enum" => Some(RuleKind::EnumBound),
            "unit" => Some(RuleKind::UnitCheck),
            "formula" => Some(RuleKind::FormulaRecalc),
            "data_shape" => Some(RuleKind::DataShapeError),
            "duplicate" => Some(RuleKind::SuspectedDuplicate),
            _ => None,
        }
    }
}

/// 严重度：阻断级 → 校验结论「不通过」；提示级 → 「有提示」
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Blocking,
    Hint,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Blocking => "阻断",
            Severity::Hint => "提示",
        }
    }

    /// 规则表文件里用的机器可读代号
    pub fn as_token(self) -> &'static str {
        match self {
            Severity::Blocking => "blocking",
            Severity::Hint => "hint",
        }
    }

    /// 由代号还原；未知代号返回 `None`
    pub fn from_token(token: &str) -> Option<Severity> {
        match token {
            "blocking" => Some(Severity::Blocking),
            "hint" => Some(Severity::Hint),
            _ => None,
        }
    }
}

/// 规则状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuleStatus {
    /// 生效中
    Active,
    /// 语法/装载不通过，不参与执行（不静默通过）
    Invalid,
    /// 已被新版本取代，旧版本保留可追
    Superseded,
    /// 换版待确认生效：新版本已就位，但没过换版闸门（未经质控岗确认 /
    /// 生效日期未到），本轮继续用旧版
    ///
    /// 与「已被取代」的区别：被取代的旧版**已经**让位给生效版；
    /// 待确认生效的新版**还没**上位 —— 它在等一张确认记录。
    Pending,
}

impl RuleStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleStatus::Active => "生效",
            RuleStatus::Invalid => "不生效",
            RuleStatus::Superseded => "已被取代",
            RuleStatus::Pending => "待确认生效",
        }
    }
}

/// 命名参数的值
#[derive(Debug, Clone, PartialEq)]
pub enum RuleParam {
    Text(String),
    Int(i64),
    Null,
}

impl RuleParam {
    /// 由规则表文件里的 JSON 取值转换（字符串 / 整数 / null；其它类型拒绝装载）
    pub fn from_value(value: &Value) -> Result<RuleParam, String> {
        match value {
            Value::Null => Ok(RuleParam::Null),
            Value::String(s) => Ok(RuleParam::Text(s.clone())),
            Value::Number(n) => n
                .as_i64()
                .map(RuleParam::Int)
                .ok_or_else(|| "参数数值必须是整数".to_string()),
            Value::Bool(_) => Err("参数不支持布尔值".to_string()),
            other => Err(format!("参数类型不支持：{}", other)),
        }
    }
}

/// 一条口径规则
#[derive(Debug, Clone)]
pub struct Rule {
    /// 规则编号（问题清单与举证包按此回溯）
    pub rule_id: String,
    /// 适用指标编号；`None` 表示适用于全部指标
    pub indicator_code: Option<String>,
    /// 检查项：同一检查类型下的**不同检查项**（如枚举越界里的「报告期格式」与「计量单位」）。
    ///
    /// 口径槽位 = (检查类型, 适用指标, 检查项)。少了这一段，同一检查类型下的两条规则
    /// 会互相顶掉 —— 换一版「报告期格式」的口径时，把「计量单位」那条也一起顶成「已被取代」。
    pub check_column: Option<String>,
    pub rule_kind: RuleKind,
    /// 规则 SQL（数据，不是代码）
    pub sql: String,
    /// 命名参数（执行时绑定，不拼进 SQL 字符串）
    pub params: BTreeMap<String, RuleParam>,
    /// 官方依据出处；必填，缺依据的规则不装载
    pub basis: String,
    /// 规则版本（换版后历史批次可按原版本复算）
    pub version: String,
    pub severity: Severity,
    pub effective_from: Option<String>,
    pub status: RuleStatus,
    /// 装载来源（规则表文件名）；口径内容之外的装载元数据，便于回溯是哪张规则表带来的
    pub source_file: String,
}

impl Rule {
    /// 规则中文名（给业务人员看）：检查类型 + 检查项 + 适用指标。
    ///
    /// 报错里要同时点名「哪条规则」和「这条规则管什么」—— 光有编号，
    /// 业务人员还得自己去翻规则表才知道改哪儿。
    pub fn display_name(&self) -> String {
        let kind = self.rule_kind.as_str();
        let scope = match &self.indicator_code {
            Some(code) => format!("适用指标 {}", code),
            None => "适用全部指标".to_string(),
        };
        match &self.check_column {
            Some(column) => format!("{} · {} · {}", kind, column, scope),
            None => format!("{} · {}", kind, scope),
        }
    }
}

/// 一条口径问题
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub row_uid: String,
    pub rule_id: String,
    /// 问题类型（写进「问题类型」列）
    pub issue_kind: String,
    /// 严重度：阻断 / 提示
    pub severity: String,
    /// 问题说明（写进「问题说明（含依据）」列）
    pub message: String,
    /// 官方依据出处
    pub basis: String,
    /// 同键去重的区分键（如缺失的列名）
    pub issue_key: String,
    /// 缺失列清单（可多列）
    pub missing_columns: Vec<String>,
    /// 机器可读错误码（与 [`crate::errors::ErrorCode`] 同一套 code）。
    ///
    /// 「问题类型」已足够定位时留空；同一类问题里还要再分小类时带码 ——
    /// 公式复算的三类问题都归在「公式不符 / 数据形态错误」之下，但分母为 0
    /// （`DENOMINATOR_ZERO`）与非数值 / 负值（`INVALID_NUMERIC`）要改的地方不同，
    /// 插件与导出按码分组才分得开。实体绑定类问题
    /// （科室主数据不一致、同权威键异值）同样带码。
    pub error_code: String,
}

/// 一条规则本轮**没跑起来**的记录（缺列不生效 / 执行失败）。
///
/// 口径规则没跑起来时不能静默：少一条检查就可能把不合格的行判成「通过」。
/// 因此每条都进本轮执行报告，由结果契约的规则清单段逐条列出 —— 失败规则计数可见。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionFailureEntry {
    /// 规则编号
    pub rule_id: String,
    /// 规则中文名（检查类型 + 适用指标）
    pub rule_name: String,
    /// 结论：不生效 / 执行失败
    pub outcome: String,
    /// 机器可读错误码
    pub code: String,
    /// 中文说明（给业务人员看）
    pub message: String,
    /// 出错位置（缺哪一列 / 规则 id）
    pub location: String,
}

/// 内置规则集的口径版本
pub const BUILTIN_VERSION: &str = "2021.1";

/// 内置规则集。
///
/// 这是 `REGVAL_RULE_DIR` **未配置**时的兜底口径表，内容与规则目录里的
/// `rules-2021.1.rules.json` 一致（见 `tests/rules_loader_test.rs` 的一致性断言），
/// 版本号同为 `2021.1` —— 口径版本不因「从哪装载」而变，结论可对照。
///
/// 规则由检查器编译而来（[`crate::rules::checks`]）：口径写在检查器里，
/// 编译成「SQL 文本 + 命名参数 + 依据 + 版本」这几条数据，执行器只管执行。
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        checks::required::compile_required_rule(BUILTIN_VERSION, checks::required::REQUIRED_BASIS),
        checks::required::compile_conditional_required_rule(
            BUILTIN_VERSION,
            checks::required::CONDITIONAL_BASIS,
        ),
        checks::required::compile_table_unique_rule(
            BUILTIN_VERSION,
            checks::required::TABLE_UNIQUE_BASIS,
        ),
        checks::enum_::compile_reporting_period_format_rule(
            BUILTIN_VERSION,
            checks::enum_::PERIOD_FORMAT_BASIS,
        ),
        checks::enum_::compile_reporting_period_type_rule(
            BUILTIN_VERSION,
            checks::enum_::PERIOD_TYPE_BASIS,
        ),
        checks::enum_::compile_unit_enum_rule(BUILTIN_VERSION, checks::enum_::UNIT_ENUM_BASIS),
        checks::enum_::compile_indicator_code_enum_rule(
            BUILTIN_VERSION,
            checks::enum_::INDICATOR_CODE_BASIS,
        ),
        checks::unit::compile_unit_match_rule(BUILTIN_VERSION, checks::unit::UNIT_MATCH_BASIS),
        checks::formula::compile_formula_recalc_rule(
            BUILTIN_VERSION,
            checks::formula::FORMULA_BASIS,
        ),
        checks::formula::compile_denominator_zero_rule(
            BUILTIN_VERSION,
            checks::formula::DENOMINATOR_ZERO_BASIS,
        ),
        checks::formula::compile_numeric_shape_rule(
            BUILTIN_VERSION,
            checks::formula::NUMERIC_SHAPE_BASIS,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 内置规则带依据且不空() {
        for rule in builtin_rules() {
            assert!(!rule.basis.is_empty(), "规则 {} 缺依据", rule.rule_id);
            assert_eq!(rule.status, RuleStatus::Active);
        }
    }

    #[test]
    fn 规则_sql_用命名参数不带数据拼接痕迹() {
        let rule = &builtin_rules()[0];
        assert!(rule.sql.contains(":scope"));
        assert!(rule.sql.contains(":label_1"));
        // 值一律走 params，不进 SQL 文本
        assert!(!rule.sql.contains("指标编号"));
    }
}
