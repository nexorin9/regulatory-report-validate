//! 模型调用审计的落库层。
//!
//! # 为什么这一层在 `db/` 而不在 `model/`
//!
//! 模型层（[`crate::model`]）有一条硬规矩：**不碰数据库**。它不出现 `rusqlite`、
//! 不出现 `Repo`、不出现 `INSERT` —— 业务层只依赖
//! [`crate::model::ModelGateway`] 这个抽象，模型返回的东西除了结构校验什么也不写。
//!
//! 审计元数据却必须落盘（不然「模型掺和了没有」永远查不出来），所以落库这件事
//! 放在这一层：模型层只定义 [`ModelCallAuditSink`] 这个接口与它的数据结构，
//! 由本模块给它套上数据库实现。**接口在模型层，实现在库层** —— 各守各的规矩。
//!
//! # 表里放什么
//!
//! 只有元数据与计数：模型名、端点、耗时、token 用量、候选对数、采纳数、
//! 是否降级、降级错误码、候选来源等级。
//!
//! **没有任何一行业务内容**：没有指标名称、没有科室名称、没有业务主键，
//! 更没有患者可识别明细。审计表读出来的是「这一轮模型干了什么、成了没成」，
//! 不是「这一轮判了什么」—— 后者去结果契约里看。

use rusqlite::{params, Row};

use crate::db::repo::Repo;
use crate::errors::{EngineError, ErrorCode};
use crate::model::audit::{ModelCallAudit, ModelCallAuditSink};

impl ModelCallAuditSink for Repo {
    /// 记一条模型调用审计。
    ///
    /// 幂等：同一 `call_id` 重复记只留一条（主键冲突时覆盖）——
    /// 重跑同一批校验不该让审计表凭空多出几条一模一样的记录。
    fn record(&mut self, audit: &ModelCallAudit) -> Result<(), EngineError> {
        let sql = r#"
INSERT INTO model_call_audit (
    call_id, ts, batch_ref, endpoint, requested_model, served_model,
    duration_ms, candidate_pairs, accepted_pairs, dropped_duplicate_pairs,
    prompt_tokens, completion_tokens, total_tokens,
    degraded, error_code, error_message, candidate_source
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
ON CONFLICT(call_id) DO UPDATE SET
    duration_ms = excluded.duration_ms,
    accepted_pairs = excluded.accepted_pairs,
    dropped_duplicate_pairs = excluded.dropped_duplicate_pairs,
    prompt_tokens = excluded.prompt_tokens,
    completion_tokens = excluded.completion_tokens,
    total_tokens = excluded.total_tokens,
    degraded = excluded.degraded,
    error_code = excluded.error_code,
    error_message = excluded.error_message,
    candidate_source = excluded.candidate_source
"#;
        self.conn()
            .execute(
                sql,
                params![
                    audit.call_id,
                    audit.ts,
                    audit.batch_ref,
                    audit.endpoint,
                    audit.requested_model,
                    audit.served_model,
                    audit.duration_ms as i64,
                    audit.candidate_pairs as i64,
                    audit.accepted_pairs as i64,
                    audit.dropped_duplicate_pairs as i64,
                    audit.prompt_tokens.map(|v| v as i64),
                    audit.completion_tokens.map(|v| v as i64),
                    audit.total_tokens.map(|v| v as i64),
                    if audit.degraded { 1i64 } else { 0i64 },
                    audit.error_code,
                    audit.error_message,
                    audit.candidate_source,
                ],
            )
            .map_err(|e| {
                EngineError::at(
                    ErrorCode::Db,
                    format!("模型调用审计写入失败：{}", e),
                    "model_call_audit",
                )
            })?;
        Ok(())
    }
}

impl Repo {
    /// 查模型调用审计（按批次过滤；不传批次则查全部）。
    ///
    /// 新的在前 —— 排障时先看最近一次。
    pub fn list_model_audits(
        &self,
        batch_ref: Option<&str>,
    ) -> Result<Vec<ModelCallAudit>, EngineError> {
        let (sql, args): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match batch_ref {
            Some(b) if !b.trim().is_empty() => (
                "SELECT call_id, ts, batch_ref, endpoint, requested_model, served_model, \
                 duration_ms, candidate_pairs, accepted_pairs, dropped_duplicate_pairs, \
                 prompt_tokens, completion_tokens, total_tokens, degraded, error_code, \
                 error_message, candidate_source \
                 FROM model_call_audit WHERE batch_ref = ?1 ORDER BY ts DESC, call_id DESC",
                vec![Box::new(b.trim().to_string())],
            ),
            _ => (
                "SELECT call_id, ts, batch_ref, endpoint, requested_model, served_model, \
                 duration_ms, candidate_pairs, accepted_pairs, dropped_duplicate_pairs, \
                 prompt_tokens, completion_tokens, total_tokens, degraded, error_code, \
                 error_message, candidate_source \
                 FROM model_call_audit ORDER BY ts DESC, call_id DESC",
                vec![],
            ),
        };
        let mut stmt = self.conn().prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_audit(row)?);
        }
        Ok(out)
    }

    /// 最近一次模型调用审计（没有则 None）
    pub fn latest_model_audit(
        &self,
        batch_ref: Option<&str>,
    ) -> Result<Option<ModelCallAudit>, EngineError> {
        Ok(self.list_model_audits(batch_ref)?.into_iter().next())
    }
}

/// 一行库记录 → 一条审计记录
fn row_to_audit(row: &Row<'_>) -> Result<ModelCallAudit, EngineError> {
    let degraded: i64 = row.get(13)?;
    Ok(ModelCallAudit {
        call_id: row.get(0)?,
        ts: row.get(1)?,
        batch_ref: row.get(2)?,
        endpoint: row.get(3)?,
        requested_model: row.get(4)?,
        served_model: row.get(5)?,
        duration_ms: row.get::<_, i64>(6)? as u64,
        candidate_pairs: row.get::<_, i64>(7)? as usize,
        accepted_pairs: row.get::<_, i64>(8)? as usize,
        dropped_duplicate_pairs: row.get::<_, i64>(9)? as usize,
        prompt_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        completion_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
        total_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
        degraded: degraded != 0,
        error_code: row.get(14)?,
        error_message: row.get(15)?,
        candidate_source: row.get(16)?,
    })
}
