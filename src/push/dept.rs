//! 院内 HIS 科室主数据目录出站客户端（正式对接层的第二条出站读）。
//!
//! # 协议（正式对接规范，见 `spec.md`「出站报文（正式对接规范）」）
//!
//! - 路径：`GET {HIS_BASE_URL}/api/v1/dept-directory`
//! - 凭据：`Authorization: Bearer $HIS_API_TOKEN` —— **只从环境变量读**，程序里不硬编码。
//! - 响应：`[{dept_code, dept_name, aliases[], parent_code}]`
//!
//! # 目录拉不到 ≠ 科室没问题
//!
//! 目录拉不到时，本轮**跳过**科室主数据一致性检查，并在结果里标「科室主数据未校验」。
//! 「没查」与「查了、全部通过」在结果里必须能分开 —— 前者不能算成后者。
//! 同理：**空目录绝不当作「目录可用但一个科室都没有」** —— 那会把每一行都报成
//! 「科室主数据不一致」，凭空造出一批假问题。空目录一律按「不可用」处理。
//!
//! # 缓存与数据时间
//!
//! 拉取结果按「机构代码 + 拉取时间」落本机库；拉取失败时用上次缓存，并**标注数据时间**。
//! 缓存超过有效期（`REGVAL_DEPT_DIRECTORY_TTL_SECS`，缺省 1 小时）会被标「已过期」——
//! 仍然用它（一份旧名单比没有名单强），但经办人一眼看得出这份目录有多旧。
//!
//! # 源产品能力 → 融合后行为
//!
//! 参考实现里的**远端连接层**（由根地址拼出端点、`Bearer {token}` 拼出鉴权头）提供的是
//! 「端点 + 凭据」这三件套的组织方式。本产品**复用**这套组织方式，
//! **改写**为院内读接口的固定语义：
//! - 端点固定成一条目录路径（`/api/v1/dept-directory`），不做多端点分派；
//! - 失败按「连不上 / 超时 / 4xx / 5xx / 响应不合法」各归各的机器可读错误码，
//!   **不塌成一个通用错**（这是参考实现里错误码枚举的做法）；
//! - 拉不到就降级（跳过一致性检查 + 标「未校验」），**不静默通过**。
//!
//! 协议兼容替身只出现在 `tests/` 测试数据层，**不是**最终对接方式。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::bind::dept::{DeptDirectory, DeptNameVerdict};
use crate::config::{optional_usize, EnvSource, PartialConfig};
use crate::errors::{EngineError, ErrorCode};

/// 科室目录路径（挂在 HIS 根地址下）
pub const DEPT_DIRECTORY_PATH: &str = "/api/v1/dept-directory";

/// HIS 侧的两个环境变量（缺任一项都不兜底）
pub const HIS_ENV_VARS: [&str; 2] = ["HIS_BASE_URL", "HIS_API_TOKEN"];

/// 目录调用超时（秒）—— 可选调参项，不设就用默认值
pub const HIS_TIMEOUT_ENV: &str = "HIS_TIMEOUT_SECS";

/// 目录调用超时默认值（秒）
pub const DEFAULT_HIS_TIMEOUT_SECS: u64 = 10;

/// 目录缓存有效期（秒）—— 超过即标「已过期」
pub const DEPT_TTL_ENV: &str = "REGVAL_DEPT_DIRECTORY_TTL_SECS";

/// 目录缓存有效期默认值（秒）
pub const DEFAULT_DEPT_TTL_SECS: u64 = 3600;

/// 「科室主数据未校验」——结果契约里必须能搜到的字样。
///
/// 「没核对过」和「核对过、全部通过」在结果里必须分得开；这个字样就是那个分界。
pub const UNCHECKED_MARK: &str = "科室主数据未校验";

/// 目录配置（地址 + 凭据 + 超时，全来自环境变量）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeptDirectoryConfig {
    /// HIS 根地址（末尾斜杠会被去掉）
    pub base_url: String,
    /// Bearer 凭据
    pub token: String,
    /// 超时秒数
    pub timeout_secs: u64,
}

impl DeptDirectoryConfig {
    /// 从环境变量读齐；缺项报 `CONFIG_MISSING` 并点名（**发请求之前**就拦下）
    pub fn load(src: &dyn EnvSource) -> Result<DeptDirectoryConfig, EngineError> {
        let partial = PartialConfig::load(src);
        partial.require(&HIS_ENV_VARS)?;
        let timeout_secs = optional_usize(src, HIS_TIMEOUT_ENV)?
            .unwrap_or(DEFAULT_HIS_TIMEOUT_SECS as usize) as u64;
        Ok(DeptDirectoryConfig {
            base_url: partial
                .get("HIS_BASE_URL")
                .unwrap_or("")
                .trim_end_matches('/')
                .to_string(),
            token: partial.get("HIS_API_TOKEN").unwrap_or("").to_string(),
            timeout_secs,
        })
    }

    /// 目录端点
    pub fn directory_endpoint(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            DEPT_DIRECTORY_PATH
        )
    }
}

/// 目录里的一条科室（响应字段全集：`dept_code` / `dept_name` / `aliases[]` / `parent_code`）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeptEntry {
    /// 科室代码（权威标识）
    pub dept_code: String,
    /// 科室名称（主数据写法）
    #[serde(default)]
    pub dept_name: String,
    /// 别名（医院自定义后缀等合法写法）
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 上级科室代码
    #[serde(default)]
    pub parent_code: String,
}

impl DeptEntry {
    /// 该科室的全部合法写法（主数据名称 + 别名）
    pub fn accepted_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.dept_name.trim().is_empty() {
            out.push(self.dept_name.clone());
        }
        for alias in &self.aliases {
            if !alias.trim().is_empty() {
                out.push(alias.clone());
            }
        }
        out
    }
}

/// 目录快照：条目 + 数据时间（拉取那一刻的时间）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshot {
    /// 科室条目
    pub entries: Vec<DeptEntry>,
    /// 数据时间（`YYYY-MM-DD HH:mm:ss`）
    pub fetched_at: String,
}

impl DirectorySnapshot {
    /// 条目数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 序列化成缓存载荷（存状态库用）
    pub fn to_payload(&self) -> Result<String, EngineError> {
        serde_json::to_string(&self.entries).map_err(|e| {
            EngineError::new(
                ErrorCode::DeptDirectoryResponseInvalid,
                format!("科室目录序列化失败：{}", e),
            )
        })
    }

    /// 从缓存载荷还原条目
    pub fn from_payload(payload: &str) -> Result<Vec<DeptEntry>, EngineError> {
        serde_json::from_str::<Vec<DeptEntry>>(payload).map_err(|e| {
            EngineError::new(
                ErrorCode::DeptDirectoryResponseInvalid,
                format!("科室目录缓存载荷解析失败：{}", e),
            )
        })
    }
}

/// 目录来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySource {
    /// 本轮实时拉取
    Live,
    /// 本轮拉取失败，用的是上次缓存
    Cache,
    /// 拉不到、缓存也没有 —— 不可用
    Unavailable,
}

impl DirectorySource {
    /// 机器可读取值
    pub fn as_str(self) -> &'static str {
        match self {
            DirectorySource::Live => "live",
            DirectorySource::Cache => "cache",
            DirectorySource::Unavailable => "unavailable",
        }
    }

    /// 中文说明
    pub fn zh(self) -> &'static str {
        match self {
            DirectorySource::Live => "实时拉取",
            DirectorySource::Cache => "上次缓存",
            DirectorySource::Unavailable => "不可用",
        }
    }
}

/// 目录解析结果：这一轮拿到的目录是什么样。
#[derive(Debug, Clone)]
pub struct DeptResolution {
    /// 目录（`None` = 不可用，本轮跳过科室主数据一致性检查）
    pub directory: Option<HisDeptDirectory>,
    /// 来源
    pub source: DirectorySource,
    /// 条目数
    pub entry_count: usize,
    /// 数据时间（缓存来源时是那次拉取的时间）
    pub fetched_at: String,
    /// 缓存是否已超过有效期
    pub stale: bool,
    /// 有效期（秒）
    pub ttl_secs: u64,
    /// 拉取失败时的错误码（空串 = 没失败）
    pub error_code: String,
    /// 人话说明（终端与契约里都显示这一句）
    pub note: String,
}

impl DeptResolution {
    /// 目录是否可用（= 本轮真的核对过科室主数据）
    pub fn is_available(&self) -> bool {
        self.directory.is_some()
    }

    /// 未核对时的提示（核对过时为空串）。
    ///
    /// 不可用时这一句里带「科室主数据未校验」——结果里搜得到这个字样，
    /// 就说明这批结论的科室主数据维度**没查**，不能当成查过了。
    pub fn unavailable_note(&self) -> String {
        if self.is_available() {
            return String::new();
        }
        format!("{}（{}）", UNCHECKED_MARK, self.note)
    }
}

/// 科室目录缓存（存取接口；实现在库层 `src/db/dept_cache.rs`）。
///
/// 接口在出站客户端这一层，实现在库层 —— 与模型调用审计同一套路：
/// 客户端不碰数据库，缓存的落盘由库层接管。
pub trait DeptDirectoryCache {
    /// 取上次缓存（按机构代码）
    fn load(&self, org_code: &str) -> Result<Option<CachedDirectory>, EngineError>;
    /// 存本次拉取结果（按机构代码 + 拉取时间）
    fn save(&mut self, org_code: &str, snapshot: &DirectorySnapshot) -> Result<(), EngineError>;
}

/// 一次缓存里的一条
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDirectory {
    /// 机构代码
    pub org_code: String,
    /// 数据时间（那次拉取的时间）
    pub fetched_at: String,
    /// 条目数
    pub entry_count: usize,
    /// 目录载荷（JSON 数组）
    pub payload: String,
}

/// 科室主数据目录出站客户端。
///
/// **只有这一处**出现目录协议细节（路径、头、响应字段、状态码语义）。
pub struct DeptDirectoryClient {
    config: DeptDirectoryConfig,
    http: reqwest::blocking::Client,
}

/// 调试输出**不打印凭据** —— 报错信息会被写进日志与终端。
impl std::fmt::Debug for DeptDirectoryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeptDirectoryClient")
            .field("base_url", &self.config.base_url)
            .field("token", &"（已设置，不显示）")
            .field("timeout_secs", &self.config.timeout_secs)
            .finish()
    }
}

impl DeptDirectoryClient {
    /// 用已装配好的配置建客户端
    pub fn new(config: DeptDirectoryConfig) -> Result<DeptDirectoryClient, EngineError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(1)))
            .build()
            .map_err(|e| {
                EngineError::new(
                    ErrorCode::DeptDirectoryUnavailable,
                    format!("创建 HIS 科室目录 HTTP 客户端失败：{}", e),
                )
            })?;
        Ok(DeptDirectoryClient { config, http })
    }

    /// 从环境变量装配（凭据与超时全在环境变量里；缺项即报错，不发请求）
    pub fn from_env(src: &dyn EnvSource) -> Result<DeptDirectoryClient, EngineError> {
        Ok(DeptDirectoryClient::new(DeptDirectoryConfig::load(src)?)?)
    }

    /// 当前配置（只读）
    pub fn config(&self) -> &DeptDirectoryConfig {
        &self.config
    }

    /// 目录端点
    pub fn endpoint(&self) -> String {
        self.config.directory_endpoint()
    }

    /// 拉一次目录。
    ///
    /// 失败**不静默**：每一类失败各有各的机器可读错误码（连不上 / 超时 / 4xx / 5xx /
    /// 响应不合法），由调用方决定降级还是硬失败。
    pub fn fetch(&self) -> Result<Vec<DeptEntry>, EngineError> {
        let resp = self
            .http
            .get(self.endpoint())
            .bearer_auth(&self.config.token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(map_send_error)?;

        let status = resp.status();
        let text = resp.text().unwrap_or_default();

        if status.is_success() {
            return parse_entries(&text);
        }
        if status.is_client_error() {
            return Err(EngineError::at(
                ErrorCode::DeptDirectoryBadRequest,
                format!(
                    "HIS 科室目录拒绝了请求（HTTP {}）：{}",
                    status.as_u16(),
                    snippet(&text)
                ),
                DEPT_DIRECTORY_PATH,
            ));
        }
        Err(EngineError::at(
            ErrorCode::DeptDirectoryServerError,
            format!(
                "HIS 科室目录返回服务端错误（HTTP {}）：{}",
                status.as_u16(),
                snippet(&text)
            ),
            DEPT_DIRECTORY_PATH,
        ))
    }
}

/// 解析目录响应：必须是一个数组，且每条都有科室代码。
///
/// 缺 `dept_code` 的条目直接报错 —— 拿一条没有代码的记录去比对科室代码，
/// 比不出来却当成「匹配不到」，等于凭空造一条假问题。
fn parse_entries(text: &str) -> Result<Vec<DeptEntry>, EngineError> {
    let entries: Vec<DeptEntry> = serde_json::from_str(text).map_err(|e| {
        EngineError::at(
            ErrorCode::DeptDirectoryResponseInvalid,
            format!("HIS 科室目录响应不是合法的科室数组：{}", e),
            DEPT_DIRECTORY_PATH,
        )
    })?;
    for (i, entry) in entries.iter().enumerate() {
        if entry.dept_code.trim().is_empty() {
            return Err(EngineError::at(
                ErrorCode::DeptDirectoryResponseInvalid,
                format!("HIS 科室目录第 {} 条缺 dept_code（科室代码）", i + 1),
                DEPT_DIRECTORY_PATH,
            ));
        }
    }
    Ok(entries)
}

/// 发送期错误 → 错误码（超时 / 连不上分开，不塌成一个通用错）
fn map_send_error(e: reqwest::Error) -> EngineError {
    if e.is_timeout() {
        EngineError::at(
            ErrorCode::DeptDirectoryTimeout,
            format!("HIS 科室目录调用超时：{}", e),
            DEPT_DIRECTORY_PATH,
        )
    } else if e.is_connect() {
        EngineError::at(
            ErrorCode::DeptDirectoryConnectionFailed,
            format!("HIS 科室目录连接失败：{}", e),
            DEPT_DIRECTORY_PATH,
        )
    } else {
        EngineError::at(
            ErrorCode::DeptDirectoryUnavailable,
            format!("HIS 科室目录调用失败：{}", e),
            DEPT_DIRECTORY_PATH,
        )
    }
}

/// 响应正文摘要（只取前 400 字，够看又不刷屏）
fn snippet(text: &str) -> String {
    text.chars().take(400).collect::<String>().trim().to_string()
}

/// 由目录条目建成的科室目录（实现 [`DeptDirectory`]）。
#[derive(Debug, Clone)]
pub struct HisDeptDirectory {
    by_code: BTreeMap<String, DeptEntry>,
}

impl HisDeptDirectory {
    /// 用目录条目建目录（科室代码去空白后作键；同代码保留第一条）
    pub fn new(entries: Vec<DeptEntry>) -> HisDeptDirectory {
        let mut by_code: BTreeMap<String, DeptEntry> = BTreeMap::new();
        for entry in entries {
            let key = entry.dept_code.trim().to_string();
            if key.is_empty() {
                continue;
            }
            by_code.entry(key).or_insert(entry);
        }
        HisDeptDirectory { by_code }
    }

    /// 条目数
    pub fn len(&self) -> usize {
        self.by_code.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }

    /// 逐条目录（按科室代码排序）
    pub fn entries(&self) -> Vec<&DeptEntry> {
        self.by_code.values().collect()
    }

    /// 取一条科室
    pub fn get(&self, dept_code: &str) -> Option<&DeptEntry> {
        self.by_code.get(dept_code.trim())
    }
}

impl DeptDirectory for HisDeptDirectory {
    fn is_available(&self) -> bool {
        true
    }

    fn contains(&self, dept_code: &str) -> bool {
        self.by_code.contains_key(dept_code.trim())
    }

    /// 科室名称写法判定：主数据名称一致 / 别名一致 / 都不一致（只提示）/ 无从判定
    fn name_verdict(&self, dept_code: &str, dept_name: &str) -> Option<DeptNameVerdict> {
        let entry = self.by_code.get(dept_code.trim())?;
        let want = normalize_dept_name(dept_name);
        if want.is_empty() {
            return None;
        }
        let accepted = entry.accepted_names();
        if accepted.is_empty() {
            // 目录这一条既没有主数据名称也没有别名 —— 无从判定，不硬报
            return None;
        }
        for name in &accepted {
            if normalize_dept_name(name) == want {
                let canonical = !entry.dept_name.trim().is_empty()
                    && normalize_dept_name(&entry.dept_name) == want;
                return Some(if canonical {
                    DeptNameVerdict::Canonical
                } else {
                    DeptNameVerdict::Alias
                });
            }
        }
        Some(DeptNameVerdict::Mismatch {
            canonical: entry.dept_name.clone(),
        })
    }
}

/// 科室名称归一：去空白 + 全角转半角。
///
/// 「检验科（本部）」与「检验科 (本部)」是同一个科室；不归一就会把别名命中
/// 判成「写法不符」，那是**误伤** —— 误伤比漏报更招科室烦。
pub fn normalize_dept_name(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            let code = c as u32;
            if (0xFF01..=0xFF5E).contains(&code) {
                char::from_u32(code - 0xFEE0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// 解析这一轮的科室目录：实时拉取 → 失败退回缓存 → 都没有就不可用。
///
/// `client` 为 `None` 表示**没配** HIS 目录（凭据或地址缺）—— 与「配了但拉不到」
/// 同样按「拿不到目录」处理：先用缓存，没有就标「科室主数据未校验」。
///
/// `cache` 写失败**不阻断**主路径：缓存只是兜底，缓存写不进去不等于目录不可用。
pub fn resolve_directory(
    client: Option<&DeptDirectoryClient>,
    cache: &mut dyn DeptDirectoryCache,
    org_code: &str,
    now: &str,
    ttl_secs: u64,
) -> DeptResolution {
    // 1. 实时拉取
    let mut fetch_error: Option<EngineError> = None;
    if let Some(client) = client {
        match client.fetch() {
            Ok(entries) => {
                let snapshot = DirectorySnapshot {
                    entries: entries.clone(),
                    fetched_at: now.to_string(),
                };
                let _ = cache.save(org_code, &snapshot);
                let count = snapshot.len();
                return DeptResolution {
                    directory: Some(HisDeptDirectory::new(entries)),
                    source: DirectorySource::Live,
                    entry_count: count,
                    fetched_at: now.to_string(),
                    stale: false,
                    ttl_secs,
                    error_code: String::new(),
                    note: format!(
                        "科室主数据已校验 · 目录 {} 条 · 数据时间 {}（实时拉取）",
                        count, now
                    ),
                };
            }
            Err(e) => fetch_error = Some(e),
        }
    }

    // 2. 退回上次缓存（并标注数据时间）
    let cached = cache.load(org_code).ok().flatten();
    if let Some(cached) = cached {
        let entries = DirectorySnapshot::from_payload(&cached.payload).unwrap_or_default();
        if !entries.is_empty() {
            let age = age_secs(&cached.fetched_at, now);
            let stale = age > ttl_secs;
            let count = entries.len();
            let error_code = fetch_error
                .as_ref()
                .map(|e| e.code.as_str().to_string())
                .unwrap_or_default();
            let reason = match &fetch_error {
                Some(e) => format!("目录拉取失败：{}", e.code.as_str()),
                None => "未配置院内 HIS 科室目录".to_string(),
            };
            let note = format!(
                "科室主数据已校验（{}，本轮用上次缓存）· 目录 {} 条 · 数据时间 {}{}",
                reason,
                count,
                cached.fetched_at,
                if stale {
                    format!("（缓存已超过有效期 {} 秒）", ttl_secs)
                } else {
                    String::new()
                }
            );
            return DeptResolution {
                directory: Some(HisDeptDirectory::new(entries)),
                source: DirectorySource::Cache,
                entry_count: count,
                fetched_at: cached.fetched_at.clone(),
                stale,
                ttl_secs,
                error_code,
                note,
            };
        }
    }

    // 3. 拿不到目录：跳过一致性检查，如实标「未校验」
    let reason = match &fetch_error {
        Some(e) => format!("目录拉取失败：{} · {}", e.code.as_str(), e.message),
        None => "未配置院内 HIS 科室目录（HIS_BASE_URL / HIS_API_TOKEN）".to_string(),
    };
    DeptResolution {
        directory: None,
        source: DirectorySource::Unavailable,
        entry_count: 0,
        fetched_at: String::new(),
        stale: false,
        ttl_secs,
        error_code: fetch_error
            .as_ref()
            .map(|e| e.code.as_str().to_string())
            .unwrap_or_default(),
        note: reason,
    }
}

/// 数据时间距今多少秒（解析不了就当 0 —— 解析不了不代表数据是新的，
/// 但也不该把整轮校验打断）
fn age_secs(fetched_at: &str, now: &str) -> u64 {
    let fmt = "%Y-%m-%d %H:%M:%S";
    let Ok(from) = chrono::NaiveDateTime::parse_from_str(fetched_at.trim(), fmt) else {
        return 0;
    };
    let Ok(to) = chrono::NaiveDateTime::parse_from_str(now.trim(), fmt) else {
        return 0;
    };
    let diff = to.signed_duration_since(from);
    if diff.num_seconds() < 0 {
        0
    } else {
        diff.num_seconds() as u64
    }
}

/// 结果契约里的「科室主数据目录」段（追加段：不改写回列与行结果结构，契约版本不升）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeptDirectorySection {
    /// 本轮是否真的核对过科室主数据
    pub checked: bool,
    /// 来源：live / cache / unavailable
    pub source: String,
    /// 来源中文说明
    pub source_zh: String,
    /// 目录条目数
    pub entry_count: usize,
    /// 数据时间
    pub fetched_at: String,
    /// 缓存有效期（秒）
    pub ttl_secs: u64,
    /// 缓存是否已过期
    pub stale: bool,
    /// 拉取失败时的错误码（空串 = 没失败）
    pub error_code: String,
    /// 人话说明
    pub note: String,
}

impl DeptDirectorySection {
    /// 不可用时的缺省段
    pub fn unavailable(reason: &str) -> DeptDirectorySection {
        DeptDirectorySection {
            checked: false,
            source: DirectorySource::Unavailable.as_str().to_string(),
            source_zh: DirectorySource::Unavailable.zh().to_string(),
            entry_count: 0,
            fetched_at: String::new(),
            ttl_secs: DEFAULT_DEPT_TTL_SECS,
            stale: false,
            error_code: String::new(),
            note: reason.to_string(),
        }
    }
}

/// 缺省段 = 「不可用」。
///
/// 契约反序列化时缺这一段（老版本契约文件）就落到这里 —— **缺**也意味着
/// 「这一批的科室维度没查过」，不能默认当成查过了。
impl Default for DeptDirectorySection {
    fn default() -> Self {
        DeptDirectorySection::unavailable(UNCHECKED_MARK)
    }
}

impl From<&DeptResolution> for DeptDirectorySection {
    fn from(res: &DeptResolution) -> Self {
        DeptDirectorySection {
            checked: res.is_available(),
            source: res.source.as_str().to_string(),
            source_zh: res.source.zh().to_string(),
            entry_count: res.entry_count,
            fetched_at: res.fetched_at.clone(),
            ttl_secs: res.ttl_secs,
            stale: res.stale,
            error_code: res.error_code.clone(),
            // 不可用时 note 里带「科室主数据未校验」——契约里搜得到这个字样，
            // 就说明这批结论的科室维度没查过。
            note: if res.is_available() {
                res.note.clone()
            } else {
                res.unavailable_note()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MapEnv;

    fn entry(code: &str, name: &str, aliases: &[&str]) -> DeptEntry {
        DeptEntry {
            dept_code: code.to_string(),
            dept_name: name.to_string(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            parent_code: String::new(),
        }
    }

    #[test]
    fn 端点按his根地址拼出固定路径() {
        let cfg = DeptDirectoryConfig {
            base_url: "https://his.internal.example/".to_string(),
            token: "t".to_string(),
            timeout_secs: 5,
        };
        assert_eq!(
            cfg.directory_endpoint(),
            "https://his.internal.example/api/v1/dept-directory"
        );
    }

    #[test]
    fn 缺凭据时报错并点名且不发请求() {
        let mut env = MapEnv::new();
        env.set("HIS_BASE_URL", "https://his.internal.example");
        let err = DeptDirectoryConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigMissing);
        assert!(err.message.contains("HIS_API_TOKEN"));
        assert!(err.location.unwrap().contains("HIS_API_TOKEN"));
    }

    #[test]
    fn 超时取值不合法时报配置错误() {
        let mut env = MapEnv::new();
        env.set("HIS_BASE_URL", "https://his.internal.example");
        env.set("HIS_API_TOKEN", "t");
        env.set(HIS_TIMEOUT_ENV, "soon");
        let err = DeptDirectoryConfig::load(&env).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }

    #[test]
    fn 目录里没有就查不到() {
        let dir = HisDeptDirectory::new(vec![entry("0301", "检验科", &[])]);
        assert!(dir.contains("0301"));
        assert!(!dir.contains("0999"));
    }

    #[test]
    fn 名称一致与别名命中分别判定() {
        let dir = HisDeptDirectory::new(vec![entry("0301", "检验科", &["检验科（本部）"])]);
        assert_eq!(
            dir.name_verdict("0301", "检验科"),
            Some(DeptNameVerdict::Canonical)
        );
        assert_eq!(
            dir.name_verdict("0301", "检验科（本部）"),
            Some(DeptNameVerdict::Alias)
        );
        assert_eq!(
            dir.name_verdict("0301", "随便写的"),
            Some(DeptNameVerdict::Mismatch {
                canonical: "检验科".to_string()
            })
        );
    }

    #[test]
    fn 名称归一后全角半角与空格写法不误伤() {
        let dir = HisDeptDirectory::new(vec![entry("0301", "检验科", &["检验科（本部）"])]);
        assert_eq!(
            dir.name_verdict("0301", "检验科 (本部)"),
            Some(DeptNameVerdict::Alias)
        );
        assert_eq!(
            dir.name_verdict("0301", " 检验科 "),
            Some(DeptNameVerdict::Canonical)
        );
    }

    #[test]
    fn 目录缺名称与别名时无从判定() {
        let dir = HisDeptDirectory::new(vec![entry("0301", "", &[])]);
        assert_eq!(dir.name_verdict("0301", "检验科"), None);
    }

    #[test]
    fn 响应缺科室代码时报错不静默跳过() {
        let err = parse_entries(r#"[{"dept_name":"检验科"}]"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::DeptDirectoryResponseInvalid);
        assert!(err.message.contains("dept_code"));
    }

    #[test]
    fn 响应不是数组时报错() {
        let err = parse_entries(r#"{"dept_code":"0301"}"#).unwrap_err();
        assert_eq!(err.code, ErrorCode::DeptDirectoryResponseInvalid);
    }

    #[test]
    fn 快照能来回序列化() {
        let snapshot = DirectorySnapshot {
            entries: vec![entry("0301", "检验科", &["检验科（本部）"])],
            fetched_at: "2026-11-06 09:12:00".to_string(),
        };
        let payload = snapshot.to_payload().unwrap();
        let back = DirectorySnapshot::from_payload(&payload).unwrap();
        assert_eq!(back, snapshot.entries);
    }
}
