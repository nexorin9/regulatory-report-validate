//! 上报提交计划编译器（条件链）的验收测试。
//!
//! 验收点（对应任务 30）：
//! 1. N 步程序的**条件依赖**正确（第 i 步只在第 i-1 步成功后执行）；
//! 2. **幂等命中的行不入程序**（但进「已跳过」清单，不静默丢行）；
//! 3. **空集合编译为空程序**且不报错；
//! 4. **冲突行报「待人工判定」**而不是二选一。

use std::collections::BTreeMap;

use regval::db::repo::Repo;
use regval::push::idempotency::{ExistingSubmission, NumericField, PayloadInput, SubmissionPayload};
use regval::push::program::{self, Cond, PlanCandidate, PlanInput, SkipReason};

// ---------------------------------------------------------------- 造候选行

fn candidate(row_uid: &str, state: &str, business_key: &str, value: f64) -> PlanCandidate {
    let payload = SubmissionPayload::new(PayloadInput {
        batch_ref: "batch-plan-1",
        org_code: "ORG-1",
        reporting_period: "2026-Q3",
        indicator_code: "MER-TL-04",
        dept_code: "0301",
        business_key,
        numerator: NumericField::Number(96.0),
        denominator: NumericField::Number(100.0),
        value: NumericField::Number(value),
        unit: "%",
        basis: "国卫办医函〔2021〕28号 附件",
        rule_version: "2021.1",
    });
    PlanCandidate {
        row_uid: row_uid.to_string(),
        source_ns: "科室一表".to_string(),
        source_file: "科室一表.csv".to_string(),
        row_no: 1,
        authority_state: state.to_string(),
        match_key: "MER-TL-04|0301|2026-Q3".to_string(),
        payload,
    }
}

fn compile(candidates: Vec<PlanCandidate>, existing: BTreeMap<String, Vec<ExistingSubmission>>) -> program::Program {
    program::compile(&PlanInput {
        batch_ref: "batch-plan-1".to_string(),
        candidates,
        existing,
    })
}

// ---------------------------------------------------------------- 验收 1：条件依赖

#[test]
fn n步程序的条件依赖正确_第i步只在第i减1步成功后执行() {
    let p = compile(
        vec![
            candidate("row-c", "authoritative", "BK-3", 96.0),
            candidate("row-a", "authoritative", "BK-1", 96.0),
            candidate("row-b", "authoritative", "BK-2", 96.0),
        ],
        BTreeMap::new(),
    );

    assert_eq!(p.len(), 3, "三行权威行应编成 3 步");
    // 顺序按行键（可复现）
    let keys: Vec<&str> = p.steps().iter().map(|s| s.row_uid.as_str()).collect();
    assert_eq!(keys, vec!["row-a", "row-b", "row-c"]);

    // 第一步无条件；第 i 步依赖第 i-1 步
    assert!(p.steps()[0].cond.is_none(), "第一步无条件执行");
    assert_eq!(p.steps()[1].cond, Some(Cond::Ok { step: 0 }));
    assert_eq!(p.steps()[2].cond, Some(Cond::Ok { step: 1 }));
    for (i, s) in p.steps().iter().enumerate() {
        assert_eq!(
            s.cond.as_ref().and_then(|c| c.depends_on()),
            if i == 0 { None } else { Some(i - 1) },
            "第 {} 步的条件依赖不对",
            i + 1
        );
    }
}

#[test]
fn 编译两次程序逐字一样_可复现() {
    let cands = vec![
        candidate("row-b", "authoritative", "BK-2", 96.0),
        candidate("row-a", "authoritative", "BK-1", 96.0),
        candidate("row-c", "authoritative", "BK-3", 96.0),
    ];
    let p1 = compile(cands.clone(), BTreeMap::new());
    let p2 = compile(cands, BTreeMap::new());
    assert_eq!(p1.to_json(), p2.to_json(), "同一批输入两次编译必须逐字一样");
    assert_eq!(p1.plan_lines(), p2.plan_lines());
}

// ---------------------------------------------------------------- 验收 2：幂等命中不入程序

#[test]
fn 幂等命中的行不入程序_但进已跳过清单() {
    let c = candidate("row-a", "authoritative", "BK-1", 96.0);
    let key = c.payload.idempotency_key.clone();
    let digest = c.payload.payload_digest.clone();

    let mut existing = BTreeMap::new();
    existing.insert(
        key.clone(),
        vec![ExistingSubmission {
            revision: 1,
            payload_digest: digest,
            status: regval::push::status::ACCEPTED.to_string(),
        }],
    );

    let p = compile(vec![c], existing);

    assert!(p.is_empty(), "幂等命中的行不该编进程序");
    assert_eq!(p.steps().len(), 0);
    assert_eq!(p.skipped().len(), 1, "但要进已跳过清单 —— 不静默丢行");
    let s = &p.skipped()[0];
    assert_eq!(s.reason, SkipReason::IdempotentHit);
    assert_eq!(s.row_uid, "row-a");
    assert_eq!(s.idempotency_key, key);
    assert_eq!(s.revision, 1, "重复提交沿用原修订号");
    assert!(s.detail.contains("幂等键"), "说明要点名幂等键");
}

#[test]
fn 载荷变了就不是幂等命中_编进程序且修订号加一() {
    let first = candidate("row-a", "authoritative", "BK-1", 96.0);
    let key = first.payload.idempotency_key.clone();
    let digest = first.payload.payload_digest.clone();

    let mut existing = BTreeMap::new();
    existing.insert(
        key,
        vec![ExistingSubmission {
            revision: 1,
            payload_digest: digest,
            status: regval::push::status::ACCEPTED.to_string(),
        }],
    );

    // 同样的业务键，值改了 → 载荷摘要变了 → 订正后重报
    let revised = candidate("row-a", "authoritative", "BK-1", 75.0);
    let p = compile(vec![revised], existing);

    assert_eq!(p.len(), 1, "订正后要编进程序");
    assert_eq!(p.steps()[0].revision(), 2, "修订号 +1");
    assert!(p.skipped().is_empty());
}

// ---------------------------------------------------------------- 验收 3：空集合

#[test]
fn 空待推送集合编译为空程序且不报错() {
    let p = compile(vec![], BTreeMap::new());
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert!(p.skipped().is_empty());
    assert!(p.pending_manual().is_empty());
    // 空程序也要能导出（不炸）
    let json: serde_json::Value = serde_json::from_str(&p.to_json()).expect("空程序也能导出 JSON");
    assert_eq!(json["summary"]["step_count"], 0);
    assert!(p.to_csv().starts_with("段,步骤,行键"));
}

// ---------------------------------------------------------------- 验收 4：冲突待人工判定

#[test]
fn 同一笔记录跨表取值不同_报待人工判定而不是二选一() {
    let a = candidate("row-a", "authoritative", "BK-1", 96.0);
    let mut b = candidate("row-b", "authoritative", "BK-1", 75.0);
    b.source_file = "科室二表.csv".to_string();
    b.source_ns = "科室二表".to_string();

    let p = compile(vec![a, b], BTreeMap::new());

    assert!(p.is_empty(), "同键异值一行都不该编进程序 —— 不二选一");
    assert_eq!(p.pending_manual().len(), 2, "两行都要进待人工判定清单");
    for pend in p.pending_manual() {
        assert!(
            pend.detail.contains("待人工判定"),
            "说明要写明待人工判定：{}",
            pend.detail
        );
        assert!(
            pend.detail.contains("取值不同"),
            "说明要写明取值不同：{}",
            pend.detail
        );
    }
    // 两行的幂等键相同（同一笔记录）
    assert_eq!(
        p.pending_manual()[0].idempotency_key,
        p.pending_manual()[1].idempotency_key
    );
}

#[test]
fn 同一笔记录跨表取值一致_只编一行() {
    let a = candidate("row-a", "authoritative", "BK-1", 96.0);
    let mut b = candidate("row-b", "authoritative", "BK-1", 96.0);
    b.source_file = "科室二表.csv".to_string();

    let p = compile(vec![a, b], BTreeMap::new());

    assert_eq!(p.len(), 1, "同一笔记录只编一行");
    assert_eq!(p.steps()[0].row_uid, "row-a");
    assert_eq!(p.skipped().len(), 1);
    assert_eq!(p.skipped()[0].reason, SkipReason::CrossTableDuplicate);
    assert!(p.pending_manual().is_empty());
}

#[test]
fn 权威归属未判定的行不进程序() {
    let p = compile(vec![candidate("row-a", "raw", "BK-1", 96.0)], BTreeMap::new());
    assert!(p.is_empty());
    assert_eq!(p.skipped().len(), 1);
    assert_eq!(p.skipped()[0].reason, SkipReason::NotAuthoritative);
}

#[test]
fn 权威归属已判待人工判定的行不进程序() {
    let p = compile(
        vec![candidate("row-a", "pending_manual", "BK-1", 96.0)],
        BTreeMap::new(),
    );
    assert!(p.is_empty());
    assert_eq!(p.pending_manual().len(), 1);
    assert!(p.skipped().is_empty());
}

// ---------------------------------------------------------------- 计划导出

#[test]
fn 计划每步一行_含行键幂等键载荷摘要修订号() {
    let c = candidate("row-a", "authoritative", "BK-1", 96.0);
    let key = c.payload.idempotency_key.clone();
    let digest = c.payload.payload_digest.clone();
    let p = compile(vec![c], BTreeMap::new());

    let lines = p.plan_lines();
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line.contains("row-a"), "计划行要含行键");
    assert!(line.contains(&key), "计划行要含幂等键");
    assert!(line.contains(&digest), "计划行要含载荷摘要");
    assert!(line.contains("修订 1"), "计划行要含修订号");

    // CSV 也要能核对到同一批值
    let csv = p.to_csv();
    assert!(csv.contains("row-a"));
    assert!(csv.contains(&key));
    assert!(csv.contains(&digest));
    let mut lines = csv.lines();
    assert!(lines.next().unwrap().starts_with("段,步骤,行键"));
    assert!(lines.next().unwrap().contains("待上报"));
}

#[test]
fn 计划_csv每行列数一致_来源表列不串位() {
    let mut c = candidate("row-a", "authoritative", "BK-1", 96.0);
    c.source_file = "科室一表.csv".to_string();
    c.row_no = 7;
    let p = compile(vec![c], BTreeMap::new());

    let csv = p.to_csv();
    let mut lines = csv.lines();
    let header = lines.next().expect("表头");
    assert_eq!(header.split(',').count(), 12, "表头 12 列");
    for line in lines {
        assert_eq!(
            line.split(',').count(),
            12,
            "每行都要 12 列（不能串位）：{}",
            line
        );
    }
    // 来源表列（第 4 列）放的是来源文件名，不是指标编号
    let first = csv.lines().nth(1).expect("首行数据");
    let cols: Vec<&str> = first.split(',').collect();
    assert_eq!(cols[3], "科室一表.csv", "来源表列应是来源文件名");
    assert_eq!(cols[4], "7", "行号列应是行号");
    assert_eq!(cols[5], "MER-TL-04", "指标编号列应是指标编号");
}

#[test]
fn 计划_json可解析且结构完整() {    let p = compile(
        vec![
            candidate("row-a", "authoritative", "BK-1", 96.0),
            candidate("row-b", "raw", "BK-2", 96.0),
        ],
        BTreeMap::new(),
    );
    let v: serde_json::Value = serde_json::from_str(&p.to_json()).expect("计划 JSON 可解析");
    assert_eq!(v["schema_version"], "regval.plan/1.0.0");
    assert_eq!(v["summary"]["step_count"], 1);
    assert_eq!(v["summary"]["skipped_count"], 1);
    assert_eq!(v["summary"]["pending_manual_count"], 0);
    assert_eq!(v["steps"][0]["step"], 1);
    assert_eq!(v["steps"][0]["row_uid"], "row-a");
    assert!(v["steps"][0]["idempotency_key"].as_str().unwrap().len() == 64);
    assert!(v["steps"][0]["payload_digest"].as_str().unwrap().len() == 64);
    assert_eq!(v["steps"][0]["revision"], 1);
    assert_eq!(v["steps"][0]["depends_on"], serde_json::Value::Null);
    assert_eq!(v["skipped"][0]["reason"], "权威归属未判定，不编入上报程序");
}

// ---------------------------------------------------------------- 端到端：跑引擎再编译计划

#[test]
fn 端到端_validate后能编出计划_且不推任何东西() {
    let dir = tempfile::tempdir().expect("临时目录");
    let state_db = dir.path().join("state.db");
    let contract = dir.path().join("contract.json");
    let input = dir.path().join("mini.csv");

    // 6 行脱敏最小样例
    std::fs::write(
        &input,
        "指标编号,指标名称,科室代码,报告期,业务主键,分子,分母,填报值,计量单位\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-01,980,1000,98.0,%\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-02,900,1000,90.0,%\n\
         MER-TL-04,病案首页24小时内完成率,0302,2026-Q3,BK-03,950,1000,95.0,%\n\
         MER-HR-01,每出院人员负担工时,0301,2026-Q3,BK-04,10,5,2.0,\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-05,,,,\n\
         MER-TL-04,病案首页24小时内完成率,0301,2026-Q3,BK-01,980,1000,98.0,%\n",
    )
    .expect("写样例");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "validate",
            "--input",
            input.to_str().unwrap(),
            "--out",
            contract.to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ])
        .env("REGVAL_RULE_DIR", "rules")
        .output()
        .expect("跑 validate");
    assert!(
        out.status.success(),
        "validate 应成功：{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 编译计划（只编译不发）
    let plan_out = dir.path().join("plan.csv");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "push",
            "--plan",
            "--out",
            plan_out.to_str().unwrap(),
            "--state-db",
            state_db.to_str().unwrap(),
        ])
        .env("REGVAL_RULE_DIR", "rules")
        .output()
        .expect("跑 push --plan");
    assert!(
        out.status.success(),
        "push --plan 应成功：{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("上报提交计划（只编译不发）"),
        "应有计划抬头：{}",
        stdout
    );
    assert!(
        stdout.contains("待上报") && stdout.contains("步"),
        "应报出计划汇总：{}",
        stdout
    );
    assert!(plan_out.exists(), "计划产物应写出");

    // 只编译不发：库里一张提交记录都没有
    let repo = Repo::open(&state_db).expect("打开状态库");
    let batch = repo.latest_batch_ref().expect("查批次").expect("有批次");
    assert!(
        repo.list_submissions(&batch).expect("查提交记录").is_empty(),
        "只编译不该产生任何提交记录"
    );

    // 计划里每一行都有行键与幂等键
    let csv = std::fs::read_to_string(&plan_out).expect("读计划");
    assert!(csv.starts_with("段,步骤,行键"));
    let step_lines: Vec<&str> = csv.lines().filter(|l| l.starts_with("待上报")).collect();
    assert!(!step_lines.is_empty(), "应至少编出一步");
    for l in step_lines {
        assert!(l.split(',').count() >= 10, "计划行字段应齐：{}", l);
    }
}
