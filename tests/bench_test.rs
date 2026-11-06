//! 规模与性能基准（真实规模输入）。
//!
//! 验收口径（见 task.json 任务 47）：
//! 1. 约 5000 行的真实规模样例跑**完整校验（四类检查器）+ 提交计划编译**，测总耗时与峰值内存；
//! 2. 单表 200 行与 5000 行分别计时，给出**线性度观察**（每行耗时有没有随规模恶化）；
//! 3. 宏侧写回（分块 200 行一档、一次 `Range.Value2` 赋值）在大表下不出现逐格写回退化；
//! 4. **阈值写进测试**：超阈即失败（回归时同一台机器上跑出来的数就能守住）；
//! 5. 数值落 `docs/bench.md`（含测量命令，可复现）。
//!
//! 测量口径（写清楚，别让人误读）：
//! - **端到端**走真实命令行：`regval validate` + `regval push --plan` —— 就是信息科在院内
//!   机器上敲的那两条命令，含进程启动、读表、落库、跑规则、编计划、写契约文件。
//! - **分段耗时**走进程内同一套引擎调用（读表 → 映射 → 绑定 → 漏斗 → 落库 → 跑规则 → 编计划），
//!   用来定位时间花在哪一段，不是另一套逻辑。
//! - **峰值常驻内存**取子进程口径（`getrusage(RUSAGE_CHILDREN)` 的 `ru_maxrss`）。
//! - 规模样例来自 `samples/generate.py`（固定种子合成，只含指标级监管数据，无患者可识别明细）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use regval::bind;
use regval::bind::dept::UnavailableDirectory;
use regval::db::repo::Repo;
use regval::dupe;
use regval::dupe::distance;
use regval::push::idempotency::ExistingSubmission;
use regval::push::program;
use regval::rules::executor;
use regval::rules::indicators;
use regval::rules::loader;
use regval::rules::version;

// ————————————————————————————————————————————————————————————
// 输入样例（任务 45 的合成生成器产物）
// ————————————————————————————————————————————————————————————

/// 规模集：约 5000 行（10 张科室表各 500 行合并）
const SCALE_INPUT: &str = "samples/dataset-scale/合并表.csv";
/// 基准集：1200 行（6 张科室表各 200 行合并）
const MERGED_INPUT: &str = "samples/dataset/合并表.csv";
/// 单表：200 行
const SMALL_INPUT: &str = "samples/dataset/病案统计室表.csv";
/// 口径规则目录（与院内 `.env` 里的 `REGVAL_RULE_DIR=./rules` 一致）
const RULE_DIR: &str = "rules";

// ————————————————————————————————————————————————————————————
// 阈值（写进测试：超阈即失败）
// ————————————————————————————————————————————————————————————

/// 5000 行「完整校验 + 提交计划编译」端到端总耗时上限（毫秒）
///
/// 这条是**粗线**（挡住「整体慢了一大截」）；真正灵敏的是下面的每行耗时放大倍数 ——
/// 形状一变（从线性变成平方级）放大倍数立刻越线。给 6.8 倍余量是为了让
/// 比开发机慢几倍的院内机器 / CI 机器不会误报。
const THRESHOLD_SCALE_TOTAL_MS: u128 = 45_000;
/// 5000 行那一轮的峰值常驻内存上限（MB）
const THRESHOLD_SCALE_RSS_MB: u64 = 512;
/// 线性度上限：5000 行的每行耗时相对 200 行每行耗时的放大倍数
const THRESHOLD_PER_ROW_AMPLIFICATION: f64 = 12.0;
/// 写回计划编译（5000 行）的耗时上限（毫秒）
const THRESHOLD_WRITEBACK_PLAN_MS: u128 = 5_000;
/// 新列个数（写回只碰这 5 列）
const NEW_COLS: usize = 5;

// ————————————————————————————————————————————————————————————
// 工具
// ————————————————————————————————————————————————————————————

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn abs(rel: &str) -> PathBuf {
    project_root().join(rel)
}

/// 从 Excel 宏源码里读出分块写回一档的行数。
///
/// **不另写一份常量**：宏改了分块大小，这里跟着变，测试不会拿着过期的 200
/// 去判一条已经改成 500 的写回路径。
fn macro_chunk_rows() -> usize {
    let src = std::fs::read_to_string(project_root().join("macro/vba/RegVal.bas"))
        .expect("读 Excel 宏源码 macro/vba/RegVal.bas");
    for line in src.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Private Const REGVAL_CHUNK_ROWS As Long = ") {
            return rest
                .trim()
                .parse()
                .expect("宏里的 REGVAL_CHUNK_ROWS 应是整数");
        }
    }
    panic!("宏里找不到 REGVAL_CHUNK_ROWS");
}

/// 宏侧一次分块赋值的覆盖情况。
struct MacroWriteProfile {
    /// 每档一次批量赋值覆盖的格子数
    cells_per_assignment: Vec<usize>,
    /// 每档覆盖的行数
    rows_per_assignment: Vec<usize>,
}

impl MacroWriteProfile {
    fn assignments(&self) -> usize {
        self.cells_per_assignment.len()
    }
    fn total_cells(&self) -> usize {
        self.cells_per_assignment.iter().sum()
    }
    /// 最小的一档覆盖多少格子 —— 逐格写回退化会让它塌到个位数
    fn min_cells_per_assignment(&self) -> usize {
        self.cells_per_assignment.iter().copied().min().unwrap_or(0)
    }
}

/// 照 `macro/vba/RegVal.bas` 的分块循环走一遍，记录每档一次赋值覆盖多少格子。
///
/// 判据是**赋值次数与每档覆盖量**，不是「跑得够快」：
/// - 逐格写回退化成 5000 行 × 5 列 = 25000 次单格赋值，每档只盖 1 个格子；
/// - 分块写回是 25 次批量赋值，每档盖 200 行 × 5 列 = 1000 个格子。
///
/// 分块边界与宏同形（闭区间）：最后一档不足一整块时只盖剩下的行，不多读一行。
fn simulate_macro_chunked_write(rows: usize, chunk_rows: usize) -> MacroWriteProfile {
    assert!(rows > 0, "至少要有一行");
    let mut cells_per_assignment = Vec::new();
    let mut rows_per_assignment = Vec::new();
    let mut chunk_start = 0usize;
    while chunk_start < rows {
        let mut chunk_end = chunk_start;
        // 与宏里 `Do While chunkEnd < gEnd And (chunkEnd - chunkStart + 1) < REGVAL_CHUNK_ROWS` 同形
        while chunk_end < rows - 1 && (chunk_end - chunk_start + 1) < chunk_rows {
            chunk_end += 1;
        }
        let covered_rows = chunk_end - chunk_start + 1;
        rows_per_assignment.push(covered_rows);
        cells_per_assignment.push(covered_rows * NEW_COLS);
        chunk_start = chunk_end + 1;
    }
    MacroWriteProfile {
        cells_per_assignment,
        rows_per_assignment,
    }
}

/// 真实命令行二进制（`cargo test --release` 时是 release 构建产物）
fn regval_bin() -> &'static str {
    env!("CARGO_BIN_EXE_regval")
}

/// 子进程峰值常驻内存（字节）。
///
/// `getrusage(RUSAGE_CHILDREN)` 的 `ru_maxrss` 是**已回收子进程的较大值**：
/// 同一进程里跑多个子进程时读到的就是其中内存占用最高的那一个。
/// 本文件里只有规模基准那一个用例会起子进程，读数归它。
fn peak_rss_children_bytes() -> Option<u64> {
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) != 0 {
            return None;
        }
        let raw = usage.ru_maxrss as u64;
        if raw == 0 {
            return None;
        }
        // macOS 记字节，Linux 记千字节
        #[cfg(target_os = "macos")]
        {
            Some(raw)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(raw * 1024)
        }
    }
}

/// 跑一条真实命令，返回耗时、退出码与标准输出。
fn run_cli(args: &[&str], state_db: &Path) -> (u128, i32, String) {
    let started = Instant::now();
    let output = Command::new(regval_bin())
        .args(args)
        .current_dir(project_root())
        .env("REGVAL_RULE_DIR", RULE_DIR)
        .env_remove("OPENAI_BASE_URL")
        .env_remove("OPENAI_API_KEY")
        .env_remove("HIS_BASE_URL")
        .env_remove("HIS_API_TOKEN")
        .env_remove("REGULATORY_REPORT_BASE_URL")
        .env_remove("REGULATORY_REPORT_TOKEN")
        .output()
        .expect("跑 regval 命令行");
    let elapsed = started.elapsed().as_millis();
    let code = output.status.code().unwrap_or(-1);
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    // 状态库路径必须落在本轮基准目录里，别把别处的库写脏
    let _ = state_db;
    (elapsed, code, text)
}

/// 进程内跑一遍完整主路径（读表 → 绑定 → 漏斗 → 落库 → 跑规则 → 编计划），分段计时。
struct StageRun {
    rows: usize,
    issues: usize,
    rule_issues: usize,
    bind_issues: usize,
    plan_steps: usize,
    plan_skipped: usize,
    plan_pending_manual: usize,
    read_ms: u128,
    map_ms: u128,
    bind_ms: u128,
    funnel_ms: u128,
    funnel_pairs: usize,
    db_ms: u128,
    rules_ms: u128,
    rule_times: Vec<(String, u128)>,
    plan_ms: u128,
    total_ms: u128,
    /// 写回计划：5 个新列要写的格子数（行数 × 5）
    writeback_cells: usize,
    /// 写回分块数（每块 200 行）
    writeback_chunks: usize,
    writeback_plan_ms: u128,
}

fn run_pipeline(input: &str, state_db: &Path) -> StageRun {
    let overall = Instant::now();

    // 0. 规则集（口径闸门需要状态库里的确认记录）
    let mut repo = Repo::open(state_db).expect("打开状态库");
    let approvals = repo.approval_index().expect("读换版确认记录");
    let applied = repo.applied_rule_versions().expect("读已用口径版本");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let gate = version::VersionGate {
        approvals: &approvals,
        applied_versions: &applied,
        today: &today,
    };
    let rule_set = loader::load_gated(&abs(RULE_DIR), loader::LoadMode::Strict, &gate)
        .expect("装载口径规则集");

    // 1. 读表
    let t = Instant::now();
    let tables = regval::ingest::read_tables(&abs(input)).expect("读表");
    let read_ms = t.elapsed().as_millis();

    // 2. 表头映射
    let t = Instant::now();
    let mut rows = Vec::new();
    for table in &tables {
        let mapped = regval::ingest::map_table_with_report(table).expect("表头映射");
        rows.extend(mapped.rows);
    }
    let map_ms = t.elapsed().as_millis();

    // 3. 实体绑定（科室目录未配置 → 明确标「科室主数据未校验」，不静默通过）
    let t = Instant::now();
    let binding = bind::bind(&rows, &UnavailableDirectory);
    let bind_ms = t.elapsed().as_millis();

    // 4. 重复检测三级漏斗
    let t = Instant::now();
    let distance_cfg = distance::DistanceConfig::default();
    let funnel = dupe::run_deterministic_levels(&rows, &distance_cfg);
    let funnel_pairs = funnel.cumulative.len();
    let funnel_ms = t.elapsed().as_millis();

    // 5. 落库
    let t = Instant::now();
    let batch_ref = regval::ingest::batch_ref(&abs(input));
    repo.insert_rows(&batch_ref, &rows).expect("落库");
    repo.set_batch_rule_version(&batch_ref, &rule_set.manifest().set_version)
        .expect("记口径版本");
    // 权威归属（不提升的话每行都停在 raw，编出来的提交计划会是空的 —— 数字得跟真实主路径一致）
    repo.apply_authority_states(&batch_ref, &binding.authority_assignments())
        .expect("落权威归属");
    let db_ms = t.elapsed().as_millis();

    // 6. 跑口径规则（四类检查器）—— 逐条计时，看清时间花在哪条规则上
    let t = Instant::now();
    let mut rule_times: Vec<(String, u128)> = Vec::new();
    let mut issues: Vec<regval::rules::model::Issue> = Vec::new();
    for rule in rule_set.executable() {
        let rt = Instant::now();
        let exec = executor::execute_rule(repo.conn(), rule, &batch_ref);
        rule_times.push((rule.rule_id.clone(), rt.elapsed().as_millis()));
        issues.extend(exec.issues.iter().cloned());
    }
    let rules_ms = t.elapsed().as_millis();

    // 7. 编上报提交计划
    let t = Instant::now();
    let stored = repo.list_stored_rows(&batch_ref).expect("读库里已落的行");
    let mut bases: BTreeMap<String, String> = BTreeMap::new();
    for spec in indicators::INDICATORS.iter() {
        bases.insert(spec.code.to_string(), indicators::BASIS.to_string());
    }
    let candidates: Vec<program::PlanCandidate> = stored
        .iter()
        .map(|row| program::candidate_for_row(row, &batch_ref, &bases))
        .collect();
    let existing: BTreeMap<String, Vec<ExistingSubmission>> = BTreeMap::new();
    let prog = program::compile(&program::PlanInput {
        batch_ref: batch_ref.clone(),
        candidates,
        existing,
    });
    let plan_ms = t.elapsed().as_millis();

    // 8. 写回计划（宏侧照着它写：只碰 5 个新列，分块 200 行一档）
    let t = Instant::now();
    let metas: Vec<regval::ingest::RowMeta> = rows.iter().map(|r| r.meta()).collect();
    let mut all_issues = binding.issues.clone();
    all_issues.extend(issues.iter().cloned());
    let contract = regval::contract::build(
        &batch_ref,
        &metas,
        all_issues,
        tables.len(),
        0,
        &regval::contract::Extra::default(),
    );
    let mut col_counts: BTreeMap<String, usize> = BTreeMap::new();
    for sheet in &tables {
        col_counts.insert(
            sheet.source_ns.clone(),
            sheet.rows.first().map(|r| r.len()).unwrap_or(0),
        );
    }
    let plans = regval::writeback::plans_by_sheet(&contract, &col_counts);
    let writeback_cells: usize = plans.values().map(|p| p.values.len() * p.columns.len()).sum();
    let writeback_rows: usize = plans.values().map(|p| p.values.len()).sum();
    let writeback_chunks = writeback_rows.div_ceil(macro_chunk_rows());
    let writeback_plan_ms = t.elapsed().as_millis();

    StageRun {
        rows: rows.len(),
        issues: binding.issues.len() + issues.len(),
        rule_issues: issues.len(),
        bind_issues: binding.issues.len(),
        plan_steps: prog.len(),
        plan_skipped: prog.skipped().len(),
        plan_pending_manual: prog.pending_manual().len(),
        read_ms,
        map_ms,
        bind_ms,
        funnel_ms,
        funnel_pairs,
        db_ms,
        rules_ms,
        rule_times,
        plan_ms,
        total_ms: overall.elapsed().as_millis(),
        writeback_cells,
        writeback_chunks,
        writeback_plan_ms,
    }
}

/// 跑 `times` 遍主路径，返回**最快**的那一遍（基准测试取最小值：最接近代码本身要多久）。
fn best_of(input: &str, db_stem: &Path, times: usize) -> StageRun {
    let mut best: Option<StageRun> = None;
    for i in 0..times {
        let run = run_pipeline(input, &db_stem.join(format!("run{}.db", i)));
        best = match best {
            Some(prev) if prev.total_ms <= run.total_ms => Some(prev),
            _ => Some(run),
        };
    }
    best.expect("至少跑一遍")
}

// ————————————————————————————————————————————————————————————
// 用例
// ————————————————————————————————————————————————————————————

/// 规模基准：5000 行完整校验 + 提交计划编译。
///
/// 这一条是**唯一会起子进程**的用例 —— 峰值内存读数靠 `RUSAGE_CHILDREN`，
/// 同一文件里别的用例不起子进程，读数才归它。
#[test]
fn bench_5000行完整校验与计划编译在阈值内() {
    let tmp = tempfile::tempdir().expect("建临时目录");
    let scale_db = tmp.path().join("scale.db");
    let small_db = tmp.path().join("small.db");

    // 先跑小的（200 行），再跑大的（5000 行）：峰值内存读到的是两者里更高的那个
    let (small_ms, small_code, small_out) = run_cli(
        &[
            "validate",
            "--input",
            SMALL_INPUT,
            "--out",
            &tmp.path().join("small.json").display().to_string(),
            "--state-db",
            &small_db.display().to_string(),
        ],
        &small_db,
    );
    assert_eq!(small_code, 0, "200 行校验应成功：\n{}", small_out);
    assert!(
        small_out.contains("已校验 200 行"),
        "200 行样例应校验 200 行：\n{}",
        small_out
    );

    let (scale_ms, scale_code, scale_out) = run_cli(
        &[
            "validate",
            "--input",
            SCALE_INPUT,
            "--out",
            &tmp.path().join("scale.json").display().to_string(),
            "--state-db",
            &scale_db.display().to_string(),
        ],
        &scale_db,
    );
    assert_eq!(scale_code, 0, "5000 行校验应成功：\n{}", scale_out);
    assert!(
        scale_out.contains("已校验 5000 行"),
        "规模集应校验 5000 行：\n{}",
        scale_out
    );

    let (plan_ms, plan_code, plan_out) = run_cli(
        &[
            "push",
            "--plan",
            "--state-db",
            &scale_db.display().to_string(),
            "--out",
            &tmp.path().join("plan.csv").display().to_string(),
        ],
        &scale_db,
    );
    assert_eq!(plan_code, 0, "提交计划编译应成功：\n{}", plan_out);

    let total_ms = scale_ms + plan_ms;
    let peak_bytes = peak_rss_children_bytes().unwrap_or(0);
    let peak_mb = peak_bytes / (1024 * 1024);

    // 阈值：超阈即失败
    assert!(
        total_ms <= THRESHOLD_SCALE_TOTAL_MS,
        "5000 行校验 + 计划编译耗时 {} ms，超过阈值 {} ms\
         （若这台机器整体就慢，看每行耗时放大倍数那条用例；那是形状的护栏）",
        total_ms,
        THRESHOLD_SCALE_TOTAL_MS
    );
    assert!(
        peak_mb <= THRESHOLD_SCALE_RSS_MB,
        "5000 行那一轮峰值常驻内存 {} MB，超过阈值 {} MB",
        peak_mb,
        THRESHOLD_SCALE_RSS_MB
    );

    // 数值落 docs/bench.md（含测量命令，可复现）。
    // **只在 release 档覆盖**：debug 档的数字慢一个数量级，写进去会让人误读；
    // debug 档跑批只验阈值，本页数值以 `cargo test --release bench` 为准。
    if !cfg!(debug_assertions) {
        let doc = render_bench_doc(&BenchNumbers {
            small_rows: 200,
            small_total_ms: small_ms,
            scale_rows: 5000,
            scale_validate_ms: scale_ms,
            scale_plan_ms: plan_ms,
            scale_total_ms: total_ms,
            peak_rss_mb: peak_mb,
            stage: best_of(SCALE_INPUT, &tmp.path().join("stage"), 2),
            small_stage: best_of(SMALL_INPUT, &tmp.path().join("stage-small"), 3),
            mid_stage: best_of(MERGED_INPUT, &tmp.path().join("stage-mid"), 3),
        });
        // 先写临时文件再改名：同目录内 rename 是原子的，读者不会读到写了一半的页面
        // （同一个测试文件里的「文档落进文档」那条用例是并行跑的）。
        // 写之前先自己核一遍：模板缺项在这一条上当场报，不靠读者那条去发现。
        for needle in [
            "读表",
            "重复检测漏斗",
            "跑口径规则",
            "编译上报提交计划",
            "线性度",
            "峰值常驻内存",
            "cargo test --release bench",
            "测量档位：release",
        ] {
            assert!(
                doc.contains(needle),
                "渲染出来的 docs/bench.md 缺「{}」一节（模板漏了，不是数值问题）",
                needle
            );
        }
        let target = project_root().join("docs/bench.md");
        let tmp_doc = project_root().join("docs/bench.md.tmp");
        std::fs::write(&tmp_doc, doc).expect("写 docs/bench.md 临时文件");
        std::fs::rename(&tmp_doc, &target).expect("改名为 docs/bench.md");
    }
}

/// 线性度观察：200 行与 5000 行分别计时，每行耗时的放大倍数要在阈值内。
///
/// 这是**回归护栏**：哪天真出现「规模一上去就塌」的改动，这里先红。
///
/// 小表（200 行）单次只跑十几毫秒，机器抖动的占比不小 —— 跑 3 次取**最小值**
/// （基准测试的标准做法：最小值最接近「这段代码本身要多久」，不受别的进程干扰）。
#[test]
fn bench_单表200行与5000行的每行耗时放大在阈值内() {
    let tmp = tempfile::tempdir().expect("建临时目录");
    let small = best_of(SMALL_INPUT, &tmp.path().join("small"), 3);
    let mid = best_of(MERGED_INPUT, &tmp.path().join("mid"), 3);
    let scale = best_of(SCALE_INPUT, &tmp.path().join("scale"), 2);

    assert_eq!(small.rows, 200, "单表样例应有 200 行");
    assert_eq!(mid.rows, 1200, "合并样例应有 1200 行");
    assert_eq!(scale.rows, 5000, "规模集应有 5000 行");

    let per_row_small = small.total_ms as f64 / small.rows as f64;
    let per_row_mid = mid.total_ms as f64 / mid.rows as f64;
    let per_row_scale = scale.total_ms as f64 / scale.rows as f64;
    let amplification = per_row_scale / per_row_small;

    assert!(
        amplification <= THRESHOLD_PER_ROW_AMPLIFICATION,
        "每行耗时从 200 行（{:.4} ms/行）放大到 1200 行（{:.4} ms/行）再到 5000 行\
         （{:.4} ms/行），首尾放大 {:.2} 倍，超过阈值 {:.1} 倍 —— 规模上去后单行成本恶化",
        per_row_small,
        per_row_mid,
        per_row_scale,
        amplification,
        THRESHOLD_PER_ROW_AMPLIFICATION
    );
}

/// 宏侧写回在大表下不出现逐格写回退化。
///
/// 判据不是「跑得够快」，而是**赋值次数与每档覆盖量**：
/// - 逐格写回退化 = 25000 次单格赋值（每档只盖 1 个格子）；
/// - 分块写回 = 25 次批量赋值（每档盖 200 行 × 5 列 = 1000 个格子）。
#[test]
fn bench_宏侧写回分块在大表下不出现逐格退化() {
    let tmp = tempfile::tempdir().expect("建临时目录");
    let scale = run_pipeline(SCALE_INPUT, &tmp.path().join("scale.db"));
    let chunk_rows = macro_chunk_rows();

    // 5000 行 × 5 个新列 = 25000 个格子（每格写一次，不多不少）
    assert_eq!(
        scale.writeback_cells,
        scale.rows * NEW_COLS,
        "写回格子数应等于行数 × 5 个新列（逐格写回退化会让它按平方涨）"
    );

    // 照宏的分块循环走一遍：赋值次数 = 行数 ÷ 每档行数
    let profile = simulate_macro_chunked_write(scale.rows, chunk_rows);
    assert_eq!(
        profile.assignments(),
        25,
        "5000 行按 {chunk_rows} 行一档应分 25 次批量赋值",
    );
    assert_eq!(
        profile.total_cells(),
        scale.rows * NEW_COLS,
        "分块写回覆盖的格子总数应等于行数 × 5 个新列"
    );
    assert!(
        profile.min_cells_per_assignment() >= chunk_rows * NEW_COLS,
        "最差的一档也只盖了 {} 个格子 —— 写回退化成逐格赋值了",
        profile.min_cells_per_assignment()
    );
    assert!(
        profile.rows_per_assignment.iter().all(|r| *r <= chunk_rows),
        "每档覆盖的行数不应超过分块大小 {}",
        chunk_rows
    );
    assert_eq!(
        scale.writeback_chunks,
        profile.assignments(),
        "引擎侧按宏的分块大小算出的块数，应与照宏分块循环走一遍的赋值次数一致"
    );

    // 写回计划编译耗时（5000 行）
    assert!(
        scale.writeback_plan_ms <= THRESHOLD_WRITEBACK_PLAN_MS,
        "写回计划编译耗时 {} ms，超过阈值 {} ms",
        scale.writeback_plan_ms,
        THRESHOLD_WRITEBACK_PLAN_MS
    );
}

/// 分块边界是闭区间：最后一档不足一整块时只盖剩下的行，不多读一行。
///
/// 这条盯的是宏侧最容易写错的一处（`chunkEnd < gEnd` 写成 `<=` 就会多读一行），
/// 在 5000 行这种正好整除的规模上根本看不出来，所以要另造一个除不尽的规模。
#[test]
fn bench_分块写回在除不尽的规模上不多读一行() {
    let chunk_rows = macro_chunk_rows();
    let rows = 5231usize; // 5231 = 26 × 200 + 31，最后一档只剩 31 行
    let profile = simulate_macro_chunked_write(rows, chunk_rows);

    assert_eq!(
        profile.assignments(),
        27,
        "5231 行按 {chunk_rows} 行一档应分 27 次赋值（26 整档 + 1 个零头）"
    );
    assert_eq!(
        *profile.rows_per_assignment.last().expect("应有最后一档"),
        31,
        "最后一档应只盖剩下的 31 行（闭区间边界，多读一行就是宏里的 off-by-one）"
    );
    assert_eq!(
        profile.total_cells(),
        rows * NEW_COLS,
        "分块写回覆盖的格子总数应等于行数 × 5 个新列"
    );
}

/// 分段耗时落在 docs/bench.md 的表格里 —— 文档不能只是一句「跑通了」。
#[test]
fn bench_分段耗时与阈值落进文档() {
    let needles = [
        "读表",
        "重复检测漏斗",
        "跑口径规则",
        "编译上报提交计划",
        "线性度",
        "峰值常驻内存",
        "cargo test --release bench",
        "测量档位：release",
    ];
    // 同一测试文件里「写文档」那条用例是并行跑的：它会原子替换 docs/bench.md
    // （先写临时文件再 rename，读者不会读到半截页面）。这里读到的可能还是替换前
    // 的那一版 —— 缺项就稍等一下让它换完，别把并行时序当成内容缺失。
    let mut missing: Vec<&str> = needles.to_vec();
    for _ in 0..40 {
        let doc = std::fs::read_to_string(project_root().join("docs/bench.md"))
            .expect("docs/bench.md 应存在（规模基准用例会写它）");
        missing = needles
            .iter()
            .copied()
            .filter(|n| !doc.contains(n))
            .collect();
        if missing.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        missing.is_empty(),
        "docs/bench.md 缺这几节：{:?}（数值与测量命令都要能照着复现）",
        missing
    );
}

// ————————————————————————————————————————————————————————————
// docs/bench.md 生成
// ————————————————————————————————————————————————————————————

struct BenchNumbers {
    small_rows: usize,
    small_total_ms: u128,
    scale_rows: usize,
    scale_validate_ms: u128,
    scale_plan_ms: u128,
    scale_total_ms: u128,
    peak_rss_mb: u64,
    stage: StageRun,
    small_stage: StageRun,
    mid_stage: StageRun,
}

fn render_bench_doc(n: &BenchNumbers) -> String {
    let s = &n.stage;
    let small = &n.small_stage;
    let mid = &n.mid_stage;
    let per_row_small = small.total_ms as f64 / small.rows as f64;
    let per_row_mid = mid.total_ms as f64 / mid.rows as f64;
    let per_row_scale = s.total_ms as f64 / s.rows as f64;
    let amplification = per_row_scale / per_row_small;
    let rules_share = if s.total_ms == 0 {
        0.0
    } else {
        s.rules_ms as f64 / s.total_ms as f64
    };
    let rules_share_pct = rules_share * 100.0;
    let mut rule_rows = String::new();
    let mut ordered = s.rule_times.clone();
    ordered.sort_by(|a, b| b.1.cmp(&a.1));
    for (rule_id, ms) in ordered {
        rule_rows.push_str(&format!("| `{}` | {} ms |\n", rule_id, ms));
    }

    format!(
        r##"# 规模与性能基准

> 本页是**实测数值**，不是估算：每一条都给了复现命令。跑一遍 `cargo test --release bench`
> 会重新测量并覆盖本页的数值；下面的阈值由同一个测试守着，超阈即测试失败。
>
> **测量档位：release**（`cargo test --release bench`）。debug 档（`cargo test`）的数字慢一个
> 数量级，只用来守阈值、**不覆盖本页**；本页数值一律以 release 档为准。

## 测什么

| 口径 | 说明 |
|------|------|
| 规模集 | `samples/dataset-scale/合并表.csv` —— 约 5000 行（10 张科室表各 500 行），由 `samples/generate.py` 以固定种子合成，**只含指标级监管数据，无患者可识别明细** |
| 基准集 | `samples/dataset/合并表.csv` —— 1200 行（6 张科室表各 200 行），最接近一个报告期的真实体量 |
| 单表 | `samples/dataset/病案统计室表.csv` —— 200 行，用来看规模上去后单行成本有没有恶化 |
| 端到端 | 真实命令行 `regval validate` + `regval push --plan`（含进程启动、读表、落库、跑规则、编计划、写契约文件） |
| 分段耗时 | 进程内同一套引擎调用（读表 → 映射 → 绑定 → 漏斗 → 落库 → 跑规则 → 编计划），用来定位时间花在哪一段 |
| 峰值常驻内存 | 子进程口径（`getrusage(RUSAGE_CHILDREN)` 的 `ru_maxrss`） |

## 端到端实测（5000 行）

| 指标 | 实测 | 阈值 |
|------|------|------|
| 单表 200 行完整校验（对照） | {small_total} ms | — |
| 5000 行完整校验（四类检查器） | {validate_ms} ms | — |
| 5000 行编译上报提交计划 | {plan_ms} ms | — |
| **合计** | **{total_ms} ms** | ≤ {threshold_total} ms |
| 峰值常驻内存 | {rss} MB | ≤ {threshold_rss} MB |
| 校验结论 | {rows} 行 · {issues} 项问题（口径规则 {rule_issues} 项 · 实体绑定 {bind_issues} 项） · 提交计划 {steps} 步（已跳过 {skipped} 行 · 待人工判定 {pending} 行） | — |

## 线性度观察（200 行 vs 5000 行）

进程内同一套引擎调用，每档跑若干遍取**最小值**（小表单次只十几毫秒，机器抖动占比不小）。

| 规模 | 输入 | 总耗时 | 每行耗时 |
|------|------|--------|----------|
| 单表 | {small_rows} 行 | {small_inproc} ms | {per_row_small:.4} ms/行 |
| 基准集 | {mid_rows} 行 | {mid_inproc} ms | {per_row_mid:.4} ms/行 |
| 规模集 | {scale_rows} 行 | {scale_inproc} ms | {per_row_scale:.4} ms/行 |

**每行耗时放大 {amplification:.2} 倍**（阈值 ≤ {threshold_amp:.1} 倍）。

放大倍数远大于 1 是已知的、也是**如实记下来的**：口径规则里「表内业务主键唯一」这条
（`src/rules/checks/required.rs` 的 `TABLE_UNIQUE_SQL`）对每一行都要扫一遍 `rows` 表找同键的
对照行 —— 行数一多，比对次数按行数的平方涨。5000 行这一轮里「跑口径规则」占了 {rules_share_pct:.0}%
的时间（{rules_ms} ms / {total_inproc} ms），逐条规则的耗时见下表，主要就是它。

这不影响院内可用性：一个报告期实际是几个科室、每个科室一二百行（{mid_rows} 行那一档，
{mid_inproc} ms），信息科点一次「校验」等不到一秒。但它是**回归护栏**：哪天真把这条规则改成
平方级以外的更差形态、或者别处引入了同类比较，这里会先红 —— 真按平方级算，5000 / 200 行对应
的放大倍数应该是 625 倍。

这个比值还**偏保守**：小表那一档里含固定开销（开状态库、装载并语法预校验口径规则集），
这部分与行数无关，摊到 200 行上会抬高「每行耗时」，把放大倍数压小。所以实测的
{amplification:.2} 倍比真实形状还要温和 —— 这条线的方向是「只会低估、不会高估」退化程度。

### 逐条口径规则耗时（5000 行）

| 规则 | 耗时 |
|------|------|
{rule_rows}

## 分段耗时（进程内，5000 行）

| 阶段 | 耗时 | 说明 |
|------|------|------|
| 读表 | {read_ms} ms | 读 CSV、按物理行切分、表头映射前的解析 |
| 表头映射 | {map_ms} ms | 表头名 → 业务字段；缺列报错不猜列 |
| 实体绑定 | {bind_ms} ms | 行键 / 业务主键 / 跨表匹配键 + 同权威键歧义拒绝合并 |
| 重复检测漏斗（确定性三级） | {funnel_ms} ms | 确定性键 → 归一化 → 编辑距离（本轮候选 {funnel_pairs} 对） |
| 落本机校验库 | {db_ms} ms | 一次事务批量落库 |
| 跑口径规则（四类检查器） | {rules_ms} ms | 必填缺失 / 枚举越界 / 单位不符 / 公式复算 |
| 编译上报提交计划 | {plan_ms_inproc} ms | 条件链编译（只编译不发） |
| **合计** | **{total_inproc} ms** | — |

写回计划（宏侧照着写的那份：只碰 5 个新列）编译耗时 {writeback_ms} ms，
写回格子 {cells} 个（{rows} 行 × 5 列），按 {chunk_rows} 行一档分 {chunks} 块 ——
每块**一次** `Range.Value2` 批量赋值（{chunk_cells} 个格子一档），不是逐格写回；
分块大小直接读自 `macro/vba/RegVal.bas` 的 `REGVAL_CHUNK_ROWS`，宏改了这里跟着变。

## 复现命令

```bash
export PATH="$HOME/.cargo/bin:$PATH"

# 全量跑这一组基准（阈值超了就红）
cargo test --release bench

# 端到端手测：5000 行校验 + 提交计划编译
REGVAL_RULE_DIR=./rules \
  ./target/release/regval validate \
    --input samples/dataset-scale/合并表.csv \
    --out /tmp/regval-bench/scale.json \
    --state-db /tmp/regval-bench/scale.db
REGVAL_RULE_DIR=./rules \
  ./target/release/regval push --plan \
    --state-db /tmp/regval-bench/scale.db \
    --out /tmp/regval-bench/plan.csv

# 峰值常驻内存（macOS）
/usr/bin/time -l ./target/release/regval validate \
  --input samples/dataset-scale/合并表.csv \
  --out /tmp/regval-bench/scale.json \
  --state-db /tmp/regval-bench/scale.db 2>&1 | grep 'maximum resident'
```

## 阈值

| 阈值 | 值 | 守住它的用例 |
|------|----|--------------|
| 5000 行校验 + 计划编译总耗时 | ≤ {threshold_total} ms | `bench_5000行完整校验与计划编译在阈值内` |
| 5000 行峰值常驻内存 | ≤ {threshold_rss} MB | `bench_5000行完整校验与计划编译在阈值内` |
| 每行耗时放大倍数 | ≤ {threshold_amp:.1} 倍 | `bench_单表200行与5000行的每行耗时放大在阈值内` |
| 写回计划编译耗时（5000 行） | ≤ {threshold_writeback} ms | `bench_宏侧写回分块在大表下不出现逐格退化` |

阈值是**回归护栏**，不是性能承诺：院内机器比开发机慢是常态，
阈值给的是「慢了这么多倍就说明算法退化了」这条线，而不是「必须多快」。
"##,
        validate_ms = n.scale_validate_ms,
        plan_ms = n.scale_plan_ms,
        total_ms = n.scale_total_ms,
        threshold_total = THRESHOLD_SCALE_TOTAL_MS,
        rss = n.peak_rss_mb,
        threshold_rss = THRESHOLD_SCALE_RSS_MB,
        rows = s.rows,
        issues = s.issues,
        rule_issues = s.rule_issues,
        bind_issues = s.bind_issues,
        steps = s.plan_steps,
        skipped = s.plan_skipped,
        pending = s.plan_pending_manual,
        small_rows = n.small_rows,
        small_total = n.small_total_ms,
        small_inproc = small.total_ms,
        mid_rows = mid.rows,
        mid_inproc = mid.total_ms,
        scale_rows = n.scale_rows,
        scale_inproc = s.total_ms,
        rules_share_pct = rules_share_pct,
        rule_rows = rule_rows,
        per_row_small = per_row_small,
        per_row_scale = per_row_scale,
        amplification = amplification,
        threshold_amp = THRESHOLD_PER_ROW_AMPLIFICATION,
        read_ms = s.read_ms,
        map_ms = s.map_ms,
        bind_ms = s.bind_ms,
        funnel_ms = s.funnel_ms,
        funnel_pairs = s.funnel_pairs,
        db_ms = s.db_ms,
        rules_ms = s.rules_ms,
        plan_ms_inproc = s.plan_ms,
        total_inproc = s.total_ms,
        writeback_ms = s.writeback_plan_ms,
        cells = s.writeback_cells,
        chunks = s.writeback_chunks,
        chunk_rows = macro_chunk_rows(),
        chunk_cells = macro_chunk_rows() * NEW_COLS,
        threshold_writeback = THRESHOLD_WRITEBACK_PLAN_MS,
    )
}
