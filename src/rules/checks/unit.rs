//! 计量单位检查器。
//!
//! # 这一层管什么
//!
//! 「计量单位」这一列有两问，**两问分开报，不重复报**：
//!
//! | 问 | 谁判 | 报什么 | 规则编号 |
//! |---|---|---|---|
//! | **写没写错** | 本模块（值域） | 这个单位不在五种规范单位之内 | `R-ENUM-003` |
//! | **配不配** | 本模块（与指标匹配） | 单位在合法取值之内，但与该指标的官方口径不符 | `R-UNIT-001` |
//!
//! 同一行同一件事只报一次：
//!
//! - 单位写成「台」→ 不在五种之内 → 只报 `R-ENUM-003`（值域问题）；
//! - 单位写成「例」而指标是比率型（必须是 %）→ 单位**合法** → 只报 `R-UNIT-001`；
//! - 单位写成「台」而指标也是比率型 → 仍然是值域问题，`R-UNIT-001` **不重复报**
//!   （科室先把单位改成合法写法，再看配不配 —— 两张问题单改一次就够）。
//!
//! # 判定用归一后的值
//!
//! 同一个单位有多种写法（全角 `％`、`例数`、`人次数`），按字面比大小会把同义写法
//! 判成两个结论。因此两条规则都先归一再判定 —— 别名表见
//! [`crate::rules::normalize`]，它随规则表下发（规则参数 `unit_aliases`），
//! 不是检查器里的私有常量。
//!
//! # 与其它检查器的分工
//!
//! - **必填缺失**（[`super::required`]）：单位**没填**是必填问题，不是单位问题；
//! - **公式复算**（Task 13）：数值算得对不对；
//! - **枚举越界的其它四类**（[`super::enum_`]）：报告期格式 / 报告期类型 / 指标编号 / 科室代码。
//!
//! # 「缺失单位」什么时候报
//!
//! **不报** —— 单位没填归必填缺失（[`super::required`] 的条件必填：比率型指标必须填单位）。
//! 本模块只判「填了但填错了」。填了空格、全角空格也按没填算，交给必填那条。
//!
//! # 「口径待补」的指标不猜
//!
//! 官方附件写明了单位的指标（本轮 24 项比率型，单位 %）才判「配不配」；
//! 官方附件**没写**单位的指标（`MER-HR-01…03` 人均负担型）标「口径待补」，
//! 本轮**不判**其单位是否匹配 —— 猜出来的单位会让科室按错口径填数，比「没写」更糟。

use std::collections::BTreeMap;

use crate::rules::indicators;
use crate::rules::model::{Rule, RuleKind, RuleParam, RuleStatus, Severity};
use crate::rules::normalize;

/// 计量单位的合法取值（五种规范单位）
pub use crate::rules::normalize::CANONICAL_UNITS as UNIT_ENUM;

/// 计量单位枚举检查的依据
pub const UNIT_ENUM_BASIS: &str = "本产品最小数据格式与字段要求「计量单位」列（取值 ∈ 例 / 例次 / 人次 / 人 / %）；国卫办医函〔2021〕28号 附件 24 项比率型指标的计算公式为「分子 ÷ 分母 × 100%」，计量单位为 %；同一单位的同义写法（全角百分号、字数写法等）按单位别名表归一后判定，别名表随规则表下发（规则参数 unit_aliases）";

/// 单位与指标口径匹配检查的依据
pub const UNIT_MATCH_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》：指标四至指标二十七（MER-TL-01…04、MER-ME-01…03、MER-D&T-01…08、MER-TQ-01…09，共 24 项比率型指标）的计算公式为「分子 ÷ 分母 × 100%」，计量单位为 %，故这 24 项指标的计量单位必须是 %；指标一至指标三（MER-HR-01…03）官方附件未写明计量单位，标「口径待补」，本轮不判其单位是否匹配";

/// 逗号分隔的合法取值清单（绑定参数用）
fn csv(values: &[&str]) -> String {
    values.join(",")
}

/// 别名表 CTE（归一后的写法 → 规范单位）
///
/// 别名表是**数据**（规则参数 `unit_aliases`），不是拼进 SQL 的字面量 ——
/// 运维改别名不用改 SQL，改完重新装载即可。
fn alias_cte() -> String {
    "WITH al AS (\n\
    \x20 SELECT json_extract(value, '$.alias') AS alias,\n\
    \x20        json_extract(value, '$.canonical') AS canonical\n\
    \x20   FROM json_each(:unit_aliases)\n\
    )"
    .to_string()
}

/// 期望单位 CTE（指标编号 → 官方口径里的计量单位）
///
/// 只收**官方写明了单位**的指标；未写明的（口径待补）不进这张表 ——
/// 进来了就等于替它猜了一个单位。
fn expected_units_cte() -> String {
    "WITH al AS (\n\
    \x20 SELECT json_extract(value, '$.alias') AS alias,\n\
    \x20        json_extract(value, '$.canonical') AS canonical\n\
    \x20   FROM json_each(:unit_aliases)\n\
    ), ex AS (\n\
    \x20 SELECT json_extract(value, '$.code') AS code,\n\
    \x20        json_extract(value, '$.name') AS name,\n\
    \x20        json_extract(value, '$.unit') AS unit\n\
    \x20   FROM json_each(:expected_units)\n\
    )"
    .to_string()
}

/// 编译「计量单位值域」枚举规则（`R-ENUM-003`）
///
/// 单位归一后不在五种规范单位之内 → 报枚举越界。
///
/// 归一是**先做**的：写成「例数」「％」的表不该被判成枚举越界 —— 它们指的是
/// 「例」「%」，是合法单位。同一个单位的两种写法必须得出同一个结论。
pub fn compile_unit_enum_rule(version: &str, basis: &str) -> Rule {
    let sql = format!(
        "{cte}, u AS (\n\
        \x20 SELECT r.row_uid AS row_uid,\n\
        \x20        trim(r.unit, {blank}) AS unit_text,\n\
        \x20        {norm} AS folded\n\
        \x20   FROM rows r\n\
        \x20  WHERE r.batch_ref = :batch_ref\n\
        \x20    AND r.unit IS NOT NULL\n\
        \x20    AND trim(r.unit, {blank}) <> ''\n\
        )\n\
        \x20SELECT u.row_uid AS row_uid,\n\
        \x20       '计量单位「' || u.unit_text || '」不在枚举内；期望取值：' || replace(:unit_csv, ',', ' / ')\n\
        \x20         || CASE WHEN COALESCE(al.canonical, u.folded) <> u.folded\n\
        \x20                 THEN '（按别名表归一为「' || COALESCE(al.canonical, u.folded) || '」）'\n\
        \x20                 ELSE '' END AS unit_note\n\
        \x20  FROM u\n\
        \x20  LEFT JOIN al ON al.alias = u.folded\n\
        \x20 WHERE instr(',' || :unit_csv || ',', ',' || COALESCE(al.canonical, u.folded) || ',') = 0",
        cte = alias_cte(),
        blank = normalize::BLANK_CHARS_SQL,
        norm = normalize::normalized_unit_sql("r.unit"),
    );
    let mut params = BTreeMap::new();
    params.insert("unit_csv".to_string(), RuleParam::Text(csv(&UNIT_ENUM)));
    params.insert(
        "unit_aliases".to_string(),
        RuleParam::Text(normalize::alias_table_json()),
    );
    Rule {
        rule_id: "R-ENUM-003".to_string(),
        indicator_code: None,
        check_column: Some("unit".to_string()),
        rule_kind: RuleKind::EnumBound,
        sql,
        params,
        basis: basis.to_string(),
        version: version.to_string(),
        severity: Severity::Blocking,
        effective_from: None,
        status: RuleStatus::Active,
        source_file: "（内置规则集）".to_string(),
    }
}

/// 编译「单位与指标口径匹配」规则（`R-UNIT-001`）
///
/// 单位**合法**但与该指标的官方口径不符 → 报「单位不符」，并给出期望单位。
///
/// 24 项比率型指标的官方计量单位是 %，写成别的就报；官方未写明单位的指标
/// （`MER-HR-01…03`，口径待补）不在期望单位表里，本轮不判。
///
/// 期望单位表是**数据**（规则参数 `expected_units`），来自官方 27 项口径登记表
/// （[`crate::rules::indicators`]）—— 后续任务把 27 项种子补全后，这张表自动长全，
/// 不用改这里的代码。
pub fn compile_unit_match_rule(version: &str, basis: &str) -> Rule {
    compile_unit_match_rule_with_expected_units(version, basis, &indicators::expected_units_json())
}

/// 编译「单位与指标口径匹配」规则（期望单位表由调用方给）
///
/// 期望单位表来源有两个：内置兜底登记表（[`crate::rules::indicators`]）与官方口径种子
/// （[`crate::rules::seed`]）。官方附件未写明单位的三项（`MER-HR-01…03`）不在表里 ——
/// 标「口径待补」，本轮不判其单位是否匹配。
pub fn compile_unit_match_rule_with_expected_units(
    version: &str,
    basis: &str,
    expected_units: &str,
) -> Rule {
    let sql = format!(
        "{cte}, u AS (\n\
        \x20 SELECT r.row_uid AS row_uid,\n\
        \x20        trim(r.indicator_code) AS indicator_code,\n\
        \x20        trim(r.unit, {blank}) AS unit_text,\n\
        \x20        {norm} AS folded\n\
        \x20   FROM rows r\n\
        \x20  WHERE r.batch_ref = :batch_ref\n\
        \x20    AND r.unit IS NOT NULL\n\
        \x20    AND trim(r.unit, {blank}) <> ''\n\
        )\n\
        \x20SELECT u.row_uid AS row_uid,\n\
        \x20       '计量单位「' || u.unit_text || '」与指标 ' || u.indicator_code\n\
        \x20         || '（' || COALESCE(ex.name, '') || '）不符；期望单位：' || ex.unit\n\
        \x20         || '；本行单位按别名表归一后为「' || COALESCE(al.canonical, u.folded) || '」' AS unit_note\n\
        \x20  FROM u\n\
        \x20  LEFT JOIN al ON al.alias = u.folded\n\
        \x20  JOIN ex ON ex.code = u.indicator_code\n\
        \x20 WHERE COALESCE(al.canonical, u.folded) <> ex.unit\n\
        \x20   AND instr(',' || :unit_csv || ',', ',' || COALESCE(al.canonical, u.folded) || ',') > 0",
        cte = expected_units_cte(),
        blank = normalize::BLANK_CHARS_SQL,
        norm = normalize::normalized_unit_sql("r.unit"),
    );
    let mut params = BTreeMap::new();
    params.insert("unit_csv".to_string(), RuleParam::Text(csv(&UNIT_ENUM)));
    params.insert(
        "unit_aliases".to_string(),
        RuleParam::Text(normalize::alias_table_json()),
    );
    params.insert(
        "expected_units".to_string(),
        RuleParam::Text(expected_units.to_string()),
    );
    Rule {
        rule_id: "R-UNIT-001".to_string(),
        indicator_code: None,
        check_column: Some("unit".to_string()),
        rule_kind: RuleKind::UnitCheck,
        sql,
        params,
        basis: basis.to_string(),
        version: version.to_string(),
        severity: Severity::Blocking,
        effective_from: None,
        status: RuleStatus::Active,
        source_file: "（内置规则集）".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 两条单位规则各占一个槽位() {
        let e = compile_unit_enum_rule("2021.1", UNIT_ENUM_BASIS);
        let m = compile_unit_match_rule("2021.1", UNIT_MATCH_BASIS);
        assert_eq!(e.rule_kind, RuleKind::EnumBound);
        assert_eq!(m.rule_kind, RuleKind::UnitCheck);
        // 检查项相同但检查类型不同 → 换一版不会互相顶掉
        assert_eq!(e.check_column.as_deref(), Some("unit"));
        assert_eq!(m.check_column.as_deref(), Some("unit"));
        assert_ne!(e.rule_kind, m.rule_kind);
    }

    #[test]
    fn 取值一律走参数_不拼进_sql() {
        for rule in [
            compile_unit_enum_rule("2021.1", UNIT_ENUM_BASIS),
            compile_unit_match_rule("2021.1", UNIT_MATCH_BASIS),
        ] {
            assert!(rule.sql.contains(":unit_aliases"));
            assert!(rule.sql.contains(":unit_csv"));
            assert!(!rule.sql.contains("例次数"), "别名不得写进 SQL 文本");
            assert!(!rule.sql.contains("MER-TL-01"), "指标编号不得写进 SQL 文本");
            assert!(!rule.basis.is_empty());
        }
        let m = compile_unit_match_rule("2021.1", UNIT_MATCH_BASIS);
        assert!(m.sql.contains(":expected_units"));
        assert_eq!(
            m.params.get("unit_csv").cloned(),
            Some(RuleParam::Text("例,例次,人次,人,%".to_string()))
        );
    }

    #[test]
    fn 别名表参数与口径层内置表逐条一致() {
        let table = normalize::AliasTable::builtin();
        for rule in [
            compile_unit_enum_rule("2021.1", UNIT_ENUM_BASIS),
            compile_unit_match_rule("2021.1", UNIT_MATCH_BASIS),
        ] {
            let raw = match rule.params.get("unit_aliases") {
                Some(RuleParam::Text(t)) => t.clone(),
                _ => panic!("规则 {} 缺别名表参数", rule.rule_id),
            };
            let loaded = normalize::AliasTable::from_json(&raw).expect("别名表应可装载");
            assert_eq!(
                loaded.len(),
                table.len(),
                "规则 {} 的别名表条数对不上",
                rule.rule_id
            );
            assert_eq!(loaded.to_json(), table.to_json());
        }
    }

    #[test]
    fn 期望单位参数只收官方写明单位的指标() {
        let m = compile_unit_match_rule("2021.1", UNIT_MATCH_BASIS);
        let raw = match m.params.get("expected_units") {
            Some(RuleParam::Text(t)) => t.clone(),
            _ => panic!("规则缺期望单位参数"),
        };
        let items: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        // 24 项比率型指标单位都是 %
        assert_eq!(items.len(), 24, "官方写明单位的指标只有 24 项");
        for item in items.iter() {
            assert_eq!(item["unit"].as_str(), Some("%"));
        }
        // 口径待补的三项不在里面 —— 不猜
        let codes: Vec<&str> = items.iter().map(|i| i["code"].as_str().unwrap()).collect();
        for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
            assert!(
                !codes.contains(&code),
                "{} 是口径待补，不该进期望单位表",
                code
            );
        }
    }
}
