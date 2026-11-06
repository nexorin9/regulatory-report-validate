//! 机器可读错误码体系。
//!
//! 每个错误码三件套：
//! - [`ErrorCode::as_str`]：机器读的 code（**只增不改**；要改语义就新增一个码）
//! - [`ErrorCode::zh`]：给业务人员看的中文说明
//! - [`ErrorCode::advice`]：处理建议（README 的「错误与降级」表与 doctor 输出同源）
//!
//! 引擎错误另带 `location`：出错位置 —— 文件路径 / 字段名 / 规则 id / 环境变量名。
//!
//! 错误码是引擎与表格插件之间的契约一部分：宏按 code 决定提示文案与是否中止写回，
//! 举证包与状态条按 code 回溯，所以同一语义的错误必须复用同一个码。

use std::fmt;
use std::path::Path;

/// 错误报告（JSON）的版本。表格插件按此判断能不能读这份报告。
pub const ERROR_REPORT_SCHEMA_VERSION: &str = "regval.error/1.0.0";

/// 机器可读错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    // —— 输入与表结构 ——
    /// 输入表读不出来（文件不存在、损坏、编码无法解析）
    InputUnreadable,
    /// 不支持的输入格式
    UnsupportedFormat,
    /// 表头缺少必填列
    MissingColumn,
    /// 表里没有任何数据行
    EmptyTable,

    // —— 口径规则 ——
    /// 规则 SQL 语法不通过（该规则不生效，报出错 token 与偏移）
    RuleSyntaxError,
    /// 规则表装载失败（目录不可读 / 文件解析失败 / 字段写错 / 同槽位同版本有歧义）
    RuleLoadFailed,
    /// 规则缺官方依据出处（拒绝装载，不以空依据进入执行）
    RuleBasisMissing,
    /// 口径规则执行失败
    RuleExec,
    /// 口径换版未经质控岗确认（本轮继续用旧版）
    RuleSwitchUnconfirmed,
    /// 口径版本的生效日期未到（本轮不启用该版本）
    RuleEffectiveDateNotReached,
    /// 规则集里没有指定版本的口径规则
    RuleVersionNotFound,
    /// 分母为 0（与「公式不符」分开归类）
    DenominatorZero,
    /// 数值列不是合法数值（与「空值」分开归类）
    InvalidNumeric,

    // —— 配置与凭据 ——
    /// 缺少必需的环境变量（点名缺哪几项，不用默认值兜底）
    ConfigMissing,
    /// 环境变量取值格式不合法
    ConfigInvalid,

    // —— 契约与写回 ——
    /// 结果契约写不出去
    ContractWrite,
    /// 契约版本与插件侧不一致 —— 拒绝写回，不做部分写回
    ContractVersionMismatch,

    // —— 上报与状态闭环 ——
    /// 缺幂等键（幂等键只由引擎生成，不接受外部传入）
    IdempotencyMissing,
    /// 行级状态迁移不合法（不在合法迁移表里；状态未被改写）
    InvalidStateTransition,
    /// 行状态版本冲突（乐观锁：后写者被拒，状态未被改写）
    RowStateConflict,
    /// 该行未经人工确认，写闸门拒绝推送
    UnconfirmedRow,
    /// 人工确认缺确认人（空名不入库：没有人的确认等于没人负责）
    ConfirmActorMissing,
    /// 上报网关退回（4xx）
    GatewayRejected,
    /// 上报网关返回服务端错误（HTTP 5xx）—— 可退避重试
    GatewayServerError,
    /// 上报网关调用超时 —— 可退避重试
    GatewayTimeout,
    /// 上报网关连不上（连接被拒 / DNS / 网络不可达）—— 可退避重试
    GatewayConnectionFailed,
    /// 上报网关回执结构不合法（200 但回执缺字段 / 不是 JSON）—— 不可当成成功
    GatewayResponseInvalid,
    /// 幂等键 ↔ 回执 ID 不是一一对应（同一幂等键落了两条回执 / 同一回执号挂了两个幂等键）
    ReceiptConflict,
    /// 回执台账交叉核对不通过（已上报却没有回执 / 有回执找不到提交记录 / 上报状态与行状态对不上）
    ReceiptLedgerMismatch,
    /// 科室代码在 HIS 主数据目录里匹配不到
    DeptNotFound,
    /// 科室名称写法与 HIS 主数据（或其别名）不符 —— 只提示，不阻断
    DeptNameMismatch,
    /// 科室主数据目录不可用（总码：具体情形见下面几个细分码）
    DeptDirectoryUnavailable,
    /// 科室主数据目录连不上（连接被拒 / DNS / 网络不可达）
    DeptDirectoryConnectionFailed,
    /// 科室主数据目录调用超时
    DeptDirectoryTimeout,
    /// 科室主数据目录拒绝了请求（HTTP 4xx）
    DeptDirectoryBadRequest,
    /// 科室主数据目录返回服务端错误（HTTP 5xx）
    DeptDirectoryServerError,
    /// 科室主数据目录响应结构不合法（不是数组 / 条目缺科室代码）
    DeptDirectoryResponseInvalid,

    // —— 模型 ——
    /// 模型端点不可达 / 超时 / 响应不可解析（总码：具体情形见下面四个细分码）
    ModelUnavailable,
    /// 模型端点连不上（连接被拒 / DNS / 网络不可达）
    ModelConnectionFailed,
    /// 模型端点调用超时
    ModelTimeout,
    /// 模型端点拒绝请求（HTTP 4xx）
    ModelBadRequest,
    /// 模型端点返回服务端错误（HTTP 5xx）
    ModelServerError,
    /// 模型响应未通过结构校验（不是 chat/completions 结构 / 内容不是 JSON / 字段不合法）
    ModelResponseInvalid,

    // —— 存储与 IO ——
    /// 本机校验库读写失败
    Db,
    /// 状态库文件损坏（不是有效校验库，或完整性检查不通过）
    DbCorrupted,
    /// 状态库版本不被本引擎识别（高于本引擎支持的版本）
    DbVersionUnsupported,
    /// 状态库迁移失败
    DbMigrationFailed,
    /// 权威键重复：同一机构 + 报告期 + 指标 + 科室 + 业务主键只能有一行权威数据
    AuthorityKeyConflict,
    /// 批次汇总重放：同一批次的同一修订号已经写过一遍（整次事务回滚，不留半截状态）
    BatchSummaryConflict,
    /// 文件系统读写失败
    Io,

    // —— 能力边界 ——
    /// 该能力尚未在本构建中提供
    CapabilityNotReady,

    // —— 评估基准集 ——
    /// 标注基准集不合法（标注取值不认识 / 定位不到行 / 左右两行是同一行）
    BenchSetInvalid,
}

impl ErrorCode {
    /// 错误码（机器读）
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::InputUnreadable => "INPUT_UNREADABLE",
            ErrorCode::UnsupportedFormat => "UNSUPPORTED_FORMAT",
            ErrorCode::MissingColumn => "MISSING_COLUMN",
            ErrorCode::EmptyTable => "EMPTY_TABLE",
            ErrorCode::RuleSyntaxError => "RULE_SYNTAX_ERROR",
            ErrorCode::RuleLoadFailed => "RULE_LOAD_FAILED",
            ErrorCode::RuleBasisMissing => "RULE_BASIS_MISSING",
            ErrorCode::RuleExec => "RULE_EXEC_ERROR",
            ErrorCode::RuleSwitchUnconfirmed => "RULE_SWITCH_UNCONFIRMED",
            ErrorCode::RuleEffectiveDateNotReached => "RULE_EFFECTIVE_DATE_NOT_REACHED",
            ErrorCode::RuleVersionNotFound => "RULE_VERSION_NOT_FOUND",
            ErrorCode::DenominatorZero => "DENOMINATOR_ZERO",
            ErrorCode::InvalidNumeric => "INVALID_NUMERIC",
            ErrorCode::ConfigMissing => "CONFIG_MISSING",
            ErrorCode::ConfigInvalid => "CONFIG_INVALID",
            ErrorCode::ContractWrite => "CONTRACT_WRITE_ERROR",
            ErrorCode::ContractVersionMismatch => "CONTRACT_VERSION_MISMATCH",
            ErrorCode::IdempotencyMissing => "IDEMPOTENCY_MISSING",
            ErrorCode::InvalidStateTransition => "INVALID_STATE_TRANSITION",
            ErrorCode::RowStateConflict => "ROW_STATE_CONFLICT",
            ErrorCode::UnconfirmedRow => "UNCONFIRMED_ROW",
            ErrorCode::ConfirmActorMissing => "CONFIRM_ACTOR_MISSING",
            ErrorCode::GatewayRejected => "GATEWAY_REJECTED",
            ErrorCode::GatewayServerError => "GATEWAY_SERVER_ERROR",
            ErrorCode::GatewayTimeout => "GATEWAY_TIMEOUT",
            ErrorCode::GatewayConnectionFailed => "GATEWAY_CONNECTION_FAILED",
            ErrorCode::GatewayResponseInvalid => "GATEWAY_RESPONSE_INVALID",
            ErrorCode::ReceiptConflict => "RECEIPT_CONFLICT",
            ErrorCode::ReceiptLedgerMismatch => "RECEIPT_LEDGER_MISMATCH",
            ErrorCode::DeptNotFound => "DEPT_NOT_FOUND",
            ErrorCode::DeptNameMismatch => "DEPT_NAME_MISMATCH",
            ErrorCode::DeptDirectoryUnavailable => "DEPT_DIRECTORY_UNAVAILABLE",
            ErrorCode::DeptDirectoryConnectionFailed => "DEPT_DIRECTORY_CONNECTION_FAILED",
            ErrorCode::DeptDirectoryTimeout => "DEPT_DIRECTORY_TIMEOUT",
            ErrorCode::DeptDirectoryBadRequest => "DEPT_DIRECTORY_BAD_REQUEST",
            ErrorCode::DeptDirectoryServerError => "DEPT_DIRECTORY_SERVER_ERROR",
            ErrorCode::DeptDirectoryResponseInvalid => "DEPT_DIRECTORY_RESPONSE_INVALID",
            ErrorCode::ModelUnavailable => "MODEL_UNAVAILABLE",
            ErrorCode::ModelConnectionFailed => "MODEL_CONNECTION_FAILED",
            ErrorCode::ModelTimeout => "MODEL_TIMEOUT",
            ErrorCode::ModelBadRequest => "MODEL_BAD_REQUEST",
            ErrorCode::ModelServerError => "MODEL_SERVER_ERROR",
            ErrorCode::ModelResponseInvalid => "MODEL_RESPONSE_INVALID",
            ErrorCode::Db => "DB_ERROR",
            ErrorCode::DbCorrupted => "DB_CORRUPTED",
            ErrorCode::DbVersionUnsupported => "DB_VERSION_UNSUPPORTED",
            ErrorCode::DbMigrationFailed => "DB_MIGRATION_FAILED",
            ErrorCode::AuthorityKeyConflict => "AUTHORITY_KEY_CONFLICT",
            ErrorCode::BatchSummaryConflict => "BATCH_SUMMARY_CONFLICT",
            ErrorCode::Io => "IO_ERROR",
            ErrorCode::CapabilityNotReady => "CAPABILITY_NOT_READY",
            ErrorCode::BenchSetInvalid => "BENCH_SET_INVALID",
        }
    }

    /// 错误码中文说明（给人读）
    pub fn zh(&self) -> &'static str {
        match self {
            ErrorCode::InputUnreadable => "输入表读取失败",
            ErrorCode::UnsupportedFormat => "不支持的输入格式",
            ErrorCode::MissingColumn => "表头缺少必填列",
            ErrorCode::EmptyTable => "表中没有数据行",
            ErrorCode::RuleSyntaxError => "口径规则语法不通过",
            ErrorCode::RuleLoadFailed => "口径规则表装载失败",
            ErrorCode::RuleBasisMissing => "口径规则缺官方依据",
            ErrorCode::RuleExec => "口径规则执行失败",
            ErrorCode::RuleSwitchUnconfirmed => "口径换版未经质控岗确认",
            ErrorCode::RuleEffectiveDateNotReached => "口径版本的生效日期未到",
            ErrorCode::RuleVersionNotFound => "规则集里没有该版本的口径规则",
            ErrorCode::DenominatorZero => "分母为 0",
            ErrorCode::InvalidNumeric => "数值列不是合法数值",
            ErrorCode::ConfigMissing => "缺少必需的环境变量",
            ErrorCode::ConfigInvalid => "环境变量取值不合法",
            ErrorCode::ContractWrite => "结果契约写出失败",
            ErrorCode::ContractVersionMismatch => "引擎与宏版本不一致，请升级",
            ErrorCode::IdempotencyMissing => "缺幂等键",
            ErrorCode::InvalidStateTransition => "行级状态迁移不合法",
            ErrorCode::RowStateConflict => "行状态版本冲突",
            ErrorCode::UnconfirmedRow => "该行未经人工确认，不能上报",
            ErrorCode::ConfirmActorMissing => "人工确认缺确认人",
            ErrorCode::GatewayRejected => "上报网关退回",
            ErrorCode::GatewayServerError => "上报网关返回服务端错误",
            ErrorCode::GatewayTimeout => "上报网关调用超时",
            ErrorCode::GatewayConnectionFailed => "上报网关连不上",
            ErrorCode::GatewayResponseInvalid => "上报网关回执结构不合法",
            ErrorCode::ReceiptConflict => "幂等键与回执 ID 不是一一对应",
            ErrorCode::ReceiptLedgerMismatch => "回执台账交叉核对不通过",
            ErrorCode::DeptNotFound => "科室主数据不一致",
            ErrorCode::DeptNameMismatch => "科室名称写法与主数据不一致",
            ErrorCode::DeptDirectoryUnavailable => "科室主数据目录不可用",
            ErrorCode::DeptDirectoryConnectionFailed => "科室主数据目录连不上",
            ErrorCode::DeptDirectoryTimeout => "科室主数据目录调用超时",
            ErrorCode::DeptDirectoryBadRequest => "科室主数据目录拒绝了请求",
            ErrorCode::DeptDirectoryServerError => "科室主数据目录返回服务端错误",
            ErrorCode::DeptDirectoryResponseInvalid => "科室主数据目录响应结构不合法",
            ErrorCode::ModelUnavailable => "模型端点不可达",
            ErrorCode::ModelConnectionFailed => "模型端点连不上",
            ErrorCode::ModelTimeout => "模型端点调用超时",
            ErrorCode::ModelBadRequest => "模型端点拒绝了请求",
            ErrorCode::ModelServerError => "模型端点返回服务端错误",
            ErrorCode::ModelResponseInvalid => "模型响应未通过结构校验",
            ErrorCode::Db => "本机校验库操作失败",
            ErrorCode::DbCorrupted => "状态库文件损坏",
            ErrorCode::DbVersionUnsupported => "状态库版本不被本引擎识别",
            ErrorCode::DbMigrationFailed => "状态库迁移失败",
            ErrorCode::AuthorityKeyConflict => "权威键重复",
            ErrorCode::BatchSummaryConflict => "批次汇总重放冲突",
            ErrorCode::Io => "文件读写失败",
            ErrorCode::CapabilityNotReady => "该能力尚未在本构建中提供",
            ErrorCode::BenchSetInvalid => "标注基准集不合法",
        }
    }

    /// 处理建议（写进 README 的错误表、doctor 输出与状态条文案）
    pub fn advice(&self) -> &'static str {
        match self {
            ErrorCode::InputUnreadable => "确认文件路径与读取权限；Excel 另存为 .xlsx 后重试",
            ErrorCode::UnsupportedFormat => "本轮支持 .csv / .xlsx / .xlsm，先把表另存为 xlsx",
            ErrorCode::MissingColumn => "检查表头是否写错，或多表并入时列被挤掉",
            ErrorCode::EmptyTable => "确认表头之外至少有一行数据",
            ErrorCode::RuleSyntaxError => "修正规则 SQL 后重新装载；该规则在此之前不生效",
            ErrorCode::RuleLoadFailed => "按报错里的文件名与字段名修正规则表；没装载上的规则不会参与校验",
            ErrorCode::RuleBasisMissing => "给该条规则补上官方依据出处（文件与条款），补齐后再装载",
            ErrorCode::RuleExec => "按日志里的规则 id 定位；其余规则不受影响",
            ErrorCode::RuleSwitchUnconfirmed => "请质控岗确认新口径版本后执行 regval rules approve；未确认前本轮继续用旧版",
            ErrorCode::RuleEffectiveDateNotReached => "按规则表里的 effective_from 字段确认启用日期；未到日期该版本不生效",
            ErrorCode::RuleVersionNotFound => "确认规则目录里有该版本的口径表，或改用已装载的版本",
            ErrorCode::DenominatorZero => "先核对该行分母口径，补齐后再填分子与填报值",
            ErrorCode::InvalidNumeric => "把该格改成数值；非数值与空值是两类不同问题",
            ErrorCode::ConfigMissing => "复制 .env.example 为 .env 并填齐；凭据不要写进代码",
            ErrorCode::ConfigInvalid => "按该项的取值格式修正后重跑",
            ErrorCode::ContractWrite => "确认输出目录可写且磁盘有空间",
            ErrorCode::ContractVersionMismatch => "引擎与宏版本不一致，请升级表格插件或引擎",
            ErrorCode::IdempotencyMissing => "重跑上报计划编译；幂等键只由引擎生成",
            ErrorCode::InvalidStateTransition => "按报错里的两个状态看这一行该从哪来：未过校验的行不能直接上报，已上报的行不会被普通校验改回",
            ErrorCode::RowStateConflict => "这一行刚被另一个动作推进过；重跑当前动作即可，不要手工改状态",
            ErrorCode::UnconfirmedRow => "在原表勾选确认后再上报；未确认行一律不推",
            ErrorCode::ConfirmActorMissing => "确认时写明确认人（regval confirm --by 姓名）；空名不入库",
            ErrorCode::GatewayRejected => "按回执里的错误码与字段名修正后重新上报",
            ErrorCode::GatewayServerError => "网关侧临时故障，退避重试；重试耗尽转「待重试」，不改成已上报",
            ErrorCode::GatewayTimeout => "调大 REGULATORY_REPORT_TIMEOUT_SECS 或排查院内网段；超时不算「已上报」",
            ErrorCode::GatewayConnectionFailed => "确认网关地址可达、院内网段未断开；本行转「待重试」，保留幂等键",
            ErrorCode::GatewayResponseInvalid => "回执结构不合法，该行标「回执待人工核对」；不要按已上报处理",
            ErrorCode::ReceiptConflict => "同一幂等键只允许一条回执、同一回执号只允许挂一个幂等键；先查清重复回执来源再重跑",
            ErrorCode::ReceiptLedgerMismatch => "按台账里列出的冲突逐条查（幂等键 ↔ 回执号 ↔ 行状态）；查清前这份台账不要归档",
            ErrorCode::DeptNotFound => "核对科室代码与 HIS 主数据；本轮结果标「科室主数据未校验」",
            ErrorCode::DeptNameMismatch => "只提示不改数：核对科室名称写法，或把该写法报给信息科登记为别名",
            ErrorCode::DeptDirectoryUnavailable => "确认 HIS_BASE_URL / HIS_API_TOKEN 已配置且院内网段可达；目录拉不到时本轮标「科室主数据未校验」",
            ErrorCode::DeptDirectoryConnectionFailed => "确认 HIS 目录地址可达、院内网段未断开；本轮退回上次缓存，缓存也没有则标「科室主数据未校验」",
            ErrorCode::DeptDirectoryTimeout => "调大 HIS_TIMEOUT_SECS 或排查院内网段；超时不算查过",
            ErrorCode::DeptDirectoryBadRequest => "按错误里的端点原话核对 HIS 目录接口与凭据权限",
            ErrorCode::DeptDirectoryServerError => "HIS 侧临时故障，稍后重试；本轮退回上次缓存并标注数据时间",
            ErrorCode::DeptDirectoryResponseInvalid => "科室目录响应结构不对（应为数组且每条带 dept_code）；找信息科核对接口实现",
            ErrorCode::ModelUnavailable => "模型不可达时主路径仍可跑；只标「疑似」，不合并不推送",
            ErrorCode::ModelConnectionFailed => "确认模型端点地址可达、院内网段未断开；主路径仍可跑，语义归并本轮为空",
            ErrorCode::ModelTimeout => "调大 OPENAI_TIMEOUT_SECS 或减小单次候选对数；超时不算查过，重试后仍失败才降级",
            ErrorCode::ModelBadRequest => "按错误里带出的端点原话修正请求；端点若不支持 JSON 模式，换一个兼容端点",
            ErrorCode::ModelServerError => "端点侧临时故障，稍后重试；重试仍失败则本轮只留确定性候选",
            ErrorCode::ModelResponseInvalid => "响应未通过结构校验，整次调用已作废；本轮只留确定性候选，不写任何业务表",
            ErrorCode::Db => "检查状态库是否被占用或损坏；按状态库恢复说明处理",
            ErrorCode::DbCorrupted => "先把损坏文件改名备份（不要删），按状态库恢复流程用幂等键与回执记录恢复到最后一个一致状态",
            ErrorCode::DbVersionUnsupported => "升级引擎到支持该状态的版本，或换一个状态库路径；不要用新引擎直接改旧库",
            ErrorCode::DbMigrationFailed => "按日志里的失败迁移版本排查；迁移是幂等的，修好后重跑即可",
            ErrorCode::AuthorityKeyConflict => "同一机构+报告期+指标+科室+业务主键只能有一行权威数据；确认重复来源后由经办人判定",
            ErrorCode::BatchSummaryConflict => "同一版批次汇总已经写过一次；本次事务已整笔回滚，汇总回到批前值 —— 确认不是重复触发后再重跑",
            ErrorCode::Io => "检查路径与读写权限",
            ErrorCode::CapabilityNotReady => "该能力尚未在本构建中提供，请改用已就绪的子命令",
            ErrorCode::BenchSetInvalid => "按报错里的标注序号修正标注文件；定位不到行时确认行号（表头不算第一行）",
        }
    }

    /// 全部错误码（doctor、README 与自检共用同一份清单）
    pub const ALL: [ErrorCode; 52] = [
        ErrorCode::InputUnreadable,
        ErrorCode::UnsupportedFormat,
        ErrorCode::MissingColumn,
        ErrorCode::EmptyTable,
        ErrorCode::RuleSyntaxError,
        ErrorCode::RuleLoadFailed,
        ErrorCode::RuleBasisMissing,
        ErrorCode::RuleExec,
        ErrorCode::RuleSwitchUnconfirmed,
        ErrorCode::RuleEffectiveDateNotReached,
        ErrorCode::RuleVersionNotFound,
        ErrorCode::DenominatorZero,
        ErrorCode::InvalidNumeric,
        ErrorCode::ConfigMissing,
        ErrorCode::ConfigInvalid,
        ErrorCode::ContractWrite,
        ErrorCode::ContractVersionMismatch,
        ErrorCode::IdempotencyMissing,
        ErrorCode::InvalidStateTransition,
        ErrorCode::RowStateConflict,
        ErrorCode::UnconfirmedRow,
        ErrorCode::ConfirmActorMissing,
        ErrorCode::GatewayRejected,
        ErrorCode::GatewayServerError,
        ErrorCode::GatewayTimeout,
        ErrorCode::GatewayConnectionFailed,
        ErrorCode::GatewayResponseInvalid,
        ErrorCode::ReceiptConflict,
        ErrorCode::ReceiptLedgerMismatch,
        ErrorCode::DeptNotFound,
        ErrorCode::DeptNameMismatch,
        ErrorCode::DeptDirectoryUnavailable,
        ErrorCode::DeptDirectoryConnectionFailed,
        ErrorCode::DeptDirectoryTimeout,
        ErrorCode::DeptDirectoryBadRequest,
        ErrorCode::DeptDirectoryServerError,
        ErrorCode::DeptDirectoryResponseInvalid,
        ErrorCode::ModelUnavailable,
        ErrorCode::ModelConnectionFailed,
        ErrorCode::ModelTimeout,
        ErrorCode::ModelBadRequest,
        ErrorCode::ModelServerError,
        ErrorCode::ModelResponseInvalid,
        ErrorCode::Db,
        ErrorCode::DbCorrupted,
        ErrorCode::DbVersionUnsupported,
        ErrorCode::DbMigrationFailed,
        ErrorCode::AuthorityKeyConflict,
        ErrorCode::BatchSummaryConflict,
        ErrorCode::Io,
        ErrorCode::CapabilityNotReady,
        ErrorCode::BenchSetInvalid,
    ];

    /// 按 code 反查错误码（表格插件按 code 查文案时用）
    pub fn from_str(code: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.iter().copied().find(|c| c.as_str() == code)
    }
}

/// 引擎错误：错误码 + 说明 + 出错位置（字段名 / 文件路径 / 规则 id / 环境变量名）。
#[derive(Debug, Clone)]
pub struct EngineError {
    pub code: ErrorCode,
    pub message: String,
    pub location: Option<String>,
}

impl EngineError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        EngineError {
            code,
            message: message.into(),
            location: None,
        }
    }

    /// 带出错位置的错误（例如「缺哪一列」「哪一条规则」「缺哪个环境变量」）
    pub fn at(code: ErrorCode, message: impl Into<String>, location: impl Into<String>) -> Self {
        EngineError {
            code,
            message: message.into(),
            location: Some(location.into()),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)?;
        if let Some(loc) = &self.location {
            write!(f, "（位置：{}）", loc)?;
        }
        Ok(())
    }
}

impl std::error::Error for EngineError {}

/// 把引擎错误写成一份**机器可读**的报告（JSON）。
///
/// # 为什么要有这份文件
///
/// 表格插件（Excel 宏）拿得到的是引擎的退出码，拿不到的是「错在哪」。
/// 让宏去解析人类可读的 stderr（`错误：[CODE] 说明`）是脆的：文案一改就断。
///
/// 所以引擎失败时按 `--error-out` 指定的路径写这份文件：code、中文说明、
/// 出错位置、处理建议各占一个字段，宏按字段读，不用猜。
///
/// 写失败**不覆盖原始错误**：这份报告是给人看的附加物，报告写不出来时
/// 原错误照样报给终端。
pub fn write_error_report(path: &Path, err: &EngineError) -> Result<(), EngineError> {
    let body = serde_json::json!({
        "schema_version": ERROR_REPORT_SCHEMA_VERSION,
        "code": err.code.as_str(),
        "code_zh": err.code.zh(),
        "message": err.message,
        "location": err.location.clone().unwrap_or_default(),
        "advice": err.code.advice(),
    });
    let text = serde_json::to_string_pretty(&body)
        .map_err(|e| EngineError::at(ErrorCode::Io, format!("错误报告序列化失败：{}", e), path.display().to_string()))?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    std::fs::write(path, text)
        .map_err(|e| EngineError::at(ErrorCode::Io, format!("错误报告写不出去：{}", e), path.display().to_string()))
}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        EngineError::new(ErrorCode::Db, e.to_string())
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::new(ErrorCode::Io, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 错误码机器可读且互不重复() {
        let mut seen = std::collections::BTreeSet::new();
        for code in ErrorCode::ALL.iter() {
            assert!(seen.insert(code.as_str()), "错误码重复：{}", code.as_str());
            assert!(!code.zh().is_empty(), "{} 缺中文说明", code.as_str());
            assert!(!code.advice().is_empty(), "{} 缺处理建议", code.as_str());
        }
    }

    #[test]
    fn 必答错误码全部在册() {
        for want in [
            "CONTRACT_VERSION_MISMATCH",
            "RULE_SYNTAX_ERROR",
            "RULE_LOAD_FAILED",
            "RULE_BASIS_MISSING",
            "MISSING_COLUMN",
            "DENOMINATOR_ZERO",
            "IDEMPOTENCY_MISSING",
            "UNCONFIRMED_ROW",
            "GATEWAY_REJECTED",
            "GATEWAY_SERVER_ERROR",
            "GATEWAY_TIMEOUT",
            "GATEWAY_CONNECTION_FAILED",
            "GATEWAY_RESPONSE_INVALID",
            "DEPT_NOT_FOUND",
            "MODEL_UNAVAILABLE",
            "MODEL_CONNECTION_FAILED",
            "MODEL_TIMEOUT",
            "MODEL_BAD_REQUEST",
            "MODEL_SERVER_ERROR",
            "MODEL_RESPONSE_INVALID",
            "INVALID_NUMERIC",
        ] {
            assert!(
                ErrorCode::from_str(want).is_some(),
                "错误码体系必须覆盖 {}",
                want
            );
        }
    }

    #[test]
    fn 错误带位置且位置可见() {
        let e = EngineError::at(ErrorCode::MissingColumn, "表头缺少必填列", "科室代码");
        assert_eq!(e.code, ErrorCode::MissingColumn);
        assert!(e.to_string().contains("科室代码"));
    }
}
