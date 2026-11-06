//! 工程底座的验收：各子命令 --help 可用、doctor 逐项自检、结构化日志、运行摘要。
//!
//! 每条子命令都在**清掉相关环境变量**的干净环境下跑，避免受开发机环境影响；
//! 工作目录也指到临时目录，免得读到开发者本机的 `.env`。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_dir().join("tests/fixtures").join(name)
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn 各子命令都在且各自_help可用() {
    let dir = tempfile::tempdir().unwrap();
    for cmd in [
        "validate",
        "push",
        "status",
        "rules",
        "merge",
        "export",
        "replay",
        "doctor",
        "dept",
    ] {
        let out = run_in(dir.path(), &[cmd, "--help"]);
        assert!(
            out.status.success(),
            "`regval {} --help` 应可用，实际退出码 {:?}",
            cmd,
            out.status.code()
        );
        let text = stdout_of(&out);
        assert!(
            text.contains(cmd),
            "`regval {} --help` 的帮助里应出现子命令名",
            cmd
        );
    }
}

#[test]
fn 顶层_help列出全部子命令() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_in(dir.path(), &["--help"]);
    assert!(out.status.success());
    let text = stdout_of(&out);
    for cmd in [
        "validate", "push", "status", "rules", "merge", "export", "replay", "doctor",
    ] {
        assert!(text.contains(cmd), "顶层帮助应列出 {}", cmd);
    }
}

#[test]
fn doctor_在环境齐备时逐项列出且退出码为0() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let rule_dir = dir.path().join("rules");
    std::fs::create_dir_all(&rule_dir).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["doctor", "--state-db"])
        .arg(&state_db)
        .current_dir(dir.path())
        .env(
            "REGULATORY_REPORT_BASE_URL",
            "https://gateway.internal.example",
        )
        .env("REGULATORY_REPORT_TOKEN", "token-value-1")
        .env("HIS_BASE_URL", "https://his.internal.example")
        .env("HIS_API_TOKEN", "token-value-2")
        .env("OPENAI_BASE_URL", "https://model.internal.example/v1")
        .env("OPENAI_API_KEY", "token-value-3")
        .env("OPENAI_MODEL", "placeholder-model")
        .env("OPENAI_TIMEOUT_SECS", "20")
        .env("REGVAL_RULE_DIR", &rule_dir)
        .env("REGVAL_STATE_DB", &state_db)
        .output()
        .expect("regval 应可执行");

    assert!(out.status.success(), "齐备时 doctor 应退出 0");
    let text = stdout_of(&out);
    // 逐项列出，而不是只输出一句 OK
    assert!(text.contains("工具链"), "应逐项列出检查项");
    assert!(text.contains("状态库"));
    assert!(text.contains("规则目录"));
    assert!(text.contains("REGULATORY_REPORT_TOKEN"));
    assert!(text.contains("结论："));
    assert!(text.contains("0 项缺失"));
    // 敏感值不进输出
    assert!(!text.contains("token-value-1"));
}

#[test]
fn doctor_在环境缺失时点名缺哪几项并以非0退出() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_in(dir.path(), &["doctor"]);
    assert!(!out.status.success(), "缺项时 doctor 应以非 0 退出");
    let text = stdout_of(&out);
    assert!(text.contains("[缺失]"), "应标出缺失项");
    assert!(text.contains("REGULATORY_REPORT_TOKEN"), "应点名缺失变量");
    assert!(text.contains("HIS_API_TOKEN"), "应点名缺失变量");
    assert!(text.contains("补法："), "应给出补法");
    assert!(!text.contains("结论：0 项"), "结论不应为零缺失");
}

#[test]
fn 缺状态库配置时校验命令报错且给出错误码() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            fixture("mini.csv").to_str().unwrap(),
            "--out",
            dir.path().join("result.json").to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("CONFIG_MISSING"), "应给出明确错误码：{}", err);
    assert!(err.contains("REGVAL_STATE_DB"), "应点名缺哪个变量：{}", err);
    assert!(
        !dir.path().join("result.json").exists(),
        "报错时不应产出契约文件"
    );
}

#[test]
fn 校验产出契约并把运行摘要落进状态库() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out_json = dir.path().join("result.json");

    let out = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            fixture("mini.csv").to_str().unwrap(),
            "--out",
            out_json.to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "校验应成功：{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("已校验 6 行"));

    // 结构化日志：一行一条 JSON，带时间/级别/事件/批次
    let err = stderr_of(&out);
    let lines: Vec<&str> = err.lines().filter(|l| l.starts_with('{')).collect();
    assert!(lines.len() >= 2, "应有起止两条 JSON 日志：{}", err);
    for line in &lines {
        let v: Value = serde_json::from_str(line).expect("日志行应为合法 JSON");
        assert!(v["ts"].is_string(), "日志应带时间");
        assert!(v["level"].is_string(), "日志应带级别");
        assert!(v["event"].is_string(), "日志应带事件名");
        assert!(v["batch_ref"].is_string(), "日志应带批次号");
    }
    // 起止两条都在，且起在前、止在后（中间可能还有别的环节日志，
    // 例如科室主数据目录的解析结果 —— 它也是这一批的一部分）
    let start_at = lines
        .iter()
        .position(|l| l.contains("validate.start"))
        .expect("应有 validate.start 日志");
    let done_at = lines
        .iter()
        .position(|l| l.contains("validate.done"))
        .expect("应有 validate.done 日志");
    assert!(start_at < done_at, "起止日志顺序不对：{:?}", lines);

    // 运行摘要落状态库，status 看得见
    let out = run_in(
        dir.path(),
        &["status", "--state-db", state_db.to_str().unwrap()],
    );
    assert!(out.status.success());
    let text = stdout_of(&out);
    assert!(text.contains("validate"), "status 应显示子命令：{}", text);
    assert!(text.contains("行 6"), "status 应显示行数：{}", text);
    assert!(text.contains("问题 2 项"), "status 应显示问题数：{}", text);
}

#[test]
fn 前置条件不满足的子命令明确报错而不是静默成功() {
    let dir = tempfile::tempdir().unwrap();
    // 没配状态库 → 报「缺哪个配置」，不静默成功
    let out = run_in(dir.path(), &["push", "--batch", "b1"]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("CONFIG_MISSING"),
        "应给出机器可读错误码：{}",
        err
    );
    assert!(
        err.contains("REGVAL_STATE_DB"),
        "应点名缺哪个配置项：{}",
        err
    );

    // 状态库在、批次不在 → 报「库里没有这一批」，同样不静默成功
    let state_db = dir.path().join("state.db");
    let out = run_in(
        dir.path(),
        &[
            "push",
            "--batch",
            "不存在的批次",
            "--state-db",
            state_db.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("EMPTY_TABLE"),
        "应给出机器可读错误码：{}",
        err
    );
}

#[test]
fn 规则命令列出内置规则及其依据() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_in(dir.path(), &["rules"]);
    assert!(out.status.success());
    let text = stdout_of(&out);
    assert!(text.contains("R-REQUIRED-001"));
    assert!(text.contains("依据："), "规则应带官方依据：{}", text);
}
