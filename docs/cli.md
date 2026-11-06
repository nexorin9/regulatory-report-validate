# CLI 与接口参考

> `regval` 的全部子命令、参数、出站接口端点与错误码全表。README 只留速查表（见 README「主命令速查」），本页是完整参考。

- 引擎：`cargo run -- <子命令>`，或用编译产物 `target/release/regval <子命令>`
- 状态库：`--state-db <path>`（默认取环境变量 `REGVAL_STATE_DB`）
- 规则目录：`--rule-dir <path>`（默认取环境变量 `REGVAL_RULE_DIR`）
- 所有凭据只从环境变量读；缺失即以 `CONFIG_MISSING` 退出并点名缺哪几项，不用默认值兜底
- 结构化 JSON 行日志写到 **stderr**（stdout 留给给人读的摘要），门槛由 `REGVAL_LOG_LEVEL` 控制

---

## 全部 CLI 子命令与接口端点

| 命令或路径 | 用途 | 关键参数 |
|------------|------|----------|
| `regval validate` | 读表并产出结果契约 JSON | `--input <path>` `--out <path>` `--state-db <path>` `--expect-schema-version <v>` `--error-out <path>`（出错时把错误码与中文说明写成 JSON，给表格插件读）`--require-model`（模型不可用时不许降级，直接以 `MODEL_UNAVAILABLE` 退出） |
| `regval push` | 提交已确认行到上报网关（发送路径：编译条件批量程序 → 过写闸门 → 逐步上报 → 回执写回本机库；批次中断后重跑同一命令即从断点续跑，已上报的行不重复推送） | `--batch <ref>` `--out <path>`（执行报告 JSON）`--error-out <path>` |
| `regval push --preview` | 上报身份预览：逐行给出幂等键 / 载荷摘要 / 修订号（**只算不发**，不联网、不改状态库） | `--batch <ref>` `--state-db <path>` |
| `regval push --plan` | 编译上报提交计划：把这一批要上报的行编成条件链（第 i 步只在第 i-1 步成功后执行），并给出「已跳过」与「待人工判定」两张清单（**只编译不发**，不联网、不改状态库） | `--batch <ref>` `--out <path>`（`.csv` 出表格 / `.json` 出结构化）`--state-db <path>` |
| `regval status` | 批次汇总与行状态分布（汇总口径四项：已上报 / 被退回 / 待重试 / 问题项数；批末回滚时汇总显示批前值） | `--batch <ref>` `--limit <n>` `--state-db <path>` |
| `regval rules` | 规则清单与装载状态 | `--dir <path>` `--out <path>` `--state-db <path>` |
| `regval rules approve` | 记一条换版确认（质控岗确认新口径生效） | `--version <v>` `--approver <姓名>` `--from-version <v>` `--slot <槽位键>` `--note <说明>` |
| `regval rules approvals` | 查看已有的换版确认记录 | `--state-db <path>` |
| `regval merge` | 应用一条已勾选的疑似重复合并（**逐条勾选**：一条命令对应一对行；并进来的那行不再单独上报，原始数据两行都留着可回溯） | `--batch <ref>`（必填）`--row <保留行键>`（必填）`--drop <并入行键>`（必填）`--by <确认人>`（必填）`--note <理由>` `--state-db <path>` |
| `regval confirm` | 记一条人工确认（复核通过 / 疑似重复逐条勾选）；过了口径校验的行同时推进到「待确认」，没过校验的行只留痕、仍推不出去 | `--row <行键>` `--by <确认人>`（必填）`--with <对方行键>` `--batch <ref>` `--note <理由>` `--state-db <path>` |
| `regval revoke` | 撤销一条人工确认（这一行退回「校验通过」，需重新确认才能再报） | `--row <行键>` `--by <撤销人>`（必填）`--batch <ref>` `--note <理由>` `--state-db <path>` |
| `regval confirmations` | 查看人工确认记录（谁在什么时候确认了哪一行、谁撤销的） | `--batch <ref>` `--state-db <path>` |
| `regval export` | 导出依据举证包 / 未上报名单 / 回执 / 口径差异清单 / 编辑距离候选 | `evidence\|pending\|receipts\|diff\|dupe`（位置参数）`--out <path>` `--batch <ref>` `--from-version <v>` `--to-version <v>` `--state-db <path>` `--contract <结果.json>`（`dupe` 用）`--reminder-out <path>`（`pending` 用：催报对照另写一份文件）`--rule-dir <path>`（`pending` 用：名册文件所在目录） |
| `regval export receipts` | 回执台账（留档与第三方核对用）：**一次提交一条**（订正重报出两条），带批次 / 行键 / 幂等键 / 修订号 / 回执号 / 受理时间 / 错误码与字段名 / 行状态；并自带一份交叉核对结论 | `--out <path>`（`.csv` 出表格，其余出 JSON）`--batch <ref>` `--state-db <path>` |
| `regval replay` | 按原规则版本复算历史批次（结论一致退出 0，不一致退出 1） | `--batch <ref>` `--rule-version <v>` |
| `regval doctor` | 运行环境与凭据逐项自检 | — |
| `regval dept` | 查看院内 HIS 科室主数据目录状态（能不能拉到、多少条、数据时间、用的是实时拉取还是上次缓存） | `--state-db <path>` |
| `regval bench` | 跑重复检测漏斗的标注基准集，出逐级召回率 / 精确率对比表 | `--bench-dir <目录>` `--out <path>`（`.csv` 出表格，其余出 JSON）`--threshold <n>` |
| `regval compare` | 与基线方案做可复现对比（重复检测轴：朴素基线 vs 四级漏斗；问题发现轴：仅规则校验 vs 加分子分母公式复算），出一张对比表 | `--bench-dir <目录>` `--input <表>` `--out <path>`（`.csv` 出表格，其余出 JSON）`--threshold <n>` `--rule-dir <目录>` |
| `regval audit` | 回放审计链（按行看这一行的完整轨迹、按批次看这批推了什么），并核对幂等键 ↔ 回执 ID 的一一对应 | `--row <行键>` `--batch <ref>` `--idempotency` `--state-db <path>` |
| `regval recover` | 状态库损坏时按幂等键与回执记录恢复到最后一个一致状态，并给出恢复报告（恢复到哪一批、丢了什么） | `--state-db <path>` `--out <path>`（恢复报告 JSON）`--apply`（把恢复库换上去，原库先挪成 `.corrupt-N` 备份，**不删**） |
| `regval tokens` | 导出表格内插件的视觉 token（配色 / 字体 / 列宽 / 「问题类型 → 什么颜色」的映射）：`.bas` 出宏用常量模块，其余出 JSON 清单 | `--out <path>` `--vba`（强制出宏用常量模块，缺省按扩展名判断） |
| `regval messages` | 导出表格内插件的提示文案常量表（加载态 / 空结果态 / 错误态 / 对话框的话术 + 每个错误码的中文说明）：`.bas` 出宏用常量模块，其余出 JSON 清单 | `--out <path>` `--vba`（强制出宏用常量模块，缺省按扩展名判断） |
| `regval macro` | 装配 WPS JSA 宏：手写模板 + 引擎侧的契约常量 / 视觉 token / 提示文案 → 一份可直接加载的宏（改了模板、配色或文案后跑一次） | `--out <path>`（默认 `macro/wps/regval.js`）`--template <path>`（默认 `macro/wps/regval.template.js`） |
| `POST {REGULATORY_REPORT_BASE_URL}/api/v1/regulatory-submissions` | 出站写：提交一行上报 | 头 `Authorization: Bearer`、`Idempotency-Key`；body 15 字段 |
| `GET {HIS_BASE_URL}/api/v1/dept-directory` | 出站读：科室主数据目录 | 头 `Authorization: Bearer`；响应 `[{dept_code, dept_name, aliases[], parent_code}]` |

## 错误码全表与降级处置

| 场景 | 表现 | 处理方式 |
|------|------|----------|
| `CONTRACT_VERSION_MISMATCH` | 引擎与表格宏的契约版本对不上 | 宏提示「引擎与宏版本不一致，请升级」并中止写回，不做部分写回 |
| `RULE_SYNTAX_ERROR` | 口径规则 SQL 语法错 | 报出出错词与偏移，该条规则标为不生效，其余规则照常执行 |
| `RULE_SWITCH_UNCONFIRMED` | 口径目录里出现了没确认过的新版本 | 本轮继续用旧版并逐条说明哪一版没生效；由质控岗确认后重跑 |
| `RULE_EFFECTIVE_DATE_NOT_REACHED` | 新版本的生效日期还没到 | 该版本本轮不启用，继续用旧版 |
| `RULE_VERSION_NOT_FOUND` | 规则集里没有该版本的口径规则 | 确认一个不存在的版本等于没确认，先把规则表放进目录 |
| `MISSING_COLUMN` | 表头缺必需列 | 报明确缺哪一列，不猜列、不静默跳过该规则 |
| `INPUT_UNREADABLE` | CSV 读不出来（文件损坏、某一行既不是 UTF-8 也不是 GBK、UTF-16 编码） | 报出**第几行**解不出来；把那一行另存为 UTF-8 或 GBK 后重试。整表 UTF-16 会明说「本轮只支持 UTF-8 与 GBK」，不做乱码替换 |
| `UNSUPPORTED_FORMAT` | 输入不是 `.csv` / `.xlsx` / `.xlsm` | 明说本轮支持哪三种格式；先把表另存为 xlsx 或 csv |
| `DENOMINATOR_ZERO` | 比率型指标分母为 0 | 单独归类，不报成「公式不符」，不做静默 0 处理 |
| `INVALID_NUMERIC` | 分子/分母是负数或非数值 | 归入数据形态错误，与「公式不符」区分 |
| `IDEMPOTENCY_MISSING` | 上报行算不出幂等键 | 请求发出前即拦截，不发半截请求 |
| `INVALID_STATE_TRANSITION` | 行级状态迁移不在合法迁移表里（如「待校验」直接跳到「已上报」） | 拒绝推进并报错，**行状态一个字节都不改**；按报错里的两个状态看这一行该从哪来 |
| `ROW_STATE_CONFLICT` | 同一行刚被另一个动作推进过（行级乐观锁） | 后写者被拒，不覆盖先写者；重跑当前动作即可，不要手工改状态 |
| `UNCONFIRMED_ROW` | 这一行没过写闸门：没有人工确认记录、确认被撤销过，或行状态还不在可推送状态 | 写闸门**不往网关发这个请求**（一个字节都不发），该行留在原状态；补确认或改完数据重新校验后再报 |
| `CONFIRM_ACTOR_MISSING` | 确认 / 撤销 / 合并时没给确认人 | 空名不入库（没有人的确认等于没人负责）；补上 `--by <姓名>` 再跑 |
| `GATEWAY_REJECTED` | 网关 4xx 退回 | 该行置「被退回」，错误码与字段名写回原表说明列，不重试 |
| `GATEWAY_SERVER_ERROR` | 网关返回服务端错误（HTTP 5xx） | 退避重试；重试耗尽转「待重试」，**不改成已上报** |
| `GATEWAY_TIMEOUT` | 网关调用超时 | 调大 `REGULATORY_REPORT_TIMEOUT_SECS` 或排查院内网段；超时不算「已上报」 |
| `GATEWAY_CONNECTION_FAILED` | 网关连不上（连接被拒 / DNS / 网段不通） | 本行转「待重试」，保留幂等键，续跑时用同一个 `Idempotency-Key` |
| `GATEWAY_RESPONSE_INVALID` | 网关回了 200 但回执结构不合法 | **不当成成功**；该行标「回执待人工核对」，人工核对后再处理 |
| `RECEIPT_CONFLICT` | 同一幂等键落了两条回执，或同一回执号挂了两个幂等键 | 已有回执**不覆盖**；先查清重复回执的来源再重跑，别直接改库 |
| `RECEIPT_LEDGER_MISMATCH` | 导出回执台账时交叉核对不通过（已上报却找不到回执 / 有回执找不到提交记录 / 上报状态与行状态对不上） | 台账**仍然写出来**供逐条查，但退出码非 0 —— 这份台账先别归档；按台账里列出的冲突逐条查清再重新导出 |
| `BATCH_SUMMARY_CONFLICT` | 同一批次的同一版汇总被写第二次（重放） | 本次事务**整笔回滚**，汇总回到批前值；确认不是重复触发后再重跑 |
| `DEPT_NOT_FOUND` | 科室代码在 HIS 目录匹配不到 | 报「科室主数据不一致」（阻断级），不自动改数 |
| `DEPT_NAME_MISMATCH` | 科室名称写法与主数据或别名不符 | 只提示、不阻断、不改数；核对后自行修改，或把该写法报给信息科登记为别名 |
| `DEPT_DIRECTORY_UNAVAILABLE` | 科室主数据目录整体拉不到 | 本轮跳过科室维度检查，结果标「科室主数据未校验」，**不静默通过**；配了 HIS 变量却没配全也算这一类 |
| `DEPT_DIRECTORY_CONNECTION_FAILED` | 科室目录连不上（连接被拒 / DNS / 网段不通） | 退回上次缓存并标注数据时间；缓存也没有才标「科室主数据未校验」 |
| `DEPT_DIRECTORY_TIMEOUT` | 科室目录调用超时 | 调大 `HIS_TIMEOUT_SECS` 或排查院内网段；超时不算「查过了」 |
| `DEPT_DIRECTORY_BAD_REQUEST` | 科室目录拒绝了请求（HTTP 4xx） | 按错误里的端点原话核对接口与凭据权限 |
| `DEPT_DIRECTORY_SERVER_ERROR` | 科室目录返回服务端错误（HTTP 5xx） | 退回上次缓存并标注数据时间；缓存超有效期会标「已超过有效期」 |
| `DEPT_DIRECTORY_RESPONSE_INVALID` | 科室目录响应结构不合法（不是数组 / 条目缺科室代码） | 找信息科核对接口实现；**空目录不当成「没有科室」** —— 那会把每一行都误报成科室主数据不一致 |
| `MODEL_UNAVAILABLE` | 模型端点不可达/超时/返回不合法 | 默认降级：只给确定性候选并一律标「疑似」，主路径不阻塞；加 `--require-model` 时直接以本错误码退出 |
| `MODEL_CONNECTION_FAILED` | 模型端点连不上（连接被拒 / DNS / 网段不通） | 确认端点地址可达；主路径仍可跑，语义归并本轮为空 |
| `MODEL_TIMEOUT` | 模型端点调用超时 | 调大 `OPENAI_TIMEOUT_SECS` 或减少单次候选对数；超时不算「查过了」 |
| `MODEL_BAD_REQUEST` | 模型端点拒绝请求（HTTP 4xx） | 错误里带端点原话，按原话改请求或换兼容端点 |
| `MODEL_SERVER_ERROR` | 模型端点返回服务端错误（HTTP 5xx） | 端点侧临时故障，稍后重试；重试仍失败则本轮只留确定性候选 |
| `MODEL_RESPONSE_INVALID` | 模型响应没通过结构校验（不是 chat/completions 结构 / 内容不是 JSON / 字段不合法 / 行键没问过） | 整次调用作废，**不写任何业务表**；本轮只留确定性候选 |
| `CONFIG_MISSING` | 必需的环境变量没设或填了空 | 点名缺哪几项并给出补法；**不用默认值兜底**，凭据缺失不带着假地址去连院内系统 |
| `CONFIG_INVALID` | 环境变量取值格式不对（如超时不是整数） | 报错指出是哪一个变量，不猜、不替换 |
| `DB_CORRUPTED` | 本机状态库文件损坏（掉电 / 被截断 / 被别的程序写坏） | **不静默重建空库**（重建会把已确认、已上报的记录悄悄抹掉）；跑 `regval recover` 按幂等键与回执记录恢复，恢复产物写 `<状态库>.recovered`，确认后再 `--apply` 换上去（原库留成 `.corrupt-N` 备份） |
| `CAPABILITY_NOT_READY` | 该子命令依赖的能力尚未落地 | 明确说明缺哪项能力，不静默退出、不假装成功 |

## 对接现有系统

**上报网关（出站写）**

- 路径：`POST {REGULATORY_REPORT_BASE_URL}/api/v1/regulatory-submissions`
- 凭据：`Authorization: Bearer $REGULATORY_REPORT_TOKEN`（环境变量），随请求带 `Idempotency-Key` 头
- body 字段全集（固定 15 项，不得少传多传）：`batch_ref` / `reporting_period` / `org_code` / `indicator_code` / `dept_code` / `business_key` / `numerator` / `denominator` / `value` / `unit` / `basis` / `rule_version` / `idempotency_key` / `payload_digest` / `revision`
- 响应语义：`200 {receipt_id, accepted_at, status}` → 已上报；`4xx {error_code, error_field, message}` → 被退回（带错误码与字段名）；`5xx` 与超时 → 退避重试，重试耗尽转「待重试」；`200` 但回执缺字段 → 不当成成功，该行标「回执待人工核对」
- 网关超时：环境变量 `REGULATORY_REPORT_TIMEOUT_SECS`（缺省 30 秒）；写坏了报 `CONFIG_INVALID`，不静默回落默认值

**HIS 科室目录（出站读）**

- 路径：`GET {HIS_BASE_URL}/api/v1/dept-directory`，`Authorization: Bearer $HIS_API_TOKEN`
- 响应：`[{dept_code, dept_name, aliases[], parent_code}]` —— 科室代码是权威标识，名称与别名是合法写法
- 用途：核对科室代码与院内主数据是否一致（查不到 → 报「科室主数据不一致」，阻断级）；科室名称写法与主数据或别名不符 → 只提示、不阻断、不改数
- 拉不到时降级为「科室主数据未校验」并在结果中标出，不静默通过；**空目录不当成「没有科室」**（那会把每一行都误报成科室主数据不一致）
- 缓存：拉取结果按机构代码 + 拉取时间落本机库；拉取失败时退回上次缓存并**标注数据时间**，超过有效期（`REGVAL_DEPT_DIRECTORY_TTL_SECS`，缺省 1 小时）会标「已超过有效期」
- 看目录状态：`regval dept` —— 能不能拉到、多少条、数据时间、用的是实时拉取还是上次缓存

**验证方式**：协议兼容替身只出现在 `tests/` 测试数据层，用于断言请求路径、请求头与 body 字段全集；它**不是**最终对接方式，产品代码走的是上面的正式协议。同一套协议另有一个独立进程版（`tests/gateway_stub/stub_server.py`，只用 Python 标准库），供没有院内网关的环境离线跑一遍完整上报路径 —— 见「快速开始 → 上报 → 导出 → 状态：完整跑一遍」。