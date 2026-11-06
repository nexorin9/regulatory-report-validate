//! 版本化口径规则表装载器。
//!
//! # 口径规则是数据，不是代码
//!
//! 规则表放在 `REGVAL_RULE_DIR` 指向的目录里，每个 `*.rules.json` 文件是一张规则表。
//! 装载做四件事：
//!
//! 1. **逐条校验**：每条规则必须带官方依据（`basis`）。**缺依据的规则拒绝装载** ——
//!    没有出处的口径问题没法向科室解释，宁可停下来报错，也不让它混进结果清单。
//! 2. **版本归并**：同一「口径槽位」= (检查类型, 适用指标, 检查项) 内，版本号最高的一条生效，
//!    其余版本标为「已被取代」**保留可追**（不物理删除），历史批次因此能按原版本复算。
//!    「检查项」这一段不能少：枚举越界下有四条不同的检查（报告期格式 / 报告期类型 /
//!    计量单位 / 指标编号），少了它，换一版口径会把另外三条一起顶掉。
//! 3. **出清单**：产出规则清单（规则条数、各状态计数、依据缺失计数），供 `regval rules` 查看。
//! 4. **可复现**：规则表文件按名排序装载、版本比较有确定序 —— 同一目录装载两次结果一致。
//!
//! # 两种装载模式
//!
//! - [`LoadMode::Strict`]（`validate` 走这条）：有任何一条规则被拒绝装载就**直接报错**，
//!   错误码点名是哪条规则、哪个文件。宁可停下来，也不让口径悄悄少一条。
//! - [`LoadMode::Reporting`]（`regval rules` 走这条）：被拒绝的规则剔除出执行集合，
//!   但计入规则清单的「依据缺失」计数，让运维一眼看出规则表哪里要补。
//!
//! 两种模式下**缺依据的规则都不会进入执行集合** —— 这是硬约束，不是模式差异。
//!
//! # 源产品机制迁移
//!
//! 「配置单元带显式状态、版本号落在数据里、打开时按序归并、旧版保留不删」这一套取自
//! 参考实现的**配置版本化**范式：显式状态枚举（序列化后落库取值 —— 状态是数据不是分支）、
//! 打开时读版本文件并按序迁移（旧版本数据保留而不是丢弃）。
//! 移植到本产品：口径规则表的「状态 + 版本 + 保留旧版」三层语义。
//! 口径内容本身来自官方文件，不是源仓库能力（见 round 根 `spec.md` → `## 开源对照`）。
//!
//! # 换版流程（本任务落地的是装载与归并，闸门与差异标注在后续任务）
//!
//! 属地口径文件改版时，规则目录里**新增**一张版本更高的规则表即可：
//! 旧表不删 —— 装载后旧版本规则标为「已被取代」，仍可在规则清单里查到，
//! 历史批次据此按原版本复算。

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{EngineError, ErrorCode};

use super::model::{ExecutionFailureEntry, Rule, RuleKind, RuleParam, RuleStatus, Severity};
use super::seed::{self, SeedManifest};
use super::syntax;
use super::version::{self, BlockedSwitch, VersionGate};

/// 规则表文件的后缀。目录里只有 `*.rules.json` 才是规则表；
/// 口径种子是另一种后缀（[`seed::SEED_FILE_SUFFIX`），单独装载、单独出清单。
pub const RULE_FILE_SUFFIX: &str = ".rules.json";

/// 装载模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// 严格：有任何规则被拒绝装载即报错（`validate` 用这条）
    Strict,
    /// 汇报：被拒绝的规则剔除出执行集合并计入清单（`regval rules` 用这条）
    Reporting,
}

/// 一条被拒绝装载的规则（缺依据、字段写错等）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RejectedRule {
    /// 来自哪个规则表文件
    pub source_file: String,
    /// 规则编号（文件里没写出来时为空串）
    pub rule_id: String,
    /// 机器可读错误码
    pub code: String,
    /// 中文说明（给业务人员看）
    pub message: String,
}

/// 一条规则因 SQL 语法不通过而不生效的记录
///
/// 语法错的规则**不中止整轮**：它自己不生效，其余规则照常执行；
/// 但出错位置必须逐条列出 —— 不静默通过。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyntaxErrorEntry {
    /// 来自哪个规则表文件
    pub source_file: String,
    /// 规则编号
    pub rule_id: String,
    /// 规则中文名（业务可读：检查类型 + 适用指标）
    pub rule_name: String,
    /// 机器可读错误码
    pub code: String,
    /// 出错 token 的类型
    pub token_type: String,
    /// 出错 token 原文
    pub found: String,
    /// 出错 token 在规则 SQL 里的字节偏移
    pub offset: usize,
    /// 给业务人员看的中文说明（含规则 id、规则中文名、出错位置）
    pub message: String,
}

/// 规则清单：装载结果的汇总（规则条数、各状态计数、依据缺失计数……）
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuleManifest {
    /// 规则目录（内置规则集时为「（内置规则集）」）
    pub rule_dir: String,
    /// 生效规则里的最高版本号（规则集版本）
    pub set_version: String,
    /// 生效规则的指纹（规则编号|版本|SQL 摘要|依据 的 sha256 前 16 位）
    pub set_fingerprint: String,
    /// 装载到的规则总数（含已被取代的旧版本）
    pub total: usize,
    /// 生效中、参与执行的规则数
    pub active: usize,
    /// 已被取代但保留可追的规则数
    pub superseded: usize,
    /// 换版待确认生效的规则数（新版本已就位，但没过换版闸门）
    #[serde(default)]
    pub pending: usize,
    /// 被换版闸门拦下的口径（逐条列出：哪一版、为什么没生效、怎么补）
    #[serde(default)]
    pub blocked_switches: Vec<BlockedSwitch>,
    /// 因缺官方依据被拒绝装载的规则数
    pub basis_missing: usize,
    /// 因 SQL 语法不通过而不生效的规则数
    pub syntax_error_count: usize,
    /// 本轮**执行失败**的规则数（参数不全 / SQL 运行失败；其余规则继续）
    ///
    /// 少一条口径就可能把不合格的行判成「通过」，所以这个数必须能被看见。
    #[serde(default)]
    pub execution_failed_count: usize,
    /// 本轮因引用了不存在的列而**不生效**的规则数
    #[serde(default)]
    pub execution_skipped_count: usize,
    /// 本轮没跑起来的规则明细（缺列不生效 / 执行失败）
    #[serde(default)]
    pub execution_failures: Vec<ExecutionFailureEntry>,
    /// 口径槽位数（(检查类型, 适用指标, 检查项) 去重后）
    pub slots: usize,
    /// 各状态计数（生效 / 已被取代 / 不生效）
    pub by_status: BTreeMap<String, usize>,
    /// 参与装载的规则表文件名（按名排序）
    pub files: Vec<String>,
    /// 各规则表声明的口径版本标签（与 `files` 一一对应；未声明时为空串）
    pub rule_sets: Vec<String>,
    /// 被拒绝装载的规则明细
    pub rejected: Vec<RejectedRule>,
    /// 语法不通过而不生效的规则明细（含出错 token 与偏移）
    pub syntax_errors: Vec<SyntaxErrorEntry>,
    /// 参与装载的口径种子文件（按名排序）
    #[serde(default)]
    pub seed_files: Vec<String>,
    /// 口径种子的清单段（逐份种子一段：条数、类别分布、口径待补明细）
    ///
    /// 「口径待补」必须**看得见**：官方附件没写明的单位、没给依据的指标，
    /// 不能悄悄按猜的值判 —— 运维得知道哪几项口径还没补全。
    #[serde(default)]
    pub seed_sets: Vec<SeedManifest>,
}

impl RuleManifest {
    /// 某个状态的规则数（状态名取 `RuleStatus::as_str` 的中文名）
    pub fn status_count(&self, status: &str) -> usize {
        self.by_status.get(status).copied().unwrap_or(0)
    }
}

/// 规则清单里的一条规则（供 `regval rules` 与结果契约共用）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleStatusEntry {
    pub rule_id: String,
    /// 检查类型（中文，写进「问题类型」列的那个名）
    pub rule_kind: String,
    /// 检查项（同一检查类型下的不同检查项，如「报告期格式」）
    #[serde(default)]
    pub check_column: Option<String>,
    pub indicator_code: Option<String>,
    pub version: String,
    /// 状态：生效 / 已被取代 / 不生效
    pub status: String,
    /// 严重度：阻断 / 提示
    pub severity: String,
    /// 官方依据出处
    pub basis: String,
    pub effective_from: Option<String>,
    /// 来自哪个规则表文件
    pub source_file: String,
}

/// 规则清单段：清单汇总 + 逐条规则。
///
/// 结果契约（`contract.rs`）与 `regval rules --out` 共用这一份结构 —— 两处看到的
/// 是同一套口径，不会有两套说法。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RuleStatusSection {
    pub manifest: RuleManifest,
    pub rules: Vec<RuleStatusEntry>,
}

/// 装载好的规则集
#[derive(Debug, Clone)]
pub struct RuleSet {
    /// 全部规则（含已被取代的旧版本 —— 保留可追，不物理删除）
    rules: Vec<Rule>,
    manifest: RuleManifest,
}

impl RuleSet {
    /// 参与执行的规则（状态为「生效」）
    pub fn executable(&self) -> Vec<&Rule> {
        self.rules
            .iter()
            .filter(|r| r.status == RuleStatus::Active)
            .collect()
    }

    /// 全部规则（含已被取代的旧版本）
    pub fn all(&self) -> &[Rule] {
        &self.rules
    }

    /// 取某个版本的规则（历史批次按原版本复算用）
    pub fn by_version(&self, version: &str) -> Vec<&Rule> {
        self.rules.iter().filter(|r| r.version == version).collect()
    }

    /// 规则清单
    pub fn manifest(&self) -> &RuleManifest {
        &self.manifest
    }

    /// 被换版闸门拦下的口径（逐条：哪一版、为什么没生效、怎么补）
    pub fn blocked_switches(&self) -> &[BlockedSwitch] {
        &self.manifest.blocked_switches
    }

    /// 规则清单段（汇总 + 逐条规则）
    pub fn status_section(&self) -> RuleStatusSection {
        RuleStatusSection {
            manifest: self.manifest.clone(),
            rules: self
                .rules
                .iter()
                .map(|r| RuleStatusEntry {
                    rule_id: r.rule_id.clone(),
                    rule_kind: r.rule_kind.as_str().to_string(),
                    check_column: r.check_column.clone(),
                    indicator_code: r.indicator_code.clone(),
                    version: r.version.clone(),
                    status: r.status.as_str().to_string(),
                    severity: r.severity.as_str().to_string(),
                    basis: r.basis.clone(),
                    effective_from: r.effective_from.clone(),
                    source_file: r.source_file.clone(),
                })
                .collect(),
        }
    }
}

/// 内置规则集（`REGVAL_RULE_DIR` 未配置时的兜底口径表）
pub fn builtin_set() -> RuleSet {
    let mut rules = super::model::builtin_rules();
    // 内置口径表同样过一遍语法预校验：万一将来有人改坏了内置 SQL，
    // 也要在清单里显出来，而不是跑到校验阶段才炸
    let syntax_errors = apply_syntax_check(&mut rules);
    let manifest = build_manifest(
        "（内置规则集）".to_string(),
        &rules,
        &[],
        &syntax_errors,
        Vec::new(),
        Vec::new(),
        vec![rules[0].version.clone()],
        Vec::new(),
        Vec::new(),
    );
    RuleSet { rules, manifest }
}

/// 内置规则集（带换版闸门）
///
/// 内置兜底口径表同样过闸门：状态库里已经在用别的口径版本时，
/// 悄悄退回内置旧版就是**静默换口径** —— 与其默默换，不如报错让人决定。
pub fn builtin_set_gated(gate: &VersionGate) -> Result<RuleSet, EngineError> {
    let mut rules = super::model::builtin_rules();
    let blocked = version::apply_gate(&mut rules, gate);
    let syntax_errors = apply_syntax_check(&mut rules);
    let manifest = build_manifest(
        "（内置规则集）".to_string(),
        &rules,
        &[],
        &syntax_errors,
        blocked,
        Vec::new(),
        vec![rules[0].version.clone()],
        Vec::new(),
        Vec::new(),
    );
    Ok(RuleSet { rules, manifest })
}

/// 从规则目录装载（严格模式：有任何规则被拒绝装载即报错）
pub fn load(dir: &Path) -> Result<RuleSet, EngineError> {
    load_with(dir, LoadMode::Strict)
}

/// 从规则目录装载（汇报模式：被拒绝的规则剔除出执行集合并计入清单）
pub fn load_reporting(dir: &Path) -> Result<RuleSet, EngineError> {
    load_with(dir, LoadMode::Reporting)
}

/// 从规则目录装载（带换版闸门）
///
/// 闸门决定同一口径槽位里哪一版生效：生效日期到了、且（不需要确认 / 已确认）才生效；
/// 未确认就继续用旧版，并把被拦下的换版逐条记进规则清单。
pub fn load_gated(dir: &Path, mode: LoadMode, gate: &VersionGate) -> Result<RuleSet, EngineError> {
    load_gated_impl(dir, mode, Some(gate))
}

/// 从规则目录装载（不带换版闸门 —— 装载器原语，看清单用）
pub fn load_with(dir: &Path, mode: LoadMode) -> Result<RuleSet, EngineError> {
    load_gated_impl(dir, mode, None)
}

fn load_gated_impl(
    dir: &Path,
    mode: LoadMode,
    gate: Option<&VersionGate>,
) -> Result<RuleSet, EngineError> {
    // 1. 目录必须存在 —— 配错了路径就报错，不静默退回内置规则集
    if !dir.is_dir() {
        return Err(EngineError::at(
            ErrorCode::RuleLoadFailed,
            format!("规则目录不存在或不是目录：{}", dir.display()),
            dir.display().to_string(),
        ));
    }

    // 2. 收集规则表文件与口径种子文件（按文件名排序 —— 装载结果必须可复现）
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut seed_paths: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| {
        EngineError::at(
            ErrorCode::RuleLoadFailed,
            format!("规则目录读取失败：{}", e),
            dir.display().to_string(),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("规则目录条目读取失败：{}", e),
                dir.display().to_string(),
            )
        })?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_rule_file = name.ends_with(RULE_FILE_SUFFIX);
        let is_seed_file = name.ends_with(seed::SEED_FILE_SUFFIX);
        if path.is_file() && is_rule_file {
            files.push(path);
        } else if path.is_file() && is_seed_file {
            seed_paths.push(path);
        }
    }
    files.sort();
    seed_paths.sort();
    if files.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleLoadFailed,
            format!(
                "规则目录里没有规则表文件（文件名须以 {} 结尾）：{}",
                RULE_FILE_SUFFIX,
                dir.display()
            ),
            dir.display().to_string(),
        ));
    }

    // 3. 逐文件解析、逐条校验
    let mut rules: Vec<Rule> = Vec::new();
    let mut rejected: Vec<RejectedRule> = Vec::new();
    let mut file_names: Vec<String> = Vec::new();
    let mut table_labels: Vec<String> = Vec::new();
    for path in &files {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("（未命名规则表）")
            .to_string();
        file_names.push(file_name.clone());

        let raw = std::fs::read_to_string(path).map_err(|e| {
            EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("规则表文件读取失败：{}", e),
                file_name.clone(),
            )
        })?;
        let table: RuleTableFile = serde_json::from_str(&raw).map_err(|e| {
            EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("规则表文件解析失败：{}", e),
                file_name.clone(),
            )
        })?;

        for def in &table.rules {
            match to_rule(def, &file_name) {
                Ok(rule) => rules.push(rule),
                Err(rej) => rejected.push(rej),
            }
        }
        // 规则表声明的口径版本标签（未声明则留空 —— 不猜）
        table_labels.push(
            table
                .rule_set
                .as_ref()
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        );
    }

    // 4. 严格模式：有规则被拒绝装载就直接报错（点名第一条）
    if mode == LoadMode::Strict && !rejected.is_empty() {
        let rej = &rejected[0];
        let code = ErrorCode::from_str(&rej.code).unwrap_or(ErrorCode::RuleLoadFailed);
        let extra = if rejected.len() > 1 {
            format!("（另有 {} 条规则同样未装载）", rejected.len() - 1)
        } else {
            String::new()
        };
        return Err(EngineError::at(
            code,
            format!("规则 {} 未装载：{}{}", rej.rule_id, rej.message, extra),
            format!("{} · {}", rej.source_file, rej.rule_id),
        ));
    }

    // 5. 装载口径种子：官方 27 项指标的登记，编译成四类检查器的规则并入规则集
    let mut seed_sets: Vec<SeedManifest> = Vec::new();
    let mut seed_files: Vec<String> = Vec::new();
    for path in &seed_paths {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("（未命名种子文件）")
            .to_string();
        seed_files.push(file_name.clone());
        let set = seed::SeedSet::load(path)?;
        let manifest = merge_seed_rules(&mut rules, &set);
        seed_sets.push(manifest);
    }

    // 6. 版本归并：同一槽位内版本号最高的一条生效，其余标「已被取代」保留可追
    resolve_versions(&mut rules)?;

    // 6b. 换版闸门（可选）：没过闸门的新版不生效，继续用旧版并逐条说明
    let blocked = match gate {
        Some(g) => version::apply_gate(&mut rules, g),
        None => Vec::new(),
    };
    // 闸门把整本规则都拦下（既没有可用的新版，旧版又不在目录里）→ 装载器照样把
    // 结果交出去，但**执行方**必须自己判断能不能跑：带着「零条口径」继续校验，
    // 就可能把不合格的行判成「通过」（见 `regval validate` 的执行前检查）。

    // 7. 语法预校验：语法错的规则标「不生效」且不参与执行，其余规则照常执行
    //    （与「缺依据拒绝装载」不同 —— 语法错不中止整轮，但逐条列出出错位置）
    let syntax_errors = apply_syntax_check(&mut rules);

    // 8. 出规则清单
    let manifest = build_manifest(
        dir.display().to_string(),
        &rules,
        &rejected,
        &syntax_errors,
        blocked,
        file_names,
        table_labels,
        seed_files,
        seed_sets,
    );
    Ok(RuleSet { rules, manifest })
}

/// 闸门把整本规则都拦下时的报错
///
/// 说清三件事：哪一版没生效、为什么、怎么补 —— 不让操作的人对着空结果猜。
pub fn blocked_all_error(blocked: &[BlockedSwitch]) -> EngineError {
    let detail: Vec<String> = blocked.iter().take(3).map(|b| b.reason.clone()).collect();
    let more = if blocked.len() > 3 {
        format!("（另有 {} 条同样被拦下）", blocked.len() - 3)
    } else {
        String::new()
    };
    EngineError::at(
        ErrorCode::RuleSwitchUnconfirmed,
        format!(
            "本轮没有一条口径生效：口径换版未通过闸门，且规则目录里没有可用的旧版规则表 —— 本轮一条检查都没跑，少一条口径就可能把不合格的行判成「通过」。{}{}",
            detail.join("；"),
            more
        ),
        "rule_set_version",
    )
}

/// 装载口径种子并把编译出的规则并入规则集
///
/// **口径以官方种子为准**：同一条规则编号（口径槽位）若规则表与种子都有，
/// - 版本相同 → 取种子（官方附件是上游真源，规则表只是它的可编辑副本）；
/// - 种子版本更低 → 保留规则表的新版（属地口径改版优先，种子清单里点名）。
///
/// 两种情况都记进种子清单，不静默 —— 口径从哪来必须说得清。
fn merge_seed_rules(rules: &mut Vec<Rule>, set: &seed::SeedSet) -> SeedManifest {
    let compiled = set.compile_rules(&set.seed_set);
    let mut manifest = set.manifest(compiled.len());
    for seed_rule in compiled {
        match rules.iter_mut().find(|r| r.rule_id == seed_rule.rule_id) {
            Some(existing) => match compare_versions(&seed_rule.version, &existing.version) {
                Ordering::Greater | Ordering::Equal => {
                    manifest.replaced_rules.push(seed_rule.rule_id.clone());
                    *existing = seed_rule;
                }
                Ordering::Less => {
                    manifest.kept_newer_rules.push(existing.rule_id.clone());
                }
            },
            None => {
                manifest.added_rules.push(seed_rule.rule_id.clone());
                rules.push(seed_rule);
            }
        }
    }
    manifest
}

/// 规则表文件（一批规则）
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleTableFile {
    /// 规则集标签（口径版本，如 `2021.1`）；缺省时取规则自身的最高版本
    #[serde(default)]
    rule_set: Option<String>,
    /// 规则清单
    rules: Vec<RuleDef>,
}

/// 一条规则在规则表文件里的原始形态
///
/// 未知字段直接报错（`deny_unknown_fields`）—— 口径文件是业务人员手工维护的，
/// 字段名写错若被静默忽略，等于「看起来配了、其实没生效」。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleDef {
    /// 规则编号
    rule_id: String,
    /// 适用指标编号；缺省或 null 表示适用于全部指标
    #[serde(default)]
    indicator_code: Option<String>,
    /// 检查项：同一检查类型下的不同检查项（枚举越界：报告期格式 / 报告期类型 / 计量单位 / 指标编号）
    #[serde(default)]
    check_column: Option<String>,
    /// 检查类型代号：required / enum / unit / formula / data_shape
    rule_kind: String,
    /// 规则 SQL（整段文本）
    #[serde(default)]
    sql: Option<String>,
    /// 规则 SQL（按行给出，拼回一段文本）—— 业务人员手工维护口径文件时更顺手
    #[serde(default)]
    sql_lines: Option<Vec<String>>,
    /// 命名参数（取值一律走参数，不拼进 SQL）
    #[serde(default)]
    params: BTreeMap<String, serde_json::Value>,
    /// 官方依据出处（必填）
    basis: String,
    /// 规则版本
    version: String,
    /// 严重度代号：blocking / hint
    severity: String,
    /// 生效日期（换版闸门在后续任务落地；本任务只记录，不静默丢弃）
    #[serde(default)]
    effective_from: Option<String>,
}

/// 一条规则 → 可装载的规则；不满足装载条件的给出拒绝原因（带错误码）
fn to_rule(def: &RuleDef, source_file: &str) -> Result<Rule, RejectedRule> {
    let rule_id = def.rule_id.trim().to_string();

    let reject = |code: ErrorCode, message: String| RejectedRule {
        source_file: source_file.to_string(),
        rule_id: rule_id.clone(),
        code: code.as_str().to_string(),
        message,
    };

    if rule_id.is_empty() {
        return Err(reject(
            ErrorCode::RuleLoadFailed,
            "规则编号为空".to_string(),
        ));
    }

    // 依据必填：没有出处的口径问题没法向科室解释，拒绝装载
    let basis = def.basis.trim().to_string();
    if basis.is_empty() {
        return Err(reject(
            ErrorCode::RuleBasisMissing,
            "缺官方依据出处（口径问题必须能指到文件与条款）".to_string(),
        ));
    }

    let version = def.version.trim().to_string();
    if version.is_empty() {
        return Err(reject(
            ErrorCode::RuleLoadFailed,
            "规则版本为空".to_string(),
        ));
    }

    let sql = match (&def.sql, &def.sql_lines) {
        (Some(_), Some(_)) => {
            return Err(reject(
                ErrorCode::RuleLoadFailed,
                "sql 与 sql_lines 只能给一个".to_string(),
            ))
        }
        (Some(s), None) => s.trim().to_string(),
        (None, Some(lines)) => lines.join("\n").trim().to_string(),
        (None, None) => {
            return Err(reject(
                ErrorCode::RuleLoadFailed,
                "规则 SQL 为空（给 sql 或 sql_lines）".to_string(),
            ))
        }
    };
    if sql.is_empty() {
        return Err(reject(
            ErrorCode::RuleLoadFailed,
            "规则 SQL 为空".to_string(),
        ));
    }

    let rule_kind = RuleKind::from_token(def.rule_kind.trim()).ok_or_else(|| {
        reject(
            ErrorCode::RuleLoadFailed,
            format!(
                "检查类型代号不认识：「{}」（可用：required / required_conditional / enum / unit / formula / data_shape / duplicate）",
                def.rule_kind
            ),
        )
    })?;

    let severity = Severity::from_token(def.severity.trim()).ok_or_else(|| {
        reject(
            ErrorCode::RuleLoadFailed,
            format!(
                "严重度代号不认识：「{}」（可用：blocking / hint）",
                def.severity
            ),
        )
    })?;

    let mut params = BTreeMap::new();
    for (name, value) in &def.params {
        let param = RuleParam::from_value(value).map_err(|e| {
            reject(
                ErrorCode::RuleLoadFailed,
                format!("参数「{}」取值不合法：{}", name, e),
            )
        })?;
        params.insert(name.clone(), param);
    }

    let indicator_code = def
        .indicator_code
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(Rule {
        rule_id,
        indicator_code,
        check_column: def
            .check_column
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        rule_kind,
        sql,
        params,
        basis,
        version,
        severity,
        effective_from: def
            .effective_from
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        status: RuleStatus::Active,
        source_file: source_file.to_string(),
    })
}

/// 版本归并：同一口径槽位 = (检查类型, 适用指标, 检查项) 内，版本号最高的一条生效，
/// 其余标为「已被取代」**保留可追**（不物理删除）。
///
/// 检查项这一段不能少：枚举越界下有四条不同的检查（报告期格式 / 报告期类型 /
/// 计量单位 / 指标编号），它们同属「枚举越界」但各管各的列。若槽位只按
/// (检查类型, 适用指标) 分，换一版「报告期格式」的口径会把另外三条一起顶成「已被取代」。
///
/// 同槽位内出现两条**同版本**规则是歧义（到底哪条算口径？），直接报错，不二选一。
fn resolve_versions(rules: &mut [Rule]) -> Result<(), EngineError> {
    let mut slots: BTreeMap<(RuleKind, Option<String>, Option<String>), Vec<usize>> =
        BTreeMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        slots
            .entry((
                rule.rule_kind,
                rule.indicator_code.clone(),
                rule.check_column.clone(),
            ))
            .or_default()
            .push(idx);
    }

    for ((kind, indicator, check_column), idxs) in slots {
        let mut best = idxs[0];
        for &idx in &idxs[1..] {
            match compare_versions(&rules[idx].version, &rules[best].version) {
                Ordering::Greater => best = idx,
                Ordering::Equal => {
                    return Err(EngineError::at(
                        ErrorCode::RuleLoadFailed,
                        format!(
                            "同一口径槽位（{} · {} · {}）下有两条同版本规则：{} 与 {}，口径有歧义",
                            kind.as_str(),
                            check_column.as_deref().unwrap_or("全部检查项"),
                            indicator.as_deref().unwrap_or("全部指标"),
                            rules[best].rule_id,
                            rules[idx].rule_id
                        ),
                        format!("{} · {}", rules[best].source_file, rules[best].rule_id),
                    ));
                }
                Ordering::Less => {}
            }
        }
        for &idx in &idxs {
            rules[idx].status = if idx == best {
                RuleStatus::Active
            } else {
                RuleStatus::Superseded
            };
        }
    }
    Ok(())
}

/// 版本号比较：`2021.2` > `2021.1` > `v1`；纯数字段按数值比，其余按字典序。
///
/// 比较结果确定 —— 同一组规则装载两次的版本归并结果必须一致。
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let sa = version_segments(a);
    let sb = version_segments(b);
    let len = sa.len().max(sb.len());
    for i in 0..len {
        match (sa.get(i), sb.get(i)) {
            (Some(x), Some(y)) => {
                if let (Ok(nx), Ok(ny)) = (x.parse::<u64>(), y.parse::<u64>()) {
                    if nx != ny {
                        return nx.cmp(&ny);
                    }
                } else {
                    let c = x.cmp(y);
                    if c != Ordering::Equal {
                        return c;
                    }
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => break,
        }
    }
    Ordering::Equal
}

/// 版本号分段（去掉开头的 v/V，按 `.` 切分）
fn version_segments(version: &str) -> Vec<&str> {
    let trimmed = version.trim();
    let trimmed = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    trimmed.split('.').collect()
}

/// 规则集清单
fn build_manifest(
    rule_dir: String,
    rules: &[Rule],
    rejected: &[RejectedRule],
    syntax_errors: &[SyntaxErrorEntry],
    blocked: Vec<BlockedSwitch>,
    files: Vec<String>,
    rule_sets: Vec<String>,
    seed_files: Vec<String>,
    seed_sets: Vec<SeedManifest>,
) -> RuleManifest {
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for rule in rules {
        *by_status
            .entry(rule.status.as_str().to_string())
            .or_insert(0) += 1;
    }

    let active: Vec<&Rule> = rules
        .iter()
        .filter(|r| r.status == RuleStatus::Active)
        .collect();
    let superseded = rules
        .iter()
        .filter(|r| r.status == RuleStatus::Superseded)
        .count();
    let pending = rules
        .iter()
        .filter(|r| r.status == RuleStatus::Pending)
        .count();
    let basis_missing = rejected
        .iter()
        .filter(|r| r.code == ErrorCode::RuleBasisMissing.as_str())
        .count();

    let set_version = active
        .iter()
        .map(|r| r.version.clone())
        .max_by(|a, b| compare_versions(a, b))
        .unwrap_or_default();

    let mut slots: BTreeSet<(RuleKind, Option<String>, Option<String>)> = BTreeSet::new();
    for rule in rules {
        slots.insert((
            rule.rule_kind,
            rule.indicator_code.clone(),
            rule.check_column.clone(),
        ));
    }

    RuleManifest {
        rule_dir,
        set_version,
        set_fingerprint: fingerprint(&active),
        total: rules.len(),
        active: active.len(),
        superseded,
        pending,
        blocked_switches: blocked,
        basis_missing,
        syntax_error_count: syntax_errors.len(),
        execution_failed_count: 0,
        execution_skipped_count: 0,
        execution_failures: Vec::new(),
        slots: slots.len(),
        by_status,
        files,
        rule_sets,
        rejected: rejected.to_vec(),
        syntax_errors: syntax_errors.to_vec(),
        seed_files,
        seed_sets,
    }
}

/// 语法预校验：语法错的规则标为「不生效」，不参与执行，但**不中止整轮**。
///
/// 与「缺依据拒绝装载」的区别：缺依据是口径本身站不住（没法向科室解释），
/// 必须停下来让业务补；语法错只是这条规则写坏了，改好就能用 —— 其余规则照常执行，
/// 出错位置在规则清单里逐条列出（不静默通过）。
///
/// 已在版本归并之后执行：版本归并先定「哪条是当前口径」，再判它写得好不好。
/// 最高版本若语法不通过，该槽位本轮就没有生效规则（**不自动回退旧版** ——
/// 悄悄换口径比缺一条检查更危险），旧版仍标「已被取代」保留可追。
fn apply_syntax_check(rules: &mut [Rule]) -> Vec<SyntaxErrorEntry> {
    let mut out = Vec::new();
    for rule in rules.iter_mut() {
        // 已被取代的旧版不参与执行，不必预校验
        if rule.status != RuleStatus::Active {
            continue;
        }
        let report = match syntax::check_sql(&rule.sql) {
            Some(r) => r,
            None => continue,
        };
        rule.status = RuleStatus::Invalid;
        let (line, column) = syntax::line_column(&rule.sql, report.error.offset);
        out.push(SyntaxErrorEntry {
            source_file: rule.source_file.clone(),
            rule_id: rule.rule_id.clone(),
            rule_name: rule.display_name(),
            code: ErrorCode::RuleSyntaxError.as_str().to_string(),
            token_type: report.error.token_type.clone(),
            found: report.error.found.clone(),
            offset: report.error.offset,
            message: format!(
                "规则 {}（{}）的 SQL 语法不通过：出错 token 类型「{}」，原文「{}」，位置在第 {} 行第 {} 列（字节偏移 {}，检测方式：{}）；该规则本次不生效，其余规则照常执行",
                rule.rule_id,
                rule.display_name(),
                report.error.token_type,
                report.error.found,
                line,
                column,
                report.error.offset,
                report.detector.as_str(),
            ),
        });
    }
    out
}

/// 生效规则的指纹：规则编号|版本|SQL 摘要|依据 的 sha256 前 16 位。
///
/// 用途：历史批次能说清「这批结论是按哪套口径算的」—— 指纹不同即口径不同。
///
/// SQL 摘要取 **trim 后的文本**：内置兜底表与规则目录里的同一套口径，
/// 一边是编译出来的（末尾带换行）、一边是从文件读进来的（已 trim），
/// 不 trim 就会算出两个不同指纹 —— 同一套口径两个指纹，等于口径指纹没用。
fn fingerprint(rules: &[&Rule]) -> String {
    let mut items: Vec<String> = rules
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}|{}",
                r.rule_id,
                r.version,
                sha256_hex(r.sql.trim().as_bytes()),
                r.basis
            )
        })
        .collect();
    items.sort();
    let mut hasher = Sha256::new();
    hasher.update(items.join("\n").as_bytes());
    hex::encode(hasher.finalize()).chars().take(16).collect()
}

/// sha256 十六进制摘要
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 版本号比较有确定序() {
        assert_eq!(compare_versions("2021.2", "2021.1"), Ordering::Greater);
        assert_eq!(compare_versions("2021.1", "2021.2"), Ordering::Less);
        assert_eq!(compare_versions("2021.1", "2021.1"), Ordering::Equal);
        assert_eq!(compare_versions("v2", "v1"), Ordering::Greater);
        assert_eq!(compare_versions("v1", "1"), Ordering::Equal);
        assert_eq!(compare_versions("2021.10", "2021.9"), Ordering::Greater);
        assert_eq!(compare_versions("2021.1", "2021.1.0"), Ordering::Less);
    }

    #[test]
    fn 指纹随口径内容变化() {
        let mut rules = super::super::model::builtin_rules();
        let a = fingerprint(&[&rules[0]]);
        rules[0].sql.push_str(" -- 改了");
        let b = fingerprint(&[&rules[0]]);
        assert_ne!(a, b, "口径内容变了，指纹必须跟着变");
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn 内置规则集可执行且带清单() {
        let set = builtin_set();
        // 内置口径表：五列必填 + 比率型指标条件必填 + 表内业务主键唯一 + 四条枚举越界
        let builtin = super::super::model::builtin_rules();
        assert_eq!(set.executable().len(), builtin.len());
        assert_eq!(set.manifest().active, builtin.len());
        assert!(
            builtin.len() >= 7,
            "内置口径表至少含必填、条件必填、唯一与四条枚举越界"
        );
        assert_eq!(set.manifest().basis_missing, 0);
        assert_eq!(set.manifest().set_version, "2021.1");
        // 四条枚举越界各占一个口径槽位（检查项不同，互相不顶掉）
        let enum_slots: Vec<&str> = builtin
            .iter()
            .filter(|r| r.rule_kind == RuleKind::EnumBound)
            .map(|r| r.check_column.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(enum_slots.len(), 4);
        let mut uniq = enum_slots.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), 4, "四条枚举规则必须各占一个检查项");
    }

    #[test]
    fn 规则槽位去重后计数正确() {
        let mut rules = super::super::model::builtin_rules();
        let base = rules.len();
        rules.push(Rule {
            rule_id: "R-REQUIRED-900".to_string(),
            indicator_code: Some("MER-HR-01".to_string()),
            check_column: None,
            rule_kind: RuleKind::RequiredField,
            sql: "SELECT 1".to_string(),
            params: BTreeMap::new(),
            basis: "依据".to_string(),
            version: "2021.1".to_string(),
            severity: Severity::Hint,
            effective_from: None,
            status: RuleStatus::Active,
            source_file: "t".to_string(),
        });
        let manifest = build_manifest(
            "d".to_string(),
            &rules,
            &[],
            &[],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(manifest.slots, base + 1, "不同指标应另占一个槽位");
        assert_eq!(manifest.total, base + 1);
    }

    #[test]
    fn 五列必填与条件必填占不同槽位() {
        // 两者都是「必填」类检查，但口径范围不同（五列必填适用全部指标，
        // 条件必填只适用比率型指标）—— 同一槽位会让换版把其中一条顶掉。
        let rules = super::super::model::builtin_rules();
        let required = rules
            .iter()
            .find(|r| r.rule_kind == RuleKind::RequiredField)
            .unwrap();
        let conditional = rules
            .iter()
            .find(|r| r.rule_kind == RuleKind::ConditionalRequired)
            .unwrap();
        assert_ne!(required.rule_kind, conditional.rule_kind);
        assert_eq!(required.indicator_code, conditional.indicator_code);
    }
}
