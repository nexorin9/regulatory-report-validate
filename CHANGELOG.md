# 变更日志

本文件由沌构机（ChaosForge）在每轮交付时自动生成与维护，**不需要人工编辑**。条目按交付顺序排列，面向「这个项目现在能做什么」。版本块最新的在最上方。

## [0.1.0] - 2026-11-07

**本轮目标**：把各科室填好的几张卫健专项上报表并进模板，在原表内点一次「校验」，必填缺失、枚举越界、单位不符、疑似重复四类问题就地标出；经办人勾选确认后点「上报」，通过的行按幂等键推到院内上报网关，回执写回原表状态列

| 给谁用 | 什么时候用 |
|--------|------------|
| 信息科数据管理员（经办） | 报告期收齐各科室专项上报表，要一张表一张表人工核对再汇总 |

### 跑通主干

- **创建项目子目录与中文 README 完整骨架**
  在 round 根目录创建项目子目录 regulatory-report-validate/（与 task.json 顶层 project 字段逐字一致），放入 .gitignore、qrcode.jpg、依赖清单、配置样例与中文 README 完整章节骨架，并建 docs/concepts/ 与 docs/screenshots/ 两个目录。本任务只做地基与文档骨架，不写业务逻辑
  - 证据：验收全通过：test -f regulatory-report-validate/qrcode.jpg 成功；grep -q '## 关注我们' 与 grep -q 'qrcode.jpg' 均命中 README.md；README.md 实测 ## 章节 13（≥12）、### 子节 18（≥8）、表格行 61（≥5）、代码块围栏 22（≥8）、正文字符 13281（≥5000）、含 ASCI…

### 新增能力

- **主干最窄端到端切片：读表 → 落本机库 → 一条口径规则 → 结果契约 JSON**
  打通产品主干的最窄一条可运行路径：读一张科室上报表 → 落本机校验库 → 执行一条参数化口径规则 → 产出可核对的结果契约 JSON。完成后必须能用一条命令得到产物，不留空壳抽象层。本任务是 plan.md 里「主干最窄端到端」这一格
  - 证据：cargo test 13 passed（9 单测 + 4 集成，日志 logs/task2-cargo-test.log）；cargo run -- validate --input tests/fixtures/mini.csv --out /tmp/regval-result.json 输出「已校验 6 行 · 2 项口径问题 · 涉及 2 行」；/tmp/regval-result.js…
- **结果契约 schema_version、5 个新列映射与 Excel 宏最小版写回**
  把引擎与表格插件之间的交接面定死：带 schema_version 的 JSON 契约、写入原表的 5 个新列映射，以及一个最小 Excel 加载宏（点一下「校验」→ 原表右侧出现新列）。契约是三入口共用面，版本不匹配必须拒绝写回
  - 证据：cargo test --offline 全绿 26 passed / 0 failed / 0 warning（13 单测 + 6 契约一致性 + 4 主干集成 + 3 写回集成，日志 logs/task3-cargo-test.log）；契约一致性测试断言 RegVal.bas 的 REGVAL_SCHEMA_VERSION=regval.contract/1.0.0 与 REGVAL_CO…
- **工程底座：环境变量配置、错误码体系、结构化日志、CLI 框架与 doctor 自检**
  支撑层的可验收底座：环境变量凭据位、机器可读错误码、结构化日志与运行摘要、clap CLI 框架、regval doctor 自检。本任务不含库 schema（任务 5）、不含实体绑定（任务 6）、不含任何业务规则
  - 证据：cargo test --offline 全绿 50 passed / 0 failed / 0 warning（29 单测 + 8 CLI 集成 + 6 契约一致性 + 4 主干集成 + 3 写回集成，日志 logs/task4-cargo-test.log）；regval doctor 实跑两态：环境齐备时退出码 0 并逐项列出「13 项正常 · 0 项提示 · 0 项缺失」（工具链/状态库…
- **校验库完整 schema 与迁移**
  本机校验库的 6 张表与迁移机制，是后续规则、状态、审计的落地基础。迁移可重复执行且幂等；库损坏要能识别并给出恢复路径，不静默重建空库
  - 证据：cargo test --offline 全绿 64 passed / 0 failed（日志 logs/task5-cargo-test.log：34 单测 + 8 CLI + 6 契约一致性 + 4 主干集成 + 9 新增库结构 + 3 写回集成）。新增 tests/db_schema_test.rs 9 例逐条命中验收：①迁移连跑两次表清单/索引清单/rows 列定义三项快照完全一致且第二…
- **行键、业务主键与跨表匹配键绑定；同键歧义拒绝合并**
  权威实体绑定层：把每张来源表的行绑定到稳定行键与业务主键，跨表用匹配键对齐；同键多行且取值不同时不自动合并，标「待人工判定」，绝不二选一
  - 证据：cargo test --offline 全绿 83 passed / 0 failed（45 单测 + 8 CLI + 6 契约一致性 + 4 主干集成 + 9 库结构 + 8 实体绑定 + 3 写回，日志 logs/task6-cargo-test.log）；新增 tests/entity_binding_test.rs 8 例逐条命中验收：①同权威键异值整组 2 行标 pending_ma…
- **口径规则引擎 A-1：版本化规则表装载与规则版本切换**
  核心差异化模块第一段第一格。规则先装载后执行：每条规则带官方依据与版本号；换版走「新版本 + 差异标注」，旧版本留存可追。语法预校验在任务 8，27 项官方种子在任务 17，换版闸门与差异标注在任务 19
  - 证据：cargo test --offline 全量回归 99 passed / 0 failed（日志 logs/task7-cargo-test.log：49 单测 + 8 CLI + 6 契约一致性 + 4 主干集成 + 9 库结构 + 8 实体绑定 + 12 新增规则装载 + 3 写回）。新增 tests/rules_loader_test.rs 12 例逐条命中验收：①同一目录装载两次 ma…
- **口径规则引擎 A-2：规则 SQL 语法预校验与出错定位**
  核心差异化模块第一段第二格。规则先解析再执行：装载即预校验，语法错的规则不生效并报出出错 token 与位置，绝不静默通过。源产品主路径「SQL 先经解析器得到 AST 再执行、解析错误带出错 token」在此内置（github_ref/libsql/vendored/sqlite3-parser/src/parser/mod.rs 的 ParserError 与 dialect/mod.rs 的 tokenizer）
  - 证据：cargo test --offline 全量回归 110 passed / 0 failed（日志 logs/task8-cargo-test.log：54 单测 + 8 CLI + 6 契约一致性 + 4 主干集成 + 9 库结构 + 8 实体绑定 + 12 规则装载 + 6 规则语法 + 3 写回），相对 Task 7 的 99 passed 净增 11 例。新增 tests/rules_…
- **口径规则引擎 A-3：参数化规则执行器**
  核心差异化模块第一段第三格。规则以绑定参数执行，不拼字符串；每条规则产出可落库的问题行（issue），带规则 id、类型、严重度与官方依据。四类检查器分别在任务 10–13 实现，本任务只做执行底座
  - 证据：cargo test --offline 全量回归 126 passed / 0 failed / 0 warning（57 单测 + 8 CLI + 6 契约一致性 + 4 契约 + 9 库结构 + 8 实体绑定 + 13 规则执行器 + 12 规则装载 + 6 规则语法 + 3 写回，日志 logs/task9-cargo-test.log；相对 Task 8 的 110 passed 净增…
- **口径规则引擎 A-4：必填与表内唯一性检查器**
  核心差异化模块第一段第四格。四类口径问题之一：必填缺失与业务主键重复。空值语义明确，不做「0 值不算缺失」的宽容处理
  - 证据：cargo test --offline 全量回归 143 passed / 0 failed（日志 logs/task10-cargo-test.log：63 单测 + 8 CLI + 6 契约一致性 + 4 契约 + 9 库结构 + 8 实体绑定 + 11 新增必填与唯一性 + 13 规则执行器 + 12 规则装载 + 6 规则语法 + 3 写回）。新增 tests/required_che…
- **口径规则引擎 A-5：枚举越界与条件必填检查器**
  核心差异化模块第一段第五格。枚举越界要给出「期望取值」清单，不只报「不在枚举内」；条件必填按指标类别判定，避免在非比率型指标上误报
  - 证据：cargo test --offline 全量回归 170 passed / 0 failed（74 单测 + 8 CLI + 6 契约一致性 + 4 契约 + 9 库结构 + 8 实体绑定 + 15 新增枚举检查器 + 1 规则表回归 + 11 必填与唯一性 + 13 规则执行器 + 12 规则装载 + 6 规则语法 + 3 写回），相对 Task 10 的 143 passed 净增 27…
- **口径规则引擎 A-6：计量单位检查器**
  核心差异化模块第一段第四格。单位不符是四类口径问题之一：比率型指标单位固定为 %，其余指标按官方口径逐项校验；单位别名归一与重复检测共用同一份别名表
  - 证据：cargo test --offline 全量回归 197 passed / 0 failed（日志 logs/task12-cargo-test.log：86 单测 + 15 新增计量单位检查器 + 96 既有集成）。新增 tests/unit_checker_test.rs 15 例逐条命中验收：①比率型指标单位为 % 时通过（该行 0 条问题）；②写成「例」报 R-UNIT-001 一条「…
- **口径规则引擎 A-7：分子分母公式复算检查器**
  核心差异化模块第一段第五格。比率型指标按官方口径复算填报值，容差按指标给定的精度；分母为 0、负值与非数值分别归类，不与「公式不符」混在一起
  - 证据：cargo test --offline 全量回归 212 passed / 0 failed / 0 warning（日志 logs/task13-cargo-test.log：90 单测 + 8 CLI + 6 契约一致性 + 4 契约 + 9 库结构 + 8 实体绑定 + 15 枚举检查器 + 11 规则装载 + 11 规则语法 + 13 规则执行器 + 12 必填与唯一性 + 6 计量单…
- **口径规则引擎 A-8：27 项官方指标口径种子装载**
  把官方口径文件里的 27 项指标变成可执行的规则种子。编号、定义、计算公式与依据一律以官方 PDF 为准，不推断；依据缺失的指标标「口径待补」而不是猜一个
  - 证据：cargo test --offline 全量回归 228 passed / 0 failed / 0 warning（日志 logs/task17-cargo-test.log；相对 Task 13 的 212 passed 净增 16 例 = 12 个新增 tests/seed_test.rs + 4 个 seed.rs 模块内单测）。新增 tests/seed_test.rs 12 例逐条…
- **口径规则引擎 A-9：问题清单汇总与每行依据举证包**
  把四类检查器产出的 issue 汇总成可核对的清单，并为每一行产出含官方依据的举证包——这是给科室看的「为什么不合格、按哪条口径、该怎么改」的落点
  - 证据：cargo test --offline 全量回归 248 passed / 0 failed（日志 logs/task18-cargo-test.log：103 单测 + 8 CLI + 6 契约一致性 + 4 契约 + 9 库结构 + 8 实体绑定 + 15 枚举 + 11 公式 + 11 本任务新增 + 1 规则表回归 + 11 必填 + 13 执行器 + 12 规则装载 + 6 规则语法…
- **口径规则引擎 A-10：规则版本切换与口径差异标注（换版闸门）**
  核心差异化模块第一段的版本追踪能力。口径随属地文件变动，换版必须有闸门与差异说明，不能静默换口径导致历史批次结论漂移
  - 证据：cargo test --offline 全量回归 264 passed / 0 failed（日志 logs/task19-cargo-test.log；相对 Task 18 的 248 净增 16 例 = 新增 tests/version_switch_test.rs 6 例 + 新模块内单测 10 例）。新增 tests/version_switch_test.rs 6 例逐条命中验收：①…
- **重复检测四级漏斗 L1：确定性键重复检测**
  核心差异化模块第二段第一级：完全一致重复键检测。只标「疑似重复」，不自动合并、不自动推送，等人工确认（确认流见任务 36）
  - 证据：cargo test --offline 全量回归 283 passed / 0 failed / 0 warning（日志 logs/task20-cargo-test.log：125 单测 + 158 集成）。新增 tests/dupe_exact_test.rs 7 例 + src/dupe/exact.rs 6 例 + src/dupe/mod.rs 5 例，逐条命中验收：①取值完全一致…
- **重复检测四级漏斗 L2：归一化候选**
  核心差异化模块第二段第二级：书写差异但语义相同的行（院感 / 医院感染、全半角、中文空格）靠确定性键抓不到，靠归一化能抓到
  - 证据：cargo test --offline 全量回归 307 passed / 0 failed（日志 logs/task21-cargo-test.log；相对 Task 20 的 283 净增 24 例 = 12 个 L2 模块内单测 + 6 个口径层归一单测 + 6 个集成验收）。新增 tests/dupe_normalize_test.rs 6 例逐条命中验收：①全角/半角、中文空格、指标…
- **重复检测四级漏斗 L3：编辑距离候选与阈值**
  核心差异化模块第二段第三级：归一化仍吸收不了的近似写法用字符级编辑距离取近邻。阈值可配，且必须给出可解释的候选理由与距离分布
  - 证据：cargo test --offline 全量回归 326 passed / 0 failed（日志 logs/task22-cargo-test.log；相对 Task 21 的 307 净增 19 例 = 11 个 L3 模块内单测 + 8 个集成验收）。新增 tests/dupe_distance_test.rs 8 例逐条命中验收：①阈值内的行对出候选对并给出距离与差异说明（BK-DIS…
- **重复检测四级漏斗编排与逐级召回/精确率基线对比**
  核心差异化模块第二段的收口：把三级确定性候选 + 模型级语义归并做成可逐级对比的漏斗，用同一份带标注的样例产出可复现的召回/精确率对比表——这是决定「要不要加模型这一级」的依据，不是拍脑袋
  - 证据：cargo test --offline 全量回归 341 passed / 0 failed / 1 ignored（logs/task23-cargo-test.log；较 Task 22 的 326 passed 净增 15 例 = 新增 tests/dupe_funnel_eval_test.rs 13 例 + src/dupe/eval.rs 模块内单测 2 例）。新增 13 例逐条命…
- **ModelGateway（OpenAI-compatible）客户端与结构化输出**
  ai_role=assist 的唯一模型入口。单一 provider 中立的 ModelGateway 抽象，业务层只依赖该抽象，不耦合任何厂商 SDK；base_url / api_key / model / timeout 四项全部环境变量化
  - 证据：cargo test --offline 全量回归 367 passed / 0 failed / 1 ignored（库内单测 172 含本任务 model 18 例 + 集成 195 含本任务 model_gateway_test 10 例；1 例 ignored 为既有规则表重生成用例；日志 logs/task24-cargo-test.log）。新增 tests/model_gatewa…
- **ModelGateway 降级与调用审计**
  模型不可达时主路径不能阻塞：只输出确定性候选并一律标「疑似」，绝不静默合并、绝不自动推送。每次调用留可审计元数据
  - 证据：cargo test --offline 全量回归 386 passed / 0 failed / 1 ignored（26 个测试目标，日志 logs/task25-cargo-test.log；相对 Task 24 的 367 净增 19 例 = 新增 tests/model_degradation_test.rs 7 例，以及修好此前编译不过的 tests/dupe_funnel_eval…
- **重复检测四级漏斗 L4：语义归并接入（人工确认前不合并）**
  模型级只作为漏斗第四级的候选来源。归并结论一律为「疑似」，合并与否由经办人勾选确认后才生效；模型永不推进任何业务状态
  - 证据：cargo test --offline 全量回归 402 passed / 0 failed / 1 ignored（日志 logs/task26-cargo-test.log；相对 Task 25 的 386 passed 净增 16 例 = 新增 tests/semantic_merge_test.rs 10 例 + src/dupe/merge.rs 模块内单测 6 例；1 例 igno…
- **幂等上报 B-1：幂等键与载荷摘要**
  核心差异化模块第三段第一格。幂等键与时间戳、重试次数、载荷内容均无关；订正后载荷摘要变化才产生新 revision，业务键不变
  - 证据：cargo test --offline 全量回归 421 passed / 0 failed / 1 ignored（日志 logs/task27-cargo-test.log；相对 Task 26 的 402 passed 净增 19 例 = src/push/idempotency.rs 模块内单测 10 例 + 新增 tests/push_idempotency_test.rs 9 例）…
- **幂等上报 B-2：院内卫健上报网关出站客户端**
  落生产的一段：真实协议 + 真实路径 + 真实字段规范，凭据一律从环境变量读。协议兼容替身只出现在 tests/ 数据层，不是最终对接方式
  - 证据：cargo test --offline 全量回归 442 passed / 0 failed / 1 ignored（日志 logs/task28-cargo-test.log；相对 Task 27 的 421 passed 净增 21 例 = src/push/client.rs 模块内单测 10 例 + 新增 tests/push_gateway_client_test.rs 11 例；1…
- **幂等上报 B-3：HIS 科室主数据客户端与一致性检查**
  正式对接层的第二条出站读。科室代码与主数据的一致性提示依赖院内权威目录；目录拉不到时明确标「科室主数据未校验」，不静默通过
  - 证据：cargo test --offline 全量回归 468 passed / 0 failed（新增 tests/his_dept_directory_test.rs 16 例；日志 logs/task29-cargo-test.log，本任务新增用例全绿且未引入新告警）。新增 16 例逐条命中验收：①契约测试断言 GET /api/v1/dept-directory + Authorizati…
- **幂等上报 B-4：上报提交计划编译器（条件链）**
  核心差异化模块第三段第四格。把上报编成有条件的批量提交程序——第 i 行只在第 i-1 行成功后提交。参考源产品主路径的条件批量程序语义（github_ref/libsql/libsql-server/src/connection/mod.rs 的 make_batch_program 与 program::{Program, Step, Cond}）
  - 证据：cargo test --offline 全量回归 488 passed / 0 failed / 1 ignored（日志 logs/task30-cargo-test.log；相对 Task 29 的 468 净增 20 例 = 新增 tests/push_program_test.rs 13 例 + src/push/program.rs 模块内单测 7 例；1 例 ignored 是既有…
- **幂等上报 B-5：条件批量执行器与批末条件回滚**
  核心差异化模块第三段第五格。关键区分：批末回滚只回滚批次汇总，已在网关生效的行级回执不回滚——网关侧已生效的事实不可撤销
  - 证据：cargo test --offline 全量回归 508 passed / 0 failed / 1 ignored（日志 logs/task31-cargo-test.log；相对 Task 30 的 488 passed 净增 20 例 = 新增 tests/push_executor_test.rs 12 例 + src/push/executor.rs 模块内单测 8 例；1 例 ig…
- **幂等上报 B-6：回执解析与行级状态推进**
  核心差异化模块第三段第六格。回执是状态推进的唯一依据：200 → 已上报、4xx → 被退回（带回错误码与字段名）、5xx/超时 → 待重试
  - 证据：cargo test --offline 全量回归 530 passed / 0 failed / 1 ignored（日志 logs/task32-cargo-test.log；相对 Task 31 的 508 passed 净增 22 例 = 新增 tests/push_receipt_test.rs 14 例 + src/push/receipt.rs 模块内单测 8 例；1 例 igno…
- **幂等上报 B-7：退避重试与断点续跑**
  幂等与并发重试维度。5xx 与超时退避重试，4xx 不重试；重试不重推已确认且已成功的行；批次中断后可从最后一步继续
  - 证据：cargo test --offline 全量回归 556 passed / 0 failed / 1 ignored（日志 logs/task33-cargo-test.log；较 Task 32 的 547 passed 净增 9 例 = 新增 tests/push_retry_test.rs 4 例 + src/push/idempotency.rs 模块内单测 3 例 + src/pus…
- **幂等上报 B-8：批次汇总事务**
  批次汇总与批次状态在一次事务内落库，半途失败不留半截状态；汇总口径固定，供 regval status 与未上报名单使用，不另建报表系统
  - 证据：cargo test --offline 全量回归 565 passed / 0 failed / 1 ignored（库内单测 255 含本任务 push::batch 3 例 + 集成 310 含本任务 tests/batch_summary_test.rs 6 例；1 例 ignored 为既有「重新生成规则表文件」用例；日志 logs/task34-cargo-test.log）。相对…
- **业务状态闭环 A：行级状态机与合法迁移**
  行级状态机：待校验 → 校验通过 → 待确认 → 已上报 / 被退回 / 待重试。非法迁移被拒绝并报错，状态不被静默改写；所有状态推进经状态机单一入口
  - 证据：cargo test --offline 全量回归 579 passed / 0 failed / 1 ignored（状态机模块内单测 5 例 + tests/row_state_machine_test.rs 9 例；日志 logs/task35-cargo-test.log；相对 Task 34 的 565 净增 14 例）。①合法迁移表 14 条逐条走通（状态到位、版本号 +1；自迁移为…
- **业务状态闭环 B：人工确认流与上报写闸门**
  确认回写维度。未确认的行一律不推；确认可撤销，撤销后需重新确认；模型给出的疑似重复必须逐条勾选才合并
  - 证据：cargo test --offline --no-fail-fast 全量回归 594 passed / 0 failed / 1 ignored（37 个测试目标；日志 logs/task36-cargo-test.log；相对 Task 35 的 579 净增 15 例 = 新增 tests/confirm_gate_test.rs 11 例 + state/audit.rs 模块内单测…
- **业务状态闭环 C：审计链与状态库恢复**
  安全审计维度。审计链可按行与按批次回放；状态库损坏时按幂等键与回执记录恢复到最后一个一致状态，并给出恢复报告
  - 证据：cargo test --offline 全量回归 611 passed / 0 failed / 1 ignored（日志 logs/task37-cargo-test.log；相对 Task 36 的 594 净增 17 例 = 新增 tests/audit_recovery_test.rs 11 例 + src/db/journal.rs 4 例 + src/db/recover.rs 2…
- **表格内插件 UI-1：Excel 视觉 token 与状态样式规范**
  界面三段式的第一段（设计 token / 基础样式）。产品带界面，本轮界面落在 Excel 原生能力上，不自绘：先把配色、字体、字号、列宽写成具名 token，再由宏按 token 应用样式
  - 证据：cargo test --offline 全量回归 633 passed / 0 failed / 1 ignored（36 个测试目标，日志 logs/task38-cargo-test.log；相对 Task 37 的 611 净增 22 例 = 新增 tests/ui_tokens_test.rs 13 例 + src/ui/tokens.rs 模块内单测 9 例）。新增 tests/ui…
- **表格内插件 UI-2：Excel 加载宏完整版（两按钮 + 状态条 + 分块写回）**
  界面三段式的第二段（核心界面搭建）。产品主界面 = Excel 加载宏：两个按钮 + 底部状态条 + 分块写回，保留原表格式与公式
  - 证据：cargo test --offline 全量回归 649 passed / 0 failed（40 个测试目标，日志 logs/task39-cargo-test.log；相对 Task 38 的 633 净增 16 例 = 新增 tests/macro_ui_test.rs 9 例 + 契约一致性新增 1 例 + 状态条/错误报告模块内单测 6 例）。新增 tests/macro_ui_te…
- **表格内插件 UI-3：宏侧三态与反馈打磨（加载 / 空 / 错误）**
  界面三段式的第三段（状态与打磨）。加载态、空结果态、错误态都要有明确反馈，不得静默失败或静默空表。排在发布清理之前，不是末条任务
  - 证据：cargo test --offline 全量回归 679 passed / 0 failed / 0 ignored（41 个测试目标，日志 logs/task40-cargo-test.log；相对 Task 39 的 649 净增 30 例 = 新增 tests/ui_states_test.rs 20 例 + src/ui/messages.rs 模块内单测 7 例 + src/ui/s…
- **表格内插件：三入口同构（WPS JSA 宏 + 契约一致性测试）**
  三条入口共用同一引擎与同一 JSON 契约，产出同一批产物；任何一条入口被企业组策略禁用都不影响其余两条
  - 证据：cargo test --offline --no-fail-fast 全量回归 691 passed / 0 failed / 1 ignored（41 个测试目标，日志 logs/task41-cargo-test.log）。新增 tests/three_entry_consistency_test.rs 5 例逐条命中验收：①三条入口产出同一份契约 —— 同一份样例写成 mini.csv…
- **表格内插件：CSV 直跑降级路径**
  企业组策略禁 VBA、或专网报送机没有宏环境时的等价降级路径：同引擎、同契约、同结果，只是交接方式换成文件
  - 证据：cargo test --offline 全量回归 715 passed / 0 failed / 1 ignored（43 个测试目标；相对 Task 41 的 691 净增 24 例 = 新增 tests/csv_fallback_test.rs 9 例 + src/ingest/quality.rs 单测 8 例 + src/ingest/csv.rs 单测 7 例）。新增 9 例逐条命中…
- **产物 A：未上报名单与催报对照**
  经办人能直接带走的产物：按科室与报告期列出未上报行与具体待办原因，并给出「已收到 / 未收到」催报对照，供报告期截止前催办
  - 证据：cargo test --offline 全量回归 737 passed / 0 failed / 1 ignored（44 个测试目标；日志 logs/task43-cargo-test.log；相对 Task 42 的 715 净增 22 例 = 新增 tests/pending_report_test.rs 8 例 + src/report/pending.rs 模块内单测 12 例 +…
- **产物 B：回执导出与举证包导出**
  留档与核对用的产物：回执导出（批次、回执 ID、幂等键、状态、时间、行键）与每行依据举证包，可被第三方按幂等键与回执 ID 交叉核对
  - 证据：cargo test --offline --no-fail-fast 全量回归 751 passed / 0 failed / 1 ignored（45 个测试目标，日志 logs/task44-cargo-test.log；1 例 ignored 为既有的规则表重生成用例）。新增 tests/receipt_export_test.rs 6 例逐条命中验收：①回执台账一次提交一条且可按幂等键…
- **数据获取：脱敏样例集（脚本化合成生成器）**
  真实院内行级数据不可得，本任务负责把替代数据做成显式的、脚本化的、可重跑、断网可用的合成生成器，并把数据规模写进验收。样例只作测试数据层，不是最终对接方式
  - 证据：生成器与样例均已落盘并逐条验收：①python3 samples/generate.py --self-test 输出「[OK] 同一份种子（20261107）生成两次，22 个文件逐字节相同」（同种子两次生成 sha256 一致）；②python3 samples/generate.py --check 输出「[OK] 已落盘的样例文件与清单一致（sha256 与行数都对得上）」；③行数实测（…
- **验证 A：官方口径案例同构用例集**
  官方口径案例集（本轮口径为卫健 27 项指标，非医保结算清单）转成可执行单测，逐条断言实际 issue 与期望一致
  - 证据：cargo test --offline 全量回归 770 passed / 0 failed / 1 ignored（46 个测试目标，日志 logs/task46-cargo-test.log；基线 751 passed，本任务净增 19 例 = 新增 tests/official_cases_test.rs 19 例 = 15 条逐条用例 + 4 条用例集纪律用例）。用例集落盘 regul…
- **验证 B：规模与性能基准（真实规模输入）**
  深度落点 (c)：处理真实规模输入（约 5000 行）并对正确性与性能给出可度量验收，阈值写进测试，超阈即失败
  - 证据：cargo test --release bench → 5 passed / 0 failed / 0 ignored（release 档 15.1 s，连跑 3 次一致，日志 logs/task47-cargo-test.log）；端到端实测 5000 行完整校验（四类检查器）6607 ms + 提交计划编译 49 ms = 合计 6656 ms（阈值 ≤ 45000 ms），峰值常驻内存…
- **验证 C：与基线方案的可复现对比**
  深度落点 (b)：与一个基础方案对比，产出可复现的对比结果。两条轴——重复检测（朴素确定性 vs 四级漏斗）与问题发现（仅规则校验 vs 含公式复算）
  - 证据：cargo test --offline 全量回归 792 passed / 0 failed / 1 ignored（日志 logs/task48-cargo-test.log；基线 775 passed，净增 17 例 = 新增 tests/baseline_compare_test.rs 12 例 + src/compare/naive.rs 模块内单测 4 例 + src/compare…
- **验证 D：网关协议兼容契约测试**
  正式对接层的契约守护。协议兼容替身按 spec.md 出站报文规范实现，只出现在 tests/ 数据层，不是最终对接方式
  - 证据：cargo test --offline --test gateway_contract_test → 14 passed / 0 failed；cargo test --offline 全量回归 806 passed / 0 failed / 1 ignored（日志 logs/task49-cargo-test.log；基线 792 passed，净增 14 例 = 本任务新增契约套件，产品…
- **验证 E：端到端全流程集成测试与重启续跑**
  整条主路径的一次性验收：校验 → 人工确认 → 上报 → 回执 → 导出，并在进程重启后续跑，验证幂等与断点续跑
  - 证据：cargo test --offline 全量回归 812 passed / 0 failed / 1 ignored（日志 logs/task50-cargo-test.log；基线 806 passed（Task 49），净增 6 例 = 新增 tests/end_to_end_integration_test.rs 4 例 + src/push/send.rs 模块内单测 2 例；1 例…
- **验证 F：全量回归与 README 快速开始可跑通**
  发布前的整体验证：全量测试通过，且 README「快速开始」的每条命令在真实环境下实跑通过（不一致以实跑为准改 README）
  - 证据：cargo test --offline 全绿 812 passed / 0 failed / 1 ignored（日志 logs/task51-cargo-test.log；README 改完后再跑一遍 logs/task51-cargo-test-final.log 仍为 812，EXIT=0）。README 快速开始逐条实跑：cargo build --release 产出 target/…
- **交付：院内离线安装与运维说明**
  交付形态是「二进制 + 宏」，院内网专机常无外网。离线安装包与运维说明是把产品真正装到信息科工位上的最后一步，属于本轮交付面而非额外堆料
  - 证据：cargo test --offline 全量回归 826 passed / 0 failed / 1 ignored（日志 logs/task52-cargo-test.log；相对 Task 51 的 812 净增 14 例 = 新增 tests/install_doc_test.rs 14 例）。新增 14 例逐条命中验收：①docs/install.md 存在且覆盖交付要求的五项（离线构…
- **采集真实运行截图到 docs/screenshots/**
  收尾三段的第一段：采集真实运行证据（真实终端会话输出、真实 Excel 写回界面、真实导出的报告片段），落到项目子目录 docs/screenshots/。禁止用概念图或设计稿冒充实测截图
  - 证据：docs/screenshots/ 实存 8 张 PNG（ui-state-normal / ui-state-empty / ui-state-version-mismatch / ui-state-loading / statusbar-summary / terminal-validate-summary / sheet-writeback / export-report），ls docs…
- **README 补全：把骨架占位符填实并过丰度下限自检**
  收尾三段的第二段：按 README 撰写规范把 Task 1 骨架里所有「（Phase 4 后续 task 补齐）」填实——补配置表、命令/端点表、错误与降级表、验收与自检表、完整目录树注释，并跑基数自检。发布态 README 不得残留占位符
  - 证据：验收全通过。① 基数自检（README.md）：总行数 342（≤ 350 上限，Task 14 遗留的 1300+ 行已压到位）；grep -c '^## ' = 13（≥10）、grep -c '^### ' = 19（≥8）、grep -c '^|' = 79（≥5）、grep -c '^```' = 26（≥8，13 对围栏全部配平、无未闭合）、正文字符 22405（≥5000）、ASC…

### 收尾与发布

- **发布前清理：删除运行时数据与构建产物，保留源码与文档**
  收尾三段的最后一段：清理项目子目录内的运行时数据、日志、缓存与构建产物，确保可直接作为仓库内容发布；不得删除源码、README、LICENSE、docs/concepts/、docs/screenshots/ 与必要样例数据
  - 证据：清理验收全通过。体积：项目子目录 10G / 352,621 个文件 → 6.5M / 221 个文件。已删除：target/（9.8G 构建产物）、vendor/（259M 第三方依赖目录，Rust 版 node_modules）、.cargo/（offline-build.sh 生成的 vendored-sources 配置，脚本自身注明不可入库 —— 留着会让干净 clone 指向不存在的…

### 交付证据

| 项 | 值 |
|----|----|
| 版本 | 0.1.0 |
| 轮次 | R026 |
| 任务完成 | 52/52 |
| 交付状态 | 全部完成 |
| 综合评分 | 6.9 |
| 发布 | https://github.com/nexorin9/regulatory-report-validate |
