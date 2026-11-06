//! 参数化规则执行器。
//!
//! # 规则是数据，取值走绑定参数
//!
//! 规则 SQL 的**文本**来自规则表，规则 SQL 里**比较用的取值**来自 `Rule::params`。
//! 两者在运行期不拼接到一起 —— 引擎里没有任何一处把参数值拼进 SQL 字符串。
//! 规则作者改了口径取值，SQL 文本不动；规则 SQL 里写了取值字面量，走的仍是
//! 绑定参数那条路（少一条拼接路径 = 少一处口径被悄悄改写的可能）。
//!
//! # 规则结果契约
//!
//! 规则 SQL 返回两到三列：
//!
//! | 列 | 含义 |
//! |---|---|
//! | 第 1 列 | 行键（`rows.row_uid`）—— 问题挂在哪一行 |
//! | 第 2 列（可选） | 问题区分键 —— 缺的是哪一列 / 哪一项检查不通过；枚举越界、单位不符、公式复算这几类**直接就是给人看的完整说明** |
//! | 第 3 列（可选） | 机器可读错误码 —— 同一类问题里还要再分小类时用它（公式复算的「分母为 0」与「非数值 / 负值」就靠它分开） |
//!
//! 第二、三列都可省略：只判「这一行有问题」时返回一列即可。
//!
//! # 一条规则一条结论，互不影响
//!
//! | 情况 | 结论 | 引擎行为 |
//! |---|---|---|
//! | 正常跑通 | 生效 | 产出问题集合 |
//! | 规则引用了本机库里没有的列 | 不生效 | 报 `MISSING_COLUMN`，点名缺哪一列；**不猜列** |
//! | 参数不全 / SQL 运行失败 | 执行失败 | 报 `RULE_EXEC_ERROR`；**其余规则继续** |
//!
//! 少一条口径就可能把不合格的行判成「通过」，所以没跑起来的规则**一律不静默**：
//! 逐条进本轮执行报告 → 结构化日志 → 结果契约的规则清单段（失败规则计数可见）。
//!
//! # 同一行同一规则只落一条问题
//!
//! 一条规则可能对同一行命中多次（必填缺失要同时看五列）。问题清单按
//! **(行键, 规则编号)** 归并成一条，缺失列合并成清单写进「问题说明（含依据）」，
//! 库层再由唯一索引 `UNIQUE (batch_ref, row_uid, rule_id, issue_key)` 兜底。

use std::collections::BTreeMap;

use rusqlite::{Connection, ToSql};

use crate::errors::ErrorCode;

use super::model::{ExecutionFailureEntry, Issue, Rule, RuleKind, RuleParam};

/// 空值绑定用（`:scope` 为空时表示适用于全部指标）
static NULL_TEXT: Option<String> = None;

/// 一条规则本次执行的结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleOutcome {
    /// 生效：正常执行并产出问题
    Executed,
    /// 不生效：规则引用了本机库里不存在的列（不猜列、不静默跳过）
    SkippedMissingColumn,
    /// 执行失败：参数不全或 SQL 运行失败（其余规则继续）
    Failed,
}

impl RuleOutcome {
    /// 结论的中文名（写进结果契约的规则清单段）
    pub fn as_str(self) -> &'static str {
        match self {
            RuleOutcome::Executed => "生效",
            RuleOutcome::SkippedMissingColumn => "不生效",
            RuleOutcome::Failed => "执行失败",
        }
    }
}

/// 一条规则的执行结果
#[derive(Debug, Clone)]
pub struct RuleExecution {
    /// 规则编号
    pub rule_id: String,
    /// 规则中文名（检查类型 + 适用指标）
    pub rule_name: String,
    /// 执行结论
    pub outcome: RuleOutcome,
    /// 本条规则产出的问题（已按行归并：同一行同一规则只一条）
    pub issues: Vec<Issue>,
    /// 机器可读错误码（正常执行时为空）
    pub error_code: Option<ErrorCode>,
    /// 中文说明（给业务人员看）
    pub message: String,
    /// 出错位置（缺哪一列 / 规则 id）
    pub location: String,
}

impl RuleExecution {
    /// 正常执行
    fn executed(rule: &Rule) -> Self {
        RuleExecution {
            rule_id: rule.rule_id.clone(),
            rule_name: rule.display_name(),
            outcome: RuleOutcome::Executed,
            issues: Vec::new(),
            error_code: None,
            message: String::new(),
            location: rule.rule_id.clone(),
        }
    }

    /// 没跑起来（缺列不生效 / 执行失败）
    fn not_run(
        rule: &Rule,
        outcome: RuleOutcome,
        code: ErrorCode,
        message: String,
        location: String,
    ) -> Self {
        RuleExecution {
            rule_id: rule.rule_id.clone(),
            rule_name: rule.display_name(),
            outcome,
            issues: Vec::new(),
            error_code: Some(code),
            message,
            location,
        }
    }
}

/// 一轮规则执行的报告
///
/// 汇总本轮每条规则的结论与产出的问题。`regval validate` 把它写进结果契约的
/// 规则清单段 —— 「这批结论按哪套口径算的、哪几条没跑起来」有据可查。
#[derive(Debug, Clone, Default)]
pub struct RuleRunReport {
    /// 逐条规则的执行结果（顺序与规则集一致）
    pub executions: Vec<RuleExecution>,
}

impl RuleRunReport {
    /// 本轮产出的全部问题（已按行归并）
    pub fn issues(&self) -> Vec<Issue> {
        self.executions
            .iter()
            .flat_map(|e| e.issues.iter().cloned())
            .collect()
    }

    /// 生效的规则数
    pub fn executed_rules(&self) -> usize {
        self.count(RuleOutcome::Executed)
    }

    /// 因缺列而不生效的规则数
    pub fn skipped_rules(&self) -> usize {
        self.count(RuleOutcome::SkippedMissingColumn)
    }

    /// 执行失败的规则数
    pub fn failed_rules(&self) -> usize {
        self.count(RuleOutcome::Failed)
    }

    /// 没跑起来的规则数（缺列 + 失败）
    pub fn not_run_rules(&self) -> usize {
        self.executions
            .iter()
            .filter(|e| e.outcome != RuleOutcome::Executed)
            .count()
    }

    /// 没跑起来的规则明细（写进结果契约与日志）
    pub fn failure_entries(&self) -> Vec<ExecutionFailureEntry> {
        self.executions
            .iter()
            .filter(|e| e.outcome != RuleOutcome::Executed)
            .map(|e| ExecutionFailureEntry {
                rule_id: e.rule_id.clone(),
                rule_name: e.rule_name.clone(),
                outcome: e.outcome.as_str().to_string(),
                code: e
                    .error_code
                    .map(|c| c.as_str().to_string())
                    .unwrap_or_default(),
                message: e.message.clone(),
                location: e.location.clone(),
            })
            .collect()
    }

    fn count(&self, outcome: RuleOutcome) -> usize {
        self.executions
            .iter()
            .filter(|e| e.outcome == outcome)
            .count()
    }
}

/// 执行一批规则，产出一轮执行报告。
///
/// **一条规则失败不影响其余规则**：缺列的规则不生效、跑挂的规则记失败，
/// 报告里逐条可见（不静默跳过、不静默通过）。
pub fn execute_rules(conn: &Connection, rules: &[&Rule], batch_ref: &str) -> RuleRunReport {
    let mut report = RuleRunReport::default();
    for rule in rules {
        report.executions.push(execute_rule(conn, rule, batch_ref));
    }
    report
}

/// 执行一条规则，产出问题集合。
///
/// 出错不外抛：问题分三种（生效 / 缺列不生效 / 执行失败），都由返回值说清楚。
/// 外抛会让一条写坏的规则把整轮校验带下水 —— 而口径少一条的代价，
/// 恰恰是「不合格的行被当成通过」。
pub fn execute_rule(conn: &Connection, rule: &Rule, batch_ref: &str) -> RuleExecution {
    let mut execution = RuleExecution::executed(rule);

    // 1. 准备语句 —— 准备失败就是这条规则跑不起来（缺列 / 缺表 / 语法残留）
    let mut stmt = match conn.prepare(&rule.sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            let (outcome, code, message, location) = classify_prepare_error(&e, rule);
            return RuleExecution::not_run(rule, outcome, code, message, location);
        }
    };

    // 2. 枚举 SQL 里实际用到的命名参数 —— 只绑这些，多绑会被 SQLite 判为无效参数名
    let mut sql_params: Vec<String> = Vec::new();
    for idx in 1..=stmt.parameter_count() {
        match stmt.parameter_name(idx) {
            Some(name) => sql_params.push(name.to_string()),
            None => {
                // 位置参数（? / ?1）没法用命名参数绑定 —— 口径规则一律用命名参数
                return RuleExecution::not_run(
                    rule,
                    RuleOutcome::Failed,
                    ErrorCode::RuleExec,
                    format!(
                        "规则 {}（{}）使用了位置参数，无法按命名参数绑定；口径规则的取值必须写在 params 里",
                        rule.rule_id,
                        rule.display_name()
                    ),
                    rule.rule_id.clone(),
                );
            }
        }
    }

    // 3. 组装绑定：取值一律来自规则表的 params（外加批次号），不拼进 SQL
    let mut binds: Vec<(String, &dyn ToSql)> = Vec::new();
    for name in &sql_params {
        let bare = strip_param_prefix(name);
        if bare == "batch_ref" {
            binds.push((name.clone(), &batch_ref as &dyn ToSql));
        } else if let Some(param) = rule.params.get(bare) {
            binds.push((name.clone(), param_to_sql(param)));
        } else {
            return RuleExecution::not_run(
                rule,
                RuleOutcome::Failed,
                ErrorCode::RuleExec,
                format!(
                    "规则 {}（{}）的 SQL 引用了未提供的命名参数「{}」；取值必须写在规则表的 params 里",
                    rule.rule_id,
                    rule.display_name(),
                    name
                ),
                name.clone(),
            );
        }
    }
    let bind_slice: Vec<(&str, &dyn ToSql)> = binds
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();

    // 4. 执行 —— 结果按行归并
    let has_key = stmt.column_count() >= 2;
    let has_code = stmt.column_count() >= 3;
    let mut rows = match stmt.query_map(&bind_slice[..], |row| {
        let row_uid: String = row.get(0)?;
        let key: Option<String> = if has_key { row.get(1)? } else { None };
        let code: Option<String> = if has_code { row.get(2)? } else { None };
        Ok((row_uid, key.unwrap_or_default(), code.unwrap_or_default()))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            return RuleExecution::not_run(
                rule,
                RuleOutcome::Failed,
                ErrorCode::RuleExec,
                format!(
                    "规则 {}（{}）执行失败：{}；其余规则继续",
                    rule.rule_id,
                    rule.display_name(),
                    e
                ),
                rule.rule_id.clone(),
            );
        }
    };

    // 5. 按行归集命中：同一行命中多次只留一条，区分键合并成清单
    let mut order: Vec<String> = Vec::new();
    let mut keys_by_row: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut codes_by_row: BTreeMap<String, Vec<String>> = BTreeMap::new();
    loop {
        match rows.next() {
            None => break,
            Some(Ok((row_uid, key, code))) => {
                let entry = keys_by_row.entry(row_uid.clone()).or_insert_with(|| {
                    order.push(row_uid.clone());
                    Vec::new()
                });
                if !key.is_empty() && !entry.contains(&key) {
                    entry.push(key);
                }
                if !code.is_empty() {
                    let codes = codes_by_row.entry(row_uid.clone()).or_default();
                    if !codes.contains(&code) {
                        codes.push(code);
                    }
                }
            }
            Some(Err(e)) => {
                return RuleExecution::not_run(
                    rule,
                    RuleOutcome::Failed,
                    ErrorCode::RuleExec,
                    format!(
                        "规则 {}（{}）读取结果失败：{}；其余规则继续",
                        rule.rule_id,
                        rule.display_name(),
                        e
                    ),
                    rule.rule_id.clone(),
                );
            }
        }
    }

    for row_uid in order {
        let keys = keys_by_row.get(&row_uid).cloned().unwrap_or_default();
        let codes = codes_by_row.get(&row_uid).cloned().unwrap_or_default();
        execution
            .issues
            .push(build_issue(rule, row_uid, keys, codes));
    }

    execution
}

/// 一行的问题（消息按问题类型给业务人员看，缺失列合并成清单）
fn build_issue(rule: &Rule, row_uid: String, keys: Vec<String>, codes: Vec<String>) -> Issue {
    let message = if keys.is_empty() {
        format!("命中规则 {}，请按依据核对本行", rule.rule_id)
    } else if matches!(
        rule.rule_kind,
        RuleKind::SuspectedDuplicate
            | RuleKind::EnumBound
            | RuleKind::UnitCheck
            | RuleKind::FormulaRecalc
            | RuleKind::DataShapeError
    ) {
        // 疑似重复 / 枚举越界 / 单位不符 / 公式复算 / 数据形态错误：
        // SQL 带出来的第二列就是给人看的完整说明
        // （疑似重复是对照行；枚举越界是「实际填了什么 + 期望取值清单」；
        //   单位不符是「实际填了什么 + 与哪个指标不符 + 期望单位」；
        //   公式复算是「实际填了什么 + 复算得多少 + 期望取值」；
        //   数据形态错误是「哪一格不是数 / 为负」）
        keys.join("、")
    } else if matches!(
        rule.rule_kind,
        RuleKind::RequiredField | RuleKind::ConditionalRequired
    ) {
        format!("缺少{}「{}」", missing_label(rule), keys.join("、"))
    } else {
        format!(
            "命中规则 {}（检查项：{}），请按依据核对本行",
            rule.rule_id,
            keys.join("、")
        )
    };
    // 缺失列清单只对「必填缺失」有意义：其它检查类型的区分键是检查项，不是缺哪一列
    let missing_columns = if matches!(
        rule.rule_kind,
        RuleKind::RequiredField | RuleKind::ConditionalRequired
    ) {
        keys.clone()
    } else {
        Vec::new()
    };
    Issue {
        row_uid,
        rule_id: rule.rule_id.clone(),
        issue_kind: rule.rule_kind.as_issue_kind().to_string(),
        severity: rule.severity.as_str().to_string(),
        message,
        basis: rule.basis.clone(),
        issue_key: keys.join("、"),
        missing_columns,
        // 规则 SQL 第三列带出来的机器可读错误码（没带就留空）
        error_code: codes.join("、"),
    }
}

/// 缺失列的措辞：默认按检查类型给，规则表可用 `missing_label` 参数改写。
///
/// 「必填缺失」与「条件必填缺失」措辞不同 —— 科室看到「缺少条件必填列『分子』」
/// 才知道是比率型指标按官方公式要填，不是全表都得填。
fn missing_label(rule: &Rule) -> &'static str {
    if let Some(RuleParam::Text(label)) = rule.params.get("missing_label") {
        // 参数里的措辞随规则表走，但只在已知措辞里选，避免规则表写错字进结果列
        match label.as_str() {
            "必填列" => "必填列",
            "条件必填列" => "条件必填列",
            _ => default_missing_label(rule.rule_kind),
        }
    } else {
        default_missing_label(rule.rule_kind)
    }
}

/// 默认措辞
fn default_missing_label(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::ConditionalRequired => "条件必填列",
        _ => "必填列",
    }
}

/// 准备阶段的错误分类：缺列 → 该规则不生效；其余 → 执行失败。
///
/// 缺列单独分一类：口径规则引用的列在本机库里不存在，说明**这条规则写错了**
/// （列名是口径的一部分，不该由引擎去猜一个近似的列）。它不生效但必须被看见。
fn classify_prepare_error(
    e: &rusqlite::Error,
    rule: &Rule,
) -> (RuleOutcome, ErrorCode, String, String) {
    let raw = e.to_string();
    if let Some(column) = missing_column_of(&raw) {
        return (
            RuleOutcome::SkippedMissingColumn,
            ErrorCode::MissingColumn,
            format!(
                "规则 {}（{}）引用了本机库里不存在的列「{}」，该规则本轮不生效；不猜列、不静默跳过",
                rule.rule_id,
                rule.display_name(),
                column
            ),
            column,
        );
    }
    let code = if raw.contains("syntax error") {
        ErrorCode::RuleSyntaxError
    } else {
        ErrorCode::RuleExec
    };
    (
        RuleOutcome::Failed,
        code,
        format!(
            "规则 {}（{}）准备失败：{}；其余规则继续",
            rule.rule_id,
            rule.display_name(),
            raw
        ),
        rule.rule_id.clone(),
    )
}

/// 从 SQLite 的报错里取出缺失的列名（`no such column: foo`）
fn missing_column_of(raw: &str) -> Option<String> {
    let marker = "no such column:";
    let idx = raw.find(marker)?;
    let rest = raw[idx + marker.len()..].trim_start();
    let name = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 去掉命名参数的前缀（`:name` / `@name` / `$name` → `name`）
fn strip_param_prefix(name: &str) -> &str {
    name.trim_start_matches([':', '@', '$'])
}

/// 规则参数 → 绑定值（值按声明类型；空值用 NULL）
fn param_to_sql(value: &RuleParam) -> &dyn ToSql {
    match value {
        RuleParam::Text(s) => s,
        RuleParam::Int(i) => i,
        RuleParam::Null => &NULL_TEXT,
    }
}

/// 规则参数 → 命名参数绑定对（名字补前缀，值按声明类型）。
/// 抽出来便于单测直接断言绑定形态。
pub fn named_binds(params: &BTreeMap<String, RuleParam>) -> Vec<(String, &dyn ToSql)> {
    params
        .iter()
        .map(|(name, value)| {
            let key = if name.starts_with(':') {
                name.clone()
            } else {
                format!(":{}", name)
            };
            (key, param_to_sql(value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::model::RuleParam;

    #[test]
    fn 绑定值按声明类型给出() {
        let mut params = BTreeMap::new();
        params.insert("a".to_string(), RuleParam::Text("x".to_string()));
        params.insert("b".to_string(), RuleParam::Int(7));
        params.insert("c".to_string(), RuleParam::Null);
        let binds = named_binds(&params);
        let names: Vec<&str> = binds.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec![":a", ":b", ":c"]);
    }

    #[test]
    fn 缺列名能从报错里取出来() {
        assert_eq!(
            missing_column_of("no such column: no_such_column").as_deref(),
            Some("no_such_column")
        );
        assert!(missing_column_of("no such table: ghost").is_none());
    }

    #[test]
    fn 参数前缀剥得干净() {
        assert_eq!(strip_param_prefix(":batch_ref"), "batch_ref");
        assert_eq!(strip_param_prefix("@scope"), "scope");
        assert_eq!(strip_param_prefix("$scope"), "scope");
        assert_eq!(strip_param_prefix("scope"), "scope");
    }
}
