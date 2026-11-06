//! 必填缺失与表内唯一性检查器。
//!
//! # 五列必填
//!
//! 指标编号 / 指标名称 / 科室代码 / 报告期 / 业务主键 —— 任何一行都必填。
//! 缺哪一列就报哪一列；一行缺多列时合并成一张清单写进「问题说明（含依据）」，
//! 让科室一次改完，不用一行一行地改。
//!
//! # 比率型指标的条件必填
//!
//! 比率型指标按官方计算公式还必须有 分子 / 分母 / 填报值 / 计量单位。
//! 比率型指标清单见 [`RATIO_INDICATORS`]，依据国卫办医函〔2021〕28号附件的指标分类：
//! 指标四至指标二十七共 24 项的计算公式都是「分子 ÷ 分母 × 100%」（计量单位 %）；
//! 指标一至指标三（MER-HR-01…03）是人力资源配置指标（人均负担），不适用本口径。
//!
//! 条件必填**只在比率型指标上生效**：非比率型指标没有分子分母，不报。
//!
//! # 空值语义
//!
//! | 单元格里的东西 | 判定 | 理由 |
//! |---|---|---|
//! | 空串 | **缺失** | 没填 |
//! | 纯空白（含全角空格、不间断空格、Tab） | **缺失** | 等同没填 |
//! | 未填（None） | **缺失** | 没填 |
//! | `NaN` | **缺失** | 官方口径里没有「NaN」这个取值，它不是填报结果 |
//! | `0` | **不是缺失** | 0 是有效填报值（本期零事件），不能当没填 |
//! | 非数值原文（如 `abc`） | **不是缺失** | 单元格填了 —— 是**格式错误**，归公式复算检查器（Task 13，报 `INVALID_NUMERIC`） |
//!
//! 两条容易搞反的：
//!
//! - **0 不当缺失。** 分子为 0 是真实业务取值。若把 0 当缺失，「分母为 0」
//!   （错误码 `DENOMINATOR_ZERO`，Task 13）会被「必填缺失」先一步吃掉，
//!   两个错误码永远见不到面 —— 报错的地方和真正要改的地方对不上。
//! - **不得用 0 顶替空值。** 空就是空：判定用「这一列有没有值」，
//!   不做 `COALESCE(列, 0)` 之类的兜底，也不做「0 值不算缺失」的宽松处理。
//!
//! # 表内业务主键唯一
//!
//! 同一张来源表里，业务主键（指标编号 + 科室代码 + 报告期 + 业务主键）必须唯一。
//! 表内撞键一律标**疑似重复**（提示级），交重复检测漏斗（Task 20 的 L1 确定性键）判定：
//! 取值完全一致 → 疑似重复；取值不同 → 待人工判定。**本轮不合并、不上报。**
//!
//! 跨表撞键不由这条规则管（那是实体绑定层的事，见 [`crate::bind`]）——
//! 这一条只管「同一张表里的同一件事被填了两遍」。

use std::collections::BTreeMap;

use crate::rules::indicators;
use crate::rules::model::{Rule, RuleKind, RuleParam, RuleStatus, Severity};

/// 比率型指标编号清单（24 项）。
///
/// 清单本体在 [`crate::rules::indicators`]（官方 27 项口径登记表），
/// 这里转出来是为了保持本模块既有的引用路径不变 —— 名单只有一份。
pub use crate::rules::indicators::RATIO_INDICATORS;

/// 列的类别：决定「什么算缺失」
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// 文本列：空串与纯空白算缺失
    Text,
    /// 数值列：空白与 NaN 算缺失；非数值原文不算缺失（那是格式错误）
    Numeric,
}

/// 一列必填口径
#[derive(Debug, Clone, Copy)]
pub struct RequiredColumn {
    /// 业务名（写进「问题说明（含依据）」列，给科室看）
    pub header: &'static str,
    /// 本机库列名
    pub column: &'static str,
    /// 类别
    pub kind: ColumnKind,
}

/// 五列必填（任何一行都必填）
pub const REQUIRED_COLUMNS: [RequiredColumn; 5] = [
    RequiredColumn {
        header: "指标编号",
        column: "indicator_code",
        kind: ColumnKind::Text,
    },
    RequiredColumn {
        header: "指标名称",
        column: "indicator_name",
        kind: ColumnKind::Text,
    },
    RequiredColumn {
        header: "科室代码",
        column: "dept_code",
        kind: ColumnKind::Text,
    },
    RequiredColumn {
        header: "报告期",
        column: "reporting_period",
        kind: ColumnKind::Text,
    },
    RequiredColumn {
        header: "业务主键",
        column: "business_key",
        kind: ColumnKind::Text,
    },
];

/// 比率型指标的条件必填列
pub const CONDITIONAL_COLUMNS: [RequiredColumn; 4] = [
    RequiredColumn {
        header: "分子",
        column: "numerator",
        kind: ColumnKind::Numeric,
    },
    RequiredColumn {
        header: "分母",
        column: "denominator",
        kind: ColumnKind::Numeric,
    },
    RequiredColumn {
        header: "填报值",
        column: "value",
        kind: ColumnKind::Numeric,
    },
    RequiredColumn {
        header: "计量单位",
        column: "unit",
        kind: ColumnKind::Text,
    },
];

/// 比率型指标编号清单（24 项）的说明见 [`crate::rules::indicators::RATIO_INDICATORS`]。

/// 是否比率型指标（决定条件必填是否对该行生效）
///
/// 判定查官方 27 项口径登记表：比率型 → 是；人均负担型 → 否；
/// 编号不在册（类别未知）→ 否 —— 此时先报「指标编号不在册」，
/// 不按猜出来的口径套条件必填。
pub fn is_ratio_indicator(code: &str) -> bool {
    indicators::is_ratio(code)
}

/// 比率型指标编号清单（绑定参数用，逗号分隔）
pub fn ratio_indicator_list() -> String {
    indicators::ratio_indicator_list()
}

/// 五列必填规则的依据
pub const REQUIRED_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》（27 项指标编号与名称）；科室代码 / 报告期 / 业务主键为本产品行标识字段（最小数据格式与字段要求）";

/// 比率型指标条件必填规则的依据
pub const CONDITIONAL_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》指标四至指标二十七（MER-TL-01…04、MER-ME-01…03、MER-D&T-01…08、MER-TQ-01…09，共 24 项比率型指标）的计算公式均为「分子 ÷ 分母 × 100%」，计量单位为 %，故分子 / 分母 / 填报值 / 计量单位为条件必填；指标一至指标三（MER-HR-01…03）为人力资源配置指标，不适用本口径";

/// 表内业务主键唯一规则的依据
pub const TABLE_UNIQUE_BASIS: &str = "本产品最小数据格式与字段要求（业务主键 = 指标编号 + 科室代码 + 报告期 + 业务主键，同一来源表内唯一）；取值是否一致由重复检测漏斗判定，本轮只标「疑似重复」，不合并、不上报";

/// 文本列的缺失判定：空串与纯空白算缺失。
///
/// 纯空白含全角空格（U+3000）、不间断空格（U+00A0）与 Tab —— 表里常见「看着空、其实有字符」。
pub fn is_missing_text(raw: &str) -> bool {
    raw.trim().is_empty()
}

/// 数值列的缺失判定：空白与 NaN 算缺失；非数值原文**不算**缺失。
///
/// 与 SQL 里的缺失表达式一一对应（见 [`numeric_missing_expr`]）：
/// 空白 → 缺失；`NaN` / `+NaN` / `-NaN` → 缺失；`abc` 这类非数值原文 → 不是缺失，
/// 那是格式错误，交给公式复算检查器（Task 13）报 `INVALID_NUMERIC`。
pub fn is_missing_numeric_text(raw: &str) -> bool {
    let t = raw.trim();
    t.is_empty() || matches!(t.to_ascii_uppercase().as_str(), "NAN" | "+NAN" | "-NAN")
}

/// 已解析数值的缺失判定：空值或非有限数（NaN / 无穷）算缺失。
///
/// 注意：解析不出来的**非数值原文**在这里与空值无法区分 —— 要区分请用
/// [`is_missing_numeric_text`]（判定依据是原文）。
pub fn is_missing_number(value: Option<f64>) -> bool {
    match value {
        None => true,
        Some(v) => !v.is_finite(),
    }
}

/// 视为空白的字符集（SQL 表达式）
///
/// 定义在口径归一化层（[`crate::rules::normalize`]）—— 各层共用一份，
/// 漏改一处就会出现「一处当空白、一处不当空白」的差异。
pub use crate::rules::normalize::BLANK_CHARS_SQL;

/// 文本列的缺失表达式（SQL）
fn text_missing_expr(column: &str) -> String {
    format!(
        "({column} IS NULL OR trim({column}, {blank}) = '')",
        column = column,
        blank = BLANK_CHARS_SQL
    )
}

/// 数值列的缺失表达式（SQL）
///
/// 数值列有三种「不像数」的写法，处理方式不同：
///
/// | 原值 | 库里数值列 | 判定 |
/// |---|---|---|
/// | 未填 / 纯空白 | 空 | **缺失** |
/// | `NaN` | 空 | **缺失**（官方口径里没有这个取值） |
/// | `abc` | 空 | **不是缺失** —— 填了，是格式错误（Task 13 报 `INVALID_NUMERIC`） |
///
/// 库里数值列存的是数值（存不到有限数值时为空），数值原文另存 `numeric_raw`
/// （JSON，键与列名同名）。取原文用 `json_extract`；原文不是合法 JSON 时按缺失处理
/// —— 判不出来时宁可多报一条缺失，不可把没填的行放过去。
fn numeric_missing_expr(column: &str) -> String {
    format!(
        "({column} IS NULL AND (CASE WHEN json_valid(numeric_raw) \
         THEN COALESCE(json_extract(numeric_raw, '$.{column}'), '') = '' \
           OR upper(COALESCE(json_extract(numeric_raw, '$.{column}'), '')) IN ('NAN', '+NAN', '-NAN') \
         ELSE 1 END))",
        column = column
    )
}

/// 一列的缺失表达式
pub fn missing_expr(col: &RequiredColumn) -> String {
    match col.kind {
        ColumnKind::Text => text_missing_expr(col.column),
        ColumnKind::Numeric => numeric_missing_expr(col.column),
    }
}

/// 必填检查 SQL 的一个分支：一行缺哪一列，命中哪个分支。
///
/// 每个分支查一遍表，把缺的那一列名（走命名参数 `:label_n`）带出来；
/// 同一行命中多个分支时，执行器按行归并成一条问题，缺失列合并成清单。
fn missing_branch(scope: &str, expr: &str, label_param: &str) -> String {
    format!(
        "SELECT row_uid AS row_uid, :{label} AS missing_column\n  FROM rows\n \
         WHERE batch_ref = :batch_ref\n   AND {scope}\n   AND {expr}",
        label = label_param,
        scope = scope,
        expr = expr
    )
}

/// 拼一整段必填检查 SQL（多个分支 UNION ALL）
fn missing_branches(
    columns: &[RequiredColumn],
    scope: &str,
) -> (String, BTreeMap<String, RuleParam>) {
    let mut sql = String::new();
    let mut params = BTreeMap::new();
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            sql.push_str("\nUNION ALL\n");
        }
        let label = format!("label_{}", i + 1);
        sql.push_str(&missing_branch(scope, &missing_expr(col), &label));
        params.insert(label.clone(), RuleParam::Text(col.header.to_string()));
    }
    (sql, params)
}

/// 编译「五列必填」规则
///
/// 适用全部指标：指标编号 / 指标名称 / 科室代码 / 报告期 / 业务主键。
pub fn compile_required_rule(version: &str, basis: &str) -> Rule {
    let (sql, mut params) = missing_branches(
        &REQUIRED_COLUMNS,
        "(:scope IS NULL OR indicator_code = :scope)",
    );
    params.insert("scope".to_string(), RuleParam::Null);
    Rule {
        rule_id: "R-REQUIRED-001".to_string(),
        indicator_code: None,
        check_column: None,
        rule_kind: RuleKind::RequiredField,
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

/// 编译「比率型指标条件必填」规则
///
/// 只在比率型指标（见 [`RATIO_INDICATORS`]）上生效：这些指标按官方公式必须有
/// 分子 / 分母 / 填报值 / 计量单位。非比率型指标不报。
///
/// 比率型指标清单走命名参数 `:ratio_codes`（取值不拼进 SQL 文本），
/// 判定用 `instr` 做字面量包含 —— 指标编号里的 `&`、全角字符都不会被当成通配符。
pub fn compile_conditional_required_rule(version: &str, basis: &str) -> Rule {
    compile_conditional_required_rule_with_ratio_codes(version, basis, &ratio_indicator_list())
}

/// 编译「比率型指标条件必填」规则（比率型清单由调用方给）
///
/// 清单来源有两个：内置兜底登记表（[`crate::rules::indicators`]）与官方口径种子
/// （[`crate::rules::seed`]）。两者是同一份口径的两种形态，编译出的规则逐字一致 ——
/// `tests/seed_test.rs` 对此有断言。
pub fn compile_conditional_required_rule_with_ratio_codes(
    version: &str,
    basis: &str,
    ratio_codes: &str,
) -> Rule {
    let scope = "instr(',' || :ratio_codes || ',', ',' || indicator_code || ',') > 0";
    let (sql, mut params) = missing_branches(&CONDITIONAL_COLUMNS, scope);
    params.insert(
        "ratio_codes".to_string(),
        RuleParam::Text(ratio_codes.to_string()),
    );
    Rule {
        rule_id: "R-REQUIRED-002".to_string(),
        indicator_code: None,
        check_column: None,
        rule_kind: RuleKind::ConditionalRequired,
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

/// 编译「表内业务主键唯一」规则
///
/// 同一来源表内业务主键（指标编号 + 科室代码 + 报告期 + 业务主键）撞键 → 标「疑似重复」。
///
/// 严重度是**提示级**：撞键只是候选，最终判定在重复检测漏斗（Task 20）与人工确认
/// （Task 36）；上报时同一业务键的幂等键相同，网关侧也不会重复收。
pub fn compile_table_unique_rule(version: &str, basis: &str) -> Rule {
    let sql = TABLE_UNIQUE_SQL.to_string();
    Rule {
        rule_id: "R-UNIQUE-001".to_string(),
        indicator_code: None,
        check_column: None,
        rule_kind: RuleKind::SuspectedDuplicate,
        sql,
        params: BTreeMap::new(),
        basis: basis.to_string(),
        version: version.to_string(),
        severity: Severity::Hint,
        effective_from: None,
        status: RuleStatus::Active,
        source_file: "（内置规则集）".to_string(),
    }
}

/// 表内业务主键唯一的检查 SQL
///
/// 只看**同一来源表内**（`source_ns` 相同）的撞键；跨表撞键由实体绑定层管。
/// 键要素不全的行不参与（它缺的是必填列，不该再报一次重复）。
const TABLE_UNIQUE_SQL: &str = r#"
SELECT r.row_uid AS row_uid,
       '疑似重复：同一来源表「' || r.source_ns || '」内业务主键（指标编号 ' || r.indicator_code
       || '｜科室代码 ' || r.dept_code || '｜报告期 ' || r.reporting_period
       || '｜业务主键 ' || r.business_key || '）重复，对照行：'
       || (SELECT GROUP_CONCAT(o.source_ns || ' 第 ' || o.row_no || ' 行', '、')
            FROM rows o
           WHERE o.batch_ref = r.batch_ref
             AND o.source_ns = r.source_ns
             AND o.row_uid <> r.row_uid
             AND o.indicator_code IS r.indicator_code
             AND o.dept_code IS r.dept_code
             AND o.reporting_period IS r.reporting_period
             AND o.business_key IS r.business_key)
       || '；取值是否一致交重复检测判定，本轮不合并、不上报' AS dup_note
  FROM rows r
 WHERE r.batch_ref = :batch_ref
   AND r.indicator_code IS NOT NULL AND trim(r.indicator_code) <> ''
   AND r.dept_code IS NOT NULL AND trim(r.dept_code) <> ''
   AND r.reporting_period IS NOT NULL AND trim(r.reporting_period) <> ''
   AND r.business_key IS NOT NULL AND trim(r.business_key) <> ''
   AND EXISTS (SELECT 1 FROM rows o
                WHERE o.batch_ref = r.batch_ref
                  AND o.source_ns = r.source_ns
                  AND o.row_uid <> r.row_uid
                  AND o.indicator_code IS r.indicator_code
                  AND o.dept_code IS r.dept_code
                  AND o.reporting_period IS r.reporting_period
                  AND o.business_key IS r.business_key)
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 五列必填与四列条件必填定义固定() {
        assert_eq!(REQUIRED_COLUMNS.len(), 5);
        assert_eq!(REQUIRED_COLUMNS[0].header, "指标编号");
        assert_eq!(REQUIRED_COLUMNS[4].column, "business_key");
        assert_eq!(CONDITIONAL_COLUMNS.len(), 4);
        assert_eq!(CONDITIONAL_COLUMNS[3].header, "计量单位");
        for col in CONDITIONAL_COLUMNS.iter() {
            assert!(
                col.kind == ColumnKind::Numeric || col.column == "unit",
                "条件必填里除计量单位外都应是数值列"
            );
        }
    }

    #[test]
    fn 比率型指标清单覆盖官方附件的二十四项且不含人力资源指标() {
        assert_eq!(RATIO_INDICATORS.len(), 24);
        assert!(is_ratio_indicator("MER-TL-04"));
        assert!(is_ratio_indicator("MER-D&T-08"));
        assert!(is_ratio_indicator("MER-TQ-09"));
        // 人力资源配置指标不是比率型
        assert!(!is_ratio_indicator("MER-HR-01"));
        assert!(!is_ratio_indicator("MER-HR-02"));
        assert!(!is_ratio_indicator("MER-HR-03"));
        // 清单里不出现重复
        let mut sorted = RATIO_INDICATORS.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), RATIO_INDICATORS.len());
    }

    #[test]
    fn 空值语义_空白与_nan_算缺失_零与非数值原文不算() {
        assert!(is_missing_text(""));
        assert!(is_missing_text("   "));
        assert!(is_missing_text("\u{3000}")); // 全角空格
        assert!(is_missing_text("\u{00A0}")); // 不间断空格
        assert!(!is_missing_text("0"));
        assert!(!is_missing_text(" 指标一 "));

        assert!(is_missing_numeric_text(""));
        assert!(is_missing_numeric_text("  "));
        assert!(is_missing_numeric_text("NaN"));
        assert!(is_missing_numeric_text(" nan "));
        assert!(!is_missing_numeric_text("0"));
        assert!(!is_missing_numeric_text("12.5"));
        // 非数值原文不是「缺失」，是格式错误
        assert!(!is_missing_numeric_text("abc"));
    }

    #[test]
    fn 缺失表达式_文本列看空值_数值列额外排除非数值原文() {
        let text_expr = missing_expr(&REQUIRED_COLUMNS[0]);
        assert!(text_expr.contains("indicator_code IS NULL"));
        // 空白字符集要含全角空格与不间断空格：SQLite 自带的 trim() 只去 ASCII 空格
        assert!(text_expr.contains("trim(indicator_code,"));
        assert!(text_expr.contains("char(12288)"), "全角空格要算空白");
        assert!(text_expr.contains("char(160)"), "不间断空格要算空白");

        let num_expr = missing_expr(&CONDITIONAL_COLUMNS[0]);
        assert!(num_expr.contains("numerator IS NULL"));
        assert!(num_expr.contains("json_extract(numeric_raw, '$.numerator')"));
        assert!(num_expr.contains("'NAN'"));
    }

    #[test]
    fn 编译出的规则_口径_版本_依据齐备且取值不拼进_sql() {
        let r = compile_required_rule("2021.1", REQUIRED_BASIS);
        assert_eq!(r.rule_kind, RuleKind::RequiredField);
        assert_eq!(r.severity, Severity::Blocking);
        assert!(!r.basis.is_empty());
        assert_eq!(
            r.params.get("label_1").map(|p| p.clone()),
            Some(RuleParam::Text("指标编号".to_string()))
        );
        assert_eq!(
            r.params.get("scope").map(|p| p.clone()),
            Some(RuleParam::Null)
        );
        // 取值一律走参数，不进 SQL 文本
        assert!(!r.sql.contains("指标编号"));

        let c = compile_conditional_required_rule("2021.1", CONDITIONAL_BASIS);
        assert_eq!(c.rule_kind, RuleKind::ConditionalRequired);
        assert!(c.sql.contains(":ratio_codes"));
        assert!(!c.sql.contains("MER-TL-01"));
        assert!(c
            .params
            .get("ratio_codes")
            .and_then(|p| match p {
                RuleParam::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap()
            .contains("MER-TL-01"));

        let u = compile_table_unique_rule("2021.1", TABLE_UNIQUE_BASIS);
        assert_eq!(u.rule_kind, RuleKind::SuspectedDuplicate);
        assert_eq!(u.severity, Severity::Hint);
        assert!(u.sql.contains("GROUP_CONCAT"));
    }
}
