//! 模型接入层 —— 语义归并（重复检测第四级）的**唯一**模型入口。
//!
//! # 这一层只做两件事
//!
//! ① **提交归并候选**：把「这两行是不是同一件事」问给模型；
//! ② **校验结构化回答**：回答必须通过结构校验才算数。
//!
//! # 这一层不做什么（是别处的责任，不是这层的）
//!
//! - **不推进任何业务状态**：合并 / 上报由经办人勾选确认后由引擎执行；
//! - **不写本机校验库**：模型层的返回只经过结构校验，不会落任何业务表
//!   （模型层源码里不出现 `rusqlite` / `Repo` / `INSERT`，有静态用例盯着）；
//! - **不决定「合不合并」**：模型给的是候选，判定在漏斗与人工确认里。
//!
//! # provider 中立
//!
//! 业务层（[`crate::dupe`]）只依赖 [`gateway::ModelGateway`] 这个 trait，
//! 不依赖任何厂商 SDK，也不直接碰 HTTP 客户端 —— 协议细节全在 [`openai`]
//! （OpenAI-compatible 形态）里，换端点不改业务层。
//!
//! # 凭据与超时
//!
//! `OPENAI_BASE_URL` / `OPENAI_API_KEY` / `OPENAI_MODEL` / `OPENAI_TIMEOUT_SECS`
//! 四项全部读环境变量，程序内不硬编码、不落盘。缺项报 `CONFIG_MISSING` 并点名，
//! 不用默认值兜底 —— 拿一个猜出来的地址去连模型端点，只会把「连不上」变成
//! 「连上了别的东西」。
//!
//! # 端点兼容说明
//!
//! 协议取 OpenAI-compatible 的 `POST {base_url}/chat/completions` 形态。
//! 请求带 `response_format: {"type":"json_object"}`（结构化输出的标准做法）；
//! 若端点不支持该字段并就此返回 4xx，客户端会**去掉该字段重试一次**
//! （系统提示里仍然要求只回 JSON），仍失败才报 `MODEL_BAD_REQUEST`，
//! 并把端点原话带在错误里 —— 不静默改协议，也不静默吞掉端点的抱怨。

pub mod audit;
pub mod gateway;
pub mod openai;
pub mod schema;

pub use audit::{
    enforce_requirement, run_merge_call, CallContext, CandidateSource, MemoryAuditSink,
    ModelCallAudit, ModelCallAuditSink, ModelCallOutcome, ModelProposal, ModelRequirement,
    ModelStatusSection,
};
pub use gateway::{
    MergeCandidate, MergePair, MergeRequest, ModelGateway, MODEL_REQUEST_SCHEMA_VERSION,
};
pub use openai::{ModelConfig, OpenAiModelGateway};
pub use schema::{MergeItem, MergeKind, MergeResponse, ModelUsage};
