//! 环境变量配置。
//!
//! 凭据一律从环境变量读，程序内不硬编码、不落盘。
//! 缺项时给明确错误码（`CONFIG_MISSING`）并**点名缺哪几项**，不用默认值兜底 ——
//! 凭据缺失时宁可早点报错，也不让程序带着假地址去连院内系统。
//!
//! 取值来源抽象成 [`EnvSource`]：生产走 [`ProcessEnv`]，单测注入 [`MapEnv`]，
//! 单测因此不必改进程环境（并行跑不互相污染）。
//!
//! 本机另有一份 `.env`（从 `.env.example` 复制而来）供本机开发使用：
//! [`load_dotenv`] 只在**进程环境里没有该变量时**补上 `.env` 的取值，进程环境优先。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::errors::{EngineError, ErrorCode};

/// 一个配置项的登记信息：变量名、用途、是否敏感、哪些子命令需要它。
#[derive(Debug, Clone, Copy)]
pub struct VarSpec {
    /// 环境变量名
    pub name: &'static str,
    /// 用途（给人看：doctor 输出与 README 配置表同源）
    pub purpose: &'static str,
    /// 是否敏感（敏感项在 doctor 输出里掩码）
    pub secret: bool,
    /// 哪些子命令需要这一项
    pub required_by: &'static [&'static str],
}

/// 全部配置项 —— 单一真源。
///
/// doctor 的逐项检查、README 的配置表、缺项错误提示都从这里取，
/// 避免「文档写了一套、程序读的是另一套」。
pub const VAR_SPECS: &[VarSpec] = &[
    VarSpec {
        name: "REGULATORY_REPORT_BASE_URL",
        purpose: "院内卫健上报网关根地址（出站写拼路径用）",
        secret: false,
        required_by: &["push"],
    },
    VarSpec {
        name: "REGULATORY_REPORT_TOKEN",
        purpose: "上报网关 Bearer 凭据",
        secret: true,
        required_by: &["push"],
    },
    VarSpec {
        name: "HIS_BASE_URL",
        purpose: "HIS 科室主数据目录地址（出站读；配了才核对科室主数据，不配则结果标「科室主数据未校验」）",
        secret: false,
        required_by: &["validate", "push", "dept"],
    },
    VarSpec {
        name: "HIS_API_TOKEN",
        purpose: "HIS 目录 Bearer 凭据",
        secret: true,
        required_by: &["validate", "push", "dept"],
    },
    VarSpec {
        name: "OPENAI_BASE_URL",
        purpose:
            "模型端点根地址（语义归并与差异说明草稿用；validate 未接或不可达时降级为主路径照跑）",
        secret: false,
        required_by: &["validate", "merge"],
    },
    VarSpec {
        name: "OPENAI_API_KEY",
        purpose: "模型端点凭据",
        secret: true,
        required_by: &["validate", "merge"],
    },
    VarSpec {
        name: "OPENAI_MODEL",
        purpose: "模型名",
        secret: false,
        required_by: &["validate", "merge"],
    },
    VarSpec {
        name: "OPENAI_TIMEOUT_SECS",
        purpose: "模型调用超时（秒）",
        secret: false,
        required_by: &["validate", "merge"],
    },
    VarSpec {
        name: "REGVAL_STATE_DB",
        purpose: "本机校验库文件路径（行列结论、确认、回执、审计链）",
        secret: false,
        required_by: &["validate", "status", "push", "merge", "export", "replay"],
    },
    VarSpec {
        name: "REGVAL_RULE_DIR",
        purpose: "口径规则目录（版本化规则表 + 27 项指标口径种子）",
        secret: false,
        required_by: &["rules"],
    },
];

/// 按变量名查登记信息
pub fn spec_of(name: &str) -> Option<&'static VarSpec> {
    VAR_SPECS.iter().find(|s| s.name == name)
}

/// 环境变量取值来源。
pub trait EnvSource {
    /// 取一个变量的原始值（未设置返回 `None`）
    fn get(&self, key: &str) -> Option<String>;
}

/// 进程环境
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// 内存里的变量表（单测注入用，不动进程环境）
#[derive(Debug, Clone, Default)]
pub struct MapEnv {
    values: BTreeMap<String, String>,
}

impl MapEnv {
    pub fn new() -> Self {
        MapEnv::default()
    }

    /// 放入一项（空串视为未设置，与进程环境语义一致）
    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.values.insert(key.to_string(), value.to_string());
        self
    }

    /// 放入全部必需变量（doctor 的「齐备」场景用；取值一律是占位值，不含真实凭据）
    pub fn with_all_present(&mut self) -> &mut Self {
        for spec in VAR_SPECS.iter() {
            let placeholder = match spec.name {
                "REGVAL_STATE_DB" => "./.regval/state.db",
                "REGVAL_RULE_DIR" => "./rules",
                "OPENAI_MODEL" => "placeholder-model",
                "OPENAI_TIMEOUT_SECS" => "20",
                _ => {
                    if spec.secret {
                        "placeholder-token"
                    } else {
                        "https://placeholder.internal.example"
                    }
                }
            };
            self.values
                .insert(spec.name.to_string(), placeholder.to_string());
        }
        self
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

/// 部分配置：只记「已设置且非空」的项。
///
/// 空串与纯空白一律按**未设置**处理 —— 半填的凭据比没填更危险。
#[derive(Debug, Clone, Default)]
pub struct PartialConfig {
    present: BTreeMap<String, String>,
}

impl PartialConfig {
    /// 从来源读取全部配置项（只留非空值）
    pub fn load(src: &dyn EnvSource) -> PartialConfig {
        let mut present = BTreeMap::new();
        for spec in VAR_SPECS.iter() {
            if let Some(v) = src.get(spec.name) {
                if !v.trim().is_empty() {
                    present.insert(spec.name.to_string(), v.trim().to_string());
                }
            }
        }
        PartialConfig { present }
    }

    /// 某项是否已设置
    pub fn is_set(&self, name: &str) -> bool {
        self.present.contains_key(name)
    }

    /// 某项的取值（未设置返回 `None`）
    pub fn get(&self, name: &str) -> Option<&str> {
        self.present.get(name).map(|s| s.as_str())
    }

    /// 给定的一组变量里，缺哪些（返回登记信息，顺序与 `VAR_SPECS` 一致）
    pub fn missing(&self, names: &[&str]) -> Vec<&'static VarSpec> {
        VAR_SPECS
            .iter()
            .filter(|s| names.contains(&s.name) && !self.is_set(s.name))
            .collect()
    }

    /// 要求给定的一组变量齐备；缺则报 `CONFIG_MISSING` 并点名缺哪几项
    pub fn require(&self, names: &[&str]) -> Result<(), EngineError> {
        let missing = self.missing(names);
        if missing.is_empty() {
            return Ok(());
        }
        Err(missing_error(&missing))
    }

    /// 要求全部配置项齐备
    pub fn require_all(&self) -> Result<(), EngineError> {
        let all: Vec<&str> = VAR_SPECS.iter().map(|s| s.name).collect();
        self.require(&all)
    }
}

/// 可选调参项的取值：**不是凭据**，因此不进 [`VAR_SPECS`]（不设不算缺失）。
///
/// 未设置 → `Ok(None)`；设置了但不是非负整数 → 报错并点名变量
/// （`CONFIG_INVALID`）—— 阈值写成 `abc` 却静默用默认值，比报错难查得多。
pub fn optional_usize(src: &dyn EnvSource, name: &str) -> Result<Option<usize>, EngineError> {
    match src.get(name) {
        None => Ok(None),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => raw.trim().parse::<usize>().map(Some).map_err(|_| {
            EngineError::at(
                ErrorCode::ConfigInvalid,
                format!("环境变量 {} 不是合法的非负整数：{}", name, raw.trim()),
                name.to_string(),
            )
        }),
    }
}

/// 缺项错误：把「缺哪个变量」与「这个变量干什么用」一起给出。
fn missing_error(missing: &[&'static VarSpec]) -> EngineError {
    let detail = missing
        .iter()
        .map(|s| format!("{}（{}）", s.name, s.purpose))
        .collect::<Vec<_>>()
        .join("、");
    EngineError::at(
        ErrorCode::ConfigMissing,
        format!("缺少必需的环境变量：{}", detail),
        missing
            .iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// 全量配置：10 项都齐备时才有。
#[derive(Debug, Clone)]
pub struct Config {
    pub gateway_base_url: String,
    pub gateway_token: String,
    pub his_base_url: String,
    pub his_token: String,
    pub model_base_url: String,
    pub model_api_key: String,
    pub model_name: String,
    pub model_timeout_secs: u64,
    pub state_db: PathBuf,
    pub rule_dir: PathBuf,
}

impl Config {
    /// 严格装载：任何一项缺失或取值不合法都报错，不填默认值。
    pub fn load(src: &dyn EnvSource) -> Result<Config, EngineError> {
        let partial = PartialConfig::load(src);
        partial.require_all()?;
        Config::from_partial(&partial)
    }

    /// 由已齐备的部分配置装配出强类型配置。
    pub fn from_partial(partial: &PartialConfig) -> Result<Config, EngineError> {
        let timeout_raw = partial
            .get("OPENAI_TIMEOUT_SECS")
            .ok_or_else(|| missing_error(&[spec_of("OPENAI_TIMEOUT_SECS").expect("变量已登记")]))?;
        let timeout_secs = timeout_raw.parse::<u64>().map_err(|_| {
            EngineError::at(
                ErrorCode::ConfigInvalid,
                format!("环境变量 OPENAI_TIMEOUT_SECS 不是合法整数：{}", timeout_raw),
                "OPENAI_TIMEOUT_SECS",
            )
        })?;

        Ok(Config {
            gateway_base_url: partial
                .get("REGULATORY_REPORT_BASE_URL")
                .unwrap_or("")
                .to_string(),
            gateway_token: partial
                .get("REGULATORY_REPORT_TOKEN")
                .unwrap_or("")
                .to_string(),
            his_base_url: partial.get("HIS_BASE_URL").unwrap_or("").to_string(),
            his_token: partial.get("HIS_API_TOKEN").unwrap_or("").to_string(),
            model_base_url: partial.get("OPENAI_BASE_URL").unwrap_or("").to_string(),
            model_api_key: partial.get("OPENAI_API_KEY").unwrap_or("").to_string(),
            model_name: partial.get("OPENAI_MODEL").unwrap_or("").to_string(),
            model_timeout_secs: timeout_secs,
            state_db: PathBuf::from(partial.get("REGVAL_STATE_DB").unwrap_or("")),
            rule_dir: PathBuf::from(partial.get("REGVAL_RULE_DIR").unwrap_or("")),
        })
    }
}

/// 读 `.env` 文件，把**进程环境里没有**的键补上。返回补上的键数。
///
/// 格式：`KEY=VALUE`，`#` 开头为注释，空行忽略；值可带引号（引号一并去掉）。
/// 进程环境优先于 `.env`（CI / 计划任务注入的环境变量不会被文件覆盖）。
pub fn load_dotenv(path: &Path) -> std::io::Result<usize> {
    let content = std::fs::read_to_string(path)?;
    let mut loaded = 0usize;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if std::env::var_os(key).is_some() {
            continue;
        }
        std::env::set_var(key, value);
        loaded += 1;
    }
    Ok(loaded)
}

/// 敏感项的掩码显示：只给长度，不给内容。
pub fn mask(value: &str) -> String {
    let n = value.chars().count();
    if n == 0 {
        "（空）".to_string()
    } else {
        format!("（已设置，长度 {}）", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缺项时报错并点名缺哪几项() {
        let mut env = MapEnv::new();
        env.set("REGULATORY_REPORT_BASE_URL", "https://x.internal");
        let err = Config::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigMissing);
        assert!(err.message.contains("REGULATORY_REPORT_TOKEN"));
        assert!(err.message.contains("REGULATORY_REPORT_BASE_URL") == false);
        assert!(err.location.unwrap().contains("REGULATORY_REPORT_TOKEN"));
    }

    #[test]
    fn 空串按未设置处理() {
        let mut env = MapEnv::new();
        env.with_all_present();
        env.set("HIS_API_TOKEN", "   ");
        let err = Config::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigMissing);
        assert!(err.message.contains("HIS_API_TOKEN"));
    }

    #[test]
    fn 全部齐备时装配出强类型配置() {
        let mut env = MapEnv::new();
        env.with_all_present();
        let cfg = Config::load(&env).unwrap();
        assert_eq!(cfg.model_timeout_secs, 20);
        assert_eq!(cfg.state_db, PathBuf::from("./.regval/state.db"));
        assert_eq!(cfg.rule_dir, PathBuf::from("./rules"));
    }

    #[test]
    fn 超时取值不合法时报配置错误() {
        let mut env = MapEnv::new();
        env.with_all_present();
        env.set("OPENAI_TIMEOUT_SECS", "abc");
        let err = Config::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }

    #[test]
    fn dotenv_只补进程环境里没有的键() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(
            &env_path,
            "# 注释\nREGVAL_STATE_DB=./.regval/state.db\n\nHIS_BASE_URL='https://his.internal.example'\n",
        )
        .unwrap();
        let loaded = load_dotenv(&env_path).unwrap();
        assert_eq!(loaded, 2);
        assert_eq!(
            std::env::var("REGVAL_STATE_DB").ok().as_deref(),
            Some("./.regval/state.db")
        );
        assert_eq!(
            std::env::var("HIS_BASE_URL").ok().as_deref(),
            Some("https://his.internal.example")
        );
        // 清理：避免影响同进程内其他用例
        std::env::remove_var("REGVAL_STATE_DB");
        std::env::remove_var("HIS_BASE_URL");
    }

    #[test]
    fn 敏感项掩码不给内容() {
        let m = mask("abcdef123456");
        assert!(!m.contains("abcdef"));
        assert!(m.contains("12"));
    }
}
