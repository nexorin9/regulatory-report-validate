//! 重新生成交付的规则表文件 `rules/rules-2021.1.rules.json`。
//!
//! # 为什么需要这个
//!
//! 内置兜底口径表（`src/rules/model.rs` 的 `builtin_rules()`）与规则目录里交付的
//! `rules/rules-2021.1.rules.json` **必须逐字一致** —— 否则「同一套口径两处装载」
//! 的结论就会对不上（`tests/rules_loader_test.rs` 里有逐字段比对断言）。
//!
//! 口径改了（新增检查器、改依据、改期望取值）之后，用这个测试重新生成文件：
//!
//! ```bash
//! cargo test --offline --test regen_rules_table -- --ignored
//! ```
//!
//! 生成规则：
//!
//! - 字段名按字典序输出（`basis` / `check_column` / `effective_from` / `indicator_code`
//!   / `params` / `rule_id` / `rule_kind` / `severity` / `sql_lines` / `version`）；
//! - SQL 按行拆成 `sql_lines`（业务人员手工维护口径文件时更顺手）；
//! - 取值一律进 `params`（不拼进 SQL）；
//! - 缩进 2 空格，文件末尾带换行。
//!
//! 这个测试默认**不跑**（标 `#[ignore]`）：它会改写交付文件，只在口径改动后手动执行。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use regval::rules::model::{builtin_rules, RuleParam};

/// 规则表文件路径（相对项目子目录）
fn rule_table_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rules/rules-2021.1.rules.json")
}

/// 规则参数 → JSON 取值
fn param_to_json(param: &RuleParam) -> Value {
    match param {
        RuleParam::Text(s) => Value::String(s.clone()),
        RuleParam::Int(i) => json!(i),
        RuleParam::Null => Value::Null,
    }
}

/// 一条规则 → 规则表里的一个 JSON 对象
fn rule_to_json(rule: &regval::rules::model::Rule) -> Value {
    let mut params = Map::new();
    for (name, param) in &rule.params {
        params.insert(name.clone(), param_to_json(param));
    }

    let mut obj = Map::new();
    obj.insert("basis".to_string(), json!(rule.basis));
    obj.insert("check_column".to_string(), json!(rule.check_column));
    obj.insert("effective_from".to_string(), json!(rule.effective_from));
    obj.insert("indicator_code".to_string(), json!(rule.indicator_code));
    obj.insert("params".to_string(), Value::Object(params));
    obj.insert("rule_id".to_string(), json!(rule.rule_id));
    obj.insert("rule_kind".to_string(), json!(rule.rule_kind.as_token()));
    obj.insert("severity".to_string(), json!(rule.severity.as_token()));
    obj.insert(
        "sql_lines".to_string(),
        Value::Array(
            rule.sql
                .lines()
                .map(|l| Value::String(l.to_string()))
                .collect(),
        ),
    );
    obj.insert("version".to_string(), json!(rule.version));
    Value::Object(obj)
}

#[test]
#[ignore = "会改写交付的规则表文件；口径改动后手动执行"]
fn 重新生成规则表文件() {
    let rules = builtin_rules();
    let version = rules
        .first()
        .map(|r| r.version.clone())
        .unwrap_or_else(|| "2021.1".to_string());

    let table = json!({
        "rule_set": version,
        "rules": rules.iter().map(rule_to_json).collect::<Vec<Value>>(),
    });

    let path = rule_table_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = serde_json::to_string_pretty(&table).unwrap();
    text.push('\n');
    std::fs::write(&path, text).unwrap();

    // 生成后立刻装载回来，确认这份文件能被规则装载器接受
    let loaded = regval::rules::loader::load(path.parent().unwrap()).unwrap();
    assert_eq!(
        loaded.executable().len(),
        rules.len(),
        "生成的规则表装载后条数应与内置口径表一致"
    );
    assert_eq!(
        loaded.manifest().basis_missing,
        0,
        "生成的规则表不该有缺依据的规则"
    );
    println!("已重新生成 {}（{} 条规则）", path.display(), rules.len());
}

#[test]
fn 规则表文件与内置口径表条数一致() {
    // 常规跑：只比对条数与规则编号，具体字段的比对在 rules_loader_test.rs 里
    let loaded = regval::rules::loader::load(rule_table_path().parent().unwrap()).unwrap();
    let builtin = builtin_rules();
    assert_eq!(
        loaded.executable().len(),
        builtin.len(),
        "交付的规则表与内置兜底口径表条数应一致；口径改动后请跑 \
         cargo test --offline --test regen_rules_table -- --ignored 重新生成"
    );
    let loaded_ids: BTreeMap<&str, &str> = loaded
        .executable()
        .iter()
        .map(|r| (r.rule_id.as_str(), r.version.as_str()))
        .collect();
    for rule in builtin.iter() {
        assert!(
            loaded_ids.contains_key(rule.rule_id.as_str()),
            "交付的规则表缺规则 {}",
            rule.rule_id
        );
    }
}
