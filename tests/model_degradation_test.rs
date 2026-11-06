//! 模型端点降级与调用留痕的验收。
//!
//! 断言的是**端到端的命令行行为**，不是单元级的函数返回：
//!
//! ① 模型端点连不上 → 主路径照样产出**完整**结果契约，疑似项来源标 `deterministic`；
//! ② 模型端点超时 → 同上，错误码是 `MODEL_TIMEOUT`；
//! ③ 响应结构不合法 → 整次调用作废并降级，不产出任何模型结论；
//! ④ `--require-model` 时端点不可用 → 直接以 `MODEL_UNAVAILABLE` 退出，
//!    **不产出契约文件、不改动业务数据**；
//! ⑤ 每次调用都留痕：审计表里查得到元数据与降级标记（`regval status` 也能看到）。
//!
//! 对端是 [`gateway_stub`] 里的协议替身与「端口无人监听」两种情形；
//! 产品代码走的是真实 OpenAI-compatible 协议，院内真实模型端点**本轮未实连**
//! （凭据走环境变量），见 progress.txt。

mod gateway_stub;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use serde_json::Value;

use gateway_stub::{chat_completion, StubResponse, StubServer};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
}

/// 跑一次 regval：清掉全部业务环境变量，工作目录指到临时目录。
///
/// `model_env` 里给的键会重新设上（用来配模型端点）。
fn run_regval(cwd: &Path, args: &[&str], model_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .env_remove("OPENAI_TIMEOUT_SECS")
        .env_remove("REGVAL_MODEL_MAX_PAIRS")
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR");
    for (k, v) in model_env {
        cmd.env(k, v);
    }
    cmd.output().expect("regval 应可执行")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// 找一个没人监听的端口（连它必被拒）
fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("端口绑定失败");
    let port = listener.local_addr().expect("读端口失败").port();
    drop(listener);
    port
}

/// 查某张表的行数
fn count_rows(db: &Path, table: &str) -> i64 {
    let conn = Connection::open(db).expect("状态库应可打开");
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0))
        .unwrap_or(0)
}

/// 读模型调用审计表（全部行）
fn model_audits(db: &Path) -> Vec<Value> {
    let conn = Connection::open(db).expect("状态库应可打开");
    let mut stmt = conn
        .prepare(
            "SELECT call_id, batch_ref, requested_model, served_model, duration_ms, \
             candidate_pairs, accepted_pairs, total_tokens, degraded, error_code, candidate_source \
             FROM model_call_audit ORDER BY ts DESC, call_id DESC",
        )
        .expect("审计表应可查");
    let rows = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "call_id": r.get::<_, String>(0)?,
                "batch_ref": r.get::<_, String>(1)?,
                "requested_model": r.get::<_, String>(2)?,
                "served_model": r.get::<_, String>(3)?,
                "duration_ms": r.get::<_, i64>(4)?,
                "candidate_pairs": r.get::<_, i64>(5)?,
                "accepted_pairs": r.get::<_, i64>(6)?,
                "total_tokens": r.get::<_, Option<i64>>(7)?,
                "degraded": r.get::<_, i64>(8)? != 0,
                "error_code": r.get::<_, String>(9)?,
                "candidate_source": r.get::<_, String>(10)?,
            }))
        })
        .expect("审计表应可查");
    rows.map(|r| r.expect("审计行应可读")).collect()
}

/// 用库里的同一套算法算出「这一轮会问模型哪几对」，取第一对的行键。
///
/// 行键是纯函数算出来的（同一份输入两次跑一致），所以测试能用它拼出
/// 一条合规的模型回答 —— 回答里的行键必须是我们问过的键，否则会被判响应不合法。
fn residual_pair_keys() -> (String, String) {
    let tables = regval::ingest::read_tables(&fixture("dupe_exact.csv")).expect("样例表应可读");
    let mut rows = Vec::new();
    for table in &tables {
        rows.extend(regval::ingest::map_table(table).expect("样例表应可映射"));
    }
    let funnel = regval::dupe::run_funnel(
        &rows,
        &regval::dupe::distance::DistanceConfig::default(),
        &regval::dupe::DisconnectedSemanticSource,
    );
    let set = regval::dupe::candidates::residual_candidates(
        &rows,
        &funnel.cumulative,
        &regval::dupe::candidates::CandidateLimits::default(),
    );
    let pair = set
        .pairs
        .first()
        .expect("样例里应有「前三级没认下」的待问行对");
    (pair.left.row_key.clone(), pair.right.row_key.clone())
}

/// 跑一次校验并把契约读回来
fn validate_to_contract(
    cwd: &Path,
    db: &Path,
    out: &Path,
    model_env: &[(&str, &str)],
    extra_args: &[&str],
) -> (Output, Option<Value>) {
    let out_path = out.to_string_lossy().to_string();
    let db_path = db.to_string_lossy().to_string();
    let input = fixture("dupe_exact.csv").to_string_lossy().to_string();
    let mut args: Vec<&str> = vec![
        "validate",
        "--input",
        &input,
        "--out",
        &out_path,
        "--state-db",
        &db_path,
    ];
    args.extend_from_slice(extra_args);
    let output = run_regval(cwd, &args, model_env);
    let contract = if out.exists() {
        let text = std::fs::read_to_string(out).expect("契约文件应可读");
        Some(serde_json::from_str::<Value>(&text).expect("契约应是合法 JSON"))
    } else {
        None
    };
    (output, contract)
}

#[test]
fn 模型端点连不上时主路径仍产出完整结果契约() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let port = unused_port();
    let base = format!("http://127.0.0.1:{}", port);
    let model_env = vec![
        ("OPENAI_BASE_URL", base.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];

    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env, &[]);

    assert!(
        output.status.success(),
        "模型不可用不应挡住主路径，实际退出码 {:?}：{}",
        output.status.code(),
        stderr_of(&output)
    );
    let contract = contract.expect("降级也要产出结果契约");
    // 主路径的产出一样不缺：行、问题、疑似重复
    assert!(contract["rows"].as_array().expect("rows 应是数组").len() >= 7);
    assert!(!contract["issues"]
        .as_array()
        .expect("issues 应是数组")
        .is_empty());
    let dupes = contract["suspected_duplicates"]
        .as_array()
        .expect("suspected_duplicates 应是数组");
    assert!(!dupes.is_empty(), "确定性三级认下的疑似重复不该被降级抹掉");
    // 疑似项来源标确定性：读的人一眼知道「这批没经过模型那一关」
    for pair in dupes {
        assert_eq!(pair["source"], "deterministic");
    }
    // 模型段照实报降级
    assert_eq!(contract["model"]["degraded"], true);
    assert_eq!(contract["model"]["error_code"], "MODEL_CONNECTION_FAILED");
    assert_eq!(contract["model"]["candidate_source"], "deterministic");
    assert_eq!(contract["model"]["accepted_pairs"], 0);
    // 送出去的候选对数照样记着：事后看得出「本来要问几对」
    assert_eq!(contract["model"]["candidate_pairs"], 1);
    // 终端上也要看得见降级，不能只有契约里有
    let text = stdout_of(&output);
    assert!(
        text.contains("模型端点"),
        "终端应说明模型端点状态：{}",
        text
    );
    assert!(text.contains("MODEL_CONNECTION_FAILED"));
}

#[test]
fn 模型端点超时时主路径仍产出完整结果契约() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    // 替身回包前先睡 2.5 秒，客户端超时 1 秒 → 超时
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion("{\"items\":[]}"),
    )
    .with_delay(2500)]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "1"),
    ];

    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env, &[]);

    assert!(
        output.status.success(),
        "模型超时不挡住主路径，实际退出码 {:?}：{}",
        output.status.code(),
        stderr_of(&output)
    );
    let contract = contract.expect("超时也要产出结果契约");
    assert!(!contract["rows"]
        .as_array()
        .expect("rows 应是数组")
        .is_empty());
    assert_eq!(contract["model"]["degraded"], true);
    assert_eq!(contract["model"]["error_code"], "MODEL_TIMEOUT");
    assert_eq!(contract["model"]["candidate_source"], "deterministic");
}

#[test]
fn 响应结构不合法时整次调用作废并降级() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    // 回的不是 JSON → 整次调用不可用
    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion("这不是 JSON"),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];

    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env, &[]);

    assert!(output.status.success());
    let contract = contract.expect("响应不合法也要产出结果契约");
    assert_eq!(contract["model"]["degraded"], true);
    assert_eq!(contract["model"]["error_code"], "MODEL_RESPONSE_INVALID");
    // 未通过结构校验的结果一条都不许进结果
    assert_eq!(contract["model"]["accepted_pairs"], 0);
    assert_eq!(
        contract["model"]["proposals"]
            .as_array()
            .expect("proposals 应是数组")
            .len(),
        0
    );
}

#[test]
fn 要求模型可用时端点不可用直接报错_不产出契约不改业务数据() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let port = unused_port();
    let base = format!("http://127.0.0.1:{}", port);
    let model_env = vec![
        ("OPENAI_BASE_URL", base.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];

    let (output, contract) =
        validate_to_contract(dir.path(), &db, &out, &model_env, &["--require-model"]);

    assert!(
        !output.status.success(),
        "--require-model 时模型不可用必须失败，实际退出码 {:?}",
        output.status.code()
    );
    assert!(
        stderr_of(&output).contains("MODEL_UNAVAILABLE"),
        "报错应带机器可读错误码：{}",
        stderr_of(&output)
    );
    assert!(
        contract.is_none(),
        "--require-model 失败时不该产出结果契约文件"
    );
    assert!(!out.exists(), "--require-model 失败时不该产出结果契约文件");
    // 业务数据一行都不许改
    for table in ["rows", "issues", "submissions", "receipts", "confirmations"] {
        assert_eq!(
            count_rows(&db, table),
            0,
            "--require-model 失败时业务表 {} 不该有数据",
            table
        );
    }
    // 但「模型挂了」这件事本身要留痕：审计表里查得到降级标记
    let audits = model_audits(&db);
    assert_eq!(audits.len(), 1, "失败的调用也要留痕：{:?}", audits);
    assert_eq!(audits[0]["degraded"], true);
    assert_eq!(audits[0]["error_code"], "MODEL_CONNECTION_FAILED");
}

#[test]
fn 未配模型端点时本轮不调用模型_主路径照常() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");

    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &[], &[]);

    assert!(output.status.success());
    let contract = contract.expect("没配模型也要产出结果契约");
    assert_eq!(contract["model"]["attempted"], false);
    assert_eq!(contract["model"]["candidate_source"], "deterministic");
    assert!(!contract["rows"]
        .as_array()
        .expect("rows 应是数组")
        .is_empty());
    // 没调用过模型 → 审计表里不该凭空多出记录
    assert!(model_audits(&db).is_empty());
}

#[test]
fn 模型调用成功后审计表留有元数据() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");

    // 先算出这一轮会问模型哪几对（行键是纯函数算出来的，两次跑一致）
    let (left, right) = residual_pair_keys();

    // 接上替身，回一条「这两个是同一指标」的合规结论
    let content = serde_json::json!({
        "items": [{
            "left_row_key": left,
            "right_row_key": right,
            "kind": "same_indicator",
            "confidence": 0.83,
            "reason": "两处指标名指的都是同一个指标，只是写法不同",
            "evidence": ["左侧含『主要诊断填写正确率』", "右侧含『主要诊断填写正确率』"]
        }]
    })
    .to_string();
    let stub = StubServer::start(vec![StubResponse::json(200, chat_completion(&content))]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (second, contract2) = validate_to_contract(dir.path(), &db, &out, &model_env, &[]);

    assert!(
        second.status.success(),
        "模型可用时主路径应照常成功：{}",
        stderr_of(&second)
    );
    let contract2 = contract2.expect("模型可用也要产出结果契约");
    assert_eq!(contract2["model"]["degraded"], false);
    assert_eq!(contract2["model"]["accepted_pairs"], 1);
    assert_eq!(contract2["model"]["candidate_source"], "semantic");
    assert_eq!(
        contract2["model"]["proposals"]
            .as_array()
            .expect("proposals 应是数组")
            .len(),
        1
    );
    // 模型结论只是标注：不改变任何一行的上报状态
    for row in contract2["rows"].as_array().expect("rows 应是数组") {
        assert_eq!(row["submission_status"], "未上报");
    }

    // 审计表：这一条调用查得到元数据与「没有降级」的标记
    let audits = model_audits(&db);
    assert_eq!(audits.len(), 1, "应有且只有一条模型调用审计：{:?}", audits);
    let audit = &audits[0];
    assert_eq!(audit["degraded"], false);
    assert_eq!(audit["error_code"], "");
    assert_eq!(audit["requested_model"], "stub-model");
    assert_eq!(audit["served_model"], "stub-model");
    assert_eq!(audit["accepted_pairs"], 1);
    assert_eq!(audit["candidate_source"], "semantic");
    assert!(audit["duration_ms"].as_i64().unwrap_or(0) >= 0);
    assert_eq!(audit["total_tokens"], 170);

    // 发出去的那份报文：路径、鉴权头、请求体形状（断言落在实际报文上）
    let request = stub.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer stub-token"));
    let body = request.body_json();
    assert_eq!(body["model"], "stub-model");
    assert!(body["messages"].is_array(), "请求体应带 messages");
    assert_eq!(body["response_format"]["type"], "json_object");

    // regval status 能看到这条审计
    let db_path = db.to_string_lossy().to_string();
    let status = run_regval(dir.path(), &["status", "--state-db", &db_path], &[]);
    assert!(status.status.success());
    let text = stdout_of(&status);
    assert!(
        text.contains("模型调用审计"),
        "status 应显示模型调用审计：{}",
        text
    );
    assert!(text.contains("stub-model"));
}

#[test]
fn 降级调用也在审计表里留下降级标记() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let port = unused_port();
    let base = format!("http://127.0.0.1:{}", port);
    let model_env = vec![
        ("OPENAI_BASE_URL", base.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];

    let (output, _) = validate_to_contract(dir.path(), &db, &out, &model_env, &[]);
    assert!(output.status.success());

    let audits = model_audits(&db);
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0]["degraded"], true);
    assert_eq!(audits[0]["error_code"], "MODEL_CONNECTION_FAILED");
    assert_eq!(audits[0]["candidate_source"], "deterministic");
    assert_eq!(audits[0]["accepted_pairs"], 0);
    // 降级也要记清楚「本来要送多少对」：分不清「没候选」与「没送成」是两回事
    assert_eq!(audits[0]["candidate_pairs"], 1);
}
