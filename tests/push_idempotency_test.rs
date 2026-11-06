//! 上报身份（幂等键 + 载荷摘要）的验收。
//!
//! 逐条对应任务 27 的验收口径：
//! - 同一业务行两次计算幂等键相同；
//! - 改值后摘要变而幂等键不变；
//! - 载荷字段顺序变化不改摘要；
//! - 空字段与 None 的规范化规则写死并固定；
//! - 订正后修订号 +1 而业务键不变；同键同摘要视为重复提交。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regval::ingest::key;
use regval::ingest::{Field, SourceRow};
use regval::push::idempotency::{
    decide_revision, idempotency_key, idempotency_source, CanonicalContent, CanonicalValue,
    ExistingSubmission, NumericField, PayloadInput, SubmissionPayload,
};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// 跑一次 regval：清掉全部业务环境变量，工作目录指到临时目录。
fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(args)
        .current_dir(cwd)
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .env_remove("OPENAI_TIMEOUT_SECS")
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .env_remove("REGVAL_LOG_LEVEL")
        .output()
        .expect("regval 应可执行")
}

fn input<'a>(batch_ref: &'a str) -> PayloadInput<'a> {
    PayloadInput {
        batch_ref,
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key: "BK-01",
        numerator: NumericField::Number(980.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    }
}

fn source_row() -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::DeptCode, "0301".to_string());
    values.insert(Field::BusinessKey, "BK-01".to_string());
    SourceRow {
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 1,
        dup_ordinal: 0,
        values,
    }
}

#[test]
fn 同一业务行两次计算幂等键相同() {
    let a = idempotency_key("ORG-001", "2026-Q3", "MER-TL-04", "0301", "BK-01");
    let b = idempotency_key("ORG-001", "2026-Q3", "MER-TL-04", "0301", "BK-01");
    assert_eq!(a, b, "同一业务行的幂等键必须两次相同");
    assert_eq!(a.len(), 64, "幂等键是 sha256 的十六进制（64 字符）");

    // 同一行的载荷算两次，幂等键与摘要都相同
    let first = SubmissionPayload::new(input("batch-a"));
    let second = SubmissionPayload::new(input("batch-a"));
    assert_eq!(first.idempotency_key, second.idempotency_key);
    assert_eq!(first.payload_digest, second.payload_digest);
}

#[test]
fn 幂等键与权威键指的是同一条记录() {
    let row = source_row();
    assert_eq!(
        idempotency_source("ORG-001", "2026-Q3", "MER-TL-04", "0301", "BK-01"),
        key::authority_key(&row),
        "幂等键的原文必须与权威键逐字一致（同一段输入，两处不能各写一套）"
    );
}

#[test]
fn 改值后摘要变而幂等键不变() {
    let first = SubmissionPayload::new(input("batch-a"));
    let mut changed = input("batch-a");
    changed.value = NumericField::Number(75.0);
    let second = SubmissionPayload::new(changed);
    assert_eq!(
        first.idempotency_key, second.idempotency_key,
        "改的是取值，不是记录本身 —— 幂等键不该变"
    );
    assert_ne!(
        first.payload_digest, second.payload_digest,
        "取值变了，载荷摘要必须变"
    );
    // 业务键也没变
    assert_eq!(first.business_key, second.business_key);
}

#[test]
fn 载荷字段顺序变化不改摘要() {
    let a = CanonicalContent::from_pairs(vec![
        ("reporting_period".into(), CanonicalValue::Text("2026-Q3".into())),
        ("org_code".into(), CanonicalValue::Text("ORG-001".into())),
        ("indicator_code".into(), CanonicalValue::Text("MER-TL-04".into())),
        ("dept_code".into(), CanonicalValue::Text("0301".into())),
        ("business_key".into(), CanonicalValue::Text("BK-01".into())),
        ("numerator".into(), CanonicalValue::Number("980".into())),
        ("denominator".into(), CanonicalValue::Number("1000".into())),
        ("value".into(), CanonicalValue::Number("98".into())),
        ("unit".into(), CanonicalValue::Text("%".into())),
        ("basis".into(), CanonicalValue::Text("依据".into())),
        ("rule_version".into(), CanonicalValue::Text("2021.1".into())),
    ]);
    let b = CanonicalContent::from_pairs(vec![
        ("rule_version".into(), CanonicalValue::Text("2021.1".into())),
        ("value".into(), CanonicalValue::Number("98".into())),
        ("unit".into(), CanonicalValue::Text("%".into())),
        ("basis".into(), CanonicalValue::Text("依据".into())),
        ("denominator".into(), CanonicalValue::Number("1000".into())),
        ("numerator".into(), CanonicalValue::Number("980".into())),
        ("business_key".into(), CanonicalValue::Text("BK-01".into())),
        ("dept_code".into(), CanonicalValue::Text("0301".into())),
        ("indicator_code".into(), CanonicalValue::Text("MER-TL-04".into())),
        ("org_code".into(), CanonicalValue::Text("ORG-001".into())),
        ("reporting_period".into(), CanonicalValue::Text("2026-Q3".into())),
    ]);
    assert_eq!(a.canonical_json(), b.canonical_json());
    assert_eq!(a.digest(), b.digest());
}

#[test]
fn 空字段与填坏了的规范化规则固定() {
    // 空：空串、纯空格、全角空格都算「没填」
    for raw in ["", "   ", "\u{3000}", "\t"] {
        let f = NumericField::from_raw(raw);
        assert_eq!(f, NumericField::Empty, "「{}」应判为空", raw);
        assert_eq!(f.as_number(), None);
        assert!(!f.is_number());
    }
    // 填坏了：保留原文，不推断成 0，也不静默丢掉
    let broken = NumericField::from_raw("abc");
    assert_eq!(broken, NumericField::Raw("abc".to_string()));
    assert_eq!(broken.as_number(), None, "填坏了不能当成 0");
    assert_eq!(broken.raw_text(), "abc");
    // 合法数值：写法不同也归成同一个值
    assert_eq!(NumericField::from_raw("96"), NumericField::Number(96.0));
    assert_eq!(NumericField::from_raw("96.0"), NumericField::Number(96.0));
    assert_eq!(NumericField::from_raw("96.00"), NumericField::Number(96.0));
    assert_eq!(NumericField::from_raw("９６"), NumericField::Number(96.0));
    // 非有限值（inf / NaN）不算合法数值
    assert!(matches!(
        NumericField::from_raw("inf"),
        NumericField::Raw(_)
    ));
    assert!(matches!(
        NumericField::from_raw("NaN"),
        NumericField::Raw(_)
    ));
}

#[test]
fn 订正后修订号加一而业务键不变() {
    let first = SubmissionPayload::new(input("batch-a"));
    let existing = vec![ExistingSubmission {
        revision: 1,
        payload_digest: first.payload_digest.clone(),
        status: regval::push::status::ACCEPTED.to_string(),
    }];

    // 载荷没变 → 重复提交：不产生新的推送请求
    let same = decide_revision(&existing, &first.payload_digest);
    assert!(same.is_duplicate());
    assert_eq!(same.revision(), 1);

    // 载荷变了 → 订正：业务键不变，修订号 +1
    let mut changed = input("batch-a");
    changed.value = NumericField::Number(75.0);
    let revised = SubmissionPayload::new(changed);
    assert_eq!(
        revised.idempotency_key, first.idempotency_key,
        "订正改的是取值 —— 幂等键（哪条记录）与业务键都不该变"
    );
    assert_eq!(revised.business_key, first.business_key);
    let decision = decide_revision(&existing, &revised.payload_digest);
    assert!(!decision.is_duplicate());
    assert_eq!(decision.revision(), 2);
}

#[test]
fn 摘要不含批次号与修订号() {
    let a = SubmissionPayload::new(input("batch-a"));
    let b = SubmissionPayload::new(input("batch-b")).with_revision(7);
    assert_eq!(
        a.payload_digest, b.payload_digest,
        "批次号是运行上下文、修订号由摘要推出，都不该进摘要"
    );
    assert_eq!(a.idempotency_key, b.idempotency_key);
}

#[test]
fn 载荷能按自身字段重算出同一个摘要() {
    let payload = SubmissionPayload::new(input("batch-a"));
    assert_eq!(
        payload.recomputed_digest(),
        payload.payload_digest,
        "载荷被改过（摘要与内容对不上）时能看出来"
    );
}

#[test]
fn 上报身份预览只算不发且不改状态() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            fixture("mini.csv").to_str().unwrap(),
            "--out",
            dir.path().join("contract.json").to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "validate 应成功，stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let preview = run_in(
        dir.path(),
        &["push", "--preview", "--state-db", state_db.to_str().unwrap()],
    );
    assert!(
        preview.status.success(),
        "push --preview 应成功，stderr: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let text = stdout_of(&preview);
    assert!(
        text.contains("上报身份预览"),
        "预览应说明这是身份预览，实际输出：{}",
        text
    );
    assert!(
        text.contains("幂等键") && text.contains("载荷摘要"),
        "预览里应逐行给出幂等键与载荷摘要，实际输出：{}",
        text
    );
    assert!(
        text.contains("MER-TL-04") || text.contains("MER-HR-01"),
        "预览应逐行给出指标编号，实际输出：{}",
        text
    );

    // 预览不写状态：库里没有任何上报提交与回执
    let conn = rusqlite::Connection::open(&state_db).unwrap();
    let submissions: i64 = conn
        .query_row("SELECT COUNT(*) FROM submissions", [], |r| r.get(0))
        .unwrap();
    let receipts: i64 = conn
        .query_row("SELECT COUNT(*) FROM receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(submissions, 0, "预览不产生上报提交");
    assert_eq!(receipts, 0, "预览不产生回执");
}
