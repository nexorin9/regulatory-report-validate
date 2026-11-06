//! 人工确认流与上报写闸门的验收测试（Task 36）。
//!
//! 验收点：
//! 1. **未确认行推送被写闸门拒绝** —— 错误码 `UNCONFIRMED_ROW`，
//!    且**一个字节都没往网关发**（断言替身没收到请求）；
//! 2. **确认后推送成功** —— 请求真的发出去了，路径与鉴权头都对；
//! 3. **撤销后再次推送被拒** —— 撤销 → 退回「校验通过」→ 需重新确认；
//! 4. 确认记录含确认人 / 确认时间 / 行键 / 批次；**空确认人不入库**；
//! 5. 撤销记录也入审计链（append-only）；
//! 6. 疑似重复**逐条勾选**才合并，合并动作本身也记确认记录。
//!
//! 对端是 [`gateway_stub`] 里的协议替身（本机可控进程）：产品代码走的是
//! `spec.md`「出站报文（正式对接规范）」里的真实协议，院内真实上报网关
//! **本轮未实连**（地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use gateway_stub::{StubResponse, StubServer};
use regval::db::repo::Repo;
use regval::errors::ErrorCode;
use regval::ingest::{self, Field, SourceRow};
use regval::push::client::{GatewayClient, GatewayConfig};
use regval::push::executor::{execute, BatchLedger};
use regval::push::gate::GatedTransport;
use regval::push::idempotency::{NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{compile, PlanCandidate, PlanInput, Program};
use regval::state::audit;
use regval::state::confirm::{
    self, ConfirmRequest, GateReason, GateVerdict, MergeRequest, RevokeRequest,
};
use regval::state::machine::{self, RowState, StateChange};

const BATCH: &str = "batch-2026-Q3";

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn open_repo(dir: &Path) -> Repo {
    Repo::open(&dir.join("state.db")).expect("状态库应可打开")
}

/// 造一行科室上报表（指标级监管数据，无患者可识别明细）
fn source_row(ns: &str, key: &str, value: &str) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::IndicatorName, "病案首页完整率".to_string());
    values.insert(Field::DeptCode, "0301".to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, key.to_string());
    values.insert(Field::Numerator, "980".to_string());
    values.insert(Field::Denominator, "1000".to_string());
    values.insert(Field::Value, value.to_string());
    values.insert(Field::Unit, "%".to_string());
    SourceRow {
        source_ns: ns.to_string(),
        source_file: format!("{}.csv", ns),
        row_no: 1,
        dup_ordinal: 0,
        values,
    }
}

/// 落库并把这一行推进到「校验通过」（口径校验已过）
///
/// 注意：同批次重跑会先清掉上一轮的行，所以一张表里的多行要**一次落库**。
fn seed_rows(repo: &mut Repo, rows: &[SourceRow]) -> Vec<String> {
    repo.insert_rows(BATCH, rows).expect("落库应成功");
    let mut uids = Vec::new();
    for row in rows {
        let uid = ingest::key::row_uid(row);
        machine::advance(
            repo.conn_mut(),
            &StateChange::new(
                &uid,
                &row.source_file,
                BATCH,
                RowState::PendingValidation,
                "validate",
            ),
        )
        .expect("建底账");
        machine::advance(
            repo.conn_mut(),
            &StateChange::new(
                &uid,
                &row.source_file,
                BATCH,
                RowState::Validated,
                "validate",
            ),
        )
        .expect("推进到校验通过");
        uids.push(uid);
    }
    uids
}

/// 落库一行并推进到「校验通过」
fn seed_validated(repo: &mut Repo, row: &SourceRow) -> String {
    seed_rows(repo, std::slice::from_ref(row)).remove(0)
}

/// 编一个只有一行的上报程序（行键取库里那一行，闸门查得到）
fn program_of(row_uid: &str, business_key: &str) -> Program {
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref: BATCH,
        org_code: "ORG-001",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key,
        numerator: NumericField::Number(980.0),
        denominator: NumericField::Number(1000.0),
        value: NumericField::Number(98.0),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件 指标七",
        rule_version: "2021.1",
    });
    let candidate = PlanCandidate {
        row_uid: row_uid.to_string(),
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 1,
        authority_state: "authoritative".to_string(),
        match_key: "MER-TL-04|0301|2026-Q3".to_string(),
        payload,
    };
    compile(&PlanInput {
        batch_ref: BATCH.to_string(),
        candidates: vec![candidate],
        existing: BTreeMap::new(),
    })
}

/// 给替身配一个客户端
fn client_for(stub: &StubServer) -> GatewayClient {
    GatewayClient::new(GatewayConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .expect("客户端应可建")
}

/// 网关受理的回包
fn accepted_body() -> StubResponse {
    StubResponse::json(
        200,
        r#"{"receipt_id":"RCPT-001","accepted_at":"2026-11-06 09:12:00","status":"accepted"}"#,
    )
}

// ————————————————————————————————————————————————————————————
// 验收三项：未确认被拒 / 确认后成功 / 撤销后又被拒
// ————————————————————————————————————————————————————————————

#[test]
fn 验收_未确认行被写闸门拒绝_确认后推送成功_撤销后又被拒() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    let uid = seed_validated(&mut repo, &row);

    let stub = StubServer::start(vec![accepted_body()]);

    // ① 还没确认 → 闸门拦下，且**一个字节都没往网关发**
    {
        let mut client = client_for(&stub);
        let mut gated = GatedTransport::new(&mut client, repo.conn(), BATCH);
        let program = program_of(&uid, "BK-01");
        let mut ledger = BatchLedger::new(BATCH, program.len());
        let report = execute(&program, &mut gated, &mut ledger);

        let outcome = &report.steps[0].outcome;
        assert!(outcome.is_failure(), "未确认的行不该推上去");
        assert_eq!(outcome.error_code(), Some(ErrorCode::UnconfirmedRow));
        assert!(
            outcome.zh().contains("UNCONFIRMED_ROW"),
            "失败说明里要带上错误码：{}",
            outcome.zh()
        );
        assert_eq!(gated.blocked(), &[uid.clone()], "被拦下的行应记在闸门清单里");
    }
    assert!(
        stub.try_next_request(300).is_none(),
        "没过闸门就不该往网关发请求"
    );

    // ② 人工确认 → 推送成功，请求真的发出去了
    let confirmed = confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科甲"),
    )
    .expect("确认应成功");
    assert_eq!(confirmed.state_after, RowState::PendingConfirmation);
    assert_eq!(confirmed.actor, "信息科甲");

    {
        let mut client = client_for(&stub);
        let mut gated = GatedTransport::new(&mut client, repo.conn(), BATCH);
        let program = program_of(&uid, "BK-01");
        let mut ledger = BatchLedger::new(BATCH, program.len());
        let report = execute(&program, &mut gated, &mut ledger);

        assert!(
            report.steps[0].outcome.is_success(),
            "确认后应能推上去：{}",
            report.steps[0].outcome.zh()
        );
        assert!(gated.blocked().is_empty(), "确认后不该再被拦");
    }
    let req = stub.next_request();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/v1/regulatory-submissions");
    assert_eq!(req.header("authorization"), Some("Bearer stub-token"));
    let body = req.body_json();
    assert_eq!(body["indicator_code"], "MER-TL-04");
    assert_eq!(body["dept_code"], "0301");

    // ③ 撤销确认 → 退回「校验通过」，再推被拒
    let revoked = confirm::revoke(
        repo.conn_mut(),
        &RevokeRequest::new(BATCH, &uid, "信息科甲"),
    )
    .expect("撤销应成功");
    assert_eq!(revoked.state_after, RowState::Validated);

    {
        let mut client = client_for(&stub);
        let mut gated = GatedTransport::new(&mut client, repo.conn(), BATCH);
        let program = program_of(&uid, "BK-01");
        let mut ledger = BatchLedger::new(BATCH, program.len());
        let report = execute(&program, &mut gated, &mut ledger);

        assert!(report.steps[0].outcome.is_failure());
        assert_eq!(report.steps[0].outcome.error_code(), Some(ErrorCode::UnconfirmedRow));
        assert_eq!(gated.blocked(), &[uid.clone()]);
    }
    assert!(
        stub.try_next_request(300).is_none(),
        "撤销后不该再往网关发请求"
    );

    // 重新确认 → 又能推了（撤销不是终态，是退回「等重新确认」）
    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科乙"),
    )
    .expect("重新确认应成功");
    let verdict = confirm::write_gate(repo.conn(), BATCH, &uid).expect("闸门判定应成功");
    assert!(verdict.is_allowed(), "重新确认后闸门应放行：{:?}", verdict);
}

#[test]
fn 写闸门拦下时给的是具体原因不是空话() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    let uid = seed_validated(&mut repo, &row);

    // 从来没确认过
    let verdict = confirm::write_gate(repo.conn(), BATCH, &uid).unwrap();
    match &verdict {
        GateVerdict::Rejected { reason, detail } => {
            assert_eq!(*reason, GateReason::NotConfirmed);
            assert!(detail.contains("还没有人工确认记录"), "{}", detail);
            assert!(detail.contains("校验通过"), "要写清当前行状态：{}", detail);
        }
        other => panic!("未确认的行不该放行：{:?}", other),
    }

    // 确认过又撤销了 → 理由是「被撤销」
    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科甲"),
    )
    .unwrap();
    confirm::revoke(
        repo.conn_mut(),
        &RevokeRequest::new(BATCH, &uid, "信息科甲"),
    )
    .unwrap();
    let verdict = confirm::write_gate(repo.conn(), BATCH, &uid).unwrap();
    match &verdict {
        GateVerdict::Rejected { reason, detail } => {
            assert_eq!(*reason, GateReason::Revoked);
            assert!(detail.contains("撤销"), "{}", detail);
        }
        other => panic!("撤销后不该放行：{:?}", other),
    }

    // 库里没有的行 → 理由是「查无此行」
    let verdict = confirm::write_gate(repo.conn(), BATCH, "row-not-in-db").unwrap();
    match &verdict {
        GateVerdict::Rejected { reason, .. } => assert_eq!(*reason, GateReason::RowUnknown),
        other => panic!("查无此行不该放行：{:?}", other),
    }

    // assert_push_allowed 把判定翻成错误码
    let err = confirm::assert_push_allowed(repo.conn(), BATCH, &uid)
        .expect_err("未确认行应被拒");
    assert_eq!(err.code, ErrorCode::UnconfirmedRow);
    assert!(err.to_string().contains("UNCONFIRMED_ROW"));
}

#[test]
fn 确认记录含确认人确认时间行键批次_且只追加不修改() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    let uid = seed_validated(&mut repo, &row);

    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科甲").with_note("复核通过"),
    )
    .unwrap();

    let records = repo.list_confirmations(Some(BATCH)).unwrap();
    assert_eq!(records.len(), 1);
    let c = &records[0];
    assert_eq!(c.batch_ref, BATCH);
    assert_eq!(c.row_uid, uid);
    assert_eq!(c.action, "confirm");
    assert_eq!(c.confirmed_by, "信息科甲");
    assert!(!c.confirmed_at.is_empty(), "确认时间必填");
    assert_eq!(c.note, "复核通过");
    assert!(c.revoked_at.is_empty());

    // 撤销是**追加**一条，不是改原来那条
    confirm::revoke(
        repo.conn_mut(),
        &RevokeRequest::new(BATCH, &uid, "信息科乙").with_note("数据改过了，重新确认"),
    )
    .unwrap();
    let records = repo.list_confirmations(Some(BATCH)).unwrap();
    assert_eq!(records.len(), 2, "确认与撤销都在库里，都留着");
    assert_eq!(records[0].action, "confirm");
    assert_eq!(records[0].confirmed_by, "信息科甲", "原记录不被改写");
    assert_eq!(records[1].action, "revoke");
    assert_eq!(records[1].confirmed_by, "信息科乙");
    assert_eq!(records[0].revoked_by, "信息科乙", "原记录上标出撤销人");
    assert!(!records[0].revoked_at.is_empty());
}

#[test]
fn 空确认人拒绝写入_库里不多一行() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    let uid = seed_validated(&mut repo, &row);

    for actor in ["", "   ", "\t"] {
        let err = confirm::confirm(
            repo.conn_mut(),
            &ConfirmRequest::new(BATCH, &uid, actor),
        )
        .expect_err("空确认人应被拒");
        assert_eq!(err.code, ErrorCode::ConfirmActorMissing);
        assert!(err.to_string().contains("CONFIRM_ACTOR_MISSING"));
    }
    // 撤销也一样
    let err = confirm::revoke(
        repo.conn_mut(),
        &RevokeRequest::new(BATCH, &uid, " "),
    )
    .expect_err("空撤销人应被拒");
    assert_eq!(err.code, ErrorCode::ConfirmActorMissing);

    let records = repo.list_confirmations(Some(BATCH)).unwrap();
    assert!(records.is_empty(), "空名不入库：{:?}", records);
}

#[test]
fn 撤销记录也入审计链() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    let uid = seed_validated(&mut repo, &row);

    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科甲"),
    )
    .unwrap();
    confirm::revoke(
        repo.conn_mut(),
        &RevokeRequest::new(BATCH, &uid, "信息科甲"),
    )
    .unwrap();

    let events = audit::events_for_row(repo.conn(), &uid).unwrap();
    assert_eq!(events.len(), 2, "确认与撤销都要留痕：{:?}", events);
    assert_eq!(events[0].1, audit::EVENT_CONFIRM);
    assert_eq!(events[1].1, audit::EVENT_REVOKE);
    assert_eq!(events[0].2, "信息科甲");
    assert_eq!(audit::count_for_batch(repo.conn(), BATCH).unwrap(), 2);
}

#[test]
fn 确认粒度是按行_确认一行不影响另一行() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_a = source_row("科室一表", "BK-01", "98");
    let row_b = source_row("科室二表", "BK-02", "97");
    let uids = seed_rows(&mut repo, &[row_a.clone(), row_b.clone()]);
    let uid_a = uids[0].clone();
    let uid_b = uids[1].clone();

    confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid_a, "信息科甲"),
    )
    .unwrap();

    let a = confirm::write_gate(repo.conn(), BATCH, &uid_a).unwrap();
    let b = confirm::write_gate(repo.conn(), BATCH, &uid_b).unwrap();
    assert!(a.is_allowed(), "确认过的行应放行");
    assert!(b.is_rejected(), "没确认的行不该跟着放行");

    // 已确认集合里只有被确认的那一行
    let confirmed = confirm::confirmed_row_uids(repo.conn(), BATCH).unwrap();
    assert_eq!(confirmed.len(), 1);
    assert!(confirmed.contains(&uid_a));
    assert!(!confirmed.contains(&uid_b));
}

#[test]
fn 口径没过的行确认了也推不出去() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row = source_row("科室一表", "BK-01", "98");
    repo.insert_rows(BATCH, std::slice::from_ref(&row)).unwrap();
    let uid = ingest::key::row_uid(&row);
    // 只建底账（待校验），不推进到校验通过
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(&uid, &row.source_file, BATCH, RowState::PendingValidation, "validate"),
    )
    .unwrap();

    // 确认只留痕：疑似重复要逐条勾，行状态不进「待确认」
    let outcome = confirm::confirm(
        repo.conn_mut(),
        &ConfirmRequest::new(BATCH, &uid, "信息科甲").with_note("两行是同一笔记录"),
    )
    .expect("确认应成功");
    assert_eq!(outcome.state_after, RowState::PendingValidation);
    assert_eq!(repo.list_confirmations(Some(BATCH)).unwrap().len(), 1);

    // 但这一行**推不出去** —— 写闸门看的是状态那一关
    let verdict = confirm::write_gate(repo.conn(), BATCH, &uid).unwrap();
    match &verdict {
        GateVerdict::Rejected { reason, detail } => {
            assert_eq!(*reason, GateReason::StateNotPushable);
            assert!(detail.contains("待校验"), "{}", detail);
        }
        other => panic!("没过校验的行不该放行：{:?}", other),
    }
    let err = confirm::assert_push_allowed(repo.conn(), BATCH, &uid)
        .expect_err("没过校验的行应被拒");
    assert_eq!(err.code, ErrorCode::UnconfirmedRow);
}

#[test]
fn 疑似重复逐条勾选才合并_合并动作本身记确认记录() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    // 同一笔记录出现在两张表里（权威键相同）
    let keep_row = source_row("科室一表", "BK-01", "98");
    let drop_row = source_row("科室二表", "BK-01", "99");
    repo.insert_rows(BATCH, &[keep_row.clone(), drop_row.clone()])
        .unwrap();
    let keep_uid = ingest::key::row_uid(&keep_row);
    let drop_uid = ingest::key::row_uid(&drop_row);
    repo.promote_authoritative(BATCH, &keep_uid).unwrap();

    // 没人勾选 → 不合并（引擎不自动合并）
    assert_eq!(
        repo.authority_state(&drop_uid).unwrap().as_deref(),
        Some("raw"),
        "没人勾选就不该合并"
    );

    // 勾选这一对 → 合并
    let outcome = confirm::merge_confirmed_pair(
        repo.conn_mut(),
        &MergeRequest::new(BATCH, &keep_uid, &drop_uid, "信息科甲").with_note("同一笔记录"),
    )
    .expect("合并应成功");
    assert_eq!(outcome.action, "merge");
    assert_eq!(outcome.actor, "信息科甲");
    assert_eq!(
        repo.authority_state(&drop_uid).unwrap().as_deref(),
        Some("duplicate"),
        "并进来的那行应标成重复（同一笔记录只报一次）"
    );
    assert_eq!(
        repo.authority_state(&keep_uid).unwrap().as_deref(),
        Some("authoritative"),
        "保留的那行仍是权威行"
    );

    // 合并动作本身也记一条确认记录
    let records = repo.list_confirmations(Some(BATCH)).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action, "merge");
    assert_eq!(records[0].confirmed_by, "信息科甲");
    assert_eq!(records[0].row_uid, drop_uid);

    // 审计链里有 merge 事件
    let events = audit::events_for_row(repo.conn(), &drop_uid).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, audit::EVENT_MERGE);
}

#[test]
fn 权威键不同的两行不能合并() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_a = source_row("科室一表", "BK-01", "98");
    let row_b = source_row("科室二表", "BK-02", "98");
    repo.insert_rows(BATCH, &[row_a.clone(), row_b.clone()])
        .unwrap();
    let uid_a = ingest::key::row_uid(&row_a);
    let uid_b = ingest::key::row_uid(&row_b);

    let err = confirm::merge_confirmed_pair(
        repo.conn_mut(),
        &MergeRequest::new(BATCH, &uid_a, &uid_b, "信息科甲"),
    )
    .expect_err("权威键不同不该能合并");
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("不是同一笔记录"));
    assert_eq!(
        repo.authority_state(&uid_b).unwrap().as_deref(),
        Some("raw"),
        "合并失败不该改动任何一行"
    );
}

#[test]
fn 确认记录的写语句只在状态确认模块里() {
    // 静态判据：`confirmations` 表的写语句只允许出现在 src/state/confirm.rs
    // （审计链与行状态各有自己的模块，写侧也各归一处）。
    let src_dir = project_dir().join("src");
    let mut files = Vec::new();
    collect_rs(&src_dir, &mut files);
    assert!(!files.is_empty(), "应能扫到源码文件");

    let mut write_hits: Vec<String> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let rel = path
            .strip_prefix(&src_dir)
            .unwrap()
            .to_string_lossy()
            .to_string();
        for needle in [
            "INSERT INTO confirmations",
            "UPDATE confirmations",
            "DELETE FROM confirmations",
        ] {
            if text.contains(needle) {
                write_hits.push(format!("{}：{}", rel, needle));
            }
        }
    }
    let files_hit: Vec<&str> = write_hits
        .iter()
        .map(|hit| hit.split('：').next().unwrap())
        .collect();
    assert!(
        files_hit.iter().all(|f| *f == "state/confirm.rs"),
        "确认记录的写语句只允许出现在 state/confirm.rs，实际命中：{:?}",
        write_hits
    );
    assert!(
        write_hits.len() >= 2,
        "确认模块里至少要有 INSERT 与 UPDATE 两条写语句，实际：{:?}",
        write_hits
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

#[test]
fn 端到端_命令行确认后状态列变成待确认且确认记录可查() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("result.json");
    let fixture = project_dir().join("tests/fixtures/mini.csv");

    // 先跑一次校验
    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "validate",
            "--input",
            fixture.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--state-db",
            db.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .expect("regval 应可执行");
    assert!(
        output.status.success(),
        "校验应成功，stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // 从结果契约里取一行「校验通过」的行键
    let contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let rows = contract["rows"].as_array().expect("契约应有 rows");
    let passing = rows
        .iter()
        .find(|r| r["conclusion"].as_str() == Some("通过"))
        .expect("样例里应有一行通过");
    let row_uid = passing["row_uid"].as_str().unwrap().to_string();

    // 确认人缺失 → 非 0 退出，且明确报 CONFIRM_ACTOR_MISSING
    let bad = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "confirm",
            "--row",
            &row_uid,
            "--by",
            "   ",
            "--state-db",
            db.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .output()
        .expect("regval 应可执行");
    assert!(!bad.status.success(), "空确认人不该成功");
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("CONFIRM_ACTOR_MISSING"),
        "应报 CONFIRM_ACTOR_MISSING：{}",
        stderr
    );

    // 正常确认 → 成功，且能看到行状态推进到「待确认」
    let ok = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "confirm",
            "--row",
            &row_uid,
            "--by",
            "信息科张老师",
            "--note",
            "复核通过",
            "--state-db",
            db.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .output()
        .expect("regval 应可执行");
    assert!(
        ok.status.success(),
        "确认应成功，stderr={}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let stdout = String::from_utf8_lossy(&ok.stdout).to_string();
    assert!(stdout.contains("已记人工确认"), "{}", stdout);
    assert!(stdout.contains("待确认"), "{}", stdout);

    // 确认记录可查
    let list = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["confirmations", "--state-db", db.to_str().unwrap()])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .output()
        .expect("regval 应可执行");
    let stdout = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(stdout.contains("信息科张老师"), "{}", stdout);
    assert!(stdout.contains("复核通过"), "{}", stdout);
    assert!(stdout.contains("已确认"), "{}", stdout);

    // 撤销 → 确认记录显示「已撤销」，行状态退回「校验通过」
    let rev = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "revoke",
            "--row",
            &row_uid,
            "--by",
            "信息科李老师",
            "--note",
            "数据改过了",
            "--state-db",
            db.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .output()
        .expect("regval 应可执行");
    assert!(
        rev.status.success(),
        "撤销应成功，stderr={}",
        String::from_utf8_lossy(&rev.stderr)
    );
    let stdout = String::from_utf8_lossy(&rev.stdout).to_string();
    assert!(stdout.contains("已撤销人工确认"), "{}", stdout);
    assert!(stdout.contains("校验通过"), "{}", stdout);

    let list = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["confirmations", "--state-db", db.to_str().unwrap()])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .output()
        .expect("regval 应可执行");
    let stdout = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(stdout.contains("已撤销"), "{}", stdout);
    assert!(stdout.contains("信息科李老师"), "{}", stdout);
}
