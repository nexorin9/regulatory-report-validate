//! 重复检测第四级（模型语义归并）的验收。
//!
//! 断言的是**端到端的命令行行为**，不是单元级的函数返回：
//!
//! ① 模型给的语义结论进入漏斗第四级 → 写进结果契约的 `suspected_duplicates`
//!    （归并类型 / 置信度 / 理由 / 依据都留着，供经办人逐条看）；
//! ② 置信度不够的结论降级为「疑似待人工判定」，**不进可合并候选**；
//! ③ 语义候选在原表「疑似重复项」列里可读展示（带对方行键）；
//! ④ **模型结论未经人工确认不改变任何一行的上报状态**（三张业务表计数为 0）；
//! ⑤ 人工确认后确认记录含确认人 / 确认时间 / 行键；
//! ⑥ 接入点只在重复检测层 —— 上报层与状态层不碰模型。
//!
//! 对端是 [`gateway_stub`] 里的协议替身；产品代码走的是真实 OpenAI-compatible
//! 协议，院内真实模型端点**本轮未实连**（凭据走环境变量），见 progress.txt。

mod gateway_stub;

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
        .env_remove("REGVAL_MODEL_MIN_CONFIDENCE")
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

/// 查某张表的行数
fn count_rows(db: &Path, table: &str) -> i64 {
    let conn = Connection::open(db).expect("状态库应可打开");
    conn.query_row(&format!("SELECT COUNT(*) FROM {}", table), [], |r| r.get(0))
        .unwrap_or(0)
}

/// 读确认记录表（全部行）
fn confirmations(db: &Path) -> Vec<Value> {
    let conn = Connection::open(db).expect("状态库应可打开");
    let mut stmt = conn
        .prepare(
            "SELECT batch_ref, row_uid, action, confirmed_by, confirmed_at, note
               FROM confirmations ORDER BY id",
        )
        .expect("确认记录表应可查");
    let rows = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "batch_ref": r.get::<_, String>(0)?,
                "row_uid": r.get::<_, String>(1)?,
                "action": r.get::<_, String>(2)?,
                "confirmed_by": r.get::<_, String>(3)?,
                "confirmed_at": r.get::<_, String>(4)?,
                "note": r.get::<_, String>(5)?,
            }))
        })
        .expect("确认记录表应可查");
    rows.map(|r| r.expect("确认行应可读")).collect()
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

/// 造一份模型回答（置信度可调）
fn merge_content(left: &str, right: &str, confidence: f64) -> String {
    serde_json::json!({
        "items": [{
            "left_row_key": left,
            "right_row_key": right,
            "kind": "same_indicator",
            "confidence": confidence,
            "reason": "两处指标名指的都是主要诊断填写正确率，只是写法不同",
            "evidence": ["左侧指标名含『主要诊断填写正确率』", "右侧指标名含『主要诊断填写正确率』"]
        }]
    })
    .to_string()
}

/// 跑一次校验并把契约读回来
fn validate_to_contract(
    cwd: &Path,
    db: &Path,
    out: &Path,
    model_env: &[(&str, &str)],
) -> (Output, Option<Value>) {
    let out_path = out.to_string_lossy().to_string();
    let db_path = db.to_string_lossy().to_string();
    let input = fixture("dupe_exact.csv").to_string_lossy().to_string();
    let output = run_regval(
        cwd,
        &[
            "validate",
            "--input",
            &input,
            "--out",
            &out_path,
            "--state-db",
            &db_path,
        ],
        model_env,
    );
    let contract = if out.exists() {
        let text = std::fs::read_to_string(out).expect("契约文件应可读");
        Some(serde_json::from_str::<Value>(&text).expect("契约应是合法 JSON"))
    } else {
        None
    };
    (output, contract)
}

/// 找契约里的语义级候选对
fn semantic_pairs(contract: &Value) -> Vec<&Value> {
    contract["suspected_duplicates"]
        .as_array()
        .expect("suspected_duplicates 应是数组")
        .iter()
        .filter(|p| p["match_level"] == "semantic")
        .collect()
}

#[test]
fn 模型语义结论进入漏斗第四级_归并类型与依据都留在契约里() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);

    assert!(
        output.status.success(),
        "模型可用时主路径应成功：{}",
        stderr_of(&output)
    );
    let contract = contract.expect("模型可用也要产出结果契约");
    let pairs = semantic_pairs(&contract);
    assert_eq!(
        pairs.len(),
        1,
        "语义级应产出一对：{:?}",
        contract["suspected_duplicates"]
    );
    let pair = pairs[0];
    assert_eq!(pair["source"], "semantic");
    assert_eq!(pair["semantic_kind"], "same_indicator");
    assert_eq!(pair["semantic_confidence"], 0.92);
    assert!(pair["semantic_reason"]
        .as_str()
        .unwrap()
        .contains("主要诊断填写正确率"));
    assert_eq!(
        pair["semantic_evidence"]
            .as_array()
            .expect("依据应是数组")
            .len(),
        2
    );
    assert_eq!(pair["state"], "疑似重复");
    assert_eq!(pair["mergeable"], true, "置信度达标应进可合并候选");
    // 第四级这一轮的状态段看得见
    assert_eq!(contract["dupe_semantic"]["connected"], true);
    assert_eq!(contract["dupe_semantic"]["stats"]["mergeable"], 1);
    assert_eq!(contract["dupe_semantic"]["stats"]["pending_manual"], 0);
    // 契约版本不变（写回用的 5 个新列与行结果结构没变）
    assert_eq!(contract["schema_version"], "regval.contract/1.0.0");
}

#[test]
fn 语义候选在疑似重复项列里可读展示_带对方行键() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (_, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);
    let contract = contract.expect("契约应产出");

    let rows = contract["rows"].as_array().expect("rows 应是数组");
    let hit: Vec<&Value> = rows
        .iter()
        .filter(|r| r["row_uid"] == left || r["row_uid"] == right)
        .collect();
    assert_eq!(hit.len(), 2, "这两行都该有疑似重复项标注");
    for row in &hit {
        let text = row["suspected_duplicate"].as_str().unwrap_or("");
        assert!(
            text.contains("模型判语义等价"),
            "疑似重复项列应写明是模型判的：{}",
            text
        );
        // 带对方行键：经办人要能顺着行键找到另一行
        let other = if row["row_uid"] == left {
            &right
        } else {
            &left
        };
        assert!(
            text.contains(&other[..8.min(other.len())]),
            "疑似重复项列应带对方行键：{}",
            text
        );
    }
}

#[test]
fn 置信度不足的语义结论降级为疑似待人工判定_不进可合并候选() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.42)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);

    assert!(output.status.success());
    let contract = contract.expect("契约应产出");
    let pairs = semantic_pairs(&contract);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0]["state"], "疑似待人工判定");
    assert_eq!(pairs[0]["mergeable"], false, "置信度不足不进可合并候选");
    // 降级的原因写在依据里，且行级标注写明是「待人工判定」
    assert!(pairs[0]["basis"].as_str().unwrap().contains("低于阈值"));
    assert_eq!(contract["dupe_semantic"]["stats"]["pending_manual"], 1);
    assert_eq!(contract["dupe_semantic"]["stats"]["mergeable"], 0);
    let rows = contract["rows"].as_array().expect("rows 应是数组");
    let hit: Vec<&Value> = rows
        .iter()
        .filter(|r| r["row_uid"] == left || r["row_uid"] == right)
        .collect();
    for row in &hit {
        assert!(row["suspected_duplicate"]
            .as_str()
            .unwrap_or("")
            .contains("疑似待人工判定"));
    }
}

#[test]
fn 置信度阈值可配_调低后同一结论进可合并候选() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.42)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
        ("REGVAL_MODEL_MIN_CONFIDENCE", "0.40"),
    ];
    let (_, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);
    let contract = contract.expect("契约应产出");
    let pairs = semantic_pairs(&contract);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0]["mergeable"], true);
    assert_eq!(contract["dupe_semantic"]["min_confidence"], 0.40);
}

#[test]
fn 模型结论未经人工确认不改变任何一行的上报状态() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);
    assert!(output.status.success());

    let contract = contract.expect("契约应产出");
    // 每一行的上报状态都还是「未上报」—— 模型不推进任何状态
    for row in contract["rows"].as_array().expect("rows 应是数组") {
        assert_eq!(row["submission_status"], "未上报");
    }
    // 语义候选确实在结果里（不是「模型没跑」）
    assert_eq!(semantic_pairs(&contract).len(), 1);
    // 上报与确认三张表都还是空的：没有确认就不许推
    for table in ["submissions", "receipts", "confirmations"] {
        assert_eq!(
            count_rows(&db, table),
            0,
            "未经人工确认时业务表 {} 不该有数据",
            table
        );
    }
    // 终端上也要看得见第四级做了什么
    let text = stdout_of(&output);
    assert!(text.contains("语义归并"), "终端应报第四级计数：{}", text);
    assert!(text.contains("可合并候选 1 对"));
}

#[test]
fn 人工确认后确认记录含确认人_确认时间_行键() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);
    assert!(output.status.success());
    let contract = contract.expect("契约应产出");
    assert_eq!(semantic_pairs(&contract).len(), 1);

    // 经办人逐条勾选确认
    let db_path = db.to_string_lossy().to_string();
    let confirm = run_regval(
        dir.path(),
        &[
            "confirm",
            "--row",
            &left,
            "--by",
            "数据管理员张老师",
            "--note",
            "两行是同一笔记录",
            "--state-db",
            &db_path,
        ],
        &[],
    );
    assert!(
        confirm.status.success(),
        "确认应成功：{}",
        stderr_of(&confirm)
    );

    let records = confirmations(&db);
    assert_eq!(records.len(), 1, "确认记录应有一条：{:?}", records);
    let record = &records[0];
    assert_eq!(record["row_uid"], left.as_str());
    assert_eq!(record["confirmed_by"], "数据管理员张老师");
    assert_eq!(record["action"], "confirm");
    assert!(
        !record["confirmed_at"].as_str().unwrap_or("").is_empty(),
        "确认时间必须留痕"
    );
    assert!(record["note"].as_str().unwrap_or("").contains("同一笔记录"));

    // 确认记录可查
    let list = run_regval(dir.path(), &["confirmations", "--state-db", &db_path], &[]);
    assert!(list.status.success());
    let text = stdout_of(&list);
    assert!(
        text.contains("数据管理员张老师"),
        "确认记录应列出确认人：{}",
        text
    );
    assert!(text.contains(&left), "确认记录应列出行键：{}", text);
}

#[test]
fn 确认人不得为空_确认的行必须真的存在() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (output, _) = validate_to_contract(dir.path(), &db, &out, &model_env);
    assert!(output.status.success());

    let db_path = db.to_string_lossy().to_string();
    // 缺确认人 → 参数校验直接拦下
    let no_by = run_regval(
        dir.path(),
        &["confirm", "--row", &left, "--state-db", &db_path],
        &[],
    );
    assert!(
        !no_by.status.success(),
        "没有确认人的确认不该成功：{}",
        stdout_of(&no_by)
    );
    // 行键不存在 → 报错，不静默记一条悬空确认
    let ghost = run_regval(
        dir.path(),
        &[
            "confirm",
            "--row",
            "row-ghost-999",
            "--by",
            "张老师",
            "--state-db",
            &db_path,
        ],
        &[],
    );
    assert!(!ghost.status.success());
    assert!(
        stderr_of(&ghost).contains("row-ghost-999"),
        "报错应点名那一行：{}",
        stderr_of(&ghost)
    );
    assert_eq!(
        count_rows(&db, "confirmations"),
        0,
        "两次失败的确认都不该留下记录"
    );
}

#[test]
fn 模型端点不可用时第四级候选为空_状态照实报() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");

    // 不配模型端点 → 本轮未接入
    let (output, contract) = validate_to_contract(dir.path(), &db, &out, &[]);
    assert!(output.status.success());
    let contract = contract.expect("契约应产出");
    assert!(
        semantic_pairs(&contract).is_empty(),
        "没接模型端点时第四级不该有候选"
    );
    assert_eq!(contract["dupe_semantic"]["connected"], false);
    assert!(contract["dupe_semantic"]["note"]
        .as_str()
        .unwrap_or("")
        .contains("未接入"));
    // 确定性三级的候选一条不少
    assert!(!contract["suspected_duplicates"]
        .as_array()
        .expect("suspected_duplicates 应是数组")
        .is_empty());
}

#[test]
fn 语义级不与前三级重复报_候选集合单调不减() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("contract.json");
    let (left, right) = residual_pair_keys();

    let stub = StubServer::start(vec![StubResponse::json(
        200,
        chat_completion(&merge_content(&left, &right, 0.92)),
    )]);
    let model_env = vec![
        ("OPENAI_BASE_URL", stub.base_url.as_str()),
        ("OPENAI_API_KEY", "stub-token"),
        ("OPENAI_MODEL", "stub-model"),
        ("OPENAI_TIMEOUT_SECS", "5"),
    ];
    let (_, contract) = validate_to_contract(dir.path(), &db, &out, &model_env);
    let contract = contract.expect("契约应产出");

    let pairs = contract["suspected_duplicates"]
        .as_array()
        .expect("suspected_duplicates 应是数组");
    // 每一对行的规范键唯一：不被两级重复认下
    let mut keys: Vec<String> = Vec::new();
    for pair in pairs {
        let l = pair["left_row_key"].as_str().unwrap_or("");
        let r = pair["right_row_key"].as_str().unwrap_or("");
        let key = if l <= r {
            format!("{}|{}", l, r)
        } else {
            format!("{}|{}", r, l)
        };
        assert!(!keys.contains(&key), "同一对行不该被报两次：{}", key);
        keys.push(key);
    }
    // 确定性三级 + 第四级：语义那一对是新增的，不是替换
    let semantic = semantic_pairs(&contract);
    assert_eq!(semantic.len(), 1);
    assert!(pairs.len() > 1, "确定性三级的候选还在");
}

#[test]
fn 接入点只在重复检测层_上报层与状态层不碰模型() {
    // 第四级的接入代码在 src/dupe/ 里
    let merge_file = project_dir().join("src/dupe/merge.rs");
    let merge_src = std::fs::read_to_string(&merge_file).expect("src/dupe/merge.rs 应存在");
    assert!(merge_src.contains("ModelProposal"));
    assert!(merge_src.contains("SemanticCandidateSource"));

    // 上报层与状态层里不出现模型归并 —— 模型不得推进状态
    for dir in ["src/push", "src/state"] {
        let path = project_dir().join(dir);
        if !path.is_dir() {
            // 该层还没落地（后续任务），落地后这个用例会自动开始盯着
            continue;
        }
        let mut stack = vec![path.clone()];
        while let Some(cur) = stack.pop() {
            for entry in std::fs::read_dir(&cur).expect("目录应可读") {
                let entry = entry.expect("目录项应可读");
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).expect("源码应可读");
                for banned in ["ModelGateway", "ModelProposal", "dupe::merge"] {
                    assert!(
                        !text.contains(banned),
                        "{} 里不该出现 {}（模型只标，不推进状态）",
                        p.display(),
                        banned
                    );
                }
            }
        }
    }
}
