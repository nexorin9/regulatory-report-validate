//! 科室主数据目录缓存的落库层。
//!
//! # 为什么这一层在 `db/` 而不在 `push/`
//!
//! 与模型调用审计同一套路：出站客户端（[`crate::push::dept`]）**不碰数据库** ——
//! 它只定义 [`DeptDirectoryCache`] 这个接口，由本模块给它套上数据库实现。
//! **接口在出站层，实现在库层** —— 各守各的规矩。
//!
//! # 表里放什么
//!
//! 机构代码、拉取时间（数据时间）、条目数、目录载荷。
//! 目录里只有科室代码 / 科室名称 / 别名 / 上级科室 —— **没有患者可识别明细**。

use rusqlite::params;

use crate::db::repo::Repo;
use crate::errors::{EngineError, ErrorCode};
use crate::push::dept::{CachedDirectory, DeptDirectoryCache, DirectorySnapshot};

impl DeptDirectoryCache for Repo {
    /// 取上次缓存（按机构代码）。
    ///
    /// 库里没有这一行 → `Ok(None)`（**不是错误**）：「没缓存过」是正常状态，
    /// 由调用方决定降级成「科室主数据未校验」。
    fn load(&self, org_code: &str) -> Result<Option<CachedDirectory>, EngineError> {
        let mut stmt = self.conn().prepare(
            "SELECT org_code, fetched_at, entry_count, payload FROM dept_directory_cache \
             WHERE org_code = ?1",
        )?;
        let mut rows = stmt.query(params![org_code])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(CachedDirectory {
            org_code: row.get(0)?,
            fetched_at: row.get(1)?,
            entry_count: row.get::<_, i64>(2)? as usize,
            payload: row.get(3)?,
        }))
    }

    /// 存本次拉取结果（按机构代码 + 拉取时间）。
    ///
    /// 幂等：同一机构代码重复存只留最新一份（主键冲突时覆盖）——
    /// 缓存表存的是「最近一次成功拉到的目录」，不是历史流水。
    fn save(&mut self, org_code: &str, snapshot: &DirectorySnapshot) -> Result<(), EngineError> {
        let payload = snapshot.to_payload()?;
        self.conn()
            .execute(
                r#"
INSERT INTO dept_directory_cache (
    org_code, fetched_at, entry_count, payload, source, created_at
) VALUES (?1, ?2, ?3, ?4, 'live', ?5)
ON CONFLICT(org_code) DO UPDATE SET
    fetched_at = excluded.fetched_at,
    entry_count = excluded.entry_count,
    payload = excluded.payload,
    source = excluded.source
"#,
                params![
                    org_code,
                    snapshot.fetched_at,
                    snapshot.len() as i64,
                    payload,
                    crate::db::repo::now_iso8601(),
                ],
            )
            .map_err(|e| {
                EngineError::at(
                    ErrorCode::Db,
                    format!("科室目录缓存写入失败：{}", e),
                    "dept_directory_cache",
                )
            })?;
        Ok(())
    }
}

impl Repo {
    /// 库里缓存的科室目录（按机构代码；没有则 None）
    pub fn dept_directory_cache(&self, org_code: &str) -> Result<Option<CachedDirectory>, EngineError> {
        <Repo as DeptDirectoryCache>::load(self, org_code)
    }
}
