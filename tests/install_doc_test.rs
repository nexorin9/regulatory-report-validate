//! 院内离线安装与运维说明（交付任务）的验收。
//!
//! 这一组用例守的是**「信息科照着文档能装起来」**这件事：
//!
//! 1. **文档覆盖五项**：离线构建 / 院内安装 / 宏导入与权限 / 运维清单 / 最小验证，
//!    少一项就有一类操作没法做。
//! 2. **文档与程序同源**：文档里写的每个错误码、每个配置项、每个子命令，
//!    都必须真的在 `src/errors.rs` / `src/config.rs` / CLI 里存在 ——
//!    文档写错了、程序改了，两边对不上是最常见的运维事故。
//! 3. **脚本是真的**：离线构建脚本要真的写出 `cargo vendor` 与 `cargo build --offline`
//!    这两条命令，打包脚本要真的把交付物列全。
//! 4. **最小验证跑得通**：文档里承诺「2 分钟跑出可核对结果」的那条命令，
//!    这里真的跑一遍，核对行数、问题数与口径版本。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn install_doc() -> String {
    let path = project_dir().join("docs/install.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到 docs/install.md：{}", e))
}

fn script(name: &str) -> String {
    let path = project_dir().join("scripts").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到 scripts/{}：{}", name, e))
}

/// 跑一次 regval：清掉全部业务环境变量，工作目录指到临时目录。
fn run_in(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
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
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .env_remove("REGVAL_LOG_LEVEL");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("regval 应可执行")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ---------------------------------------------------------------------
// 1. 文档覆盖五项
// ---------------------------------------------------------------------

#[test]
fn 安装文档存在且覆盖交付要求的五项() {
    let doc = install_doc();
    assert!(
        doc.chars().count() > 4000,
        "docs/install.md 篇幅太薄：{} 字符",
        doc.chars().count()
    );

    // 五项交付要求，逐项要能在文档里找到对应小节
    let required: [(&str, &str); 5] = [
        ("离线构建", "离线构建"),
        ("院内安装", "院内安装说明"),
        ("宏导入与权限", "宏导入与权限说明"),
        ("运维清单", "运维清单"),
        ("最小可运行验证", "最小可运行验证"),
    ];
    for (name, marker) in required {
        assert!(
            doc.contains(marker),
            "docs/install.md 里找不到「{}」这一项（关键字 {}）",
            name,
            marker
        );
    }

    // 分节写，不是坨
    let h2 = doc.lines().filter(|l| l.starts_with("## ")).count();
    let h3 = doc.lines().filter(|l| l.starts_with("### ")).count();
    assert!(h2 >= 6, "docs/install.md 的 ## 章节只有 {} 个，应不少于 6", h2);
    assert!(h3 >= 20, "docs/install.md 的 ### 子节只有 {} 个，应不少于 20", h3);

    // 可比较的信息要进表格
    let table_rows = doc.lines().filter(|l| l.starts_with("|")).count();
    assert!(
        table_rows >= 30,
        "docs/install.md 的表格行只有 {} 行，应不少于 30",
        table_rows
    );
}

#[test]
fn 安装文档写清了离线构建的完整链路() {
    let doc = install_doc();
    for needle in [
        "cargo vendor",
        "cargo build --release --offline",
        ".cargo/config.toml",
        "vendor/",
        "Cargo.lock",
        "rustup target add x86_64-pc-windows-msvc",
    ] {
        assert!(
            doc.contains(needle),
            "docs/install.md 里找不到「{}」—— 离线构建链路写漏了",
            needle
        );
    }
}

#[test]
fn 安装文档写清了院内安装的路径与注入方式() {
    let doc = install_doc();
    for needle in [
        "REGVAL_STATE_DB",
        "REGVAL_RULE_DIR",
        "REGULATORY_REPORT_BASE_URL",
        "REGULATORY_REPORT_TOKEN",
        "HIS_BASE_URL",
        "OPENAI_BASE_URL",
        "REGVAL_ENGINE_PATH",
        "Program Files",
    ] {
        assert!(
            doc.contains(needle),
            "docs/install.md 里找不到「{}」—— 院内安装说明写漏了",
            needle
        );
    }
    // 环境变量注入方式至少写了三种（.env / 系统环境变量 / 计划任务）
    assert!(doc.contains(".env"), "没写 .env 注入方式");
    assert!(doc.contains("系统环境变量"), "没写系统环境变量注入方式");
    assert!(doc.contains("计划任务"), "没写计划任务注入方式");
}

#[test]
fn 安装文档写清了宏的启用步骤与版本对齐() {
    let doc = install_doc();
    for needle in [
        "RegVal.bas",
        "RegValTokens.bas",
        "RegValMessages.bas",
        "regval.js",
        "信任中心",
        "受信任位置",
        "regval.contract/1.0.0",
        "CONTRACT_VERSION_MISMATCH",
    ] {
        assert!(
            doc.contains(needle),
            "docs/install.md 里找不到「{}」—— 宏导入与版本对齐写漏了",
            needle
        );
    }
    // 三个模块必须都提到，少一个导入就编译不过
    assert_eq!(
        doc.matches("RegValTokens.bas").count() >= 1,
        true,
        "没写清楚要导入样式 token 模块"
    );
}

#[test]
fn 安装文档写清了运维清单的三件事() {
    let doc = install_doc();
    for needle in [
        "regval doctor",
        "日志位置",
        "保留策略",
        "regval recover",
        "regval audit",
    ] {
        assert!(
            doc.contains(needle),
            "docs/install.md 里找不到「{}」—— 运维清单写漏了",
            needle
        );
    }
    // 错误码处理动作：至少要覆盖常见几类
    for code in [
        "CONFIG_MISSING",
        "CONTRACT_VERSION_MISMATCH",
        "UNCONFIRMED_ROW",
        "GATEWAY_REJECTED",
        "DENOMINATOR_ZERO",
        "MODEL_UNAVAILABLE",
        "DB_CORRUPTED",
        "DEPT_NOT_FOUND",
    ] {
        assert!(
            doc.contains(code),
            "docs/install.md 的错误码表里少了 {}",
            code
        );
    }
}

// ---------------------------------------------------------------------
// 2. 文档与程序同源
// ---------------------------------------------------------------------

#[test]
fn 文档里点名的每一个错误码与配置项都真实存在() {
    let doc = install_doc();

    // 文档里出现的「全大写 + 下划线」token，一律要在程序里找得到：
    // 错误码在 src/errors.rs，登记在册的配置项在 src/config.rs，
    // 可选配置项在 .env.example，宏常量在宏文件里。
    let errors_src = std::fs::read_to_string(project_dir().join("src/errors.rs"))
        .unwrap_or_else(|e| panic!("读不到 src/errors.rs：{}", e));
    let config_src = std::fs::read_to_string(project_dir().join("src/config.rs"))
        .unwrap_or_else(|e| panic!("读不到 src/config.rs：{}", e));
    let env_example = std::fs::read_to_string(project_dir().join(".env.example"))
        .unwrap_or_else(|e| panic!("读不到 .env.example：{}", e));
    let bas = std::fs::read_to_string(project_dir().join("macro/vba/RegVal.bas"))
        .unwrap_or_else(|e| panic!("读不到 macro/vba/RegVal.bas：{}", e));

    // 抓 token：大写字母开头、只含大写字母数字与下划线、至少含一个下划线
    let mut tokens: Vec<String> = Vec::new();
    for raw in doc.split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')) {
        let t = raw.trim_matches('_');
        if t.len() >= 6 && t.contains('_') && !tokens.iter().any(|x| x == t) {
            tokens.push(t.to_string());
        }
    }
    assert!(
        tokens.len() >= 40,
        "文档里只抓到 {} 个全大写 token，解析规则可能失效",
        tokens.len()
    );

    let mut missing: Vec<&String> = Vec::new();
    for token in &tokens {
        let found = errors_src.contains(token.as_str())
            || config_src.contains(token.as_str())
            || env_example.contains(token.as_str())
            || bas.contains(token.as_str());
        if !found {
            missing.push(token);
        }
    }
    assert!(
        missing.is_empty(),
        "docs/install.md 里这几个名字在程序里都找不到（文档与程序对不上）：{:?}",
        missing
    );
}

#[test]
fn 文档里的配置项都在引擎的配置登记表里() {
    let doc = install_doc();
    let config_src = std::fs::read_to_string(project_dir().join("src/config.rs"))
        .unwrap_or_else(|e| panic!("读不到 src/config.rs：{}", e));

    for var in [
        "REGULATORY_REPORT_BASE_URL",
        "REGULATORY_REPORT_TOKEN",
        "HIS_BASE_URL",
        "HIS_API_TOKEN",
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_MODEL",
        "OPENAI_TIMEOUT_SECS",
        "REGVAL_STATE_DB",
        "REGVAL_RULE_DIR",
    ] {
        assert!(
            doc.contains(var),
            "docs/install.md 里没写配置项 {}",
            var
        );
        assert!(
            config_src.contains(var),
            "src/config.rs 的配置登记表里没有 {}",
            var
        );
    }
}

#[test]
fn 文档里的子命令都真的存在() {
    let doc = install_doc();
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
        "recover",
        "audit",
        "confirm",
        "messages",
    ] {
        let out = run_in(dir.path(), &[cmd, "--help"], &[]);
        assert!(
            out.status.success(),
            "regval {} --help 不可用（退出码 {:?}）",
            cmd,
            out.status.code()
        );
        // 文档里引用了这个子命令，文档才不算写过
        assert!(
            doc.contains(cmd),
            "docs/install.md 里没有提到子命令 {}",
            cmd
        );
    }
}

#[test]
fn 文档里的契约版本与引擎一致() {
    let doc = install_doc();
    let contract_src = std::fs::read_to_string(project_dir().join("src/contract.rs"))
        .unwrap_or_else(|e| panic!("读不到 src/contract.rs：{}", e));

    // 契约版本号在文档、契约模块、宏常量三处必须一致
    let version = "regval.contract/1.0.0";
    assert!(doc.contains(version), "docs/install.md 没写契约版本号");
    assert!(
        contract_src.contains(version),
        "src/contract.rs 里找不到契约版本号 {}",
        version
    );
    let bas = std::fs::read_to_string(project_dir().join("macro/vba/RegVal.bas"))
        .unwrap_or_else(|e| panic!("读不到 macro/vba/RegVal.bas：{}", e));
    assert!(
        bas.contains(version),
        "macro/vba/RegVal.bas 里的 REGVAL_SCHEMA_VERSION 与契约版本不一致"
    );
}

// ---------------------------------------------------------------------
// 3. 脚本是真的
// ---------------------------------------------------------------------

#[test]
fn 离线构建脚本写对了关键命令() {
    let s = script("offline-build.sh");
    for needle in [
        "cargo vendor",
        "cargo build --offline",
        ".cargo-checksum.json",
        "Cargo.lock",
        "--check",
        "--vendor",
    ] {
        assert!(
            s.contains(needle),
            "scripts/offline-build.sh 里找不到「{}」",
            needle
        );
    }
    // 脚本必须可执行
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = project_dir().join("scripts/offline-build.sh");
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "scripts/offline-build.sh 没有可执行权限（{:o}）",
            mode
        );
    }
}

#[test]
fn 打包脚本把交付物列全了() {
    let s = script("package-offline.sh");
    for needle in [
        "regval",
        "macro",
        "rules",
        "samples/dataset",
        "docs/install.md",
        ".env.example",
        "SHA256SUMS",
    ] {
        assert!(
            s.contains(needle),
            "scripts/package-offline.sh 里找不到「{}」—— 交付物漏打进包",
            needle
        );
    }
    // 凭据不能进包
    assert!(
        !s.contains("cp .env "),
        "打包脚本里出现了把 .env 打进包的动作"
    );
}

#[test]
fn 依赖目录存在且完整_离线构建的产物齐备() {
    let vendor = project_dir().join("vendor");
    if !vendor.exists() {
        // 依赖目录不进库（250 MB），干净 clone 上没有是正常的；
        // 但脚本必须能把没有它的情形说清楚。
        let s = script("offline-build.sh");
        assert!(
            s.contains("./scripts/offline-build.sh --vendor"),
            "依赖目录不在时，脚本没有告诉人怎么生成"
        );
        return;
    }

    // 有依赖目录时逐个 crate 校验完整性
    let mut total = 0;
    let mut bad = Vec::new();
    for entry in std::fs::read_dir(&vendor).unwrap() {
        let entry = entry.unwrap();
        if !entry.path().is_dir() {
            continue;
        }
        total += 1;
        let sum = entry.path().join(".cargo-checksum.json");
        if !sum.exists() {
            bad.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    assert!(total > 100, "依赖目录里只有 {} 个 crate，明显不完整", total);
    assert!(
        bad.is_empty(),
        "以下 crate 缺 .cargo-checksum.json（拷贝时断过）：{:?}",
        bad
    );

    // Cargo.lock 声明的包数应当比 crate 数多 1（多的是本项目自身）
    let lock = std::fs::read_to_string(project_dir().join("Cargo.lock")).unwrap();
    let locked = lock.matches("[[package]]").count();
    assert_eq!(
        locked,
        total + 1,
        "Cargo.lock 声明 {} 个包，依赖目录 {} 个 crate —— 差值应当正好是 1（本项目自身）",
        locked,
        total
    );
}

// ---------------------------------------------------------------------
// 4. 最小可运行验证跑得通
// ---------------------------------------------------------------------

#[test]
fn 最小验证命令实跑通过并给出可核对结果() {
    let dir = tempfile::tempdir().unwrap();
    let state_db = dir.path().join("state.db");
    let out_path = dir.path().join("result.json");
    let rules = project_dir().join("rules");
    let input = project_dir().join("samples/dataset/合并表.csv");

    assert!(input.exists(), "样例表不在：{}", input.display());

    let out = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            input.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ],
        &[
            ("REGVAL_STATE_DB", state_db.to_str().unwrap()),
            ("REGVAL_RULE_DIR", rules.to_str().unwrap()),
        ],
    );

    assert!(
        out.status.success(),
        "最小验证应退出码 0，实际 {:?}；stderr：{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = stdout_of(&out);
    // 文档里承诺的核对点，逐条对得上
    assert!(
        stdout.contains("已校验 1200 行"),
        "最小验证没有报出「已校验 1200 行」：\n{}",
        stdout
    );
    assert!(
        stdout.contains("口径规则集：版本 2021.1"),
        "最小验证没有报出口径版本：\n{}",
        stdout
    );
    assert!(
        stdout.contains("项疑似重复待确认"),
        "最小验证没有报出疑似重复计数：\n{}",
        stdout
    );

    // 产物是可解析的契约，且行数与终端一致
    let raw = std::fs::read_to_string(&out_path).expect("应产出结果契约");
    let v: Value = serde_json::from_str(&raw).expect("结果契约应可解析");
    assert_eq!(v["schema_version"], "regval.contract/1.0.0");
    assert_eq!(v["rows"].as_array().unwrap().len(), 1200);
    assert!(
        !v["issues"].as_array().unwrap().is_empty(),
        "样例表应查出口径问题"
    );
}

#[test]
fn 最小验证不需要连院内任何系统() {
    // 清掉全部院内地址后仍能跑通 —— 校验环节完全离线
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("result.json");
    let out = run_in(
        dir.path(),
        &[
            "validate",
            "--input",
            project_dir()
                .join("samples/dataset/合并表.csv")
                .to_str()
                .unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ],
        &[(
            "REGVAL_STATE_DB",
            dir.path().join("state.db").to_str().unwrap(),
        )],
    );
    assert!(
        out.status.success(),
        "不配院内地址也应能校验（只标「未校验」）：stderr {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("科室主数据未校验"),
        "不配 HIS 目录时应标「科室主数据未校验」：\n{}",
        stdout
    );
    assert!(
        stdout.contains("模型端点：本轮未接入"),
        "不配模型端点时应标「本轮未接入」：\n{}",
        stdout
    );
}
