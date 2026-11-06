//! 口径换版闸门与口径差异标注。
//!
//! # 换版为什么要有闸门
//!
//! 属地口径文件会改版。改版一旦**静默生效**，历史批次的结论就会漂移：
//! 上个月按 2021.1 判「通过」的报告期，这个月按 2022.1 重算可能变成「不通过」，
//! 而没人说得清这批结论是按哪一版口径算的。所以换版走两道闸门：
//!
//! 1. **生效日期闸门**：`effective_from` 之前不启用新版本（哪怕已经确认过）；
//! 2. **换版确认闸门**：新版本要**质控岗确认**（确认记录含确认人与时间）才生效；
//!    未确认就**继续用旧版**，并明确提示「哪一版没生效、为什么、怎么补」。
//!
//! 两道闸门都不放行时，引擎**不静默降级**：口径少一条就可能把不合格的行判成「通过」，
//! 因此被拦下的口径逐条进规则清单（`rule_status.manifest.blocked_switches`），
//! 写回结果契约，运维一眼看得到少了哪条检查。
//!
//! # 闸门管的是「已在用的口径被换掉」
//!
//! 状态库里还没用过任何口径版本（首次装载）时**不算换版** —— 没有旧版要保护，
//! 直接用规则目录里版本最高的那一版。已经用过的版本旁边又出现更高的版本，
//! 就是换版，必须经质控岗确认。
//!
//! # 口径差异清单
//!
//! 换版前要能回答「到底哪几条口径变了」。差异清单按**口径槽位**
//! （检查类型 + 适用指标 + 检查项）逐项对比两个版本，列出变化项与前后取值：
//! 必填列清单、期望取值清单、期望单位（逐指标）、比对精度（逐指标）、严重度、官方依据。
//! 清单随 `regval export diff` 导出，给质控岗与科室看。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::errors::ErrorCode;

use super::model::{Rule, RuleKind, RuleParam, RuleStatus};

/// 换版确认记录（质控岗确认）。
///
/// 确认人与确认时间**必填** —— 没有人的口径换版，等于没人负责。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Approval {
    /// 被确认的新口径版本
    pub to_version: String,
    /// 被替换的旧口径版本（空串 = 此前没有版本）
    #[serde(default)]
    pub from_version: String,
    /// 口径槽位键（空串 = 整套口径，对该版本的所有槽位都算数）
    #[serde(default)]
    pub slot_key: String,
    /// 确认人（质控岗）
    pub approver: String,
    /// 确认时间
    pub approved_at: String,
    /// 备注（换版原因、口径文件文号等）
    #[serde(default)]
    pub note: String,
}

impl Approval {
    /// 确认记录是否可用（确认人与版本都不能为空）
    pub fn is_valid(&self) -> bool {
        !self.to_version.trim().is_empty() && !self.approver.trim().is_empty()
    }
}

/// 换版确认记录索引
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalIndex {
    pub entries: Vec<Approval>,
}

impl ApprovalIndex {
    /// 查某个口径槽位的某个版本是否已确认。
    ///
    /// **整套口径的确认（`slot_key` 为空）对该版本的所有槽位都算数** ——
    /// 属地文件整体换版时，质控岗确认的是「这一版口径」，不必逐条检查再确认一遍。
    pub fn find(&self, slot_key: &str, to_version: &str) -> Option<&Approval> {
        self.entries.iter().find(|a| {
            a.to_version == to_version && (a.slot_key.is_empty() || a.slot_key == slot_key)
        })
    }

    /// 某个版本上的全部确认记录
    pub fn for_version(&self, to_version: &str) -> Vec<&Approval> {
        self.entries
            .iter()
            .filter(|a| a.to_version == to_version)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 换版被闸门拦下的一个口径槽位
///
/// 逐条进规则清单 —— 少一条口径就可能把不合格的行判成「通过」，不能静默。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedSwitch {
    /// 口径槽位键（`检查类型|适用指标|检查项`）
    pub slot_key: String,
    /// 口径槽位的中文说明（给业务人员看）
    pub slot_label: String,
    /// 原本要启用的新版本
    pub to_version: String,
    /// 实际继续使用的旧版本（空串 = 没有可回退的旧版）
    pub from_version: String,
    /// 机器可读错误码
    pub code: String,
    /// 中文说明（为什么没生效、怎么补）
    pub reason: String,
}

/// 换版闸门
///
/// 闸门要三样东西才能判：谁确认过（确认记录）、哪一版在用（已用过的口径版本）、
/// 今天是哪天（生效日期）。
pub struct VersionGate<'a> {
    /// 质控岗确认记录
    pub approvals: &'a ApprovalIndex,
    /// **已经实际用过**的口径版本（历史批次用过的）。
    /// 空 = 还没用过任何版本（首次装载，不算换版）。
    pub applied_versions: &'a [String],
    /// 今天（`YYYY-MM-DD`），用于判断生效日期
    pub today: &'a str,
}

/// 口径槽位键：检查类型 + 适用指标 + 检查项
pub type SlotKey = (RuleKind, Option<String>, Option<String>);

/// 槽位键的字符串形式（确认记录与差异清单都用它）
///
/// 形如 `enum|MER-TL-04|计量单位`；适用指标与检查项缺省时用 `*`。
pub fn slot_key_string(key: &SlotKey) -> String {
    format!(
        "{}|{}|{}",
        key.0.as_token(),
        key.1.clone().unwrap_or_else(|| "*".to_string()),
        key.2.clone().unwrap_or_else(|| "*".to_string())
    )
}

/// 槽位的中文说明（给业务人员看）
pub fn slot_label(key: &SlotKey) -> String {
    let kind = key.0.as_str();
    let scope = key.1.clone().unwrap_or_else(|| "全部指标".to_string());
    match &key.2 {
        Some(column) => format!("{} · {} · {}", kind, column, scope),
        None => format!("{} · {}", kind, scope),
    }
}

/// 应用换版闸门：逐口径槽位决定哪一版生效，并给出被拦下的换版记录。
///
/// 闸门按**版本从高到低**逐个试：生效日期到了、且（不需要确认 / 已确认）→ 该版本生效；
/// 都被拦下就继续往下一版找 —— 这就是「未确认则继续用旧版」。
/// 找得到就生效；一版都找不到（该槽位只有被拦下的新版、旧版又不在规则目录里），
/// 该槽位本轮没有生效口径，逐条记进 [`BlockedSwitch`]。
pub fn apply_gate(rules: &mut [Rule], gate: &VersionGate) -> Vec<BlockedSwitch> {
    let mut blocked: Vec<BlockedSwitch> = Vec::new();

    let mut slots: BTreeMap<SlotKey, Vec<usize>> = BTreeMap::new();
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

    for (key, idxs) in slots {
        let key_str = slot_key_string(&key);
        let label = slot_label(&key);

        // 同一槽位内按版本从高到低排
        let mut ordered = idxs.clone();
        ordered.sort_by(|a, b| {
            super::loader::compare_versions(&rules[*b].version, &rules[*a].version)
        });

        let mut chosen: Option<usize> = None;
        let mut chosen_version = String::new();
        for &idx in &ordered {
            let rule = &rules[idx];

            // 闸门一：生效日期
            if let Some(reason) = effective_date_block(&rule.effective_from, gate.today) {
                blocked.push(BlockedSwitch {
                    slot_key: key_str.clone(),
                    slot_label: label.clone(),
                    to_version: rule.version.clone(),
                    from_version: String::new(),
                    code: ErrorCode::RuleEffectiveDateNotReached.as_str().to_string(),
                    reason,
                });
                continue;
            }

            // 闸门二：换版确认
            if needs_approval(&rule.version, gate) {
                match gate.approvals.find(&key_str, &rule.version) {
                    Some(approval) => {
                        // 已确认：继续往下走时把确认人记进原因，便于回溯
                        let _ = approval;
                    }
                    None => {
                        blocked.push(BlockedSwitch {
                            slot_key: key_str.clone(),
                            slot_label: label.clone(),
                            to_version: rule.version.clone(),
                            from_version: String::new(),
                            code: ErrorCode::RuleSwitchUnconfirmed.as_str().to_string(),
                            reason: format!(
                                "口径 {} 换到版本 {} 未经质控岗确认，本轮继续用旧版；请执行 `regval rules approve --version {} --approver <确认人姓名>` 后重跑",
                                label, rule.version, rule.version
                            ),
                        });
                        continue;
                    }
                }
            }

            chosen = Some(idx);
            chosen_version = rule.version.clone();
            break;
        }

        // 定状态：选中的那条生效；比它高的（被拦下的新版）标「待确认生效」；
        // 比它低的（旧版）标「已被取代」，保留可追。
        for &idx in &ordered {
            let status = match chosen {
                Some(c) if c == idx => RuleStatus::Active,
                Some(_) => RuleStatus::Superseded,
                None => RuleStatus::Pending,
            };
            rules[idx].status = status;
        }

        // 回填「实际继续使用的旧版本」
        if chosen.is_some() {
            for b in blocked.iter_mut() {
                if b.slot_key == key_str && b.from_version.is_empty() {
                    b.from_version = chosen_version.clone();
                }
            }
        }
    }

    blocked
}

/// 这一版是否需要质控岗确认
///
/// - 状态库里还没用过任何口径版本（首次装载）→ 不算换版，不需确认；
/// - 这一版用过 → 不是换版，不需确认；
/// - 否则 = 换版 → 需确认。
fn needs_approval(version: &str, gate: &VersionGate) -> bool {
    if gate.applied_versions.is_empty() {
        return false;
    }
    !gate
        .applied_versions
        .iter()
        .any(|v| v.trim() == version.trim())
}

/// 生效日期闸门：返回 `Some(原因)` 表示这一版现在还不能启用
fn effective_date_block(effective_from: &Option<String>, today: &str) -> Option<String> {
    let raw = effective_from.as_deref().unwrap_or("").trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw.replace('/', "-");
    let date = normalized.get(..10).unwrap_or("");
    let today_date = today.get(..10).unwrap_or(today);
    if date.len() != 10 || !date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Some(format!(
            "口径版本的生效日期「{}」格式无法识别（须 YYYY-MM-DD），该版本本轮不启用",
            raw
        ));
    }
    if date > today_date {
        return Some(format!(
            "口径版本的生效日期为 {}，今天 {} 还没到，该版本本轮不启用",
            date, today_date
        ));
    }
    None
}

// ——————————————————————————————————————————————————————————————
// 口径差异清单
// ——————————————————————————————————————————————————————————————

/// 一处口径变化
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaliberChange {
    /// 适用指标（具体编号或「全部指标」）
    pub indicator_code: String,
    /// 检查类型（中文）
    pub check_kind: String,
    /// 检查项（同一检查类型下的不同检查项）
    pub check_column: String,
    /// 变化类型：新增 / 取消 / 修改
    pub change: String,
    /// 变化项（必填列清单 / 期望取值清单 / 期望单位 / 比对精度 / 严重度 / 官方依据）
    pub field: String,
    /// 前值（旧版本）
    pub before: String,
    /// 后值（新版本）
    pub after: String,
}

/// 口径差异清单：两个版本之间逐项口径的差异
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaliberDiff {
    /// 旧版本
    pub from_version: String,
    /// 新版本
    pub to_version: String,
    /// 逐项变化
    pub changes: Vec<CaliberChange>,
    /// 涉及的口径项数（检查类型 + 适用指标 + 检查项）
    pub compared_slots: usize,
    /// 完全没变的口径项数
    pub unchanged_slots: usize,
}

impl CaliberDiff {
    /// 变化条数
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    /// 按变化类型计数（新增 / 取消 / 修改）
    pub fn count_by_change(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for c in &self.changes {
            *out.entry(c.change.clone()).or_insert(0) += 1;
        }
        out
    }
}

/// 对比两个版本的口径差异
///
/// 逐口径槽位对比：旧版有新版没有 → 「取消」；新版有旧版没有 → 「新增」；
/// 两边都有 → 逐项比对口径取值（必填列清单 / 期望取值 / 期望单位 / 比对精度 /
/// 严重度 / 官方依据），不同的逐项列出前后取值。
pub fn diff_rule_sets(
    old: &[&Rule],
    new: &[&Rule],
    from_version: &str,
    to_version: &str,
) -> CaliberDiff {
    let mut diff = CaliberDiff {
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        ..Default::default()
    };

    let old_by_slot = index_by_slot(old);
    let new_by_slot = index_by_slot(new);

    let mut all_slots: Vec<SlotKey> = old_by_slot.keys().cloned().collect();
    for key in new_by_slot.keys() {
        if !all_slots.contains(key) {
            all_slots.push(key.clone());
        }
    }
    all_slots.sort();

    for key in all_slots {
        let key_str = slot_key_string(&key);
        let label = slot_label(&key);
        let indicator = key.1.clone().unwrap_or_else(|| "全部指标".to_string());
        let check_kind = key.0.as_str().to_string();
        let check_column = key.2.clone().unwrap_or_default();

        match (old_by_slot.get(&key), new_by_slot.get(&key)) {
            (Some(old_rule), None) => {
                diff.changes.push(CaliberChange {
                    indicator_code: indicator,
                    check_kind,
                    check_column,
                    change: "取消".to_string(),
                    field: "整项检查".to_string(),
                    before: format!("{}（版本 {}）", label, old_rule.version),
                    after: "（新版本不再检查这一项）".to_string(),
                });
            }
            (None, Some(new_rule)) => {
                diff.changes.push(CaliberChange {
                    indicator_code: indicator,
                    check_kind,
                    check_column,
                    change: "新增".to_string(),
                    field: "整项检查".to_string(),
                    before: "（旧版本没有这一项）".to_string(),
                    after: format!("{}（版本 {}）", label, new_rule.version),
                });
            }
            (Some(old_rule), Some(new_rule)) => {
                let changes = compare_rules(old_rule, new_rule, &indicator);
                if changes.is_empty() {
                    diff.unchanged_slots += 1;
                } else {
                    diff.changes.extend(changes);
                }
                diff.compared_slots += 1;
            }
            (None, None) => {}
        }
        let _ = key_str;
    }

    diff
}

/// 按口径槽位归并规则（同一槽位取第一条）
fn index_by_slot<'a>(rules: &'a [&'a Rule]) -> BTreeMap<SlotKey, &'a Rule> {
    let mut out: BTreeMap<SlotKey, &Rule> = BTreeMap::new();
    for rule in rules {
        let key = (
            rule.rule_kind,
            rule.indicator_code.clone(),
            rule.check_column.clone(),
        );
        out.entry(key).or_insert(rule);
    }
    out
}

/// 逐项比对两条同槽位规则的口径取值
fn compare_rules(old: &Rule, new: &Rule, indicator: &str) -> Vec<CaliberChange> {
    let mut out = Vec::new();
    let check_kind = new.rule_kind.as_str().to_string();
    let check_column = new.check_column.clone().unwrap_or_default();

    // 必填列清单
    let old_labels = required_labels(old);
    let new_labels = required_labels(new);
    if old_labels != new_labels {
        out.push(change_of(
            indicator,
            &check_kind,
            &check_column,
            "必填列清单",
            join_or_none(&old_labels),
            join_or_none(&new_labels),
        ));
    }

    // 期望取值清单
    let old_expected = param_text(old, "expected_values");
    let new_expected = param_text(new, "expected_values");
    if old_expected != new_expected {
        out.push(change_of(
            indicator,
            &check_kind,
            &check_column,
            "期望取值清单",
            old_expected,
            new_expected,
        ));
    }

    // 期望单位（逐指标）
    out.extend(compare_unit_tables(
        old,
        new,
        indicator,
        &check_kind,
        &check_column,
    ));

    // 比对精度（逐指标）
    out.extend(compare_precision_tables(
        old,
        new,
        indicator,
        &check_kind,
        &check_column,
    ));

    // 比率型指标清单
    let old_ratio = param_text(old, "ratio_codes");
    let new_ratio = param_text(new, "ratio_codes");
    if old_ratio != new_ratio {
        out.push(change_of(
            indicator,
            &check_kind,
            &check_column,
            "比率型指标清单",
            old_ratio,
            new_ratio,
        ));
    }

    // 严重度
    if old.severity != new.severity {
        out.push(change_of(
            indicator,
            &check_kind,
            &check_column,
            "严重度",
            old.severity.as_str().to_string(),
            new.severity.as_str().to_string(),
        ));
    }

    // 官方依据
    if old.basis != new.basis {
        out.push(change_of(
            indicator,
            &check_kind,
            &check_column,
            "官方依据",
            old.basis.clone(),
            new.basis.clone(),
        ));
    }

    out
}

/// 组装一条「修改」类变化
#[allow(clippy::too_many_arguments)]
fn change_of(
    indicator: &str,
    check_kind: &str,
    check_column: &str,
    field: &str,
    before: String,
    after: String,
) -> CaliberChange {
    CaliberChange {
        indicator_code: indicator.to_string(),
        check_kind: check_kind.to_string(),
        check_column: check_column.to_string(),
        change: "修改".to_string(),
        field: field.to_string(),
        before,
        after,
    }
}

/// 期望单位表逐指标对比
///
/// 期望单位表是「指标编号 → 单位」的映射，逐指标比才有意义：
/// 「MER-TL-04 的期望单位由 % 改为 例」比「整张表变了」有用得多。
fn compare_unit_tables(
    old: &Rule,
    new: &Rule,
    indicator: &str,
    check_kind: &str,
    check_column: &str,
) -> Vec<CaliberChange> {
    let old_map = parse_json_map(&param_text(old, "expected_units"));
    let new_map = parse_json_map(&param_text(new, "expected_units"));
    if old_map.is_empty() && new_map.is_empty() {
        return Vec::new();
    }
    let mut codes: Vec<&String> = old_map.keys().chain(new_map.keys()).collect();
    codes.sort();
    codes.dedup();

    let mut out = Vec::new();
    for code in codes {
        let before = old_map.get(code).cloned().unwrap_or_default();
        let after = new_map.get(code).cloned().unwrap_or_default();
        if before == after {
            continue;
        }
        out.push(CaliberChange {
            indicator_code: if indicator == "全部指标" {
                code.clone()
            } else {
                indicator.to_string()
            },
            check_kind: check_kind.to_string(),
            check_column: check_column.to_string(),
            change: "修改".to_string(),
            field: "期望单位".to_string(),
            before: if before.is_empty() {
                "（旧版本未登记）".to_string()
            } else {
                before
            },
            after: if after.is_empty() {
                "（新版本不再登记）".to_string()
            } else {
                after
            },
        });
    }
    out
}

/// 逐指标比对精度表
fn compare_precision_tables(
    old: &Rule,
    new: &Rule,
    indicator: &str,
    check_kind: &str,
    check_column: &str,
) -> Vec<CaliberChange> {
    let old_map = parse_precision_table(&param_text(old, "precision_table"));
    let new_map = parse_precision_table(&param_text(new, "precision_table"));
    if old_map.is_empty() && new_map.is_empty() {
        return Vec::new();
    }
    let mut codes: Vec<&String> = old_map.keys().chain(new_map.keys()).collect();
    codes.sort();
    codes.dedup();

    let mut out = Vec::new();
    for code in codes {
        let before = old_map.get(code).cloned().unwrap_or_default();
        let after = new_map.get(code).cloned().unwrap_or_default();
        if before == after {
            continue;
        }
        out.push(CaliberChange {
            indicator_code: if indicator == "全部指标" {
                code.clone()
            } else {
                indicator.to_string()
            },
            check_kind: check_kind.to_string(),
            check_column: check_column.to_string(),
            change: "修改".to_string(),
            field: "比对精度".to_string(),
            before: if before.is_empty() {
                "（旧版本未登记）".to_string()
            } else {
                before
            },
            after: if after.is_empty() {
                "（新版本不再登记）".to_string()
            } else {
                after
            },
        });
    }
    out
}

/// 规则参数里的必填列清单（`label_1` … `label_n`，按序号排）
fn required_labels(rule: &Rule) -> Vec<String> {
    let mut labels: Vec<(u32, String)> = Vec::new();
    for (name, param) in &rule.params {
        if let Some(rest) = name.strip_prefix("label_") {
            if let Ok(n) = rest.parse::<u32>() {
                if let RuleParam::Text(text) = param {
                    labels.push((n, text.clone()));
                }
            }
        }
    }
    labels.sort_by(|a, b| a.0.cmp(&b.0));
    labels.into_iter().map(|(_, t)| t).collect()
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "（无）".to_string()
    } else {
        items.join("、")
    }
}

/// 取规则参数的文本取值（无该参数时返回空串）
fn param_text(rule: &Rule, name: &str) -> String {
    match rule.params.get(name) {
        Some(RuleParam::Text(text)) => text.clone(),
        Some(RuleParam::Int(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 解析「键 → 值」的 JSON 对象（期望单位表）
fn parse_json_map(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if raw.trim().is_empty() {
        return out;
    }
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(map) = parsed.as_object() {
        for (k, v) in map {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.insert(k.clone(), text);
        }
    }
    out
}

/// 解析逐指标精度表（`[{code, precision, scale, tolerance}]`）
fn parse_precision_table(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if raw.trim().is_empty() {
        return out;
    }
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let items = match parsed.as_array() {
        Some(items) => items,
        None => return out,
    };
    for item in items {
        let code = item
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if code.is_empty() {
            continue;
        }
        let precision = item
            .get("precision")
            .map(|v| v.to_string())
            .unwrap_or_default();
        let tolerance = item
            .get("tolerance")
            .map(|v| v.to_string())
            .unwrap_or_default();
        out.insert(
            code,
            format!("小数点后 {} 位（容差 {}）", precision, tolerance),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::model::Severity;
    use std::collections::BTreeMap;

    fn rule(id: &str, version: &str, kind: RuleKind) -> Rule {
        Rule {
            rule_id: id.to_string(),
            indicator_code: None,
            check_column: None,
            rule_kind: kind,
            sql: "SELECT 1".to_string(),
            params: BTreeMap::new(),
            basis: "依据甲".to_string(),
            version: version.to_string(),
            severity: Severity::Blocking,
            effective_from: None,
            status: RuleStatus::Active,
            source_file: "t".to_string(),
        }
    }

    #[test]
    fn 生效日期未到则该版本不启用() {
        let gate = VersionGate {
            approvals: &ApprovalIndex::default(),
            applied_versions: &[],
            today: "2026-11-06",
        };
        assert!(effective_date_block(&Some("2027-01-01".to_string()), gate.today).is_some());
        assert!(effective_date_block(&Some("2026-01-01".to_string()), gate.today).is_none());
        assert!(effective_date_block(&None, gate.today).is_none());
    }

    #[test]
    fn 生效日期格式不对也不启用() {
        assert!(effective_date_block(&Some("2027/01".to_string()), "2026-11-06").is_some());
    }

    #[test]
    fn 首次装载不算换版() {
        let gate = VersionGate {
            approvals: &ApprovalIndex::default(),
            applied_versions: &[],
            today: "2026-11-06",
        };
        assert!(!needs_approval("2022.1", &gate));
    }

    #[test]
    fn 用过的版本再次出现不需确认_新版本需要确认() {
        let applied = vec!["2021.1".to_string()];
        let gate = VersionGate {
            approvals: &ApprovalIndex::default(),
            applied_versions: &applied,
            today: "2026-11-06",
        };
        assert!(!needs_approval("2021.1", &gate));
        assert!(needs_approval("2022.1", &gate));
    }

    #[test]
    fn 整套口径的确认对该版本所有槽位都算数() {
        let index = ApprovalIndex {
            entries: vec![Approval {
                to_version: "2022.1".to_string(),
                from_version: "2021.1".to_string(),
                slot_key: String::new(),
                approver: "张三".to_string(),
                approved_at: "2026-11-01 10:00:00".to_string(),
                note: String::new(),
            }],
        };
        assert!(index.find("enum|MER-TL-04|计量单位", "2022.1").is_some());
        assert!(index.find("enum|MER-TL-04|计量单位", "2023.1").is_none());
    }

    #[test]
    fn 差异清单列出具体前后取值() {
        let mut old = rule("R-UNIT-001", "2021.1", RuleKind::UnitCheck);
        old.params.insert(
            "expected_units".to_string(),
            RuleParam::Text("{\"MER-TL-04\":\"%\"}".to_string()),
        );
        let mut new = rule("R-UNIT-001", "2022.1", RuleKind::UnitCheck);
        new.params.insert(
            "expected_units".to_string(),
            RuleParam::Text("{\"MER-TL-04\":\"例\"}".to_string()),
        );

        let diff = diff_rule_sets(&[&old], &[&new], "2021.1", "2022.1");
        assert_eq!(diff.change_count(), 1);
        let change = &diff.changes[0];
        assert_eq!(change.indicator_code, "MER-TL-04");
        assert_eq!(change.field, "期望单位");
        assert_eq!(change.before, "%");
        assert_eq!(change.after, "例");
        assert_eq!(change.change, "修改");
    }

    #[test]
    fn 差异清单认得出新增与取消() {
        let old = rule("R-A", "2021.1", RuleKind::RequiredField);
        let new = rule("R-B", "2022.1", RuleKind::EnumBound);
        let diff = diff_rule_sets(&[&old], &[&new], "2021.1", "2022.1");
        let kinds: Vec<&str> = diff.changes.iter().map(|c| c.change.as_str()).collect();
        assert!(kinds.contains(&"取消"));
        assert!(kinds.contains(&"新增"));
    }
}
