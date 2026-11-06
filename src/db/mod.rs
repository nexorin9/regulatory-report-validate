//! 本机校验库（单文件 SQLite，无服务依赖）。
//!
//! 落库语义取自源仓库的「一次事务批量落库」：要么整批写入成功，要么整批不入库，
//! 中途失败不留半截状态（Task 5 起在此基础上加权威键唯一索引与迁移版本号）。

pub mod batch_summary;
pub mod dept_cache;
pub mod journal;
pub mod model_audit;
pub mod recover;
pub mod repo;
pub mod schema;
