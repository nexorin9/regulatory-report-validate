//! 产物 B 的验收：回执导出与举证包导出。
//!
//! 验收点（Task 44）：
//! - **导出文件行数与行状态计数一致**（一次提交一条；导出行数 == 库里的提交记录数；
//!   导出的行状态分布 == 状态机里同一批行的行状态分布）；
//! - **每条问题都带 basis 字段**（举证包：命中的行每条命中都有官方依据出处；
//!   没命中任何规则的行也要在包里，标「无命中」）；
//! - 导出内容**可被第三方按幂等键与回执 ID 交叉核对**（字段名固定、顺序固定；
//!   `(幂等键, 修订号) ↔ 回执号` 一一对应）；
//! - 交叉核对不通过时**如实报出**，不静默归档。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use regval::db::repo::{Repo, SubmissionIdentity};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::receipt::{advance, persist, Verdict};
use regval::push::status;
use regval::state::machine;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 跑一次 `regval validate`，返回 (退出码, stdout, stderr)
fn run_validate(input: &Path, out: &Path, state_db: &Path) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["validate"])
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .env("REGVAL_RULE_DIR", project_dir().join("rules"))
        .output()
        .expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 跑一次 `regval export <what>`，返回 (退出码, stdout, stderr)
fn run_export(what: &str, out: &Path, state_db: &Path, batch: Option<&str>) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(["export", what])
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db);
    if let Some(b) = batch {
        cmd.arg("--batch").arg(b);
    }
    let output = cmd.output().expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 契约里的一行（按业务主键找）
fn row_of<'a>(contract: &'a Value, business_key: &str) -> &'a Value {
    contract["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["business_key"] == business_key)
        .unwrap_or_else(|| panic!("契约里应有业务主键 {} 的行", business_key))
}

/// 为某一行造上报身份（幂等键与载荷摘要都按引擎的算法算，不手编）
fn identity_of(
    row: &Value,
    batch_ref: &str,
    revision: i64,
) -> SubmissionIdentity {
    let indicator = row["indicator_code"].as_str().unwrap_or("").to_string();
    let dept = row["dept_code"].as_str().unwrap_or("").to_string();
    let period = row["reporting_period"].as_str().unwrap_or("").to_string();
    let business_key = row["business_key"].as_str().unwrap_or("").to_string();
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref,
        org_code: "",
        reporting_period: &period,
        indicator_code: &indicator,
        dept_code: &dept,
        business_key: &business_key,
        numerator: NumericField::Number(98.0),
        denominator: NumericField::Number(100.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    })
    .with_revision(revision);
    SubmissionIdentity {
        batch_ref: batch_ref.to_string(),
        row_uid: row["row_uid"].as_str().unwrap().to_string(),
        idempotency_key: payload.idempotency_key.clone(),
        payload_digest: payload.payload_digest.clone(),
        revision,
    }
}

/// 造一个已经校验过、可以上报的批次
///
/// 场景（每一行都走**真实的回执推进流程**，不是手写库表）：
/// - BK-SAMPLE-01：受理（回执 RCPT-001）
/// - BK-SAMPLE-02：受理（回执 RCPT-002）
/// - BK-SAMPLE-03：退回（4xx，没有回执号，留错误码与字段名）
/// - BK-SAMPLE-05：待重试（5xx，没有回执号）
/// - BK-SAMPLE-01 再报一次：**订正重报**（同一幂等键、修订号 2、另一个回执号）
fn prepared_batch(dir: &Path) -> (Value, PathBuf) {
    let state_db = dir.join("state.db");
    let contract_path = dir.join("result.json");
    let (ok, stdout, stderr) = run_validate(&fixture("mini.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("已校验 6 行"), "应报出校验行数：{}", stdout);

    let contract: Value =
        serde_json::from_str(&std::fs::read_to_string(&contract_path).unwrap()).unwrap();
    let batch_ref = contract["batch_ref"].as_str().unwrap().to_string();

    let accepted_1 = identity_of(row_of(&contract, "BK-SAMPLE-01"), &batch_ref, 1);
    let accepted_2 = identity_of(row_of(&contract, "BK-SAMPLE-02"), &batch_ref, 1);
    let rejected = identity_of(row_of(&contract, "BK-SAMPLE-03"), &batch_ref, 1);
    let retryable = identity_of(row_of(&contract, "BK-SAMPLE-05"), &batch_ref, 1);
    // 订正重报：同一业务行，修订号 2
    let revised = identity_of(row_of(&contract, "BK-SAMPLE-01"), &batch_ref, 2);

    {
        let mut repo = Repo::open(&state_db).unwrap();

        let adv = advance(
            &accepted_1,
            &Verdict::Accepted {
                receipt_id: "RCPT-001".to_string(),
                accepted_at: "2026-11-06 09:12:00".to_string(),
                status: "accepted".to_string(),
            },
        );
        persist(&mut repo, &accepted_1, &adv).unwrap();

        let adv = advance(
            &accepted_2,
            &Verdict::Accepted {
                receipt_id: "RCPT-002".to_string(),
                accepted_at: "2026-11-06 09:12:05".to_string(),
                status: "accepted".to_string(),
            },
        );
        persist(&mut repo, &accepted_2, &adv).unwrap();

        let adv = advance(
            &rejected,
            &Verdict::Rejected {
                error_code: "E_REPORTING_PERIOD".to_string(),
                error_field: "reporting_period".to_string(),
                message: "报告期格式不合法".to_string(),
            },
        );
        persist(&mut repo, &rejected, &adv).unwrap();

        let adv = advance(
            &retryable,
            &Verdict::Retryable {
                error_code: regval::errors::ErrorCode::GatewayServerError,
                message: "网关 500".to_string(),
            },
        );
        persist(&mut repo, &retryable, &adv).unwrap();

        // 订正重报：同一幂等键，修订号 2，网关给了另一个回执号
        let adv = advance(
            &revised,
            &Verdict::Accepted {
                receipt_id: "RCPT-003".to_string(),
                accepted_at: "2026-11-06 10:20:00".to_string(),
                status: "accepted".to_string(),
            },
        );
        persist(&mut repo, &revised, &adv).unwrap();
    }

    (contract, state_db)
}

#[test]
fn 回执台账一次提交一条且可按幂等键与回执号交叉核对() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, state_db) = prepared_batch(dir.path());

    let out = dir.path().join("receipts.json");
    let (ok, stdout, stderr) = run_export("receipts", &out, &state_db, None);
    assert!(ok, "export receipts 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("回执台账已写出"), "{}", stdout);
    assert!(stdout.contains("交叉核对"), "应报出交叉核对结论：{}", stdout);

    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(ledger["schema_version"], "regval.receipts/1.0.0");
    assert_eq!(ledger["batch_ref"], contract["batch_ref"]);

    // 5 次提交：2 行受理 + 1 行退回 + 1 行待重试 + 1 次订正重报
    assert_eq!(ledger["counts"]["entries"], 5, "{}", ledger["counts"]);
    // 涉及 4 行
    assert_eq!(ledger["counts"]["rows"], 4, "{}", ledger["counts"]);
    // 3 个回执（退回与待重试没有回执）
    assert_eq!(ledger["counts"]["receipts"], 3, "{}", ledger["counts"]);
    // 订正重报 1 行
    assert_eq!(ledger["counts"]["revised_rows"], 1, "{}", ledger["counts"]);
    // 按上报状态计数
    assert_eq!(ledger["counts"]["by_status"][status::ACCEPTED], 3);
    assert_eq!(ledger["counts"]["by_status"][status::REJECTED], 1);
    assert_eq!(ledger["counts"]["by_status"][status::RETRYABLE], 1);

    // 交叉核对：一一对应成立，三处计数都是 0
    let cc = &ledger["cross_check"];
    assert_eq!(cc["receipt_pairs_one_to_one"], true, "{}", cc);
    assert_eq!(cc["accepted_without_receipt"], 0, "{}", cc);
    assert_eq!(cc["receipt_without_submission"], 0, "{}", cc);
    assert_eq!(cc["state_disagreements"], 0, "{}", cc);
    assert_eq!(cc["conflicts"].as_array().unwrap().len(), 0, "{}", cc);
    assert_eq!(cc["identity_pairs"], 5);
    assert_eq!(cc["distinct_receipt_ids"], 3);

    // 逐条看：批次 / 回执 ID / 幂等键 / 状态 / 时间 / 行键 六样都要有
    let rows = ledger["rows"].as_array().unwrap();
    let row1 = rows
        .iter()
        .find(|r| r["row_uid"] == row_of(&contract, "BK-SAMPLE-01")["row_uid"])
        .unwrap();
    assert_eq!(row1["batch_ref"], contract["batch_ref"]);
    assert_eq!(row1["receipt_id"], "RCPT-001");
    assert_eq!(row1["push_status"], status::ACCEPTED);
    assert_eq!(row1["accepted_at"], "2026-11-06 09:12:00");
    assert!(!row1["idempotency_key"].as_str().unwrap().is_empty());

    // 订正重报的两条：幂等键相同、修订号不同、回执号不同
    let revised_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["row_uid"] == row_of(&contract, "BK-SAMPLE-01")["row_uid"])
        .collect();
    assert_eq!(revised_rows.len(), 2, "订正重报应出两条：{:?}", rows);
    assert_eq!(revised_rows[0]["idempotency_key"], revised_rows[1]["idempotency_key"]);
    assert_ne!(revised_rows[0]["revision"], revised_rows[1]["revision"]);
    assert_ne!(revised_rows[0]["receipt_id"], revised_rows[1]["receipt_id"]);

    // 被退回的那条：没有回执号，但有错误码与字段名
    let rejected = rows
        .iter()
        .find(|r| r["row_uid"] == row_of(&contract, "BK-SAMPLE-03")["row_uid"])
        .unwrap();
    assert_eq!(rejected["receipt_id"], "");
    assert_eq!(rejected["push_status"], status::REJECTED);
    assert_eq!(rejected["error_code"], "GATEWAY_REJECTED");
    assert_eq!(rejected["error_field"], "reporting_period");
    assert!(
        rejected["message"].as_str().unwrap().contains("E_REPORTING_PERIOD"),
        "退回说明应带网关原话：{}",
        rejected["message"]
    );
}

#[test]
fn 导出行数与行状态计数一致() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, state_db) = prepared_batch(dir.path());

    let out = dir.path().join("receipts.json");
    let (ok, _, stderr) = run_export("receipts", &out, &state_db, None);
    assert!(ok, "export receipts 应成功退出，stderr：{}", stderr);
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    let repo = Repo::open(&state_db).unwrap();
    let batch_ref = contract["batch_ref"].as_str().unwrap();

    // ① 导出行数 == 库里的提交记录数
    let submissions = repo.list_submissions(batch_ref).unwrap();
    assert_eq!(
        ledger["counts"]["entries"].as_u64().unwrap(),
        submissions.len() as u64,
        "导出行数应与提交记录数一致"
    );
    assert_eq!(
        ledger["rows"].as_array().unwrap().len(),
        submissions.len(),
        "导出的条目数应与提交记录数一致"
    );

    // ② 与库里「最新修订」那份视图逐行对得上（SQL JOIN 视角 vs 导出视角）
    let push_states = repo.list_row_push_states(batch_ref).unwrap();
    for st in &push_states {
        // 库里那份视图取的是**最新修订**那一次 → 按（行键, 修订号）定位同一条
        let row = ledger["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["row_uid"] == st.row_uid && r["revision"] == st.revision)
            .unwrap_or_else(|| panic!("导出台账里应有行 {} 修订 {}", st.row_uid, st.revision));
        assert_eq!(row["push_status"], st.status, "行 {} 的上报状态应一致", st.row_uid);
        assert_eq!(row["receipt_id"], st.receipt_id, "行 {} 的回执号应一致", st.row_uid);
        assert_eq!(row["idempotency_key"], st.idempotency_key);
        assert_eq!(row["revision"], st.revision);
    }

    // ③ 行状态计数与状态机里同一批行的行状态分布一致
    let mut expected: BTreeMap<String, u64> = BTreeMap::new();
    for (row_uid, state) in machine::list_state_values(repo.conn(), batch_ref).unwrap() {
        if ledger["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["row_uid"] == row_uid)
        {
            *expected.entry(state).or_insert(0) += 1;
        }
    }
    let actual = ledger["counts"]["by_row_state"].as_object().unwrap();
    let actual_sum: u64 = actual.values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(
        actual_sum,
        expected.values().sum::<u64>(),
        "行状态计数应覆盖导出的每一行：{:?} vs {:?}",
        actual,
        expected
    );
    for (state, n) in &expected {
        assert_eq!(
            actual.get(state).and_then(|v| v.as_u64()),
            Some(*n),
            "状态 {} 的行数应一致：{:?} vs {:?}",
            state,
            actual,
            expected
        );
    }
    // 四行都有行状态记录（都推过，行状态机推得动）
    assert_eq!(
        ledger["counts"]["entries_without_row_state"], 0,
        "推过的行都该有行状态记录：{}",
        ledger["counts"]
    );
}

#[test]
fn csv列定义固定且不含患者可识别字段() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, state_db) = prepared_batch(dir.path());

    let out = dir.path().join("receipts.csv");
    let (ok, stdout, stderr) = run_export("receipts", &out, &state_db, None);
    assert!(ok, "export receipts 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("回执台账已写出"));

    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines[0],
        "批次,行键,幂等键,修订号,载荷摘要,上报状态,回执号,网关状态,受理时间,错误码,错误字段,网关说明,尝试次数,提交时间,更新时间,行状态,口径版本",
        "列定义必须固定：{}",
        lines[0]
    );
    // 表头 + 5 条（一次提交一条）
    assert_eq!(lines.len(), 6, "表头 + 每条提交一行：\n{}", text);
    // 关键字段在行里
    assert!(text.contains("RCPT-001"), "回执号应出现在台账里：\n{}", text);
    assert!(text.contains("RCPT-003"), "订正重报的回执号也在：\n{}", text);
    // 不含患者可识别字段
    for banned in ["姓名", "身份证", "住院号", "病案号", "处方号"] {
        assert!(!text.contains(banned), "导出文件里出现了患者可识别字段名：{}", banned);
    }
}

#[test]
fn 举证包每条问题都带依据且无命中的行也在包里() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, state_db) = prepared_batch(dir.path());

    let out = dir.path().join("evidence.json");
    let (ok, stdout, stderr) = run_export("evidence", &out, &state_db, None);
    assert!(ok, "export evidence 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("依据举证包已写出"), "{}", stdout);

    let pkg: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();

    // 举证包行数 == 本批行数（没命中任何规则的行也在包里）
    assert_eq!(
        pkg["counts"]["rows"].as_u64().unwrap(),
        contract["rows"].as_array().unwrap().len() as u64,
        "举证包行数应与本批行数一致：{}",
        pkg["counts"]
    );
    assert_eq!(
        pkg["rows"].as_array().unwrap().len(),
        contract["rows"].as_array().unwrap().len()
    );

    // 每条命中都带官方依据出处
    let mut hits = 0usize;
    let mut rows_without_hits = 0usize;
    for row in pkg["rows"].as_array().unwrap() {
        if row["match_state"] == "无命中" {
            rows_without_hits += 1;
            assert_eq!(row["hit_count"], 0, "无命中的行命中条数应为 0：{}", row);
            continue;
        }
        for hit in row["hits"].as_array().unwrap() {
            hits += 1;
            let basis = hit["basis"].as_str().unwrap_or("");
            assert!(!basis.trim().is_empty(), "问题缺依据：{}", hit);
            assert!(
                !hit["suggestion"].as_str().unwrap_or("").is_empty(),
                "问题缺建议动作：{}",
                hit
            );
        }
    }
    assert_eq!(hits, pkg["counts"]["hits"].as_u64().unwrap() as usize);
    assert_eq!(
        rows_without_hits,
        pkg["counts"]["rows_without_hits"].as_u64().unwrap() as usize
    );
    assert!(hits > 0, "本批有口径问题，举证包应带命中");

    // CSV 版同样逐条带依据
    let csv_path = dir.path().join("evidence.csv");
    let (ok, _, stderr) = run_export("evidence", &csv_path, &state_db, None);
    assert!(ok, "export evidence 应成功退出，stderr：{}", stderr);
    let text = std::fs::read_to_string(&csv_path).unwrap();
    assert!(text.contains("国卫办医函"), "CSV 的依据列应带官方依据原文：\n{}", text);
    // 无命中的行也出一行（命中状态 = 无命中）
    assert!(text.contains("无命中"), "无命中的行也要在举证包里：\n{}", text);
}

#[test]
fn 交叉核对不通过时如实报出并拒绝静默归档() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, state_db) = prepared_batch(dir.path());

    // 人为制造一处对不上：把已上报那一行的回执记录删掉（模拟状态库被别处改过）
    {
        let mut repo = Repo::open(&state_db).unwrap();
        repo.conn_mut()
            .execute("DELETE FROM receipts WHERE receipt_id = 'RCPT-002'", [])
            .unwrap();
    }

    let out = dir.path().join("receipts.json");
    let (ok, stdout, stderr) = run_export("receipts", &out, &state_db, None);
    assert!(!ok, "交叉核对不通过应非 0 退出：{}", stdout);
    assert!(
        stderr.contains("RECEIPT_LEDGER_MISMATCH"),
        "应报出错误码：{}",
        stderr
    );
    assert!(
        stdout.contains("已上报但没有回执号"),
        "应逐条列出冲突：{}",
        stdout
    );
    // 台账仍然写出来 —— 写出来是为了让人照着查
    assert!(out.exists(), "台账应写出来供核对");
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(ledger["cross_check"]["accepted_without_receipt"], 1);
    assert_eq!(ledger["cross_check"]["receipt_pairs_one_to_one"], false);
    assert_eq!(
        ledger["cross_check"]["conflicts"].as_array().unwrap().len(),
        1,
        "冲突应逐条列出：{}",
        ledger["cross_check"]
    );
    let _ = contract;
}

#[test]
fn 导出不存在的批次时报错不产出半截文件() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, state_db) = prepared_batch(dir.path());

    let out = dir.path().join("receipts.json");
    let (ok, _, stderr) = run_export("receipts", &out, &state_db, Some("不存在的批次"));
    assert!(!ok, "不存在的批次应非 0 退出");
    assert!(!out.exists(), "不该产出半截文件");
    assert!(
        stderr.contains("EMPTY_TABLE") || stderr.contains("没有上报提交记录"),
        "应说明为什么导不出来：{}",
        stderr
    );
}
