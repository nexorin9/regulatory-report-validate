//! 官方 27 项指标口径种子。
//!
//! # 这层解决什么问题
//!
//! 口径规则引擎的四个检查器（必填 / 枚举 / 单位 / 公式）都要知道**官方 27 项指标**
//! 是什么：哪些编号在册、哪些是比率型（因而必须有 分子 / 分母 / 填报值 / 计量单位）、
//! 每项的期望计量单位是什么、复算按几位小数比。
//!
//! 以前这些名单是代码里的常量（[`crate::rules::indicators`]）。本层把同一份内容
//! **逐项落到数据文件**（`rules/*.seed.json`），来源是官方附件
//! 《病案管理质量控制指标（2021年版）》（国卫办医函〔2021〕28号 附件），
//! 种子再**编译成规则** —— 口径的取值（名单、单位、精度）是数据，检查器只提供 SQL。
//!
//! # 三条硬纪律
//!
//! 1. **依据必填**：每条种子都要指到官方条款（`指标一`…`指标二十七`）。缺依据的种子
//!    **不猜** —— 标「口径待补」，不进入编译参数（否则就等于替它编了一条口径）。
//! 2. **单位不推断**：官方附件没有逐项写计量单位。比率型指标的公式带 `×100%`，单位为 `%`；
//!    人均负担型（`MER-HR-01…03`）公式不含百分号、官方也没写单位 —— 种子记 `unit: null`，
//!    标「口径待补」，**本轮不判**这三项的单位是否匹配。猜出来的单位会让科室按错口径填数，
//!    比「没写」更糟。
//! 3. **精度不推断**：官方附件给了公式但没写填报值保留几位小数。种子记 `precision: null`，
//!    按默认小数点后 1 位比对（容差为末位的一半），并在清单里报出「按默认精度比对」的条数。
//!
//! # 种子与内置登记表的关系
//!
//! [`crate::rules::indicators`] 是内置兜底登记表（`REGVAL_RULE_DIR` 未配置时用），
//! 种子是同一份口径的**文件形态**。两者由 `tests/seed_test.rs` 逐字段比对：
//! 编号、名称、类别、期望单位必须逐字一致 —— 种子是官方附件的提取物，
//! 内置表是同一份内容的兜底，两边漂移会被测试当场抓住。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::{EngineError, ErrorCode};

use super::checks;
use super::indicators;
use super::model::Rule;

/// 口径种子文件的后缀。规则目录里只有 `*.seed.json` 才是口径种子。
pub const SEED_FILE_SUFFIX: &str = ".seed.json";

/// 指标类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedCategory {
    /// 比率型：分子 ÷ 分母 × 100%，计量单位 %
    Ratio,
    /// 人均负担型：病历数 ÷（实际工作总月数 × 同期人员数），非比例
    Burden,
}

impl SeedCategory {
    /// 类别中文名（写进规则清单与问题说明）
    pub fn as_str(self) -> &'static str {
        match self {
            SeedCategory::Ratio => "比率型",
            SeedCategory::Burden => "人均负担型",
        }
    }

    /// 种子文件里的机器可读代号
    pub fn as_token(self) -> &'static str {
        match self {
            SeedCategory::Ratio => "ratio",
            SeedCategory::Burden => "burden",
        }
    }

    /// 由代号还原；未知代号返回 `None`（调用方据此拒绝装载，不猜）
    pub fn from_token(token: &str) -> Option<SeedCategory> {
        match token.trim() {
            "ratio" => Some(SeedCategory::Ratio),
            "burden" => Some(SeedCategory::Burden),
            _ => None,
        }
    }
}

/// 一项官方指标的种子
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorSeed {
    /// 指标编号（MER-xx-nn）
    pub indicator_code: String,
    /// 指标中文名（官方附件原文）
    pub name: String,
    /// 指标类别
    pub category: SeedCategory,
    /// 官方条款号（指标一…指标二十七）
    pub official_clause: String,
    /// 计量单位；`None` = 官方附件未写明（口径待补，不推断）
    pub unit: Option<String>,
    /// 计算公式的分子口径（官方附件原文）
    pub numerator: String,
    /// 计算公式的分母口径（官方附件原文）
    pub denominator: String,
    /// 计算公式（官方附件原文整理成一行）
    pub formula: String,
    /// 填报值精度（小数位数）；`None` = 官方附件未写明（按默认精度比对，标口径待补）
    pub precision: Option<u32>,
    /// 官方依据出处（必填；空串视为缺失）
    pub basis: String,
}

impl IndicatorSeed {
    /// 是否带官方依据
    pub fn has_basis(&self) -> bool {
        !self.basis.trim().is_empty()
    }

    /// 口径待补的原因清单
    ///
    /// 只有**影响判定**的缺失才算待补：
    /// - 缺依据 → 没有出处就没法向科室解释，不猜；
    /// - 单位未写明 → 不判这一项的单位是否匹配。
    ///
    /// 精度未写明不列在这里 —— 27 项统一按默认精度比对，缺失是**整份种子**的性质，
    /// 由种子清单的 `precision_defaulted` 计数报出，不逐项刷屏。
    pub fn pending_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if !self.has_basis() {
            reasons.push("缺官方依据出处（口径待补，不推断）".to_string());
        }
        if self.unit.is_none() {
            reasons.push("计量单位官方附件未写明（口径待补，不推断）".to_string());
        }
        reasons
    }

    /// 这一项是否处于口径待补状态
    pub fn is_pending(&self) -> bool {
        !self.pending_reasons().is_empty()
    }
}

/// 一份口径种子（27 项指标的官方登记）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSet {
    /// 种子集版本标签（与规则集版本一致时口径才是一套）
    pub seed_set: String,
    /// 官方文件出处
    pub source_document: String,
    /// 官方文件（PDF）文件名
    pub source_document_file: String,
    /// 精度口径说明（官方未逐项写明时的处理方式）
    pub precision_note: String,
    /// 逐项种子（按官方指标序号排列）
    pub indicators: Vec<IndicatorSeed>,
    /// 种子文件名（装载来源）
    pub source_file: String,
}

/// 一项口径待补的种子（写进规则清单，让运维一眼看出哪几项口径还没补全）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingSeed {
    /// 指标编号
    pub indicator_code: String,
    /// 指标中文名
    pub name: String,
    /// 待补原因（逐条）
    pub reasons: Vec<String>,
}

/// 口径种子的清单段（挂在规则清单里）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SeedManifest {
    /// 种子集版本标签
    pub seed_set: String,
    /// 官方文件出处
    pub source_document: String,
    /// 官方文件（PDF）文件名
    pub source_document_file: String,
    /// 种子文件名
    pub source_file: String,
    /// 种子条数
    pub total: usize,
    /// 比率型指标数
    pub ratio_count: usize,
    /// 人均负担型指标数
    pub burden_count: usize,
    /// 口径待补的指标（依据缺失 / 单位未写明）
    pub pending: Vec<PendingSeed>,
    /// 依据缺失、不进入编译参数的种子数
    pub basis_missing: usize,
    /// 填报值精度官方未写明、按默认精度比对的种子数
    pub precision_defaulted: usize,
    /// 由这份种子编译出的规则数
    pub compiled_rules: usize,
    /// 被种子替换掉的手写规则编号（口径以官方种子为准）
    pub replaced_rules: Vec<String>,
    /// 规则表里没有、由种子新增的规则编号
    pub added_rules: Vec<String>,
    /// 种子版本低于手写规则、因而保留手写规则的编号
    pub kept_newer_rules: Vec<String>,
}

// ---------------------------------------------------------------- 种子文件格式

/// 种子文件（一批指标种子）
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedFile {
    /// 种子集版本标签
    #[serde(default)]
    seed_set: Option<String>,
    /// 官方文件出处
    #[serde(default)]
    source_document: Option<String>,
    /// 官方文件（PDF）文件名
    #[serde(default)]
    source_document_file: Option<String>,
    /// 精度口径说明
    #[serde(default)]
    precision_note: Option<String>,
    /// 逐项种子
    indicators: Vec<SeedIndicatorDef>,
}

/// 一项种子在文件里的原始形态
///
/// 未知字段直接报错（`deny_unknown_fields`）—— 种子是官方文件的提取物，
/// 字段名写错若被静默忽略，等于「看着登记了、其实没进口径」。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeedIndicatorDef {
    /// 指标编号
    indicator_code: String,
    /// 指标中文名
    name: String,
    /// 指标类别：ratio / burden
    category: String,
    /// 官方条款号
    #[serde(default)]
    official_clause: Option<String>,
    /// 计量单位（`null` = 官方未写明，口径待补）
    #[serde(default)]
    unit: Option<String>,
    /// 计算公式的分子口径
    #[serde(default)]
    numerator: Option<String>,
    /// 计算公式的分母口径
    #[serde(default)]
    denominator: Option<String>,
    /// 计算公式
    #[serde(default)]
    formula: Option<String>,
    /// 填报值精度（`null` = 官方未写明，按默认精度比对）
    #[serde(default)]
    precision: Option<u32>,
    /// 官方依据出处（必填；空串视为缺失）
    #[serde(default)]
    basis: Option<String>,
}

impl SeedSet {
    /// 装载一份种子文件
    pub fn load(path: &Path) -> Result<SeedSet, EngineError> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("（未命名种子文件）")
            .to_string();

        let raw = std::fs::read_to_string(path).map_err(|e| {
            EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("口径种子文件读取失败：{}", e),
                file_name.clone(),
            )
        })?;

        let file: SeedFile = serde_json::from_str(&raw).map_err(|e| {
            EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("口径种子文件解析失败：{}", e),
                file_name.clone(),
            )
        })?;

        let mut indicators_out: Vec<IndicatorSeed> = Vec::with_capacity(file.indicators.len());
        for def in &file.indicators {
            let code = def.indicator_code.trim().to_string();
            if code.is_empty() {
                return Err(EngineError::at(
                    ErrorCode::RuleLoadFailed,
                    "口径种子的指标编号为空",
                    file_name.clone(),
                ));
            }
            let category = SeedCategory::from_token(&def.category).ok_or_else(|| {
                EngineError::at(
                    ErrorCode::RuleLoadFailed,
                    format!(
                        "指标 {} 的类别代号不认识：「{}」（可用：ratio / burden）",
                        code, def.category
                    ),
                    format!("{} · {}", file_name, code),
                )
            })?;
            let name = def.name.trim().to_string();
            if name.is_empty() {
                return Err(EngineError::at(
                    ErrorCode::RuleLoadFailed,
                    format!("指标 {} 的中文名为空", code),
                    format!("{} · {}", file_name, code),
                ));
            }

            indicators_out.push(IndicatorSeed {
                indicator_code: code,
                name,
                category,
                official_clause: def
                    .official_clause
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
                unit: def
                    .unit
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                numerator: def
                    .numerator
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
                denominator: def
                    .denominator
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
                formula: def
                    .formula
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
                precision: def.precision,
                basis: def
                    .basis
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
            });
        }

        let seed_set = file
            .seed_set
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if seed_set.is_empty() {
            // 版本标签必填：种子编译出的规则要带版本号，缺了就没法与规则表比版本、
            // 也没法让历史批次按原版本复算。不猜。
            return Err(EngineError::at(
                ErrorCode::RuleLoadFailed,
                "口径种子文件缺版本标签（seed_set）",
                file_name.clone(),
            ));
        }

        Ok(SeedSet {
            seed_set,
            source_document: file
                .source_document
                .as_deref()
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            source_document_file: file
                .source_document_file
                .as_deref()
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            precision_note: file
                .precision_note
                .as_deref()
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            indicators: indicators_out,
            source_file: file_name,
        })
    }

    /// 查一项种子（按编号）
    pub fn lookup(&self, code: &str) -> Option<&IndicatorSeed> {
        let code = code.trim();
        self.indicators.iter().find(|i| i.indicator_code == code)
    }

    /// 进入编译口径的种子：**依据齐备**的才算
    ///
    /// 缺依据的种子不猜、不进参数 —— 它既不该出现在「指标编号在册」清单里，
    /// 也不该出现在期望单位表与精度表里。
    fn usable(&self) -> impl Iterator<Item = &IndicatorSeed> {
        self.indicators.iter().filter(|i| i.has_basis())
    }

    /// 比率型指标编号清单（逗号分隔，绑定参数用）
    pub fn ratio_codes(&self) -> String {
        self.usable()
            .filter(|i| i.category == SeedCategory::Ratio)
            .map(|i| i.indicator_code.clone())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// 全部在册指标编号清单（逗号分隔，绑定参数用）
    pub fn all_codes(&self) -> String {
        self.usable()
            .map(|i| i.indicator_code.clone())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// 在册指标编号的紧凑写法（写进问题说明，给科室看）
    ///
    /// 27 个编号逐个列出来太长，按官方附件的五段分类写成区间形式：
    /// 「MER-HR-01…MER-HR-03、MER-TL-01…MER-TL-04、…（共 27 项）」。
    pub fn compact_codes_display(&self) -> String {
        let usable: Vec<&IndicatorSeed> = self.usable().collect();
        let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
        for seed in &usable {
            let prefix = indicator_prefix(&seed.indicator_code);
            match groups.last_mut() {
                Some((last, items)) if *last == prefix => items.push(&seed.indicator_code),
                _ => groups.push((prefix, vec![&seed.indicator_code])),
            }
        }
        let parts: Vec<String> = groups
            .iter()
            .map(|(_prefix, items)| {
                let first = items[0];
                let last = items[items.len() - 1];
                if first == last {
                    first.to_string()
                } else {
                    format!("{}…{}", first, last)
                }
            })
            .collect();
        format!("{}（共 {} 项）", parts.join("、"), usable.len())
    }

    /// 期望计量单位表 → JSON（规则参数 `expected_units`）
    ///
    /// 形态：`[{"code": "MER-TL-01", "name": "入院记录24小时内完成率", "unit": "%"}, ...]`。
    /// 官方未写明单位的三项**不在表里** —— 它们标口径待补，本轮不判单位是否匹配。
    pub fn expected_units_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .usable()
            .filter(|i| i.unit.is_some())
            .map(|i| {
                serde_json::json!({
                    "code": i.indicator_code,
                    "name": i.name,
                    "unit": i.unit.as_deref().unwrap_or_default(),
                })
            })
            .collect();
        serde_json::to_string(&items).expect("期望单位表序列化不应失败")
    }

    /// 逐指标精度表 → JSON（规则参数 `precision_table`）
    ///
    /// 形态：`[{"code": "MER-TL-01", "name": "…", "precision": 1, "scale": 10, "tolerance": 0.05}, ...]`。
    ///
    /// 只收**比率型**指标 —— 人均负担型的官方公式是「病历数 ÷（实际工作总月数 × 同期人员数）」，
    /// 与本产品的最小数据格式对不上（缺两个字段），本轮不套用比率型口径（标口径待补）。
    pub fn precision_table_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .usable()
            .filter(|i| i.category == SeedCategory::Ratio)
            .map(|i| {
                let precision = i.precision.unwrap_or(indicators::DEFAULT_PRECISION);
                serde_json::json!({
                    "code": i.indicator_code,
                    "name": i.name,
                    "precision": precision,
                    "scale": indicators::scale_of(precision),
                    "tolerance": indicators::tolerance_of(precision),
                })
            })
            .collect();
        serde_json::to_string(&items).expect("精度表序列化不应失败")
    }

    /// 口径待补清单 + 各项计数
    pub fn manifest(&self, compiled_rules: usize) -> SeedManifest {
        let mut pending = Vec::new();
        for seed in &self.indicators {
            let reasons = seed.pending_reasons();
            if !reasons.is_empty() {
                pending.push(PendingSeed {
                    indicator_code: seed.indicator_code.clone(),
                    name: seed.name.clone(),
                    reasons,
                });
            }
        }
        SeedManifest {
            seed_set: self.seed_set.clone(),
            source_document: self.source_document.clone(),
            source_document_file: self.source_document_file.clone(),
            source_file: self.source_file.clone(),
            total: self.indicators.len(),
            ratio_count: self
                .indicators
                .iter()
                .filter(|i| i.category == SeedCategory::Ratio)
                .count(),
            burden_count: self
                .indicators
                .iter()
                .filter(|i| i.category == SeedCategory::Burden)
                .count(),
            pending,
            basis_missing: self.indicators.iter().filter(|i| !i.has_basis()).count(),
            precision_defaulted: self
                .indicators
                .iter()
                .filter(|i| i.precision.is_none())
                .count(),
            compiled_rules,
            replaced_rules: Vec::new(),
            added_rules: Vec::new(),
            kept_newer_rules: Vec::new(),
        }
    }

    /// 把种子**编译成规则**：按指标类别映射到四类检查器的规则定义
    ///
    /// | 种子里的什么 | 编译成哪条规则 | 规则参数 |
    /// |---|---|---|
    /// | 类别 = ratio 的指标 | 条件必填（比率型必须有 分子 / 分母 / 填报值 / 计量单位） | `ratio_codes` |
    /// | 全部在册指标 | 枚举越界（指标编号必须在册） | `indicator_codes` / `indicator_code_values` |
    /// | 写明了单位的指标 | 单位不符（期望单位必须匹配） | `expected_units` |
    /// | 比率型指标的精度 | 公式不符（分子 ÷ 分母 × 100% 复算） | `precision_table` / `ratio_codes` |
    ///
    /// 编译出来的规则与内置兜底口径表**逐字一致**（同样的 SQL、参数、依据、版本）——
    /// 种子是同一份口径的文件形态，不是另一套口径。`tests/seed_test.rs` 对此有逐字段断言。
    pub fn compile_rules(&self, version: &str) -> Vec<Rule> {
        let mut rules = vec![
            checks::required::compile_conditional_required_rule_with_ratio_codes(
                version,
                checks::required::CONDITIONAL_BASIS,
                &self.ratio_codes(),
            ),
            checks::enum_::compile_indicator_code_enum_rule_with(
                version,
                checks::enum_::INDICATOR_CODE_BASIS,
                &self.all_codes(),
                &self.compact_codes_display(),
            ),
            checks::unit::compile_unit_match_rule_with_expected_units(
                version,
                checks::unit::UNIT_MATCH_BASIS,
                &self.expected_units_json(),
            ),
            checks::formula::compile_formula_recalc_rule_with(
                version,
                checks::formula::FORMULA_BASIS,
                &self.precision_table_json(),
                &self.ratio_codes(),
            ),
        ];
        for rule in &mut rules {
            rule.source_file = self.source_file.clone();
        }
        rules
    }
}

/// 指标编号的类别前缀（`MER-D&T-01` → `MER-D&T`）
///
/// 取编号的前两段：官方附件的五个分类各占一个前缀（HR / TL / ME / D&T / TQ）。
fn indicator_prefix(code: &str) -> String {
    code.split('-').take(2).collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_seed() -> SeedSet {
        SeedSet {
            seed_set: "2021.1".to_string(),
            source_document: "国卫办医函〔2021〕28号 附件".to_string(),
            source_document_file: "demo.pdf".to_string(),
            precision_note: "官方未写明精度".to_string(),
            source_file: "demo.seed.json".to_string(),
            indicators: vec![
                IndicatorSeed {
                    indicator_code: "MER-HR-01".to_string(),
                    name: "住院病案管理人员月均负担出院患者病历数".to_string(),
                    category: SeedCategory::Burden,
                    official_clause: "指标一".to_string(),
                    unit: None,
                    numerator: "出院患者病历总数".to_string(),
                    denominator: "实际工作总月数 × 同期住院病案管理人员".to_string(),
                    formula: "…".to_string(),
                    precision: None,
                    basis: "依据指标一".to_string(),
                },
                IndicatorSeed {
                    indicator_code: "MER-TL-01".to_string(),
                    name: "入院记录24小时内完成率".to_string(),
                    category: SeedCategory::Ratio,
                    official_clause: "指标四".to_string(),
                    unit: Some("%".to_string()),
                    numerator: "…".to_string(),
                    denominator: "…".to_string(),
                    formula: "…".to_string(),
                    precision: None,
                    basis: "依据指标四".to_string(),
                },
                IndicatorSeed {
                    indicator_code: "MER-TQ-01".to_string(),
                    name: "出院患者病历2日归档率".to_string(),
                    category: SeedCategory::Ratio,
                    official_clause: "指标十九".to_string(),
                    unit: Some("%".to_string()),
                    numerator: "…".to_string(),
                    denominator: "…".to_string(),
                    formula: "…".to_string(),
                    precision: None,
                    basis: "依据指标十九".to_string(),
                },
            ],
        }
    }

    #[test]
    fn 单位缺失的种子标口径待补() {
        let seed = demo_seed();
        let hr = seed.lookup("MER-HR-01").unwrap();
        assert!(hr.is_pending());
        assert!(hr.pending_reasons().iter().any(|r| r.contains("计量单位")));
        let tl = seed.lookup("MER-TL-01").unwrap();
        assert!(!tl.is_pending());
    }

    #[test]
    fn 缺依据的种子不猜单位也不进编译参数() {
        let mut seed = demo_seed();
        seed.indicators[1].basis = String::new();
        let tl = seed.lookup("MER-TL-01").unwrap();
        assert!(!tl.has_basis());
        assert!(tl.is_pending());
        // 缺依据的指标不进在册编号清单，也不进期望单位表
        assert_eq!(seed.all_codes(), "MER-HR-01,MER-TQ-01");
        assert!(!seed.expected_units_json().contains("MER-TL-01"));
        assert_eq!(seed.manifest(0).basis_missing, 1);
    }

    #[test]
    fn 紧凑编号写法按前缀分组() {
        let seed = demo_seed();
        let display = seed.compact_codes_display();
        assert!(display.contains("MER-HR-01"));
        assert!(display.contains("MER-TL-01"));
        assert!(display.contains("MER-TQ-01"));
        assert!(display.contains("共 3 项"));
    }

    #[test]
    fn 比率型清单只收比率型指标() {
        let seed = demo_seed();
        assert_eq!(seed.ratio_codes(), "MER-TL-01,MER-TQ-01");
    }
}
