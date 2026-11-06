//! 枚举越界检查器。
//!
//! # 这一层管什么
//!
//! 表里的「取值」有两种查法：一种是**值域枚举**（报告期格式、报告期类型、计量单位、
//! 指标编号各有各的合法取值清单），一种是**目录型枚举**（科室代码必须能在院内 HIS
//! 科室目录里查到）。两种都归到本模块，共**五类**。
//!
//! | 维度 | 列 | 判定方式 | 期望取值 |
//! |---|---|---|---|
//! | 报告期格式 | `reporting_period` | 值域枚举 | `2026` / `2026-H1` / `2026-Q1…Q4` / `2026-M01…M12` |
//! | 报告期类型 | `period_type` | 值域枚举 | 年报 / 半年报 / 季报 / 月报 / 实时报 |
//! | 计量单位 | `unit` | 值域枚举 | 例 / 例次 / 人次 / 人 / % |
//! | 指标编号 | `indicator_code` | 值域枚举 | 官方 27 项编号 |
//! | 科室代码 | `dept_code` | 目录型枚举 | 院内 HIS 科室目录里已登记的科室代码 |
//!
//! 计量单位这一维的**规则编译**在 [`super::unit`]（它要带别名归一：同一单位的
//! 不同写法先归一再判），本模块只保留维度清单与依据 —— 期望取值与依据只有一份。
//!
//! 科室代码这一维的依据与期望取值只留一处（[`crate::bind::dept`]），
//! 规则清单与问题行用的是同一条。
//!
//! # 报错必须给出「期望取值」
//!
//! 只说「不在枚举内」科室是改不动的 —— 他不知道该改成什么。
//! 因此每条枚举规则的问题说明都带**期望取值清单**（见 [`EnumDimension::expected`]），
//! 清单写进「问题说明（含依据）」列，科室照着改一遍就行。
//!
//! # 取值不拼进 SQL
//!
//! 合法取值清单、27 项指标编号这类「口径取值」一律走命名参数绑定（`Rule::params`），
//! SQL 文本里只有比对逻辑。改了口径取值不用改代码，规则表里改一个取值即可。
//!
//! # 与其它检查器的分工
//!
//! - **单位不符**（[`super::unit`]）：单位**在枚举内但与该指标不匹配**
//!   （比率型指标单位必须是 %）—— 本层只管「写没写错」，不管「配不配」；
//!   计量单位这一维的枚举检查本身也在那一层（带别名归一）；
//! - **公式复算**（Task 13，`formula.rs`）：数值算得对不对；
//! - **条件必填**：按指标类别判定（见下）。
//!
//! # 条件必填：按指标类别判定
//!
//! 比率型指标必须有 分子 与 分母；人均负担型指标不适用这套口径（它也有分子分母，
//! 但含义是「病历数」与「人月数」，由 Task 13 的公式复算单独判）。
//!
//! 判定依据是 [`crate::rules::indicators`] 的官方 27 项登记表：
//!
//! | 指标编号 | 类别 | 条件必填 |
//! |---|---|---|
//! | `MER-TL-04`（比率型） | 比率型 | **生效** —— 缺 分子 / 分母 要报 |
//! | `MER-HR-01`（人均负担型） | 人均负担型 | **不生效** —— 不误报 |
//! | `MER-XX-99`（不在册） | 类别未知 | **不生效** —— 先报「指标编号不在册」 |
//!
//! 最后一行是关键：**编号不在官方清单里，就无法判断这个指标是不是比率型**，
//! 这时候套条件必填是猜口径。此时报「指标编号不在册」（本模块的值域枚举），
//! 让科室先把编号改对 —— 比按猜出来的口径报一堆错要强。

use std::collections::BTreeMap;

use crate::bind::dept::{DeptCheck, DeptDirectory};
use crate::ingest::SourceRow;
use crate::rules::indicators;
use crate::rules::model::{Rule, RuleKind, RuleParam, RuleStatus, Severity};

use super::required::BLANK_CHARS_SQL;

/// 科室代码（目录型枚举）的依据 —— 与 [`crate::bind::dept`] 同一条，不另写一份
pub use crate::bind::dept::DEPT_BASIS;

/// 科室代码（目录型枚举）的期望取值 —— 同样只留一处
pub use crate::bind::dept::DEPT_EXPECTED_VALUES;

/// 报告期类型的合法取值
pub const PERIOD_TYPE_ENUM: [&str; 5] = ["年报", "半年报", "季报", "月报", "实时报"];

/// 计量单位的合法取值
///
/// 计量单位这一类枚举越界归 [`super::unit`] 管（它要带别名归一），定义与依据在那里，
/// 这里转出来是为了保持本模块既有的引用路径不变 —— 值只有一份。
pub use super::unit::UNIT_ENUM;

/// 报告期格式检查的依据
pub const PERIOD_FORMAT_BASIS: &str = "本产品最小数据格式与字段要求「报告期」列（形如 2026 / 2026-H1 / 2026-Q3 / 2026-M03；报告期类型口径见《全国卫生资源与医疗服务统计调查制度》）";

/// 报告期类型检查的依据
pub const PERIOD_TYPE_BASIS: &str = "本产品最小数据格式与字段要求「报告期」列说明（报告期类型 ∈ 年报 / 半年报 / 季报 / 月报 / 实时报，依据《全国卫生资源与医疗服务统计调查制度》）";

/// 计量单位枚举检查的依据
///
/// 同上：定义在 [`super::unit`]，这里只转出路径。
pub use super::unit::UNIT_ENUM_BASIS;

/// 指标编号在册检查的依据
pub const INDICATOR_CODE_BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》（指标一至指标二十七，共 27 项；编号与名称以官方附件为准）";

/// 枚举维度的判定方式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnumDimensionKind {
    /// 值域枚举：取值必须落在枚举清单内
    ValueEnum,
    /// 目录型枚举：取值必须能在院内主数据目录里查到
    DirectoryEnum,
}

impl EnumDimensionKind {
    /// 中文名（写进问题说明与规则清单）
    pub fn as_str(self) -> &'static str {
        match self {
            EnumDimensionKind::ValueEnum => "值域枚举",
            EnumDimensionKind::DirectoryEnum => "目录型枚举",
        }
    }
}

/// 一个枚举维度
///
/// 「期望取值」写在这里，是给科室看的清单的唯一出处 —— 规则 SQL 里出现的同一份清单
/// 由绑定参数带进去，两处同源。
#[derive(Debug, Clone, Copy)]
pub struct EnumDimension {
    /// 规则编号（目录型维度由实体绑定层的 `R-DEPT-001` 承担）
    pub code: &'static str,
    /// 维度中文名
    pub name: &'static str,
    /// 本机库列名
    pub column: &'static str,
    /// 判定方式
    pub kind: EnumDimensionKind,
    /// 期望取值清单（中文，给科室看）
    pub expected: &'static str,
    /// 依据出处
    pub basis: &'static str,
}

/// 五类枚举越界维度
pub const ENUM_DIMENSIONS: [EnumDimension; 5] = [
    EnumDimension {
        code: "R-ENUM-001",
        name: "报告期格式",
        column: "reporting_period",
        kind: EnumDimensionKind::ValueEnum,
        expected: "2026（年报，格式 YYYY）/ 2026-H1（半年报，格式 YYYY-H1）/ 2026-Q1…2026-Q4（季报，格式 YYYY-Qn）/ 2026-M01…2026-M12（月报，格式 YYYY-Mnn）",
        basis: PERIOD_FORMAT_BASIS,
    },
    EnumDimension {
        code: "R-ENUM-002",
        name: "报告期类型",
        column: "period_type",
        kind: EnumDimensionKind::ValueEnum,
        expected: "年报 / 半年报 / 季报 / 月报 / 实时报",
        basis: PERIOD_TYPE_BASIS,
    },
    EnumDimension {
        code: "R-ENUM-003",
        name: "计量单位",
        column: "unit",
        kind: EnumDimensionKind::ValueEnum,
        expected: "例 / 例次 / 人次 / 人 / %",
        basis: UNIT_ENUM_BASIS,
    },
    EnumDimension {
        code: "R-ENUM-004",
        name: "指标编号",
        column: "indicator_code",
        kind: EnumDimensionKind::ValueEnum,
        expected: "MER-HR-01…MER-HR-03、MER-TL-01…MER-TL-04、MER-ME-01…MER-ME-03、MER-D&T-01…MER-D&T-08、MER-TQ-01…MER-TQ-09（共 27 项）",
        basis: INDICATOR_CODE_BASIS,
    },
    EnumDimension {
        code: "R-DEPT-001",
        name: "科室代码",
        column: "dept_code",
        kind: EnumDimensionKind::DirectoryEnum,
        expected: DEPT_EXPECTED_VALUES,
        basis: DEPT_BASIS,
    },
];

/// 取某个维度的期望取值清单
pub fn expected_values(code: &str) -> Option<&'static str> {
    ENUM_DIMENSIONS
        .iter()
        .find(|d| d.code == code)
        .map(|d| d.expected)
}

/// 逗号分隔的合法取值清单（绑定参数用）
fn csv(values: &[&str]) -> String {
    values.join(",")
}

/// 编译「报告期格式」枚举规则
///
/// 合法写法四种：`YYYY`（年报）、`YYYY-H1`（半年报）、`YYYY-Qn`（季报，n ∈ 1..4）、
/// `YYYY-Mnn`（月报，nn ∈ 01..12）。其余写法（如 `2026Q3`、`2026 年第三季度`、
/// `2026-Q5`、`2026-M13`）一律报枚举越界，并给出期望取值清单。
pub fn compile_reporting_period_format_rule(version: &str, basis: &str) -> Rule {
    let sql = format!(
        "SELECT row_uid AS row_uid,\n\
        \x20      '报告期格式「' || trim(reporting_period, {blank}) || '」的写法不在枚举内；期望取值：' || :expected_values\n\
        \x20  FROM rows\n\
        \x20 WHERE batch_ref = :batch_ref\n\
        \x20   AND reporting_period IS NOT NULL\n\
        \x20   AND trim(reporting_period, {blank}) <> ''\n\
        \x20   AND NOT (\n\
        \x20        trim(reporting_period, {blank}) GLOB '[0-9][0-9][0-9][0-9]'\n\
        \x20     OR trim(reporting_period, {blank}) GLOB '[0-9][0-9][0-9][0-9]-H1'\n\
        \x20     OR trim(reporting_period, {blank}) GLOB '[0-9][0-9][0-9][0-9]-Q[1-4]'\n\
        \x20     OR trim(reporting_period, {blank}) GLOB '[0-9][0-9][0-9][0-9]-M0[1-9]'\n\
        \x20     OR trim(reporting_period, {blank}) GLOB '[0-9][0-9][0-9][0-9]-M1[0-2]'\n\
        \x20   )",
        blank = BLANK_CHARS_SQL,
    );
    let mut params = BTreeMap::new();
    params.insert(
        "expected_values".to_string(),
        RuleParam::Text(ENUM_DIMENSIONS[0].expected.to_string()),
    );
    Rule {
        rule_id: "R-ENUM-001".to_string(),
        indicator_code: None,
        check_column: Some("reporting_period".to_string()),
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

/// 报告期写法是否在枚举内（与 [`compile_reporting_period_format_rule`] 的 SQL 判据一致）。
///
/// 合法写法：`2026` / `2026-H1` / `2026-Q1`…`2026-Q4` / `2026-M01`…`2026-M12`。
/// 其余写法（`2026Q3`、`2026 年第三季度`、`2026-Q5`、`2026-M13`、`26-Q3`）一律不在枚举内。
///
/// **唯一出处**：SQL 规则与本函数是同一套判据 —— 报告期分组、催报对照都靠它，
/// 不能另写一份「看起来差不多」的判断（两处判据一宽一紧，催报就会漏人或冤枉人）。
pub fn is_valid_reporting_period(raw: &str) -> bool {
    let value = raw.trim_matches(|c: char| crate::rules::normalize::BLANK_CHARS.contains(&c));
    let bytes = value.as_bytes();
    let digits = |slice: &[u8]| slice.iter().all(|b| b.is_ascii_digit());
    match bytes.len() {
        4 => digits(bytes),
        7 => {
            digits(&bytes[0..4])
                && ((bytes[4] == b'-' && bytes[5] == b'H' && bytes[6] == b'1')
                    || (bytes[4] == b'-'
                        && bytes[5] == b'Q'
                        && (b'1'..=b'4').contains(&bytes[6])))
        }
        8 => {
            digits(&bytes[0..4])
                && bytes[4] == b'-'
                && bytes[5] == b'M'
                && bytes[6].is_ascii_digit()
                && ((bytes[6] == b'0' && (b'1'..=b'9').contains(&bytes[7]))
                    || (bytes[6] == b'1' && (b'0'..=b'2').contains(&bytes[7])))
        }
        _ => false,
    }
}

/// 编译「报告期类型」枚举规则
///
/// 「报告期类型」是可选列：表里没有这一列时不报（不猜），有这一列时取值必须落在
/// 年报 / 半年报 / 季报 / 月报 / 实时报 之内。
pub fn compile_reporting_period_type_rule(version: &str, basis: &str) -> Rule {
    let sql = format!(
        "SELECT row_uid AS row_uid,\n\
        \x20      '报告期类型「' || trim(period_type, {blank}) || '」不在枚举内；期望取值：'\n\
        \x20      || replace(:period_type_csv, ',', ' / ')\n\
        \x20  FROM rows\n\
        \x20 WHERE batch_ref = :batch_ref\n\
        \x20   AND period_type IS NOT NULL\n\
        \x20   AND trim(period_type, {blank}) <> ''\n\
        \x20   AND instr(',' || :period_type_csv || ',', ',' || trim(period_type, {blank}) || ',') = 0",
        blank = BLANK_CHARS_SQL,
    );
    let mut params = BTreeMap::new();
    params.insert(
        "period_type_csv".to_string(),
        RuleParam::Text(csv(&PERIOD_TYPE_ENUM)),
    );
    Rule {
        rule_id: "R-ENUM-002".to_string(),
        indicator_code: None,
        check_column: Some("period_type".to_string()),
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

/// 编译「计量单位」枚举规则
///
/// 计量单位这一类归 [`super::unit`] 管 —— 它要带别名归一（同一单位的不同写法
/// 视为同单位），归一逻辑与规则编译必须同源，所以规则本身也在那边编译。
/// 这里转出路径，既有引用不必改。
pub use super::unit::compile_unit_enum_rule;

/// 编译「指标编号在册」枚举规则
///
/// 编号不在官方 27 项之内 → 报枚举越界。这一条同时决定了**条件必填能不能生效**：
/// 编号不在册就判不出指标类别（见 [`conditional_required_applies`]）。
pub fn compile_indicator_code_enum_rule(version: &str, basis: &str) -> Rule {
    compile_indicator_code_enum_rule_with(
        version,
        basis,
        &indicators::all_indicator_list(),
        &indicators::compact_indicator_list(),
    )
}

/// 编译「指标编号在册」枚举规则（在册清单由调用方给）
///
/// 清单来源有两个：内置兜底登记表（[`crate::rules::indicators`]）与官方口径种子
/// （[`crate::rules::seed`]）。后者是官方附件的提取物 —— 种子装载时以它为准。
pub fn compile_indicator_code_enum_rule_with(
    version: &str,
    basis: &str,
    indicator_codes: &str,
    indicator_code_values: &str,
) -> Rule {
    let sql = format!(
        "SELECT row_uid AS row_uid,\n\
        \x20      '指标编号「' || trim(indicator_code, {blank}) || '」不在官方 27 项指标编号清单内；期望取值：'\n\
        \x20      || :indicator_code_values\n\
        \x20  FROM rows\n\
        \x20 WHERE batch_ref = :batch_ref\n\
        \x20   AND indicator_code IS NOT NULL\n\
        \x20   AND trim(indicator_code, {blank}) <> ''\n\
        \x20   AND instr(',' || :indicator_codes || ',', ',' || trim(indicator_code, {blank}) || ',') = 0",
        blank = BLANK_CHARS_SQL,
    );
    let mut params = BTreeMap::new();
    params.insert(
        "indicator_codes".to_string(),
        RuleParam::Text(indicator_codes.to_string()),
    );
    params.insert(
        "indicator_code_values".to_string(),
        RuleParam::Text(indicator_code_values.to_string()),
    );
    Rule {
        rule_id: "R-ENUM-004".to_string(),
        indicator_code: None,
        check_column: Some("indicator_code".to_string()),
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

/// 科室代码（目录型枚举）的核对结果
///
/// 目录客户端在 Task 29 落地（`GET {HIS_BASE_URL}/api/v1/dept-directory`）。
/// 本模块**只消费**核对结果：查得到 → 不报；查不到 → 报「科室主数据不一致」
/// （错误码 `DEPT_NOT_FOUND`）；查不了 → 跳过并标「科室主数据未校验」，**不静默通过**。
///
/// 返回整个 [`DeptCheck`] 而不是只给问题列表 —— 只给问题列表的话，
/// 「目录拉不到所以一条都没报」与「核对了、全部通过」长得一模一样，
/// 调用方会把前者当成后者，**静默通过**就是这么来的。`checked` 字段就是那个区别。
pub fn dept_master_check(rows: &[SourceRow], directory: &dyn DeptDirectory) -> DeptCheck {
    crate::bind::dept::check(rows, directory)
}

/// 条件必填是否对该行生效（按指标类别判定）
///
/// 依据官方 27 项登记表（[`crate::rules::indicators`]）：
/// 比率型 → 生效；人均负担型 → 不生效；编号不在册（类别未知）→ 不生效，
/// 先由「指标编号在册」检查报枚举越界。
pub fn conditional_required_applies(indicator_code: &str) -> bool {
    indicators::is_ratio(indicator_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 五类枚举维度齐备且各归其位() {
        assert_eq!(ENUM_DIMENSIONS.len(), 5);
        let value_enums = ENUM_DIMENSIONS
            .iter()
            .filter(|d| d.kind == EnumDimensionKind::ValueEnum)
            .count();
        let dir_enums = ENUM_DIMENSIONS
            .iter()
            .filter(|d| d.kind == EnumDimensionKind::DirectoryEnum)
            .count();
        assert_eq!(value_enums, 4);
        assert_eq!(dir_enums, 1);
        // 每个维度的期望取值清单都不为空
        for d in ENUM_DIMENSIONS.iter() {
            assert!(!d.expected.is_empty(), "{} 缺期望取值清单", d.name);
            assert!(!d.basis.is_empty(), "{} 缺依据", d.name);
        }
    }

    #[test]
    fn 期望取值清单可按规则编号取到() {
        assert!(expected_values("R-ENUM-001").unwrap().contains("2026-H1"));
        assert!(expected_values("R-ENUM-002").unwrap().contains("实时报"));
        assert!(expected_values("R-ENUM-003").unwrap().contains("例次"));
        assert!(expected_values("R-ENUM-004").unwrap().contains("27 项"));
        assert!(expected_values("R-DEPT-001")
            .unwrap()
            .contains("HIS 科室目录"));
        assert!(expected_values("R-NOPE-999").is_none());
    }

    #[test]
    fn 四条枚举规则_口径_参数_取值不拼进_sql() {
        let period = compile_reporting_period_format_rule("2021.1", PERIOD_FORMAT_BASIS);
        assert_eq!(period.rule_kind, RuleKind::EnumBound);
        assert_eq!(period.severity, Severity::Blocking);
        assert!(period.sql.contains("GLOB"));
        assert!(!period.sql.contains("2026-H1"), "取值必须走参数");

        let ptype = compile_reporting_period_type_rule("2021.1", PERIOD_TYPE_BASIS);
        assert_eq!(ptype.rule_kind, RuleKind::EnumBound);
        assert!(ptype.sql.contains(":period_type_csv"));
        assert!(!ptype.sql.contains("实时报"));
        assert_eq!(
            ptype.params.get("period_type_csv").cloned(),
            Some(RuleParam::Text("年报,半年报,季报,月报,实时报".to_string()))
        );

        let unit = compile_unit_enum_rule("2021.1", UNIT_ENUM_BASIS);
        assert!(unit.sql.contains(":unit_csv"));
        assert!(!unit.sql.contains("例次"));
        assert_eq!(
            unit.params.get("unit_csv").cloned(),
            Some(RuleParam::Text("例,例次,人次,人,%".to_string()))
        );

        let code = compile_indicator_code_enum_rule("2021.1", INDICATOR_CODE_BASIS);
        assert!(code.sql.contains(":indicator_codes"));
        assert!(!code.sql.contains("MER-TL-01"));
        let codes = code
            .params
            .get("indicator_codes")
            .and_then(|p| match p {
                RuleParam::Text(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(codes.split(',').count(), 27);
    }

    #[test]
    fn 科室目录型枚举的依据与期望取值_与绑定层同一处() {
        // 规则清单里写的依据与问题行里写的依据必须是同一条 ——
        // 两处各写一份必然漂移，科室按规则清单查、按问题行改，对不上。
        assert_eq!(
            ENUM_DIMENSIONS
                .iter()
                .find(|d| d.code == "R-DEPT-001")
                .map(|d| d.basis),
            Some(crate::bind::dept::DEPT_BASIS)
        );
        assert_eq!(
            ENUM_DIMENSIONS
                .iter()
                .find(|d| d.code == "R-DEPT-001")
                .map(|d| d.expected),
            Some(crate::bind::dept::DEPT_EXPECTED_VALUES)
        );
    }

    #[test]
    fn 条件必填按指标类别判定_比率型生效_人均负担型与不在册不生效() {
        assert!(conditional_required_applies("MER-TL-04"));
        assert!(conditional_required_applies("MER-D&T-01"));
        assert!(conditional_required_applies("MER-TQ-09"));
        // 人均负担型不适用比率型口径
        assert!(!conditional_required_applies("MER-HR-01"));
        assert!(!conditional_required_applies("MER-HR-02"));
        assert!(!conditional_required_applies("MER-HR-03"));
        // 编号不在册 → 类别未知 → 不套条件必填（先报「指标编号不在册」）
        assert!(!conditional_required_applies("MER-XX-99"));
    }
}
