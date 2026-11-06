//! 官方口径案例同构用例集（Task 46）。
//!
//! `cases/` 下每个 JSON 文件是一组**可执行的卫健口径用例**：每条用例给一行（或几行）
//! 上报表输入，写明期望跑出来的问题（问题类型 / 规则编号 / 严重度 / 依据出处）。
//! 每条用例对应本文件里一个 `#[test]`，断言实际跑出来的问题与用例里写的期望一致。
//!
//! # 为什么期望值只写在用例文件里
//!
//! 测试代码里不另抄一份期望值 —— 抄一份就多一处会漂移的地方：口径改了、用例改了，
//! 测试里的旧期望还在，两边对不上时谁也不知道该信谁。**用例文件是期望值的唯一事实来源**，
//! 改口径只改 `cases/*.json`。
//!
//! # 用例数与文件条目数必须一致
//!
//! [`用例数与_cases_文件条目数一致且每条依据非空`] 一条用例守着这条纪律：少一条用例、
//! 多一条用例、id 对不上、某条忘了写依据，都会在这一条上失败。
//!
//! # 案例内容只用卫健口径
//!
//! 本轮口径是**国卫办医函〔2021〕28号 附件的 27 项病案管理质量控制指标**，
//! 不是医保结算清单 —— [`案例内容不混入医保结算清单口径`] 盯着这件事：
//! 案例文件里出现「结算清单」「DRG」「ICD-10」这类别的域的词，即失败。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;

use regval::bind::dept::{DeptDirectory, InMemoryDirectory, UnavailableDirectory};
use regval::dupe;
use regval::ingest::{self, Field, SourceRow};
use regval::rules::executor;
use regval::rules::model::{Issue, Rule};

/// 用例 id 全集 —— 一条用例一个 `#[test]`。
///
/// 这份清单与 `cases/*.json` 的条目数必须一致（由
/// [`用例数与_cases_文件条目数一致且每条依据非空`] 断言）。
const CASE_IDS: &[&str] = &[
    // 必填缺失
    "wj-required-001",
    "wj-required-002",
    // 枚举越界
    "wj-enum-001",
    "wj-enum-002",
    "wj-enum-003",
    // 单位不符
    "wj-unit-001",
    "wj-unit-002",
    // 疑似重复
    "wj-dupe-001",
    "wj-dupe-002",
    "wj-dupe-003",
    // 边界例
    "wj-boundary-001",
    "wj-boundary-002",
    "wj-boundary-003",
    "wj-boundary-004",
    "wj-boundary-005",
];

// ————————————————————————————————————————————————————————————————————————————
// 用例的数据结构（与 cases/*.json 一一对应）
// ————————————————————————————————————————————————————————————————————————————

/// 一条期望：这一行应当命中（或不命中）哪些问题
#[derive(Debug, Clone, Deserialize)]
struct Expect {
    /// 规则编号（省略 = 不限定规则）
    #[serde(default)]
    rule_id: Option<String>,
    /// 问题类型（写进「问题类型」列的中文名）
    #[serde(default)]
    issue_kind: Option<String>,
    /// 严重度：阻断 / 提示
    #[serde(default)]
    severity: Option<String>,
    /// 缺失列清单（必填缺失类才有）
    #[serde(default)]
    missing_columns: Option<Vec<String>>,
    /// 机器可读错误码
    #[serde(default)]
    error_code: Option<String>,
    /// 问题说明里应当出现的片段
    #[serde(default)]
    message_contains: Option<Vec<String>>,
    /// 期望命中条数（默认 1；写 0 = 该规则不应命中本行）
    #[serde(default)]
    issue_count: Option<usize>,
}

/// 疑似重复候选对的期望
#[derive(Debug, Clone, Deserialize)]
struct ExpectPair {
    /// 命中层级：exact（确定性键）/ normalize（归一化候选）
    level: String,
    /// 期望候选对数
    pair_count: usize,
}

/// 一条用例
#[derive(Debug, Clone, Deserialize)]
struct Case {
    /// 用例编号
    case_id: String,
    /// 用例标题（中文短句，给人看）
    #[serde(default)]
    title: String,
    /// 默认来源表名
    #[serde(default)]
    source: Option<String>,
    /// 逐行来源表名（多表用例：同一业务记录在几张科室表里各填一次）
    #[serde(default)]
    sources: Option<Vec<String>>,
    /// 单行输入
    #[serde(default)]
    input: Option<Value>,
    /// 多行输入
    #[serde(default)]
    rows: Option<Vec<Value>>,
    /// 院内 HIS 科室目录（省略 = 目录不可用，科室主数据本轮不核对）
    #[serde(default)]
    dept_directory: Option<Vec<String>>,
    /// 断言针对哪几行（行号从 1 起；省略 = 只断言第 1 行）
    #[serde(default)]
    assert_rows: Option<Vec<usize>>,
    /// 期望问题
    #[serde(default)]
    expect: Vec<Expect>,
    /// 期望疑似重复候选对
    #[serde(default)]
    expect_pair: Option<ExpectPair>,
    /// 该行不应产生任何问题
    #[serde(default)]
    expect_no_issue: bool,
    /// 官方依据出处（必填）
    basis: String,
    /// 置信度标注：official / derived
    #[serde(default)]
    confidence: String,
}

// ————————————————————————————————————————————————————————————————————————————
// 用例集装载
// ————————————————————————————————————————————————————————————————————————————

/// 用例集目录
fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("cases")
}

/// 用例文件（`cases/*.json`，按文件名排序）
fn case_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .unwrap_or_else(|e| panic!("读 cases 目录失败：{}", e))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "cases/ 目录下没有用例文件");
    files
}

/// 读全部用例
fn load_cases() -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for path in case_files() {
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读 {} 失败：{}", path.display(), e));
        let doc: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} 解析失败：{}", path.display(), e));
        let arr = doc["cases"]
            .as_array()
            .unwrap_or_else(|| panic!("{} 缺 cases 数组", path.display()));
        for item in arr {
            let case: Case = serde_json::from_value(item.clone()).unwrap_or_else(|e| {
                panic!("{} 里的用例结构不合法：{}", path.display(), e)
            });
            cases.push(case);
        }
    }
    cases
}

/// 表头名 → 本机库的列（只认本产品最小数据格式里的列）
fn field_of(header: &str) -> Option<Field> {
    Some(match header {
        "机构代码" => Field::OrgCode,
        "指标编号" => Field::IndicatorCode,
        "指标名称" => Field::IndicatorName,
        "科室代码" => Field::DeptCode,
        "科室名称" => Field::DeptName,
        "报告期" => Field::ReportingPeriod,
        "报告期类型" => Field::PeriodType,
        "分子" => Field::Numerator,
        "分母" => Field::Denominator,
        "填报值" => Field::Value,
        "计量单位" => Field::Unit,
        "业务主键" => Field::BusinessKey,
        "备注" => Field::Remark,
        _ => return None,
    })
}

/// 用例输入 → 来源行
fn build_rows(case: &Case) -> Vec<SourceRow> {
    let mut raw_rows: Vec<Value> = Vec::new();
    if let Some(input) = &case.input {
        raw_rows.push(input.clone());
    }
    if let Some(rows) = &case.rows {
        raw_rows.extend(rows.iter().cloned());
    }
    assert!(!raw_rows.is_empty(), "用例 {} 没有输入行", case.case_id);

    let default_source = case.source.clone().unwrap_or_else(|| "表一".to_string());
    let mut rows: Vec<SourceRow> = Vec::new();
    for (i, raw) in raw_rows.iter().enumerate() {
        let source_ns = case
            .sources
            .as_ref()
            .and_then(|s| s.get(i).cloned())
            .unwrap_or_else(|| default_source.clone());
        let obj = raw.as_object().unwrap_or_else(|| {
            panic!("用例 {} 第 {} 行不是对象", case.case_id, i + 1)
        });
        let mut values = BTreeMap::new();
        for (header, value) in obj {
            let field = field_of(header).unwrap_or_else(|| {
                panic!(
                    "用例 {} 第 {} 行的表头「{}」不是本产品的列名",
                    case.case_id,
                    i + 1,
                    header
                )
            });
            values.insert(field, value.as_str().unwrap_or_default().to_string());
        }
        rows.push(SourceRow {
            source_ns: source_ns.clone(),
            source_file: format!("{}.csv", source_ns),
            row_no: i + 1,
            dup_ordinal: 0,
            values,
        });
    }
    // 同一业务主键下的多行要各有各的行键（否则问题会串到别的行上）
    ingest::key::assign_dup_ordinals(&mut rows);
    rows
}

// ————————————————————————————————————————————————————————————————————————————
// 跑一遍本轮会产出的口径问题
// ————————————————————————————————————————————————————————————————————————————

/// 解析数值列（与读表层一致：空 / 非数值 / 非有限数都存不进数值列）
fn parse_number(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => Some(v),
        _ => None,
    }
}

/// 建一个带样例行的本机库（内存库）
fn seed_db(rows: &[SourceRow]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    regval::db::schema::migrate(&conn).unwrap();
    for row in rows {
        let text = |field: Field| row.get_trimmed(field).unwrap_or("").to_string();
        // 数值原文（与落库层同格式）：「空」与「非数值」靠它区分开
        let numeric_raw = serde_json::json!({
            "numerator": text(Field::Numerator),
            "denominator": text(Field::Denominator),
            "value": text(Field::Value),
        })
        .to_string();
        conn.execute(
            "INSERT INTO rows (batch_ref, source_ns, source_file, row_no, row_uid, org_code,
                               authority_state, indicator_code, indicator_name, dept_code, dept_name,
                               reporting_period, period_type, business_key, numerator, denominator,
                               value, unit, remark, numeric_raw, rule_version, created_at)
             VALUES ('B1', ?1, ?2, ?3, ?4, ?5, 'raw', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, NULL, '2026-11-07 00:00:00')",
            rusqlite::params![
                row.source_ns,
                row.source_file,
                row.row_no as i64,
                ingest::key::row_uid(row),
                text(Field::OrgCode),
                text(Field::IndicatorCode),
                text(Field::IndicatorName),
                text(Field::DeptCode),
                text(Field::DeptName),
                text(Field::ReportingPeriod),
                text(Field::PeriodType),
                text(Field::BusinessKey),
                parse_number(&text(Field::Numerator)),
                parse_number(&text(Field::Denominator)),
                parse_number(&text(Field::Value)),
                text(Field::Unit),
                text(Field::Remark),
                numeric_raw,
            ],
        )
        .unwrap();
    }
    conn
}

/// 跑一遍本轮会产出的全部口径问题。
///
/// 两处合起来才是「一次校验实际会报什么」：
/// 1. **口径规则**（必填 / 枚举 / 单位 / 公式 / 表内唯一）；
/// 2. **实体绑定层**（同权威键异值 → 待人工判定；科室代码不在目录 → 科室主数据不一致）。
fn run_issues(rows: &[SourceRow], directory: &dyn DeptDirectory) -> Vec<Issue> {
    let conn = seed_db(rows);
    let rules: Vec<Rule> = regval::rules::model::builtin_rules();
    let refs: Vec<&Rule> = rules.iter().collect();
    let mut issues = executor::execute_rules(&conn, &refs, "B1").issues();
    issues.extend(regval::bind::bind(rows, directory).issues);
    issues
}

// ————————————————————————————————————————————————————————————————————————————
// 断言
// ————————————————————————————————————————————————————————————————————————————

/// 跑一条用例并逐条核对期望
fn assert_case(case_id: &str) {
    let cases = load_cases();
    let case = cases
        .iter()
        .find(|c| c.case_id == case_id)
        .unwrap_or_else(|| panic!("cases/ 里找不到用例 {}", case_id));

    // 每条用例都得说清依据与置信度 —— 没依据的判定没法给科室看
    assert!(
        !case.basis.trim().is_empty(),
        "用例 {} 缺依据（basis 不能为空）",
        case_id
    );
    assert!(
        !case.confidence.trim().is_empty(),
        "用例 {} 缺 confidence",
        case_id
    );

    let rows = build_rows(case);
    let directory: Box<dyn DeptDirectory> = match &case.dept_directory {
        Some(codes) => Box::new(InMemoryDirectory::new(codes.clone())),
        // 目录不可用 → 科室主数据本轮不核对（标「未校验」，不当成通过）
        None => Box::new(UnavailableDirectory),
    };
    let issues = run_issues(&rows, directory.as_ref());

    let assert_rows = case.assert_rows.clone().unwrap_or_else(|| vec![1]);
    for index in assert_rows {
        assert!(
            index >= 1 && index <= rows.len(),
            "用例 {} 的断言行号 {} 越界（共 {} 行）",
            case_id,
            index,
            rows.len()
        );
        let uid = ingest::key::row_uid(&rows[index - 1]);
        let row_issues: Vec<&Issue> = issues.iter().filter(|i| i.row_uid == uid).collect();

        for expect in &case.expect {
            let matched: Vec<&&Issue> = row_issues
                .iter()
                .filter(|i| {
                    expect.rule_id.as_ref().map_or(true, |r| &i.rule_id == r)
                        && expect.issue_kind.as_ref().map_or(true, |k| &i.issue_kind == k)
                        && expect.severity.as_ref().map_or(true, |s| &i.severity == s)
                        && expect.error_code.as_ref().map_or(true, |c| &i.error_code == c)
                })
                .collect();
            let want = expect.issue_count.unwrap_or(1);
            assert_eq!(
                matched.len(),
                want,
                "用例 {} 第 {} 行：期望 {} 条符合条件的问题，实际 {} 条；该行全部问题：{:?}",
                case_id,
                index,
                want,
                matched.len(),
                row_issues
            );
            for issue in &matched {
                if let Some(columns) = &expect.missing_columns {
                    assert_eq!(
                        &issue.missing_columns,
                        columns,
                        "用例 {} 第 {} 行：缺失列清单对不上",
                        case_id,
                        index
                    );
                }
                if let Some(parts) = &expect.message_contains {
                    for part in parts {
                        assert!(
                            issue.message.contains(part),
                            "用例 {} 第 {} 行：问题说明里没有「{}」：{}",
                            case_id,
                            index,
                            part,
                            issue.message
                        );
                    }
                }
                assert!(
                    !issue.basis.trim().is_empty(),
                    "用例 {}：报出来的问题缺依据",
                    case_id
                );
            }
        }

        if case.expect_no_issue {
            assert!(
                row_issues.is_empty(),
                "用例 {} 第 {} 行不该有任何问题，实际：{:?}",
                case_id,
                index,
                row_issues
            );
        }
    }

    // 疑似重复候选对（重复检测漏斗的级别）
    if let Some(pair) = &case.expect_pair {
        let pairs = match pair.level.as_str() {
            "exact" => dupe::exact::detect(&rows).pairs,
            "normalize" => dupe::normalize::detect(&rows).pairs,
            other => panic!("用例 {} 的候选对层级「{}」不认识", case_id, other),
        };
        assert_eq!(
            pairs.len(),
            pair.pair_count,
            "用例 {}：{} 级候选对数对不上（实际 {} 对）",
            case_id,
            pair.level,
            pairs.len()
        );
        for p in &pairs {
            assert_eq!(p.match_level, pair.level, "用例 {}：候选对层级对不上", case_id);
            assert!(
                !p.basis.trim().is_empty(),
                "用例 {}：候选对缺依据",
                case_id
            );
            // 漏斗的铁律：只标疑似，不合并、不推进状态
            assert!(
                !p.state.contains("已合并"),
                "用例 {}：候选对的状态不该是「已合并」",
                case_id
            );
        }
    }
}

// ————————————————————————————————————————————————————————————————————————————
// 用例集的纪律用例
// ————————————————————————————————————————————————————————————————————————————

#[test]
fn 用例数与_cases_文件条目数一致且每条依据非空() {
    let cases = load_cases();
    assert_eq!(
        cases.len(),
        CASE_IDS.len(),
        "cases/ 里的用例数（{}）与测试清单（{}）不一致",
        cases.len(),
        CASE_IDS.len()
    );

    let mut ids: Vec<&str> = cases.iter().map(|c| c.case_id.as_str()).collect();
    ids.sort();
    let mut want: Vec<&str> = CASE_IDS.to_vec();
    want.sort();
    assert_eq!(ids, want, "用例 id 清单与 cases/ 文件里的对不上");

    for case in &cases {
        assert!(
            !case.basis.trim().is_empty(),
            "用例 {} 缺依据（basis 不能为空）",
            case.case_id
        );
        assert!(
            !case.confidence.trim().is_empty(),
            "用例 {} 缺 confidence",
            case.case_id
        );
        assert!(
            !case.expect.is_empty() || case.expect_pair.is_some() || case.expect_no_issue,
            "用例 {} 没有任何期望",
            case.case_id
        );
        assert!(
            !case.title.trim().is_empty(),
            "用例 {} 缺标题",
            case.case_id
        );
    }
}

#[test]
fn 四类口径问题各至少两例且边界例齐全() {
    let cases = load_cases();

    // 四类口径问题：必填缺失 / 枚举越界 / 单位不符 / 疑似重复，各至少 2 例
    for kind in ["必填缺失", "枚举越界", "单位不符", "疑似重复"] {
        let count = cases
            .iter()
            .filter(|c| {
                c.expect
                    .iter()
                    .any(|e| e.issue_kind.as_deref() == Some(kind))
            })
            .count();
        assert!(count >= 2, "「{}」至少要有 2 例，实际 {} 例", kind, count);
    }

    // 边界例五类，逐条点名 —— 少一类都不行
    let has_error_code = |code: &str| {
        cases
            .iter()
            .any(|c| c.expect.iter().any(|e| e.error_code.as_deref() == Some(code)))
    };
    // 分母为 0
    assert!(
        has_error_code("DENOMINATOR_ZERO"),
        "边界例缺「分母为 0」"
    );
    // 科室代码不存在
    assert!(has_error_code("DEPT_NOT_FOUND"), "边界例缺「科室代码不存在」");
    // 疑似重复对（确定性键一级）
    assert!(
        cases
            .iter()
            .any(|c| c.expect_pair.as_ref().map_or(false, |p| p.level == "exact")),
        "边界例缺「疑似重复对」"
    );
    // 别名指标名（归一化一级）
    assert!(
        cases
            .iter()
            .any(|c| c.expect_pair.as_ref().map_or(false, |p| p.level == "normalize")),
        "边界例缺「别名指标名」"
    );
    // 单位别名：有一行写法不在规范取值里（归一后才是同一个单位），且该行不应报任何问题
    let alias_unit_case = cases.iter().find(|c| {
        c.expect_no_issue
            && c.rows.is_none()
            && c.input
                .as_ref()
                .and_then(|i| i.get("计量单位"))
                .and_then(|u| u.as_str())
                .map(|u| u != "%")
                .unwrap_or(false)
    });
    assert!(
        alias_unit_case.is_some(),
        "边界例缺「单位别名」（同一单位的另一种写法归一后不应报问题）"
    );
}

#[test]
fn 案例内容不混入医保结算清单口径() {
    // 本轮口径是卫健 27 项指标；案例文件里出现别的域的词，就是口径串了
    for path in case_files() {
        let raw = std::fs::read_to_string(&path).unwrap();
        for forbidden in [
            "结算清单",
            "医保",
            "DRG",
            "DIP",
            "ICD-10",
            "主诊断唯一性",
            "离院方式",
            "拟接收机构",
        ] {
            assert!(
                !raw.contains(forbidden),
                "{} 里出现了不属于卫健口径的内容「{}」",
                path.display(),
                forbidden
            );
        }
    }
}

#[test]
fn 每条用例都能跑出可核对的问题或候选对() {
    // 兜底：一条用例要么跑出可核对的问题 / 候选对，要么它本身就是在断言
    // 「这一行不该报任何问题」（别名归一那类）—— 两者都不是，就说明用例写空了，
    // 跑不出东西的用例不算用例。
    for case in load_cases() {
        let rows = build_rows(&case);
        let directory: Box<dyn DeptDirectory> = match &case.dept_directory {
            Some(codes) => Box::new(InMemoryDirectory::new(codes.clone())),
            None => Box::new(UnavailableDirectory),
        };
        let issues = run_issues(&rows, directory.as_ref());
        let pairs =
            dupe::exact::detect(&rows).pairs.len() + dupe::normalize::detect(&rows).pairs.len();
        assert!(
            !issues.is_empty() || pairs > 0 || case.expect_no_issue,
            "用例 {} 跑出来什么都没有：既没有口径问题也没有疑似重复对，用例本身写空了",
            case.case_id
        );
    }
}

// ————————————————————————————————————————————————————————————————————————————
// 逐条用例（一条用例一个 #[test]）
// ————————————————————————————————————————————————————————————————————————————

// —— 必填缺失 ——

#[test]
fn 用例_wj_required_001_缺报告期报必填缺失() {
    assert_case("wj-required-001");
}

#[test]
fn 用例_wj_required_002_缺指标名称与科室代码报必填缺失() {
    assert_case("wj-required-002");
}

// —— 枚举越界 ——

#[test]
fn 用例_wj_enum_001_报告期写法不合法报枚举越界() {
    assert_case("wj-enum-001");
}

#[test]
fn 用例_wj_enum_002_报告期类型不合法报枚举越界() {
    assert_case("wj-enum-002");
}

#[test]
fn 用例_wj_enum_003_指标编号不在册报枚举越界() {
    assert_case("wj-enum-003");
}

// —— 单位不符 ——

#[test]
fn 用例_wj_unit_001_比率型指标单位写成例报单位不符() {
    assert_case("wj-unit-001");
}

#[test]
fn 用例_wj_unit_002_甲级病历率单位写成人次报单位不符() {
    assert_case("wj-unit-002");
}

// —— 疑似重复 ——

#[test]
fn 用例_wj_dupe_001_表内同键同值标疑似重复() {
    assert_case("wj-dupe-001");
}

#[test]
fn 用例_wj_dupe_002_跨表同键异值标待人工判定() {
    assert_case("wj-dupe-002");
}

#[test]
fn 用例_wj_dupe_003_表内同键异值既标疑似重复也标待人工判定() {
    assert_case("wj-dupe-003");
}

// —— 边界例 ——

#[test]
fn 用例_wj_boundary_001_分母为零报数据形态错误() {
    assert_case("wj-boundary-001");
}

#[test]
fn 用例_wj_boundary_002_单位别名归一后不报问题() {
    assert_case("wj-boundary-002");
}

#[test]
fn 用例_wj_boundary_003_指标名称别名归一后进候选对() {
    assert_case("wj-boundary-003");
}

#[test]
fn 用例_wj_boundary_004_跨表同一笔记录出疑似重复对() {
    assert_case("wj-boundary-004");
}

#[test]
fn 用例_wj_boundary_005_科室代码不在目录报科室主数据不一致() {
    assert_case("wj-boundary-005");
}
