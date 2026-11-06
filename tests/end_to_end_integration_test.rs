//! 端到端全流程集成测试（Task 50）。
//!
//! 验收点：
//!
//! 1. **一条龙跑通**：校验 → 人工确认 → 上报（对协议替身）→ 回执写回 → 导出
//!    （未上报名单 / 回执台账 / 依据举证包）；
//! 2. **进程重启续跑**：第 k 步失败后**重启进程**（重新跑一次 `regval push`），
//!    从最后一个已确认成功的步骤继续，**已成功的行不重复推送**；
//! 3. **幂等重跑**：同一批次连续跑两次，第二次**不产生新的推送请求**；
//! 4. **状态库损坏恢复**：损坏后按恢复流程找回「报过没有」这条事实。
//!
//! # 端到端跑的是真命令
//!
//! 本文件里的每一步都是 `regval` 这个**可执行文件**（`CARGO_BIN_EXE_regval`），
//! 不是把内部函数调一遍就算数 —— 命令行参数、环境变量读取、退出码、
//! 写回本机库的行为都在验收范围内。
//!
//! # 对端是协议替身，不是最终对接方式
//!
//! 对端是 [`gateway_stub`] 里的协议兼容替身（本机可控进程）：产品代码走的是
//! `spec.md`「出站报文（正式对接规范）」里的真实协议（路径 / 鉴权头 / 幂等头 /
//! 15 项报文字段全集），替身只换了对端地址与响应内容。
//! 院内真实上报网关**本轮未实连**（地址与凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gateway_stub::reporting::{ReceivedRequest, ReportingGatewayStub, Scenario};
use serde_json::Value;

/// 确认人（每一次人工确认都要有人负责）
const ACTOR: &str = "数据管理员张老师";
/// 替身期待的 Bearer 凭据
const TOKEN: &str = "stub-token";

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// 跑一次 `regval`：先把全部业务环境变量清掉，再按需注入。
///
/// 清环境变量是为了让测试**不受开发机 `.env` 影响** —— 否则本机配过网关地址，
/// 测试就会打到真实网关上去了。
fn run_regval(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.current_dir(dir)
        .args(args)
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("HIS_TIMEOUT_SECS")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .env_remove("OPENAI_TIMEOUT_SECS")
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .env_remove("REGULATORY_REPORT_TIMEOUT_SECS")
        .env_remove("REGVAL_PUSH_RETRY_MAX_ATTEMPTS")
        .env_remove("REGVAL_PUSH_RETRY_BASE_MS")
        .env_remove("REGVAL_PUSH_RETRY_MAX_DELAY_MS")
        .env_remove("REGVAL_LOG_LEVEL");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("regval 应可执行")
}

/// 口径规则目录（用交付的那一套规则表与 27 项指标种子，不用内置兜底表）
fn rule_dir() -> String {
    project_dir().join("rules").display().to_string()
}

/// 把替身收到的请求全部取走（取完为止）。
fn drain(stub: &ReportingGatewayStub) -> Vec<ReceivedRequest> {
    let mut out = Vec::new();
    while let Some(req) = stub.try_next_request(300) {
        out.push(req);
    }
    out
}

/// 请求里的幂等键
fn idem_of(req: &ReceivedRequest) -> String {
    req.body_json()
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// CSV 数据行数（不含表头）
fn csv_data_rows(path: &Path) -> usize {
    let text = std::fs::read_to_string(path).expect("导出产物应存在");
    text.lines().filter(|l| !l.trim().is_empty()).count() - 1
}

/// 一条龙跑通之后的现场：状态库路径、契约路径、行键表
struct Ready {
    dir: tempfile::TempDir,
    state_db: PathBuf,
    /// 业务主键 → 行键
    rows: Vec<(String, String)>,
}

impl Ready {
    fn db(&self) -> &str {
        self.state_db.to_str().unwrap()
    }
}

/// 把样例表放进临时目录并跑一次 `validate`（校验 → 落本机库 → 产出契约）
fn prepare(dir: tempfile::TempDir) -> Ready {
    let input = dir.path().join("report.csv");
    std::fs::copy(fixture("e2e_report.csv"), &input).expect("复制样例表");
    let state_db = dir.path().join("state.db");
    let contract = dir.path().join("contract.json");

    let rules = rule_dir();
    let out = run_regval(
        dir.path(),
        &[
            "validate",
            "--input",
            input.to_str().unwrap(),
            "--out",
            contract.to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ],
        &[("REGVAL_RULE_DIR", &rules)],
    );
    assert!(
        out.status.success(),
        "校验应成功：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains("已校验 3 行"),
        "校验摘要应说得清行数：{}",
        stdout_of(&out)
    );

    let text = std::fs::read_to_string(&contract).expect("契约应已写出");
    let value: Value = serde_json::from_str(&text).expect("契约应是合法 JSON");
    assert_eq!(value["schema_version"], "regval.contract/1.0.0");
    let rows: Vec<(String, String)> = value["rows"]
        .as_array()
        .expect("契约应有 rows 数组")
        .iter()
        .map(|r| {
            (
                r["business_key"].as_str().unwrap_or("").to_string(),
                r["row_uid"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(rows.len(), 3, "样例表 3 行");
    assert_eq!(value["issues"].as_array().unwrap().len(), 0, "样例表不应有口径问题");

    Ready {
        dir,
        state_db,
        rows,
    }
}

/// 逐行人工确认（真实走 `regval confirm`）
fn confirm_all(ready: &Ready) {
    let rules = rule_dir();
    for (_, row_uid) in &ready.rows {
        let out = run_regval(
            ready.dir.path(),
            &[
                "confirm",
                "--row",
                row_uid,
                "--by",
                ACTOR,
                "--note",
                "复核通过",
                "--state-db",
                ready.db(),
            ],
            &[("REGVAL_RULE_DIR", &rules)],
        );
        assert!(
            out.status.success(),
            "确认应成功：stdout={} stderr={}",
            stdout_of(&out),
            stderr_of(&out)
        );
        assert!(
            stdout_of(&out).contains("已记人工确认"),
            "确认应有回执：{}",
            stdout_of(&out)
        );
    }
}

/// 跑一次上报（真实走 `regval push` 的发送路径）
fn push(ready: &Ready, stub: &ReportingGatewayStub, extra_env: &[(&str, &str)]) -> Output {
    let rules = rule_dir();
    let mut env: Vec<(&str, &str)> = vec![
        ("REGVAL_RULE_DIR", &rules),
        ("REGULATORY_REPORT_BASE_URL", &stub.base_url),
        ("REGULATORY_REPORT_TOKEN", TOKEN),
    ];
    env.extend_from_slice(extra_env);
    run_regval(
        ready.dir.path(),
        &["push", "--state-db", ready.db()],
        &env,
    )
}

// ————————————————————————————————————————————————————————————
// 验收一：一条龙跑通（校验 → 确认 → 上报 → 回执 → 导出）
// ————————————————————————————————————————————————————————————

#[test]
fn 全流程跑通_回执写回且导出文件行数与行状态一致() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let ready = prepare(tempfile::tempdir().unwrap());
    confirm_all(&ready);

    let out = push(&ready, &stub, &[]);
    assert!(
        out.status.success(),
        "上报应成功：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    assert!(text.contains("已上报 3 行"), "上报摘要应说得清行数：{}", text);
    assert!(
        text.contains("回执落库：行 3 · 回执 3"),
        "回执应逐行落库：{}",
        text
    );

    // 替身确实收到了 3 个请求，报文形状合规
    let requests = drain(&stub);
    assert_eq!(requests.len(), 3, "三行应各发一次请求");
    for req in &requests {
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/v1/regulatory-submissions");
        assert_eq!(
            req.header("authorization"),
            Some(format!("Bearer {}", TOKEN).as_str()),
            "鉴权头应带上凭据"
        );
        assert!(req.header("idempotency-key").is_some(), "幂等头不能少");
        assert!(
            req.body_json().get("idempotency_key").is_some(),
            "报文里要带幂等键"
        );
        assert!(req.conforms(), "报文应符合 spec 的字段全集");
    }
    assert_eq!(stub.receipts_created(), 3, "网关应为三行各建一张回执");

    // 幂等键 ↔ 回执 ID 一一对应
    let out = run_regval(
        ready.dir.path(),
        &["audit", "--idempotency", "--state-db", ready.db()],
        &[("REGVAL_RULE_DIR", &rule_dir())],
    );
    assert!(out.status.success());
    let text = stdout_of(&out);
    assert!(
        text.contains("一一对应成立"),
        "幂等键与回执号应一一对应：{}",
        text
    );

    // 导出三样产物
    let receipts = ready.dir.path().join("receipts.csv");
    let pending = ready.dir.path().join("pending.csv");
    let evidence = ready.dir.path().join("evidence.csv");
    for (what, path) in [
        ("receipts", &receipts),
        ("pending", &pending),
        ("evidence", &evidence),
    ] {
        let out = run_regval(
            ready.dir.path(),
            &[
                "export",
                what,
                "--out",
                path.to_str().unwrap(),
                "--state-db",
                ready.db(),
            ],
            &[("REGVAL_RULE_DIR", &rule_dir())],
        );
        assert!(
            out.status.success(),
            "导出 {} 应成功：{}",
            what,
            stderr_of(&out)
        );
        assert!(path.exists(), "导出 {} 应产出文件", what);
    }

    // 导出文件行数与行状态一致：3 行已上报 → 回执台账 3 行、未上报名单 0 行、举证包 3 行
    assert_eq!(
        csv_data_rows(&receipts),
        3,
        "回执台账应与「已上报 3 行」对上"
    );
    assert_eq!(
        csv_data_rows(&pending),
        0,
        "全部已上报，未上报名单不该还有行"
    );
    assert_eq!(
        csv_data_rows(&evidence),
        3,
        "举证包应逐行给出依据（一行一行对得上）"
    );

    // 状态库里的行状态：3 行都已上报
    let out = run_regval(
        ready.dir.path(),
        &["status", "--state-db", ready.db()],
        &[("REGVAL_RULE_DIR", &rule_dir())],
    );
    let text = stdout_of(&out);
    assert!(text.contains("已上报 3 行"), "状态汇总应与导出一致：{}", text);
}

// ————————————————————————————————————————————————————————————
// 验收二：进程重启后从断点续跑，已成功的行不重复推送
// ————————————————————————————————————————————————————————————

#[test]
fn 进程重启后从断点续跑_已成功的行不重复推送() {
    // 第 1 步受理；第 2 步 5xx；续跑时第 2、3 步受理
    let stub = ReportingGatewayStub::start(vec![
        Scenario::Accepted,
        Scenario::ServerError { status: 500 },
        Scenario::Accepted,
        Scenario::Accepted,
    ]);
    let ready = prepare(tempfile::tempdir().unwrap());
    confirm_all(&ready);

    // 第一次上报：第 2 步 5xx → 停批。退避次数压到 1（不重试），
    // 好让「第 2 步失败」这件事立刻发生。
    let out = push(&ready, &stub, &[("REGVAL_PUSH_RETRY_MAX_ATTEMPTS", "1")]);
    assert!(
        out.status.success(),
        "第一次上报应跑完（失败也要如实报出来）：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    assert!(text.contains("已上报 1 行"), "第 1 行应已上报：{}", text);
    assert!(text.contains("待重试 1 行"), "第 2 行应待重试：{}", text);
    assert!(
        text.contains("未执行 1 行"),
        "第 3 行应未执行（前面的步骤失败）：{}",
        text
    );
    assert!(
        text.contains("批末条件回滚"),
        "批次没跑完应触发批末回滚（汇总回批前值）：{}",
        text
    );

    let first_round = drain(&stub);
    assert_eq!(first_round.len(), 2, "第一次只发了两个请求（第 3 行没发）");
    let first_key = idem_of(&first_round[0]);

    // —— 进程重启：重新跑一次 regval push ——
    let out = push(&ready, &stub, &[("REGVAL_PUSH_RETRY_MAX_ATTEMPTS", "1")]);
    assert!(
        out.status.success(),
        "续跑应成功：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    assert!(
        text.contains("已上报 3 行"),
        "续跑后三行都该报上去了：{}",
        text
    );

    let second_round = drain(&stub);
    assert_eq!(second_round.len(), 2, "续跑只推剩下两行");
    for req in &second_round {
        assert_ne!(
            idem_of(req),
            first_key,
            "第 1 行已经报上去了，续跑**不能重复推送**"
        );
    }

    // 三行的回执都在库里，且幂等键 ↔ 回执号一一对应
    let out = run_regval(
        ready.dir.path(),
        &["audit", "--idempotency", "--state-db", ready.db()],
        &[("REGVAL_RULE_DIR", &rule_dir())],
    );
    let text = stdout_of(&out);
    assert!(
        text.contains("一一对应成立"),
        "续跑后对应关系仍应成立：{}",
        text
    );
    assert!(
        text.contains("孤儿回执 0 条") && text.contains("缺回执 0 条"),
        "不该有孤儿回执或缺回执：{}",
        text
    );
}

// ————————————————————————————————————————————————————————————
// 验收三：同一批次连续跑两次，第二次不产生新的推送请求
// ————————————————————————————————————————————————————————————

#[test]
fn 同一批次连续跑两次_第二次不产生新的推送请求() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let ready = prepare(tempfile::tempdir().unwrap());
    confirm_all(&ready);

    let out = push(&ready, &stub, &[]);
    assert!(out.status.success(), "第一次上报应成功");
    let first_round = drain(&stub);
    assert_eq!(first_round.len(), 3, "第一次发了三个请求");

    // 第二次：同一批次再跑一次 —— 载荷没变、已上报的行**不重复推送**
    let out = push(&ready, &stub, &[]);
    assert!(
        out.status.success(),
        "第二次上报应成功：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    assert!(
        text.contains("本批没有待上报的行"),
        "第二次应判为「没有待上报的行」：{}",
        text
    );
    let second_round = drain(&stub);
    assert!(
        second_round.is_empty(),
        "第二次**一个请求都不该发**，实际发了 {} 个",
        second_round.len()
    );
    assert_eq!(
        stub.receipts_created(),
        3,
        "网关侧也不该新建回执（重复提交不新建）"
    );
}

// ————————————————————————————————————————————————————————————
// 验收四：状态库损坏后按恢复流程找回「报过没有」这条事实
// ————————————————————————————————————————————————————————————

#[test]
fn 状态库损坏后能按恢复流程找回报过没有的事实() {
    let stub = ReportingGatewayStub::start(vec![Scenario::Accepted]);
    let ready = prepare(tempfile::tempdir().unwrap());
    confirm_all(&ready);
    let out = push(&ready, &stub, &[]);
    assert!(out.status.success(), "上报应成功");
    drain(&stub);

    // 把状态库文件写坏（模拟掉电 / 文件被截断）
    let mut bytes = vec![0x53u8; 4096];
    bytes[0..17].copy_from_slice(b"not-a-sqlite-file");
    std::fs::write(&ready.state_db, &bytes).expect("写坏状态库");
    let broken = std::fs::read(&ready.state_db).unwrap();

    // 恢复
    let report = ready.dir.path().join("recover.json");
    let out = run_regval(
        ready.dir.path(),
        &[
            "recover",
            "--state-db",
            ready.db(),
            "--out",
            report.to_str().unwrap(),
        ],
        &[("REGVAL_RULE_DIR", &rule_dir())],
    );
    assert!(
        out.status.success(),
        "恢复应跑完：stdout={} stderr={}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    assert!(
        !text.contains("没有恢复产物可换"),
        "有恢复日志就该恢复得回来：{}",
        text
    );

    // 原库一个字节都没动
    assert_eq!(
        std::fs::read(&ready.state_db).unwrap(),
        broken,
        "恢复不该改原库"
    );

    // 恢复出来的库里，回执与幂等键的对应关系还在
    let recovered = PathBuf::from(format!("{}.recovered", ready.db()));
    assert!(recovered.exists(), "恢复产物应写出来");
    let out = run_regval(
        ready.dir.path(),
        &[
            "audit",
            "--idempotency",
            "--state-db",
            recovered.to_str().unwrap(),
        ],
        &[("REGVAL_RULE_DIR", &rule_dir())],
    );
    let text = stdout_of(&out);
    assert!(
        text.contains("一一对应成立"),
        "恢复后幂等键与回执号的对应关系不该丢：{}",
        text
    );
}
