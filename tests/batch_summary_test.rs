//! 批次汇总事务的验收测试（Task 34）。
//!
//! 验收点：
//! 1. **事务中途失败 → 汇总值等于批前值**（不留半截状态）；
//! 2. **正常批次 → 汇总数值与行状态计数一致**；
//! 3. **批末回滚 → 汇总回到批前值**，而行级回执仍留（网关侧已生效的事实不可撤销）；
//! 4. 修订号只增不减、历史 append-only、同一版汇总写第二次会被挡住（重放防护）；
//! 5. 汇总真的能被 `regval status` 看见（CLI 端到端）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::params;

use regval::db::batch_summary::BatchSummaryRecord;
use regval::db::repo::{Repo, SubmissionIdentity};
use regval::errors::{EngineError, ErrorCode};
use regval::push::batch;
use regval::push::executor::{
    execute, BatchLedger, ExecutionReport, FailureClass, StepOutcome, SubmissionTransport,
};
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput, Program};
use regval::push::receipt::{advance, persist_batch, Verdict};
use regval::rules::model::Issue;

const BATCH: &str = "batch-2026-Q3";

/// 造一个临时状态库
fn open_repo(dir: &Path) -> Repo {
    Repo::open(&dir.join("state.db")).expect("状态库应能打开")
}

/// 造一条提交身份
fn identity(row_uid: &str) -> SubmissionIdentity {
    SubmissionIdentity {
        batch_ref: BATCH.to_string(),
        row_uid: row_uid.to_string(),
        idempotency_key: format!("idem-{}", row_uid),
        payload_digest: format!("digest-{}", row_uid),
        revision: 1,
    }
}

/// 造一行权威候选
fn candidate(row_uid: &str, business_key: &str, value: f64) -> PlanCandidate {
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref: BATCH,
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key,
        numerator: NumericField::Number(value * 10.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(value),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    });
    PlanCandidate {
        row_uid: row_uid.to_string(),
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 1,
        authority_state: "authoritative".to_string(),
        match_key: "MER-TL-04|0301|2026-Q3".to_string(),
        payload,
    }
}

/// 编一个 N 步程序（按行键排序）
fn program_of(values: &[f64]) -> Program {
    let candidates: Vec<PlanCandidate> = values
        .iter()
        .enumerate()
        .map(|(i, v)| candidate(&format!("r{:02}", i + 1), &format!("BK-{:02}", i + 1), *v))
        .collect();
    compile(&PlanInput {
        batch_ref: BATCH.to_string(),
        candidates,
        existing: BTreeMap::new(),
    })
}

/// 假通道：按行键给裁决
struct FakeTransport {
    outcomes: BTreeMap<String, StepOutcome>,
    called: Vec<String>,
}

impl FakeTransport {
    fn new(outcomes: Vec<(&str, StepOutcome)>) -> FakeTransport {
        FakeTransport {
            outcomes: outcomes
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            called: Vec::new(),
        }
    }
}

impl SubmissionTransport for FakeTransport {
    fn submit(&mut self, step: &regval::push::program::Step) -> Result<StepOutcome, EngineError> {
        self.called.push(step.row_uid.clone());
        Ok(self
            .outcomes
            .get(&step.row_uid)
            .cloned()
            .unwrap_or_else(|| accepted(&step.row_uid)))
    }
}

fn accepted(row: &str) -> StepOutcome {
    StepOutcome::Accepted {
        receipt_id: format!("RCPT-{}", row),
        accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
        status: "accepted".to_string(),
    }
}

fn server_error() -> StepOutcome {
    StepOutcome::Failed {
        class: FailureClass::RetryableServer,
        error_code: ErrorCode::GatewayServerError,
        error_field: String::new(),
        message: "上报网关返回服务端错误（HTTP 500）".to_string(),
    }
}

/// 落一条口径问题
fn issue(row_uid: &str, rule_id: &str) -> Issue {
    Issue {
        row_uid: row_uid.to_string(),
        rule_id: rule_id.to_string(),
        issue_kind: "必填缺失".to_string(),
        severity: "阻断".to_string(),
        message: "缺少必填列：指标名称".to_string(),
        basis: "国卫办医函〔2021〕28号 附件".to_string(),
        issue_key: String::new(),
        missing_columns: vec!["指标名称".to_string()],
        error_code: String::new(),
    }
}

/// 把一行的上报状态落库（受理 / 退回 / 待重试），走真实落库路径
fn push_row(repo: &mut Repo, row_uid: &str, verdict: Verdict) {
    let id = identity(row_uid);
    let adv = advance(&id, &verdict);
    persist_batch(repo, &[(id, adv)]).expect("行级状态应能落库");
}

// ————————————————————————————————————————————————————————————
// 验收 1：事务中途失败 → 汇总值等于批前值
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_事务中途失败后汇总值等于批前值且不留半截状态() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    // 批前值：先落一版汇总
    let mut before = BatchSummaryRecord::empty(BATCH);
    before.reported = 2;
    before.rejected = 1;
    before.retryable = 0;
    before.issue_count = 3;
    before.batch_status = batch::BATCH_STATUS_VALIDATED.to_string();
    before.rule_version = "2021.1".to_string();
    let rev = repo.save_batch_summary(&before).expect("批前值应能落库");
    assert_eq!(rev, 1, "第一版汇总的修订号是 1");

    let snapshot_before = repo.batch_summary(BATCH).unwrap().expect("应有快照");
    let history_before = repo.batch_summary_history(BATCH).unwrap().len();

    // 制造「同一版汇总被写第二次」：历史表里先塞一条下一修订号的记录。
    // 落库时修订号会算成 2 → 撞 (batch_ref, revision) 唯一索引 → 整次事务回滚。
    repo.conn()
        .execute(
            "INSERT INTO batch_summary_history (batch_ref, revision, batch_status, planned_steps,
                    attempted, accepted, idempotent_hit, reported, rejected, retryable, blocked,
                    invalid_receipt, not_executed, issue_count, sum_value, sum_numerator,
                    sum_denominator, rule_version, written_at)
             VALUES (?1, 2, '重放', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, '', '2026-11-06T10:00:00+08:00')",
            params![BATCH],
        )
        .expect("注入冲突记录");

    // 再落一版：应当在历史那一步失败，整次事务回滚
    let mut next = before.clone();
    next.reported = 9;
    next.accepted = 9;
    next.batch_status = batch::BATCH_STATUS_COMPLETED.to_string();
    let err = repo.save_batch_summary(&next).expect_err("重放同一版汇总应被挡住");
    assert_eq!(
        err.code,
        ErrorCode::BatchSummaryConflict,
        "重放同一版汇总应有机器可读的错误码"
    );
    assert!(
        err.message.contains("重放") && err.message.contains("回滚"),
        "错误应说清是重放、且已整笔回滚，实际：{}",
        err.message
    );

    // ① 汇总值等于批前值（快照回到批前状态，一项都没改）
    let snapshot_after = repo.batch_summary(BATCH).unwrap().expect("快照应还在");
    assert_eq!(
        snapshot_after.reported, snapshot_before.reported,
        "汇总应回到批前值（已上报行数）"
    );
    assert_eq!(snapshot_after.accepted, snapshot_before.accepted);
    assert_eq!(snapshot_after.rejected, snapshot_before.rejected);
    assert_eq!(snapshot_after.issue_count, snapshot_before.issue_count);
    assert_eq!(
        snapshot_after.batch_status, snapshot_before.batch_status,
        "批次状态也不该被这次失败的写入改掉"
    );
    assert_eq!(snapshot_after.revision, 1, "修订号不该前进");

    // ② 不留半截状态：历史表没有多出新行（第一步的写入被一起回滚了）
    let history_after = repo.batch_summary_history(BATCH).unwrap();
    assert_eq!(
        history_after.len(),
        history_before + 1,
        "只有注入的那条冲突记录，失败的那次没写进来"
    );
    assert!(
        history_after.iter().all(|h| h.reported != 9),
        "失败的事务不该留下半截的汇总行"
    );
}

// ————————————————————————————————————————————————————————————
// 验收 2：正常批次 → 汇总数值与行状态计数一致
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_正常批次的汇总数值与行状态计数一致() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    // 行级状态：2 行受理、1 行退回
    push_row(
        &mut repo,
        "r01",
        Verdict::Accepted {
            receipt_id: "RCPT-r01".to_string(),
            accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
            status: "accepted".to_string(),
        },
    );
    push_row(
        &mut repo,
        "r02",
        Verdict::Accepted {
            receipt_id: "RCPT-r02".to_string(),
            accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
            status: "accepted".to_string(),
        },
    );
    push_row(
        &mut repo,
        "r03",
        Verdict::Rejected {
            error_code: "E_REPORTING_PERIOD".to_string(),
            error_field: "reporting_period".to_string(),
            message: "报告期不合规".to_string(),
        },
    );
    // 两条口径问题
    repo.insert_issues(BATCH, &[issue("r01", "R-REQUIRED-001"), issue("r02", "R-REQUIRED-001")])
        .expect("口径问题应能落库");

    // 跑一个两步全成功的程序
    let program = program_of(&[10.0, 20.0]);
    let mut transport = FakeTransport::new(vec![]);
    let mut ledger = BatchLedger::new(BATCH, program.len());
    let report: ExecutionReport = execute(&program, &mut transport, &mut ledger);
    assert!(report.completed);

    let record = batch::persist_after_run(&mut repo, BATCH, &report, None)
        .expect("批次汇总应能落库");

    // 四项口径
    assert_eq!(record.reported, 2, "已上报 2 行");
    assert_eq!(record.rejected, 1, "被退回 1 行");
    assert_eq!(record.retryable, 0, "待重试 0 行");
    assert_eq!(
        record.issue_count, 3,
        "问题项数 = 2 条口径问题 + 1 条上报退回问题"
    );
    assert_eq!(record.batch_status, batch::BATCH_STATUS_COMPLETED);
    assert_eq!(record.accepted, 2);
    assert_eq!(record.planned_steps, 2);

    // 汇总数值与行状态计数一致
    let states = repo.list_row_push_states(BATCH).unwrap();
    let totals = batch::totals_from_rows(&states, record.issue_count as usize);
    assert_eq!(record.reported as usize, totals.reported);
    assert_eq!(record.rejected as usize, totals.rejected);
    assert_eq!(record.retryable as usize, totals.retryable);
    assert_eq!(
        record.reported as usize + record.rejected as usize + record.retryable as usize,
        states.len(),
        "每个行级状态都归到三项口径里，不多不少"
    );

    // 汇总真的落在库里了（不是只在内存里）
    let stored = repo.batch_summary(BATCH).unwrap().expect("库里应有快照");
    assert_eq!(stored.reported, 2);
    assert_eq!(stored.revision, 1);
}

// ————————————————————————————————————————————————————————————
// 验收 3：批末回滚 → 汇总回批前值，行级回执仍留
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_批末回滚后汇总回到批前值而行级回执仍留() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    // 批前值（这批还没报过任何一行）
    let mut before_record = BatchSummaryRecord::empty(BATCH);
    before_record.batch_status = batch::BATCH_STATUS_VALIDATED.to_string();
    before_record.issue_count = 2;
    repo.save_batch_summary(&before_record).unwrap();
    let before = repo.batch_summary(BATCH).unwrap().expect("应有批前快照");

    // 跑一个第 2 步失败的程序 → 批末条件回滚
    let program = program_of(&[10.0, 20.0, 30.0]);
    let mut transport = FakeTransport::new(vec![("r02", server_error())]);
    let mut ledger = BatchLedger::new(BATCH, program.len());
    let report: ExecutionReport = execute(&program, &mut transport, &mut ledger);
    assert!(report.rollback_applied, "批末条件回滚应已触发");
    assert_eq!(transport.called.len(), 2, "第 3 步不该发请求");

    // 第 1 行的行级回执已经落库（网关侧已受理的事实不可撤销）
    push_row(
        &mut repo,
        "r01",
        Verdict::Accepted {
            receipt_id: "RCPT-r01".to_string(),
            accepted_at: "2026-11-06T10:00:00+08:00".to_string(),
            status: "accepted".to_string(),
        },
    );

    let record = batch::persist_after_run(&mut repo, BATCH, &report, Some(&before))
        .expect("批次汇总应能落库");

    // 汇总回到批前值
    assert_eq!(record.reported, before.reported, "已上报行数应回到批前值");
    assert_eq!(record.rejected, before.rejected);
    assert_eq!(record.retryable, before.retryable);
    assert_eq!(record.issue_count, before.issue_count);
    assert_eq!(record.accepted, before.accepted);
    assert_eq!(
        record.batch_status,
        batch::BATCH_STATUS_ROLLED_BACK,
        "批次状态应记「批末回滚」"
    );

    // 行级回执仍在（回滚的是账，不是事实）
    let receipts = repo.list_receipts(BATCH).unwrap();
    assert_eq!(receipts.len(), 1, "第 1 行的回执应保留");
    assert_eq!(receipts[0].receipt_id, "RCPT-r01");
    let states = repo.list_row_push_states(BATCH).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].status, "已上报");
}

// ————————————————————————————————————————————————————————————
// 验收 4：修订号只增不减 · 历史 append-only · 重放防护
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_修订号只增不减且历史只追加不修改() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    let mut rec = BatchSummaryRecord::empty(BATCH);
    rec.batch_status = batch::BATCH_STATUS_VALIDATED.to_string();
    assert_eq!(repo.save_batch_summary(&rec).unwrap(), 1);
    rec.reported = 3;
    rec.batch_status = batch::BATCH_STATUS_COMPLETED.to_string();
    assert_eq!(repo.save_batch_summary(&rec).unwrap(), 2);
    rec.reported = 5;
    assert_eq!(repo.save_batch_summary(&rec).unwrap(), 3);

    let snapshot = repo.batch_summary(BATCH).unwrap().unwrap();
    assert_eq!(snapshot.revision, 3, "快照是最新那一版");
    assert_eq!(snapshot.reported, 5);

    let history = repo.batch_summary_history(BATCH).unwrap();
    assert_eq!(history.len(), 3, "历史一行一版，只追加不修改");
    assert_eq!(
        history.iter().map(|h| h.revision).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(history[0].reported, 0, "第一版的内容不被后来的写改掉");
    assert_eq!(history[1].reported, 3);
    assert_eq!(history[2].reported, 5);
}

#[test]
fn 验收_空批次号不落库() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let rec = BatchSummaryRecord::empty("   ");
    let err = repo.save_batch_summary(&rec).expect_err("空批次号应被拒");
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
}

// ————————————————————————————————————————————————————————————
// 验收 5：CLI 端到端 —— 汇总能被 regval status 看见
// ————————————————————————————————————————————————————————————

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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
        .output()
        .expect("regval 应可执行")
}

#[test]
fn 验收_校验完批次汇总落在库里且status看得见() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out = dir.path().join("result.json");
    let fixture = project_dir().join("tests/fixtures/mini.csv");

    let validate = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            fixture.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ],
    );
    assert!(
        validate.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&validate.stderr)
    );
    let stdout = String::from_utf8_lossy(&validate.stdout).to_string();
    assert!(
        stdout.contains("批次汇总"),
        "validate 应报出批次汇总，实际输出：{}",
        stdout
    );

    // 库里确实落了汇总
    let repo = Repo::open(&state_db).unwrap();
    let records = repo.list_batch_summaries(10).unwrap();
    assert_eq!(records.len(), 1, "库里应有一份批次汇总");
    assert_eq!(records[0].batch_status, batch::BATCH_STATUS_VALIDATED);
    assert_eq!(records[0].issue_count, 2, "mini.csv 有 2 项口径问题");
    assert_eq!(records[0].reported, 0);

    // regval status 看得到这一份
    let status = run_in(
        dir.path(),
        &["status", "--state-db", state_db.to_str().unwrap()],
    );
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout).to_string();
    assert!(
        text.contains("批次汇总"),
        "status 应显示批次汇总，实际输出：{}",
        text
    );
    assert!(
        text.contains("已上报") && text.contains("被退回") && text.contains("待重试"),
        "status 的批次汇总应给出四项口径，实际输出：{}",
        text
    );
}
