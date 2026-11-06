//! 分子分母公式复算检查器。
//!
//! # 这一层管什么
//!
//! 比率型指标（官方 27 项里的 24 项）的填报值按官方公式算出来是唯一的：
//! **「分子 ÷ 分母 × 100%」**。本模块把填报值复算一遍，跟科室填的值对一下。
//!
//! | 情形 | 问题类型 | 错误码 | 规则编号 |
//! |---|---|---|---|
//! | 复算值与填报值对不上（超出该精度的容差） | 公式不符 | — | `R-FORMULA-001` |
//! | 分母为 0 | 数据形态错误 | `DENOMINATOR_ZERO` | `R-FORMULA-002` |
//! | 分子 / 分母 / 填报值 非数值或为负 | 数据形态错误 | `INVALID_NUMERIC` | `R-FORMULA-003` |
//!
//! # 三类问题必须分开
//!
//! 「算错了」与「填坏了」不是一回事，混在一起会让人改错地方：
//!
//! - **公式不符**：数都填了、也都能算，就是算出来的和填的不一样 → 科室要重算填报值；
//! - **分母为 0**：分母本身就不成立，复算无从谈起 → 科室要补分母口径。
//!   把它报成「公式不符」，人就会去调填报值 —— 改错了地方；
//! - **非数值 / 负值**：这一格压根不是数 → 科室要改这一格。
//!
//! 三类各有各的错误码与依据：插件与导出按码分组，不混成一类。
//!
//! # 容差按指标给，不按一个全局常数
//!
//! 比对容差 = 0.5 × 10^-精度，精度由**逐指标精度表**给（规则参数 `precision_table`，
//! 来自官方 27 项口径登记表 [`crate::rules::indicators`]）。本轮官方附件只给了计算公式、
//! 没写填报值保留几位，统一取默认精度（小数点后 1 位）；口径种子落地后按官方文件逐项填。
//!
//! 比较在**整数标度**上做：把复算值与填报值各自放大到精度对应的标度（10^精度）后四舍五入，
//! 再比整数是否相等 —— 浮点边界不会把「恰好差半个末位」的值判成算错。
//!
//! # 与其它检查器的分工
//!
//! - **必填缺失**（[`super::required`]）：分子 / 分母 / 填报值**没填**是必填问题，
//!   本模块不判「没填」；
//! - **单位不符**（[`super::unit`]）：单位与指标配不配，与数值算得对不对无关；
//! - **枚举越界**（[`super::enum_`]）：指标编号在不在册 —— 编号不在册就判不出指标类别，
//!   本模块不套用比率型口径。
//!
//! # 同一行不重复报两类问题
//!
//! 复算只在三个数值列**都填了、且都是非负数**时才做。负值与非数值由数值形态那条规则
//! 单报（`INVALID_NUMERIC`），分母为 0 由另一条单报（`DENOMINATOR_ZERO`）——
//! 一行同时挂「数据形态错误」和「公式不符」会让人以为要改两处，实际只需改那一格。
//!
//! # 人均负担型（MER-HR-01…03）本轮不判
//!
//! 官方给这 3 项的公式是「病历数 ÷（实际工作总月数 × 同期人员数）」，与本产品的最小数据格式
//! （分子 / 分母 / 填报值）对不上 —— 缺「实际工作总月数」与「同期人员数」两个字段。
//! 本轮**不猜**、不套用比率型口径，标「口径待补」（见 [`crate::rules::indicators`]）。

use std::collections::BTreeMap;

use crate::rules::indicators;
use crate::rules::model::{Rule, RuleKind, RuleParam, RuleStatus, Severity};

/// 公式复算规则的依据
pub const FORMULA_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》指标四至指标二十七（MER-TL-01…04、MER-ME-01…03、MER-D&T-01…08、MER-TQ-01…09，共 24 项比率型指标）的计算公式为「分子 ÷ 分母 × 100%」，计量单位为 %；填报值按指标给定的小数位数（默认小数点后 1 位）四舍五入后应与复算值一致，比对容差为该精度末位的一半";

/// 分母为 0 的依据
pub const DENOMINATOR_ZERO_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》指标四至指标二十七（24 项比率型指标）的计算公式为「分子 ÷ 分母 × 100%」；分母为 0 时公式无意义，按数据形态错误单列（错误码 DENOMINATOR_ZERO），不做静默 0 处理，也不报成「公式不符」";

/// 数值形态错误（非数值原文 / 负值）的依据
pub const NUMERIC_SHAPE_BASIS: &str = "本产品最小数据格式与字段要求：分子 / 分母 / 填报值三列为数值列；非数值原文与负值属数据形态错误（错误码 INVALID_NUMERIC），与「公式不符」「分母为 0」分开归类 —— 非数值原文不是「没填」（没填归必填缺失），负值与分母为 0 也不并成一类";

/// 公式复算规则编号
pub const RECALC_RULE_ID: &str = "R-FORMULA-001";

/// 分母为 0 规则编号
pub const DENOMINATOR_ZERO_RULE_ID: &str = "R-FORMULA-002";

/// 数值形态规则编号
pub const NUMERIC_SHAPE_RULE_ID: &str = "R-FORMULA-003";

/// 分母为 0 的机器可读错误码
pub const DENOMINATOR_ZERO_CODE: &str = "DENOMINATOR_ZERO";

/// 数值形态错误的机器可读错误码
pub const INVALID_NUMERIC_CODE: &str = "INVALID_NUMERIC";

/// 三个数值列（本机库列名 / 业务名）
const NUMERIC_COLUMNS: [(&str, &str); 3] = [
    ("numerator", "分子"),
    ("denominator", "分母"),
    ("value", "填报值"),
];

/// 数值列原文表达式（取自 `numeric_raw`；原文不是合法 JSON 时按空处理）
///
/// 原文保留「空」与「非数值」的差别（见 [`crate::ingest`]）—— 规则层据此区分
/// 「没填」（必填缺失）与「填坏了」（数据形态错误）。
fn raw_text_expr(column: &str) -> String {
    format!(
        "COALESCE(CASE WHEN json_valid(r.numeric_raw) THEN json_extract(r.numeric_raw, '$.{column}') END, '')",
        column = column
    )
}

/// 非数值判定：库里存不下有限数值，原文又不是空的、也不是 NaN
///
/// 与 [`super::required`] 的缺失判定互为补集：
/// 「原文为空 / NaN」→ 没填（必填缺失）；「原文有内容但不是数」→ 填坏了（数据形态错误）。
fn non_numeric_condition(column: &str) -> String {
    let raw = raw_text_expr(column);
    format!(
        "r.{column} IS NULL AND {raw} <> '' AND upper({raw}) NOT IN ('NAN', '+NAN', '-NAN')",
        column = column,
        raw = raw
    )
}

/// 比率型指标适用范围（绑定参数 `:ratio_codes`）
fn ratio_scope() -> String {
    "instr(',' || :ratio_codes || ',', ',' || trim(r.indicator_code) || ',') > 0".to_string()
}

/// 编译「公式复算」规则（`R-FORMULA-001`）
///
/// 填报值与复算值对不上就报「公式不符」，并给出**实际值**与**期望取值** ——
/// 只说「不符」科室改不动，他不知道该填多少。
///
/// 复算口径：分子 ÷ 分母 × 100%；比对容差按逐指标精度给（默认小数点后 1 位），
/// 比较在整数标度上做（放大到 10^精度后四舍五入再比整数）。
pub fn compile_formula_recalc_rule(version: &str, basis: &str) -> Rule {
    compile_formula_recalc_rule_with(
        version,
        basis,
        &indicators::precision_table_json(),
        &indicators::ratio_indicator_list(),
    )
}

/// 编译「公式复算」规则（精度表与比率型清单由调用方给）
///
/// 两个参数来源有两个：内置兜底登记表（[`crate::rules::indicators`]）与官方口径种子
/// （[`crate::rules::seed`]）。精度表是**数据**（走绑定参数），改精度不用改规则 SQL。
pub fn compile_formula_recalc_rule_with(
    version: &str,
    basis: &str,
    precision_table: &str,
    ratio_codes: &str,
) -> Rule {
    let sql = format!(
        "WITH p AS (\n\
        \x20 SELECT json_extract(value, '$.code') AS code,\n\
        \x20        json_extract(value, '$.precision') AS precision,\n\
        \x20        json_extract(value, '$.scale') AS scale,\n\
        \x20        json_extract(value, '$.tolerance') AS tolerance\n\
        \x20   FROM json_each(:precision_table)\n\
        \x20), r AS (\n\
        \x20 SELECT row_uid, indicator_code, numerator, denominator, value, numeric_raw\n\
        \x20   FROM rows\n\
        \x20  WHERE batch_ref = :batch_ref\n\
        \x20 )\n\
        \x20SELECT r.row_uid AS row_uid,\n\
        \x20       '填报值与官方公式复算结果不符：按「分子 ÷ 分母 × 100%」复算得 '\n\
        \x20         || printf('%.4f', r.numerator * 100.0 / r.denominator)\n\
        \x20         || '，本行填报值为「' || {raw} || '」，两者相差 '\n\
        \x20         || printf('%.4f', abs(r.value - r.numerator * 100.0 / r.denominator))\n\
        \x20         || '；按指标口径应四舍五入到小数点后 ' || p.precision || ' 位后填「'\n\
        \x20         || printf('%.' || p.precision || 'f', r.numerator * 100.0 / r.denominator)\n\
        \x20         || '」（容差为该精度末位的一半，即 ' || printf('%.4f', p.tolerance)\n\
        \x20         || '）；期望取值：'\n\
        \x20         || printf('%.' || p.precision || 'f', r.numerator * 100.0 / r.denominator)\n\
        \x20         || '；本行不参与上报，请按官方公式重算后修正' AS formula_note\n\
        \x20  FROM r\n\
        \x20  JOIN p ON p.code = trim(r.indicator_code)\n\
        \x20 WHERE {scope}\n\
        \x20   AND r.numerator IS NOT NULL AND r.denominator IS NOT NULL AND r.value IS NOT NULL\n\
        \x20   AND r.denominator <> 0\n\
        \x20   AND r.numerator >= 0 AND r.denominator >= 0 AND r.value >= 0\n\
        \x20   AND CAST(ROUND(r.value * p.scale) AS INTEGER)\n\
        \x20       <> CAST(ROUND(r.numerator * 100.0 * p.scale / r.denominator) AS INTEGER)",
        raw = raw_text_expr("value"),
        scope = ratio_scope(),
    );
    let mut params = BTreeMap::new();
    params.insert(
        "ratio_codes".to_string(),
        RuleParam::Text(ratio_codes.to_string()),
    );
    params.insert(
        "precision_table".to_string(),
        RuleParam::Text(precision_table.to_string()),
    );
    Rule {
        rule_id: RECALC_RULE_ID.to_string(),
        indicator_code: None,
        check_column: Some("value".to_string()),
        rule_kind: RuleKind::FormulaRecalc,
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

/// 编译「分母为 0」规则（`R-FORMULA-002`）
///
/// 分母为 0 时公式无意义：既不静默当 0 处理，也不报成「公式不符」——
/// 分为 0 要改的是分母口径，不是填报值。
pub fn compile_denominator_zero_rule(version: &str, basis: &str) -> Rule {
    let sql = format!(
        "SELECT r.row_uid AS row_uid,\n\
        \x20       '分母为 0，无法复算填报值；请核对该行分母口径（同期病历数等）后重新填报'\n\
        \x20         || '（本行分子「' || {num} || '」、填报值「' || {val} || '」暂不参与复算）' AS shape_note,\n\
        \x20       :error_code AS error_code\n\
        \x20  FROM rows r\n\
        \x20 WHERE r.batch_ref = :batch_ref\n\
        \x20   AND {scope}\n\
        \x20   AND r.denominator IS NOT NULL\n\
        \x20   AND r.denominator = 0",
        num = raw_text_expr("numerator"),
        val = raw_text_expr("value"),
        scope = ratio_scope(),
    );
    let mut params = BTreeMap::new();
    params.insert(
        "ratio_codes".to_string(),
        RuleParam::Text(indicators::ratio_indicator_list()),
    );
    params.insert(
        "error_code".to_string(),
        RuleParam::Text(DENOMINATOR_ZERO_CODE.to_string()),
    );
    Rule {
        rule_id: DENOMINATOR_ZERO_RULE_ID.to_string(),
        indicator_code: None,
        check_column: Some("denominator".to_string()),
        rule_kind: RuleKind::DataShapeError,
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

/// 编译「数值形态」规则（`R-FORMULA-003`）
///
/// 三个数值列各两个分支：**非数值原文**（这一格填的不是数）与**负值**
/// （比率型指标的分子与分母不应为负）。两者都归数据形态错误，错误码 `INVALID_NUMERIC`，
/// 与「公式不符」「分母为 0」分开 —— 报错的地方要和真正要改的地方对得上。
pub fn compile_numeric_shape_rule(version: &str, basis: &str) -> Rule {
    let mut branches: Vec<String> = Vec::new();
    for (column, header) in NUMERIC_COLUMNS.iter() {
        branches.push(format!(
            "SELECT r.row_uid AS row_uid,\n\
            \x20       '{header}「' || {raw} || '」不是合法数值，无法参与公式复算；请把该格改成数值后重新填报' AS shape_note,\n\
            \x20       :error_code AS error_code\n\
            \x20  FROM rows r\n\
            \x20 WHERE r.batch_ref = :batch_ref\n\
            \x20   AND {scope}\n\
            \x20   AND {cond}",
            header = header,
            raw = raw_text_expr(column),
            scope = ratio_scope(),
            cond = non_numeric_condition(column),
        ));
        branches.push(format!(
            "SELECT r.row_uid AS row_uid,\n\
            \x20       '{header}为负值（本行填的是「' || {raw} || '」），比率型指标的分子与分母不应为负；请核对该行口径后重新填报' AS shape_note,\n\
            \x20       :error_code AS error_code\n\
            \x20  FROM rows r\n\
            \x20 WHERE r.batch_ref = :batch_ref\n\
            \x20   AND {scope}\n\
            \x20   AND r.{column} IS NOT NULL\n\
            \x20   AND r.{column} < 0",
            header = header,
            raw = raw_text_expr(column),
            scope = ratio_scope(),
            column = column,
        ));
    }
    let sql = branches.join("\nUNION ALL\n");
    let mut params = BTreeMap::new();
    params.insert(
        "ratio_codes".to_string(),
        RuleParam::Text(indicators::ratio_indicator_list()),
    );
    params.insert(
        "error_code".to_string(),
        RuleParam::Text(INVALID_NUMERIC_CODE.to_string()),
    );
    Rule {
        rule_id: NUMERIC_SHAPE_RULE_ID.to_string(),
        indicator_code: None,
        check_column: Some("numeric".to_string()),
        rule_kind: RuleKind::DataShapeError,
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
    use crate::rules::model::RuleParam;

    #[test]
    fn 三条公式规则各占一个槽位() {
        let recalc = compile_formula_recalc_rule("2021.1", FORMULA_BASIS);
        let zero = compile_denominator_zero_rule("2021.1", DENOMINATOR_ZERO_BASIS);
        let shape = compile_numeric_shape_rule("2021.1", NUMERIC_SHAPE_BASIS);

        assert_eq!(recalc.rule_kind, RuleKind::FormulaRecalc);
        assert_eq!(zero.rule_kind, RuleKind::DataShapeError);
        assert_eq!(shape.rule_kind, RuleKind::DataShapeError);
        // 检查项不同 → 换一版不会互相顶掉
        assert_eq!(recalc.check_column.as_deref(), Some("value"));
        assert_eq!(zero.check_column.as_deref(), Some("denominator"));
        assert_eq!(shape.check_column.as_deref(), Some("numeric"));
        for rule in [&recalc, &zero, &shape] {
            assert_eq!(rule.severity, Severity::Blocking);
            assert!(!rule.basis.is_empty(), "规则 {} 缺依据", rule.rule_id);
        }
    }

    #[test]
    fn 取值一律走参数_不拼进_sql() {
        let recalc = compile_formula_recalc_rule("2021.1", FORMULA_BASIS);
        assert!(recalc.sql.contains(":precision_table"));
        assert!(recalc.sql.contains(":ratio_codes"));
        assert!(
            !recalc.sql.contains("MER-TL-01"),
            "指标编号不得写进 SQL 文本"
        );
        assert!(!recalc.sql.contains("0.05"), "容差不得写进 SQL 文本");
        assert!(
            !recalc.sql.contains("DENOMINATOR_ZERO"),
            "错误码不得写进 SQL 文本"
        );

        let zero = compile_denominator_zero_rule("2021.1", DENOMINATOR_ZERO_BASIS);
        assert!(zero.sql.contains(":error_code"));
        assert_eq!(
            zero.params.get("error_code").cloned(),
            Some(RuleParam::Text(DENOMINATOR_ZERO_CODE.to_string()))
        );

        let shape = compile_numeric_shape_rule("2021.1", NUMERIC_SHAPE_BASIS);
        assert_eq!(
            shape.params.get("error_code").cloned(),
            Some(RuleParam::Text(INVALID_NUMERIC_CODE.to_string()))
        );
        // 三个数值列各两个分支（非数值 / 负值）
        assert_eq!(shape.sql.matches("UNION ALL").count(), 5);
    }

    #[test]
    fn 精度表只收比率型指标且默认精度为一位() {
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&indicators::precision_table_json()).unwrap();
        assert_eq!(items.len(), 24, "官方写明公式的比率型指标共 24 项");
        for item in items.iter() {
            assert_eq!(item["precision"].as_u64(), Some(1));
            assert_eq!(item["scale"].as_i64(), Some(10));
            assert_eq!(item["tolerance"].as_f64(), Some(0.05));
            let code = item["code"].as_str().unwrap();
            assert!(
                indicators::is_ratio(code),
                "{} 不是比率型指标，不该进精度表",
                code
            );
        }
        // 人均负担型不在表里 —— 它的官方公式与本产品字段对不上，不套用
        for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
            assert!(
                !items.iter().any(|i| i["code"].as_str() == Some(code)),
                "{} 本轮不判公式复算",
                code
            );
        }
    }

    #[test]
    fn 容差由精度算出来_不是一个全局常数() {
        assert_eq!(indicators::tolerance_of(1), 0.05);
        assert_eq!(indicators::tolerance_of(2), 0.005);
        assert_eq!(indicators::scale_of(1), 10);
        assert_eq!(indicators::scale_of(3), 1000);
        assert_eq!(indicators::precision_of("MER-TL-01"), 1);
    }
}
