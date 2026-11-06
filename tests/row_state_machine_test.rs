//! 行级状态机的验收测试（Task 35）。
//!
//! 验收点：
//! 1. **全部合法迁移**都能走通（迁移表里的一条不落）；
//! 2. **非法迁移被拒绝并报错**（至少 4 条），且**非法迁移后行状态与原值一致**
//!    —— 状态一个字节都不动；
//! 3. **并发推进以行级乐观锁拒绝后写者**（版本号对不上就报错，不覆盖）；
//! 4. **状态推进只有一个入口**：`row_states` 表的写语句只出现在 `src/state/machine.rs`；
//! 5. **两条业务规矩真的生效**：已上报的行不被普通校验改回；被退回的行修正后回到待确认。

use std::path::{Path, PathBuf};
use std::process::Command;

use regval::db::repo::Repo;
use regval::errors::ErrorCode;
use regval::state::machine::{
    self, RowState, RowValidationInput, StateChange, StateSnapshot, LEGAL_TRANSITIONS,
};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn open_repo(dir: &Path) -> Repo {
    Repo::open(&dir.join("state.db")).expect("状态库应可打开")
}

/// 把一行推到指定状态（只走合法路径）。
fn drive_to(repo: &mut Repo, row_uid: &str, target: RowState) -> StateSnapshot {
    let file = format!("{}.csv", row_uid);
    let mut snap = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, &file, "batch-1", RowState::PendingValidation, "测试"),
    )
    .expect("建底账");
    // 待校验 → 校验通过
    if target == RowState::PendingValidation {
        return snap;
    }
    snap = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, &file, "batch-1", RowState::Validated, "测试"),
    )
    .expect("推进到校验通过");
    if target == RowState::Validated {
        return snap;
    }
    // 校验通过 → 待确认
    snap = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, &file, "batch-1", RowState::PendingConfirmation, "测试"),
    )
    .expect("推进到待确认");
    if target == RowState::PendingConfirmation {
        return snap;
    }
    // 待确认 → 终态
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, &file, "batch-1", target, "测试"),
    )
    .expect("推进到终态")
}

#[test]
fn 合法迁移表里的每一条都能走通() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let mut checked = 0;
    for (from, to) in LEGAL_TRANSITIONS {
        let row_uid = format!("row-legal-{:?}-{:?}", from, to)
            .replace(' ', "")
            .replace('"', "");
        let before = drive_to(&mut repo, &row_uid, *from);
        assert_eq!(before.state, *from, "前置状态不对：{:?}", from);

        let after = machine::advance(
            repo.conn_mut(),
            &StateChange::new(
                &row_uid,
                &format!("{}.csv", row_uid),
                "batch-1",
                *to,
                "测试",
            ),
        )
        .unwrap_or_else(|e| panic!("合法迁移 {:?} → {:?} 被拒了：{}", from, to, e));

        assert_eq!(after.state, *to);
        if from == to {
            // 合法空转：状态不变、版本号不变
            assert_eq!(after.version, before.version, "空转不该动版本号");
        } else {
            assert_eq!(after.version, before.version + 1, "推进后版本号应 +1");
        }
        checked += 1;
    }
    assert!(checked >= LEGAL_TRANSITIONS.len());
}

#[test]
fn 非法迁移被拒绝且行状态与原值一致() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    // 几条典型的非法迁移（任务验收要求至少 4 条）
    // 注：`待确认 → 校验通过` **不在**这里 —— 那是人工撤销确认的合法路径（任务 36），
    // 撤销后这一行退回「等重新确认」，不是「它没被复核过」。
    let illegal = [
        (RowState::PendingValidation, RowState::Reported),
        (RowState::Validated, RowState::Reported),
        (RowState::Reported, RowState::Rejected),
        (RowState::Reported, RowState::Validated),
        (RowState::Rejected, RowState::Reported),
        (RowState::PendingValidation, RowState::PendingConfirmation),
    ];

    for (from, to) in illegal {
        let row_uid = format!("row-bad-{}-{}", from.as_str(), to.as_str());
        let before = drive_to(&mut repo, &row_uid, from);
        let err = machine::advance(
            repo.conn_mut(),
            &StateChange::new(
                &row_uid,
                &format!("{}.csv", row_uid),
                "batch-1",
                to,
                "测试",
            ),
        )
        .expect_err(&format!("非法迁移 {} → {} 应该被拒", from, to));

        assert_eq!(err.code, ErrorCode::InvalidStateTransition);
        assert!(err.to_string().contains(from.as_str()));
        assert!(err.to_string().contains(to.as_str()));

        // 非法迁移后行状态与原值一致：状态、版本号、摘要一个都没动
        let after = machine::current(
            repo.conn(),
            &row_uid,
            &machine::file_identity(&format!("{}.csv", row_uid)),
        )
        .unwrap()
        .expect("行状态记录应还在");
        assert_eq!(after.state, before.state, "非法迁移改动了状态");
        assert_eq!(after.version, before.version, "非法迁移动了版本号");
        assert_eq!(after.data_digest, before.data_digest, "非法迁移动了数据摘要");
    }
}

#[test]
fn 并发推进以行级乐观锁拒绝后写者() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_uid = "row-lock";
    let file = "row-lock.csv";

    let first = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::Validated, "信息科甲")
            .with_expected_version(0),
    )
    .expect("第一次推进应成功");
    assert_eq!(first.version, 1);

    // 后写者拿着过期的版本号 → 被拒
    let err = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::PendingConfirmation, "信息科乙")
            .with_expected_version(0),
    )
    .expect_err("过期的期望版本应被拒");
    assert_eq!(err.code, ErrorCode::RowStateConflict);
    assert!(err.to_string().contains("后写者"));

    // 状态没有被后写者改掉
    let after = machine::current(repo.conn(), row_uid, &machine::file_identity(file))
        .unwrap()
        .unwrap();
    assert_eq!(after.state, RowState::Validated);
    assert_eq!(after.version, 1);

    // 拿着当前版本号再推一次 → 成功（乐观锁不挡合法推进）
    let ok = machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::PendingConfirmation, "信息科乙")
            .with_expected_version(1),
    )
    .expect("拿着当前版本号应能推进");
    assert_eq!(ok.state, RowState::PendingConfirmation);
    assert_eq!(ok.version, 2);
}

#[test]
fn 状态推进只有一个入口_写语句只在状态机模块里() {
    // 静态判据：`row_states` 的写语句（INSERT / UPDATE / DELETE）只允许出现在
    // src/state/machine.rs；建表语句在 src/db/schema.rs 里，别处一律不许碰。
    let src_dir = project_dir().join("src");
    let mut files = Vec::new();
    collect_rs(&src_dir, &mut files);
    assert!(!files.is_empty(), "应能扫到源码文件");

    let mut write_hits: Vec<String> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let rel = path
            .strip_prefix(&src_dir)
            .unwrap()
            .to_string_lossy()
            .to_string();
        for needle in [
            "UPDATE row_states",
            "INSERT INTO row_states",
            "DELETE FROM row_states",
        ] {
            if text.contains(needle) {
                write_hits.push(format!("{}：{}", rel, needle));
            }
        }
    }
    let files_hit: Vec<&str> = write_hits
        .iter()
        .map(|hit| hit.split('：').next().unwrap())
        .collect();
    assert!(
        files_hit.iter().all(|f| *f == "state/machine.rs"),
        "行状态表的写语句只允许出现在 state/machine.rs，实际命中：{:?}",
        write_hits
    );
    assert!(
        write_hits.len() >= 2,
        "状态机里至少要有 INSERT 与 UPDATE 两条写语句，实际：{:?}",
        write_hits
    );
    // 别的文件连表名都不该提（读也不该从这里走）
    let mut mentioning: Vec<String> = files
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.contains("row_states"))
                .unwrap_or(false)
        })
        .map(|p| {
            p.strip_prefix(&src_dir)
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    mentioning.sort();
    assert_eq!(
        mentioning,
        vec!["db/schema.rs".to_string(), "state/machine.rs".to_string()],
        "提到 row_states 的文件应只有建表处与状态机模块，实际：{:?}",
        mentioning
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

#[test]
fn 已上报的行不被普通校验改回() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_uid = "row-reported";
    let file = "row-reported.csv";
    drive_to(&mut repo, row_uid, RowState::Reported);

    // 普通校验（结论通过）再来一遍 —— 已上报的行必须原样不动
    let report = machine::reconcile_after_validation(
        repo.conn_mut(),
        "batch-1",
        &[RowValidationInput {
            row_uid: row_uid.to_string(),
            source_file: file.to_string(),
            conclusion_ok: true,
            data_digest: "digest-after-correction".to_string(),
        }],
    )
    .expect("对账应成功");

    assert_eq!(report.protected_reported, 1);
    assert_eq!(report.advanced, 0);
    // 内容变了只记提示，不自动改状态（要重报得走明确的订正路径）
    assert_eq!(report.reported_changed, 1);

    let after = machine::current(repo.conn(), row_uid, &machine::file_identity(file))
        .unwrap()
        .unwrap();
    assert_eq!(after.state, RowState::Reported, "已上报的行被普通校验改回了");
}

#[test]
fn 被退回的行修正后回到待确认_没修正则保持被退回() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_uid = "row-rejected";
    let file = "row-rejected.csv";

    // 推进到被退回，并把数据摘要记成「修正前」的那一版
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::Validated, "测试")
            .with_digest("digest-v1"),
    )
    .unwrap();
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::PendingConfirmation, "测试")
            .with_digest("digest-v1"),
    )
    .unwrap();
    machine::advance(
        repo.conn_mut(),
        &StateChange::new(row_uid, file, "batch-1", RowState::Rejected, "网关")
            .with_digest("digest-v1"),
    )
    .unwrap();

    // 还没改数据就再校验一遍 → 保持被退回（没改就别急着重报）
    let report = machine::reconcile_after_validation(
        repo.conn_mut(),
        "batch-1",
        &[RowValidationInput {
            row_uid: row_uid.to_string(),
            source_file: file.to_string(),
            conclusion_ok: true,
            data_digest: "digest-v1".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(report.rejected_fixed, 0);
    assert_eq!(
        machine::current(repo.conn(), row_uid, &machine::file_identity(file))
            .unwrap()
            .unwrap()
            .state,
        RowState::Rejected
    );

    // 改了数据（摘要变了）→ 回到待确认，需重新确认
    let report = machine::reconcile_after_validation(
        repo.conn_mut(),
        "batch-1",
        &[RowValidationInput {
            row_uid: row_uid.to_string(),
            source_file: file.to_string(),
            conclusion_ok: true,
            data_digest: "digest-v2".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(report.rejected_fixed, 1);
    assert_eq!(report.advanced, 1);
    let after = machine::current(repo.conn(), row_uid, &machine::file_identity(file))
        .unwrap()
        .unwrap();
    assert_eq!(after.state, RowState::PendingConfirmation);
    assert_eq!(after.data_digest, "digest-v2");
}

#[test]
fn 校验不通过的行停在待校验_复校验通过才推进() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());
    let row_uid = "row-failing";
    let file = "row-failing.csv";

    // 结论不通过 → 待校验
    let report = machine::reconcile_after_validation(
        repo.conn_mut(),
        "batch-1",
        &[RowValidationInput {
            row_uid: row_uid.to_string(),
            source_file: file.to_string(),
            conclusion_ok: false,
            data_digest: "digest-v1".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(report.created, 1);
    assert_eq!(report.advanced, 0);
    let snap = machine::current(repo.conn(), row_uid, &machine::file_identity(file))
        .unwrap()
        .unwrap();
    assert_eq!(snap.state, RowState::PendingValidation);

    // 复校验通过 → 校验通过
    let report = machine::reconcile_after_validation(
        repo.conn_mut(),
        "batch-1",
        &[RowValidationInput {
            row_uid: row_uid.to_string(),
            source_file: file.to_string(),
            conclusion_ok: true,
            data_digest: "digest-v2".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(report.advanced, 1);
    assert_eq!(
        machine::current(repo.conn(), row_uid, &machine::file_identity(file))
            .unwrap()
            .unwrap()
            .state,
        RowState::Validated
    );
}

#[test]
fn 上报回执把行状态推进到终态_没过校验的行不动() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open_repo(dir.path());

    // 造两行并落库：一行校验通过，一行没通过
    let mut values = std::collections::BTreeMap::new();
    values.insert(regval::ingest::Field::OrgCode, "ORG-001".to_string());
    values.insert(
        regval::ingest::Field::IndicatorCode,
        "MER-TL-04".to_string(),
    );
    values.insert(
        regval::ingest::Field::IndicatorName,
        "病案首页完整率".to_string(),
    );
    values.insert(regval::ingest::Field::DeptCode, "0301".to_string());
    values.insert(
        regval::ingest::Field::ReportingPeriod,
        "2026-Q3".to_string(),
    );
    values.insert(regval::ingest::Field::BusinessKey, "BK-01".to_string());
    values.insert(regval::ingest::Field::Numerator, "980".to_string());
    values.insert(regval::ingest::Field::Denominator, "1000".to_string());
    values.insert(regval::ingest::Field::Value, "98".to_string());
    values.insert(regval::ingest::Field::Unit, "%".to_string());
    let mk_row = |ns: &str, key: &str| {
        let mut v = values.clone();
        v.insert(
            regval::ingest::Field::BusinessKey,
            key.to_string(),
        );
        regval::ingest::SourceRow {
            source_ns: ns.to_string(),
            source_file: format!("{}.csv", ns),
            row_no: 1,
            dup_ordinal: 0,
            values: v,
        }
    };
    let good = mk_row("科室一表", "BK-01");
    let rejected = mk_row("科室二表", "BK-02");
    let bad = mk_row("科室三表", "BK-03");
    repo.insert_rows(
        "batch-1",
        &[good.clone(), rejected.clone(), bad.clone()],
    )
    .expect("落库应成功");

    // 前两行先落到「校验通过」；第三行留在「待校验」（没过校验）
    for row in [&good, &rejected] {
        machine::advance(
            repo.conn_mut(),
            &StateChange::new(
                &regval::ingest::key::row_uid(row),
                &row.source_file,
                "batch-1",
                RowState::Validated,
                "validate",
            ),
        )
        .unwrap();
    }

    // 回执受理 → 已上报
    let state = machine::advance_after_push(
        repo.conn_mut(),
        "batch-1",
        &regval::ingest::key::row_uid(&good),
        regval::push::status::ACCEPTED,
        "push",
    )
    .unwrap();
    assert_eq!(state, Some(RowState::Reported));
    let snap = machine::current(
        repo.conn(),
        &regval::ingest::key::row_uid(&good),
        &machine::file_identity(&good.source_file),
    )
    .unwrap()
    .unwrap();
    assert_eq!(snap.state, RowState::Reported);

    // 回执退回 → 被退回
    let state = machine::advance_after_push(
        repo.conn_mut(),
        "batch-1",
        &regval::ingest::key::row_uid(&rejected),
        regval::push::status::REJECTED,
        "push",
    )
    .unwrap();
    assert_eq!(state, Some(RowState::Rejected));

    // 没过校验的行 → 推不动（不被标成已上报）
    let state = machine::advance_after_push(
        repo.conn_mut(),
        "batch-1",
        &regval::ingest::key::row_uid(&bad),
        regval::push::status::ACCEPTED,
        "push",
    )
    .unwrap();
    assert_eq!(state, None);
    let snap = machine::current(
        repo.conn(),
        &regval::ingest::key::row_uid(&bad),
        &machine::file_identity(&bad.source_file),
    )
    .unwrap();
    assert!(snap.is_none(), "没过校验的行不该被推进成已上报");

    // 库里没有的行 → 推不动（返回 None，不凭空造一条状态）
    let state = machine::advance_after_push(
        repo.conn_mut(),
        "batch-1",
        "row-not-in-db",
        regval::push::status::ACCEPTED,
        "push",
    )
    .unwrap();
    assert_eq!(state, None);
}

#[test]
fn 端到端_校验一遍行状态落库且状态分布可查() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let out = dir.path().join("result.json");
    let fixture = project_dir().join("tests/fixtures/mini.csv");

    let output = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args([
            "validate",
            "--input",
            fixture.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--state-db",
            db.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .expect("regval 应可执行");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "校验应成功，stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("行状态："), "stdout 应有一行行状态摘要：{}", stdout);
    assert!(stdout.contains("行状态分布："), "stdout 应有状态分布：{}", stdout);

    // 库里能查到行状态，且分布与 stdout 一致
    let repo = open_repo(dir.path());
    let counts = machine::counts(repo.conn(), None).unwrap();
    assert!(!counts.is_empty(), "库里应有行状态");
    let total: usize = counts.values().sum();
    assert_eq!(total, 6, "mini.csv 是 6 行，行状态应一行一条");

    // regval status 能看到行状态分布
    let status = Command::new(env!("CARGO_BIN_EXE_regval"))
        .args(["status", "--state-db", db.to_str().unwrap()])
        .current_dir(dir.path())
        .env_remove("REGVAL_STATE_DB")
        .env_remove("REGVAL_RULE_DIR")
        .output()
        .expect("regval 应可执行");
    let status_out = String::from_utf8_lossy(&status.stdout).to_string();
    assert!(
        status_out.contains("行状态："),
        "status 应打印行状态分布：{}",
        status_out
    );
}
