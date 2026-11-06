//! 模型调用的可审计元数据与降级路径。
//!
//! # 这一层解决什么
//!
//! 模型端点是**外部依赖**：它可能连不上、超时、返回一个不是 JSON 的字符串，
//! 或者干脆编出一个我们没问过的行键。这几种情形没有一种该让整条主路径停下来 ——
//! 上报表还得校验，口径问题还得列出来，经办人还得拿到那份清单。
//!
//! 所以这里定死两条规矩：
//!
//! 1. **不可用即降级**：模型不可达 / 超时 / 响应不可解析 / 结构校验不通过，
//!    一律只输出确定性候选（精确键 + 归一化 + 编辑距离阈值），且一律标「疑似」。
//!    **不静默合并、不静默推送、不静默推进任何状态**。
//! 2. **每次调用都留痕**：模型名、请求耗时、token 用量、是否降级、候选来源等级、
//!    错误码 —— 落审计表，事后能查「这一批的疑似项里模型掺和了没有」。
//!
//! # 想要「模型不可用就停下」怎么办
//!
//! [`ModelRequirement::Required`]（CLI 的 `--require-model`）：模型不可用直接以
//! [`ErrorCode::ModelUnavailable`] 退出，**不产出结果契约、不改动业务状态**。
//! 默认是 [`ModelRequirement::Optional`] —— 降级，但降级这件事本身留痕、可见。
//!
//! # 分层
//!
//! 本模块**不碰数据库**（模型层不写业务表，有静态用例盯着）。落库由状态库层
//! 实现 [`ModelCallAuditSink`]，业务层只认这个 trait。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::errors::{EngineError, ErrorCode};
use crate::model::gateway::{MergeRequest, ModelGateway};
use crate::model::schema::MergeResponse;

/// 候选来源等级：这一轮的疑似项是哪一级判出来的。
///
/// 机器可读取值固定三个（契约与审计表共用同一套说法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    /// 确定性三级（精确键 + 归一化 + 编辑距离）—— 模型不可用时只剩这一档
    Deterministic,
    /// 模型语义级
    Semantic,
    /// 没有候选
    None,
}

impl CandidateSource {
    /// 机器可读取值
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateSource::Deterministic => "deterministic",
            CandidateSource::Semantic => "semantic",
            CandidateSource::None => "none",
        }
    }

    /// 中文说明（给人看）
    pub fn zh(self) -> &'static str {
        match self {
            CandidateSource::Deterministic => "确定性候选（精确键 + 归一化 + 编辑距离）",
            CandidateSource::Semantic => "模型语义候选",
            CandidateSource::None => "无候选",
        }
    }

    /// 解析机器可读取值
    pub fn parse(raw: &str) -> CandidateSource {
        match raw.trim() {
            "semantic" => CandidateSource::Semantic,
            "none" => CandidateSource::None,
            _ => CandidateSource::Deterministic,
        }
    }
}

/// 一次模型调用的可审计元数据。
///
/// **只含模型侧元数据与计数，不含任何行内容** —— 审计表里看不到指标名称、
/// 看不到科室名称、更看不到患者可识别明细（这一层压根没有这些字段）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCallAudit {
    /// 调用标识（批次 + 进程内序号）
    pub call_id: String,
    /// 调用时间（`YYYY-MM-DD HH:mm:ss`）
    pub ts: String,
    /// 批次号
    pub batch_ref: String,
    /// 端点地址（**不含凭据**：凭据走环境变量，不进审计表）
    pub endpoint: String,
    /// 请求的模型名
    pub requested_model: String,
    /// 端点回报的模型名（端点没报就空串）
    pub served_model: String,
    /// 请求耗时（毫秒）
    pub duration_ms: u64,
    /// 提交给模型的候选对数
    pub candidate_pairs: usize,
    /// 通过结构校验的结论数
    pub accepted_pairs: usize,
    /// 同一对行重复出现被合并掉的条数（不静默丢）
    pub dropped_duplicate_pairs: usize,
    /// 输入 token 数
    pub prompt_tokens: Option<u64>,
    /// 输出 token 数
    pub completion_tokens: Option<u64>,
    /// 总 token 数
    pub total_tokens: Option<u64>,
    /// 是否降级（模型不可用 / 超时 / 响应不合法）
    pub degraded: bool,
    /// 降级时的错误码（机器可读）；没降级为空串
    pub error_code: String,
    /// 降级原因（中文）
    pub error_message: String,
    /// 本轮疑似项的来源等级（deterministic / semantic / none）
    pub candidate_source: String,
}

impl ModelCallAudit {
    /// 新建一条审计记录（未降级）
    fn new(ctx: &CallContext) -> ModelCallAudit {
        ModelCallAudit {
            call_id: next_call_id(&ctx.batch_ref),
            ts: now_timestamp(),
            batch_ref: ctx.batch_ref.clone(),
            endpoint: ctx.endpoint.clone(),
            requested_model: ctx.requested_model.clone(),
            served_model: String::new(),
            duration_ms: 0,
            candidate_pairs: 0,
            accepted_pairs: 0,
            dropped_duplicate_pairs: 0,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            degraded: false,
            error_code: String::new(),
            error_message: String::new(),
            candidate_source: CandidateSource::None.as_str().to_string(),
        }
    }

    /// 降级记录（模型不可用）：来源等级退回确定性
    fn degraded(ctx: &CallContext, duration_ms: u64, err: &EngineError) -> ModelCallAudit {
        let mut audit = ModelCallAudit::new(ctx);
        audit.duration_ms = duration_ms;
        audit.degraded = true;
        audit.error_code = err.code.as_str().to_string();
        audit.error_message = err.message.clone();
        audit.candidate_source = CandidateSource::Deterministic.as_str().to_string();
        audit
    }

    /// 一行中文摘要（CLI 与 `regval status` 用）
    pub fn summary_line(&self) -> String {
        if self.degraded {
            format!(
                "模型调用 {} · 降级（{}）· 耗时 {} ms · 候选 {} 对 · 来源 {}",
                self.requested_model,
                self.error_code,
                self.duration_ms,
                self.candidate_pairs,
                self.candidate_source
            )
        } else {
            format!(
                "模型调用 {} · 成功 · 耗时 {} ms · 候选 {} 对 · 采纳 {} 对 · token {}",
                self.requested_model,
                self.duration_ms,
                self.candidate_pairs,
                self.accepted_pairs,
                self.total_tokens
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "未报".to_string())
            )
        }
    }
}

/// 一次模型调用的结果：成功，或降级。
#[derive(Debug, Clone)]
pub enum ModelCallOutcome {
    /// 调用成功，响应已通过结构校验
    Succeeded {
        response: MergeResponse,
        audit: ModelCallAudit,
    },
    /// 调用不可用（连不上 / 超时 / 4xx / 5xx / 响应不合法）—— 主路径继续走确定性候选
    Degraded { audit: ModelCallAudit },
}

impl ModelCallOutcome {
    /// 本次调用的审计记录
    pub fn audit(&self) -> &ModelCallAudit {
        match self {
            ModelCallOutcome::Succeeded { audit, .. } => audit,
            ModelCallOutcome::Degraded { audit } => audit,
        }
    }

    /// 是否降级
    pub fn is_degraded(&self) -> bool {
        matches!(self, ModelCallOutcome::Degraded { .. })
    }

    /// 成功时的响应（降级时为 None）
    pub fn response(&self) -> Option<&MergeResponse> {
        match self {
            ModelCallOutcome::Succeeded { response, .. } => Some(response),
            ModelCallOutcome::Degraded { .. } => None,
        }
    }
}

/// 模型是否必须可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRequirement {
    /// 默认可降级：模型不可用时只输出确定性候选（降级本身留痕、可见）
    Optional,
    /// 必须可用：模型不可用直接报错退出（`--require-model`）
    Required,
}

/// 按要求处置一次调用的结果。
///
/// - [`ModelRequirement::Optional`]：降级也放行（主路径不阻塞）；
/// - [`ModelRequirement::Required`]：降级即报错 [`ErrorCode::ModelUnavailable`]，
///   **不产出结果契约、不改动业务状态**（调用方在写盘之前就返回错误）。
pub fn enforce_requirement(
    outcome: &ModelCallOutcome,
    requirement: ModelRequirement,
) -> Result<(), EngineError> {
    match (requirement, outcome) {
        (ModelRequirement::Required, ModelCallOutcome::Degraded { audit }) => Err(EngineError::at(
            ErrorCode::ModelUnavailable,
            format!(
                "模型端点不可用（{}）：{} —— 已按「模型必须可用」的要求中止，本轮不产出结果契约、不改动业务状态",
                audit.error_code, audit.error_message
            ),
            audit.endpoint.clone(),
        )),
        _ => Ok(()),
    }
}

/// 调用的上下文（哪一批、哪个端点、哪个模型）
#[derive(Debug, Clone, Default)]
pub struct CallContext {
    /// 批次号
    pub batch_ref: String,
    /// 端点地址（不含凭据）
    pub endpoint: String,
    /// 请求的模型名
    pub requested_model: String,
}

impl CallContext {
    /// 用上下文建一个
    pub fn new(
        batch_ref: impl Into<String>,
        endpoint: impl Into<String>,
        requested_model: impl Into<String>,
    ) -> CallContext {
        CallContext {
            batch_ref: batch_ref.into(),
            endpoint: endpoint.into(),
            requested_model: requested_model.into(),
        }
    }
}

/// 发起一次归并调用，把结果与审计记录一并带回。
///
/// **不吞异常**：端点不可用时返回 [`ModelCallOutcome::Degraded`]，错误码原样带出；
/// 调用方决定是降级继续还是硬失败（见 [`enforce_requirement`]）。
pub fn run_merge_call(
    gateway: &dyn ModelGateway,
    request: &MergeRequest,
    ctx: &CallContext,
) -> ModelCallOutcome {
    let started = Instant::now();
    match gateway.merge_candidates(request) {
        Ok(response) => {
            let mut audit = ModelCallAudit::new(ctx);
            audit.duration_ms = started.elapsed().as_millis() as u64;
            audit.candidate_pairs = request.pairs.len();
            audit.accepted_pairs = response.items.len();
            audit.dropped_duplicate_pairs = response.dropped_duplicate_pairs;
            audit.served_model = response.served_model.clone().unwrap_or_default();
            if let Some(usage) = &response.usage {
                audit.prompt_tokens = usage.prompt_tokens;
                audit.completion_tokens = usage.completion_tokens;
                audit.total_tokens = usage.total_tokens;
            }
            // 模型参与了这一轮 —— 候选来源等级含语义级
            // （模型答「没有一对是同一件事」也是结论，不是「没查」）
            audit.candidate_source = CandidateSource::Semantic.as_str().to_string();
            ModelCallOutcome::Succeeded { response, audit }
        }
        Err(err) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            let mut audit = ModelCallAudit::degraded(ctx, duration_ms, &err);
            // 降级也要记清楚「本来要送多少对」—— 不然事后只看到「候选 0 对」，
            // 分不清是「本来就没有候选」还是「有候选没送成」。这两件事的处置完全不同。
            audit.candidate_pairs = request.pairs.len();
            ModelCallOutcome::Degraded { audit }
        }
    }
}

/// 模型调用审计的落库接口。
///
/// 实现在状态库层 —— 模型层自己不碰数据库（模型层不写业务表，有静态用例盯着）。
pub trait ModelCallAuditSink {
    /// 记一条审计
    fn record(&mut self, audit: &ModelCallAudit) -> Result<(), EngineError>;
}

/// 内存审计收集器（测试与干跑用；不落盘）
#[derive(Debug, Clone, Default)]
pub struct MemoryAuditSink {
    records: Vec<ModelCallAudit>,
}

impl MemoryAuditSink {
    /// 空收集器
    pub fn new() -> MemoryAuditSink {
        MemoryAuditSink::default()
    }

    /// 已收集的审计记录
    pub fn records(&self) -> &[ModelCallAudit] {
        &self.records
    }

    /// 最后一条（没有则 None）
    pub fn last(&self) -> Option<&ModelCallAudit> {
        self.records.last()
    }
}

impl ModelCallAuditSink for MemoryAuditSink {
    fn record(&mut self, audit: &ModelCallAudit) -> Result<(), EngineError> {
        self.records.push(audit.clone());
        Ok(())
    }
}

/// 结果契约里的模型调用段（这一轮模型端点是什么状态）。
///
/// 这一段是**追加**的：写回用的 5 个新列与行结果结构没变，插件读法不变，
/// 因此契约版本不升。留这一段是为了让「这一批的疑似项里模型掺和了没有、
/// 有没有降级」有据可查。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelStatusSection {
    /// 本轮是否发起了模型调用（没配模型端点时为否）
    pub attempted: bool,
    /// 是否降级（模型不可用 / 超时 / 响应不合法）
    pub degraded: bool,
    /// 降级时的错误码（机器可读）；没降级为空串
    pub error_code: String,
    /// 降级原因（中文）
    pub error_message: String,
    /// 本轮疑似项的来源等级（deterministic / semantic / none）
    pub candidate_source: String,
    /// 端点地址（不含凭据）
    pub endpoint: String,
    /// 请求的模型名
    pub requested_model: String,
    /// 端点回报的模型名
    pub served_model: String,
    /// 调用耗时（毫秒）
    pub duration_ms: u64,
    /// 提交给模型的候选对数
    pub candidate_pairs: usize,
    /// 通过结构校验的结论数
    pub accepted_pairs: usize,
    /// 同一对行重复出现被合并掉的条数
    pub dropped_duplicate_pairs: usize,
    /// 输入 token 数
    pub prompt_tokens: Option<u64>,
    /// 输出 token 数
    pub completion_tokens: Option<u64>,
    /// 总 token 数
    pub total_tokens: Option<u64>,
    /// 模型提出的归并候选。
    ///
    /// **本轮只作标注**：一律待经办人逐条确认，不合并、不推送、不推进任何状态。
    #[serde(default)]
    pub proposals: Vec<ModelProposal>,
}

impl ModelStatusSection {
    /// 「本轮未接入模型端点」的状态段（没配模型凭据时）
    pub fn not_attempted() -> ModelStatusSection {
        ModelStatusSection {
            attempted: false,
            degraded: false,
            error_code: String::new(),
            error_message: String::new(),
            candidate_source: CandidateSource::Deterministic.as_str().to_string(),
            endpoint: String::new(),
            requested_model: String::new(),
            served_model: String::new(),
            duration_ms: 0,
            candidate_pairs: 0,
            accepted_pairs: 0,
            dropped_duplicate_pairs: 0,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            proposals: Vec::new(),
        }
    }

    /// 由一次调用的结果生成状态段
    pub fn from_outcome(outcome: &ModelCallOutcome) -> ModelStatusSection {
        let audit = outcome.audit();
        let mut section = ModelStatusSection {
            attempted: true,
            degraded: audit.degraded,
            error_code: audit.error_code.clone(),
            error_message: audit.error_message.clone(),
            candidate_source: audit.candidate_source.clone(),
            endpoint: audit.endpoint.clone(),
            requested_model: audit.requested_model.clone(),
            served_model: audit.served_model.clone(),
            duration_ms: audit.duration_ms,
            candidate_pairs: audit.candidate_pairs,
            accepted_pairs: audit.accepted_pairs,
            dropped_duplicate_pairs: audit.dropped_duplicate_pairs,
            prompt_tokens: audit.prompt_tokens,
            completion_tokens: audit.completion_tokens,
            total_tokens: audit.total_tokens,
            proposals: Vec::new(),
        };
        if let Some(response) = outcome.response() {
            section.proposals = response
                .items
                .iter()
                .map(|item| ModelProposal {
                    left_row_key: item.left_row_key.clone(),
                    right_row_key: item.right_row_key.clone(),
                    kind: item.kind.as_str().to_string(),
                    confidence: item.confidence,
                    reason: item.reason.clone(),
                    evidence: item.evidence.clone(),
                })
                .collect();
        }
        section
    }

    /// 一行中文摘要（CLI 输出用）
    pub fn summary_line(&self) -> String {
        if !self.attempted {
            return "模型端点：本轮未接入（疑似项来源为确定性候选）".to_string();
        }
        if self.degraded {
            format!(
                "模型端点：不可用（{}）—— 本轮只输出确定性候选，疑似项来源标记为 {}",
                self.error_code, self.candidate_source
            )
        } else {
            format!(
                "模型端点：可用 · 模型 {} · 耗时 {} ms · 候选 {} 对 · 采纳 {} 对",
                self.requested_model, self.duration_ms, self.candidate_pairs, self.accepted_pairs
            )
        }
    }
}

/// 模型提出的一条归并候选（只作标注，等经办人确认）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProposal {
    /// 左侧行键
    pub left_row_key: String,
    /// 右侧行键
    pub right_row_key: String,
    /// 归并类型（same_indicator / duplicate_record）
    pub kind: String,
    /// 置信度 0.0–1.0
    pub confidence: f64,
    /// 判断理由（中文）
    pub reason: String,
    /// 判断依据（可核对的事实）
    pub evidence: Vec<String>,
}

/// 进程内调用序号（审计记录的编号用）
fn next_call_id(batch_ref: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    format!("{}-{:04}", batch_ref, seq)
}

/// 运行时间戳（本地时区，`YYYY-MM-DD HH:mm:ss`）
fn now_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::gateway::{MergeCandidate, MergePair};
    use crate::model::schema::MergeItem;

    /// 一个什么都答不出来的替身端点
    struct FailingGateway {
        code: ErrorCode,
    }

    impl ModelGateway for FailingGateway {
        fn merge_candidates(&self, _request: &MergeRequest) -> Result<MergeResponse, EngineError> {
            Err(EngineError::new(self.code, "端点不可用"))
        }

        fn describe(&self) -> String {
            "测试替身".to_string()
        }
    }

    /// 一个答得合规的替身端点
    struct OkGateway;

    impl ModelGateway for OkGateway {
        fn merge_candidates(&self, _request: &MergeRequest) -> Result<MergeResponse, EngineError> {
            Ok(MergeResponse {
                items: vec![MergeItem {
                    left_row_key: "k1".to_string(),
                    right_row_key: "k2".to_string(),
                    kind: crate::model::schema::MergeKind::SameIndicator,
                    confidence: 0.8,
                    reason: "同一指标".to_string(),
                    evidence: vec!["都含『感染』".to_string()],
                }],
                dropped_duplicate_pairs: 0,
                served_model: Some("stub-model".to_string()),
                usage: Some(crate::model::schema::ModelUsage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_tokens: Some(15),
                }),
            })
        }

        fn describe(&self) -> String {
            "测试替身".to_string()
        }
    }

    fn request() -> MergeRequest {
        MergeRequest::new(vec![MergePair {
            left: MergeCandidate {
                row_key: "k1".to_string(),
                indicator_name: "医院感染例数".to_string(),
                dept_name: "院感科".to_string(),
                reporting_period: "2026-Q3".to_string(),
            },
            right: MergeCandidate {
                row_key: "k2".to_string(),
                indicator_name: "院感例数".to_string(),
                dept_name: "院感科".to_string(),
                reporting_period: "2026-Q3".to_string(),
            },
        }])
    }

    fn ctx() -> CallContext {
        CallContext::new("batch-1", "https://model.internal/v1", "stub-model")
    }

    #[test]
    fn 调用成功时审计带模型名_耗时_token与采纳数() {
        let outcome = run_merge_call(&OkGateway, &request(), &ctx());
        assert!(!outcome.is_degraded());
        let audit = outcome.audit();
        assert_eq!(audit.requested_model, "stub-model");
        assert_eq!(audit.served_model, "stub-model");
        assert_eq!(audit.accepted_pairs, 1);
        assert_eq!(audit.candidate_pairs, 1);
        assert_eq!(audit.total_tokens, Some(15));
        assert!(!audit.degraded);
        assert_eq!(audit.error_code, "");
        assert_eq!(audit.candidate_source, "semantic");
        assert!(audit.call_id.starts_with("batch-1-"));
    }

    #[test]
    fn 端点不可达时降级且错误码与来源等级都对() {
        let outcome = run_merge_call(
            &FailingGateway {
                code: ErrorCode::ModelConnectionFailed,
            },
            &request(),
            &ctx(),
        );
        assert!(outcome.is_degraded());
        let audit = outcome.audit();
        assert!(audit.degraded);
        assert_eq!(audit.error_code, "MODEL_CONNECTION_FAILED");
        assert_eq!(audit.candidate_source, "deterministic");
        assert_eq!(audit.accepted_pairs, 0);
        assert!(outcome.response().is_none());
    }

    #[test]
    fn 超时与响应不合法都归降级_错误码各自保留() {
        for code in [
            ErrorCode::ModelTimeout,
            ErrorCode::ModelResponseInvalid,
            ErrorCode::ModelServerError,
            ErrorCode::ModelBadRequest,
        ] {
            let outcome = run_merge_call(&FailingGateway { code }, &request(), &ctx());
            assert!(outcome.is_degraded(), "{:?} 应当降级", code);
            assert_eq!(outcome.audit().error_code, code.as_str());
            assert_eq!(outcome.audit().candidate_source, "deterministic");
        }
    }

    #[test]
    fn 要求模型可用时降级直接报错_可选时放行() {
        let outcome = run_merge_call(
            &FailingGateway {
                code: ErrorCode::ModelConnectionFailed,
            },
            &request(),
            &ctx(),
        );
        assert!(enforce_requirement(&outcome, ModelRequirement::Optional).is_ok());
        let err = enforce_requirement(&outcome, ModelRequirement::Required).unwrap_err();
        assert_eq!(err.code, ErrorCode::ModelUnavailable);
        assert!(err.message.contains("MODEL_CONNECTION_FAILED"));
    }

    #[test]
    fn 审计记录不含行内容与患者字段() {
        let outcome = run_merge_call(&OkGateway, &request(), &ctx());
        let json = serde_json::to_string(outcome.audit()).unwrap();
        for banned in [
            "住院号",
            "身份证",
            "患者",
            "病案号",
            "indicator_name",
            "dept_name",
            "business_key",
        ] {
            assert!(!json.contains(banned), "审计记录不该出现 {}", banned);
        }
    }

    #[test]
    fn 内存审计收集器留痕每次调用() {
        let mut sink = MemoryAuditSink::new();
        let outcome = run_merge_call(&OkGateway, &request(), &ctx());
        sink.record(outcome.audit()).unwrap();
        assert_eq!(sink.records().len(), 1);
        assert_eq!(sink.last().unwrap().accepted_pairs, 1);
    }

    #[test]
    fn 契约模型段区分未接入_降级_成功三种情形() {
        let idle = ModelStatusSection::not_attempted();
        assert!(!idle.attempted);
        assert!(idle.summary_line().contains("未接入"));

        let degraded = ModelStatusSection::from_outcome(&run_merge_call(
            &FailingGateway {
                code: ErrorCode::ModelTimeout,
            },
            &request(),
            &ctx(),
        ));
        assert!(degraded.attempted);
        assert!(degraded.degraded);
        assert_eq!(degraded.error_code, "MODEL_TIMEOUT");
        assert_eq!(degraded.candidate_source, "deterministic");
        assert!(degraded.summary_line().contains("deterministic"));

        let ok = ModelStatusSection::from_outcome(&run_merge_call(&OkGateway, &request(), &ctx()));
        assert!(!ok.degraded);
        assert_eq!(ok.proposals.len(), 1);
        assert_eq!(ok.proposals[0].kind, "same_indicator");
    }
}
