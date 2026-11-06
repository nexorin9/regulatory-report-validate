//! `regval doctor`：运行环境与凭据的逐项自检。
//!
//! 信息科在工位上装引擎时，最需要知道的是**缺哪一项、怎么补**，
//! 所以本模块逐项给结论（正常 / 提示 / 缺失），不只输出一句「OK」。
//!
//! 输入全部走结构体参数（不直接读进程环境），单测因此能构造「齐备」与「缺失」
//! 两种情形分别验证，不用改进程环境。

use std::path::{Path, PathBuf};

use crate::config::{mask, spec_of, PartialConfig, VarSpec, VAR_SPECS};

/// 一项检查的结论
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 正常
    Ok,
    /// 提示（不影响主路径，但需提醒）
    Warn,
    /// 缺失（该项不可用）
    Missing,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "正常",
            Status::Warn => "提示",
            Status::Missing => "缺失",
        }
    }

    /// 终端里的标记（等宽对齐用）
    pub fn marker(self) -> &'static str {
        match self {
            Status::Ok => "[正常]",
            Status::Warn => "[提示]",
            Status::Missing => "[缺失]",
        }
    }
}

/// 一项检查结果
#[derive(Debug, Clone)]
pub struct DoctorItem {
    /// 分类：工具链 / 状态库 / 规则目录 / 配置项
    pub category: &'static str,
    /// 项名（配置项检查时是变量名）
    pub name: String,
    /// 结论
    pub status: Status,
    /// 结论说明
    pub detail: String,
    /// 补法（缺失或提示时给出）
    pub remedy: Option<String>,
}

/// 自检输入
pub struct DoctorInput<'a> {
    /// 已读到的配置（只含非空值）
    pub config: &'a PartialConfig,
    /// 状态库路径覆盖（`--state-db` 优先于环境变量）
    pub state_db_override: Option<PathBuf>,
    /// PATH（用于查 cargo）
    pub path_env: Option<String>,
    /// cargo 主目录（`CARGO_HOME` 或 `~/.cargo`）
    pub cargo_home: Option<PathBuf>,
}

/// 自检报告
#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub items: Vec<DoctorItem>,
}

impl DoctorReport {
    /// 某一类结论的条数
    pub fn count(&self, status: Status) -> usize {
        self.items.iter().filter(|i| i.status == status).count()
    }

    /// 是否没有缺失项（提示不算缺失）
    pub fn is_ready(&self) -> bool {
        self.count(Status::Missing) == 0
    }

    /// 某一分类下的项
    pub fn of_category(&self, category: &str) -> Vec<&DoctorItem> {
        self.items
            .iter()
            .filter(|i| i.category == category)
            .collect()
    }

    /// 可读输出（逐项列出，不只输出一句结论）
    pub fn render(&self, stamp: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "regval doctor · 运行环境与凭据逐项自检 · {}\n",
            stamp
        ));
        for item in &self.items {
            out.push_str(&format!(
                "{} {:<6} {} — {}\n",
                item.status.marker(),
                item.category,
                item.name,
                item.detail
            ));
            if let Some(remedy) = &item.remedy {
                out.push_str(&format!("       ↳ 补法：{}\n", remedy));
            }
        }
        out.push_str(&format!(
            "结论：{} 项正常 · {} 项提示 · {} 项缺失",
            self.count(Status::Ok),
            self.count(Status::Warn),
            self.count(Status::Missing)
        ));
        if self.is_ready() {
            out.push_str("（无缺失项，可开始校验）");
        } else {
            out.push_str("（先补齐缺失项再上报；补法见每项下方）");
        }
        out.push('\n');
        out
    }
}

/// 跑一遍自检。
pub fn run(input: &DoctorInput) -> DoctorReport {
    let mut items = Vec::new();

    items.push(check_toolchain(input));
    items.push(check_state_db(input));
    items.push(check_rule_dir(input));
    for spec in VAR_SPECS.iter() {
        items.push(check_var(input.config, spec));
    }

    DoctorReport { items }
}

/// 工具链：cargo 在不在 PATH 里。
///
/// 院内机器与 CI 镜像常见「cargo 已装但不在默认 PATH」，
/// 引擎装不上时第一句要给的提示就是这一条。
fn check_toolchain(input: &DoctorInput) -> DoctorItem {
    let cargo_in_path = find_in_path("cargo", input.path_env.as_deref());
    let cargo_home_bin = input
        .cargo_home
        .as_ref()
        .map(|h| h.join("bin"))
        .filter(|p| p.is_dir());

    match (cargo_in_path, cargo_home_bin) {
        (Some(p), _) => DoctorItem {
            category: "工具链",
            name: "cargo".to_string(),
            status: Status::Ok,
            detail: format!("cargo 已就绪：{}", p.display()),
            remedy: None,
        },
        (None, Some(bin)) => DoctorItem {
            category: "工具链",
            name: "cargo".to_string(),
            status: Status::Warn,
            detail: format!("PATH 里没有 cargo，但 {} 存在", bin.display()),
            remedy: Some(format!(
                "构建前执行：export PATH=\"$HOME/.cargo/bin:$PATH\"（本机目录 {}）",
                bin.display()
            )),
        },
        (None, None) => DoctorItem {
            category: "工具链",
            name: "cargo".to_string(),
            status: Status::Missing,
            detail: "PATH 里没有 cargo，且未找到 cargo 主目录".to_string(),
            remedy: Some("安装 Rust 工具链后把 ~/.cargo/bin 加入 PATH".to_string()),
        },
    }
}

/// 状态库：路径在哪儿、能不能写。
fn check_state_db(input: &DoctorInput) -> DoctorItem {
    let path = input
        .state_db_override
        .clone()
        .or_else(|| input.config.get("REGVAL_STATE_DB").map(PathBuf::from));

    let path = match path {
        Some(p) => p,
        None => {
            return DoctorItem {
                category: "状态库",
                name: "REGVAL_STATE_DB".to_string(),
                status: Status::Missing,
                detail: "未指定状态库路径（环境变量 REGVAL_STATE_DB 或 --state-db 都未给）"
                    .to_string(),
                remedy: Some("在 .env 里设置 REGVAL_STATE_DB=./.regval/state.db".to_string()),
            }
        }
    };

    if path.exists() {
        let readonly = std::fs::metadata(&path)
            .map(|m| m.permissions().readonly())
            .unwrap_or(false);
        return if readonly {
            DoctorItem {
                category: "状态库",
                name: path.display().to_string(),
                status: Status::Warn,
                detail: "状态库文件只读，写入会失败".to_string(),
                remedy: Some("去掉文件只读属性，或换一个可写路径".to_string()),
            }
        } else {
            DoctorItem {
                category: "状态库",
                name: path.display().to_string(),
                status: Status::Ok,
                detail: "状态库已存在且可写".to_string(),
                remedy: None,
            }
        };
    }

    // 还没建：看所在目录能不能写
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    if !dir.exists() {
        return DoctorItem {
            category: "状态库",
            name: path.display().to_string(),
            status: Status::Warn,
            detail: format!(
                "状态库上级目录尚不存在（{}），首次运行时会自动创建",
                dir.display()
            ),
            remedy: Some("确认路径写法正确，或手工建好目录".to_string()),
        };
    }
    if dir_writable(&dir) {
        DoctorItem {
            category: "状态库",
            name: path.display().to_string(),
            status: Status::Ok,
            detail: format!("所在目录可写（{}），首次运行会新建状态库", dir.display()),
            remedy: None,
        }
    } else {
        DoctorItem {
            category: "状态库",
            name: path.display().to_string(),
            status: Status::Missing,
            detail: format!("所在目录不可写：{}", dir.display()),
            remedy: Some("换一个可写目录，或修正目录权限".to_string()),
        }
    }
}

/// 规则目录：在不在、是不是目录。
fn check_rule_dir(input: &DoctorInput) -> DoctorItem {
    let raw = match input.config.get("REGVAL_RULE_DIR") {
        Some(v) => v.to_string(),
        None => {
            return DoctorItem {
                category: "规则目录",
                name: "REGVAL_RULE_DIR".to_string(),
                status: Status::Missing,
                detail: "未设置规则目录（当前仍使用内置规则集）".to_string(),
                remedy: Some(
                    "在 .env 里设置 REGVAL_RULE_DIR=./rules 并放入版本化规则表".to_string(),
                ),
            }
        }
    };
    let path = PathBuf::from(&raw);
    if path.is_dir() {
        let count = std::fs::read_dir(&path).map(|it| it.count()).unwrap_or(0);
        DoctorItem {
            category: "规则目录",
            name: path.display().to_string(),
            status: Status::Ok,
            detail: format!("规则目录存在（{} 个条目）", count),
            remedy: None,
        }
    } else {
        DoctorItem {
            category: "规则目录",
            name: path.display().to_string(),
            status: Status::Missing,
            detail: "规则目录不存在或不是目录".to_string(),
            remedy: Some("创建该目录，或修正 REGVAL_RULE_DIR 指向已有目录".to_string()),
        }
    }
}

/// 单个配置项：设了没有、值对不对。
fn check_var(config: &PartialConfig, spec: &VarSpec) -> DoctorItem {
    match config.get(spec.name) {
        Some(v) => {
            let detail = if spec.secret { mask(v) } else { v.to_string() };
            DoctorItem {
                category: "配置项",
                name: spec.name.to_string(),
                status: Status::Ok,
                detail: format!("已设置：{}", detail),
                remedy: None,
            }
        }
        None => {
            let needed_by = if spec.required_by.is_empty() {
                "全部子命令".to_string()
            } else {
                format!("{} 子命令", spec.required_by.join(" / "))
            };
            DoctorItem {
                category: "配置项",
                name: spec.name.to_string(),
                status: Status::Missing,
                detail: format!("未设置 —— {}（{} 需要）", spec.purpose, needed_by),
                remedy: Some(format!(
                    "在 .env 里填写 {}（值取自 .env.example）",
                    spec.name
                )),
            }
        }
    }
}

/// 在 PATH 里找可执行文件
fn find_in_path(name: &str, path_env: Option<&str>) -> Option<PathBuf> {
    let path_env = path_env?;
    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 目录是否可写（写一个探针文件再删掉，比只看权限位可靠）
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".regval-doctor-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 变量名是否登记在册（doctor 与配置层共用同一份清单）
pub fn is_registered(name: &str) -> bool {
    spec_of(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MapEnv;

    fn cargo_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("regval-doctor-cargo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("cargo");
        std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755));
        }
        dir
    }

    #[test]
    fn 全部齐备时逐项列出且无缺失() {
        let dir = tempfile::tempdir().unwrap();
        let rule_dir = dir.path().join("rules");
        std::fs::create_dir_all(&rule_dir).unwrap();
        let mut env = MapEnv::new();
        env.with_all_present();
        env.set(
            "REGVAL_STATE_DB",
            dir.path().join("state.db").to_str().unwrap(),
        );
        env.set("REGVAL_RULE_DIR", rule_dir.to_str().unwrap());
        let cfg = PartialConfig::load(&env);
        let cargo = cargo_dir();

        let report = run(&DoctorInput {
            config: &cfg,
            state_db_override: None,
            path_env: Some(cargo.to_string_lossy().to_string()),
            cargo_home: Some(cargo.clone()),
        });

        assert!(
            report.is_ready(),
            "齐备场景不应有缺失项：{}",
            report.render("t")
        );
        // 逐项列出：工具链 + 状态库 + 规则目录 + 10 个配置项
        assert_eq!(report.items.len(), 3 + VAR_SPECS.len());
        assert_eq!(report.of_category("工具链").len(), 1);
        assert_eq!(report.of_category("配置项").len(), VAR_SPECS.len());
        let rendered = report.render("2026-11-06 08:00:00");
        assert!(rendered.contains("工具链"));
        assert!(rendered.contains("REGULATORY_REPORT_TOKEN"));
        assert!(rendered.contains("结论："));
    }

    #[test]
    fn 缺配置项时点名缺哪几项() {
        let mut env = MapEnv::new();
        env.set("REGULATORY_REPORT_BASE_URL", "https://x.internal");
        let cfg = PartialConfig::load(&env);
        let report = run(&DoctorInput {
            config: &cfg,
            state_db_override: None,
            path_env: Some(String::new()),
            cargo_home: None,
        });
        assert!(!report.is_ready());
        let rendered = report.render("t");
        assert!(rendered.contains("REGULATORY_REPORT_TOKEN"));
        assert!(rendered.contains("HIS_API_TOKEN"));
        assert!(rendered.contains("[缺失]"));
        assert!(rendered.contains("补法："));
        // 已设置的那项不应被报成缺失
        let base = report
            .items
            .iter()
            .find(|i| i.name == "REGULATORY_REPORT_BASE_URL")
            .unwrap();
        assert_eq!(base.status, Status::Ok);
    }

    #[test]
    fn cargo_不在_path时给出路径提示() {
        let mut env = MapEnv::new();
        env.with_all_present();
        let cfg = PartialConfig::load(&env);
        let home = tempfile::tempdir().unwrap();
        // cargo 主目录存在但 bin 子目录不在 PATH 里 —— 提示加 PATH 即可
        std::fs::create_dir_all(home.path().join("bin")).unwrap();
        let report = run(&DoctorInput {
            config: &cfg,
            state_db_override: None,
            path_env: Some(String::new()),
            cargo_home: Some(home.path().to_path_buf()),
        });
        let item = report
            .items
            .iter()
            .find(|i| i.category == "工具链")
            .unwrap();
        assert_eq!(item.status, Status::Warn);
        assert!(item.remedy.as_ref().unwrap().contains("export PATH"));
    }

    #[test]
    fn 敏感项在输出里只给长度() {
        let mut env = MapEnv::new();
        env.with_all_present();
        env.set("REGULATORY_REPORT_TOKEN", "super-secret-token-value");
        let cfg = PartialConfig::load(&env);
        let report = run(&DoctorInput {
            config: &cfg,
            state_db_override: None,
            path_env: Some(String::new()),
            cargo_home: None,
        });
        let rendered = report.render("t");
        assert!(!rendered.contains("super-secret-token-value"));
        assert!(rendered.contains("已设置，长度 24"));
    }
}
