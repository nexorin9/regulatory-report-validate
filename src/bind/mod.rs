//! 实体绑定层：把行绑到业务记录，判定权威归属，处理同键歧义。
//!
//! 这一层回答四个问题：
//!
//! 1. **这行是谁** —— 算出四层键（行键 / 业务主键 / 跨表匹配键 / 权威键，见
//!    [`crate::ingest::key`]）；
//! 2. **同一权威键下有多行怎么办** —— 取值**完全一致** → 标「疑似重复」，交重复检测漏斗
//!    （任务 20）判定；取值**不同** → 标「待人工判定」，**绝不二选一合并**；
//! 3. **跨表怎么对齐** —— 同一匹配键（指标编号 + 科室代码 + 报告期）下的行按业务主键再分，
//!    找出「同一条业务记录在几张来源表里各出现过一次」；
//! 4. **科室代码是不是真的存在** —— 在院内 HIS 科室目录里匹配不到 → 报「科室主数据不一致」
//!    （错误码 `DEPT_NOT_FOUND`）；目录拉不到 → 标「科室主数据未校验」，**不静默通过**。
//!
//! # 为什么「同键异值」不自动合并
//!
//! 同一权威键下两行填报值不同，可能是重复填报被改过、可能是两个科室填了同一条记录、
//! 也可能是其中一个写错了。引擎分不出是哪种 —— 二选一就是替业务做决定。
//! 所以这里只标状态、不动数，把判定留给经办人（确认流见任务 36）。

pub mod dept;

use std::collections::{BTreeMap, BTreeSet};

use crate::db::schema::{
    AUTHORITY_AUTHORITATIVE, AUTHORITY_DUPLICATE, AUTHORITY_PENDING_MANUAL, AUTHORITY_RAW,
};
use crate::ingest::key;
use crate::ingest::{Field, SourceRow};
use crate::rules::model::Issue;

use dept::{DeptCheck, DeptDirectory};

/// 同权威键下取值不同的问题规则号
pub const RULE_PENDING_MANUAL: &str = "R-BIND-001";

/// 同权威键歧义的依据
pub const BIND_BASIS: &str = "本产品最小数据格式与字段要求（权威键 = 机构代码 + 报告期 + 指标编号 + 科室代码 + 业务主键；跨表匹配键 = 指标编号 + 科室代码 + 报告期）；同一权威键下取值不同的行一律标「待人工判定」，引擎不自动二选一合并";

/// 权威归属状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityState {
    /// 刚落库、归属未判定（键要素不全的行留在这一档）
    Raw,
    /// 权威行：这一条业务记录在本批次里由它代表
    Authoritative,
    /// 疑似重复：与权威行取值完全一致，交重复检测漏斗判定
    Duplicate,
    /// 待人工判定：同权威键下取值不同，绝不自动二选一
    PendingManual,
}

impl AuthorityState {
    /// 写进本机库 `rows.authority_state` 的取值
    pub fn as_str(self) -> &'static str {
        match self {
            AuthorityState::Raw => AUTHORITY_RAW,
            AuthorityState::Authoritative => AUTHORITY_AUTHORITATIVE,
            AuthorityState::Duplicate => AUTHORITY_DUPLICATE,
            AuthorityState::PendingManual => AUTHORITY_PENDING_MANUAL,
        }
    }

    /// 给业务人员看的中文名
    pub fn zh(self) -> &'static str {
        match self {
            AuthorityState::Raw => "归属未判定",
            AuthorityState::Authoritative => "权威行",
            AuthorityState::Duplicate => "疑似重复",
            AuthorityState::PendingManual => "待人工判定",
        }
    }
}

/// 一行的绑定结果
#[derive(Debug, Clone)]
pub struct BoundRow {
    /// 四层键
    pub keys: key::RowKeys,
    pub source_ns: String,
    pub source_file: String,
    pub row_no: usize,
    /// 权威归属
    pub state: AuthorityState,
    /// 与本行取值完全一致的其他行（疑似重复的对照行）
    pub same_value_peers: Vec<String>,
    /// 与本行同权威键但取值不同的其他行（待人工判定的对照行）
    pub conflicting_peers: Vec<String>,
}

impl BoundRow {
    /// 行的可读引用（"表名 第 N 行"）：给经办人看，不是一串哈希
    pub fn reference(&self) -> String {
        format!("{} 第 {} 行", self.source_ns, self.row_no)
    }
}

/// 一条业务记录在跨表对齐后的样子
#[derive(Debug, Clone)]
pub struct AlignedRecord {
    /// 业务主键
    pub business_key: String,
    /// 各来源表里的行引用
    pub occurrences: Vec<String>,
    /// 是否跨了多张来源表
    pub cross_table: bool,
}

/// 绑定结果
#[derive(Debug, Clone, Default)]
pub struct BindingOutcome {
    /// 逐行绑定结果
    pub rows: Vec<BoundRow>,
    /// 绑定层产生的问题（待人工判定、科室主数据不一致）
    pub issues: Vec<Issue>,
    /// 行键 → 「疑似重复项」列取值
    pub suspected: BTreeMap<String, String>,
    /// 跨表匹配键 → 对齐后的业务记录
    pub aligned: BTreeMap<String, Vec<AlignedRecord>>,
    /// 科室主数据是否真的核对过（false = 目录不可用，结果里标「科室主数据未校验」）
    pub dept_master_checked: bool,
    /// 匹配不到的科室代码（去重、排序）
    pub dept_unmatched: Vec<String>,
    /// 科室名称写法与主数据/别名都不符的科室代码（去重、排序；只提示，不阻断）
    pub dept_name_mismatched: Vec<String>,
}

impl BindingOutcome {
    /// 权威归属落库用的赋值清单（行键 → 归属状态）
    pub fn authority_assignments(&self) -> Vec<(String, &'static str)> {
        self.rows
            .iter()
            .map(|r| (r.keys.row_uid.clone(), r.state.as_str()))
            .collect()
    }

    /// 跨表对齐命中的业务记录数（同一匹配键下出现在多张来源表里的记录）
    pub fn cross_table_record_count(&self) -> usize {
        self.aligned
            .values()
            .flat_map(|records| records.iter())
            .filter(|r| r.cross_table)
            .count()
    }
}

/// 绑定一批行。
///
/// `directory` 是科室主数据目录；目录不可用时不做科室一致性检查，
/// 结果里以 `dept_master_checked = false` 标出（不静默通过）。
pub fn bind(rows: &[SourceRow], directory: &dyn DeptDirectory) -> BindingOutcome {
    // 1. 每行算键
    let mut bound: Vec<BoundRow> = Vec::with_capacity(rows.len());
    for row in rows {
        bound.push(BoundRow {
            keys: key::keys(row),
            source_ns: row.source_ns.clone(),
            source_file: row.source_file.clone(),
            row_no: row.row_no,
            state: AuthorityState::Raw,
            same_value_peers: Vec::new(),
            conflicting_peers: Vec::new(),
        });
    }

    // 2. 按权威键分组（键要素不全的行不参与绑定）
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        if !key::is_bindable(row) {
            continue;
        }
        groups.entry(key::authority_key(row)).or_default().push(i);
    }

    // 3. 逐组判定权威归属
    let mut issues: Vec<Issue> = Vec::new();
    for (authority_key, members) in &groups {
        let signatures: Vec<String> = members.iter().map(|&i| value_signature(&rows[i])).collect();
        let all_same = signatures.windows(2).all(|w| w[0] == w[1]);

        if all_same {
            // 取值完全一致：一行为权威行，其余标「疑似重复」，交重复检测漏斗（任务 20）判定
            let mut ordered = members.clone();
            ordered.sort_by(|a, b| bound[*a].keys.row_uid.cmp(&bound[*b].keys.row_uid));
            let authority = ordered[0];
            bound[authority].state = AuthorityState::Authoritative;
            for &i in ordered.iter().skip(1) {
                let authority_ref = bound[authority].reference();
                let duplicate_ref = bound[i].reference();
                bound[i].state = AuthorityState::Duplicate;
                bound[i].same_value_peers.push(authority_ref);
                bound[authority].same_value_peers.push(duplicate_ref);
            }
        } else {
            // 同键异值：整组都标「待人工判定」，绝不二选一合并
            for &i in members {
                bound[i].state = AuthorityState::PendingManual;
                for &j in members {
                    if i != j {
                        let peer_ref = bound[j].reference();
                        bound[i].conflicting_peers.push(peer_ref);
                    }
                }
                issues.push(pending_manual_issue(
                    &bound[i],
                    &rows[i],
                    members.len(),
                    authority_key,
                ));
            }
        }
    }

    // 4. 「疑似重复项」列取值（权威行也标出来，方便经办人一眼看到成对的行）
    let mut suspected: BTreeMap<String, String> = BTreeMap::new();
    for row in &bound {
        match row.state {
            AuthorityState::Duplicate => {
                suspected.insert(
                    row.keys.row_uid.clone(),
                    format!(
                        "疑似重复：与 {} 取值完全一致",
                        row.same_value_peers.join("、")
                    ),
                );
            }
            AuthorityState::Authoritative if !row.same_value_peers.is_empty() => {
                suspected.insert(
                    row.keys.row_uid.clone(),
                    format!(
                        "疑似重复：本权威键下另有 {} 行取值完全一致（{}）",
                        row.same_value_peers.len(),
                        row.same_value_peers.join("、")
                    ),
                );
            }
            _ => {}
        }
    }

    // 5. 跨表对齐：同匹配键下按业务主键再分
    let aligned = align_across_tables(rows, &bound);

    // 6. 科室主数据核对
    //    代码匹配不到 → 阻断级「科室主数据不一致」；
    //    名称写法与主数据/别名不符 → 提示级「科室名称写法不一致」（只提示，不改数）。
    let dept: DeptCheck = dept::check(rows, directory);
    issues.extend(dept.issues);
    issues.extend(dept.name_hints);

    BindingOutcome {
        rows: bound,
        issues,
        suspected,
        aligned,
        dept_master_checked: dept.checked,
        dept_unmatched: dept.unmatched,
        dept_name_mismatched: dept.name_mismatched,
    }
}

/// 跨表对齐：同一匹配键下的行按业务主键再分，得到「一条业务记录在几张表里各出现过一次」。
pub fn align_across_tables(
    rows: &[SourceRow],
    bound: &[BoundRow],
) -> BTreeMap<String, Vec<AlignedRecord>> {
    let mut by_match: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        if !key::is_bindable(row) {
            continue;
        }
        by_match.entry(key::match_key(row)).or_default().push(i);
    }

    let mut out: BTreeMap<String, Vec<AlignedRecord>> = BTreeMap::new();
    for (match_key, members) in by_match {
        let mut by_business: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in &members {
            by_business
                .entry(key::business_key(&rows[i]))
                .or_default()
                .push(i);
        }
        let mut records = Vec::new();
        for (business_key, group) in by_business {
            let occurrences: Vec<String> = group.iter().map(|&i| bound[i].reference()).collect();
            let namespaces: BTreeSet<&str> =
                group.iter().map(|&i| bound[i].source_ns.as_str()).collect();
            records.push(AlignedRecord {
                business_key,
                occurrences,
                cross_table: namespaces.len() > 1,
            });
        }
        out.insert(match_key, records);
    }
    out
}

/// 取值签名：判断「两行取值是否完全一致」用。
///
/// **与重复检测漏斗共用同一份**（[`crate::dupe::value_signature`]）——
/// 两处各写一份，同一对行就会在两个环节得出两个结论
/// （一处说「疑似重复」、一处说「待人工判定」），经办人不知道该信哪个。
///
/// 数值列按数值比（`12` 与 `12.0` 是同一个数），比不了数时按原文比
/// （`abc` 与空串是两回事，不该当成一致），单位按别名表归一后比
/// （`％` 与 `%`、`例数` 与 `例` 是同一个单位）。
pub fn value_signature(row: &SourceRow) -> String {
    crate::dupe::value_signature(row)
}

/// 待人工判定的问题
fn pending_manual_issue(
    bound: &BoundRow,
    row: &SourceRow,
    group_size: usize,
    authority_key: &str,
) -> Issue {
    Issue {
        row_uid: bound.keys.row_uid.clone(),
        rule_id: RULE_PENDING_MANUAL.to_string(),
        issue_kind: AuthorityState::PendingManual.zh().to_string(),
        severity: "阻断".to_string(),
        message: format!(
            "同一权威键（{}）下有 {} 行且取值不同（{}），未自动合并，待人工判定",
            describe_authority_key(row),
            group_size,
            bound.conflicting_peers.join("、")
        ),
        basis: BIND_BASIS.to_string(),
        issue_key: authority_key.to_string(),
        missing_columns: Vec::new(),
        error_code: crate::errors::ErrorCode::AuthorityKeyConflict
            .as_str()
            .to_string(),
    }
}

/// 权威键的可读写法（给经办人看，不是一串拼接符）
fn describe_authority_key(row: &SourceRow) -> String {
    format!(
        "机构代码={}｜报告期={}｜指标编号={}｜科室代码={}｜业务主键={}",
        text(row, Field::OrgCode),
        text(row, Field::ReportingPeriod),
        text(row, Field::IndicatorCode),
        text(row, Field::DeptCode),
        text(row, Field::BusinessKey)
    )
}

fn text(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dept::UnavailableDirectory;

    fn row(source_ns: &str, row_no: usize, business_key: &str, value: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-HR-01".to_string());
        values.insert(Field::IndicatorName, "病案首页完整率".to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, business_key.to_string());
        values.insert(Field::Value, value.to_string());
        values.insert(Field::Unit, "%".to_string());
        SourceRow {
            source_ns: source_ns.to_string(),
            source_file: format!("{}.csv", source_ns),
            row_no,
            dup_ordinal: 0,
            values,
        }
    }

    fn bind_rows(rows: &mut Vec<SourceRow>) -> BindingOutcome {
        key::assign_dup_ordinals(rows);
        bind(rows, &UnavailableDirectory)
    }

    #[test]
    fn 同键异值全部标待人工判定且不设权威行() {
        let mut rows = vec![row("表一", 1, "BK-01", "50"), row("表二", 1, "BK-01", "60")];
        let out = bind_rows(&mut rows);
        assert!(out
            .rows
            .iter()
            .all(|r| r.state == AuthorityState::PendingManual));
        assert_eq!(out.issues.len(), 2);
        assert!(out.issues.iter().all(|i| i.issue_kind == "待人工判定"));
        assert!(out
            .issues
            .iter()
            .all(|i| i.error_code == "AUTHORITY_KEY_CONFLICT"));
    }

    #[test]
    fn 取值完全一致时一行为权威其余标疑似重复() {
        let mut rows = vec![row("表一", 1, "BK-01", "50"), row("表二", 1, "BK-01", "50")];
        let out = bind_rows(&mut rows);
        let states: Vec<AuthorityState> = out.rows.iter().map(|r| r.state).collect();
        assert!(states.contains(&AuthorityState::Authoritative));
        assert!(states.contains(&AuthorityState::Duplicate));
        assert!(out.issues.is_empty(), "完全一致不报口径问题");
        assert_eq!(out.suspected.len(), 2, "两行都要标出疑似重复");
    }

    #[test]
    fn 跨表同指标同科室行按匹配键对齐() {
        let mut rows = vec![
            row("表一", 1, "BK-01", "50"),
            row("表二", 1, "BK-01", "50"),
            row("表二", 2, "BK-02", "70"),
        ];
        let out = bind_rows(&mut rows);
        let records = &out.aligned["MER-HR-01|0301|2026-Q3"];
        assert_eq!(records.len(), 2);
        let cross: Vec<&AlignedRecord> = records.iter().filter(|r| r.cross_table).collect();
        assert_eq!(cross.len(), 1, "BK-01 跨了两张表");
        assert_eq!(cross[0].occurrences.len(), 2);
    }
}
