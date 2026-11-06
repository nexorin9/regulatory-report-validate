//! 口径归一化层：空白字符集与计量单位别名表。
//!
//! # 这一层是什么
//!
//! 表里的同一个值常常有多种写法。全角空格与半角空格、全角 `％` 与半角 `%`、
//! 「例数」与「例」、「人次数」与「人次」—— **指的是同一件事，写法却不同**。
//! 直接按字面比大小，同一个值在两个环节会得出两个结论：这一轮说「不合格」，
//! 下一轮换了写法又说「合格」，科室根本不知道该改哪儿。
//!
//! 因此判定之前先**归一**：去首尾空白 → 全角百分号转半角 → ASCII 转小写，
//! 再按别名表映射到规范单位。**判定一律用归一后的值。**
//!
//! | 原值 | 归一后 | 规范单位 |
//! |---|---|---|
//! | `%` | `%` | `%` |
//! | `％`（全角 U+FF05） | `%` | `%` |
//! | ` ％ `（前后有空格） | `%` | `%` |
//! | `例数` | `例数` | `例` |
//! | `例次数` | `例次数` | `例次` |
//!
//! # 别名表是口径数据，不是某个检查器的私有逻辑
//!
//! 这张表放在口径层，理由有三：
//!
//! 1. **单位检查器**（[`crate::rules::checks::unit`]）判「单位写没写错 / 配不配」时用它；
//! 2. **重复检测的归一化候选**（`src/dupe/normalize.rs`）判「两行是不是同一件事」时也用它 ——
//!    指标名相同、只有单位写法不同，是最典型的「疑似重复但字面不同」；
//! 3. **交付的规则表文件**里带着同一张表（规则参数 `unit_aliases`），
//!    运维改口径不用改代码 —— 与「口径规则是数据不是代码」同一条原则。
//!
//! # 归一化在两处必须一致
//!
//! 规则 SQL 里也要做同样的归一（[`normalized_unit_sql`] 生成表达式），
//! Rust 侧归一（[`normalize_unit`]）给重复检测等非 SQL 环节用。
//! **两边规则不一致 = 同一行在两个环节结论不同**，所以归一步骤写死在本模块，
//! SQL 表达式由它生成，两处同源。
//!
//! # 空白字符集为什么单独拎出来
//!
//! SQLite 自带的 `trim(X)` 只去 ASCII 空格 —— 全角空格（U+3000）、不间断空格（U+00A0）
//! 与 Tab 都要单独列进去。表里「看着空、其实有字符」的写法太常见（复制粘贴带进来的空格、
//! 全角输入法的空格），漏掉一个就等于放过一条没填的行。这个字符集原来写在
//! 必填检查器里，本层是它更合适的位置 —— **一处定义，各层共用**。

use std::collections::BTreeMap;

/// 视为空白的字符集（SQL 表达式）
///
/// SQLite 自带的 `trim(X)` 只去 ASCII 空格 —— 全角空格（U+3000）、不间断空格（U+00A0）
/// 与 Tab 都要单独列进去。表里「看着空、其实有字符」的写法太常见（复制粘贴带进来的空格、
/// 全角输入法的空格），漏掉一个就等于放过一条没填的行。
pub const BLANK_CHARS_SQL: &str =
    "' ' || char(9) || char(10) || char(11) || char(12) || char(13) || char(160) || char(12288)";

/// Rust 侧视为空白的字符集（与 [`BLANK_CHARS_SQL`] 一一对应）
pub const BLANK_CHARS: [char; 8] = [
    ' ', '\t', '\n', '\u{0B}', '\u{0C}', '\r', '\u{00A0}', '\u{3000}',
];

/// 全角百分号与小型百分号（归一化时转成半角 `%`）
///
/// 全角输入法打出来的 `％`（U+FF05）在表里极常见 —— 肉眼看不出与 `%` 的差别，
/// 但按字面比大小就是两个值。
pub const FULLWIDTH_PERCENTS: [char; 2] = ['\u{FF05}', '\u{FE6A}'];

/// 规范单位清单（五种）
///
/// 这五种是**计量单位列的合法取值**：例 / 例次 / 人次 / 人 / %。
/// 别名表里的每一条都归到这五种之一，别名不得指向清单外的单位。
pub const CANONICAL_UNITS: [&str; 5] = ["例", "例次", "人次", "人", "%"];

/// 一条别名：同一个单位的若干种写法
///
/// `canonical` 是规范单位，`aliases` 是同义写法。规范单位本身也是它自己的一个别名
/// （见 [`AliasTable::builtin`]）—— 别名表因此是一份**完整的归一化字典**，
/// 而不是一张「例外清单」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitAliasEntry {
    /// 规范单位（必在 [`CANONICAL_UNITS`] 之内）
    pub canonical: &'static str,
    /// 同一单位的其它写法
    pub aliases: &'static [&'static str],
}

/// 计量单位别名表（口径数据）
///
/// | 规范单位 | 同义写法 |
/// |---|---|
/// | 例 | 例数 |
/// | 例次 | 例次数 |
/// | 人次 | 人次数、就诊人次 |
/// | 人 | 人数 |
/// | % | 百分比 |
///
/// **不收的写法**：「个百分点」与「%」不是一回事（百分点是差值，% 是比值），
/// 归到一起会把两个不同口径混成一样 —— 宁可让科室改表。
pub const UNIT_ALIAS_TABLE: [UnitAliasEntry; 5] = [
    UnitAliasEntry {
        canonical: "例",
        aliases: &["例数"],
    },
    UnitAliasEntry {
        canonical: "例次",
        aliases: &["例次数"],
    },
    UnitAliasEntry {
        canonical: "人次",
        aliases: &["人次数", "就诊人次"],
    },
    UnitAliasEntry {
        canonical: "人",
        aliases: &["人数"],
    },
    UnitAliasEntry {
        canonical: "%",
        aliases: &["百分比"],
    },
];

/// 单位别名表的依据
pub const UNIT_ALIAS_BASIS: &str = "本产品最小数据格式与字段要求「计量单位」列（取值 ∈ 例 / 例次 / 人次 / 人 / %）；同一单位的同义写法（全角百分号、字数写法等）按单位别名表归一后判定，别名表随规则表下发（规则参数 unit_aliases）";

/// 归一化：去首尾空白 → 全角百分号转半角 → ASCII 转小写
///
/// 与 [`normalized_unit_sql`] 生成的 SQL 表达式**逐步对应** —— 两处不一致 = 同一行
/// 在规则环节与归一化环节结论不同。
pub fn normalize_unit(raw: &str) -> String {
    let trimmed = raw.trim_matches(|c: char| BLANK_CHARS.contains(&c));
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let ch = if FULLWIDTH_PERCENTS.contains(&ch) {
            '%'
        } else {
            ch
        };
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// 归一化后的 SQL 表达式（与 [`normalize_unit`] 同规则）
///
/// 生成 `lower(replace(replace(trim(<列>, <空白集>), '％', '%'), '﹪', '%'))`：
/// trim 去首尾空白 → replace 把全角百分号转半角 → lower 转 ASCII 小写。
pub fn normalized_unit_sql(column: &str) -> String {
    format!(
        "lower(replace(replace(trim({column}, {blank}), '％', '%'), '﹪', '%'))",
        column = column,
        blank = BLANK_CHARS_SQL
    )
}

// ============================================================================
// 文本归一（重复检测漏斗第二级用）
// ============================================================================
//
// # 为什么这一段和单位归一分开
//
// [`normalize_unit`] 是**判定用**的归一：单位检查器要按它判「单位写没写错」，
// 所以它必须与规则 SQL 表达式 [`normalized_unit_sql`] 逐步对应 —— 两边不一致，
// 同一行在规则环节与判定环节就会得出两个结论。
//
// 下面的 `normalize_display` / `normalize_key` 是**分组用**的归一：重复检测第二级
// 要把「写法不同但说的是同一件事」的行找出来做候选对。这件事只影响「要不要
// 提示经办人去看一眼」，不影响「这一行合不合格」，因此允许比判定口径更宽
// （多去空白、多折全角、折大小写）。
//
// 两条归一各司其职，不互相顶替。

/// 展示用归一：去首尾空白 → 全角转半角 → ASCII 转小写 → 内部空白折叠为单个空格。
///
/// - **去首尾空白**：含 ASCII 空格、Tab、换行、不间断空格（U+00A0）、全角空格（U+3000）——
///   复制粘贴带进来的空格与全角输入法打的空格，肉眼看不出来，按字面比却是两个值；
/// - **全角转半角**：`ＣＴ／ＭＲＩ` → `CT/MRI`、`％` → `%`、`，` → `,`；
/// - **ASCII 转小写**：`CT/MRI` 与 `ct/mri` 是同一件事（中文不受影响）；
/// - **内部空白折叠**：`病案  室` → `病案 室`（多个空白折成一个，不删掉 —— 删掉会把
///   `CT 检查` 与 `CT检查` 混成一样，那不是同一个写法）。
///
/// 归一过程按 **char**（不是字节）处理：中文与生僻字不会切错，也不会 panic。
pub fn normalize_display(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        if BLANK_CHARS.contains(&ch) {
            pending_space = true;
            continue;
        }
        if pending_space {
            if !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
        }
        out.push(fold_fullwidth(ch).to_ascii_lowercase());
    }
    out
}

/// 比较用归一：在 [`normalize_display`] 的基础上再去掉全部空白字符。
///
/// 判等用它：中文指标名里的空格几乎不承载含义（`病案 室` 与 `病案室` 是同一件事），
/// 留着一个空格就会把两行判成不一样。
pub fn normalize_key(raw: &str) -> String {
    normalize_display(raw)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// 全角 ASCII 转半角（`！`～`～` → `!`～`~`）
fn fold_fullwidth(ch: char) -> char {
    let code = ch as u32;
    if (0xFF01..=0xFF5E).contains(&code) {
        char::from_u32(code - 0xFEE0).unwrap_or(ch)
    } else {
        ch
    }
}

/// 指标名称别名表的一条：同一个指标的另一种写法。
///
/// `canonical` 是规范写法（**必须是官方指标名称或科室通用说法**），
/// `alias` 是同一个指标的常见写法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameAliasEntry {
    /// 规范写法
    pub canonical: &'static str,
    /// 同义写法
    pub alias: &'static str,
}

/// 指标名称别名表（口径数据）。
///
/// | 规范写法 | 常见写法 | 为什么是同一个指标 |
/// |---|---|---|
/// | 医院感染 | 院感、感控、医院感控 | 科室口语，指同一件事 |
/// | 细菌培养检查记录符合率 | 院感细菌培养检查记录符合率 | 前面加科室限定词，口径不变 |
/// | 出院患者病历2日归档率 | 出院病历2日归档率、患者病历2日归档率、出院患者病历2天内归档率 | 省略「患者」或口语加「内」，口径不变 |
/// | 主要诊断填写正确率 | 主要诊断正确率 | 省略「填写」二字，口径不变 |
/// | CT/MRI检查记录符合率 | CT、MRI检查记录符合率、CT和MRI检查记录符合率 | 顿号与「和」是同一份检查项 |
/// | 入院记录24小时内完成率 | 入院记录24H内完成率 | `24H` 与 `24小时` 同一时限 |
///
/// **只收「同一个指标的另一种写法」**：不收「医院感染管理」这类更大的概念 ——
/// 概念大小不同就不是同一个指标，归到一起会把两条不同的记录混成一条。
///
/// 表里的键按 [`normalize_key`] 归一后查表（键与判定时的归一结果对不上，
/// 等于这条别名不生效）。
pub const INDICATOR_NAME_ALIAS_TABLE: [NameAliasEntry; 13] = [
    NameAliasEntry {
        canonical: "医院感染",
        alias: "院感",
    },
    NameAliasEntry {
        canonical: "医院感染",
        alias: "感控",
    },
    NameAliasEntry {
        canonical: "医院感染",
        alias: "医院感控",
    },
    NameAliasEntry {
        canonical: "细菌培养检查记录符合率",
        alias: "院感细菌培养检查记录符合率",
    },
    NameAliasEntry {
        canonical: "出院患者病历2日归档率",
        alias: "出院病历2日归档率",
    },
    NameAliasEntry {
        canonical: "出院患者病历2日归档率",
        alias: "患者病历2日归档率",
    },
    NameAliasEntry {
        canonical: "出院患者病历2日归档率",
        alias: "出院患者病历2天内归档率",
    },
    NameAliasEntry {
        canonical: "主要诊断填写正确率",
        alias: "主要诊断正确率",
    },
    NameAliasEntry {
        canonical: "CT/MRI检查记录符合率",
        alias: "CT、MRI检查记录符合率",
    },
    NameAliasEntry {
        canonical: "CT/MRI检查记录符合率",
        alias: "CT和MRI检查记录符合率",
    },
    NameAliasEntry {
        canonical: "入院记录24小时内完成率",
        alias: "入院记录24H内完成率",
    },
    NameAliasEntry {
        canonical: "手术记录24小时内完成率",
        alias: "手术记录24H内完成率",
    },
    NameAliasEntry {
        canonical: "出院记录24小时内完成率",
        alias: "出院记录24H内完成率",
    },
];

/// 指标名称别名表的依据
pub const INDICATOR_NAME_ALIAS_BASIS: &str = "本产品最小数据格式与字段要求「指标名称」列（登记写法 = 官方指标名称或科室通用说法）；同一个指标的常见写法（科室口语「院感」、省略「填写」「患者」、顿号与「和」、「24H」与「24小时」）按指标名称别名表归一后比较，别名表只收同一个指标的写法，不收大小不同的概念";

/// 指标名称别名表（运行期形态）：归一后的写法 → 规范写法。
///
/// 与单位别名表（[`AliasTable`]）同层同构：**口径数据，不是某个模块的私有逻辑**。
/// 重复检测第二级用它，别的环节要判「两个名称说的是不是一件事」也用它。
#[derive(Debug, Clone, Default)]
pub struct NameAliasTable {
    /// 归一后的写法 → 规范写法（完整字典：含规范写法自己）
    map: BTreeMap<String, String>,
    /// 归一后的别名 → 规范写法（只含别名条目，不含规范写法自己）
    ///
    /// 单独存一份是为了序列化：规范写法自己是「自己等于自己」，
    /// 写进别名清单里会让装载时那条「自己等于自己没有意义」的校验误伤。
    aliases: BTreeMap<String, String>,
}

impl NameAliasTable {
    /// 内置别名表
    pub fn builtin() -> Self {
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for entry in INDICATOR_NAME_ALIAS_TABLE.iter() {
            // 规范写法本身也是它自己的写法 —— 表因此是完整的归一化字典
            map.insert(normalize_key(entry.canonical), entry.canonical.to_string());
            aliases.insert(normalize_key(entry.alias), entry.canonical.to_string());
        }
        for (alias, canonical) in &aliases {
            map.insert(alias.clone(), canonical.clone());
        }
        NameAliasTable { map, aliases }
    }

    /// 从 JSON 文本装载别名表（形态：`[{"alias": "...", "canonical": "..."}, ...]`）
    ///
    /// 装载时逐条校验：
    ///
    /// - 缺字段 → 报错并点名缺哪个字段；
    /// - 同一个写法指向两个规范写法 → 那是歧义，不二选一；
    /// - 写法与规范写法归一后相同 → 那是「自己等于自己」，这条别名没有意义，报错。
    pub fn from_json(text: &str) -> Result<Self, String> {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(text).map_err(|e| format!("指标名称别名表解析失败：{}", e))?;
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for item in raw.iter() {
            let alias = item
                .get("alias")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "指标名称别名表缺 alias 字段".to_string())?;
            let canonical = item
                .get("canonical")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "指标名称别名表缺 canonical 字段".to_string())?;
            let key = normalize_key(alias);
            let canonical_key = normalize_key(canonical);
            if key == canonical_key {
                return Err(format!(
                    "指标名称写法「{}」与规范写法归一后相同，这条别名没有意义",
                    alias
                ));
            }
            if let Some(prev) = aliases.get(&key) {
                if prev != canonical {
                    return Err(format!(
                        "指标名称写法「{}」同时指向「{}」与「{}」，口径有歧义",
                        alias, prev, canonical
                    ));
                }
            }
            aliases.insert(key, canonical.to_string());
            // 规范写法本身也是它自己的写法 —— 字典因此是完整的
            map.insert(canonical_key, canonical.to_string());
        }
        for (alias, canonical) in &aliases {
            map.insert(alias.clone(), canonical.clone());
        }
        Ok(NameAliasTable { map, aliases })
    }

    /// 归一到规范写法（展示用）
    ///
    /// 命中不了 → 返回归一后的写法本身（那是未知写法，由它自己报错，不由这里猜）。
    pub fn canonicalize(&self, raw: &str) -> String {
        let key = normalize_key(raw);
        match self.map.get(&key) {
            Some(canonical) => canonical.clone(),
            None => normalize_display(raw),
        }
    }

    /// 归一到规范写法的**比较键**（判等用）
    pub fn canonical_key(&self, raw: &str) -> String {
        normalize_key(&self.canonicalize(raw))
    }

    /// 别名条数（不含规范写法自己）
    pub fn len(&self) -> usize {
        self.aliases.len()
    }

    /// 空表
    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    /// 序列化为 JSON 文本（规则参数形态；只出别名条目）
    pub fn to_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .aliases
            .iter()
            .map(|(alias, canonical)| serde_json::json!({ "alias": alias, "canonical": canonical }))
            .collect();
        serde_json::to_string(&items).expect("别名表序列化不应失败")
    }
}

/// 别名表（运行期形态）：归一后的写法 → 规范单位
///
/// 之所以是运行期结构而不只是常量：别名表要能随规则表下发（运维改口径不用改代码），
/// 所以它同时能从 JSON 文本装载。
#[derive(Debug, Clone, Default)]
pub struct AliasTable {
    /// 归一后的写法 → 规范单位
    map: BTreeMap<String, String>,
}

impl AliasTable {
    /// 内置别名表（兜底口径表用；与规则表里的 `unit_aliases` 参数逐条一致）
    pub fn builtin() -> Self {
        let mut map = BTreeMap::new();
        for entry in UNIT_ALIAS_TABLE.iter() {
            // 规范单位本身也是它自己的写法 —— 表因此是完整的归一化字典
            map.insert(entry.canonical.to_string(), entry.canonical.to_string());
            for alias in entry.aliases.iter() {
                map.insert(alias.to_string(), entry.canonical.to_string());
            }
        }
        AliasTable { map }
    }

    /// 从 JSON 文本装载别名表（规则参数 `unit_aliases` 的形态）
    ///
    /// 文本形态：`[{"alias": "例数", "canonical": "例"}, ...]`。
    ///
    /// 装载时逐条校验：
    ///
    /// - 规范单位必须在 [`CANONICAL_UNITS`] 之内 —— 别名指向清单外的单位是口径写错了；
    /// - 同一个写法不能指向两个规范单位 —— 那是歧义，不二选一；
    /// - 写法的键先归一 —— 别名表里的键与判定时的归一结果对不上，等于这条别名不生效。
    pub fn from_json(text: &str) -> Result<Self, String> {
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(text).map_err(|e| format!("单位别名表解析失败：{}", e))?;
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for item in raw.iter() {
            let alias = item
                .get("alias")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "单位别名表缺 alias 字段".to_string())?;
            let canonical = item
                .get("canonical")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "单位别名表缺 canonical 字段".to_string())?;
            if !CANONICAL_UNITS.contains(&canonical) {
                return Err(format!(
                    "单位别名「{}」指向的规范单位「{}」不在规范单位清单内（{:?}）",
                    alias, canonical, CANONICAL_UNITS
                ));
            }
            let key = normalize_unit(alias);
            if let Some(prev) = map.get(&key) {
                if prev != canonical {
                    return Err(format!(
                        "单位写法「{}」同时指向「{}」与「{}」，口径有歧义",
                        alias, prev, canonical
                    ));
                }
            }
            map.insert(key, canonical.to_string());
        }
        Ok(AliasTable { map })
    }

    /// 归一到规范单位
    ///
    /// 归一后能命中别名表 → 返回规范单位；命中不了 → 返回归一后的值本身
    /// （那是一个未知写法，由单位枚举检查报「不在枚举内」，不由这里猜）。
    pub fn canonicalize(&self, raw: &str) -> String {
        let norm = normalize_unit(raw);
        self.map.get(&norm).cloned().unwrap_or(norm)
    }

    /// 归一后是否是五种规范单位之一
    pub fn is_known_unit(&self, raw: &str) -> bool {
        CANONICAL_UNITS.contains(&self.canonicalize(raw).as_str())
    }

    /// 条数（规范写法 + 别名写法）
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 空表
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 序列化为 JSON 文本（规则参数 `unit_aliases` 的形态）
    ///
    /// 输出按写法排序 —— 同一张表序列化两次结果逐字相同（规则表指纹才稳定）。
    pub fn to_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .map
            .iter()
            .map(|(alias, canonical)| serde_json::json!({ "alias": alias, "canonical": canonical }))
            .collect();
        serde_json::to_string(&items).expect("别名表序列化不应失败")
    }
}

/// 内置别名表 → JSON 文本（编译规则参数用）
pub fn alias_table_json() -> String {
    AliasTable::builtin().to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 归一化去掉首尾空白并把全角百分号转半角() {
        assert_eq!(normalize_unit("%"), "%");
        assert_eq!(normalize_unit("％"), "%");
        assert_eq!(normalize_unit("﹪"), "%");
        assert_eq!(normalize_unit("  ％  "), "%");
        assert_eq!(normalize_unit("\u{3000}%\u{3000}"), "%");
        assert_eq!(normalize_unit("\u{00A0}%\u{00A0}"), "%");
        assert_eq!(normalize_unit("\t%\r\n"), "%");
        // 中文写法不做大小写折叠，只去空白
        assert_eq!(normalize_unit(" 例数 "), "例数");
        assert_eq!(normalize_unit(""), "");
    }

    #[test]
    fn 空白字符集与_sql_表达式一一对应() {
        // SQL 表达式里的 char() 序列必须覆盖 Rust 侧的空白字符集
        for code in [9u32, 10, 11, 12, 13, 160, 12288] {
            assert!(
                BLANK_CHARS_SQL.contains(&format!("char({})", code)),
                "SQL 空白字符集漏了 char({})",
                code
            );
        }
        assert!(BLANK_CHARS_SQL.contains("' '"));
        assert_eq!(BLANK_CHARS.len(), 8);
    }

    #[test]
    fn 别名表里的别名都归到五种规范单位() {
        let table = AliasTable::builtin();
        for entry in UNIT_ALIAS_TABLE.iter() {
            assert!(
                CANONICAL_UNITS.contains(&entry.canonical),
                "{} 不是规范单位",
                entry.canonical
            );
            for alias in entry.aliases.iter() {
                assert_eq!(
                    table.canonicalize(alias),
                    entry.canonical,
                    "别名 {} 应归到 {}",
                    alias,
                    entry.canonical
                );
            }
        }
        // 规范单位本身归到自己
        for unit in CANONICAL_UNITS.iter() {
            assert_eq!(table.canonicalize(unit), *unit);
        }
    }

    #[test]
    fn 同义写法判定一致() {
        let table = AliasTable::builtin();
        // 全角百分号与半角百分号是同一个单位
        assert_eq!(table.canonicalize("％"), table.canonicalize("%"));
        // 例数与例是同一个单位
        assert_eq!(table.canonicalize("例数"), table.canonicalize("例"));
        // 人次数与人次是同一个单位
        assert_eq!(table.canonicalize("人次数"), table.canonicalize("人次"));
        // 不同单位不能归到一起
        assert_ne!(table.canonicalize("例次"), table.canonicalize("例"));
        assert_ne!(table.canonicalize("人次"), table.canonicalize("人"));
    }

    #[test]
    fn 未知写法原样返回_不猜() {
        let table = AliasTable::builtin();
        assert_eq!(table.canonicalize("台"), "台");
        assert_eq!(table.canonicalize("万元"), "万元");
        assert!(!table.is_known_unit("台"));
        assert!(table.is_known_unit("％"));
        assert!(table.is_known_unit("例数"));
    }

    #[test]
    fn 别名表_json_可往返装载且逐条一致() {
        let json = alias_table_json();
        let table = AliasTable::from_json(&json).unwrap();
        assert_eq!(table.len(), AliasTable::builtin().len());
        assert_eq!(table.to_json(), json, "装载回来应与原表逐字一致");
        // 序列化两次结果相同 —— 规则表指纹才稳定
        assert_eq!(alias_table_json(), json);
    }

    #[test]
    fn 别名表装载时拒绝口径写错() {
        // 指向清单外的规范单位
        let bad = r#"[{"alias":"份","canonical":"份"}]"#;
        assert!(AliasTable::from_json(bad).is_err());
        // 同一个写法指向两个规范单位 —— 歧义，不二选一
        let ambiguous = r#"[{"alias":"例","canonical":"例"},{"alias":"例","canonical":"人次"}]"#;
        assert!(AliasTable::from_json(ambiguous).is_err());
        // 缺字段
        assert!(AliasTable::from_json(r#"[{"alias":"例数"}]"#).is_err());
        assert!(AliasTable::from_json("不是 JSON").is_err());
    }

    #[test]
    fn sql_表达式与_rust_归一一致() {
        let sql = normalized_unit_sql("unit");
        assert!(sql.contains("trim(unit,"));
        assert!(sql.contains("replace("));
        assert!(sql.contains("lower("));
        // 全角百分号确实进了表达式
        assert!(sql.contains('％'));
    }

    #[test]
    fn 展示归一去空白折全角折大小写() {
        assert_eq!(normalize_display("  病案  室  "), "病案 室");
        assert_eq!(normalize_display("CT/MRI"), "ct/mri");
        assert_eq!(
            normalize_display("ＣＴ／ＭＲＩ检查记录符合率"),
            "ct/mri检查记录符合率"
        );
        assert_eq!(normalize_display("％"), "%");
        assert_eq!(normalize_display("\u{3000}病案室\u{00A0}"), "病案室");
        assert_eq!(normalize_display("病案室"), "病案室", "普通写法不该被改动");
    }

    #[test]
    fn 比较键再去掉全部空白() {
        assert_eq!(normalize_key("病案  室"), "病案室");
        assert_eq!(normalize_key(" CT / MRI "), "ct/mri");
        assert_eq!(normalize_key("\u{3000}病案室"), "病案室");
    }

    #[test]
    fn 生僻字与繁体字按字符归一不切错() {
        // 「龘」「𠮷」是表外字符与生僻字：按 char 处理不会 panic，也不会切错
        let raw = "病案室（𠮷）龘";
        assert_eq!(normalize_key(raw), normalize_key(raw));
        assert!(normalize_display(raw).contains('龘'));
        assert!(normalize_display(raw).contains('𠮷'));
    }

    #[test]
    fn 指标名称别名表把别名归到规范写法() {
        let table = NameAliasTable::builtin();
        assert_eq!(table.canonicalize("院感"), "医院感染");
        assert_eq!(table.canonicalize("感控"), "医院感染");
        assert_eq!(table.canonicalize("院感"), table.canonicalize("医院感染"));
        assert_eq!(table.canonicalize("主要诊断正确率"), "主要诊断填写正确率");
        assert_eq!(
            table.canonicalize("CT、MRI检查记录符合率"),
            "CT/MRI检查记录符合率"
        );
        // 规范写法归到自己
        assert_eq!(table.canonicalize("医院感染"), "医院感染");
        // 未知写法原样返回，不猜
        assert_eq!(table.canonicalize("某个没登记的名称"), "某个没登记的名称");
    }

    #[test]
    fn 别名表键按归一后查_所以带空白的写法也命中() {
        let table = NameAliasTable::builtin();
        assert_eq!(
            table.canonical_key(" 院感 "),
            table.canonical_key("医院感染")
        );
        assert_eq!(
            table.canonical_key("CT／MRI检查记录符合率"),
            table.canonical_key("ct/mri检查记录符合率")
        );
    }

    #[test]
    fn 指标名称别名表装载时拒绝口径写错() {
        assert!(NameAliasTable::from_json(r#"[{"alias":"院感"}]"#).is_err());
        assert!(NameAliasTable::from_json("不是 JSON").is_err());
        // 同一写法指向两个规范写法 —— 歧义，不二选一
        let ambiguous =
            r#"[{"alias":"院感","canonical":"医院感染"},{"alias":"院感","canonical":"感染管理"}]"#;
        assert!(NameAliasTable::from_json(ambiguous).is_err());
        // 自己等于自己 —— 这条别名没有意义
        let meaningless = r#"[{"alias":"院感","canonical":"院感"}]"#;
        assert!(NameAliasTable::from_json(meaningless).is_err());
    }

    #[test]
    fn 指标名称别名表_json_可往返装载且逐条一致() {
        let json = NameAliasTable::builtin().to_json();
        let table = NameAliasTable::from_json(&json).unwrap();
        assert_eq!(table.len(), NameAliasTable::builtin().len());
        assert_eq!(table.to_json(), json);
    }
}
