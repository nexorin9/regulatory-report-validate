//! 产物 A 的验收：未上报名单与催报对照。
//!
//! 验收点：
//! - 未上报行数与状态计数一致（未上报 + 已上报 == 本批行数）；
//! - 导出文件列定义完整，且**每行都有具体原因**（错误码 / 缺失列 / 未确认标记）；
//! - 催报对照按报告期给出「已收到 / 未收到」两列对照；
//! - 没有应交科室名册时，未收到一列为空并说明为什么空（不拿本地见过的科室凑名册）；
//! - 已被网关受理的行不在未上报名单里（已上报只由回执驱动）。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use regval::db::repo::{Repo, SubmissionIdentity};
use regval::push::status;

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

/// 跑一次 `regval export pending`，返回 (退出码, stdout, stderr)
fn run_export_pending(
    out: &Path,
    reminder_out: Option<&Path>,
    state_db: &Path,
    rule_dir: Option<&Path>,
) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(["export", "pending"])
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db);
    if let Some(path) = reminder_out {
        cmd.arg("--reminder-out").arg(path);
    }
    match rule_dir {
        Some(dir) => {
            cmd.env("REGVAL_RULE_DIR", dir);
        }
        None => {
            // 不给规则目录 → 名册取不到（也没有科室目录缓存）
            cmd.env_remove("REGVAL_RULE_DIR");
            cmd.env_remove("HIS_BASE_URL");
            cmd.env_remove("HIS_API_TOKEN");
        }
    }
    let output = cmd.output().expect("regval 应可执行");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 校验一次 → 导出未上报名单与催报对照，返回 (契约, 未上报名单 JSON)
fn validate_and_export(dir: &Path, roster_dir: Option<&Path>) -> (Value, Value) {
    let contract_path = dir.join("result.json");
    let pending_path = dir.join("pending.json");
    let reminder_path = dir.join("reminder.json");
    let state_db = dir.join("state.db");

    let (ok, stdout, stderr) = run_validate(&fixture("pending_report.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("已校验 6 行"), "应报出校验行数：{}", stdout);

    let (ok, stdout, stderr) =
        run_export_pending(&pending_path, Some(&reminder_path), &state_db, roster_dir);
    assert!(ok, "export pending 应成功退出，stderr：{}", stderr);
    assert!(
        stdout.contains("未上报名单已写出"),
        "应报出未上报名单已写出：{}",
        stdout
    );
    assert!(
        stdout.contains("催报对照已写出"),
        "应报出催报对照已写出：{}",
        stdout
    );

    let contract: Value =
        serde_json::from_str(&std::fs::read_to_string(&contract_path).unwrap()).unwrap();
    let pending: Value =
        serde_json::from_str(&std::fs::read_to_string(&pending_path).unwrap()).unwrap();
    (contract, pending)
}

/// 建一个只含名册文件的规则目录（名册放哪儿由 `--rule-dir` / REGVAL_RULE_DIR 决定）
fn roster_dir(base: &Path) -> PathBuf {
    let dir = base.join("rule-dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("roster.json"), dir.join("roster.json")).unwrap();
    dir
}

#[test]
fn 未上报行数与状态计数一致() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, pending) = validate_and_export(dir.path(), Some(&roster_dir(dir.path())));

    let counts = &pending["counts"];
    let total = counts["rows_total"].as_u64().expect("rows_total 应为数字");
    let pending_n = counts["rows_pending"].as_u64().expect("rows_pending 应为数字");
    let reported_n = counts["rows_reported"].as_u64().expect("rows_reported 应为数字");

    assert_eq!(total, 6, "本批 6 行：{}", pending["counts"]);
    assert_eq!(
        pending_n + reported_n,
        total,
        "未上报 + 已上报 应等于本批行数：{}",
        pending["counts"]
    );
    // 清单里的行数与计数一致
    assert_eq!(
        pending["rows"].as_array().unwrap().len() as u64,
        pending_n,
        "清单行数应与未上报计数一致"
    );
    // 按待办原因的计数合计 == 未上报行数
    let by_reason: u64 = counts["by_reason"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(by_reason, pending_n, "按原因计数合计应等于未上报行数");
    // 行状态计数覆盖全部行
    let by_state: u64 = counts["by_state"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(by_state, total, "行状态计数应覆盖全部行");
    // 三行取值齐全（待确认），三行有口径问题（有问题待修）
    assert_eq!(counts["by_reason"]["待确认"], 3, "{}", pending["counts"]);
    assert_eq!(counts["by_reason"]["有问题待修"], 3, "{}", pending["counts"]);
}

#[test]
fn 每行都有具体原因不留空话() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, pending) = validate_and_export(dir.path(), Some(&roster_dir(dir.path())));

    for row in pending["rows"].as_array().unwrap() {
        let reason = row["todo_reason"].as_str().unwrap_or("");
        let detail = row["reason_detail"].as_str().unwrap_or("");
        assert!(!reason.trim().is_empty(), "行缺待办原因：{}", row);
        assert!(!detail.trim().is_empty(), "行缺原因明细：{}", row);
        assert!(
            !detail.contains("待处理") && !detail.contains("待办"),
            "行 {} 的原因是无信息量文案：{}",
            row["row_uid"],
            detail
        );
        // 追得到具体原因：错误码 / 缺失列 / 规则编号 / 未确认标记，至少有一项
        let traceable = !row["error_code"].as_str().unwrap_or("").is_empty()
            || !row["missing_columns"].as_str().unwrap_or("").is_empty()
            || !row["rule_ids"].as_str().unwrap_or("").is_empty()
            || detail.contains("规则 ")
            || detail.contains("未确认标记");
        assert!(
            traceable,
            "行 {} 追不到具体原因：{}",
            row["row_uid"],
            detail
        );
    }

    // 有口径问题的行带回缺失列与规则编号
    let fix = pending["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["todo_reason"] == "有问题待修" && !r["missing_columns"].as_str().unwrap_or("").is_empty())
        .expect("应有一行缺必填列");
    assert_eq!(fix["missing_columns"], "指标名称");
    assert_eq!(fix["rule_ids"], "R-REQUIRED-001", "原因编号应点名规则");
    assert!(
        fix["reason_detail"].as_str().unwrap().contains("规则 "),
        "问题明细应点名规则：{}",
        fix["reason_detail"]
    );
}

#[test]
fn 导出文件列定义完整() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let contract_path = dir.path().join("result.json");
    let (ok, _, stderr) = run_validate(&fixture("pending_report.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    let csv_path = dir.path().join("pending.csv");
    let (ok, stdout, stderr) =
        run_export_pending(&csv_path, None, &state_db, Some(&roster_dir(dir.path())));
    assert!(ok, "export pending 应成功退出，stderr：{}", stderr);
    assert!(stdout.contains("未上报名单已写出"));

    let text = std::fs::read_to_string(&csv_path).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines[0],
        "来源表,行号,行键,指标编号,科室代码,科室名称,报告期,业务主键,行状态,待办原因,原因来源,原因编号,原因明细,错误码,缺失列,问题条数,已确认",
        "列定义必须固定：{}",
        lines[0]
    );
    // 表头 + 6 行未上报
    assert_eq!(lines.len(), 7, "表头 + 每行一条：\n{}", text);
    // 每行都有待办原因与原因明细
    for line in &lines[1..] {
        assert!(line.contains("待校验") || line.contains("校验通过") || line.contains("待确认"));
    }
    // 不含患者可识别字段
    for banned in ["姓名", "身份证", "住院号", "病案号", "处方号"] {
        assert!(!text.contains(banned), "导出文件里出现了患者可识别字段名：{}", banned);
    }
}

#[test]
fn 催报对照按报告期给出已收到与未收到() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, pending) = validate_and_export(dir.path(), Some(&roster_dir(dir.path())));

    let periods = pending["reminder"]["periods"].as_array().unwrap();
    assert_eq!(periods.len(), 1, "本批只有一个报告期：{}", pending["reminder"]);
    let period = &periods[0];
    assert_eq!(period["reporting_period"], "2026-Q3");

    let received = period["received"].as_array().unwrap();
    let missing = period["missing"].as_array().unwrap();
    assert_eq!(received.len(), 2, "0301 与 0302 交了表：{}", period);
    assert_eq!(missing.len(), 1, "0303 没交表：{}", period);
    assert_eq!(missing[0]["dept_code"], "0303");
    assert_eq!(missing[0]["receipt_state"], "未收到");
    assert!(missing[0]["note"]
        .as_str()
        .unwrap()
        .contains("未收到该科室上报表"));
    assert_eq!(received[0]["receipt_state"], "已收到");
    // 名册来源写清楚
    assert!(
        pending["reminder"]["roster_source"]
            .as_str()
            .unwrap()
            .contains("名册文件"),
        "名册来源应写明：{}",
        pending["reminder"]
    );
}

#[test]
fn 报告期写法不合法的行不计入催报分组但仍在未上报名单里() {
    let dir = tempfile::tempdir().unwrap();
    let (contract, pending) = validate_and_export(dir.path(), Some(&roster_dir(dir.path())));

    // 样例里有一行报告期写成 2026Q3（枚举越界）—— 它不该自己开一个催报分组
    assert_eq!(
        pending["reminder"]["periods"].as_array().unwrap().len(),
        1,
        "只按合法报告期分组：{}",
        pending["reminder"]
    );
    assert_eq!(
        pending["reminder"]["rows_without_valid_period"], 1,
        "不合法的报告期要计数，不悄悄少掉：{}",
        pending["reminder"]
    );
    // 那一行仍在未上报名单里，且原因指向报告期写法
    let bad = pending["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["reporting_period"] == "2026Q3")
        .expect("报告期写错的那行应在未上报名单里");
    assert_eq!(bad["todo_reason"], "有问题待修");
    assert!(
        bad["reason_detail"].as_str().unwrap().contains("枚举越界"),
        "原因应点名枚举越界：{}",
        bad["reason_detail"]
    );
    // 契约里同一行也是不通过
    let contract_row = contract["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["row_uid"] == bad["row_uid"])
        .unwrap();
    assert_eq!(contract_row["conclusion"], "不通过");
}

#[test]
fn 没有名册时未收到一列为空并说明为什么() {
    let dir = tempfile::tempdir().unwrap();
    let (_contract, pending) = validate_and_export(dir.path(), None);

    let period = &pending["reminder"]["periods"].as_array().unwrap()[0];
    assert_eq!(
        period["missing"].as_array().unwrap().len(),
        0,
        "没名册就不能说谁没交：{}",
        period
    );
    assert_eq!(period["received"].as_array().unwrap().len(), 2);
    assert_eq!(pending["reminder"]["roster_source"], "未取到名册");
    assert!(
        pending["reminder"]["roster_note"]
            .as_str()
            .unwrap()
            .contains("未取到"),
        "要说明为什么空：{}",
        pending["reminder"]
    );
}

#[test]
fn 已被网关受理的行不在未上报名单里() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let contract_path = dir.path().join("result.json");
    let (ok, _, stderr) = run_validate(&fixture("pending_report.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    // 读回契约，取第一行的行键
    let contract: Value =
        serde_json::from_str(&std::fs::read_to_string(&contract_path).unwrap()).unwrap();
    let rows = contract["rows"].as_array().unwrap();
    let row_uid = rows[0]["row_uid"].as_str().unwrap().to_string();
    let batch_ref = contract["batch_ref"].as_str().unwrap().to_string();

    // 记一次「网关已受理」的提交（模拟回执驱动后的状态）
    {
        let mut repo = Repo::open(&state_db).unwrap();
        repo.upsert_submission(
            &SubmissionIdentity {
                batch_ref: batch_ref.clone(),
                row_uid: row_uid.clone(),
                idempotency_key: "a".repeat(64),
                payload_digest: "b".repeat(64),
                revision: 1,
            },
            status::ACCEPTED,
            "",
            "",
            true,
        )
        .unwrap();
    }

    let pending_path = dir.path().join("pending.json");
    let (ok, _, stderr) =
        run_export_pending(&pending_path, None, &state_db, Some(&roster_dir(dir.path())));
    assert!(ok, "export pending 应成功退出，stderr：{}", stderr);
    let pending: Value =
        serde_json::from_str(&std::fs::read_to_string(&pending_path).unwrap()).unwrap();

    assert_eq!(pending["counts"]["rows_reported"], 1, "{}", pending["counts"]);
    assert_eq!(pending["counts"]["rows_pending"], 5, "{}", pending["counts"]);
    let still_listed = pending["rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["row_uid"] == row_uid);
    assert!(
        !still_listed,
        "已上报的行不该再出现在未上报名单里：{}",
        pending["rows"]
    );
}

#[test]
fn 导出到不存在的批次时报错不产出半截文件() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let contract_path = dir.path().join("result.json");
    let (ok, _, stderr) = run_validate(&fixture("pending_report.csv"), &contract_path, &state_db);
    assert!(ok, "validate 应成功退出，stderr：{}", stderr);

    let out = dir.path().join("pending.json");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(["export", "pending"])
        .arg("--out")
        .arg(&out)
        .arg("--state-db")
        .arg(&state_db)
        .arg("--batch")
        .arg("不存在的批次");
    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "不存在的批次应非 0 退出");
    assert!(!out.exists(), "不该产出半截文件");
}
