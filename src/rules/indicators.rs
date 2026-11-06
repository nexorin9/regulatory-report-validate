//! 官方 27 项指标口径登记表（单一事实来源）。
//!
//! # 这张表是什么
//!
//! 国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》共 27 项指标，
//! 本模块把它们逐项登记成结构化数据：**编号 / 中文名称 / 指标类别 / 计量单位 / 依据出处**。
//!
//! 口径规则引擎里所有「这个指标属于哪一类」「这个编号在不在册」的判断都查这张表，
//! 不在各个检查器里各写一份名单 —— 名单散落 inevitably 会漂移：
//! 口径文件改版时只改这张表，其余模块跟着走。
//!
//! # 指标类别
//!
//! | 类别 | 指标 | 计算公式 | 计量单位 |
//! |---|---|---|---|
//! | [`IndicatorKind::Ratio`] 比率型 | 指标四至指标二十七（24 项） | 分子 ÷ 分母 × 100% | % |
//! | [`IndicatorKind::Burden`] 人均负担型 | 指标一至指标三（3 项） | 病历数 ÷（实际工作总月数 × 同期人员数） | 官方文件未给，**不推断** |
//!
//! 类别决定两件事：
//!
//! 1. **条件必填是否生效**：比率型指标必须有 分子 / 分母 / 填报值 / 计量单位；
//!    人均负担型指标不适用（它也要填这两个数，但口径含义不同，见 Task 13 的公式复算）。
//! 2. **计量单位的期望值**：比率型固定为 `%`（Task 12 落地逐指标单位校验）。
//!
//! # 不推断的原则
//!
//! 官方附件只给了指标定义与计算公式，**没有逐项写明计量单位**。比率型指标的公式里带
//! `×100%`，因此单位为 `%`；人均负担型的公式不含百分号，官方文件也没写单位 ——
//! 本表记 `unit: None`（**口径待补**），不替它猜一个「份」或「例」。
//! 猜出来的单位会让科室按错口径填数，比「没写」更糟。
//!
//! # 与其它模块的关系
//!
//! - [`crate::rules::checks::required`] 的比率型指标清单（`RATIO_INDICATORS`）从本表取，
//!   避免「24 项」在两处各写一遍；
//! - [`crate::rules::checks::enum_`] 的「指标编号是否在册」检查查本表；
//! - Task 17 的 27 项口径种子以本表为底稿落成规则目录里的 seed 文件。

/// 指标类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorKind {
    /// 比率型：分子 ÷ 分母 × 100%，计量单位 %
    Ratio,
    /// 人均负担型：病历数 ÷（实际工作总月数 × 同期人员数），非比例
    Burden,
}

impl IndicatorKind {
    /// 类别中文名（写进问题说明与规则清单）
    pub fn as_str(self) -> &'static str {
        match self {
            IndicatorKind::Ratio => "比率型",
            IndicatorKind::Burden => "人均负担型",
        }
    }
}

/// 一项官方指标的登记信息
#[derive(Debug, Clone, Copy)]
pub struct IndicatorSpec {
    /// 指标编号（MER-xx-nn）
    pub code: &'static str,
    /// 指标中文名（官方附件原文）
    pub name: &'static str,
    /// 指标类别
    pub kind: IndicatorKind,
    /// 计量单位；`None` = 官方文件未写明（口径待补，不推断）
    pub unit: Option<&'static str>,
}

/// 27 项指标的官方依据出处
pub const BASIS: &str = "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》（指标一至指标二十七，共 27 项；编号 / 名称 / 定义 / 计算公式以官方附件为准）";

/// 27 项指标登记表
///
/// 顺序按官方附件的指标序号：一至三（人力资源配置）、四至七（病历书写时效性）、
/// 八至十（重大检查记录符合率）、十一至十八（诊疗行为记录符合率）、十九至二十七（病历归档质量）。
pub const INDICATORS: [IndicatorSpec; 27] = [
    // 一、人力资源配置指标（人均负担型；官方文件未写明计量单位 → 口径待补，不推断）
    IndicatorSpec {
        code: "MER-HR-01",
        name: "住院病案管理人员月均负担出院患者病历数",
        kind: IndicatorKind::Burden,
        unit: None,
    },
    IndicatorSpec {
        code: "MER-HR-02",
        name: "门诊病案管理人员月均负担门诊患者病历数",
        kind: IndicatorKind::Burden,
        unit: None,
    },
    IndicatorSpec {
        code: "MER-HR-03",
        name: "病案编码人员月均负担出院患者病历数",
        kind: IndicatorKind::Burden,
        unit: None,
    },
    // 二、病历书写时效性指标
    IndicatorSpec {
        code: "MER-TL-01",
        name: "入院记录24小时内完成率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TL-02",
        name: "手术记录24小时内完成率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TL-03",
        name: "出院记录24小时内完成率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TL-04",
        name: "病案首页24小时内完成率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    // 三、重大检查记录符合率
    IndicatorSpec {
        code: "MER-ME-01",
        name: "CT/MRI检查记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-ME-02",
        name: "病理检查记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-ME-03",
        name: "细菌培养检查记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    // 四、诊疗行为记录符合率
    IndicatorSpec {
        code: "MER-D&T-01",
        name: "抗菌药物使用记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-02",
        name: "恶性肿瘤化学治疗记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-03",
        name: "恶性肿瘤放射治疗记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-04",
        name: "手术相关记录完整率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-05",
        name: "植入物相关记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-06",
        name: "临床用血相关记录符合率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-07",
        name: "医师查房记录完整率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-D&T-08",
        name: "患者抢救记录及时完成率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    // 五、病历归档质量指标
    IndicatorSpec {
        code: "MER-TQ-01",
        name: "出院患者病历2日归档率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-02",
        name: "出院患者病历归档完整率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-03",
        name: "主要诊断填写正确率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-04",
        name: "主要诊断编码正确率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-05",
        name: "主要手术填写正确率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-06",
        name: "主要手术编码正确率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-07",
        name: "不合理复制病历发生率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-08",
        name: "知情同意书规范签署率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
    IndicatorSpec {
        code: "MER-TQ-09",
        name: "甲级病历率",
        kind: IndicatorKind::Ratio,
        unit: Some("%"),
    },
];

/// 比率型指标编号清单（24 项：指标四至指标二十七）。
///
/// 依据：国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》的指标分类 ——
/// 指标四至指标二十七（病历书写时效性 4 项 / 重大检查记录符合率 3 项 /
/// 诊疗行为记录符合率 8 项 / 病历归档质量 9 项）的计算公式均为「分子 ÷ 分母 × 100%」，
/// 计量单位为 %；指标一至指标三（MER-HR-01…03）为人力资源配置指标（人均负担），
/// 不是比例型，不适用条件必填。
///
/// 这份清单是 27 项口径种子的**一部分**：Task 17 的种子装载会在此基础上补齐
/// 单位、计算公式、精度与「口径待补」标记。本任务只需要「哪些指标是比率型」这一条。
pub const RATIO_INDICATORS: [&str; 24] = [
    // 二、病历书写时效性指标
    "MER-TL-01",
    "MER-TL-02",
    "MER-TL-03",
    "MER-TL-04",
    // 三、重大检查记录符合率
    "MER-ME-01",
    "MER-ME-02",
    "MER-ME-03",
    // 四、诊疗行为记录符合率
    "MER-D&T-01",
    "MER-D&T-02",
    "MER-D&T-03",
    "MER-D&T-04",
    "MER-D&T-05",
    "MER-D&T-06",
    "MER-D&T-07",
    "MER-D&T-08",
    // 五、病历归档质量指标
    "MER-TQ-01",
    "MER-TQ-02",
    "MER-TQ-03",
    "MER-TQ-04",
    "MER-TQ-05",
    "MER-TQ-06",
    "MER-TQ-07",
    "MER-TQ-08",
    "MER-TQ-09",
];

/// 查一项指标（按编号）。编号不在册返回 `None` —— **不猜、不按前缀近似匹配**。
pub fn lookup(code: &str) -> Option<&'static IndicatorSpec> {
    let code = code.trim();
    INDICATORS.iter().find(|i| i.code == code)
}

/// 指标编号是否在官方 27 项之内
pub fn is_known(code: &str) -> bool {
    lookup(code).is_some()
}

/// 是否比率型指标（决定条件必填是否对该行生效）
pub fn is_ratio(code: &str) -> bool {
    match lookup(code) {
        Some(spec) => spec.kind == IndicatorKind::Ratio,
        // 编号不在册 → 类别未知 → 不套用比率型口径（先报「指标编号不在册」）
        None => false,
    }
}

/// 比率型指标编号清单（绑定参数用，逗号分隔）
pub fn ratio_indicator_list() -> String {
    RATIO_INDICATORS.join(",")
}

/// 全部 27 项指标编号（绑定参数用，逗号分隔）
pub fn all_indicator_list() -> String {
    INDICATORS
        .iter()
        .map(|i| i.code)
        .collect::<Vec<_>>()
        .join(",")
}

/// 在册指标的紧凑编号清单（写进问题说明，给科室看）
///
/// 27 个编号逐个列出来太长，按官方附件的五段分类写成区间形式：
/// 「MER-HR-01…MER-HR-03、MER-TL-01…MER-TL-04、…（共 27 项）」。
pub fn compact_indicator_list() -> String {
    "MER-HR-01…MER-HR-03、MER-TL-01…MER-TL-04、MER-ME-01…MER-ME-03、MER-D&T-01…MER-D&T-08、MER-TQ-01…MER-TQ-09（共 27 项）".to_string()
}

/// 官方写明了计量单位的指标 → (编号, 中文名, 期望单位)
///
/// 官方附件没有逐项写单位 —— 比率型指标的公式带 `×100%`，因此单位为 %；
/// 人均负担型（MER-HR-01…03）公式不含百分号、官方也没写单位，**不推断**。
/// 这张表就是单位检查器的期望值来源（规则参数 `expected_units`）。
pub fn expected_unit_entries() -> Vec<(&'static str, &'static str, &'static str)> {
    INDICATORS
        .iter()
        .filter_map(|i| i.unit.map(|u| (i.code, i.name, u)))
        .collect()
}

/// 期望单位表 → JSON 文本（规则参数 `expected_units` 的形态）
///
/// 形态：`[{"code": "MER-TL-01", "name": "入院记录24小时内完成率", "unit": "%"}, ...]`。
/// 口径是**数据**（走绑定参数），改期望单位不用改规则 SQL。
pub fn expected_units_json() -> String {
    let items: Vec<serde_json::Value> = expected_unit_entries()
        .into_iter()
        .map(|(code, name, unit)| serde_json::json!({ "code": code, "name": name, "unit": unit }))
        .collect();
    serde_json::to_string(&items).expect("期望单位表序列化不应失败")
}

/// 官方附件未写明计量单位的指标（口径待补）
///
/// 这几项本轮**不判**单位是否匹配 —— 猜出来的单位会让科室按错口径填数，
/// 比「没写」更糟。清单随口径种子一起补全（后续任务）。
pub fn pending_unit_codes() -> Vec<&'static str> {
    INDICATORS
        .iter()
        .filter(|i| i.unit.is_none())
        .map(|i| i.code)
        .collect()
}

/// 复算精度（小数位数）—— 公式复算的比对容差由它决定
///
/// 官方附件给了计算公式（「分子 ÷ 分母 × 100%」），但**没有**逐项写明填报值保留
/// 几位小数。本轮统一取默认值（小数点后 1 位），并在口径种子落地时按官方文件逐项补。
pub const DEFAULT_PRECISION: u32 = 1;

/// 逐指标精度覆盖表（编号 → 小数位数）
///
/// 本轮官方文件未逐项写明填报值精度，表是空的 —— **不推断**：官方没写就留默认精度，
/// 猜一个「两位」会让科室按错口径改数。口径种子（Task 17）落地时按官方文件逐项填。
pub const PRECISION_OVERRIDES: &[(&str, u32)] = &[];

/// 某项指标的复算精度（小数位数）
pub fn precision_of(code: &str) -> u32 {
    PRECISION_OVERRIDES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, p)| *p)
        .unwrap_or(DEFAULT_PRECISION)
}

/// 复算容差：精度末位的一半（0.5 × 10^-精度）
///
/// 科室按指标给定的精度四舍五入填写，差在半个末位之内是四舍五入的正常结果，
/// 不是算错。容差由精度算出来，不另设一个全局常数。
pub fn tolerance_of(precision: u32) -> f64 {
    0.5 * 10f64.powi(-(precision as i32))
}

/// 复算标度：10^精度（把数值放大到整数位比较，避免浮点边界抖动）
pub fn scale_of(precision: u32) -> i64 {
    10i64.saturating_pow(precision)
}

/// 逐指标精度表 → JSON（规则参数 `precision_table`）
///
/// 形态：`[{"code": "MER-TL-01", "name": "…", "precision": 1, "scale": 10, "tolerance": 0.05}, ...]`。
///
/// 只收**比率型**指标 —— 人均负担型的官方公式是「病历数 ÷（实际工作总月数 × 同期人员数）」，
/// 与本产品的最小数据格式对不上（缺两个字段），本轮不套用比率型口径（标「口径待补」）。
/// 精度是**数据**（走绑定参数），改精度不用改规则 SQL。
pub fn precision_table_json() -> String {
    let items: Vec<serde_json::Value> = INDICATORS
        .iter()
        .filter(|i| i.kind == IndicatorKind::Ratio)
        .map(|i| {
            let precision = precision_of(i.code);
            serde_json::json!({
                "code": i.code,
                "name": i.name,
                "precision": precision,
                "scale": scale_of(precision),
                "tolerance": tolerance_of(precision),
            })
        })
        .collect();
    serde_json::to_string(&items).expect("精度表序列化不应失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 登记表是二十七项且编号不重复() {
        assert_eq!(INDICATORS.len(), 27);
        let mut codes: Vec<&str> = INDICATORS.iter().map(|i| i.code).collect();
        codes.sort();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before, "指标编号重复了");
    }

    #[test]
    fn 编号前缀覆盖官方附件的五类指标() {
        let prefixes: Vec<String> = INDICATORS
            .iter()
            .map(|i| i.code.split('-').take(2).collect::<Vec<_>>().join("-"))
            .collect();
        for want in ["MER-HR", "MER-TL", "MER-ME", "MER-D&T", "MER-TQ"] {
            assert!(
                prefixes.iter().any(|p| p == want),
                "官方附件里应有 {} 开头的指标",
                want
            );
        }
    }

    #[test]
    fn 比率型清单与登记表一致() {
        assert_eq!(RATIO_INDICATORS.len(), 24);
        for code in RATIO_INDICATORS.iter() {
            let spec = lookup(code).unwrap_or_else(|| panic!("{} 不在登记表里", code));
            assert_eq!(spec.kind, IndicatorKind::Ratio, "{} 类别对不上", code);
            assert_eq!(spec.unit, Some("%"), "{} 单位应为 %", code);
        }
        // 登记表中标为比率型的项，数量与清单一致（不多不少）
        let in_registry: Vec<&str> = INDICATORS
            .iter()
            .filter(|i| i.kind == IndicatorKind::Ratio)
            .map(|i| i.code)
            .collect();
        assert_eq!(in_registry.len(), RATIO_INDICATORS.len());
        for code in in_registry {
            assert!(RATIO_INDICATORS.contains(&code), "{} 不在清单里", code);
        }
    }

    #[test]
    fn 人均负担型三项不推断计量单位() {
        for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
            let spec = lookup(code).expect("人力资源指标应登记");
            assert_eq!(spec.kind, IndicatorKind::Burden);
            assert_eq!(spec.unit, None, "官方文件未写明单位，不推断");
            assert!(!is_ratio(code));
        }
    }

    #[test]
    fn 不在册的编号查不到且不算比率型() {
        assert!(lookup("MER-XX-99").is_none());
        assert!(!is_known("MER-XX-99"));
        assert!(!is_ratio("MER-XX-99"));
        assert!(!is_ratio(""));
    }

    #[test]
    fn 参数清单按逗号分隔便于绑定() {
        let list = all_indicator_list();
        assert_eq!(list.split(',').count(), 27);
        assert!(list.contains("MER-D&T-01"));
        assert_eq!(ratio_indicator_list().split(',').count(), 24);
    }
}
