//! 本机校验库的表结构、迁移与完整性检查。
//!
//! 单文件 SQLite，无服务依赖。库里放六张业务表：
//!
//! | 表 | 放什么 | 谁写 |
//! |---|---|---|
//! | `rows` | 落到本机库的行列级数据（带来源命名空间，多来源互不覆盖） | 校验引擎 |
//! | `issues` | 口径问题（必填缺失 / 枚举越界 / 单位不符 / 公式不符 / 疑似重复） | 口径规则执行器 |
//! | `confirmations` | 人工确认与撤销记录 | 确认流 |
//! | `audit_log` | 审计链（谁在何时确认了哪一行、哪一批推了什么） | 全部写操作 |
//! | `receipts` | 上报网关回执 | 回执解析 |
//! | `submissions` | 上报提交（幂等键 + 载荷摘要 + 修订号） | 上报执行器 |
//!
//! 另有两张支撑表：`run_summaries`（每次命令的运行摘要，`regval status` 读它）与
//! `schema_migrations`（迁移台账）；以及批次汇总的两张表：
//! `batch_summaries`（一个批次一行的**快照** —— 批次计数、汇总值与批次状态，
//! 一次事务写入）与 `batch_summary_history`（append-only 的历史版，
//! 同一修订号只写一次，撞了整次事务回滚）。
//!
//! # 权威键与重复行：为什么唯一索引是「部分索引」
//!
//! 权威键 = `机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键`（spec.md「权威数据源与实体绑定」）。
//! 同一权威键在**一次校验批次内**只能有**一行**权威数据，但**原始来源行必须都留着** ——
//! 重复检测四级漏斗的第一级（L1 确定性键）要在库里比对两行的取值是否一致，
//! 第二级以后还要看归一化值与近似写法。若在落库时就把第二行拒掉，漏斗将无从下手。
//!
//! 因此本层的做法是：
//! - `rows.authority_state` 记录该行的权威归属：`raw`（刚落库，归属未判定）、
//!   `authoritative`（权威行）、`duplicate`（与权威行完全一致）、`pending_manual`（同键异值，待人工判定）；
//! - 唯一索引 `idx_rows_authority` 只约束 `authority_state = 'authoritative'` 的行（SQLite 部分唯一索引），
//!   即「把一行提升为权威行」时，重复的权威键会被数据库直接拒掉；
//! - **何时提升、冲突如何标注**（提升策略、跨表匹配键、科室主数据核对）由实体绑定层负责，
//!   本层只提供约束与原语（[`crate::db::repo::Repo::promote_authoritative`]）。
//!
//! # 为什么唯一索引带 batch_ref
//!
//! 一次 `validate` 把多张科室表并进来判「谁代表这条记录」，结论只对**本批次**有效：
//! 下一期再来一份新表、或被退回后补报，都是新批次，各自解一次归属。
//! 跨批次的「新旧两版同一记录」由上报那条线管 —— 幂等键相同、载荷摘要变了就 `revision + 1`
//! （任务 27），不必靠权威行的唯一性来表达。
//! 所以索引是 `(batch_ref, 权威键五段)` 上的部分唯一索引。
//!
//! # 幂等键与回执的一一对应
//!
//! 幂等键 `sha256(机构代码｜报告期｜指标编号｜科室代码｜业务主键)` 与修订号 `revision` 一起构成
//! 一次提交的身份：订正后载荷摘要变化 → `revision + 1`、业务键不变，旧回执仍可查。
//! 因此 `submissions` 与 `receipts` 都以 `(idempotency_key, revision)` 唯一，
//! 而 `receipts.receipt_id` 自身唯一 —— 这就是「幂等键 ↔ 回执 ID 一一对应」的落法。

use rusqlite::{Connection, ErrorCode as SqliteErrorCode};

use crate::errors::{EngineError, ErrorCode};

/// 库结构版本（迁移到哪一版）。迁移落后时按序补齐，高于此版本则拒绝打开。
pub const SCHEMA_VERSION: i64 = 9;

/// 权威归属状态：`raw` —— 刚落库的原始来源行，归属未判定。
pub const AUTHORITY_RAW: &str = "raw";
/// 权威归属状态：`authoritative` —— 权威行，权威键唯一索引约束的就是这一类。
pub const AUTHORITY_AUTHORITATIVE: &str = "authoritative";
/// 权威归属状态：`duplicate` —— 与权威行取值完全一致（疑似重复，交给重复检测漏斗判定）。
pub const AUTHORITY_DUPLICATE: &str = "duplicate";
/// 权威归属状态：`pending_manual` —— 同键异值，待人工判定，绝不自动二选一。
pub const AUTHORITY_PENDING_MANUAL: &str = "pending_manual";

/// 迁移台账：每个已应用的迁移一行（版本号 + 名称 + 应用时间）。
pub const MIGRATIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    applied_at TEXT    NOT NULL
)"#;

/// 落到本机库的行列级数据。带来源命名空间列：多张科室表同名指标行互不覆盖。
///
/// `org_code` 是机构代码（院内表可带「机构代码」列；缺省为空串，表示单机构口径）。
/// `authority_state` 见模块文档：权威归属状态。
pub const ROWS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS rows (
    id                INTEGER PRIMARY KEY,
    batch_ref         TEXT    NOT NULL,
    source_ns         TEXT    NOT NULL,
    source_file       TEXT    NOT NULL,
    row_no            INTEGER NOT NULL,
    row_uid           TEXT    NOT NULL,
    org_code          TEXT    NOT NULL DEFAULT '',
    authority_state   TEXT    NOT NULL DEFAULT 'raw',
    indicator_code    TEXT,
    indicator_name    TEXT,
    dept_code         TEXT,
    dept_name         TEXT,
    reporting_period  TEXT,
    period_type       TEXT,
    business_key      TEXT,
    numerator         REAL,
    denominator       REAL,
    value             REAL,
    unit              TEXT,
    remark            TEXT,
    numeric_raw       TEXT,
    rule_version      TEXT,
    created_at        TEXT    NOT NULL
)"#;

/// 口径问题。同一行同一规则同一区分键只落一条（唯一索引兜底，不靠应用层自觉）。
///
/// `error_code` 是机器可读错误码（与 `errors.rs` 同一套 code）：口径规则命中时为空，
/// 实体绑定类问题（科室主数据不一致、同权威键异值）带码，便于插件与导出按码分组。
pub const ISSUES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS issues (
    id              INTEGER PRIMARY KEY,
    batch_ref       TEXT    NOT NULL,
    row_uid         TEXT    NOT NULL,
    rule_id         TEXT    NOT NULL,
    issue_kind      TEXT    NOT NULL,
    severity        TEXT    NOT NULL,
    message         TEXT    NOT NULL,
    basis           TEXT    NOT NULL,
    issue_key       TEXT    NOT NULL DEFAULT '',
    missing_columns TEXT    NOT NULL DEFAULT '',
    error_code      TEXT    NOT NULL DEFAULT '',
    created_at      TEXT    NOT NULL,
    UNIQUE (batch_ref, row_uid, rule_id, issue_key)
)"#;

/// 运行摘要：每次命令跑完落一行，供 `regval status` 查「上次跑了什么、结果如何」。
/// 只含指标级监管数据与科室级数据，不落患者可识别明细。
pub const RUN_SUMMARIES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS run_summaries (
    id               INTEGER PRIMARY KEY,
    command          TEXT    NOT NULL,
    batch_ref        TEXT    NOT NULL,
    tables           INTEGER NOT NULL,
    total_rows       INTEGER NOT NULL,
    rows_with_issues INTEGER NOT NULL,
    issue_count      INTEGER NOT NULL,
    elapsed_ms       INTEGER NOT NULL,
    started_at       TEXT    NOT NULL,
    finished_at      TEXT    NOT NULL,
    created_at       TEXT    NOT NULL
)"#;

/// 人工确认记录。确认与撤销都是追加写：撤销不删原记录，只追加一条 `revoke`。
/// 确认人不得为空 —— 空名不入库（`confirmed_by` NOT NULL，由调用方保证非空）。
pub const CONFIRMATIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS confirmations (
    id           INTEGER PRIMARY KEY,
    batch_ref    TEXT    NOT NULL,
    row_uid      TEXT    NOT NULL,
    action       TEXT    NOT NULL,
    confirmed_by TEXT    NOT NULL,
    confirmed_at TEXT    NOT NULL,
    note         TEXT    NOT NULL DEFAULT '',
    revoked_by   TEXT    NOT NULL DEFAULT '',
    revoked_at   TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL
)"#;

/// 审计链：append-only（只追加不修改，更正以新记录表达）。
/// 可按行与按批次回放。记录只含监管指标行与科室级数据，不落患者可识别明细。
pub const AUDIT_LOG_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS audit_log (
    id         INTEGER PRIMARY KEY,
    ts         TEXT    NOT NULL,
    batch_ref  TEXT    NOT NULL,
    row_uid    TEXT    NOT NULL DEFAULT '',
    event      TEXT    NOT NULL,
    actor      TEXT    NOT NULL DEFAULT '',
    detail     TEXT    NOT NULL DEFAULT '',
    created_at TEXT    NOT NULL
)"#;

/// 上报提交：一次提交的身份是 `(idempotency_key, revision)`。
/// 订正后载荷摘要变化 → revision + 1，业务键不变，旧提交与旧回执仍可查。
pub const SUBMISSIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS submissions (
    id               INTEGER PRIMARY KEY,
    batch_ref        TEXT    NOT NULL,
    row_uid          TEXT    NOT NULL,
    idempotency_key  TEXT    NOT NULL,
    payload_digest   TEXT    NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1,
    status           TEXT    NOT NULL DEFAULT 'pending',
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error_code  TEXT    NOT NULL DEFAULT '',
    last_error_field TEXT    NOT NULL DEFAULT '',
    created_at       TEXT    NOT NULL,
    updated_at       TEXT    NOT NULL,
    UNIQUE (idempotency_key, revision)
)"#;

/// 上报回执：`(idempotency_key, revision)` 与 `receipt_id` 一一对应。
pub const RECEIPTS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS receipts (
    id               INTEGER PRIMARY KEY,
    receipt_id       TEXT    NOT NULL,
    idempotency_key  TEXT    NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1,
    batch_ref        TEXT    NOT NULL,
    row_uid          TEXT    NOT NULL,
    status           TEXT    NOT NULL,
    accepted_at      TEXT    NOT NULL DEFAULT '',
    error_code       TEXT    NOT NULL DEFAULT '',
    error_field      TEXT    NOT NULL DEFAULT '',
    message          TEXT    NOT NULL DEFAULT '',
    created_at       TEXT    NOT NULL,
    UNIQUE (idempotency_key, revision)
)"#;

/// 行级状态机状态表：一行监管指标行当前处在生命周期的哪一步。
///
/// - 主键 `(row_uid, file_key)`：`row_uid` 定位到一行，`file_key` 是来源文件的身份
///   （规范化后的路径；文件不存在时退回原字符串）。两个文件行键碰巧相同时不会互相覆盖；
/// - `version` 是**行级乐观锁**：推进状态时用「读到的版本号」做条件更新，
///   抢写的那一方拿到 `ROW_STATE_CONFLICT`，不会把别人的结果盖掉；
/// - `data_digest` 是这一行数据摘要：用来判断「被退回的行有没有被修正过」
///   （摘要变了 = 修正过 → 回到待确认重新确认）。
///
/// 本表只由 `src/state/machine.rs` 写入（`src/` 下别处不得出现对它的写语句）。
pub const ROW_STATES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS row_states (
    row_uid     TEXT    NOT NULL,
    file_key    TEXT    NOT NULL,
    source_file TEXT    NOT NULL DEFAULT '',
    batch_ref   TEXT    NOT NULL DEFAULT '',
    state       TEXT    NOT NULL DEFAULT '待校验',
    version     INTEGER NOT NULL DEFAULT 0,
    data_digest TEXT    NOT NULL DEFAULT '',
    updated_at  TEXT    NOT NULL,
    updated_by  TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL,
    PRIMARY KEY (row_uid, file_key)
)"#;

/// 口径换版确认记录：质控岗确认某个口径版本可以生效。
///
/// 确认人与确认时间**必填** —— 没有人的口径换版，等于没人负责。
/// `slot_key` 为空串表示「整套口径」：该版本对所有口径槽位都算数。
pub const RULE_VERSION_APPROVALS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS rule_version_approvals (
    id           INTEGER PRIMARY KEY,
    to_version   TEXT    NOT NULL,
    from_version TEXT    NOT NULL DEFAULT '',
    slot_key     TEXT    NOT NULL DEFAULT '',
    approver     TEXT    NOT NULL,
    approved_at  TEXT    NOT NULL,
    note         TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL
)"#;

/// 口径版本应用台账：记下**实际用过**哪些口径版本。
///
/// 换版闸门靠它判断「这是不是换版」：某个版本已经用过 → 再出现不算换版；
/// 出现没用过的新版本 → 算换版，要质控岗确认。
pub const APPLIED_RULE_SETS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS applied_rule_sets (
    id               INTEGER PRIMARY KEY,
    rule_set_version TEXT    NOT NULL UNIQUE,
    applied_at       TEXT    NOT NULL,
    note             TEXT    NOT NULL DEFAULT '',
    created_at       TEXT    NOT NULL
)"#;

/// 模型调用审计表：每次模型调用的可审计元数据。
///
/// # 这张表回答什么问题
///
/// 「这一批的疑似重复里，模型掺和了没有？掺和的时候它给的是什么理由？
/// 模型端点挂掉的时候，是什么时候挂的、挂成什么错？」
///
/// 答不上来的话，事后就只能靠人回忆 —— 而「模型说是同一件事」这种结论，
/// 在没留痕的情况下根本没法跟科室解释。
///
/// # 表里放什么、不放什么
///
/// 放：模型名、端点地址、请求耗时、token 用量、候选对数、采纳数、
/// **是否降级**、降级时的错误码、本轮候选来源等级。
///
/// 不放：指标名称、科室名称、业务主键、任何患者可识别明细 ——
/// 这一层压根没有这些字段（见 `crate::model::audit::ModelCallAudit`）。
/// 审计表里只有元数据与计数，读它的人看不到任何一行业务内容。
pub const MODEL_CALL_AUDIT_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS model_call_audit (
    call_id                TEXT    PRIMARY KEY,
    ts                     TEXT    NOT NULL,
    batch_ref              TEXT    NOT NULL,
    endpoint               TEXT    NOT NULL DEFAULT '',
    requested_model        TEXT    NOT NULL DEFAULT '',
    served_model           TEXT    NOT NULL DEFAULT '',
    duration_ms            INTEGER NOT NULL DEFAULT 0,
    candidate_pairs        INTEGER NOT NULL DEFAULT 0,
    accepted_pairs         INTEGER NOT NULL DEFAULT 0,
    dropped_duplicate_pairs INTEGER NOT NULL DEFAULT 0,
    prompt_tokens          INTEGER,
    completion_tokens      INTEGER,
    total_tokens           INTEGER,
    degraded               INTEGER NOT NULL DEFAULT 0,
    error_code             TEXT    NOT NULL DEFAULT '',
    error_message          TEXT    NOT NULL DEFAULT '',
    candidate_source       TEXT    NOT NULL DEFAULT 'none'
)"#;

/// 科室主数据目录缓存：按机构代码存最近一次成功拉到的目录。
///
/// # 为什么缓存要落库
///
/// 科室目录是**权威主数据**，但院内接口也会临时抽风。拉不到的时候如果只有两个选择
/// ——「跳过检查」或「用旧数据」—— 后者明显更接近真相：一份两小时前的科室名单
/// 比「没查过」有用得多。关键是**必须标出数据时间**，让经办人知道这份名单有多旧。
///
/// 一行 = 一个机构代码的一份目录（`org_code` 主键）。`payload` 是目录条目的 JSON 数组，
/// `fetched_at` 是那次拉取的时间 —— 两者合起来就是「这份主数据是什么时候的」。
pub const DEPT_DIRECTORY_CACHE_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS dept_directory_cache (
    org_code    TEXT    PRIMARY KEY,
    fetched_at  TEXT    NOT NULL,
    entry_count INTEGER NOT NULL DEFAULT 0,
    payload     TEXT    NOT NULL,
    source      TEXT    NOT NULL DEFAULT 'live',
    created_at  TEXT    NOT NULL
)"#;

/// 批次汇总（**快照**）：一个批次一行，回答「这批整体算不算数」。
///
/// # 汇总口径（固定四项）
///
/// | 口径 | 定义 | 库里的出处 |
/// |---|---|---|
/// | 已上报行数 | 网关已受理的行数（本次受理 + 幂等命中） | `submissions.status = 已上报` |
/// | 被退回行数 | 网关退回（4xx）的行数 | `submissions.status = 被退回` |
/// | 待重试行数 | 可重试失败（5xx / 超时 / 连不上）的行数 | `submissions.status = 待重试` |
/// | 问题项数 | 该批次问题条数（口径问题 + 上报退回问题） | `issues` 表 |
///
/// 口径的**唯一定义处**是 `crate::push::batch`（业务层）与 `docs/architecture.md`；
/// 本层只负责把算好的值原样存下来，不另算一遍。
///
/// # 这张表是「可回滚」的那部分
///
/// 批末条件回滚时，批次计数与汇总值一起回到**批前状态**（见 `crate::push::executor`）：
/// 整批没跑完就不落账，下次重跑还是从零算起，不会把半批当成一批。
/// **行级回执不受影响** —— 网关侧已受理的事实不可撤销。
pub const BATCH_SUMMARIES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS batch_summaries (
    batch_ref       TEXT    PRIMARY KEY,
    planned_steps   INTEGER NOT NULL DEFAULT 0,
    attempted       INTEGER NOT NULL DEFAULT 0,
    accepted        INTEGER NOT NULL DEFAULT 0,
    idempotent_hit  INTEGER NOT NULL DEFAULT 0,
    reported        INTEGER NOT NULL DEFAULT 0,
    rejected        INTEGER NOT NULL DEFAULT 0,
    retryable       INTEGER NOT NULL DEFAULT 0,
    blocked         INTEGER NOT NULL DEFAULT 0,
    invalid_receipt INTEGER NOT NULL DEFAULT 0,
    not_executed    INTEGER NOT NULL DEFAULT 0,
    issue_count     INTEGER NOT NULL DEFAULT 0,
    sum_value       REAL    NOT NULL DEFAULT 0,
    sum_numerator   REAL    NOT NULL DEFAULT 0,
    sum_denominator REAL    NOT NULL DEFAULT 0,
    batch_status    TEXT    NOT NULL DEFAULT '已校验',
    rule_version    TEXT    NOT NULL DEFAULT '',
    revision        INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL
)"#;

/// 批次汇总历史（**append-only**）：每次落库追加一行。
///
/// 快照表只留最新一版；这张表留全部历史版 —— 汇总被改过几次、每次改了什么，
/// 事后能查清楚（更正以新记录表达，不改旧记录）。
///
/// `(batch_ref, revision)` 唯一：同一修订号只写一次。
/// 重放同一版汇总会撞这个唯一索引，**整次事务回滚** —— 不留半截状态。
pub const BATCH_SUMMARY_HISTORY_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS batch_summary_history (
    id              INTEGER PRIMARY KEY,
    batch_ref       TEXT    NOT NULL,
    revision        INTEGER NOT NULL,
    batch_status    TEXT    NOT NULL,
    planned_steps   INTEGER NOT NULL DEFAULT 0,
    attempted       INTEGER NOT NULL DEFAULT 0,
    accepted        INTEGER NOT NULL DEFAULT 0,
    idempotent_hit  INTEGER NOT NULL DEFAULT 0,
    reported        INTEGER NOT NULL DEFAULT 0,
    rejected        INTEGER NOT NULL DEFAULT 0,
    retryable       INTEGER NOT NULL DEFAULT 0,
    blocked         INTEGER NOT NULL DEFAULT 0,
    invalid_receipt INTEGER NOT NULL DEFAULT 0,
    not_executed    INTEGER NOT NULL DEFAULT 0,
    issue_count     INTEGER NOT NULL DEFAULT 0,
    sum_value       REAL    NOT NULL DEFAULT 0,
    sum_numerator   REAL    NOT NULL DEFAULT 0,
    sum_denominator REAL    NOT NULL DEFAULT 0,
    rule_version    TEXT    NOT NULL DEFAULT '',
    written_at      TEXT    NOT NULL,
    UNIQUE (batch_ref, revision)
)"#;

/// 全部索引声明。幂等：重复执行不重复建。
pub const INDEXES: &[&str] = &[
    // —— rows ——
    "CREATE INDEX IF NOT EXISTS idx_rows_batch ON rows (batch_ref)",
    "CREATE INDEX IF NOT EXISTS idx_rows_uid ON rows (row_uid)",
    // 权威键唯一索引（部分索引：只约束权威行，原始来源行都要留着给重复检测漏斗比对）
    // 约束范围带 batch_ref：一次校验批次内一条权威键只能有一行权威数据（见模块文档）
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_rows_authority ON rows (batch_ref, org_code, reporting_period, indicator_code, dept_code, business_key) WHERE authority_state = 'authoritative'",
    // —— issues ——
    "CREATE INDEX IF NOT EXISTS idx_issues_batch ON issues (batch_ref)",
    "CREATE INDEX IF NOT EXISTS idx_issues_row ON issues (row_uid)",
    // —— run_summaries ——
    "CREATE INDEX IF NOT EXISTS idx_run_summaries_batch ON run_summaries (batch_ref)",
    // —— confirmations ——
    "CREATE INDEX IF NOT EXISTS idx_confirmations_row ON confirmations (batch_ref, row_uid)",
    // —— audit_log ——
    "CREATE INDEX IF NOT EXISTS idx_audit_batch ON audit_log (batch_ref)",
    "CREATE INDEX IF NOT EXISTS idx_audit_row ON audit_log (row_uid)",
    // —— submissions / receipts：幂等键 ↔ 回执 ID 一一对应 ——
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_submissions_idem ON submissions (idempotency_key, revision)",
    "CREATE INDEX IF NOT EXISTS idx_submissions_batch ON submissions (batch_ref)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_receipt ON receipts (receipt_id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_idem ON receipts (idempotency_key, revision)",
    // —— 口径换版：确认记录与口径版本应用台账 ——
    "CREATE INDEX IF NOT EXISTS idx_rule_approvals_version ON rule_version_approvals (to_version)",
    // —— 模型调用审计 ——
    "CREATE INDEX IF NOT EXISTS idx_model_audit_batch ON model_call_audit (batch_ref)",
    // —— 批次汇总：快照一行一批，历史按批次回放 ——
    "CREATE INDEX IF NOT EXISTS idx_batch_summary_history_batch ON batch_summary_history (batch_ref)",
    // —— 行级状态机：按批次与按状态查行状态 ——
    "CREATE INDEX IF NOT EXISTS idx_row_states_batch ON row_states (batch_ref)",
    "CREATE INDEX IF NOT EXISTS idx_row_states_state ON row_states (state)",
];

/// 一条迁移：版本号 + 名称 + 语句清单。
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub statements: &'static [&'static str],
}

/// 迁移序列。**按版本号升序执行，不得跳号**。
///
/// - v1 基础表（rows / issues / run_summaries）与迁移台账；
/// - v2 状态闭环四表（confirmations / audit_log / submissions / receipts）、
///   `rows` 的机构代码与权威归属列、权威键唯一索引与幂等键/回执唯一索引；
/// - v3 实体绑定层落地：权威键唯一索引的约束范围收到批次内（`batch_ref` + 权威键五段），
///   `issues` 补机器可读错误码列；
/// - v4 枚举越界层落地：`rows` 补「报告期类型」列（枚举越界检查的一个取值面）；
/// - v5 口径换版闸门落地：换版确认记录表与口径版本应用台账；
/// - v6 模型调用审计落地：模型调用的可审计元数据表（含是否降级与降级错误码）；
/// - v7 科室主数据目录缓存落地：按机构代码存最近一次成功拉到的科室目录（含数据时间）；
/// - v8 批次汇总落地：汇总快照 + append-only 历史，一次事务写入；
/// - v9 行级状态机落地：行状态表（状态 + 行级版本号 + 数据摘要）。
///
/// v2 / v3 / v4 里的 `ALTER TABLE ... ADD COLUMN` 是给「那一版之前就已经建过库」的老库用的补列语句；
/// 新建库走 v1 的 `CREATE TABLE IF NOT EXISTS` 时列已经在表定义里，重复加列会被忽略
/// （见 [`apply_statement`] 的幂等处理）。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "base_tables",
        statements: &[
            MIGRATIONS_TABLE,
            ROWS_TABLE,
            ISSUES_TABLE,
            RUN_SUMMARIES_TABLE,
            "CREATE INDEX IF NOT EXISTS idx_rows_batch ON rows (batch_ref)",
            "CREATE INDEX IF NOT EXISTS idx_rows_uid ON rows (row_uid)",
            "CREATE INDEX IF NOT EXISTS idx_issues_batch ON issues (batch_ref)",
            "CREATE INDEX IF NOT EXISTS idx_issues_row ON issues (row_uid)",
            "CREATE INDEX IF NOT EXISTS idx_run_summaries_batch ON run_summaries (batch_ref)",
        ],
    },
    Migration {
        version: 2,
        name: "authority_and_state_closure",
        statements: &[
            CONFIRMATIONS_TABLE,
            AUDIT_LOG_TABLE,
            SUBMISSIONS_TABLE,
            RECEIPTS_TABLE,
            "ALTER TABLE rows ADD COLUMN org_code TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE rows ADD COLUMN authority_state TEXT NOT NULL DEFAULT 'raw'",
            "CREATE INDEX IF NOT EXISTS idx_confirmations_row ON confirmations (batch_ref, row_uid)",
            "CREATE INDEX IF NOT EXISTS idx_audit_batch ON audit_log (batch_ref)",
            "CREATE INDEX IF NOT EXISTS idx_audit_row ON audit_log (row_uid)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_rows_authority ON rows (org_code, reporting_period, indicator_code, dept_code, business_key) WHERE authority_state = 'authoritative'",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_submissions_idem ON submissions (idempotency_key, revision)",
            "CREATE INDEX IF NOT EXISTS idx_submissions_batch ON submissions (batch_ref)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_receipt ON receipts (receipt_id)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_receipts_idem ON receipts (idempotency_key, revision)",
        ],
    },
    Migration {
        version: 3,
        name: "entity_binding_layer",
        statements: &[
            // 权威键唯一索引改到批次内：一次校验批次解一次归属，跨批次的新旧版本由幂等键+修订号管
            "DROP INDEX IF EXISTS idx_rows_authority",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_rows_authority ON rows (batch_ref, org_code, reporting_period, indicator_code, dept_code, business_key) WHERE authority_state = 'authoritative'",
            // 口径问题带机器可读错误码（实体绑定类问题用）
            "ALTER TABLE issues ADD COLUMN error_code TEXT NOT NULL DEFAULT ''",
        ],
    },
    Migration {
        version: 4,
        name: "enum_dimension_layer",
        statements: &[
            // 报告期类型（年报 / 半年报 / 季报 / 月报 / 实时报）：枚举越界检查的一个取值面
            "ALTER TABLE rows ADD COLUMN period_type TEXT",
        ],
    },
    Migration {
        version: 5,
        name: "caliber_switch_gate",
        statements: &[
            RULE_VERSION_APPROVALS_TABLE,
            APPLIED_RULE_SETS_TABLE,
            "CREATE INDEX IF NOT EXISTS idx_rule_approvals_version ON rule_version_approvals (to_version)",
        ],
    },
    Migration {
        version: 6,
        name: "model_call_audit",
        statements: &[
            MODEL_CALL_AUDIT_TABLE,
            "CREATE INDEX IF NOT EXISTS idx_model_audit_batch ON model_call_audit (batch_ref)",
        ],
    },
    Migration {
        version: 7,
        name: "dept_directory_cache",
        statements: &[DEPT_DIRECTORY_CACHE_TABLE],
    },
    Migration {
        version: 8,
        name: "batch_summary_transaction",
        statements: &[
            BATCH_SUMMARIES_TABLE,
            BATCH_SUMMARY_HISTORY_TABLE,
            "CREATE INDEX IF NOT EXISTS idx_batch_summary_history_batch ON batch_summary_history (batch_ref)",
        ],
    },
    Migration {
        version: 9,
        name: "row_state_machine",
        statements: &[
            ROW_STATES_TABLE,
            "CREATE INDEX IF NOT EXISTS idx_row_states_batch ON row_states (batch_ref)",
            "CREATE INDEX IF NOT EXISTS idx_row_states_state ON row_states (state)",
        ],
    },
];

/// 迁移执行结果。
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// 执行前的库版本
    pub from_version: i64,
    /// 执行后的库版本
    pub to_version: i64,
    /// 本次实际应用的迁移（版本 + 名称）
    pub applied: Vec<(i64, &'static str)>,
}

/// 库里当前的结构版本。没有迁移台账表 → 0（老库）。
pub fn current_version(conn: &Connection) -> Result<i64, EngineError> {
    let has_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
        [],
        |r| r.get(0),
    )?;
    if has_table == 0 {
        return Ok(0);
    }
    let version: Option<i64> =
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })?;
    Ok(version.unwrap_or(0))
}

/// 按序执行迁移。
///
/// - 版本落后 → 从「当前版本 + 1」起按序补齐，每条迁移单独一个事务；
/// - 版本已最新 → 什么都不做（**幂等**：重复执行不重复建表、不重复加索引）；
/// - 版本高于本引擎支持的 `SCHEMA_VERSION` → 报错拒绝打开，不静默降级、不重建空库。
pub fn migrate(conn: &Connection) -> Result<MigrationReport, EngineError> {
    let from = current_version(conn)?;
    if from > SCHEMA_VERSION {
        return Err(EngineError::at(
            ErrorCode::DbVersionUnsupported,
            format!(
                "状态库结构版本 {} 高于本引擎支持的 {}，拒绝打开（不降级、不重建空库）",
                from, SCHEMA_VERSION
            ),
            "schema_migrations.version",
        ));
    }

    let mut report = MigrationReport {
        from_version: from,
        to_version: from,
        applied: Vec::new(),
    };

    for migration in MIGRATIONS.iter().filter(|m| m.version > from) {
        let tx = conn.unchecked_transaction().map_err(|e| {
            EngineError::at(
                ErrorCode::DbMigrationFailed,
                format!(
                    "迁移 v{}（{}）开启事务失败：{}",
                    migration.version, migration.name, e
                ),
                format!("version={}", migration.version),
            )
        })?;

        for stmt in migration.statements {
            apply_statement(&tx, stmt).map_err(|e| {
                EngineError::at(
                    ErrorCode::DbMigrationFailed,
                    format!(
                        "迁移 v{}（{}）执行失败：{}",
                        migration.version, migration.name, e
                    ),
                    format!(
                        "version={} statement={}",
                        migration.version,
                        first_line(stmt)
                    ),
                )
            })?;
        }

        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                migration.version,
                migration.name,
                crate::db::repo::now_iso8601()
            ],
        )
        .map_err(|e| {
            EngineError::at(
                ErrorCode::DbMigrationFailed,
                format!("迁移 v{} 记台账失败：{}", migration.version, e),
                format!("version={}", migration.version),
            )
        })?;

        tx.commit().map_err(|e| {
            EngineError::at(
                ErrorCode::DbMigrationFailed,
                format!("迁移 v{} 提交失败：{}", migration.version, e),
                format!("version={}", migration.version),
            )
        })?;

        report.applied.push((migration.version, migration.name));
        report.to_version = migration.version;
    }

    Ok(report)
}

/// 执行一条迁移语句。
///
/// 幂等处理：`ALTER TABLE ... ADD COLUMN` 在列已存在时报「duplicate column name」——
/// 说明这条补列语句对本库已经生效过，忽略即可（老库走 v1 建表时列已在表定义里，
/// v2 再补一次就会命中这里）。
fn apply_statement(conn: &Connection, stmt: &str) -> Result<(), rusqlite::Error> {
    match conn.execute_batch(stmt) {
        Ok(()) => Ok(()),
        Err(e) => {
            if is_duplicate_column_error(&e) {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// 是否是「列已存在」错误
fn is_duplicate_column_error(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(err, Some(msg)) => {
            err.code == SqliteErrorCode::Unknown && msg.contains("duplicate column name")
        }
        _ => false,
    }
}

/// 语句首行（报错时定位用）
fn first_line(stmt: &str) -> String {
    stmt.lines().next().unwrap_or("").trim().to_string()
}

/// 完整性检查：先确认「这是不是一个可读的 SQLite 库」，再跑 `PRAGMA quick_check`。
///
/// 损坏时报 [`ErrorCode::DbCorrupted`] 并指出恢复路径，**不静默重建空库**。
pub fn check_integrity(conn: &Connection, path: &str) -> Result<(), EngineError> {
    // 1. 能不能读表清单 —— 非 SQLite 文件会在这里报「file is not a database」
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| {
        EngineError::at(
            ErrorCode::DbCorrupted,
            format!("状态库文件不是有效的校验库（读取表清单失败：{}）", e),
            path.to_string(),
        )
    })?;

    // 2. 完整性 —— quick_check 返回 "ok" 才算通过
    let result: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| {
            EngineError::at(
                ErrorCode::DbCorrupted,
                format!("状态库完整性检查无法执行：{}", e),
                path.to_string(),
            )
        })?;
    if result.trim() != "ok" {
        return Err(EngineError::at(
            ErrorCode::DbCorrupted,
            format!("状态库完整性检查未通过：{}", result.trim()),
            path.to_string(),
        ));
    }
    Ok(())
}

/// 库里的表名清单（迁移幂等性验收用：跑两次，集合必须一致）
pub fn table_names(conn: &Connection) -> Result<Vec<String>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 库里的索引名清单（迁移幂等性验收用）
pub fn index_names(conn: &Connection) -> Result<Vec<String>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 库里的列名清单（表结构快照用）
pub fn column_names(conn: &Connection, table: &str) -> Result<Vec<String>, EngineError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn 迁移版本号连续且与常量一致() {
        assert_eq!(
            MIGRATIONS.last().map(|m| m.version),
            Some(SCHEMA_VERSION),
            "迁移序列的最后一条版本必须等于 SCHEMA_VERSION"
        );
        for pair in MIGRATIONS.windows(2) {
            assert_eq!(
                pair[0].version + 1,
                pair[1].version,
                "迁移版本号必须连续，不得跳号"
            );
        }
    }

    #[test]
    fn 空库迁移到最新版本且台账有记录() {
        let conn = mem();
        let report = migrate(&conn).unwrap();
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, SCHEMA_VERSION);
        assert_eq!(report.applied.len(), MIGRATIONS.len());
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn 迁移连跑两次结构一致() {
        let conn = mem();
        migrate(&conn).unwrap();
        let tables_1 = table_names(&conn).unwrap();
        let indexes_1 = index_names(&conn).unwrap();

        let report = migrate(&conn).unwrap();
        assert!(report.applied.is_empty(), "第二次迁移不应再应用任何迁移");

        assert_eq!(table_names(&conn).unwrap(), tables_1);
        assert_eq!(index_names(&conn).unwrap(), indexes_1);
    }

    #[test]
    fn 业务表齐备() {
        let conn = mem();
        migrate(&conn).unwrap();
        let tables = table_names(&conn).unwrap();
        for want in [
            "rows",
            "issues",
            "confirmations",
            "audit_log",
            "receipts",
            "submissions",
        ] {
            assert!(tables.contains(&want.to_string()), "缺业务表 {}", want);
        }
    }

    #[test]
    fn 版本高于引擎时报错不降级() {
        let conn = mem();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![SCHEMA_VERSION + 5, "from_the_future", "2099-01-01 00:00:00"],
        )
        .unwrap();
        let err = migrate(&conn).unwrap_err();
        assert_eq!(err.code, ErrorCode::DbVersionUnsupported);
    }
}
