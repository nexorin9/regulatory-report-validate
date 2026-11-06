//! 科室主数据目录：科室代码的权威来源。
//!
//! 目录客户端（`GET {HIS_BASE_URL}/api/v1/dept-directory`，见 spec.md 出站报文规范）
//! 在任务 29 落地；本任务先把「查得到 / 查不到 / 查不了」三种结果的语义定死，
//! 上层不依赖具体实现：
//!
//! - **查得到** → 科室代码在目录里，不报问题；
//! - **查不到** → 报「科室主数据不一致」（错误码 `DEPT_NOT_FOUND`），阻断级；
//! - **查不了**（目录未配置 / 拉取失败）→ 跳过一致性检查，结果里标「科室主数据未校验」，
//!   **不静默通过** —— 目录拉不到不等于「科室没问题」。
//!
//! 科室名称写法与主数据不符只提示、不改数（别名命中为准，未命中不阻断），
//! 这条在任务 29 与目录客户端一起落地。

use std::collections::BTreeSet;

use crate::errors::ErrorCode;
use crate::ingest::key;
use crate::ingest::{Field, SourceRow};
use crate::rules::model::Issue;

/// 科室主数据一致性问题规则号
pub const RULE_DEPT_MISMATCH: &str = "R-DEPT-001";

/// 科室名称写法不一致的问题规则号
pub const RULE_DEPT_NAME_MISMATCH: &str = "R-DEPT-002";

/// 科室名称写法不一致的问题类型
pub const DEPT_NAME_ISSUE_KIND: &str = "科室名称写法不一致";

/// 科室名称写法不一致的依据
pub const DEPT_NAME_BASIS: &str = "本产品最小数据格式与字段要求「科室名称」列（须与院内 HIS 科室目录的主数据名称或其别名一致；目录接口 GET {HIS_BASE_URL}/api/v1/dept-directory，见 spec.md 出站报文规范）；写法不符只提示，不改数、不阻断";

/// 科室主数据不一致的依据
///
/// 科室目录型枚举（`R-DEPT-001`）的**唯一**依据出处 —— 规则清单与问题行都用这一条，
/// 两处各写一份必然漂移。
pub const DEPT_BASIS: &str = "本产品最小数据格式与字段要求「科室代码」列（须能在院内 HIS 科室目录匹配到；目录接口 GET {HIS_BASE_URL}/api/v1/dept-directory，见 spec.md 出站报文规范）";

/// 科室代码的期望取值（给科室看）
///
/// 与另外四类值域枚举同一口径：报「不在枚举内」科室改不动，得告诉他该改成什么。
/// 目录型枚举的期望值不是一张固定清单，而是「院内 HIS 科室目录中已登记的科室代码」——
/// 目录拉不到时该维度跳过并标「科室主数据未校验」，不静默通过。
pub const DEPT_EXPECTED_VALUES: &str =
    "院内 HIS 科室目录中已登记的科室代码（目录拉取不到时，本轮标「科室主数据未校验」）";

/// 科室名称写法判定结果。
///
/// **只提示、不阻断**：科室名称写法不一致不影响这一行的数值对不对，
/// 引擎不替科室改名字 —— 改完再报一遍才是真的对不上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeptNameVerdict {
    /// 与主数据名称一致
    Canonical,
    /// 与主数据别名一致（医院自定义后缀等合法写法）
    Alias,
    /// 与主数据、别名都不一致 —— 只提示，不阻断
    Mismatch {
        /// 主数据里的规范名称
        canonical: String,
    },
}

/// 科室主数据目录。
///
/// 实现方：任务 29 的 HIS 目录客户端（本机缓存与测试替身也实现这个 trait）。
pub trait DeptDirectory {
    /// 目录是否可用。拉取失败或未配置时返回 false —— 调用方据此标「科室主数据未校验」。
    fn is_available(&self) -> bool;
    /// 科室代码是否在目录里。
    fn contains(&self, dept_code: &str) -> bool;
    /// 科室名称写法判定。默认实现：目录不提供名称信息 → **无从判定**（返回 `None`）。
    ///
    /// 「无从判定」与「判定为不符」是两回事：前者不报任何问题，后者只提示。
    fn name_verdict(&self, _dept_code: &str, _dept_name: &str) -> Option<DeptNameVerdict> {
        None
    }
}

/// 目录不可用（未配置 / 拉取失败）。
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableDirectory;

impl DeptDirectory for UnavailableDirectory {
    fn is_available(&self) -> bool {
        false
    }

    fn contains(&self, _dept_code: &str) -> bool {
        false
    }
}

/// 内存目录（本机缓存与测试替身用）。
#[derive(Debug, Clone, Default)]
pub struct InMemoryDirectory {
    codes: BTreeSet<String>,
}

impl InMemoryDirectory {
    /// 用一组科室代码建目录
    pub fn new<I, S>(codes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            codes: codes
                .into_iter()
                .map(|c| c.into().trim().to_string())
                .filter(|c| !c.is_empty())
                .collect(),
        }
    }

    /// 目录里的科室代码条数
    pub fn len(&self) -> usize {
        self.codes.len()
    }

    /// 目录是否为空
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
}

impl DeptDirectory for InMemoryDirectory {
    fn is_available(&self) -> bool {
        true
    }

    fn contains(&self, dept_code: &str) -> bool {
        self.codes.contains(dept_code.trim())
    }
}

/// 科室主数据一致性检查的结果
#[derive(Debug, Clone, Default)]
pub struct DeptCheck {
    /// 匹配不到的科室代码产生的问题（逐行一条）
    pub issues: Vec<Issue>,
    /// 是否真的核对过。false = 目录不可用，跳过并标「科室主数据未校验」。
    pub checked: bool,
    /// 匹配不到的科室代码（去重、排序）
    pub unmatched: Vec<String>,
    /// 科室名称写法不符的提示（逐行一条，提示级 —— 只提示，不阻断、不改数）
    pub name_hints: Vec<Issue>,
    /// 名称写法与主数据/别名都不符的科室代码（去重、排序）
    pub name_mismatched: Vec<String>,
}

/// 核对科室代码与目录是否一致。
///
/// 缺科室代码的行不在这里报 —— 「必填缺失」由口径规则报（任务 10），
/// 同一个错不报两遍。
pub fn check(rows: &[SourceRow], directory: &dyn DeptDirectory) -> DeptCheck {
    if !directory.is_available() {
        // 目录不可用：跳过检查并如实标出，不静默通过
        return DeptCheck {
            issues: Vec::new(),
            checked: false,
            unmatched: Vec::new(),
            name_hints: Vec::new(),
            name_mismatched: Vec::new(),
        };
    }

    let mut issues = Vec::new();
    let mut unmatched: BTreeSet<String> = BTreeSet::new();

    for row in rows {
        let code = match row.get_trimmed(Field::DeptCode) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        if directory.contains(code) {
            continue;
        }
        unmatched.insert(code.to_string());
        issues.push(Issue {
            row_uid: key::row_uid(row),
            rule_id: RULE_DEPT_MISMATCH.to_string(),
            issue_kind: "科室主数据不一致".to_string(),
            severity: "阻断".to_string(),
            message: format!(
                "科室代码 {} 在院内 HIS 科室目录中匹配不到（{}）；期望取值：{}",
                code,
                format!("{} 第 {} 行", row.source_ns, row.row_no),
                DEPT_EXPECTED_VALUES
            ),
            basis: DEPT_BASIS.to_string(),
            issue_key: code.to_string(),
            missing_columns: Vec::new(),
            error_code: ErrorCode::DeptNotFound.as_str().to_string(),
        });
    }

    // 科室名称写法：与主数据或别名不符只提示，不改数、不阻断。
    // 代码本身在目录里才谈得上比对名称 —— 代码都匹配不到时，那是上面那条阻断级问题，
    // 不要再叠一条名称提示（同一个错不报两遍）。
    let mut name_hints = Vec::new();
    let mut name_mismatched: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let code = match row.get_trimmed(Field::DeptCode) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        let name = match row.get_trimmed(Field::DeptName) {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        match directory.name_verdict(code, name) {
            Some(DeptNameVerdict::Mismatch { canonical }) => {
                name_mismatched.insert(code.to_string());
                name_hints.push(Issue {
                    row_uid: key::row_uid(row),
                    rule_id: RULE_DEPT_NAME_MISMATCH.to_string(),
                    issue_kind: DEPT_NAME_ISSUE_KIND.to_string(),
                    severity: "提示".to_string(),
                    message: format!(
                        "科室名称写法「{}」与院内 HIS 科室主数据「{}」不一致（科室代码 {} · {}）；只提示，不改数 —— 请核对后自行修改，或把该写法报给信息科登记为别名",
                        name,
                        canonical,
                        code,
                        format!("{} 第 {} 行", row.source_ns, row.row_no)
                    ),
                    basis: DEPT_NAME_BASIS.to_string(),
                    issue_key: code.to_string(),
                    missing_columns: Vec::new(),
                    error_code: ErrorCode::DeptNameMismatch.as_str().to_string(),
                });
            }
            _ => {}
        }
    }

    DeptCheck {
        issues,
        checked: true,
        unmatched: unmatched.into_iter().collect(),
        name_hints,
        name_mismatched: name_mismatched.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(dept: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::IndicatorCode, "MER-HR-01".to_string());
        values.insert(Field::DeptCode, dept.to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, "BK-01".to_string());
        SourceRow {
            source_ns: "表一".to_string(),
            source_file: "表一.csv".to_string(),
            row_no: 3,
            dup_ordinal: 0,
            values,
        }
    }

    #[test]
    fn 目录里有就不报() {
        let dir = InMemoryDirectory::new(["0301", "0302"]);
        let out = check(&[row("0301")], &dir);
        assert!(out.checked);
        assert!(out.issues.is_empty());
        assert!(out.unmatched.is_empty());
    }

    #[test]
    fn 目录里没有就报科室主数据不一致() {
        let dir = InMemoryDirectory::new(["0301"]);
        let out = check(&[row("0999")], &dir);
        assert!(out.checked);
        assert_eq!(out.issues.len(), 1);
        assert_eq!(out.issues[0].error_code, "DEPT_NOT_FOUND");
        assert_eq!(out.issues[0].issue_kind, "科室主数据不一致");
        assert_eq!(out.unmatched, vec!["0999".to_string()]);
    }

    #[test]
    fn 目录不可用时跳过并标未校验() {
        let out = check(&[row("0999")], &UnavailableDirectory);
        assert!(!out.checked, "目录不可用不能算作核对过");
        assert!(out.issues.is_empty());
    }
}
