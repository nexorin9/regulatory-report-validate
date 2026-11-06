//! 卫健上报校验系统 —— 引擎库。
//!
//! 主路径（对应 spec.md「产品主路径核验」）：
//! `读表 → 落本机库 → 口径规则装载与执行 → 结果契约 JSON`。
//!
//! 分工：
//! - [`ingest`]：读 Excel/CSV，把表头映射到业务字段（缺必填列报错并点名，不猜列）
//! - [`ingest::key`]：行键 / 业务主键 / 跨表匹配键 / 权威键
//! - [`bind`]：实体绑定层（同权威键歧义拒绝合并、跨表对齐、科室主数据核对）
//! - [`dupe`]：重复检测四级漏斗（确定性键 → 归一化 → 编辑距离 → 模型语义归并；只标疑似，不合并不推送）
//!   - [`dupe::run_funnel`]：漏斗编排层（逐级累加只增不删、每对行记住是哪一级判出来的）
//!   - [`dupe::eval`]：带人工标注的对照基准集与逐级召回率 / 精确率评估
//! - [`compare`]：与基线方案的可复现对比（重复检测轴 + 问题发现轴；基线是尺子，不进主路径）
//!   - [`compare::naive`]：朴素确定性基线（只按四段键判重复 —— 多数医院现在的做法）
//! - [`db`]：本机校验库（单文件 SQLite），一次事务批量落库，成或全不成
//! - [`rules`]：版本化口径规则 + 参数化执行器（规则 SQL 以命名参数绑定执行，不拼字符串）
//! - [`report`]：报告与举证层（问题清单汇总 + 每行依据举证包）
//! - [`push`]：上报侧（幂等键与载荷摘要：这条记录是谁、这份载荷有没有变）
//!   - [`push::client`]：院内卫健上报网关出站客户端（真实协议 + 状态码语义分类）
//!   - [`push::program`]：上报提交计划编译器（条件链：第 i 步只在第 i-1 步成功后执行）
//!   - [`push::executor`]：条件批量执行器（失败分类 + 批末条件回滚，只回滚批次汇总）
//!   - [`push::receipt`]：回执解析与行级状态推进（回执是状态推进的唯一依据；
//!     幂等键 ↔ 回执 ID 一一对应）
//!   - [`push::retry`]：退避重试与断点续跑（5xx / 超时退避重试，4xx 不重试；
//!     已报上去的那一版不重推，订正后重报按修订号判定）
//!   - [`push::status`]：行级上报状态取值（已上报 / 被退回 / 待重试 …）的单一出处
//! - [`contract`]：引擎与表格插件之间的 JSON 契约（带 `schema_version`，版本不符拒绝写回）
//! - [`writeback`]：写回计划（写哪些格子、写什么值；只碰新列，原表列与公式不动）
//! - [`config`]：环境变量配置（凭据只从环境变量读，缺项报错不兜底）
//! - [`errors`]：机器可读错误码（code + 中文说明 + 处理建议 + 出错位置）
//! - [`log`]：结构化 JSON 行日志与运行摘要（摘要落状态库，`regval status` 可见）
//! - [`doctor`]：运行环境与凭据逐项自检
//! - [`model`]：模型接入层（provider 中立的 [`model::ModelGateway`] + OpenAI-compatible 客户端）
//!   - [`model::schema`]：模型响应的结构化校验（校验不过，整次调用不可用）
//! - [`state`]：状态与确认层
//!   - [`state::machine`]：行级状态机（合法迁移表 + 单一入口 + 行级乐观锁）
//! - [`ui`]：界面层（表格内插件的视觉 token —— 宏与文档共用同一份定义）

pub mod bind;
pub mod compare;
pub mod config;
pub mod contract;
pub mod db;
pub mod doctor;
pub mod dupe;
pub mod errors;
pub mod ingest;
pub mod log;
pub mod model;
pub mod push;
pub mod report;
pub mod rules;
pub mod state;
pub mod ui;
pub mod writeback;
