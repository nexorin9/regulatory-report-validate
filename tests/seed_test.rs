//! 官方 27 项指标口径种子的验收用例。
//!
//! # 这组用例守住什么
//!
//! 口径种子（`rules/indicators-2021.1.seed.json`）是官方附件
//! 《病案管理质量控制指标（2021年版）》（国卫办医函〔2021〕28号 附件）的**结构化提取物**。
//! 它要同时守住四条：
//!
//! 1. **提全了**：27 项指标一条不少，依据（`basis`）逐项指到官方条款；
//! 2. **没提错**：种子的编号 / 名称 / 类别 / 期望单位与内置兜底登记表
//!    （[`regval::rules::indicators`]）逐字一致 —— 种子是同一份口径的文件形态，
//!    不是另一套口径；
//! 3. **不猜**：官方附件没写明的地方（人均负担型的计量单位、填报值精度、
//!    缺依据的种子）标「口径待补」，不替它编一个值；
//! 4. **编译得对**：种子编译出的四条规则与内置口径表逐字一致，
//!    装载规则目录时口径以种子为准，且「口径待补」在规则清单里看得见。

use std::path::{Path, PathBuf};

use serde_json::Value;

use regval::rules::indicators;
use regval::rules::loader;
use regval::rules::model::{builtin_rules, Rule, RuleKind};
use regval::rules::seed::{SeedCategory, SeedSet};

/// 项目子目录
fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 交付的口径种子文件
fn seed_path() -> PathBuf {
    project_dir().join("rules/indicators-2021.1.seed.json")
}

/// 交付的规则表文件
fn rule_table_path() -> PathBuf {
    project_dir().join("rules/rules-2021.1.rules.json")
}

/// 装载交付的种子
fn load_delivered_seed() -> SeedSet {
    SeedSet::load(&seed_path()).expect("交付的口径种子应能装载")
}

#[test]
fn 交付的口径种子是二十七项且依据齐备() {
    let seed = load_delivered_seed();

    assert_eq!(
        seed.indicators.len(),
        27,
        "官方附件是 27 项指标（指标一至指标二十七），一条都不能少"
    );
    assert_eq!(seed.seed_set, "2021.1", "种子集版本标签应与规则集一致");
    assert!(
        seed.source_document.contains("国卫办医函〔2021〕28号"),
        "种子要写明官方文件出处：{}",
        seed.source_document
    );

    // 逐条：依据必填（指到官方条款），编号与名称非空
    for item in &seed.indicators {
        assert!(
            !item.basis.trim().is_empty(),
            "指标 {} 缺官方依据 —— 没有出处的口径问题没法向科室解释",
            item.indicator_code
        );
        assert!(
            item.basis.contains(&item.indicator_code),
            "指标 {} 的依据应点名本项编号：{}",
            item.indicator_code,
            item.basis
        );
        assert!(
            !item.name.trim().is_empty(),
            "指标 {} 缺中文名",
            item.indicator_code
        );
        assert!(
            !item.formula.trim().is_empty(),
            "指标 {} 缺计算公式",
            item.indicator_code
        );
    }

    // 编号不重复
    let mut codes: Vec<&str> = seed
        .indicators
        .iter()
        .map(|i| i.indicator_code.as_str())
        .collect();
    codes.sort();
    let before = codes.len();
    codes.dedup();
    assert_eq!(codes.len(), before, "指标编号重复了");
}

#[test]
fn 编号前缀覆盖官方附件的五类指标() {
    let seed = load_delivered_seed();
    let prefixes: Vec<String> = seed
        .indicators
        .iter()
        .map(|i| {
            i.indicator_code
                .split('-')
                .take(2)
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect();

    // 验收点名要覆盖的两类
    for want in ["MER-HR", "MER-TQ"] {
        assert!(
            prefixes.iter().any(|p| p == want),
            "官方附件里应有 {} 开头的指标",
            want
        );
    }
    // 官方附件的五类分类都要在
    for want in ["MER-HR", "MER-TL", "MER-ME", "MER-D&T", "MER-TQ"] {
        assert!(
            prefixes.iter().any(|p| p == want),
            "官方附件里应有 {} 开头的指标",
            want
        );
    }
}

#[test]
fn 种子与内置兜底登记表逐字一致() {
    // 种子是官方附件的文件形态，内置表是同一份口径的代码形态 —— 两边不能漂移
    let seed = load_delivered_seed();
    assert_eq!(
        seed.indicators.len(),
        indicators::INDICATORS.len(),
        "种子条数与内置登记表应一致"
    );

    for (item, spec) in seed.indicators.iter().zip(indicators::INDICATORS.iter()) {
        assert_eq!(item.indicator_code, spec.code, "指标编号顺序不一致");
        assert_eq!(
            item.name, spec.name,
            "{} 的中文名与内置登记表不一致",
            spec.code
        );
        assert_eq!(
            item.unit.as_deref(),
            spec.unit,
            "{} 的期望单位与内置登记表不一致",
            spec.code
        );
        let want_category = match spec.kind {
            indicators::IndicatorKind::Ratio => SeedCategory::Ratio,
            indicators::IndicatorKind::Burden => SeedCategory::Burden,
        };
        assert_eq!(
            item.category, want_category,
            "{} 的指标类别与内置登记表不一致",
            spec.code
        );
    }
}

#[test]
fn 单位未写明的三项标口径待补且不判单位() {
    // 官方附件没逐项写计量单位。人均负担型三项（MER-HR-01…03）公式不含百分号，
    // 官方也没写单位 —— 标「口径待补」，不猜一个，期望单位表里也不该有它们。
    let seed = load_delivered_seed();
    let manifest = seed.manifest(0);

    for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
        let item = seed.lookup(code).expect("人力资源指标应登记");
        assert!(item.unit.is_none(), "{} 的单位官方未写明，不推断", code);
        assert!(item.is_pending(), "{} 应标口径待补", code);
        assert!(
            item.pending_reasons()
                .iter()
                .any(|r| r.contains("计量单位")),
            "{} 的待补原因应点名计量单位",
            code
        );
    }

    // 期望单位表只有 24 项比率型指标（单位 %）
    let expected = seed.expected_units_json();
    let parsed: Vec<Value> = serde_json::from_str(&expected).unwrap();
    assert_eq!(parsed.len(), 24, "期望单位表应只收写明了单位的 24 项");
    for code in ["MER-HR-01", "MER-HR-02", "MER-HR-03"] {
        assert!(
            !expected.contains(code),
            "{} 单位未写明，不该出现在期望单位表里",
            code
        );
    }

    // 口径待补在清单里看得见
    assert_eq!(manifest.pending.len(), 3, "应有 3 项口径待补");
    assert_eq!(manifest.burden_count, 3);
    assert_eq!(manifest.ratio_count, 24);
    assert_eq!(manifest.basis_missing, 0, "交付的种子依据齐备");
    // 官方未写明填报值精度 —— 27 项统一按默认精度比对，如实计数
    assert_eq!(manifest.precision_defaulted, 27);
    assert!(!seed.precision_note.is_empty(), "精度口径要写清处理方式");
}

#[test]
fn 缺依据的种子标口径待补而不是猜一个() {
    // 造一份缺依据的种子：既不编依据，也不猜它的单位，更不进编译参数
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.seed.json");
    std::fs::write(
        &path,
        r#"{
          "seed_set": "2021.1",
          "source_document": "国卫办医函〔2021〕28号 附件",
          "indicators": [
            {
              "indicator_code": "MER-XX-01",
              "name": "自造指标",
              "category": "ratio",
              "unit": "%",
              "precision": null,
              "basis": ""
            },
            {
              "indicator_code": "MER-TL-01",
              "name": "入院记录24小时内完成率",
              "category": "ratio",
              "unit": "%",
              "precision": null,
              "basis": "国卫办医函〔2021〕28号 附件 指标四（MER-TL-01）"
            }
          ]
        }"#,
    )
    .unwrap();

    let seed = SeedSet::load(&path).expect("缺依据的种子文件应能装载，但被标待补");
    let broken = seed.lookup("MER-XX-01").expect("自造指标应登记");
    assert!(!broken.has_basis(), "依据为空");
    assert!(broken.is_pending(), "缺依据的种子应标口径待补");
    assert!(
        broken.pending_reasons().iter().any(|r| r.contains("依据")),
        "待补原因应点名依据缺失：{:?}",
        broken.pending_reasons()
    );

    // 不猜：缺依据的指标不进在册编号清单，也不进期望单位表
    assert_eq!(seed.all_codes(), "MER-TL-01", "缺依据的指标不进在册清单");
    assert!(
        !seed.expected_units_json().contains("MER-XX-01"),
        "缺依据的指标不进期望单位表"
    );

    let manifest = seed.manifest(0);
    assert_eq!(manifest.basis_missing, 1);
    assert_eq!(manifest.pending.len(), 1);
    assert_eq!(manifest.pending[0].indicator_code, "MER-XX-01");
}

#[test]
fn 种子编译出的规则与内置口径表逐字一致() {
    // 种子编译出的四条规则（条件必填 / 指标编号在册 / 单位匹配 / 公式复算）
    // 必须与内置兜底口径表逐字一致 —— 种子是同一份口径的文件形态。
    let seed = load_delivered_seed();
    let compiled = seed.compile_rules("2021.1");
    assert_eq!(compiled.len(), 4, "种子编译出四类检查器的规则定义");

    let builtin = builtin_rules();
    for rule in &compiled {
        let expect = builtin
            .iter()
            .find(|b| b.rule_id == rule.rule_id)
            .unwrap_or_else(|| panic!("内置口径表里应有规则 {}", rule.rule_id));
        assert_eq!(
            rule.rule_kind, expect.rule_kind,
            "{} 检查类型不一致",
            rule.rule_id
        );
        assert_eq!(
            rule.indicator_code, expect.indicator_code,
            "{} 适用指标不一致",
            rule.rule_id
        );
        assert_eq!(
            rule.check_column, expect.check_column,
            "{} 检查项不一致",
            rule.rule_id
        );
        assert_eq!(
            rule.sql.trim(),
            expect.sql.trim(),
            "{} SQL 不一致",
            rule.rule_id
        );
        assert_eq!(rule.params, expect.params, "{} 参数不一致", rule.rule_id);
        assert_eq!(rule.basis, expect.basis, "{} 依据不一致", rule.rule_id);
        assert_eq!(rule.version, expect.version, "{} 版本不一致", rule.rule_id);
        assert_eq!(
            rule.severity, expect.severity,
            "{} 严重度不一致",
            rule.rule_id
        );
    }

    // 四条规则分别对应四类检查器
    let kinds: Vec<RuleKind> = compiled.iter().map(|r| r.rule_kind).collect();
    assert!(kinds.contains(&RuleKind::ConditionalRequired), "条件必填");
    assert!(kinds.contains(&RuleKind::EnumBound), "枚举越界");
    assert!(kinds.contains(&RuleKind::UnitCheck), "单位不符");
    assert!(kinds.contains(&RuleKind::FormulaRecalc), "公式不符");
}

#[test]
fn 种子编译出的参数与内置登记表的取值一致() {
    let seed = load_delivered_seed();
    assert_eq!(seed.all_codes(), indicators::all_indicator_list());
    assert_eq!(seed.ratio_codes(), indicators::ratio_indicator_list());
    assert_eq!(
        seed.compact_codes_display(),
        indicators::compact_indicator_list(),
        "在册编号的紧凑写法应逐字一致"
    );
    assert_eq!(
        seed.expected_units_json(),
        indicators::expected_units_json(),
        "期望单位表应逐字一致"
    );
    assert_eq!(
        seed.precision_table_json(),
        indicators::precision_table_json(),
        "精度表应逐字一致"
    );
}

#[test]
fn 装载规则目录时口径以种子为准且待补可见() {
    let dir = project_dir().join("rules");
    let set = loader::load(&dir).expect("交付的规则目录应能装载");

    // 种子被装载了，清单里看得见
    assert_eq!(set.manifest().seed_files.len(), 1, "应装载 1 份口径种子");
    assert_eq!(set.manifest().seed_sets.len(), 1);
    let seed_manifest = &set.manifest().seed_sets[0];
    assert_eq!(seed_manifest.total, 27);
    assert_eq!(seed_manifest.pending.len(), 3, "3 项口径待补应在清单里");
    assert_eq!(seed_manifest.ratio_count, 24);
    assert_eq!(seed_manifest.burden_count, 3);

    // 种子编译出的四条规则替换了规则表里的同名规则（口径以官方种子为准）
    assert_eq!(
        seed_manifest.replaced_rules.len(),
        4,
        "四条种子规则应替换规则表里的同名规则"
    );
    for rule_id in &seed_manifest.replaced_rules {
        assert!(
            [
                "R-REQUIRED-002",
                "R-ENUM-004",
                "R-UNIT-001",
                "R-FORMULA-001"
            ]
            .contains(&rule_id.as_str()),
            "不该被种子替换的规则：{}",
            rule_id
        );
    }

    // 装载后的执行集合与内置兜底口径表逐字一致（口径不因「从哪装载」而变）
    let builtin = builtin_rules();
    assert_eq!(set.executable().len(), builtin.len());
    for (loaded, expect) in set.executable().iter().zip(builtin.iter()) {
        assert_eq!(loaded.rule_id, expect.rule_id);
        assert_eq!(loaded.sql.trim(), expect.sql.trim());
        assert_eq!(loaded.params, expect.params);
        assert_eq!(loaded.basis, expect.basis);
    }

    // 种子来源的规则在清单里能看出出处
    let seed_rules: Vec<&Rule> = set
        .all()
        .iter()
        .filter(|r| r.source_file.ends_with(".seed.json"))
        .collect();
    assert_eq!(seed_rules.len(), 4, "四条规则的来源应是口径种子文件");
}

#[test]
fn 规则清单产物里能看到口径待补() {
    // regval rules --out 写出的清单里，口径待补明细必须看得见（不是只给一个计数）
    let out_dir = tempfile::tempdir().unwrap();
    let out = out_dir.path().join("manifest.json");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["rules", "--dir"])
        .arg(project_dir().join("rules"))
        .arg("--out")
        .arg(&out)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "regval rules 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("口径种子"),
        "终端应报出口径种子情况：{}",
        stdout
    );
    assert!(
        stdout.contains("口径待补"),
        "终端应报出口径待补：{}",
        stdout
    );
    assert!(
        stdout.contains("MER-HR-01"),
        "待补明细应点名到具体指标：{}",
        stdout
    );

    let raw = std::fs::read_to_string(&out).unwrap();
    let report: Value = serde_json::from_str(&raw).expect("规则清单 JSON 应可解析");
    let seed_sets = report["manifest"]["seed_sets"].as_array().unwrap();
    assert_eq!(seed_sets.len(), 1);
    assert_eq!(seed_sets[0]["total"], 27);
    let pending = seed_sets[0]["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 3);
    assert_eq!(pending[0]["indicator_code"], "MER-HR-01");
    assert!(
        pending[0]["reasons"][0]
            .as_str()
            .unwrap()
            .contains("计量单位"),
        "待补原因应写明缺什么"
    );
}

#[test]
fn 种子文件字段写错不静默忽略() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("typo.seed.json");
    std::fs::write(
        &path,
        r#"{
          "seed_set": "2021.1",
          "indicators": [
            {
              "indicator_code": "MER-TL-01",
              "name": "入院记录24小时内完成率",
              "category": "ratio",
              "unit": "%",
              "basis": "依据",
              "indicatr_code": "写错了"
            }
          ]
        }"#,
    )
    .unwrap();

    let err = SeedSet::load(&path).expect_err("字段名写错应报错");
    assert!(
        err.message.contains("indicatr_code"),
        "报错要点出错写的字段名：{}",
        err.message
    );
}

#[test]
fn 种子文件缺版本标签报错() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noversion.seed.json");
    std::fs::write(
        &path,
        r#"{
          "indicators": [
            {
              "indicator_code": "MER-TL-01",
              "name": "入院记录24小时内完成率",
              "category": "ratio",
              "unit": "%",
              "basis": "依据"
            }
          ]
        }"#,
    )
    .unwrap();

    let err = SeedSet::load(&path).expect_err("缺版本标签应报错");
    assert!(
        err.message.contains("seed_set"),
        "报错要点出缺版本标签：{}",
        err.message
    );
}

#[test]
fn 种子与规则表并用时结论一致且口径指纹不变() {
    // 端到端：同一份样例，三条装载路径
    //   A = 只放规则表的目录
    //   B = 规则表 + 口径种子的目录
    //   C = 内置兜底口径表（不设 REGVAL_RULE_DIR）
    // 结论必须逐条相同；且**加种子不改变口径指纹** —— 种子是同一份口径的文件形态，
    // 不是另一套口径。
    let work = tempfile::tempdir().unwrap();
    let dir_a = work.path().join("rules-a");
    let dir_b = work.path().join("rules-b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::copy(rule_table_path(), dir_a.join("rules-2021.1.rules.json")).unwrap();
    std::fs::copy(rule_table_path(), dir_b.join("rules-2021.1.rules.json")).unwrap();
    std::fs::copy(seed_path(), dir_b.join("indicators-2021.1.seed.json")).unwrap();

    let input = project_dir().join("tests/fixtures/mini.csv");

    let run = |rule_dir: Option<&Path>, out: &Path, db: &Path| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_regval"));
        cmd.args(["validate", "--input"])
            .arg(&input)
            .arg("--out")
            .arg(out);
        cmd.env("REGVAL_STATE_DB", db);
        match rule_dir {
            Some(dir) => {
                cmd.env("REGVAL_RULE_DIR", dir);
            }
            None => {
                cmd.env_remove("REGVAL_RULE_DIR");
            }
        }
        let output = cmd.output().expect("regval 应可执行");
        assert!(
            output.status.success(),
            "validate 应成功：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let out_a = work.path().join("a.json");
    let out_b = work.path().join("b.json");
    let out_c = work.path().join("c.json");
    run(Some(&dir_a), &out_a, &work.path().join("db-a.db"));
    run(Some(&dir_b), &out_b, &work.path().join("db-b.db"));
    run(None, &out_c, &work.path().join("db-c.db"));

    let read =
        |p: &Path| -> Value { serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap() };
    let a = read(&out_a);
    let b = read(&out_b);
    let c = read(&out_c);

    assert_eq!(a["issues"], b["issues"], "加种子不改变问题清单");
    assert_eq!(a["rows"], b["rows"], "加种子不改变行级结论");
    assert_eq!(
        c["issues"], b["issues"],
        "内置兜底口径表与种子装载的结论应一致"
    );
    assert_eq!(
        c["rows"], b["rows"],
        "内置兜底口径表与种子装载的行级结论应一致"
    );

    assert_eq!(
        a["rule_status"]["manifest"]["set_fingerprint"],
        b["rule_status"]["manifest"]["set_fingerprint"],
        "加种子不改变口径指纹（种子是同一份口径的文件形态）"
    );
    assert_eq!(
        b["rule_status"]["manifest"]["set_version"], a["rule_status"]["manifest"]["set_version"],
        "口径版本不变"
    );

    // 装载了种子 ⇒ 规则清单里带得出口径种子与口径待补
    assert_eq!(
        b["rule_status"]["manifest"]["seed_sets"][0]["total"], 27,
        "契约的规则清单段应带得出口径种子"
    );
    assert_eq!(
        b["rule_status"]["manifest"]["seed_sets"][0]["pending"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "3 项口径待补应在契约里看得见"
    );
}
