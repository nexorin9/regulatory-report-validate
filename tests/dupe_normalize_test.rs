//! 重复检测四级漏斗 · 第二级（归一化候选，L2）的验收：
//! ① 全角/半角、中文空格、指标名称别名三种写法差异都生成候选对；
//! ② 候选对的**原值与归一值同时落盘**（可举证）；
//! ③ 未归一的行可计数（指标名称为空 / 单位别名未覆盖），不 panic、不静默丢行；
//! ④ 与确定性键那一级不重复报（字面完全一致的归 L1）；
//! ⑤ 数值不同的对不归本级（归实体绑定层的「待人工判定」）；
//! ⑥ 只标疑似 —— 不合并、不推送、不推进任何状态；
//! ⑦ 端到端：结果契约的 `suspected_duplicates` 段 + 「疑似重复项」列 + 摘要计数。

use std::path::PathBuf;

use regval::dupe::normalize::{self, MATCH_LEVEL_NORMALIZE};
use regval::dupe::{exact, SuspectedDuplicate};
use regval::ingest;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 读样例表并映射成业务行
fn load_rows(name: &str) -> Vec<ingest::SourceRow> {
    let tables = ingest::read_tables(&fixture(name)).expect("样例表应可读");
    let mut rows = Vec::new();
    for table in &tables {
        rows.extend(ingest::map_table(table).expect("表头应完整"));
    }
    rows
}

/// 候选对里两行的原值摘要
fn originals(pair: &SuspectedDuplicate) -> (String, String) {
    (pair.left_original.clone(), pair.right_original.clone())
}

#[test]
fn 全角半角中文空格别名三种写法差异都生成候选对() {
    let rows = load_rows("dupe_normalize.xlsx");
    let out = normalize::detect(&rows);

    // BK-NORM-01（别名写法 ×2 + 官方写法 ×2）与 BK-NORM-02（半角 ×1 + 全角 ×1）
    let normalize_pairs: Vec<&SuspectedDuplicate> = out
        .pairs
        .iter()
        .filter(|p| p.match_level == MATCH_LEVEL_NORMALIZE)
        .collect();
    assert_eq!(
        normalize_pairs.len(),
        3,
        "别名写法 2 对（别名-同表官方写法、别名-跨表官方写法）+ 全角半角 1 对"
    );

    // 别名那一对：归一后指标名称一致
    let alias_pairs: Vec<&&SuspectedDuplicate> = normalize_pairs
        .iter()
        .filter(|p| p.left_normalized.contains("细菌培养检查记录符合率"))
        .collect();
    assert_eq!(alias_pairs.len(), 2, "别名写法与官方写法各成一对");

    // 全角/半角那一对：归一后也是同一个指标名称
    // （归一后的写法保留规范写法的字面形态，所以按大小写不敏感比对）
    let width_pair = normalize_pairs
        .iter()
        .find(|p| {
            p.left_normalized
                .to_lowercase()
                .contains("ct/mri检查记录符合率")
        })
        .expect("全角/半角那一对应存在");
    assert!(width_pair
        .right_normalized
        .to_lowercase()
        .contains("ct/mri检查记录符合率"));
    // 原值里留着全角写法 —— 举证要看得见原文
    let (left, right) = originals(width_pair);
    assert!(
        left.contains("ＣＴ／ＭＲＩ") || right.contains("ＣＴ／ＭＲＩ"),
        "原值应留着全角写法：{:?} {:?}",
        left,
        right
    );
}

#[test]
fn 候选对的原值与归一值同时落盘() {
    let rows = load_rows("dupe_normalize.xlsx");
    let out = normalize::detect(&rows);
    assert!(!out.pairs.is_empty());
    for pair in &out.pairs {
        assert!(!pair.left_original.is_empty(), "左行原值必须落盘");
        assert!(!pair.right_original.is_empty(), "右行原值必须落盘");
        assert!(!pair.left_normalized.is_empty(), "左行归一值必须落盘");
        assert!(!pair.right_normalized.is_empty(), "右行归一值必须落盘");
        assert!(!pair.normalize_detail.is_empty(), "归一详情必须落盘");
        // 原值里有原样的指标名称，归一值里有归一后的写法
        assert!(pair.left_original.contains("指标名称="));
        assert!(pair.left_normalized.contains("指标名称="));
        assert!(pair.normalize_detail.contains("归一为"));
        // 依据、状态、行键齐全
        assert!(!pair.basis.is_empty());
        assert_eq!(pair.state, "疑似重复");
        assert!(!pair.left_row_key.is_empty());
        assert!(!pair.right_row_key.is_empty());
    }
}

#[test]
fn 未归一的行可计数且不丢行() {
    let rows = load_rows("dupe_normalize.xlsx");
    let out = normalize::detect(&rows);

    assert_eq!(
        out.stats.unnormalized_rows, 2,
        "指标名称为空 1 行 + 单位别名未覆盖 1 行"
    );
    assert_eq!(out.stats.unnormalized_name_rows, 1);
    assert_eq!(out.stats.unnormalized_unit_rows, 1);

    // 未归一的行仍在结果里（不静默丢行），且写明原因
    let unnormalized: Vec<&normalize::NormalizedRow> = out
        .rows
        .iter()
        .filter(|r| r.status == normalize::STATUS_UNNORMALIZED)
        .collect();
    assert_eq!(unnormalized.len(), 2);
    assert!(unnormalized
        .iter()
        .any(|r| r.status_note.contains("指标名称为空")));
    assert!(unnormalized
        .iter()
        .any(|r| r.status_note.contains("别名表未覆盖")));
}

#[test]
fn 与确定性键那一级不重复报() {
    let rows = load_rows("dupe_normalize.xlsx");
    let out = normalize::detect(&rows);
    // 科室一表与科室二表那两行逐字一致 → 归确定性键那一级，本级不重复报
    assert_eq!(
        out.stats.exact_overlap_pairs, 1,
        "逐字一致的那一对应只由确定性键那一级报"
    );
    // 确定性键那一级确实抓到了它
    let exact_out = exact::detect(&rows);
    assert_eq!(exact_out.pairs.len(), 1);
    assert_eq!(exact_out.pairs[0].match_level, "exact");
}

#[test]
fn 数值不同的对不归本级() {
    let rows = load_rows("dupe_normalize.xlsx");
    let mut changed = rows.clone();
    // 把其中一行的填报值改掉 —— 同键异值归实体绑定层的「待人工判定」
    for row in changed.iter_mut() {
        if row
            .get_trimmed(ingest::Field::BusinessKey)
            .map(|k| k == "BK-NORM-01")
            .unwrap_or(false)
            && row.source_ns == "科室二表"
        {
            row.values
                .insert(ingest::Field::Numerator, "960".to_string());
            row.values.insert(ingest::Field::Value, "96".to_string());
        }
    }
    let out = normalize::detect(&changed);
    assert_eq!(
        out.stats.value_conflict_pairs, 2,
        "改值那一行与另两行的两对，计数要留痕"
    );
    // 涉及改值那一行的对不再出；两行取值一致的那一对仍在（它本来就是书写差异）
    let bk01: Vec<&SuspectedDuplicate> = out
        .pairs
        .iter()
        .filter(|p| p.business_key.contains("BK-NORM-01"))
        .collect();
    assert_eq!(
        bk01.len(),
        1,
        "只剩两行取值一致的那一对；数值不同的对不归本级"
    );
}

#[test]
fn 只标疑似_不合并不推送() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(fixture("dupe_normalize.xlsx"))
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    // 疑似重复候选对：确定性键 1 对 + 归一化 3 对
    let pairs = contract["suspected_duplicates"].as_array().unwrap();
    let normalize_pairs: Vec<&serde_json::Value> = pairs
        .iter()
        .filter(|p| p["match_level"] == MATCH_LEVEL_NORMALIZE)
        .collect();
    assert_eq!(normalize_pairs.len(), 3);
    for pair in &normalize_pairs {
        assert_eq!(pair["state"], "疑似重复");
        assert!(!pair["basis"].as_str().unwrap().is_empty());
        assert!(!pair["left_original"].as_str().unwrap().is_empty());
        assert!(!pair["right_normalized"].as_str().unwrap().is_empty());
        assert!(!pair["normalize_detail"].as_str().unwrap().is_empty());
    }
    assert_eq!(contract["summary"]["suspected_duplicate_count"], 4);

    // 「疑似重复项」列标出那五行
    let rows = contract["rows"].as_array().unwrap();
    let marked: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|r| !r["suspected_duplicate"].as_str().unwrap_or("").is_empty())
        .collect();
    assert_eq!(marked.len(), 5, "三对共五行要标出疑似对象");
    for r in &marked {
        let text = r["suspected_duplicate"].as_str().unwrap();
        assert!(text.contains("疑似重复"));
        assert!(
            text.contains("归一后") || text.contains("取值完全一致"),
            "标注要写清像在哪：{}",
            text
        );
    }

    // 只标疑似 —— 不合并、不推送：行没少、状态没推进
    assert_eq!(rows.len(), 7, "7 行一行没少，没有被合并掉");
    for r in rows {
        assert_eq!(r["submission_status"], "未上报", "上报状态不由本级推进");
    }

    // 本机库里没有任何上报、回执与确认记录
    let repo = regval::db::repo::Repo::open(&state_db).expect("状态库应可打开");
    for table in ["submissions", "receipts", "confirmations"] {
        let n: i64 = repo
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("统计 {} 应成功：{}", table, e));
        assert_eq!(n, 0, "{} 表应为空 —— 本级只标疑似，不上报", table);
    }
}
