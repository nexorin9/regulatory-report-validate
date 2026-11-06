//! HIS 科室主数据目录客户端（任务 29）的验收：
//! ① 契约测试：请求路径、鉴权头、响应字段全集；
//! ② 替身返回已知目录 → 检查结论正确（命中不报 / 未命中报「科室主数据不一致」）；
//! ③ 目录拉取失败 → 结果含「科室主数据未校验」标记，**不静默通过**；
//! ④ 科室名称别名命中不误伤；写法不符只提示、不阻断；
//! ⑤ 拉取失败退回上次缓存并标注数据时间；缓存过期会标出来；
//! ⑥ 缺凭据在发请求之前就报错（一个包都没发出去）。
//!
//! 对端是 [`gateway_stub`] 里的协议替身（本机可控进程）与「端口无人监听」情形；
//! 产品代码走的是 `spec.md`「出站报文（正式对接规范）」里的真实协议，
//! 院内真实 HIS 科室目录**本轮未实连**（地址与凭据走环境变量），见 progress.txt。

// 替身里有一部分方法本测试用不到（模型那边才用），别把告警带进本测试目标
#[allow(dead_code)]
mod gateway_stub;

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::PathBuf;

use gateway_stub::{StubResponse, StubServer};
use regval::bind::dept::UnavailableDirectory;
use regval::bind;
use regval::config::MapEnv;
use regval::errors::{EngineError, ErrorCode};
use regval::ingest::key;
use regval::ingest::{Field, SourceRow};
use regval::push::dept::{
    resolve_directory, CachedDirectory, DeptDirectoryCache, DeptDirectoryClient,
    DeptDirectoryConfig, DeptEntry, DirectorySnapshot, DirectorySource, HisDeptDirectory,
    DEFAULT_DEPT_TTL_SECS,
};

/// 内存缓存（测试替身；真实实现见 `src/db/dept_cache.rs`）
#[derive(Default)]
struct MemCache {
    rows: BTreeMap<String, CachedDirectory>,
}

impl DeptDirectoryCache for MemCache {
    fn load(&self, org_code: &str) -> Result<Option<CachedDirectory>, EngineError> {
        Ok(self.rows.get(org_code).cloned())
    }
    fn save(&mut self, org_code: &str, snapshot: &DirectorySnapshot) -> Result<(), EngineError> {
        self.rows.insert(
            org_code.to_string(),
            CachedDirectory {
                org_code: org_code.to_string(),
                fetched_at: snapshot.fetched_at.clone(),
                entry_count: snapshot.len(),
                payload: snapshot.to_payload()?,
            },
        );
        Ok(())
    }
}

/// 目录条目
fn entry(code: &str, name: &str, aliases: &[&str]) -> DeptEntry {
    DeptEntry {
        dept_code: code.to_string(),
        dept_name: name.to_string(),
        aliases: aliases.iter().map(|a| a.to_string()).collect(),
        parent_code: "0300".to_string(),
    }
}

/// 造一行来源行
fn row(dept: &str, dept_name: &str) -> SourceRow {
    let mut values = BTreeMap::new();
    values.insert(Field::OrgCode, "ORG-001".to_string());
    values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
    values.insert(Field::IndicatorName, "病案首页24小时内完成率".to_string());
    values.insert(Field::DeptCode, dept.to_string());
    values.insert(Field::DeptName, dept_name.to_string());
    values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
    values.insert(Field::BusinessKey, "BK-01".to_string());
    values.insert(Field::Numerator, "980".to_string());
    values.insert(Field::Denominator, "1000".to_string());
    values.insert(Field::Value, "98".to_string());
    values.insert(Field::Unit, "%".to_string());
    SourceRow {
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 2,
        dup_ordinal: 0,
        values,
    }
}

fn bind_rows(rows: &mut Vec<SourceRow>, dir: &dyn regval::bind::dept::DeptDirectory) -> bind::BindingOutcome {
    key::assign_dup_ordinals(rows);
    bind::bind(rows, dir)
}

/// 目录响应（两个科室 + 别名 + 上级）
fn directory_json() -> String {
    serde_json::json!([
        {
            "dept_code": "0301",
            "dept_name": "病案统计室",
            "aliases": ["病案统计室（本部）"],
            "parent_code": "0300"
        },
        {
            "dept_code": "0302",
            "dept_name": "病案室",
            "aliases": [],
            "parent_code": "0300"
        }
    ])
    .to_string()
}

/// 找一个没人监听的端口（连它必被拒）
fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("端口绑定失败");
    let port = listener.local_addr().expect("读端口失败").port();
    drop(listener);
    port
}

fn client_for(stub: &StubServer) -> DeptDirectoryClient {
    DeptDirectoryClient::new(DeptDirectoryConfig {
        base_url: stub.base_url.clone(),
        token: "stub-token".to_string(),
        timeout_secs: 5,
    })
    .expect("客户端应可建")
}

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ——————————————————————————————————————————————————————————————
// ① 契约测试：路径、头、字段
// ——————————————————————————————————————————————————————————————

#[test]
fn 请求路径鉴权头与响应字段全集() {
    let stub = StubServer::start(vec![StubResponse::json(200, directory_json())]);
    let client = client_for(&stub);

    let entries = client.fetch().expect("目录应拉得到");

    // 请求：路径 + 鉴权头
    let req = stub.next_request();
    assert_eq!(req.method, "GET");
    assert_eq!(req.path, "/api/v1/dept-directory");
    assert_eq!(req.header("authorization"), Some("Bearer stub-token"));

    // 响应字段：dept_code / dept_name / aliases[] / parent_code
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].dept_code, "0301");
    assert_eq!(entries[0].dept_name, "病案统计室");
    assert_eq!(entries[0].aliases, vec!["病案统计室（本部）".to_string()]);
    assert_eq!(entries[0].parent_code, "0300");
    assert_eq!(entries[1].dept_code, "0302");
    assert!(entries[1].aliases.is_empty());
}

// ——————————————————————————————————————————————————————————————
// ② 替身返回已知目录 → 检查结论正确
// ——————————————————————————————————————————————————————————————

#[test]
fn 目录命中不报未命中报科室主数据不一致() {
    let dir = HisDeptDirectory::new(vec![
        entry("0301", "病案统计室", &["病案统计室（本部）"]),
        entry("0302", "病案室", &[]),
    ]);
    let mut rows = vec![row("0301", "病案统计室"), row("0999", "查无此科")];
    let out = bind_rows(&mut rows, &dir);

    assert!(out.dept_master_checked, "目录可用就该标核对过");
    // 命中的那一行一条问题都没有
    assert_eq!(out.issues.len(), 1, "只有查无此科那一行该报问题");
    let issue = &out.issues[0];
    assert_eq!(issue.issue_kind, "科室主数据不一致");
    assert_eq!(issue.error_code, "DEPT_NOT_FOUND");
    assert_eq!(issue.severity, "阻断");
    assert!(!issue.basis.is_empty(), "每条问题都要带依据");
    assert!(issue.message.contains("0999"));
    assert_eq!(out.dept_unmatched, vec!["0999".to_string()]);
}

// ——————————————————————————————————————————————————————————————
// ④ 名称：别名命中不误伤；写法不符只提示
// ——————————————————————————————————————————————————————————————

#[test]
fn 科室名称别名命中不误伤() {
    let dir = HisDeptDirectory::new(vec![entry("0301", "病案统计室", &["病案统计室（本部）"])]);
    // 别名写法：不该报任何问题
    let mut rows = vec![row("0301", "病案统计室（本部）")];
    let out = bind_rows(&mut rows, &dir);
    assert!(out.issues.is_empty(), "别名命中不该报问题：{:?}", out.issues);

    // 全角半角与空格写法归一后仍算命中
    let mut rows = vec![row("0301", "病案统计室 (本部)")];
    let out = bind_rows(&mut rows, &dir);
    assert!(out.issues.is_empty(), "写法差异不该误伤：{:?}", out.issues);
}

#[test]
fn 科室名称写法不符只提示不阻断() {
    let dir = HisDeptDirectory::new(vec![entry("0301", "病案统计室", &["病案统计室（本部）"])]);
    let mut rows = vec![row("0301", "随便写的科室名")];
    let out = bind_rows(&mut rows, &dir);

    assert_eq!(out.issues.len(), 1);
    let issue = &out.issues[0];
    assert_eq!(issue.issue_kind, "科室名称写法不一致");
    assert_eq!(issue.severity, "提示", "写法不符不阻断");
    assert_eq!(issue.error_code, "DEPT_NAME_MISMATCH");
    assert!(!issue.basis.is_empty());
    assert!(issue.message.contains("病案统计室"), "要说清主数据里的写法");
    assert_eq!(out.dept_name_mismatched, vec!["0301".to_string()]);

    // 结论：有提示，不是「不通过」
    assert_eq!(
        regval::report::summary::conclusion_of(&out.issues),
        regval::report::summary::CONCLUSION_HINT
    );
}

#[test]
fn 科室代码查不到时不再叠一条名称提示() {
    let dir = HisDeptDirectory::new(vec![entry("0301", "病案统计室", &[])]);
    let mut rows = vec![row("0999", "随便写的科室名")];
    let out = bind_rows(&mut rows, &dir);

    // 代码查不到是阻断级问题；名称无从比对，不再叠一条提示（同一个错不报两遍）
    assert_eq!(out.issues.len(), 1);
    assert_eq!(out.issues[0].error_code, "DEPT_NOT_FOUND");
    assert!(out.dept_name_mismatched.is_empty());
}

// ——————————————————————————————————————————————————————————————
// ③ 目录拉不到 → 标「科室主数据未校验」，不静默通过
// ——————————————————————————————————————————————————————————————

#[test]
fn 目录拉取失败时结果含科室主数据未校验标记() {
    let stub = StubServer::start(vec![StubResponse::json(
        500,
        r#"{"error":{"message":"院内目录服务内部错误"}}"#,
    )]);
    let client = client_for(&stub);
    let mut cache = MemCache::default();

    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );

    assert!(!res.is_available());
    assert_eq!(res.source, DirectorySource::Unavailable);
    assert_eq!(res.error_code, "DEPT_DIRECTORY_SERVER_ERROR");
    assert!(
        res.unavailable_note().contains("科室主数据未校验"),
        "结果里必须带「科室主数据未校验」：{}",
        res.unavailable_note()
    );
    // 目录不可用 → 一条科室问题都不报（更不能凭空造一条假问题）
    let mut rows = vec![row("0999", "查无此科")];
    let out = bind_rows(&mut rows, &UnavailableDirectory);
    assert!(out.issues.is_empty());
    assert!(!out.dept_master_checked);
}

#[test]
fn 目录连不上时归为不可用且错误码可分() {
    let client = DeptDirectoryClient::new(DeptDirectoryConfig {
        base_url: format!("http://127.0.0.1:{}", unused_port()),
        token: "stub-token".to_string(),
        timeout_secs: 3,
    })
    .expect("客户端应可建");
    let mut cache = MemCache::default();

    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );
    assert!(!res.is_available());
    assert_eq!(res.error_code, "DEPT_DIRECTORY_CONNECTION_FAILED");
    assert!(res.unavailable_note().contains("科室主数据未校验"));
}

#[test]
fn 目录响应结构不合法时报错不当成空目录() {
    let stub = StubServer::start(vec![StubResponse::json(200, r#"{"dept_code":"0301"}"#)]);
    let err = client_for(&stub).fetch().unwrap_err();
    assert_eq!(err.code, ErrorCode::DeptDirectoryResponseInvalid);

    // 条目缺科室代码同样报错 —— 拿一条没有代码的记录比对，比不出来却当成
    // 「匹配不到」，等于凭空造一条假问题
    let stub = StubServer::start(vec![StubResponse::json(200, r#"[{"dept_name":"病案室"}]"#)]);
    let err = client_for(&stub).fetch().unwrap_err();
    assert_eq!(err.code, ErrorCode::DeptDirectoryResponseInvalid);
    assert!(err.message.contains("dept_code"));
}

// ——————————————————————————————————————————————————————————————
// ⑤ 缓存兜底 + 数据时间
// ——————————————————————————————————————————————————————————————

#[test]
fn 拉取失败时用上次缓存并标注数据时间() {
    let mut cache = MemCache::default();
    cache
        .save(
            "ORG-001",
            &DirectorySnapshot {
                entries: vec![entry("0301", "病案统计室", &[])],
                fetched_at: "2026-11-06 08:50:00".to_string(),
            },
        )
        .unwrap();

    let stub = StubServer::start(vec![StubResponse::json(503, "服务不可用")]);
    let client = client_for(&stub);
    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );

    assert!(res.is_available(), "有缓存就该继续核对");
    assert_eq!(res.source, DirectorySource::Cache);
    assert_eq!(res.entry_count, 1);
    assert_eq!(res.fetched_at, "2026-11-06 08:50:00");
    assert!(res.note.contains("2026-11-06 08:50:00"), "必须标注数据时间");
    assert!(res.note.contains("上次缓存"));
    assert!(!res.stale, "22 分钟内的缓存不算过期");
    assert_eq!(res.error_code, "DEPT_DIRECTORY_SERVER_ERROR");
}

#[test]
fn 缓存超过有效期会被标出来() {
    let mut cache = MemCache::default();
    cache
        .save(
            "ORG-001",
            &DirectorySnapshot {
                entries: vec![entry("0301", "病案统计室", &[])],
                fetched_at: "2026-11-06 08:00:00".to_string(),
            },
        )
        .unwrap();

    let stub = StubServer::start(vec![StubResponse::json(503, "服务不可用")]);
    let client = client_for(&stub);
    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );

    assert!(res.is_available());
    assert!(res.stale, "72 分钟 > 1 小时有效期");
    assert!(res.note.contains("已超过有效期"));
}

#[test]
fn 实时拉到时目录进缓存并覆盖旧的那份() {
    let mut cache = MemCache::default();
    cache
        .save(
            "ORG-001",
            &DirectorySnapshot {
                entries: vec![entry("0999", "旧目录里的科室", &[])],
                fetched_at: "2026-11-06 08:00:00".to_string(),
            },
        )
        .unwrap();

    let stub = StubServer::start(vec![StubResponse::json(200, directory_json())]);
    let client = client_for(&stub);
    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );

    assert_eq!(res.source, DirectorySource::Live);
    assert_eq!(res.entry_count, 2);
    assert_eq!(res.fetched_at, "2026-11-06 09:12:00");
    assert!(!res.stale);

    // 缓存里已是新目录（覆盖，不是追加）
    let cached = cache.load("ORG-001").unwrap().unwrap();
    assert_eq!(cached.entry_count, 2);
    assert_eq!(cached.fetched_at, "2026-11-06 09:12:00");
    assert!(!cached.payload.contains("0999"));
}

#[test]
fn 缓存载荷坏了按不可用处理不当成空目录() {
    let mut cache = MemCache::default();
    cache.rows.insert(
        "ORG-001".to_string(),
        CachedDirectory {
            org_code: "ORG-001".to_string(),
            fetched_at: "2026-11-06 08:50:00".to_string(),
            entry_count: 2,
            payload: "{不是 JSON".to_string(),
        },
    );
    let stub = StubServer::start(vec![StubResponse::json(503, "服务不可用")]);
    let client = client_for(&stub);
    let res = resolve_directory(
        Some(&client),
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );
    // 空目录会把每一行都报成「科室主数据不一致」—— 宁可标未校验
    assert!(!res.is_available());
    assert!(res.unavailable_note().contains("科室主数据未校验"));
}

// ——————————————————————————————————————————————————————————————
// ⑥ 缺凭据：发请求之前就报错
// ——————————————————————————————————————————————————————————————

#[test]
fn 缺凭据时报错且一个包都没发出去() {
    let mut env = MapEnv::new();
    env.set("HIS_BASE_URL", "https://his.internal.example");
    let err = DeptDirectoryConfig::load(&env).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigMissing);
    assert!(err.message.contains("HIS_API_TOKEN"));
    assert!(err.location.unwrap().contains("HIS_API_TOKEN"));

    // 没配目录 → 一个请求都不发，目录按不可用处理
    let stub = StubServer::start(vec![StubResponse::json(200, directory_json())]);
    let mut cache = MemCache::default();
    let res = resolve_directory(
        None,
        &mut cache,
        "ORG-001",
        "2026-11-06 09:12:00",
        DEFAULT_DEPT_TTL_SECS,
    );
    assert!(!res.is_available());
    assert!(res.unavailable_note().contains("科室主数据未校验"));
    assert!(
        stub.try_next_request(300).is_none(),
        "没配目录时不该发出任何请求"
    );
}

#[test]
fn 凭据只来自环境变量不写死在代码里() {
    let source = include_str!("../src/push/dept.rs");
    assert!(!source.contains("HIS_API_TOKEN="));
    assert!(!source.contains("Bearer his-"));
    // 端点与凭据一律从配置来
    let cfg = DeptDirectoryConfig {
        base_url: "https://his.internal.example".to_string(),
        token: "from-env".to_string(),
        timeout_secs: 5,
    };
    assert!(cfg
        .directory_endpoint()
        .starts_with("https://his.internal.example"));
}

// ——————————————————————————————————————————————————————————————
// 端到端：一次 validate 之后契约里科室维度说清楚了
// ——————————————————————————————————————————————————————————————

/// 跑一次 validate（工作目录放在临时目录，避免读到本机 .env）
fn run_validate(
    input: &std::path::Path,
    out: &std::path::Path,
    state_db: &std::path::Path,
    work_dir: &std::path::Path,
    his_base_url: Option<&str>,
) -> std::process::Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_regval"));
    cmd.arg("validate")
        .arg("--input")
        .arg(input)
        .arg("--out")
        .arg(out)
        .arg("--state-db")
        .arg(state_db)
        .current_dir(work_dir);
    // 目录相关环境变量：给了就用，没给就明确清掉（别让本机 .env 掺进来）
    match his_base_url {
        Some(url) => {
            cmd.env("HIS_BASE_URL", url);
            cmd.env("HIS_API_TOKEN", "stub-token");
        }
        None => {
            cmd.env_remove("HIS_BASE_URL");
            cmd.env_remove("HIS_API_TOKEN");
        }
    }
    cmd.output().expect("regval 应可执行")
}

#[test]
fn 端到端拉得到时契约里科室维度核对过并分得清提示与阻断() {
    let stub = StubServer::start(vec![StubResponse::json(200, directory_json())]);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let input = project_dir().join("tests/fixtures/dept_directory.csv");

    let output = run_validate(
        &input,
        &out,
        &state_db,
        dir.path(),
        Some(&stub.base_url),
    );
    assert!(
        output.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: serde_json::Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    // 目录状态：核对过、来源是实时拉取、条目数对得上
    assert_eq!(contract["summary"]["dept_master_checked"], true);
    assert_eq!(contract["dept_directory"]["checked"], true);
    assert_eq!(contract["dept_directory"]["source"], "live");
    assert_eq!(contract["dept_directory"]["entry_count"], 2);
    assert_eq!(contract["dept_directory"]["source_zh"], "实时拉取");
    assert!(!contract["dept_directory"]["fetched_at"]
        .as_str()
        .unwrap_or("")
        .is_empty());
    // 契约版本没升（追加段，插件读法不变）
    assert_eq!(contract["schema_version"], "regval.contract/1.0.0");

    // 三行三种结论：命中且写法一致 → 通过；写法不符 → 有提示；代码查不到 → 不通过
    let rows = contract["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let by_conclusion: BTreeSet<&str> = rows
        .iter()
        .map(|r| r["conclusion"].as_str().unwrap_or(""))
        .collect();
    assert!(by_conclusion.contains("通过"), "行结论：{:?}", by_conclusion);
    assert!(by_conclusion.contains("有提示"), "行结论：{:?}", by_conclusion);
    assert!(by_conclusion.contains("不通过"), "行结论：{:?}", by_conclusion);

    // 问题类型里两类科室问题分得清
    let kinds = &contract["summary"]["issue_count_by_kind"];
    assert_eq!(kinds["科室主数据不一致"], 1);
    assert_eq!(kinds["科室名称写法不一致"], 1);

    // 每条科室问题都带依据与错误码
    let issues = contract["issues"].as_array().unwrap();
    for issue in issues {
        let kind = issue["issue_kind"].as_str().unwrap_or("");
        if kind == "科室主数据不一致" {
            assert_eq!(issue["error_code"], "DEPT_NOT_FOUND");
            assert_eq!(issue["severity"], "阻断");
            assert!(!issue["basis"].as_str().unwrap_or("").is_empty());
        }
        if kind == "科室名称写法不一致" {
            assert_eq!(issue["error_code"], "DEPT_NAME_MISMATCH");
            assert_eq!(issue["severity"], "提示");
            assert!(!issue["basis"].as_str().unwrap_or("").is_empty());
        }
    }
}

#[test]
fn 端到端拉不到时结果含科室主数据未校验标记() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("contract.json");
    let state_db = dir.path().join("state.db");
    let input = project_dir().join("tests/fixtures/dept_directory.csv");

    // 指向一个没人监听的端口 → 目录拉不到
    let output = run_validate(
        &input,
        &out,
        &state_db,
        dir.path(),
        Some(&format!("http://127.0.0.1:{}", unused_port())),
    );
    assert!(
        output.status.success(),
        "目录拉不到不该让整轮校验失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = std::fs::read_to_string(&out).unwrap();
    let contract: serde_json::Value = serde_json::from_str(&raw).expect("契约 JSON 应可解析");

    assert_eq!(contract["summary"]["dept_master_checked"], false);
    assert_eq!(
        contract["summary"]["dept_master_note"].as_str(),
        Some("科室主数据未校验")
    );
    assert_eq!(contract["dept_directory"]["checked"], false);
    assert_eq!(contract["dept_directory"]["source"], "unavailable");
    assert_eq!(
        contract["dept_directory"]["error_code"],
        "DEPT_DIRECTORY_CONNECTION_FAILED"
    );
    assert!(
        contract["dept_directory"]["note"]
            .as_str()
            .unwrap_or("")
            .contains("科室主数据未校验"),
        "目录段里必须带「科室主数据未校验」"
    );

    // 目录拉不到 → 一条科室问题都不报（不能凭空造假问题）
    let kinds = &contract["summary"]["issue_count_by_kind"];
    assert!(kinds.get("科室主数据不一致").is_none());
    assert!(kinds.get("科室名称写法不一致").is_none());

    // 终端也把这件事说出来
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("科室主数据未校验"), "终端输出：{}", stdout);
}
