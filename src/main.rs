//! 卫健上报校验系统 —— 引擎入口。
//!
//! 主路径：`regval validate --input <表> --out <契约 JSON>`
//! 读表 → 落本机校验库 → 跑口径规则 → 产出结果契约 JSON（写回原表由 Excel/WPS 宏完成）。
//!
//! 10 个子命令：
//! | 命令 | 作用 | 本轮状态 |
//! |------|------|----------|
//! | `validate` | 读表并产出结果契约 JSON | 已就绪 |
//! | `push` | 提交已确认行到院内卫健上报网关（回执驱动行状态） | 已就绪 |
//! | `status` | 运行摘要与行状态分布 | 已就绪 |
//! | `rules` | 规则清单与规则目录状态 | 已就绪 |
//! | `doctor` | 运行环境与凭据逐项自检 | 已就绪 |
//! | `dept` | 院内 HIS 科室主数据目录状态 | 已就绪 |
//! | `bench` | 重复检测漏斗的标注基准集：逐级召回率 / 精确率对比表 | 已就绪 |
//! | `compare` | 与基线方案的可复现对比（重复检测轴 + 问题发现轴） | 已就绪 |
//! | `tokens` | 视觉 token 清单（JSON）/ 宏用常量模块（VBA） | 已就绪 |
//! | `messages` | 提示文案常量表（JSON）/ 宏用常量模块（VBA） | 已就绪 |
//! | `merge` / `export` / `replay` / `confirm` / `revoke` / `confirmations` / `recover` / `audit` | 合并、导出、复算、确认流、恢复与审计回放 | 已就绪 |

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand, ValueEnum};

use regval::bind;
use regval::compare;
use regval::config::{self, PartialConfig, ProcessEnv};
use regval::contract;
use regval::db::repo::Repo;
use regval::db::schema::AUTHORITY_AUTHORITATIVE;
use regval::doctor::{self, DoctorInput};
use regval::dupe;
use regval::errors;
use regval::errors::{EngineError, ErrorCode};
use regval::ingest;
use regval::log::{self, Level, RunSummary};
use regval::model;
use regval::model::{ModelCallAuditSink, ModelRequirement};
use regval::push;
use regval::push::dept::DeptDirectorySection;
use regval::push::idempotency::{self, ExistingSubmission, PayloadInput};
use regval::report;
use regval::rules::{executor, indicators, loader, seed, version};
use regval::state;
use regval::ui;

#[derive(Parser, Debug)]
#[command(
    name = "regval",
    version,
    about = "卫健上报校验系统引擎：读表 → 落本机库 → 口径规则 → 结果契约 JSON",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 读表 → 落本机库 → 跑口径规则 → 产出结果契约 JSON
    Validate {
        /// 输入表（.csv / .xlsx / .xlsm）
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
        /// 结果契约 JSON 输出路径
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
        /// 表格插件侧要求的契约版本（与引擎不一致则拒绝写回契约文件）
        #[arg(long, value_name = "VERSION")]
        expect_schema_version: Option<String>,
        /// 模型端点不可用时直接报错退出，不静默降级成「只有确定性候选」
        #[arg(long, default_value_t = false)]
        require_model: bool,
        /// 出错时把错误码与中文说明写成 JSON（表格插件据此提示，不解析 stderr）
        #[arg(long, value_name = "PATH")]
        error_out: Option<PathBuf>,
    },
    /// 提交已确认行到院内卫健上报网关（幂等键 + 回执驱动行状态）
    ///
    /// 默认走**发送路径**：编译条件批量程序 → 过写闸门 → 逐步上报 → 回执写回本机库。
    /// 批次中断后重跑会**从最后一个已确认成功的步骤继续**，已上报的行不重复推送。
    /// `--preview` / `--plan` 是只算不发的两条旁路（核对上报身份与提交计划）。
    Push {
        /// 批次号（缺省为状态库里最近一个批次）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 模型不可达时直接报错，不静默降级（上报路径不调用模型端点，此开关只留给校验）
        #[arg(long, default_value_t = false)]
        require_model: bool,
        /// 上报身份预览：只算幂等键与载荷摘要（**只算不发**，不联网、不改状态库）
        #[arg(long, default_value_t = false)]
        preview: bool,
        /// 编译上报提交计划并打印（**只编译不发**：第 i 步只在第 i-1 步成功后执行）
        #[arg(long, default_value_t = false)]
        plan: bool,
        /// 产物路径（`--plan` 出提交计划 .csv/.json；发送路径出执行报告 .json）
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
        /// 出错时把错误码与中文说明写成 JSON（表格插件据此提示，不解析 stderr）
        #[arg(long, value_name = "PATH")]
        error_out: Option<PathBuf>,
    },
    /// 查看本机状态库里的运行摘要与批次汇总
    Status {
        /// 只看某个批次
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 最多显示几条（新的在前）
        #[arg(long, default_value_t = 10, value_name = "N")]
        limit: usize,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 查看口径规则清单与规则目录状态
    Rules {
        /// 规则目录（默认取环境变量 REGVAL_RULE_DIR）
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// 把规则清单写成 JSON 文件（可核对产物）
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB；配了就按换版闸门给出生效口径）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
        /// 子动作（不给则只查看清单）
        #[command(subcommand)]
        action: Option<RulesAction>,
    },
    /// 应用已人工确认的疑似重复合并（逐条勾选：一对一行）
    Merge {
        /// 批次号
        #[arg(long, value_name = "REF")]
        batch: String,
        /// 保留的行键（继续以这一行为准上报）
        #[arg(long, value_name = "ROW_UID")]
        row: String,
        /// 并进来的行键（不再单独上报）
        #[arg(long, value_name = "ROW_UID")]
        drop_row: String,
        /// 确认人 —— 不给就报错：没有人的确认等于没人负责
        #[arg(long, value_name = "NAME")]
        by: String,
        /// 合并理由（写给复核人看）
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 记一条人工确认（复核通过，这一行可以上报）
    ///
    /// 模型给的语义结论**只是标注** —— 合并与否由经办人逐条勾选确认后才生效。
    /// 确认记录追加写进本机库的 `confirmations` 表：确认人、确认时间、行键都留痕。
    Confirm {
        /// 被确认的行键（结果契约 `rows[].row_uid` 里的那一串）
        #[arg(long, value_name = "ROW_UID")]
        row: String,
        /// 确认人 —— 不给就报错：没有人的确认等于没人负责
        #[arg(long, value_name = "NAME")]
        by: String,
        /// 对方行键（确认的是一对行时写上；可省）
        #[arg(long, value_name = "ROW_UID")]
        with: Option<String>,
        /// 批次号（缺省取状态库里最近一个批次）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 备注（确认理由，写给复核人看）
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 撤销一条人工确认（这一行退回「等重新确认」）
    Revoke {
        /// 被撤销确认的行键
        #[arg(long, value_name = "ROW_UID")]
        row: String,
        /// 撤销人 —— 不给就报错：没有人的撤销等于没人负责
        #[arg(long, value_name = "NAME")]
        by: String,
        /// 批次号（缺省取状态库里最近一个批次）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 撤销理由（写给复核人看）
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 查看人工确认记录
    Confirmations {
        /// 只看某个批次（缺省看全部）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 导出未上报名单 / 依据举证包 / 回执台账 / 口径差异清单 / 编辑距离候选
    Export {
        /// 导出内容
        #[arg(value_enum)]
        what: ExportWhat,
        /// 输出文件路径（.csv 出表格，其余出 JSON）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 只导某个批次（缺省取库里最近一次校验的批次）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
        /// 口径差异清单的旧版本（缺省取状态库里实际用过的版本）
        #[arg(long, value_name = "VERSION")]
        from_version: Option<String>,
        /// 口径差异清单的新版本（缺省取规则目录里版本最高的那一版）
        #[arg(long, value_name = "VERSION")]
        to_version: Option<String>,
        /// 规则目录（缺省取环境变量 REGVAL_RULE_DIR）
        #[arg(long, value_name = "PATH")]
        rule_dir: Option<PathBuf>,
        /// 结果契约文件路径（导出编辑距离候选用；即 regval validate --out 写出的那份）
        #[arg(long, value_name = "PATH")]
        contract: Option<PathBuf>,
        /// 催报对照输出路径（导出未上报名单时用；.csv 出表格，其余出 JSON）
        #[arg(long, value_name = "PATH")]
        reminder_out: Option<PathBuf>,
    },
    /// 按原规则版本复算历史批次
    Replay {
        /// 批次号
        #[arg(long, value_name = "REF")]
        batch: String,
        /// 规则版本
        #[arg(long, value_name = "VERSION")]
        rule_version: String,
    },
    /// 运行环境与凭据逐项自检（缺哪一项、怎么补，逐项列出）
    Doctor {
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 查看院内 HIS 科室主数据目录状态（能不能拉到、多少条、数据时间、用的哪一份）
    Dept {
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB；目录缓存存在这里）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 跑重复检测漏斗的标注基准集，产出逐级召回率 / 精确率对比表
    Bench {
        /// 基准集目录（内含 tables/ 与 labels.json）
        #[arg(long, value_name = "DIR")]
        bench_dir: PathBuf,
        /// 对比表输出路径（.csv 出表格，其余出 JSON）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 编辑距离阈值（缺省取环境变量 / 规则目录 / 内置默认值）
        #[arg(long, value_name = "N")]
        threshold: Option<usize>,
    },
    /// 与基线方案做可复现对比（重复检测轴 + 问题发现轴，产出一张对比表）
    Compare {
        /// 标注基准集目录（内含 tables/ 与 labels.json）—— 轴一的样例与标注
        #[arg(long, value_name = "DIR")]
        bench_dir: PathBuf,
        /// 轴二的输入表（.csv / .xlsx / .xlsm）—— 量「仅规则校验 vs 加公式复算」的问题发现数
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
        /// 对比表输出路径（.csv 出表格，其余出 JSON）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 编辑距离阈值（缺省取环境变量 / 规则目录 / 内置默认值）
        #[arg(long, value_name = "N")]
        threshold: Option<usize>,
        /// 规则目录（缺省用内置兜底口径表）
        #[arg(long, value_name = "DIR")]
        rule_dir: Option<PathBuf>,
    },
    /// 恢复损坏的状态库（按幂等键与回执记录恢复到最后一个一致状态）
    Recover {
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
        /// 恢复报告输出路径（.json 出结构化报告；不给就只打印）
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// 把恢复库换上去（原库先挪成 .corrupt-N 备份，**不删**）
        #[arg(long, default_value_t = false)]
        apply: bool,
    },
    /// 回放审计链（按行 / 按批次），并核对幂等键 ↔ 回执 ID 的一一对应
    Audit {
        /// 回放某一行（给行键）
        #[arg(long, value_name = "ROW_UID")]
        row: Option<String>,
        /// 回放某一批（给批次号）
        #[arg(long, value_name = "REF")]
        batch: Option<String>,
        /// 只核对幂等键 ↔ 回执 ID 的一一对应
        #[arg(long, default_value_t = false)]
        idempotency: bool,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 导出视觉 token（配色 / 字体 / 列宽 / 样式映射）
    ///
    /// 表格内插件的样式不在宏里写死：token 的唯一出处是 `src/ui/tokens.rs`，
    /// 由本命令导出成 JSON 清单（文档与 WPS 侧共用）或宏用的 VBA 常量模块。
    Tokens {
        /// 输出文件路径（.bas / .vba 出宏用常量模块，其余出 JSON 清单）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 强制输出宏用的 VBA 常量模块（缺省按扩展名判断）
        #[arg(long, default_value_t = false)]
        vba: bool,
    },
    /// 导出提示文案常量表（JSON 清单 / 宏用常量模块）
    ///
    /// 表格内插件面向用户的中文不在宏里写死：文案与错误码中文说明的
    /// 唯一出处是 `src/ui/messages.rs` 与 `src/errors.rs`，由本命令导出成
    /// 宏侧常量模块 `macro/vba/RegValMessages.bas`。
    Messages {
        /// 输出文件路径（.bas / .vba 出宏用常量模块，其余出 JSON 清单）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 强制输出宏用的 VBA 常量模块（缺省按扩展名判断）
        #[arg(long, default_value_t = false)]
        vba: bool,
    },
    /// 装配表格内插件的 WPS JSA 宏（手写模板 + 引擎侧常量）
    ///
    /// WPS JSA 的宏是一个独立加载的 `.js` 文件，运行时没有模块加载器 ——
    /// token 与文案常量必须内联。宏里不写第二套口径：契约常量、视觉 token、
    /// 提示文案都从引擎侧注入，改一处三入口同时变。
    Macro {
        /// 输出文件路径（默认 macro/wps/regval.js）
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// 模板路径（缺省 macro/wps/regval.template.js）
        #[arg(long, value_name = "PATH")]
        template: Option<PathBuf>,
    },
}

/// 导出内容
#[derive(ValueEnum, Debug, Clone, Copy)]
enum ExportWhat {
    /// 未上报名单与催报对照
    Pending,
    /// 每行依据举证包
    Evidence,
    /// 回执台账（留档与第三方核对用）
    Receipts,
    /// 口径差异清单（换版前给质控岗与科室看）
    Diff,
    /// 编辑距离候选与距离分布直方（重复检测第三级）
    Dupe,
}

/// `regval rules` 的子动作
#[derive(Subcommand, Debug)]
enum RulesAction {
    /// 记一条换版确认（质控岗确认某个口径版本可以生效）
    Approve {
        /// 被确认的新口径版本
        #[arg(long, value_name = "VERSION")]
        version: String,
        /// 确认人（质控岗）—— 不给就报错：没有人的口径换版等于没人负责
        #[arg(long, value_name = "NAME")]
        approver: String,
        /// 被替换的旧口径版本（可省）
        #[arg(long, value_name = "VERSION")]
        from_version: Option<String>,
        /// 只确认某一个口径槽位（可省；省 = 整套口径都算数）
        #[arg(long, value_name = "SLOT")]
        slot: Option<String>,
        /// 备注（换版原因、口径文件文号）
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
    /// 查看已有的换版确认记录
    Approvals {
        /// 本机校验库路径（默认取环境变量 REGVAL_STATE_DB）
        #[arg(long, value_name = "PATH")]
        state_db: Option<PathBuf>,
    },
}

fn main() {
    // 本机开发用的 .env（从 .env.example 复制）：只补进程环境里没有的键，进程环境优先
    let _ = config::load_dotenv(Path::new(".env"));

    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            log::log(
                Level::Error,
                "command.failed",
                None,
                Some(&e.code),
                &e.message,
                &[],
            );
            eprintln!("错误：[{}] {}", e.code.as_str(), e.message);
            if let Some(loc) = &e.location {
                eprintln!("位置：{}", loc);
            }
            eprintln!("处理建议：{}", e.code.advice());
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<i32, EngineError> {
    match cli.command {
        Command::Validate {
            input,
            out,
            state_db,
            expect_schema_version,
            require_model,
            error_out,
        } => {
            let result = validate(
                &input,
                &out,
                state_db.as_deref(),
                expect_schema_version.as_deref(),
                if require_model {
                    ModelRequirement::Required
                } else {
                    ModelRequirement::Optional
                },
            );
            // 出错时把错误码与中文说明落成 JSON：表格插件按字段读，不解析 stderr
            if let (Err(e), Some(path)) = (&result, error_out.as_deref()) {
                let _ = errors::write_error_report(path, e);
            }
            result?;
            Ok(0)
        }
        Command::Push {
            batch,
            require_model,
            preview,
            plan,
            out,
            state_db,
            error_out,
        } => {
            let result = if preview {
                push_preview(batch.as_deref(), state_db.as_deref())
            } else if plan {
                push_plan(batch.as_deref(), out.as_deref(), state_db.as_deref())
            } else {
                if require_model {
                    // 上报路径**不调用模型端点**（语义归并只在校验阶段做，
                    // 且只标注不推进状态）—— 说清楚，别让人以为这个开关生效了。
                    println!("注意：上报路径不调用模型端点，--require-model 对本命令无效。");
                }
                push_send(batch.as_deref(), out.as_deref(), state_db.as_deref())
            };
            // 出错时把错误码与中文说明落成 JSON：表格插件按字段读，不解析 stderr
            if let (Err(e), Some(path)) = (&result, error_out.as_deref()) {
                let _ = errors::write_error_report(path, e);
            }
            result.map(|_| 0)
        }
        Command::Status {
            batch,
            limit,
            state_db,
        } => {
            status(batch.as_deref(), limit, state_db.as_deref())?;
            Ok(0)
        }
        Command::Rules {
            dir,
            out,
            state_db,
            action,
        } => match action {
            Some(RulesAction::Approve {
                version,
                approver,
                from_version,
                slot,
                note,
                state_db: db,
            }) => {
                rules_approve(
                    &version,
                    &approver,
                    from_version.as_deref(),
                    slot.as_deref(),
                    note.as_deref(),
                    db.as_deref().or(state_db.as_deref()),
                )?;
                Ok(0)
            }
            Some(RulesAction::Approvals { state_db: db }) => {
                rules_approvals(db.as_deref().or(state_db.as_deref()))?;
                Ok(0)
            }
            None => {
                rules(dir.as_deref(), out.as_deref(), state_db.as_deref())?;
                Ok(0)
            }
        },
        Command::Merge {
            batch,
            row,
            drop_row,
            by,
            note,
            state_db,
        } => {
            merge_rows(
                &batch,
                &row,
                &drop_row,
                &by,
                note.as_deref(),
                state_db.as_deref(),
            )?;
            Ok(0)
        }
        Command::Confirm {
            row,
            by,
            with,
            batch,
            note,
            state_db,
        } => {
            confirm_row(
                &row,
                &by,
                with.as_deref(),
                batch.as_deref(),
                note.as_deref(),
                state_db.as_deref(),
            )?;
            Ok(0)
        }
        Command::Revoke {
            row,
            by,
            batch,
            note,
            state_db,
        } => {
            revoke_row(
                &row,
                &by,
                batch.as_deref(),
                note.as_deref(),
                state_db.as_deref(),
            )?;
            Ok(0)
        }
        Command::Confirmations { batch, state_db } => {
            list_confirmations(batch.as_deref(), state_db.as_deref())?;
            Ok(0)
        }
        Command::Export {
            what,
            out,
            batch,
            state_db,
            from_version,
            to_version,
            rule_dir,
            contract,
            reminder_out,
        } => match what {
            ExportWhat::Evidence => {
                export_evidence(&out, batch.as_deref(), state_db.as_deref())?;
                Ok(0)
            }
            ExportWhat::Pending => {
                export_pending(
                    &out,
                    reminder_out.as_deref(),
                    batch.as_deref(),
                    state_db.as_deref(),
                    rule_dir.as_deref(),
                )?;
                Ok(0)
            }
            ExportWhat::Diff => {
                export_diff(
                    &out,
                    from_version.as_deref(),
                    to_version.as_deref(),
                    rule_dir.as_deref(),
                    state_db.as_deref(),
                )?;
                Ok(0)
            }
            ExportWhat::Dupe => {
                export_dupe(&out, contract.as_deref())?;
                Ok(0)
            }
            ExportWhat::Receipts => {
                export_receipts(&out, batch.as_deref(), state_db.as_deref())?;
                Ok(0)
            }
        },
        Command::Replay {
            batch,
            rule_version,
        } => replay(&batch, &rule_version),
        Command::Doctor { state_db } => doctor(state_db.as_deref()),
        Command::Dept { state_db } => {
            dept_status(state_db.as_deref())?;
            Ok(0)
        }
        Command::Bench {
            bench_dir,
            out,
            threshold,
        } => {
            bench(&bench_dir, &out, threshold)?;
            Ok(0)
        }
        Command::Compare {
            bench_dir,
            input,
            out,
            threshold,
            rule_dir,
        } => {
            compare(&bench_dir, &input, &out, threshold, rule_dir.as_deref())?;
            Ok(0)
        }
        Command::Recover {
            state_db,
            out,
            apply,
        } => {
            recover(state_db.as_deref(), out.as_deref(), apply)?;
            Ok(0)
        }
        Command::Audit {
            row,
            batch,
            idempotency,
            state_db,
        } => {
            audit(
                row.as_deref(),
                batch.as_deref(),
                idempotency,
                state_db.as_deref(),
            )?;
            Ok(0)
        }
        Command::Tokens { out, vba } => {
            tokens_out(&out, vba)?;
            Ok(0)
        }
        Command::Messages { out, vba } => {
            messages_out(&out, vba)?;
            Ok(0)
        }
        Command::Macro { out, template } => {
            macro_out(&out, template.as_deref())?;
            Ok(0)
        }
    }
}

/// 装配 WPS JSA 宏。
///
/// 手写模板（`macro/wps/regval.template.js`）+ 引擎侧的契约常量、视觉 token、
/// 提示文案 → 一份可独立加载的宏文件。模板缺标记就报错，不静默少注入一块。
fn macro_out(out: &Path, template: Option<&Path>) -> Result<(), EngineError> {
    let template_path = template
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("macro/wps/regval.template.js"));
    let template_text = std::fs::read_to_string(&template_path).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("读不到宏模板 {}：{}", template_path.display(), e),
            template_path.display().to_string(),
        )
    })?;

    let assembled = ui::wps::assemble(&template_text).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("宏模板装配失败：{}", e),
            template_path.display().to_string(),
        )
    })?;

    std::fs::write(out, assembled)?;
    println!("WPS JSA 宏已写出：{}", out.display());
    println!(
        "  模板 {} · 契约版本 {} · 视觉 token {} · 文案 {}",
        template_path.display(),
        contract::SCHEMA_VERSION,
        ui::tokens::TOKENS_SCHEMA_VERSION,
        ui::messages::MESSAGES_SCHEMA_VERSION
    );
    println!("  改逻辑改模板，改样式改 src/ui/tokens.rs，改文案改 src/ui/messages.rs，然后重新生成。");
    Ok(())
}

/// 导出视觉 token。
///
/// 表格内插件的样式不写死在宏里 —— VBA 只能写死数值，所以由引擎把 token
/// 翻译成常量模块（`RegValTokens.bas`）。宏里只引用 `T_*` 与 `RegVal*ForRole`，
/// 色值与映射仍然只有一处出处：`src/ui/tokens.rs`。
fn tokens_out(out: &Path, vba: bool) -> Result<(), EngineError> {
    let is_vba = vba
        || out
            .extension()
            .map(|e| {
                let e = e.to_string_lossy().to_ascii_lowercase();
                e == "bas" || e == "vba"
            })
            .unwrap_or(false);

    if is_vba {
        std::fs::write(out, ui::tokens::render_vba_module())?;
        println!("视觉 token 已写出（宏常量模块）：{}", out.display());
        println!(
            "  token 版本 {} · 宏里只引用 T_* 常量与 RegVal*ForRole 函数；改样式请改 src/ui/tokens.rs 后重新生成。",
            ui::tokens::TOKENS_SCHEMA_VERSION
        );
    } else {
        let json = serde_json::to_string_pretty(&ui::tokens::manifest_json()).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("视觉 token JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?;
        std::fs::write(out, json)?;
        println!("视觉 token 已写出（JSON 清单）：{}", out.display());
        println!(
            "  token 版本 {} · 配色 {} 条 · 角色 {} 个 · 字体 {} 种 · 列 {} 列",
            ui::tokens::TOKENS_SCHEMA_VERSION,
            ui::tokens::PALETTE.len(),
            ui::tokens::ROLE_STYLES.len(),
            ui::tokens::FONTS.len(),
            ui::tokens::COLUMN_STYLES.len()
        );
    }
    Ok(())
}

/// 导出提示文案常量表。
///
/// 表格内插件面向用户的中文不写死在宏里 —— VBA 只能写死字符串，所以由引擎
/// 把文案表翻译成常量模块（`RegValMessages.bas`）。错误码的中文说明只有一处
/// 出处：`src/errors.rs` 的 `ErrorCode::zh()`，这里只做翻译，不重抄。
fn messages_out(out: &Path, vba: bool) -> Result<(), EngineError> {
    let is_vba = vba
        || out
            .extension()
            .map(|e| {
                let e = e.to_string_lossy().to_ascii_lowercase();
                e == "bas" || e == "vba"
            })
            .unwrap_or(false);

    if is_vba {
        std::fs::write(out, ui::messages::render_vba_module())?;
        println!("提示文案常量表已写出（宏常量模块）：{}", out.display());
        println!(
            "  文案版本 {} · 宏里只引用 REGVAL_MSG_* 与 RegValErrorText；改文案请改 src/ui/messages.rs 后重新生成。",
            ui::messages::MESSAGES_SCHEMA_VERSION
        );
    } else {
        let json = serde_json::to_string_pretty(&ui::messages::manifest_json()).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("提示文案 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?;
        std::fs::write(out, json)?;
        println!("提示文案常量表已写出（JSON 清单）：{}", out.display());
        println!(
            "  文案版本 {} · 提示文案 {} 条 · 错误码 {} 个",
            ui::messages::MESSAGES_SCHEMA_VERSION,
            ui::messages::MESSAGES.len(),
            errors::ErrorCode::ALL.len()
        );
    }
    Ok(())
}

/// 恢复损坏的状态库。
///
/// 库坏了以后最要紧的一件事是「报过没有」这条事实不能丢 —— 少了它，重跑就会把
/// 同一行再推一遍。恢复源是状态库旁边的恢复日志（append-only）与原库残留。
///
/// **原库不动**：恢复产物写在 `<状态库>.recovered`；要换上去得显式 `--apply`，
/// 换的时候原库先挪成 `.corrupt-N` 备份（不删）。
fn recover(
    state_db: Option<&Path>,
    out: Option<&Path>,
    apply: bool,
) -> Result<(), EngineError> {
    let db = resolve_state_db(state_db)?;
    let report = regval::db::recover::recover(&db)?;
    println!("{}", report.summary_line());
    for line in report.loss_lines() {
        println!("丢失 · {}", line);
    }
    for note in &report.notes {
        println!("说明 · {}", note);
    }

    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EngineError::at(
                        ErrorCode::Io,
                        format!("建目录失败：{}", e),
                        parent.display().to_string(),
                    )
                })?;
            }
        }
        std::fs::write(path, report.to_json()).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("写恢复报告失败：{}", e),
                path.display().to_string(),
            )
        })?;
        println!("恢复报告已写出 · {}", path.display());
    }

    if apply {
        match report.outcome {
            regval::db::recover::RecoveryOutcome::NotNeeded => {
                println!("状态库完好，无需替换（未做任何改动）");
            }
            regval::db::recover::RecoveryOutcome::Unrecoverable => {
                return Err(EngineError::at(
                    ErrorCode::DbCorrupted,
                    "没有恢复产物可换 —— 幂等键与回执记录都读不出来",
                    db.display().to_string(),
                ));
            }
            _ => {
                let backup = regval::db::recover::install(&db)?;
                println!("恢复库已换上去 · 原库备份为 {}", backup.display());
            }
        }
    }
    Ok(())
}

/// 回放审计链，并核对幂等键 ↔ 回执 ID 的一一对应。
///
/// - 给 `--row`：回放这一行的完整轨迹（谁确认的 → 什么时候推的 → 拿到哪个回执）；
/// - 给 `--batch`：回放这一批推了什么；
/// - 都不给或给 `--idempotency`：核对一一对应关系有没有破口。
fn audit(
    row: Option<&str>,
    batch: Option<&str>,
    idempotency: bool,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let db = resolve_state_db(state_db)?;
    let repo = Repo::open(&db)?;

    if idempotency || (row.is_none() && batch.is_none()) {
        let report = regval::state::audit::verify_idempotency_mapping(repo.conn())?;
        println!("{}", report.summary_line());
        for item in &report.orphan_receipts {
            println!("孤儿回执 · 有回执但查不到提交：{}", item);
        }
        for item in &report.missing_receipts {
            println!("缺回执 · 提交说已上报但查不到回执：{}", item);
        }
        for item in &report.duplicate_receipt_ids {
            println!("回执号重复 · {}", item);
        }
    }

    if let Some(row_uid) = row {
        let entries = regval::state::audit::replay_row(repo.conn(), row_uid)?;
        if entries.is_empty() {
            println!("审计链里没有这一行的记录 · {}", row_uid);
        }
        for entry in entries {
            println!("{}", entry.to_line());
        }
    }

    if let Some(batch_ref) = batch {
        let entries = regval::state::audit::replay_batch(repo.conn(), batch_ref)?;
        if entries.is_empty() {
            println!("审计链里没有这一批的记录 · {}", batch_ref);
        }
        for entry in entries {
            println!("{}", entry.to_line());
        }
    }
    Ok(())
}

/// 解析这一轮的科室主数据目录。
///
/// 三种情形，**都如实说**：
/// - 配了 HIS 目录 → 实时拉取，拉到就用它；
/// - 拉取失败（或根本没配）→ 退回上次缓存，并**标注数据时间**；
/// - 缓存也没有 / 缓存载荷坏了 → 标「科室主数据未校验」，本轮跳过科室维度检查。
///
/// **空目录绝不当作「目录可用但一个科室都没有」** —— 那会把每一行都报成
/// 「科室主数据不一致」，凭空造出一批假问题。
fn resolve_dept_directory(
    repo: &mut Repo,
    org_code: &str,
    batch_ref: Option<&str>,
) -> push::dept::DeptResolution {
    let env = config::ProcessEnv;
    let ttl_secs = config::optional_usize(&env, push::dept::DEPT_TTL_ENV)
        .ok()
        .flatten()
        .map(|v| v as u64)
        .unwrap_or(push::dept::DEFAULT_DEPT_TTL_SECS);
    let client = match push::dept::DeptDirectoryClient::from_env(&env) {
        Ok(c) => Some(c),
        Err(e) => {
            // 没配 HIS 目录不算错误 —— 科室维度本轮没法核对，如实标出来即可
            log::log(
                Level::Warn,
                "dept.directory_not_configured",
                batch_ref,
                Some(&e.code),
                &e.message,
                &[],
            );
            None
        }
    };
    let resolution = push::dept::resolve_directory(
        client.as_ref(),
        repo,
        org_code,
        &regval::db::repo::now_iso8601(),
        ttl_secs,
    );
    log::log(
        Level::Info,
        "dept.directory_resolved",
        batch_ref,
        None,
        &resolution.note,
        &[
            ("source", serde_json::json!(resolution.source.as_str())),
            ("checked", serde_json::json!(resolution.is_available())),
            ("entry_count", serde_json::json!(resolution.entry_count)),
            ("fetched_at", serde_json::json!(resolution.fetched_at)),
            ("stale", serde_json::json!(resolution.stale)),
            ("error_code", serde_json::json!(resolution.error_code)),
        ],
    );
    resolution
}

/// 本批的机构代码（取第一张非空的「机构代码」列；没有就空串 = 单机构口径）。
///
/// 目录缓存按机构代码存 —— 多院区时各院区的主数据互不串。
fn org_code_of(rows: &[ingest::SourceRow]) -> String {
    rows.iter()
        .filter_map(|r| r.get_trimmed(ingest::Field::OrgCode))
        .find(|c| !c.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `regval dept`：科室主数据目录状态。
///
/// 信息科先看这一条：**目录能不能拉到、这份目录是什么时候的、有多少科室**。
/// 拉不到时会说清是没配还是拉失败，以及有没有退到上次缓存。
fn dept_status(state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;
    let resolution = resolve_dept_directory(&mut repo, "", None);
    println!("科室主数据目录：{}", resolution.source.zh());
    if resolution.is_available() {
        println!("{}", resolution.note);
    } else {
        println!("{}", resolution.unavailable_note());
    }
    if let Some(dir) = resolution.directory.as_ref() {
        println!("目录条目：{} 条", dir.len());
        for entry in dir.entries().iter().take(10) {
            let aliases = if entry.aliases.is_empty() {
                String::new()
            } else {
                format!("（别名：{}）", entry.aliases.join("、"))
            };
            println!("  · {} {}{}", entry.dept_code, entry.dept_name, aliases);
        }
        if dir.len() > 10 {
            println!("  · …（其余 {} 条略）", dir.len() - 10);
        }
    }
    if !resolution.error_code.is_empty() {
        println!("错误码：{}", resolution.error_code);
    }
    Ok(())
}

/// 上报身份预览：只算「这一行上报时是什么身份」，**不发送**。
///
/// 一眼看三件事：幂等键（这条记录是谁）、载荷摘要（这份载荷有没有变）、
/// 修订号（改过没有 —— 摘要变了就 +1，摘要没变就是重复提交、不重复推送）。
///
/// 不联网、不写状态库：院内卫健上报网关的出站客户端由后续任务接入。
fn push_preview(batch: Option<&str>, state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;
    let batch_ref = match batch {
        Some(b) => b.to_string(),
        None => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::EmptyTable,
                "本机库里还没有校验过的批次，先跑一次 regval validate",
                state_db.display().to_string(),
            )
        })?,
    };

    let rows = repo.list_stored_rows(&batch_ref)?;
    if rows.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在本机库里没有行", batch_ref),
            format!("batch_ref={}", batch_ref),
        ));
    }

    // 同一个幂等键下已存在的提交：决定这次是「重复提交」「订正后重报」还是「重试上次没成功的那次」
    // —— 状态一起带上：上次是待重试的行不能当成已上报跳过。
    let mut existing: std::collections::BTreeMap<String, Vec<ExistingSubmission>> =
        std::collections::BTreeMap::new();
    for s in repo.list_submissions(&batch_ref)? {
        existing
            .entry(s.idempotency_key.clone())
            .or_default()
            .push(ExistingSubmission {
                revision: s.revision,
                payload_digest: s.payload_digest.clone(),
                status: s.status.clone(),
            });
    }

    // 指标级官方依据（口径种子里有逐项条款；没有种子文件就用整份文件的出处）
    let bases = indicator_bases();

    println!(
        "上报身份预览（只算不发）· 批次 {} · 行 {}",
        batch_ref,
        rows.len()
    );
    println!(
        "{} {} {} {} {} {}  {}",
        pad_display("指标编号", 14),
        pad_display("科室代码", 10),
        pad_display("业务主键", 14),
        pad_display("幂等键", 13),
        pad_display("载荷摘要", 13),
        pad_display("修订", 4),
        "权威状态"
    );
    let mut authoritative = 0usize;
    let mut duplicate = 0usize;
    let mut retry_rows = 0usize;
    for row in &rows {
        let (numerator, denominator, value) = push::program::numeric_fields(row);
        let basis = bases
            .get(row.indicator_code.trim())
            .cloned()
            .unwrap_or_else(|| indicators::BASIS.to_string());
        let payload = idempotency::SubmissionPayload::new(PayloadInput {
            batch_ref: &batch_ref,
            org_code: &row.org_code,
            reporting_period: &row.reporting_period,
            indicator_code: &row.indicator_code,
            dept_code: &row.dept_code,
            business_key: &row.business_key,
            numerator,
            denominator,
            value,
            unit: &row.unit,
            basis: &basis,
            rule_version: &row.rule_version,
        });
        let decision = idempotency::decide_revision(
            existing
                .get(&payload.idempotency_key)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &payload.payload_digest,
        );
        if row.authority_state == AUTHORITY_AUTHORITATIVE {
            authoritative += 1;
        }
        if decision.is_duplicate() {
            duplicate += 1;
        }
        if decision.is_retry() {
            // 上次没报上去（待重试）且载荷没变 → 这一行还要再推一次
            retry_rows += 1;
        }
        println!(
            "{} {} {} {} {} {}  {}",
            pad_display(&row.indicator_code, 14),
            pad_display(&row.dept_code, 10),
            pad_display(&row.business_key, 14),
            pad_display(&shorten(&payload.idempotency_key, 12), 13),
            pad_display(&shorten(&payload.payload_digest, 12), 13),
            pad_display(&decision.revision().to_string(), 4),
            row.authority_state
        );
    }
    println!(
        "预览汇总：{} 行 · 权威行 {} 行 · 非权威行 {} 行 · 已上报不重复推 {} 行 · 待重试可重推 {} 行",
        rows.len(),
        authoritative,
        rows.len() - authoritative,
        duplicate,
        retry_rows
    );
    println!("权威行才是可上报的行；口径问题未清、未人工确认的行由写闸门拦下。");
    println!(
        "提示：本轮只算上报身份，不发送 —— 网关出站客户端已就绪（push::client），\
         批量提交计划的编译用 regval push --plan。"
    );
    Ok(())
}

/// 上报提交计划：把这一批要上报的行编成**有条件的批量提交程序**并打印。
///
/// **只编译不发** —— 计划先给经办人核对（每步一行：行键 / 幂等键 / 载荷摘要 / 修订号），
/// 核对过之后才由执行器按条件链逐步推：第 i 步只在第 i-1 步成功后执行。
///
/// 幂等命中的行、跨表重复的行不进程序；同键异值的行**一行都不进**，进「待人工判定」。
fn push_plan(
    batch: Option<&str>,
    out: Option<&Path>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;
    let batch_ref = match batch {
        Some(b) => b.to_string(),
        None => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::EmptyTable,
                "本机库里还没有校验过的批次，先跑一次 regval validate",
                state_db.display().to_string(),
            )
        })?,
    };

    let rows = repo.list_stored_rows(&batch_ref)?;
    if rows.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在本机库里没有行", batch_ref),
            format!("batch_ref={}", batch_ref),
        ));
    }

    // 本机库里同一个幂等键下已存在的提交：决定这一行是重复提交（不重推）、订正后重报，
    // 还是重试上次没成功的那次（上次待重试且载荷没变）
    let mut existing: std::collections::BTreeMap<String, Vec<ExistingSubmission>> =
        std::collections::BTreeMap::new();
    for s in repo.list_submissions(&batch_ref)? {
        existing
            .entry(s.idempotency_key.clone())
            .or_default()
            .push(ExistingSubmission {
                revision: s.revision,
                payload_digest: s.payload_digest.clone(),
                status: s.status.clone(),
            });
    }

    let bases = indicator_bases();
    let candidates: Vec<push::program::PlanCandidate> = rows
        .iter()
        .map(|row| push::program::candidate_for_row(row, &batch_ref, &bases))
        .collect();

    let program = push::program::compile(&push::program::PlanInput {
        batch_ref: batch_ref.clone(),
        candidates,
        existing,
    });

    println!("上报提交计划（只编译不发）· 批次 {}", batch_ref);
    if program.is_empty() {
        println!(
            "待上报步骤 0 步 —— 本批没有可上报的行（原因见下方「已跳过」与「待人工判定」清单）"
        );
    }
    for line in program.plan_lines() {
        println!("{}", line);
    }
    for s in program.skipped() {
        println!(
            "已跳过  行键 {}  指标 {}  科室 {}  业务主键 {}  {}（{}）",
            s.row_uid,
            s.indicator_code,
            s.dept_code,
            s.business_key,
            s.reason.zh(),
            s.detail
        );
    }
    for p in program.pending_manual() {
        println!(
            "待人工判定  行键 {}  指标 {}  科室 {}  业务主键 {}  {}",
            p.row_uid, p.indicator_code, p.dept_code, p.business_key, p.detail
        );
    }
    println!(
        "计划汇总：待上报 {} 步 · 已跳过 {} 行 · 待人工判定 {} 行",
        program.len(),
        program.skipped().len(),
        program.pending_manual().len()
    );
    println!("条件链：第 i 步只在第 i-1 步成功后执行；计划核对无误后再执行，执行由后续任务接入。");

    if let Some(path) = out {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = match ext.as_str() {
            "csv" => program.to_csv(),
            "json" => program.to_json(),
            _ => {
                return Err(EngineError::at(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "提交计划产物只支持 .csv 或 .json 后缀，给的是 {:?}",
                        ext
                    ),
                    path.display().to_string(),
                ))
            }
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EngineError::at(
                        ErrorCode::Io,
                        format!("建目录失败：{}", e),
                        parent.display().to_string(),
                    )
                })?;
            }
        }
        std::fs::write(path, body).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("写提交计划失败：{}", e),
                path.display().to_string(),
            )
        })?;
        println!("提交计划已写出 · {}", path.display());
    }
    Ok(())
}

/// 上报发送：把这一批**已确认**的行按条件批量程序真的推给院内卫健上报网关。
///
/// 链路（每一步都是既有的模块，这一函数只负责把它们接起来）：
///
/// 1. 编译条件批量提交程序 —— 幂等命中的行不进程序，同键异值的行进「待人工判定」；
/// 2. 从本机库恢复台账 —— 上一轮拿到回执的行**不重复推送**（跨进程续跑）；
/// 3. 过写闸门 —— 没确认过的行**一个字节都不发**；
/// 4. 逐步执行 —— 第 i 步只在第 i-1 步成功后执行；5xx / 超时退避重试，4xx 不重试；
/// 5. 落库 —— 回执、提交记录、行状态、审计链、批次汇总。
///
/// **批次中断后重跑同一条命令即可续跑**：续跑点由本机库里的回执算出来，
/// 已上报的行一个请求都不再发。
fn push_send(batch: Option<&str>, out: Option<&Path>, state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;
    let batch_ref = resolve_batch(batch, &repo, &state_db)?;

    let rows = repo.list_stored_rows(&batch_ref)?;
    if rows.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在本机库里没有行", batch_ref),
            format!("batch_ref={}", batch_ref),
        ));
    }

    // 同一个幂等键下已存在的提交：决定这一行是重复提交（不重推）、订正后重报，
    // 还是重试上次没成功的那次（上次待重试且载荷没变）。
    let mut existing: std::collections::BTreeMap<String, Vec<ExistingSubmission>> =
        std::collections::BTreeMap::new();
    for s in repo.list_submissions(&batch_ref)? {
        existing
            .entry(s.idempotency_key.clone())
            .or_default()
            .push(ExistingSubmission {
                revision: s.revision,
                payload_digest: s.payload_digest.clone(),
                status: s.status.clone(),
            });
    }

    let bases = indicator_bases();
    let candidates: Vec<push::program::PlanCandidate> = rows
        .iter()
        .map(|row| push::program::candidate_for_row(row, &batch_ref, &bases))
        .collect();
    let program = push::program::compile(&push::program::PlanInput {
        batch_ref: batch_ref.clone(),
        candidates,
        existing,
    });

    println!("上报发送 · 批次 {}", batch_ref);
    println!(
        "提交计划：待上报 {} 步 · 已跳过 {} 行 · 待人工判定 {} 行",
        program.len(),
        program.skipped().len(),
        program.pending_manual().len()
    );
    for p in program.pending_manual() {
        println!(
            "待人工判定  行键 {}  指标 {}  科室 {}  业务主键 {}  {}",
            p.row_uid, p.indicator_code, p.dept_code, p.business_key, p.detail
        );
    }

    // 批前快照：批末回滚时汇总要回到这个值
    let before = repo.batch_summary(&batch_ref)?;

    if program.is_empty() {
        // 没有要推的行 —— 全部幂等命中或都进了「待人工判定」。
        // **不报错**：这就是「这一批已经报完了」的正常结局。
        println!("本批没有待上报的行 —— 已上报的行不重复推送（幂等命中）。");
        for s in program.skipped() {
            println!(
                "已跳过  行键 {}  指标 {}  科室 {}  业务主键 {}  {}（{}）",
                s.row_uid,
                s.indicator_code,
                s.dept_code,
                s.business_key,
                s.reason.zh(),
                s.detail
            );
        }
        return Ok(());
    }

    // 网关出站客户端：凭据只从环境变量读，缺项在发请求之前就报错
    let env = ProcessEnv;
    let mut client = push::client::GatewayClient::from_env(&env)?;
    let policy = push::retry::resolve_policy(&env)?;
    let mut sleeper = push::retry::RealSleeper;

    // 执行段：只读状态库（写闸门要查确认记录与行状态）
    let (outcome, blocked_rows) = {
        let mut gated = push::gate::GatedTransport::new(&mut client, repo.conn(), &batch_ref);
        let outcome = push::send::execute(
            &repo,
            &batch_ref,
            &program,
            &mut gated,
            &policy,
            &mut sleeper,
        )?;
        let blocked = gated.blocked().to_vec();
        (outcome, blocked)
    };

    // 落库段：拿可变借用，把回执、提交记录、行状态、审计链与批次汇总写进去
    let persisted = push::send::persist(&mut repo, &batch_ref, &outcome, before.as_ref())?;

    for line in outcome.report.summary_lines() {
        println!("{}", line);
    }
    println!("{}", persisted.rows.summary_line());
    println!("批次汇总：{}", persisted.totals.to_line());
    if outcome.resume_skipped() > 0 {
        println!(
            "断点续跑：{} 行此前已确认成功，本轮**未重复推送**（续跑点之后才继续）",
            outcome.resume_skipped()
        );
    }
    if !blocked_rows.is_empty() {
        println!(
            "写闸门拦下 {} 行：这些行还没人工确认，**一个字节都没往网关发** —— 确认后重跑本命令即可",
            blocked_rows.len()
        );
        for row_uid in &blocked_rows {
            println!("  · 行键 {}", row_uid);
        }
    }

    // 重试耗尽的行进待办清单（经办人手工重推时看这份）
    let todo = push::retry::retry_todo(&repo.list_pending_retry_rows(&batch_ref)?);
    if !todo.is_empty() {
        println!("待重试清单（可手工重推）：{} 行", todo.len());
        for item in &todo {
            println!(
                "  · 行键 {}  错误码 {}  已尝试 {} 次  幂等键 {}",
                item.row_uid, item.error_code, item.attempts, item.idempotency_key
            );
        }
    }

    // 执行报告（可核对产物）：逐步裁决、批前批后汇总、回执
    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    EngineError::at(
                        ErrorCode::Io,
                        format!("建目录失败：{}", e),
                        parent.display().to_string(),
                    )
                })?;
            }
        }
        std::fs::write(path, outcome.report.to_json()).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("写执行报告失败：{}", e),
                path.display().to_string(),
            )
        })?;
        println!("执行报告已写出 · {}", path.display());
    }

    log::log(
        Level::Info,
        "push.done",
        Some(&batch_ref),
        None,
        "",
        &[
            ("planned_steps", serde_json::json!(program.len())),
            ("attempted", serde_json::json!(outcome.attempted())),
            (
                "resume_skipped",
                serde_json::json!(outcome.resume_skipped()),
            ),
            ("accepted", serde_json::json!(persisted.rows.accepted)),
            ("rejected", serde_json::json!(persisted.rows.rejected)),
            ("retryable", serde_json::json!(persisted.rows.retryable)),
            ("blocked_by_gate", serde_json::json!(blocked_rows.len())),
        ],
    );
    Ok(())
}

/// 指标级官方依据：口径种子文件里有逐项条款，没有就用整份文件的出处。
fn indicator_bases() -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let cfg = PartialConfig::load(&ProcessEnv);
    let dir = match cfg.get("REGVAL_RULE_DIR") {
        Some(d) if !d.trim().is_empty() => PathBuf::from(d.trim()),
        _ => return map,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return map,
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(seed::SEED_FILE_SUFFIX))
        .collect();
    files.sort();
    for file in files {
        if let Ok(set) = seed::SeedSet::load(&file) {
            for ind in &set.indicators {
                if ind.has_basis() {
                    map.insert(ind.indicator_code.clone(), ind.basis.clone());
                }
            }
        }
    }
    map
}

/// 显示宽度：CJK 与全角字符按 2 列算，其余按 1 列。
fn display_width(text: &str) -> usize {
    text.chars().map(display_width_of).sum()
}

/// 截短（超出部分用 … 代替；摘要与幂等键在终端里看前 12 位就够认行）
fn shorten(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = display_width_of(c);
        if used + w > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// 一行的数据摘要：只拼业务字段，不含行号与时间。
///
/// 摘要变了 = 这一行被改过。状态机靠它判断「被退回的行有没有被修正过」。
fn row_data_digest(row: &ingest::SourceRow) -> String {
    let num = |field: ingest::Field| row.number(field).map(|v| v.to_string()).unwrap_or_default();
    state::machine::digest_of(&[
        row.get_trimmed(ingest::Field::IndicatorCode).unwrap_or(""),
        row.get_trimmed(ingest::Field::DeptCode).unwrap_or(""),
        row.get_trimmed(ingest::Field::ReportingPeriod).unwrap_or(""),
        row.get_trimmed(ingest::Field::BusinessKey).unwrap_or(""),
        &num(ingest::Field::Numerator),
        &num(ingest::Field::Denominator),
        &num(ingest::Field::Value),
        row.get_trimmed(ingest::Field::Unit).unwrap_or(""),
    ])
}

/// 校验完一批之后的行状态对账。
///
/// 结论只按**口径问题**算（上报退回留下的说明不算口径不合格）；
/// 状态推进只走状态机唯一入口，别处不直接改状态列。
fn reconcile_row_lifecycle(
    repo: &mut Repo,
    batch_ref: &str,
    rows: &[ingest::SourceRow],
    issues: &[regval::rules::model::Issue],
) -> Result<state::machine::ReconcileReport, EngineError> {
    let index = report::summary::IssueIndex::build(issues);
    let inputs: Vec<state::machine::RowValidationInput> = rows
        .iter()
        .map(|row| {
            let meta = row.meta();
            let conclusion = report::summary::conclusion_of(index.of_row(&meta.row_uid));
            state::machine::RowValidationInput {
                row_uid: meta.row_uid,
                source_file: meta.source_file,
                conclusion_ok: conclusion != report::summary::CONCLUSION_BLOCKED,
                data_digest: row_data_digest(row),
            }
        })
        .collect();
    state::machine::reconcile_after_validation(repo.conn_mut(), batch_ref, &inputs)
}

/// 主路径：读表 → 落库 → 跑规则 → 写契约。
fn validate(
    input: &PathBuf,
    out: &PathBuf,
    state_db: Option<&Path>,
    expect_schema_version: Option<&str>,
    requirement: ModelRequirement,
) -> Result<(), EngineError> {
    let started = Instant::now();
    let started_at = regval::db::repo::now_iso8601();

    // 版本闸门：插件侧要求的契约版本与引擎不一致时，直接拒绝写出契约文件
    if let Some(expected) = expect_schema_version {
        contract::check_schema_version(expected)?;
    }

    let state_db = resolve_state_db(state_db)?;
    let batch_ref = ingest::batch_ref(input);

    log::log(
        Level::Info,
        "validate.start",
        Some(&batch_ref),
        None,
        "",
        &[
            ("input", serde_json::json!(input.display().to_string())),
            (
                "state_db",
                serde_json::json!(state_db.display().to_string()),
            ),
        ],
    );

    // 1. 读表（Excel 一个工作表一张来源表）
    let tables = ingest::read_tables(input)?;

    // 2. 表头映射 + 落本机库（一次事务，整批不落库则回滚）
    //    状态库先开：换版闸门要读「已用过的口径版本」与「质控岗确认记录」，
    //    才知道这一轮该不该换口径。
    let mut repo = Repo::open(&state_db)?;
    //    规则集先装载：落库时要把口径版本记到行上（这批结论按哪套口径算的），
    //    举证包与后续复算都靠它。
    let rule_set = load_rule_set(&repo)?;
    // 一条口径都没生效 → 停下报错。带着「零条口径」继续校验，
    // 就可能把不合格的行判成「通过」。
    if rule_set.executable().is_empty() {
        return Err(loader::blocked_all_error(rule_set.blocked_switches()));
    }
    let mut rows = Vec::new();
    // 读表阶段的留痕：空行与完全无效行不参与校验，但计数与行号都留痕（不静默丢行）
    let mut skipped: Vec<ingest::SkippedRow> = Vec::new();
    let mut blank_rows = 0usize;
    let mut invalid_rows = 0usize;
    let mut source_rows = 0usize;
    let mut encoding = String::new();
    let mut delimiter = String::new();
    let mut format_note = String::new();
    for table in &tables {
        let mapped = ingest::map_table_with_report(table)?;
        rows.extend(mapped.rows);
        skipped.extend(mapped.skipped);
        blank_rows += mapped.blank_rows;
        invalid_rows += mapped.invalid_rows;
        source_rows += table.rows.len();
        if encoding.is_empty() {
            encoding = table.format.encoding.clone();
            delimiter = table.format.delimiter.clone();
            format_note = table.format.note.clone();
        }
    }

    // 3. 实体绑定层：行键 / 业务主键 / 跨表匹配键 + 同权威键歧义拒绝合并
    //    科室主数据目录走 HIS 出站读（push/dept.rs）：拉到了就核对科室代码与名称写法；
    //    拉不到就退回上次缓存（标注数据时间），缓存也没有就如实标「科室主数据未校验」，
    //    **不静默通过** —— 「没查」与「查了没问题」在结果里必须分得开。
    //    这一层是纯内存计算 —— 先算后落库，「模型必须可用」时本轮一行数据都不改。
    let dept_resolution = resolve_dept_directory(&mut repo, &org_code_of(&rows), Some(&batch_ref));
    let binding = match dept_resolution.directory.as_ref() {
        Some(dir) => bind::bind(&rows, dir),
        None => bind::bind(&rows, &bind::dept::UnavailableDirectory),
    };
    let cross_table_records = binding.cross_table_record_count();
    let dept_unmatched = binding.dept_unmatched.clone();

    // 3b. 重复检测四级漏斗：确定性键 → 归一化 → 编辑距离 → 语义归并。
    //     先跑确定性三级 —— 第四级要等模型答完才有候选，而「该拿哪几对去问模型」
    //     取决于前三级已经认下了哪些（已认下的证据更硬，不该拿去问）。
    //     漏斗编排层负责逐级累加（后一级只增不删）、记住每对行是哪一级判出来的，
    //     并给每一级留下可数的增量 —— 逐级召回率与精确率见 `regval bench`。
    //     全程只标疑似、不合并不推送 —— 合并与否由经办人逐条勾选确认（确认流见任务 36）。
    let distance_cfg = resolve_distance_config()?;
    let mut funnel = dupe::run_deterministic_levels(&rows, &distance_cfg);

    // 3c. 模型端点（语义复核）：能连就连，连不上就降级；要求「必须可用」时不许降级。
    //     这一步放在落库之前：降级只影响「模型结论有没有」，不影响主路径产出；
    //     而「模型必须可用」时不许降级，此时本轮一行数据都不写、契约也不出。
    let model_status = attempt_model_review(&mut repo, &rows, &funnel, &batch_ref, requirement)?;

    // 3d. 第四级（语义归并）并进漏斗：把模型结论翻译成候选对与行级标注。
    //     置信度不够的降级为「疑似待人工判定」，不进可合并候选；
    //     无论采纳与否，这一级都只标疑似 —— 合并由经办人确认后才做。
    let merge_cfg = dupe::merge::SemanticMergeConfig::from_env()?;
    let semantic_source = dupe::merge::ModelSemanticSource::build(
        &rows,
        &model_status.proposals,
        &merge_cfg,
        model_status.attempted && !model_status.degraded,
        semantic_level_note(&model_status, &merge_cfg),
    );
    let semantic_stats = dupe::push_semantic_level(&mut funnel, &rows, &semantic_source);

    // 4. 落库：行与权威归属（事务整批落库，中途失败不留半截状态）
    repo.insert_rows(&batch_ref, &rows)?;
    repo.set_batch_rule_version(&batch_ref, &rule_set.manifest().set_version)?;
    let applied = repo.apply_authority_states(&batch_ref, &binding.authority_assignments())?;

    // 4. 跑口径规则（参数化执行，规则 SQL 以命名参数绑定）
    //    规则集在落库前已装载（落库时要把口径版本记到行上）。
    //    装载走严格模式：缺依据、字段写错的规则直接报错，不静默少一条口径。
    // 语法不通过的规则不参与执行，但要让操作的人看见（不静默通过）
    for se in &rule_set.manifest().syntax_errors {
        log::log(
            Level::Warn,
            "validate.rule_syntax_error",
            Some(&batch_ref),
            Some(&ErrorCode::RuleSyntaxError),
            &se.message,
            &[
                ("rule_id", serde_json::json!(se.rule_id)),
                ("rule_name", serde_json::json!(se.rule_name)),
                ("source_file", serde_json::json!(se.source_file)),
                ("token_type", serde_json::json!(se.token_type)),
                ("found", serde_json::json!(se.found)),
                ("offset", serde_json::json!(se.offset)),
            ],
        );
    }
    let mut issues = binding.issues;
    // 规则逐条执行：一条跑不起来（缺列不生效 / 执行失败）不影响其余规则，
    // 但失败与跳过都要留下痕迹 —— 少一条口径就可能把不合格的行判成「通过」。
    let rule_report = executor::execute_rules(repo.conn(), &rule_set.executable(), &batch_ref);
    issues.extend(rule_report.issues());
    for exec in rule_report.executions.iter() {
        if exec.outcome == executor::RuleOutcome::Executed {
            continue;
        }
        log::log(
            Level::Warn,
            "validate.rule_not_executed",
            Some(&batch_ref),
            exec.error_code.as_ref(),
            &exec.message,
            &[
                ("rule_id", serde_json::json!(exec.rule_id)),
                ("rule_name", serde_json::json!(exec.rule_name)),
                ("outcome", serde_json::json!(exec.outcome.as_str())),
                ("location", serde_json::json!(exec.location)),
            ],
        );
    }
    repo.insert_issues(&batch_ref, &issues)?;

    // 4b. 行级状态机：把本轮校验结论落到行状态上（推进只走状态机唯一入口）。
    //     结论只按**口径问题**算 —— 上报退回留下的 R-PUSH- 问题不算「口径不合格」，
    //     否则被退回的行会被误判成没通过校验，从待确认退回待校验。
    //     已上报的行普通校验不改回；被退回的行修正后（数据摘要变了）回到待确认。
    let state_report = reconcile_row_lifecycle(&mut repo, &batch_ref, &rows, &issues)?;

    // 上报退回留下的说明也要进本轮契约：那一行还没报上去，退回原因要一直看得见
    // （重跑校验不抹掉它 —— 抹掉就等于把「为什么被退回」忘了）
    issues.extend(repo.list_push_issues(&batch_ref)?);

    // 5. 组装结果契约并写出
    let metas: Vec<ingest::RowMeta> = rows.iter().map(|r| r.meta()).collect();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    // 「疑似重复项」列可能有多个层级的标注落在同一行（确定性键说「取值完全一致」、
    // 归一化说「写法不同但归一后一致」）—— 漏斗编排层已把各级的说法合成一句，
    // 不叠成两句让人以为两件事
    let mut suspected_texts: std::collections::BTreeMap<String, String> =
        binding.suspected.into_iter().collect();
    // 漏斗的行级标注覆盖绑定层的同类标注（漏斗那一层写得更细，带对方行键）
    for (row_key, text) in &funnel.suspected {
        suspected_texts.insert(row_key.clone(), text.clone());
    }
    // 候选对取漏斗的累加结果：逐级只增不删，每对行记住是哪一级判出来的
    let duplicates = funnel.cumulative.clone();
    // 「上报状态」列只由网关回执驱动：库里已上报 / 被退回 / 待重试的行按回执取值，
    // 没上报过的行才是「未上报」。模型与本地时钟都不推进这一列。
    let push_states = repo.list_row_push_states(&batch_ref)?;
    let mut submission_status: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for state in &push_states {
        submission_status.insert(state.row_uid.clone(), state.status.clone());
    }
    let extra = contract::Extra {
        // 漏斗的标注更细（带对方行键），覆盖绑定层的同类标注
        suspected: suspected_texts,
        duplicates: duplicates,
        dept_master_checked: binding.dept_master_checked,
        dept_master_note: if binding.dept_master_checked {
            String::new()
        } else {
            "科室主数据未校验".to_string()
        },
        dupe_distance: dupe::distance::status_section_from(
            &funnel.stats.distance,
            &funnel.stats.distance_histogram,
            funnel.stats.distance.pairs,
            &distance_cfg,
        ),
        // 第四级（模型语义归并）的状态与计数：这一轮模型掺和了没有、掺和的够不够
        dupe_semantic: dupe::merge::SemanticStatusSection::new(
            semantic_source.is_connected(),
            semantic_source.describe(),
            &merge_cfg,
            semantic_stats.clone(),
        ),
        // 上报状态：回执说了算
        submission_status,
        // 读表记录：跳过了几行、为什么跳、这张表是什么编码与分隔符
        ingest: ingest::IngestSection::build(
            source_rows,
            rows.len(),
            blank_rows,
            invalid_rows,
            skipped,
            &encoding,
            &delimiter,
            &format_note,
        ),
    };
    let mut result = contract::build(&batch_ref, &metas, issues, tables.len(), elapsed_ms, &extra);
    // 规则清单进契约：这批结论是按哪套口径算的，有据可查
    result.rule_status = rule_set.status_section();
    // 科室主数据目录状态进契约：这批结论的科室维度查没查过、用的是哪份主数据
    result.dept_directory = DeptDirectorySection::from(&dept_resolution);
    // 模型端点状态进契约：这一批的疑似项里模型掺和了没有、有没有降级，有据可查
    result.model = model_status.clone();
    // 本轮没跑起来的规则也进契约：失败规则计数与明细可见，不静默通过
    result.rule_status.manifest.execution_failed_count = rule_report.failed_rules();
    result.rule_status.manifest.execution_skipped_count = rule_report.skipped_rules();
    result.rule_status.manifest.execution_failures = rule_report.failure_entries();
    // 写盘前再过一道版本闸门：契约版本必须与引擎常量一致，否则拒绝写出
    contract::check_schema_version(&result.schema_version)?;
    let json = serde_json::to_string_pretty(&result).map_err(|e| {
        EngineError::at(
            ErrorCode::ContractWrite,
            format!("结果契约序列化失败：{}", e),
            out.display().to_string(),
        )
    })?;
    std::fs::write(out, json)?;

    // 6. 运行摘要落状态库（regval status 可见）
    let summary = RunSummary {
        command: "validate".to_string(),
        batch_ref: batch_ref.clone(),
        tables: result.summary.tables,
        total_rows: result.summary.total_rows,
        rows_with_issues: result.summary.rows_with_issues,
        issue_count: result.summary.issue_count,
        elapsed_ms: result.summary.elapsed_ms,
        started_at,
        finished_at: regval::db::repo::now_iso8601(),
    };
    repo.insert_run_summary(&summary)?;
    // 记下「这一版口径实际用过了」：换版闸门靠它判断下一次出现更高版本时
    // 算不算换版（用过的版本再出现不算，没用过的才算）。
    repo.record_applied_rule_set(&rule_set.manifest().set_version)?;
    // 批次汇总：口径问题重算了，汇总也得跟着更新（一次事务写入；
    // 中途失败不留半截状态 —— 快照回到批前值）。
    let batch_totals = push::batch::refresh(
        &mut repo,
        &batch_ref,
        push::batch::BATCH_STATUS_VALIDATED,
        &rule_set.manifest().set_version,
    )?;
    println!("批次汇总：{}", push::batch::totals_of(&batch_totals).to_line());

    log::log(
        Level::Info,
        "validate.done",
        Some(&batch_ref),
        None,
        "",
        &summary.log_fields(),
    );
    log::log(
        Level::Info,
        "validate.binding",
        Some(&batch_ref),
        None,
        "",
        &[
            ("authority_assigned", serde_json::json!(applied)),
            (
                "suspected_duplicates",
                serde_json::json!(extra.suspected.len()),
            ),
            (
                "suspected_duplicate_pairs",
                serde_json::json!(funnel.stats.exact.pairs),
            ),
            (
                "dupe_exact_stats",
                serde_json::json!({
                    "groups": funnel.stats.exact.groups,
                    "duplicate_groups": funnel.stats.exact.duplicate_groups,
                    "conflicting_groups": funnel.stats.exact.conflicting_groups,
                    "conflicting_rows": funnel.stats.exact.conflicting_rows,
                    "skipped_rows": funnel.stats.exact.skipped_rows,
                    "collapsed_pairs": funnel.stats.exact.collapsed_pairs,
                }),
            ),
            (
                "dupe_normalize_stats",
                serde_json::json!({
                    "match_key_groups": funnel.stats.normalize.match_key_groups,
                    "buckets": funnel.stats.normalize.buckets,
                    "pairs": funnel.stats.normalize.pairs,
                    "eligible_rows": funnel.stats.normalize.eligible_rows,
                    "skipped_rows": funnel.stats.normalize.skipped_rows,
                    "unnormalized_rows": funnel.stats.normalize.unnormalized_rows,
                    "unnormalized_name_rows": funnel.stats.normalize.unnormalized_name_rows,
                    "unnormalized_unit_rows": funnel.stats.normalize.unnormalized_unit_rows,
                    "exact_overlap_pairs": funnel.stats.normalize.exact_overlap_pairs,
                    "value_conflict_pairs": funnel.stats.normalize.value_conflict_pairs,
                    "collapsed_pairs": funnel.stats.normalize.collapsed_pairs,
                }),
            ),
            (
                "dupe_distance_stats",
                serde_json::json!({
                    "threshold": distance_cfg.threshold,
                    "max_length_gap": distance_cfg.max_length_gap,
                    "pairs": funnel.stats.distance.pairs,
                    "compared_pairs": funnel.stats.distance.compared_pairs,
                    "beyond_threshold_pairs": funnel.stats.distance.beyond_threshold_pairs,
                    "skipped_long_gap_pairs": funnel.stats.distance.skipped_long_gap_pairs,
                    "skipped_oversized_pairs": funnel.stats.distance.skipped_oversized_pairs,
                    "exact_overlap_pairs": funnel.stats.distance.exact_overlap_pairs,
                    "normalize_overlap_pairs": funnel.stats.distance.normalize_overlap_pairs,
                    "value_conflict_pairs": funnel.stats.distance.value_conflict_pairs,
                    "eligible_rows": funnel.stats.distance.eligible_rows,
                    "skipped_rows": funnel.stats.distance.skipped_rows,
                    "skipped_empty_name_rows": funnel.stats.distance.skipped_empty_name_rows,
                    "histogram": funnel.stats.distance_histogram,
                }),
            ),
            (
                "cross_table_records",
                serde_json::json!(cross_table_records),
            ),
            (
                "dept_master_checked",
                serde_json::json!(extra.dept_master_checked),
            ),
            ("dept_unmatched", serde_json::json!(dept_unmatched)),
        ],
    );

    // 这一行与表格插件状态条上是同一句话（同一份 status_line，不各说一套）
    println!(
        "{} → {}",
        result.summary.status_line,
        out.display()
    );
    println!("涉及 {} 行有问题", result.summary.rows_with_issues);
    // 跳过的行照实说：表里 200 行、校验了 197 行，另外 3 行去哪了必须说得清
    // （不静默丢行 —— 差得不明不白时，经办人只能回原表数一遍）
    if result.summary.skipped_rows > 0 {
        println!("{}", result.summary.skipped_note);
        for row in result.ingest.skipped_detail.iter().take(10) {
            println!("  · 第 {} 行 · {}", row.row_no, row.reason);
        }
        if result.ingest.skipped_detail.len() > 10 {
            println!(
                "  · …（其余 {} 行见契约 ingest 段）",
                result.ingest.skipped_detail.len() - 10
            );
        }
    }
    // 读表时做过格式归一（GBK / 全角逗号）就说一句 —— 经办人看到乱码会以为引擎读错了
    if result.ingest.normalized() {
        println!(
            "读表格式：编码 {} · 分隔符 {}",
            result.ingest.encoding, result.ingest.delimiter
        );
    }
    // 行级状态机：这一批的行现在到哪一步了。已上报的行不会被普通校验改回，
    // 被退回的行修正后回到待确认（需重新确认）。
    println!("{}", state_report.summary_line());
    if !state_report.by_state.is_empty() {
        let dist: Vec<String> = state_report
            .by_state
            .iter()
            .map(|(state, count)| format!("{} {} 行", state, count))
            .collect();
        println!("行状态分布：{}", dist.join(" · "));
    }
    // 漏斗逐级计数：哪一级抓到了多少，一行一行看得见（不静默截断）
    println!(
        "重复检测漏斗：确定性键 {} 对 · 归一化候选 {} 对（未归一 {} 行 · 与确定性键重叠 {} 对 · 数值不同未收 {} 对）· 编辑距离候选 {} 对（阈值 {} · 超出阈值 {} 对 · 长度差过大跳过 {} 对）",
        funnel.stats.exact.pairs,
        funnel.stats.normalize.pairs,
        funnel.stats.normalize.unnormalized_rows,
        funnel.stats.normalize.exact_overlap_pairs,
        funnel.stats.normalize.value_conflict_pairs,
        funnel.stats.distance.pairs,
        distance_cfg.threshold,
        funnel.stats.distance.beyond_threshold_pairs,
        funnel.stats.distance.skipped_long_gap_pairs
    );
    // 距离分布直方：距离落在哪一档的行对最多（阈值调松调紧的效果一眼看得见）
    let histogram_text: Vec<String> = funnel
        .stats
        .distance_histogram
        .iter()
        .filter(|b| b.pairs > 0)
        .map(|b| format!("{} {} 对", b.label, b.pairs))
        .collect();
    if !histogram_text.is_empty() {
        println!("距离分布：{}", histogram_text.join(" · "));
    }
    println!(
        "口径规则集：版本 {} · 生效 {} 条 · 指纹 {}",
        rule_set.manifest().set_version,
        rule_set.manifest().active,
        rule_set.manifest().set_fingerprint
    );
    // 模型端点这一轮的状态：接了没有、成没成、降级没有 —— 一行说清，
    // 免得读的人以为「疑似项都过了模型那一关」。
    println!("{}", model_status.summary_line());
    // 第四级（语义归并）这一轮的计数：模型提了几条、几条够置信度、几条降级
    println!("{}", semantic_stats.summary_line());
    if model_status.degraded {
        println!(
            "注意：模型端点本轮不可用（{}）—— 疑似重复只来自确定性三级（精确键 / 归一化 / 编辑距离），模型结论缺失；调用已记入模型调用审计表",
            model_status.error_code
        );
    }
    // 换版闸门：新版本没过闸门时不静默 —— 逐条说明哪一版没生效、为什么、怎么补
    print_blocked_switches(&rule_set);
    println!(
        "批次 {} · 本机库 {} · 耗时 {} ms",
        result.batch_ref,
        state_db.display(),
        result.summary.elapsed_ms
    );
    if !extra.dept_master_checked {
        // 目录没核对上：说清是没配、拉不到，还是用了旧缓存 —— 不静默通过
        println!("{}", dept_resolution.unavailable_note());
    } else {
        println!("{}", dept_resolution.note);
    }
    if !binding.dept_name_mismatched.is_empty() {
        println!(
            "注意：{} 个科室的写法与主数据/别名不符（只提示，不改数）：{}",
            binding.dept_name_mismatched.len(),
            binding.dept_name_mismatched.join("、")
        );
    }
    // 没跑起来的规则要点名说清楚：口径少一条就可能把不合格的行判成「通过」
    if rule_report.not_run_rules() > 0 {
        println!(
            "注意：{} 条规则本轮未生效（缺列 {} 条 · 执行失败 {} 条）—— 明细见结果契约的 rule_status 段",
            rule_report.not_run_rules(),
            rule_report.skipped_rules(),
            rule_report.failed_rules()
        );
        for exec in rule_report.executions.iter() {
            if exec.outcome != executor::RuleOutcome::Executed {
                println!(
                    "  · {} {}：{}",
                    exec.rule_id,
                    exec.outcome.as_str(),
                    exec.message
                );
            }
        }
    }
    Ok(())
}

/// 模型端点：语义复核 + 降级处置 + 调用留痕。
///
/// 返回本轮模型状态段（进结果契约的 `model` 段）。四种情形：
///
/// - **没配端点** → 「本轮未接入」，疑似项来源保持确定性；
/// - **配了但没有待复核的候选** → 不去打扰端点（省一次调用，也省一次超时等待）；
/// - **配了但连不上 / 超时 / 响应不合法** → **降级**：只输出确定性候选，
///   疑似项来源标 `deterministic`，本次调用照实记入审计表；
/// - **要求「模型必须可用」**（`--require-model`）→ 降级即以 `MODEL_UNAVAILABLE`
///   退出，**不产出结果契约、不改动业务状态**。
///
/// 审计记录**在降级判定之前写**：模型挂了这件事本身就是要查的事，
/// 「没调成」不能等于「没发生过」。审计表里只有元数据与计数，
/// 没有任何一行业务内容。
fn attempt_model_review(
    repo: &mut Repo,
    rows: &[ingest::SourceRow],
    funnel: &dupe::FunnelOutcome,
    batch_ref: &str,
    requirement: ModelRequirement,
) -> Result<model::ModelStatusSection, EngineError> {
    let env = ProcessEnv;
    let partial = PartialConfig::load(&env);
    let missing = partial.missing(&model::openai::MODEL_ENV_VARS);
    if !missing.is_empty() {
        // 没配端点：默认「本轮不调用」；要求「必须可用」时不许悄悄跳过
        if requirement == ModelRequirement::Required {
            let names: Vec<&str> = missing.iter().map(|s| s.name).collect();
            return Err(EngineError::at(
                ErrorCode::ModelUnavailable,
                format!(
                    "已要求「模型必须可用」，但模型端点未配置（缺少 {}）—— 本轮不产出结果契约、不改动业务状态",
                    names.join("、")
                ),
                names.first().copied().unwrap_or(""),
            ));
        }
        return Ok(model::ModelStatusSection::not_attempted());
    }

    let config = model::ModelConfig::load(&env)?;
    let endpoint = config.base_url.clone();
    let requested_model = config.model.clone();
    let gateway = model::OpenAiModelGateway::new(config)?;

    // 问哪些行对：同一个跨表匹配键下、**前三级没认下**的行对
    // （前三级认下的证据更硬，不该拿去问模型）。
    // 候选集自带上限与截断计数 —— 截了多少写进审计与日志，不静默截断。
    let limits = dupe::candidates::limits_from_env()?;
    let candidates = dupe::candidates::residual_candidates(rows, &funnel.cumulative, &limits);
    if candidates.pairs.is_empty() {
        // 没有候选可送 —— 不去打扰端点，状态照实报「未接入」
        return Ok(model::ModelStatusSection::not_attempted());
    }
    log::log(
        Level::Info,
        "validate.model_review",
        Some(batch_ref),
        None,
        "",
        &[
            (
                "candidate_pairs",
                serde_json::json!(candidates.pair_count()),
            ),
            ("groups", serde_json::json!(candidates.groups)),
            (
                "excluded_claimed",
                serde_json::json!(candidates.excluded_claimed),
            ),
            (
                "skipped_large_groups",
                serde_json::json!(candidates.skipped_large_groups),
            ),
            (
                "skipped_over_limit",
                serde_json::json!(candidates.skipped_over_limit),
            ),
            ("endpoint", serde_json::json!(endpoint)),
        ],
    );

    let ctx = model::CallContext::new(batch_ref, endpoint, requested_model);
    let request = model::MergeRequest::new(candidates.pairs.clone());
    let outcome = model::run_merge_call(&gateway, &request, &ctx);
    // 留痕：不管成没成都记一条
    repo.record(outcome.audit())?;
    // 显式失败而非静默降级：要求「模型必须可用」时直接报错退出
    model::enforce_requirement(&outcome, requirement)?;
    let status = model::ModelStatusSection::from_outcome(&outcome);
    log::log(
        if status.degraded {
            Level::Warn
        } else {
            Level::Info
        },
        "validate.model_outcome",
        Some(batch_ref),
        if status.degraded {
            Some(&ErrorCode::ModelUnavailable)
        } else {
            None
        },
        &status.error_message,
        &[
            ("degraded", serde_json::json!(status.degraded)),
            ("error_code", serde_json::json!(status.error_code)),
            (
                "candidate_source",
                serde_json::json!(status.candidate_source),
            ),
            ("accepted_pairs", serde_json::json!(status.accepted_pairs)),
            ("duration_ms", serde_json::json!(status.duration_ms)),
        ],
    );
    Ok(status)
}

/// 记一条人工确认：疑似重复「确实是同一件事」。
///
/// **模型给的结论只是标注** —— 这一步是人把它变成决定的唯一入口。
/// 确认记录追加写进 `confirmations` 表（确认人 / 确认时间 / 行键 / 备注都留痕），
/// 撤销时追加一条 `revoke`（原记录留着：确认过又撤销过，本身就是要查的事）。
///
/// 这一步**不合并、不上报** —— 合并由 `regval merge`、上报由 `regval push` 接手，
/// 两者都以「已确认」为前提。模型永远不推进任何业务状态。
fn confirm_row(
    row: &str,
    by: &str,
    with: Option<&str>,
    batch: Option<&str>,
    note: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;
    let batch_ref = resolve_batch(batch, &repo, &state_db)?;
    // 确认一对行时，把对方行键写进备注：确认记录要能看出「确认的是哪一对」
    let mut req = state::confirm::ConfirmRequest::new(&batch_ref, row, by);
    if let Some(note) = note {
        req = req.with_note(note);
    }
    if let Some(other) = with {
        req = req.with_peer(other);
    }
    let outcome = state::confirm::confirm(repo.conn_mut(), &req)?;
    println!(
        "已记人工确认 · 批次 {} · 行 {} · 确认人 {} · 确认时间 {}",
        outcome.batch_ref, outcome.row_uid, outcome.actor, outcome.at
    );
    println!("行状态：{}（已排入待上报队列）", outcome.state_after);
    if outcome.state_after == regval::state::machine::RowState::PendingConfirmation {
        println!("下一步：只有已确认的行能上报（regval push）—— 未确认的行会被写闸门拦下（UNCONFIRMED_ROW）");
    } else {
        println!(
            "注意：这一行口径校验还没通过，确认只留痕、**不能上报** —— 先按「问题说明（含依据）」列改数据，再重新校验并确认一次"
        );
    }
    Ok(())
}

/// 撤销一条人工确认：这一行退回「校验通过」，需重新确认才能再报。
fn revoke_row(
    row: &str,
    by: &str,
    batch: Option<&str>,
    note: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;
    let batch_ref = resolve_batch(batch, &repo, &state_db)?;
    let mut req = state::confirm::RevokeRequest::new(&batch_ref, row, by);
    if let Some(note) = note {
        req = req.with_note(note);
    }
    let outcome = state::confirm::revoke(repo.conn_mut(), &req)?;
    println!(
        "已撤销人工确认 · 批次 {} · 行 {} · 撤销人 {} · 撤销时间 {}",
        outcome.batch_ref, outcome.row_uid, outcome.actor, outcome.at
    );
    println!(
        "行状态：{}（退回待重新确认；再上报前需重新确认）",
        outcome.state_after
    );
    Ok(())
}

/// 应用一条已勾选的疑似重复合并：并进来的那一行不再单独上报。
///
/// 模型的语义结论只是标注 —— 走到这一步的是**人勾选的那一对**。
/// 合并动作本身也记一条确认记录，事后查得到「谁合的、什么时候合的、留了哪一行」。
fn merge_rows(
    batch: &str,
    keep_row: &str,
    drop_row: &str,
    by: &str,
    note: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;
    let mut req = state::confirm::MergeRequest::new(batch, keep_row, drop_row, by);
    if let Some(note) = note {
        req = req.with_note(note);
    }
    let outcome = state::confirm::merge_confirmed_pair(repo.conn_mut(), &req)?;
    println!(
        "已应用疑似重复合并 · 批次 {} · 保留行 {} · 并入行 {} · 确认人 {} · 合并时间 {}",
        outcome.batch_ref, keep_row.trim(), outcome.row_uid, outcome.actor, outcome.at
    );
    println!("并入的那一行不再单独上报（同一笔记录只报一次）；原始数据两行都留着，可回溯。");
    Ok(())
}

/// 批次：显式给的优先，否则取库里最近一次校验的批次。
fn resolve_batch(
    batch: Option<&str>,
    repo: &Repo,
    state_db: &Path,
) -> Result<String, EngineError> {
    match batch {
        Some(b) if !b.trim().is_empty() => Ok(b.trim().to_string()),
        _ => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::ConfigInvalid,
                "本机库里还没有任何校验批次 —— 先跑一次 regval validate 再确认",
                state_db.display().to_string(),
            )
        }),
    }
}

/// 查看人工确认记录（谁在什么时候确认了哪一行）
fn list_confirmations(batch: Option<&str>, state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;
    let records = repo.list_confirmations(batch)?;
    println!("状态库：{}", state_db.display());
    if records.is_empty() {
        println!(
            "还没有人工确认记录{}",
            match batch {
                Some(b) => format!("（批次 {}）", b),
                None => String::new(),
            }
        );
        return Ok(());
    }
    for c in &records {
        // 撤销是追加写：原确认记录留着，只标出撤销人与撤销时间。
        // 「已确认 / 已撤销」按记录本身判，不按后一条记录反推 ——
        // 否则撤销过又重新确认的行会显示成「已撤销」，与实际状态对不上。
        let state = if c.action == "revoke" {
            "已撤销"
        } else if c.revoked_at.trim().is_empty() {
            "已确认"
        } else {
            "已撤销"
        };
        println!(
            "{} · 批次 {} · 行 {} · 动作 {} · 确认人 {} · 确认时间 {}{}{}",
            state,
            c.batch_ref,
            c.row_uid,
            c.action,
            c.confirmed_by,
            c.confirmed_at,
            if c.revoked_at.trim().is_empty() {
                String::new()
            } else {
                format!(" · 撤销人 {} · 撤销时间 {}", c.revoked_by, c.revoked_at)
            },
            if c.note.trim().is_empty() {
                String::new()
            } else {
                format!(" · 备注：{}", c.note)
            }
        );
    }
    Ok(())
}

/// 第四级（语义归并）这一轮的状态说明（人话，进漏斗说明与终端输出）。
///
/// 三种情形照实说：**没接端点**、**端点不可用（已降级）**、**端点可用**。
/// 绝不把「没查」说成「查了没查到」。
fn semantic_level_note(
    status: &model::ModelStatusSection,
    cfg: &dupe::merge::SemanticMergeConfig,
) -> String {
    if !status.attempted {
        return "本轮未接入模型端点（语义归并的模型调用未发起），第四级候选为空".to_string();
    }
    if status.degraded {
        return format!(
            "模型端点不可用（{}）—— 本轮只输出确定性候选，语义归并这一级没有结论",
            status.error_code
        );
    }
    format!(
        "模型判语义等价（置信度阈值 {:.2} · {}）",
        cfg.min_confidence,
        if status.candidate_pairs > 0 {
            format!("送审候选 {} 对", status.candidate_pairs)
        } else {
            "本轮没有候选对".to_string()
        }
    )
}

/// 导出口径差异清单：两版口径到底哪几条不一样、前后取值是什么。
///
/// 换版前给质控岗与科室看的东西。默认对比「状态库里实际用过的版本」
/// 与「规则目录里版本最高的一版」，也可以用 `--from-version` / `--to-version` 指定。
fn export_diff(
    out: &Path,
    from_version: Option<&str>,
    to_version: Option<&str>,
    rule_dir: Option<&Path>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let cfg = PartialConfig::load(&ProcessEnv);
    let dir = rule_dir
        .map(|p| p.to_path_buf())
        .or_else(|| cfg.get("REGVAL_RULE_DIR").map(PathBuf::from));

    // 规则集不带换版闸门：差异清单要把**两版都列出来**（被闸门拦下的新版本也要能对比）
    let set = match &dir {
        Some(d) if d.is_dir() => loader::load_reporting(d)?,
        Some(d) => {
            return Err(EngineError::at(
                ErrorCode::RuleLoadFailed,
                format!("规则目录不存在或不是目录：{}", d.display()),
                d.display().to_string(),
            ))
        }
        None => loader::builtin_set(),
    };

    // 旧版本：缺省取状态库里实际用过的版本（最近一个）
    let from = match from_version {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            let repo = resolve_state_db_optional(state_db)?;
            match repo {
                Some(path) => Repo::open(&path)?
                    .applied_rule_versions()?
                    .last()
                    .cloned()
                    .unwrap_or_default(),
                None => String::new(),
            }
        }
    };
    if from.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleVersionNotFound,
            "没有可对比的旧版本：用 --from-version 指定，或先跑一次 regval validate 让状态库记住在用的口径版本",
            "--from-version",
        ));
    }

    // 新版本：缺省取规则目录里版本最高的那一版
    let to = match to_version {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => set.manifest().set_version.clone(),
    };
    if to.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleVersionNotFound,
            "规则集里没有可用作对比的新版本口径规则",
            "--to-version",
        ));
    }

    let old_rules = set.by_version(&from);
    if old_rules.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleVersionNotFound,
            format!(
                "规则集里没有版本 {} 的口径规则，无法作为对比基准（旧版本规则表要留在目录里）",
                from
            ),
            format!("version={}", from),
        ));
    }
    let new_rules = set.by_version(&to);
    if new_rules.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleVersionNotFound,
            format!("规则集里没有版本 {} 的口径规则", to),
            format!("version={}", to),
        ));
    }

    let diff = version::diff_rule_sets(&old_rules, &new_rules, &from, &to);

    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        report::diff::to_csv(&diff).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("口径差异清单 CSV 生成失败：{}", e),
                out.display().to_string(),
            )
        })?
    } else {
        report::diff::to_json(&diff).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("口径差异清单 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?
    };
    std::fs::write(out, text)?;

    println!("口径差异清单已写出：{}", out.display());
    println!(
        "旧版本 {} → 新版本 {} · 对比口径项 {} · 变化 {} 处 · 未变 {} 项",
        from,
        to,
        diff.compared_slots,
        diff.change_count(),
        diff.unchanged_slots
    );
    for (change, n) in diff.count_by_change() {
        println!("  · {} {} 处", change, n);
    }
    Ok(())
}

/// 复算：按原规则版本重跑某个历史批次，与当初的结论逐条比对。
///
/// 口径换版之后，历史批次的结论不能跟着漂 —— 规则版本随行记录在库里，
/// 指定旧版本复算，结果应当与当初**逐条一致**。
///
/// 复算**只读**：不写问题表、不改行状态，产物是比对结论。
fn replay(batch: &str, rule_version: &str) -> Result<i32, EngineError> {
    let state_db = resolve_state_db(None)?;
    let repo = Repo::open(&state_db)?;

    let metas = repo.list_row_metas(batch)?;
    if metas.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在状态库里没有行，无法复算", batch),
            format!("batch_ref={}", batch),
        ));
    }

    // 规则集不带闸门：复算要显式指定版本，不能让闸门把它换成别的版本
    let cfg = PartialConfig::load(&ProcessEnv);
    let set = match cfg.get("REGVAL_RULE_DIR").map(PathBuf::from) {
        Some(d) if d.is_dir() => loader::load_reporting(&d)?,
        _ => loader::builtin_set(),
    };
    let rules = set.by_version(rule_version);
    if rules.is_empty() {
        let known: Vec<String> = {
            let mut versions: Vec<String> = set.all().iter().map(|r| r.version.clone()).collect();
            versions.sort();
            versions.dedup();
            versions
        };
        return Err(EngineError::at(
            ErrorCode::RuleVersionNotFound,
            format!(
                "规则集里没有版本 {} 的口径规则（现有版本：{}）",
                rule_version,
                if known.is_empty() {
                    "（无）".to_string()
                } else {
                    known.join("、")
                }
            ),
            format!("rule_version={}", rule_version),
        ));
    }

    // 当初的结论：只取规则产生的问题（绑定层问题不参与口径复算）
    let rule_ids: std::collections::BTreeSet<&str> =
        rules.iter().map(|r| r.rule_id.as_str()).collect();
    let stored: std::collections::BTreeSet<(String, String, String)> = repo
        .list_issues(batch)?
        .iter()
        .filter(|i| rule_ids.contains(i.rule_id.as_str()))
        .map(|i| (i.row_uid.clone(), i.rule_id.clone(), i.issue_key.clone()))
        .collect();

    // 按指定版本重跑
    let report = executor::execute_rules(repo.conn(), &rules, batch);
    let replayed: std::collections::BTreeSet<(String, String, String)> = report
        .issues()
        .iter()
        .map(|i| (i.row_uid.clone(), i.rule_id.clone(), i.issue_key.clone()))
        .collect();

    let only_stored: Vec<_> = stored.difference(&replayed).collect();
    let only_replayed: Vec<_> = replayed.difference(&stored).collect();
    let consistent = only_stored.is_empty() && only_replayed.is_empty();

    println!(
        "复算完成：批次 {} · 口径版本 {} · 原结论 {} 项 · 复算 {} 项",
        batch,
        rule_version,
        stored.len(),
        replayed.len()
    );
    println!(
        "结论：{}",
        if consistent {
            "一致 —— 换版没有让历史批次的结论漂移"
        } else {
            "不一致 —— 历史批次的结论随口径变了，须查清原因"
        }
    );
    for key in only_stored.iter().take(10) {
        println!("  · 仅原结论有：行 {} · 规则 {} · {}", key.0, key.1, key.2);
    }
    for key in only_replayed.iter().take(10) {
        println!("  · 仅复算有：行 {} · 规则 {} · {}", key.0, key.1, key.2);
    }
    if !only_stored.is_empty() || !only_replayed.is_empty() {
        println!(
            "差异合计 {} 处（仅原结论 {} · 仅复算 {}）",
            only_stored.len() + only_replayed.len(),
            only_stored.len(),
            only_replayed.len()
        );
    }
    Ok(if consistent { 0 } else { 1 })
}

/// 状态：把本机库里最近的运行摘要列出来。
fn status(batch: Option<&str>, limit: usize, state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;
    let summaries = repo.list_run_summaries(batch, limit)?;

    println!("状态库：{}", state_db.display());
    if summaries.is_empty() {
        println!(
            "还没有运行记录{}",
            match batch {
                Some(b) => format!("（批次 {}）", b),
                None => String::new(),
            }
        );
        return Ok(());
    }
    for s in &summaries {
        println!("{}", s.to_line());
    }
    // 批次汇总：这批整体算不算数（已上报 / 被退回 / 待重试 / 问题项数 四项口径）
    // —— 未上报名单与催报对照也用这一份，不另建报表系统。
    let batch_summaries = match batch {
        Some(b) if !b.trim().is_empty() => match repo.batch_summary(b.trim())? {
            Some(record) => vec![record],
            None => Vec::new(),
        },
        _ => repo.list_batch_summaries(limit)?,
    };
    if batch_summaries.is_empty() {
        println!("批次汇总：暂无记录（本机库里还没有落过批次汇总）");
    } else {
        println!("批次汇总：");
        for record in &batch_summaries {
            println!(
                "  {} · 状态 {} · 修订 {} · {}",
                record.batch_ref,
                record.batch_status,
                record.revision,
                push::batch::totals_of(record).to_line()
            );
        }
    }
    // 模型调用审计：最近一次调用的元数据与降级标记
    // （「这一批的疑似项里模型掺和了没有」—— 这一句从这里就能看，不用翻库）
    match repo.latest_model_audit(batch)? {
        Some(audit) => println!("模型调用审计：{}", audit.summary_line()),
        None => println!("模型调用审计：暂无记录（本轮还没有调用过模型端点）"),
    }
    // 行状态分布：这一批的行现在到哪一步了（待校验 / 校验通过 / 待确认 / 已上报 …）
    let state_counts = state::machine::counts(repo.conn(), batch.map(str::trim).filter(|b| !b.is_empty()))?;
    if state_counts.is_empty() {
        println!("行状态：暂无记录（本机库里还没有行状态）");
    } else {
        let dist: Vec<String> = state_counts
            .iter()
            .map(|(st, count)| format!("{} {} 行", st, count))
            .collect();
        println!("行状态：{}", dist.join(" · "));
    }
    Ok(())
}

/// 导出每行依据举证包：这一行命中了哪些规则、按哪条官方口径、该怎么改。
///
/// 产物从**本机库**读（库里落的行与问题），不重新跑规则 ——
/// 重跑可能与当初的结论不是同一套口径，产物就跟着变了，对不上当初发给科室的那一版。
///
/// 输出 `.csv` 出表格（一次命中一行，便于在 Excel 里逐条筛），其余后缀出 JSON。
/// 导出未上报名单与催报对照。
///
/// 两份产物：
/// - **未上报名单**（`--out`）：还剩哪些行没报上去，每行带**具体原因**
///   （错误码 / 缺失列 / 规则编号 / 未确认标记）；
/// - **催报对照**（`--reminder-out`）：按报告期给出「已收到 / 未收到」两列对照，
///   供报告期截止前催办。
///
/// 只读不改：不推进任何行状态、不写任何业务表。
fn export_pending(
    out: &Path,
    reminder_out: Option<&Path>,
    batch: Option<&str>,
    state_db: Option<&Path>,
    rule_dir: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;

    let batch_ref = match batch {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::Db,
                "状态库里还没有校验批次，先跑一次 regval validate 再导出未上报名单",
                state_db.display().to_string(),
            )
        })?,
    };

    let rows = repo.list_stored_rows(&batch_ref)?;
    if rows.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在状态库里没有行，无法导出未上报名单", batch_ref),
            format!("batch_ref={}", batch_ref),
        ));
    }
    let issues = repo.list_issues(&batch_ref)?;
    let states: std::collections::BTreeMap<String, String> = state::machine::list_state_values(
        repo.conn(),
        &batch_ref,
    )?
    .into_iter()
    .collect();
    let push_states = repo.list_row_push_states(&batch_ref)?;
    let confirmed = repo.confirmed_row_uids(&batch_ref)?;

    let roster = load_roster(rule_dir, &repo)?;
    let report = report::pending::build(
        &batch_ref,
        &rows,
        &issues,
        &states,
        &push_states,
        &confirmed,
        &roster,
    )?;

    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        report::pending::to_csv(&report).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("未上报名单 CSV 生成失败：{}", e),
                out.display().to_string(),
            )
        })?
    } else {
        report::pending::to_json(&report).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("未上报名单 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?
    };
    std::fs::write(out, text)?;

    println!("未上报名单已写出：{}", out.display());
    println!(
        "批次 {} · 共 {} 行 · 未上报 {} 行 · 已上报 {} 行",
        report.batch_ref, report.counts.rows_total, report.counts.rows_pending,
        report.counts.rows_reported
    );
    for (reason, n) in &report.counts.by_reason {
        println!("  · {} {} 行", reason, n);
    }

    if let Some(path) = reminder_out {
        let reminder_is_csv = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("csv"))
            .unwrap_or(false);
        let text = if reminder_is_csv {
            report::pending::to_reminder_csv(&report).map_err(|e| {
                EngineError::at(
                    ErrorCode::Io,
                    format!("催报对照 CSV 生成失败：{}", e),
                    path.display().to_string(),
                )
            })?
        } else {
            report::pending::to_json(&report).map_err(|e| {
                EngineError::at(
                    ErrorCode::Io,
                    format!("催报对照 JSON 序列化失败：{}", e),
                    path.display().to_string(),
                )
            })?
        };
        std::fs::write(path, text)?;
        println!("催报对照已写出：{}", path.display());
    }

    for period in &report.reminder.periods {
        println!(
            "  · 报告期 {}：已收到 {} 个科室 · 未收到 {} 个科室（{}）",
            period.reporting_period,
            period.received_count(),
            period.missing_count(),
            period.note
        );
    }
    if report.reminder.rows_without_valid_period > 0 {
        println!(
            "  · 注意：{} 行的报告期写法不在枚举内，未计入催报分组（这些行在未上报名单里）",
            report.reminder.rows_without_valid_period
        );
    }
    println!(
        "  · 应交科室名册来源：{} —— {}",
        report.reminder.roster_source, report.reminder.roster_note
    );
    Ok(())
}

/// 取应交科室名册：名册文件优先，院内 HIS 科室目录缓存兜底，都没有就空名册。
///
/// 名册读不出来**报错**（名册错了会算出一张冤枉科室的催报表）；
/// 目录缓存载荷坏了则退成空名册并在说明里写明 —— 催报对照因此少一列，
/// 但未上报名单照常出得来。
fn load_roster(
    rule_dir: Option<&Path>,
    repo: &Repo,
) -> Result<report::pending::Roster, EngineError> {
    let dir = match rule_dir {
        Some(d) => Some(d.to_path_buf()),
        None => {
            let cfg = PartialConfig::load(&ProcessEnv);
            cfg.get("REGVAL_RULE_DIR")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
        }
    };
    if let Some(dir) = dir {
        let path = dir.join(report::pending::ROSTER_FILE_NAME);
        if path.exists() {
            return report::pending::Roster::from_file(&path);
        }
    }
    if let Some(cached) = repo.latest_dept_directory_cache()? {
        match push::dept::DirectorySnapshot::from_payload(&cached.payload) {
            Ok(entries) if !entries.is_empty() => {
                return Ok(report::pending::Roster::from_dept_entries(
                    &entries,
                    &cached.fetched_at,
                ));
            }
            Ok(_) => {}
            Err(e) => {
                return Ok(report::pending::Roster::empty(&format!(
                    "院内科室目录缓存载荷坏了（{}），本轮未取到应交科室名册 —— 未收到一列为空",
                    e.code.as_str()
                )));
            }
        }
    }
    Ok(report::pending::Roster::empty(
        "未取到应交科室名册：规则目录里没有 roster.json，院内科室目录缓存也没有 —— 未收到一列为空（不拿本地见过的科室凑名册）",
    ))
}

fn export_evidence(
    out: &Path,
    batch: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;

    let batch_ref = match batch {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::Db,
                "状态库里还没有校验批次，先跑一次 regval validate 再导出举证包",
                state_db.display().to_string(),
            )
        })?,
    };

    let metas = repo.list_row_metas(&batch_ref)?;
    if metas.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!("批次 {} 在状态库里没有行，无法导出举证包", batch_ref),
            format!("batch_ref={}", batch_ref),
        ));
    }
    let issues = repo.list_issues(&batch_ref)?;
    let rule_set_version = repo.batch_rule_version(&batch_ref)?;
    // 逐项官方条款依据：从规则目录的口径种子文件取（指标编号 → 条款号 + 依据原文）。
    // 取不到就空着并在包里说明原因 —— 不拿别的口径顶替。
    let rule_dir = PartialConfig::load(&ProcessEnv)
        .get("REGVAL_RULE_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    let basis = match &rule_dir {
        Some(dir) => report::evidence::IndicatorBasis::from_seed_dir(dir),
        None => report::evidence::IndicatorBasis::default(),
    };
    let pkg = report::evidence::build(&batch_ref, &rule_set_version, &metas, &issues, &basis);

    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        report::evidence::to_csv(&pkg).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("举证包 CSV 生成失败：{}", e),
                out.display().to_string(),
            )
        })?
    } else {
        report::evidence::to_json(&pkg).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("举证包 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?
    };
    std::fs::write(out, text)?;

    println!("依据举证包已写出：{}", out.display());
    println!(
        "批次 {} · 行 {} · 命中 {} 项 · 未命中 {} 行 · 口径版本 {}",
        pkg.batch_ref,
        pkg.counts.rows,
        pkg.counts.hits,
        pkg.counts.rows_without_hits,
        if pkg.rule_set_version.is_empty() {
            "未记录"
        } else {
            pkg.rule_set_version.as_str()
        }
    );
    for (kind, n) in &pkg.counts.by_issue_kind {
        println!("  · {} {} 项", kind, n);
    }
    println!("  · 逐项官方条款依据：{}", pkg.indicator_basis_note);
    Ok(())
}

/// 导出回执台账（留档与第三方核对用）。
///
/// 一行**一次提交**（按幂等键 + 修订号计一条）—— 订正后重报的两笔各占一行，
/// 网关那边的两个回执号才对得上。
///
/// 导出内容可被第三方按**幂等键**与**回执号**交叉核对：
/// 台账自带 `cross_check` 段，把「一一对应成立没有」「有没有已上报却没有回执」
/// 「有没有回执找不到提交记录」「上报状态与行状态对不对得上」逐条写清楚。
fn export_receipts(
    out: &Path,
    batch: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;

    let batch_ref = match batch {
        Some(b) if !b.trim().is_empty() => b.trim().to_string(),
        _ => repo.latest_batch_ref()?.ok_or_else(|| {
            EngineError::at(
                ErrorCode::Db,
                "状态库里还没有校验批次，先跑一次 regval validate 再导出回执台账",
                state_db.display().to_string(),
            )
        })?,
    };

    let submissions = repo.list_submissions(&batch_ref)?;
    if submissions.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            format!(
                "批次 {} 在状态库里没有上报提交记录（这一批还没上报过）—— 要导出别的批次用 --batch <批次号>",
                batch_ref
            ),
            format!("batch_ref={}", batch_ref),
        ));
    }
    let receipts = repo.list_receipts(&batch_ref)?;

    // 行状态：行键 → 当前状态（取不到就是「未记录」，不猜）
    let states: std::collections::BTreeMap<String, String> = state::machine::list_state_values(
        repo.conn(),
        &batch_ref,
    )?
    .into_iter()
    .collect();

    // 退回说明：行键 → 网关原话（落在上报退回问题上）
    let push_notes = report::receipt::push_notes_by_row(&repo.list_issues(&batch_ref)?);

    // 口径版本：行键 → 这一批结论按哪套口径算的
    let rule_versions: std::collections::BTreeMap<String, String> = repo
        .list_stored_rows(&batch_ref)?
        .into_iter()
        .map(|r| (r.row_uid, r.rule_version))
        .collect();

    let export = report::receipt::build(
        &batch_ref,
        &submissions,
        &receipts,
        &states,
        &push_notes,
        &rule_versions,
    );

    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        report::receipt::to_csv(&export).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("回执台账 CSV 生成失败：{}", e),
                out.display().to_string(),
            )
        })?
    } else {
        report::receipt::to_json(&export).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("回执台账 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?
    };
    std::fs::write(out, text)?;

    println!("回执台账已写出：{}", out.display());
    println!(
        "批次 {} · 提交 {} 条 · 回执 {} 条 · 涉及 {} 行 · 订正重报 {} 行",
        export.batch_ref,
        export.counts.entries,
        export.counts.receipts,
        export.counts.rows,
        export.counts.revised_rows
    );
    for (status, n) in &export.counts.by_status {
        println!("  · 上报状态 {} {} 条", status, n);
    }
    for (state, n) in &export.counts.by_row_state {
        println!("  · 行状态 {} {} 行", state, n);
    }
    println!(
        "  · 交叉核对：幂等键+修订号 {} 组 ↔ 回执号 {} 个 · 一一对应 {}",
        export.cross_check.identity_pairs,
        export.cross_check.distinct_receipt_ids,
        if export.cross_check.receipt_pairs_one_to_one {
            "成立"
        } else {
            "不成立"
        }
    );
    println!(
        "  · 已上报无回执 {} 条 · 有回执无提交记录 {} 条 · 上报状态与行状态对不上 {} 行",
        export.cross_check.accepted_without_receipt,
        export.cross_check.receipt_without_submission,
        export.cross_check.state_disagreements
    );
    for conflict in &export.cross_check.conflicts {
        println!(
            "  ! {}（行 {} · 幂等键 {} · 修订 {}）：{}",
            conflict.kind,
            conflict.row_uid,
            conflict.idempotency_key,
            conflict.revision,
            conflict.detail
        );
    }
    // 交叉核对不通过 → 台账已写出，但**不能当干净的台账归档**。
    // 写出来是为了让人能照着查；非 0 退出是为了让脚本与人都不忽略它。
    if !export.cross_check.is_clean() {
        return Err(EngineError::at(
            ErrorCode::ReceiptLedgerMismatch,
            format!(
                "回执台账已写出：{}，但交叉核对不通过（{} 处冲突）—— 这份台账不能直接归档，先按上面列出的冲突逐条查",
                out.display(),
                export.cross_check.conflicts.len()
            ),
            format!("batch_ref={}", export.batch_ref),
        ));
    }
    Ok(())
}

/// 从结果契约里读重复检测第三级需要的那几段
#[derive(serde::Deserialize)]
struct ContractDupeView {
    /// 第三级（编辑距离）的运行参数、计数与距离分布直方
    #[serde(default)]
    dupe_distance: dupe::distance::DistanceStatusSection,
    /// 疑似重复候选对（各级都在里面，靠 match_level 区分）
    #[serde(default)]
    suspected_duplicates: Vec<dupe::SuspectedDuplicate>,
}

/// 导出重复检测第三级（编辑距离候选）与距离分布直方。
///
/// 输入是 `regval validate --out` 写出的那份结果契约 —— 候选对与直方都在契约里。
/// **导出不重跑一遍校验**：重跑会按当下生效的口径重新判一遍，
/// 导出结果与经办人当时看到的那一版对不上 —— 那比导不出来更难查。
fn export_dupe(out: &Path, contract_path: Option<&Path>) -> Result<(), EngineError> {
    let path = contract_path.ok_or_else(|| {
        EngineError::at(
            ErrorCode::ConfigMissing,
            "导出编辑距离候选需要指定结果契约文件（regval validate --out 写出的那份 JSON）",
            "regval export dupe --contract <契约 JSON 路径> --out <导出路径>".to_string(),
        )
    })?;
    let raw = std::fs::read_to_string(path).map_err(|e| {
        EngineError::at(
            ErrorCode::InputUnreadable,
            format!("结果契约读取失败：{}", e),
            path.display().to_string(),
        )
    })?;
    let view: ContractDupeView = serde_json::from_str(&raw).map_err(|e| {
        EngineError::at(
            ErrorCode::ContractWrite,
            format!("结果契约解析失败：{}", e),
            path.display().to_string(),
        )
    })?;

    let pairs: Vec<&dupe::SuspectedDuplicate> = view
        .suspected_duplicates
        .iter()
        .filter(|p| p.match_level == dupe::distance::MATCH_LEVEL_DISTANCE)
        .collect();

    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        dupe_pairs_to_csv(&pairs)?
    } else {
        let payload = serde_json::json!({
            "threshold": view.dupe_distance.threshold,
            "threshold_source": view.dupe_distance.threshold_source,
            "max_length_gap": view.dupe_distance.max_length_gap,
            "histogram": view.dupe_distance.histogram,
            "stats": view.dupe_distance.stats,
            "pairs": pairs,
        });
        serde_json::to_string_pretty(&payload).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("编辑距离候选 JSON 序列化失败：{}", e),
                out.display().to_string(),
            )
        })?
    };
    std::fs::write(out, text)?;

    println!("编辑距离候选已导出：{}", out.display());
    println!(
        "阈值 {} · 长度差上限 {} · 候选 {} 对 · 比过 {} 对",
        view.dupe_distance.threshold,
        view.dupe_distance.max_length_gap,
        pairs.len(),
        view.dupe_distance.stats.compared_pairs
    );
    for bucket in &view.dupe_distance.histogram {
        println!("  · {} {} 对", bucket.label, bucket.pairs);
    }
    Ok(())
}

/// 编辑距离候选的 CSV：一行一对
fn dupe_pairs_to_csv(pairs: &[&dupe::SuspectedDuplicate]) -> Result<String, EngineError> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record([
        "序号",
        "左侧行键",
        "右侧行键",
        "左侧引用",
        "右侧引用",
        "业务主键",
        "跨表",
        "字符级编辑距离",
        "差异说明",
        "判定状态",
        "判定依据",
    ])
    .map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("编辑距离候选 CSV 写入失败：{}", e),
            "表头",
        )
    })?;
    for (i, pair) in pairs.iter().enumerate() {
        wtr.write_record([
            (i + 1).to_string(),
            pair.left_row_key.clone(),
            pair.right_row_key.clone(),
            pair.left_reference.clone(),
            pair.right_reference.clone(),
            pair.business_key.clone(),
            if pair.cross_table { "是" } else { "否" }.to_string(),
            pair.distance.map(|d| d.to_string()).unwrap_or_default(),
            pair.distance_detail.clone(),
            pair.state.clone(),
            pair.basis.clone(),
        ])
        .map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("编辑距离候选 CSV 写入失败：{}", e),
                format!("第 {} 对", i + 1),
            )
        })?;
    }
    let bytes = wtr.into_inner().map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("编辑距离候选 CSV 收尾失败：{}", e),
            "csv",
        )
    })?;
    String::from_utf8(bytes).map_err(|e| {
        EngineError::at(
            ErrorCode::Io,
            format!("编辑距离候选 CSV 编码不是 UTF-8：{}", e),
            "csv",
        )
    })
}

/// 重复检测漏斗的标注基准集：跑一遍漏斗，逐级算召回率与精确率，落一张可核对的对比表。
///
/// 这条命令**不碰状态库、不碰口径规则** —— 它只量「漏斗这一段认得准不准」。
/// 第四级（语义归并）本轮未接入模型端点，候选为空；这一点在结论里照实写，
/// 不拿 0 冒充「查过了没查到」。
fn bench(bench_dir: &Path, out: &Path, threshold: Option<usize>) -> Result<(), EngineError> {
    let tables_dir = bench_dir.join("tables");
    let labels_path = bench_dir.join("labels.json");

    // 1. 读标注（取值不认识、左右两行是同一行 → 直接报错，不静默跳过）
    let bench_set = dupe::eval::load_bench(&labels_path)?;

    // 2. 读样例表（按文件名排序：同一份输入两次跑出行序一致）
    let rows = read_bench_tables(&tables_dir)?;

    // 3. 标注里的可读定位（表 + 行号）解析成行键
    let resolved = dupe::eval::resolve(&bench_set, &rows)?;

    // 4. 跑漏斗（第四级本轮未接入模型端点）
    let mut cfg = resolve_distance_config()?;
    if let Some(t) = threshold {
        cfg.threshold = t;
        cfg.source = format!("命令行指定（阈值 {}）", t);
    }
    let funnel = dupe::run_funnel(&rows, &cfg, &dupe::DisconnectedSemanticSource);

    // 5. 逐级算召回率与精确率
    let report = dupe::eval::evaluate(&resolved, &funnel);

    // 6. 落可核对产物（CSV 出表格，其余出 JSON）
    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        dupe::eval::to_csv(&report)
    } else {
        dupe::eval::to_json(&report)
    };
    std::fs::write(out, text)?;

    // 7. 终端摘要（人话）
    println!("重复检测漏斗基准集：{}", report.bench_set);
    println!(
        "样例 {} 行 · 标注 {} 条（真重复 {} 条 / 负例 {} 条）",
        report.total_rows,
        report.labeled_pairs,
        report.labeled_true_pairs,
        report.labeled_negative_pairs
    );
    for level in &report.levels {
        println!(
            "  · {} 累计 {} 对 · 本级新增 {} 对 · 命中 {} · 误报 {} · 召回 {} · 精确 {}",
            level.level_title,
            level.candidate_pairs,
            level.new_pairs,
            level.hit_labels,
            level.false_positives,
            fmt_rate(level.recall),
            fmt_rate(level.precision)
        );
    }
    for missed in &report.missed_true_pairs {
        println!(
            "  · 漏报（标注为真重复、四级都没报出来）：{} 与 {} —— {}",
            missed.left_reference, missed.right_reference, missed.basis
        );
    }
    if let Some(m) = &report.marginal_gain {
        println!(
            "  · 第四级边际增益：新增 {} 对 · 召回 {:+} · 精确 {:+} · {}",
            m.new_pairs,
            m.recall_delta.map(|v| v * 100.0).unwrap_or(0.0),
            m.precision_delta.map(|v| v * 100.0).unwrap_or(0.0),
            m.note
        );
    }
    println!("对比表已写出：{}", out.display());
    Ok(())
}

/// 读基准集里的样例表（按文件名排序：同一份输入两次跑出行序一致）。
///
/// 判定口径与主路径一致：空行与完全无效行不参与比对，但计数在别处留痕。
fn read_bench_tables(tables_dir: &Path) -> Result<Vec<ingest::SourceRow>, EngineError> {
    let entries = std::fs::read_dir(tables_dir).map_err(|e| {
        EngineError::at(
            ErrorCode::InputUnreadable,
            format!("基准集样例表目录读取失败：{}", e),
            tables_dir.display().to_string(),
        )
    })?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            matches!(ext.as_str(), "csv" | "xlsx" | "xlsm")
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(EngineError::at(
            ErrorCode::EmptyTable,
            "基准集里没有样例表（支持 .csv / .xlsx / .xlsm）",
            tables_dir.display().to_string(),
        ));
    }
    let mut rows: Vec<ingest::SourceRow> = Vec::new();
    for file in &files {
        for table in ingest::read_tables(file)? {
            rows.extend(ingest::map_table_with_report(&table)?.rows);
        }
    }
    Ok(rows)
}

/// 与基线方案做可复现对比。
///
/// 两条轴：
/// - **轴一 · 重复检测**：朴素确定性基线（Excel VLOOKUP 一把的做法）vs 四级漏斗，
///   在带人工标注的基准集上比召回率与精确率；
/// - **轴二 · 问题发现**：仅规则校验 vs 规则校验 + 分子分母公式复算，比问题发现数。
///
/// 产物：一张对比表（.csv 出表格，其余出 JSON）。同一份输入跑两次，文件逐字节相同。
///
/// 这条命令**不碰状态库、不碰上报网关**：它只量「比基线好在哪、好多少」。
fn compare(
    bench_dir: &Path,
    input: &Path,
    out: &Path,
    threshold: Option<usize>,
    rule_dir: Option<&Path>,
) -> Result<(), EngineError> {
    // 1. 轴一：基准集（标注 + 样例表）
    let bench_set = dupe::eval::load_bench(&bench_dir.join("labels.json"))?;
    let rows = read_bench_tables(&bench_dir.join("tables"))?;
    let resolved = dupe::eval::resolve(&bench_set, &rows)?;

    // 2. 轴二：输入表
    let axis_b_rows = compare::read_input(input)?;

    // 3. 规则集（缺省用内置兜底口径表；给了目录就装载目录里的版本化规则表）
    let rule_set = match rule_dir {
        Some(dir) => loader::load(dir)?,
        None => loader::builtin_set(),
    };
    let rules = rule_set.executable();
    if rules.is_empty() {
        return Err(EngineError::at(
            ErrorCode::RuleLoadFailed,
            "一条口径规则都没生效 —— 对比表会失真，先把规则装载问题解决",
            rule_dir
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "（内置兜底口径表）".to_string()),
        ));
    }

    // 4. 编辑距离阈值（与漏斗同一套配置口径）
    let mut cfg = resolve_distance_config()?;
    if let Some(t) = threshold {
        cfg.threshold = t;
        cfg.source = format!("命令行指定（阈值 {}）", t);
    }

    // 5. 跑两条轴
    let report = compare::build_report(
        &resolved,
        &rows,
        &cfg,
        &dupe::DisconnectedSemanticSource,
        &input.display().to_string(),
        &axis_b_rows,
        &rules,
    )?;

    // 6. 落可核对产物（CSV 出表格，其余出 JSON）
    let is_csv = out
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let text = if is_csv {
        compare::to_csv(&report)
    } else {
        compare::to_json(&report)
    };
    std::fs::write(out, text)?;

    // 7. 终端摘要（人话）
    println!("基线对比：{}", report.bench_set);
    println!(
        "轴一 · 重复检测（{} 行 · 标注 {} 条：真重复 {} 条 / 负例 {} 条）",
        report.bench_rows,
        report.labeled_pairs,
        report.labeled_true_pairs,
        report.labeled_negative_pairs
    );
    for scheme in &report.axis_a {
        println!(
            "  · {}：候选 {} 对 · 命中 {} · 误报 {} · 召回 {} · 精确 {}",
            scheme.scheme_title,
            scheme.metrics.candidate_pairs,
            scheme.metrics.hit_labels,
            scheme.metrics.false_positives,
            fmt_rate(scheme.metrics.recall),
            fmt_rate(scheme.metrics.precision)
        );
        for missed in &scheme.missed_true_pairs {
            println!(
                "    · 漏报（标注为真重复、本方案没报出来）：{} 与 {} —— {}",
                missed.left_reference, missed.right_reference, missed.basis
            );
        }
    }
    if let Some(m) = &report.marginal_gain {
        println!(
            "  · 第四级边际增益：新增 {} 对 · 召回 {:+} · 精确 {:+} · {}",
            m.new_pairs,
            m.recall_delta.map(|v| v * 100.0).unwrap_or(0.0),
            m.precision_delta.map(|v| v * 100.0).unwrap_or(0.0),
            m.note
        );
    }
    println!(
        "轴二 · 问题发现（{} · {} 行）",
        report.axis_b_input, report.axis_b_rows
    );
    for scheme in &report.axis_b {
        println!(
            "  · {}：规则 {} 条 · 问题 {} 项 · 涉及 {} 行 · 不通过 {} 行",
            scheme.scheme_title,
            scheme.rules,
            scheme.issues,
            scheme.rows_affected,
            scheme.rows_blocked
        );
    }
    println!(
        "  · 公式复算多发现 {} 项问题（{}），其中 {} 行原本判「通过」",
        report.formula_delta.new_issues,
        report.formula_delta.rule_ids.join(" / "),
        report.formula_delta.newly_blocked_rows
    );
    println!("结论：{}", report.conclusion);
    println!("对比表已写出：{}", out.display());
    Ok(())
}

/// 比率的终端显示（百分比；无定义时写「无定义」）
fn fmt_rate(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.2}%", x * 100.0),
        None => "无定义".to_string(),
    }
}

/// 规则：列出当前生效的规则集与规则目录状态。
///
/// 规则目录配了（`REGVAL_RULE_DIR`）就装载目录里的版本化规则表 —— 清单里能看到
/// 生效版本、已被取代但保留可追的旧版本、以及依据缺失计数；
/// 没配就退回内置兜底口径表。
///
/// 配了本机校验库（`REGVAL_STATE_DB`）时，装载**过换版闸门**：清单里能看出
/// 哪一版待确认生效、为什么没生效；没配则按「首次装载」口径列出。
fn rules(
    dir: Option<&Path>,
    out: Option<&Path>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    let cfg = PartialConfig::load(&ProcessEnv);
    let dir_path = dir
        .map(|p| p.to_path_buf())
        .or_else(|| cfg.get("REGVAL_RULE_DIR").map(PathBuf::from));

    // 闸门要读「已用过的口径版本」与「质控岗确认记录」—— 状态库可用才过闸门
    let repo = match resolve_state_db_optional(state_db)? {
        Some(path) => Some(Repo::open(&path)?),
        None => None,
    };
    let today = today_date();
    let approvals = match &repo {
        Some(r) => r.approval_index()?,
        None => version::ApprovalIndex::default(),
    };
    let applied = match &repo {
        Some(r) => r.applied_rule_versions()?,
        None => Vec::new(),
    };
    let gate = version::VersionGate {
        approvals: &approvals,
        applied_versions: &applied,
        today: &today,
    };

    let loaded = match &dir_path {
        Some(p) if p.is_dir() => {
            let count = std::fs::read_dir(p).map(|it| it.count()).unwrap_or(0);
            println!("规则目录：{}（{} 个条目）", p.display(), count);
            // 汇报模式：缺依据的规则被剔除出执行集合，但计入清单 —— 运维一眼看出要补哪条
            Some(loader::load_gated(p, loader::LoadMode::Reporting, &gate)?)
        }
        Some(p) => {
            println!(
                "规则目录：{} —— 不存在或不是目录（当前仍用内置规则集）",
                p.display()
            );
            None
        }
        None => {
            println!("规则目录：未设置 REGVAL_RULE_DIR（当前仍用内置规则集）");
            None
        }
    };

    let rule_set = match loaded {
        Some(set) => set,
        None => loader::builtin_set_gated(&gate)?,
    };

    print_rule_manifest(&rule_set);

    for rule in rule_set.all() {
        // 检查项（同一检查类型下的不同检查项，如枚举越界下的四个维度）单独点出来，
        // 否则清单上四条「枚举越界」看起来一模一样，运维不知道各管各的什么。
        let check = match rule.check_column.as_deref() {
            Some(column) => format!(" · 检查项 {}", column),
            None => String::new(),
        };
        println!(
            "  {} {} · {}{} · 版本 {} · 严重度 {} · 适用指标 {}",
            pad_display(rule.status.as_str(), 12),
            rule.rule_id,
            rule.rule_kind.as_str(),
            check,
            rule.version,
            rule.severity.as_str(),
            rule.indicator_code.as_deref().unwrap_or("全部")
        );
        println!("        依据：{}", rule.basis);
        println!("        来源：{}", rule.source_file);
    }

    // 换版闸门：被拦下的口径逐条说明（哪一版、为什么、怎么补）
    print_blocked_switches(&rule_set);

    // 换版确认记录：谁在什么时候确认了哪一版
    if !approvals.is_empty() {
        println!("换版确认记录：");
        for approval in &approvals.entries {
            let scope = if approval.slot_key.is_empty() {
                "整套口径".to_string()
            } else {
                approval.slot_key.clone()
            };
            println!(
                "  · 版本 {} · 确认人 {} · 确认时间 {} · 范围 {}",
                approval.to_version, approval.approver, approval.approved_at, scope
            );
            if !approval.note.is_empty() {
                println!("        备注：{}", approval.note);
            }
        }
    }

    if let Some(path) = out {
        let json = serde_json::to_string_pretty(&rule_set.status_section()).map_err(|e| {
            EngineError::at(
                ErrorCode::Io,
                format!("规则清单序列化失败：{}", e),
                path.display().to_string(),
            )
        })?;
        std::fs::write(path, json)?;
        println!("规则清单已写出：{}", path.display());
    }
    Ok(())
}

/// 打印被换版闸门拦下的口径
fn print_blocked_switches(rule_set: &loader::RuleSet) {
    let blocked = rule_set.blocked_switches();
    if blocked.is_empty() {
        return;
    }
    println!("换版闸门：{} 条口径未生效（本轮继续用旧版）", blocked.len());
    for b in blocked {
        println!(
            "  · {} · 版本 {} · 错误码 {}",
            b.slot_label, b.to_version, b.code
        );
        println!("        {}", b.reason);
        if !b.from_version.is_empty() {
            println!("        实际继续使用：版本 {}", b.from_version);
        }
    }
}

/// 记一条换版确认（质控岗确认某个口径版本可以生效）。
///
/// 确认人与确认时间必填；被确认的版本必须在规则目录里真的存在 ——
/// 确认一个不存在的版本等于没确认。
fn rules_approve(
    version: &str,
    approver: &str,
    from_version: Option<&str>,
    slot: Option<&str>,
    note: Option<&str>,
    state_db: Option<&Path>,
) -> Result<(), EngineError> {
    if approver.trim().is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "换版确认必须写确认人（质控岗）：没有人的口径换版等于没人负责",
            "--approver",
        ));
    }
    if version.trim().is_empty() {
        return Err(EngineError::at(
            ErrorCode::ConfigInvalid,
            "换版确认必须写被确认的口径版本",
            "--version",
        ));
    }

    let state_db = resolve_state_db(state_db)?;
    let mut repo = Repo::open(&state_db)?;

    // 被确认的版本必须在规则目录里真的存在
    let cfg = PartialConfig::load(&ProcessEnv);
    let rule_dir = cfg.get("REGVAL_RULE_DIR").map(PathBuf::from);
    if let Some(dir) = &rule_dir {
        let set = loader::load_reporting(dir)?;
        if set.by_version(version.trim()).is_empty() {
            return Err(EngineError::at(
                ErrorCode::RuleVersionNotFound,
                format!(
                    "规则目录 {} 里没有版本 {} 的口径规则，确认这一版没有意义",
                    dir.display(),
                    version.trim()
                ),
                format!("rule_dir={} version={}", dir.display(), version.trim()),
            ));
        }
    }

    let approval = version::Approval {
        to_version: version.trim().to_string(),
        from_version: from_version.unwrap_or_default().trim().to_string(),
        slot_key: slot.unwrap_or_default().trim().to_string(),
        approver: approver.trim().to_string(),
        approved_at: regval::db::repo::now_iso8601(),
        note: note.unwrap_or_default().trim().to_string(),
    };
    repo.record_approval(&approval)?;

    println!("换版确认已记录：");
    println!("  · 口径版本 {}", approval.to_version);
    if !approval.from_version.is_empty() {
        println!("  · 替换版本 {}", approval.from_version);
    }
    println!(
        "  · 范围 {}",
        if approval.slot_key.is_empty() {
            "整套口径"
        } else {
            &approval.slot_key
        }
    );
    println!("  · 确认人 {}", approval.approver);
    println!("  · 确认时间 {}", approval.approved_at);
    if !approval.note.is_empty() {
        println!("  · 备注 {}", approval.note);
    }
    println!("  · 状态库 {}", state_db.display());
    Ok(())
}

/// 查看已有的换版确认记录
fn rules_approvals(state_db: Option<&Path>) -> Result<(), EngineError> {
    let state_db = resolve_state_db(state_db)?;
    let repo = Repo::open(&state_db)?;
    let approvals = repo.list_approvals()?;
    println!("状态库：{}", state_db.display());
    if approvals.is_empty() {
        println!("还没有换版确认记录");
        return Ok(());
    }
    println!("换版确认记录：{} 条", approvals.len());
    for a in &approvals {
        let scope = if a.slot_key.is_empty() {
            "整套口径".to_string()
        } else {
            a.slot_key.clone()
        };
        println!(
            "  · 版本 {} · 确认人 {} · 确认时间 {} · 范围 {}",
            a.to_version, a.approver, a.approved_at, scope
        );
    }
    Ok(())
}

/// 打印规则装载结果（规则条数、各状态计数、依据缺失计数）
fn print_rule_manifest(rule_set: &loader::RuleSet) {
    let m = rule_set.manifest();
    println!(
        "装载结果：共 {} 条 · 生效 {} · 已被取代 {} · 依据缺失 {}",
        m.total, m.active, m.superseded, m.basis_missing
    );
    println!(
        "口径槽位：{} 个（同一槽位内按版本号取最高版生效，旧版保留可追）",
        m.slots
    );
    if !m.set_version.is_empty() {
        println!("规则集版本：{} · 指纹 {}", m.set_version, m.set_fingerprint);
    }
    for (i, file) in m.files.iter().enumerate() {
        let label = m.rule_sets.get(i).map(|s| s.as_str()).unwrap_or("");
        if label.is_empty() {
            println!("规则表文件：{}", file);
        } else {
            println!("规则表文件：{}（口径版本 {}）", file, label);
        }
    }
    // 口径种子：官方 27 项指标的登记。**口径待补**要看得见 ——
    // 官方附件没写明的地方（单位、精度）不猜，运维得知道哪几项还没补全。
    for seed in &m.seed_sets {
        println!(
            "口径种子：{} · 共 {} 项 · 比率型 {} · 人均负担型 {} · 口径待补 {} 项 · 按默认精度比对 {} 项（来源：{}）",
            seed.seed_set,
            seed.total,
            seed.ratio_count,
            seed.burden_count,
            seed.pending.len(),
            seed.precision_defaulted,
            seed.source_file
        );
        for pending in &seed.pending {
            println!(
                "  {} {} · {}",
                pad_display("口径待补", 10),
                pending.indicator_code,
                pending.reasons.join("；")
            );
        }
        for rule_id in &seed.replaced_rules {
            println!(
                "  {} {} · 口径以官方种子为准（规则表里的同名规则已替换）",
                pad_display("种子生效", 10),
                rule_id
            );
        }
        for rule_id in &seed.kept_newer_rules {
            println!(
                "  {} {} · 规则表版本更高，保留规则表口径",
                pad_display("保留新版", 10),
                rule_id
            );
        }
    }
    for rej in &m.rejected {
        println!(
            "  {} {} · 错误码 {} · {}（来源：{}）",
            pad_display("未装载", 10),
            rej.rule_id,
            rej.code,
            rej.message,
            rej.source_file
        );
    }
    if m.syntax_error_count > 0 {
        println!(
            "语法不通过：{} 条（下列规则本次不生效，其余规则照常执行）",
            m.syntax_error_count
        );
        for se in &m.syntax_errors {
            println!(
                "  {} {} · 错误码 {} · {}（来源：{}）",
                pad_display("语法不通过", 10),
                se.rule_id,
                se.code,
                se.message,
                se.source_file
            );
        }
    }
}

/// 规则集装载：`REGVAL_RULE_DIR` 指向目录时装载该目录的版本化规则表
/// （严格模式 —— 缺依据、字段写错等直接报错，不静默少一条口径）；
/// 未配置时用内置兜底口径表。
///
/// **装载要过换版闸门**：状态库里已经在用某一版口径时，旁边又出现更高的版本，
/// 就是换版 —— 未经质控岗确认不生效，继续用旧版并明确提示。
fn load_rule_set(repo: &Repo) -> Result<loader::RuleSet, EngineError> {
    let cfg = PartialConfig::load(&ProcessEnv);
    let today = today_date();
    let approvals = repo.approval_index()?;
    let applied = repo.applied_rule_versions()?;
    let gate = version::VersionGate {
        approvals: &approvals,
        applied_versions: &applied,
        today: &today,
    };
    match cfg.get("REGVAL_RULE_DIR") {
        Some(dir) if !dir.trim().is_empty() => {
            loader::load_gated(Path::new(dir.trim()), loader::LoadMode::Strict, &gate)
        }
        _ => loader::builtin_set_gated(&gate),
    }
}

/// 今天（`YYYY-MM-DD`）—— 换版闸门判断生效日期用
fn today_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 重复检测第三级（编辑距离）的阈值：环境变量 → 规则目录 `dupe-distance.json` → 内置默认值。
fn resolve_distance_config() -> Result<dupe::distance::DistanceConfig, EngineError> {
    let cfg = PartialConfig::load(&ProcessEnv);
    let rule_dir = cfg
        .get("REGVAL_RULE_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    dupe::distance::resolve_config(&ProcessEnv, rule_dir.as_deref())
}

/// 按显示宽度补空格（中文按 2 列宽算），让清单在终端里对齐
fn pad_display(text: &str, width: usize) -> String {
    let display_width: usize = text.chars().map(display_width_of).sum();
    format!(
        "{}{}",
        text,
        " ".repeat(width.saturating_sub(display_width))
    )
}

/// 单个字符的显示宽度（东亚全角字符按 2 列）
fn display_width_of(c: char) -> usize {
    let u = c as u32;
    let wide = (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u);
    if wide {
        2
    } else {
        1
    }
}

/// doctor：运行环境与凭据逐项自检。
fn doctor(state_db: Option<&Path>) -> Result<i32, EngineError> {
    let cfg = PartialConfig::load(&ProcessEnv);
    let input = DoctorInput {
        config: &cfg,
        state_db_override: state_db.map(|p| p.to_path_buf()),
        path_env: std::env::var("PATH").ok(),
        cargo_home: cargo_home(),
    };
    let report = doctor::run(&input);
    print!("{}", report.render(&regval::db::repo::now_iso8601()));
    Ok(if report.is_ready() { 0 } else { 1 })
}

/// 状态库路径：`--state-db` 优先，其次环境变量 `REGVAL_STATE_DB`；两者都没有就报错。
///
/// 不用默认值兜底 —— 状态库位置写错了却静默另开一个库，比直接报错难查得多。
fn resolve_state_db(explicit: Option<&Path>) -> Result<PathBuf, EngineError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let cfg = PartialConfig::load(&ProcessEnv);
    cfg.require(&["REGVAL_STATE_DB"])?;
    Ok(PathBuf::from(
        cfg.get("REGVAL_STATE_DB").unwrap_or_default(),
    ))
}

/// 状态库路径（可选）：没有配置时返回 `None` 而不是报错。
///
/// 给「诊断类」命令用 —— 看规则清单不该因为没配状态库就失败；
/// 但要说清「没配状态库」这件事（此时按首次装载口径列规则）。
fn resolve_state_db_optional(explicit: Option<&Path>) -> Result<Option<PathBuf>, EngineError> {
    if let Some(p) = explicit {
        return Ok(Some(p.to_path_buf()));
    }
    let cfg = PartialConfig::load(&ProcessEnv);
    match cfg.get("REGVAL_STATE_DB") {
        Some(p) if !p.trim().is_empty() => Ok(Some(PathBuf::from(p.trim()))),
        _ => Ok(None),
    }
}

/// cargo 主目录：`CARGO_HOME` 或 `~/.cargo`
fn cargo_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CARGO_HOME") {
        let p = PathBuf::from(h);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo"))
}
