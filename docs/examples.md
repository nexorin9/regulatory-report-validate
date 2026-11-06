# 输出样例与真实运行记录

> 本文是 `regval` 各子命令产物的**完整真实输出**（脱敏样例集，固定种子可重跑）。README 只留最能说明问题的片段，完整样例看这里。

- 样例集来源：`samples/generate.py`（脚本化合成，固定种子可重跑）、`tests/fixtures/`（最小主干样例）
- 里面的行数、问题数、批次号都是真实运行的产物，不是手写的示意值

---

## 快速开始里的真实终端输出

### 一次最小校验的完整 stdout

跑通后终端给出下面这段（**逐行真实输出**，只有「耗时」一行随机器而变）：

跑通后终端给出下面这段（**逐行真实输出**，只有「耗时」一行随机器而变）：

```text
批次汇总：已上报 0 行 · 被退回 0 行 · 待重试 0 行 · 问题 2 项
已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认 → /tmp/regval-result.json
涉及 2 行有问题
行状态：6 行 · 推进 4 · 未变 2 · 已上报受保护 0 · 退回修正回待确认 0 · 复校验退回 0
行状态分布：待校验 2 行 · 校验通过 4 行
重复检测漏斗：确定性键 0 对 · 归一化候选 0 对（未归一 2 行 · 与确定性键重叠 0 对 · 数值不同未收 0 对）· 编辑距离候选 0 对（阈值 2 · 超出阈值 0 对 · 长度差过大跳过 0 对）
口径规则集：版本 2021.1 · 生效 11 条 · 指纹 8125c3aaa14219ba
模型端点：本轮未接入（疑似项来源为确定性候选）
语义归并：模型提出 0 条 · 可合并候选 0 对 · 置信度不足待人工判定 0 对
批次 mini-941b47da51bd · 本机库 ./.regval/state.db · 耗时 32 ms
科室主数据未校验（未配置院内 HIS 科室目录（HIS_BASE_URL / HIS_API_TOKEN））
```

其中**第二行就是宏的状态条那句话**（宏直接取契约 `summary.status_line`，不自己统计 ——
表里那句与终端这一行是同一句）。

配了 `HIS_BASE_URL` / `HIS_API_TOKEN` 时，最后一行换成核对结果（拉不到就退回上次缓存并标出数据时间）：

```text
科室主数据已校验 · 目录 8 条 · 数据时间 2026-11-07 06:01:22（实时拉取）
```

### 一次完整上报（5 行全部被网关受理）

第 3 步的真实输出（每行一条回执，回执号来自网关）：

```text
上报发送 · 批次 clean-7867bdb11d18
提交计划：待上报 5 步 · 已跳过 0 行 · 待人工判定 0 行
第 1 步  行键 413cc083…  已执行  已上报 · 回执 RCPT-0001 · 受理时间 2026-11-07 06:01:03 · accepted
第 2 步  行键 4e0dff41…  已执行  已上报 · 回执 RCPT-0002 · 受理时间 2026-11-07 06:01:03 · accepted
第 3 步  行键 4f1aa8d3…  已执行  已上报 · 回执 RCPT-0003 · 受理时间 2026-11-07 06:01:03 · accepted
第 4 步  行键 94901f1c…  已执行  已上报 · 回执 RCPT-0004 · 受理时间 2026-11-07 06:01:03 · accepted
第 5 步  行键 ac53cd16…  已执行  已上报 · 回执 RCPT-0005 · 受理时间 2026-11-07 06:01:03 · accepted
本轮：计划 5 步 · 尝试 5 步 · 已上报 5 行（受理 5 · 幂等命中 0）· 被退回 0 行 · 待重试 0 行 · 未发出 0 行 · 回执待核对 0 行 · 未执行 0 行 · 续跑跳过 0 行
回执落库：行 5 · 回执 5 · 已上报 5 · 被退回 0 · 待重试 0 · 回执待核对 0 · 未发出 0
批次汇总：已上报 5 行 · 被退回 0 行 · 待重试 0 行 · 问题 0 项
```

### 再推一次：幂等命中，一个请求都不再发

**再推一次不会多发一个请求**（幂等键命中）：

```text
上报发送 · 批次 clean-7867bdb11d18
提交计划：待上报 0 步 · 已跳过 5 行 · 待人工判定 0 行
本批没有待上报的行 —— 已上报的行不重复推送（幂等命中）。
已跳过  行键 413cc083…  指标 MER-HR-01  科室 0301  业务主键 BK-CLEAN-05  幂等命中（载荷未变，不重复推送）（幂等键 87484f77… 的载荷摘要与已提交的那一版一致（修订号 1），不重复推送）
```

> **上报是串行的**：第 k 步没受理，第 k+1 步起**都不执行**（批末条件回滚，批次汇总回到批前值，
> 但已成功行的行级回执保留 —— 网关侧已受理的事实不可撤销）。
> 一行没过校验又没人工确认，会被写闸门以 `UNCONFIRMED_ROW` 拦下（**一个字节都没往网关发**），
> 串行链就停在这一步 —— 先把那行修好或确认掉，再重跑同一条 `push` 命令即可续跑。

---


## 各类产物样例

### 校验结果契约 JSON（`regval validate` 的产物）

下面是真实运行（`tests/fixtures/mini.csv`，6 行脱敏样例）产出的契约，已截取前 2 行与首条问题：

```json
{
  "schema_version": "regval.contract/1.0.0",
  "generated_at": "2026-11-07 06:00:09",
  "batch_ref": "mini-941b47da51bd",
  "columns": [
    { "name": "校验结论", "index": 1, "width": 10.0 },
    { "name": "问题类型", "index": 2, "width": 18.0 },
    { "name": "问题说明（含依据）", "index": 3, "width": 48.0 },
    { "name": "疑似重复项", "index": 4, "width": 20.0 },
    { "name": "上报状态", "index": 5, "width": 12.0 }
  ],
  "summary": {
    "tables": 1, "total_rows": 6, "rows_with_issues": 2, "issue_count": 2,
    "issue_count_by_kind": { "必填缺失": 2 }, "suspected_duplicate_count": 0,
    "elapsed_ms": 32,
    "dept_master_checked": false,
    "dept_master_note": "科室主数据未校验",
    "status_line": "已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认",
    "submission_line": "本批 0 行已提交 · 幂等键见状态库 · 回执已回填",
    "submitted_rows": 0,
    "source_rows": 6, "blank_rows": 0, "invalid_rows": 0,
    "skipped_rows": 0, "skipped_note": ""
  },
  "rows": [
    {
      "row_uid": "ba4e8a37d508ca92f947ee6e3f342c2e81a6229b081db368eced1699b1dec074",
      "source_ns": "mini", "source_file": "tests/fixtures/mini.csv", "row_no": 1,
      "indicator_code": "MER-TL-04", "dept_code": "0301",
      "reporting_period": "2026-Q3", "business_key": "BK-SAMPLE-01",
      "conclusion": "通过", "issue_types": "", "issue_detail": "",
      "suspected_duplicate": "", "submission_status": "未上报"
    }
  ],
  "issues": [
    {
      "row_uid": "31af105cfed5f78ac6f2528debdeff4f875db9b76f844560975e680cfc4877da",
      "rule_id": "R-REQUIRED-001", "issue_kind": "必填缺失", "severity": "阻断",
      "message": "缺少必填列「指标名称」",
      "basis": "国卫办医函〔2021〕28号 附件《病案管理质量控制指标（2021年版）》（27 项指标编号与名称）；科室代码 / 报告期 / 业务主键为本产品行标识字段（最小数据格式与字段要求）",
      "missing_columns": ["指标名称"],
      "error_code": ""
    }
  ],
  "suspected_duplicates": []
}
```

> 字段全集与 `schema_version` 由引擎与表格插件共同锁定：插件读到对不上的 `schema_version`
> 一律拒绝写回（`CONTRACT_VERSION_MISMATCH`），不做部分写回。
>
> `summary.status_line` / `summary.submission_line` 是**成品句子**：表格插件的底部状态条与
> 终端第一行输出打的是同一句（引擎算，插件只显示）—— 两边不会各说一套。

### 读表记录与丢行计数（`ingest` 段）

这段回答两个问题：**这张表是什么格式**、**表里的行一共有多少、少了哪几行**。

```json
{
  "ingest": {
    "source_rows": 8,
    "valid_rows": 5,
    "blank_rows": 1,
    "invalid_rows": 2,
    "skipped_rows": 3,
    "skipped_detail": [
      { "source_ns": "csv_dirty", "row_no": 5, "reason": "空行" },
      { "source_ns": "csv_dirty", "row_no": 6, "reason": "重复表头" },
      { "source_ns": "csv_dirty", "row_no": 7, "reason": "分隔线" }
    ],
    "encoding": "utf-8",
    "delimiter": "半角逗号",
    "format_note": "",
    "skipped_note": "跳过 3 行：空行 1 行 · 无效行 2 行（已计数，未参与校验）"
  }
}
```

| 字段 | 说的是什么 |
|------|-----------|
| `source_rows` | 读到的数据行总数（**含**空行与无效行） |
| `valid_rows` / `total_rows` | 真正参与校验的行数 |
| `blank_rows` / `invalid_rows` | 空行数 / 完全无效行数（重复表头、分隔线、落不上必填业务列的杂行） |
| `skipped_detail` | 每一行被跳过的**行号 + 原因** —— 照着行号能回原表找 |
| `encoding` | 这张表实际是什么编码：`utf-8` / `utf-8-bom` / `gbk` / `mixed`（UTF-8 与 GBK 混编）/ `xlsx（工作簿原生）` |
| `delimiter` | 分隔符：`半角逗号` / `全角逗号（已归一为半角）` / `xlsx（单元格原生）` |

> **跳过的行不参与校验，但绝不静默丢**：计数进 `summary` 段（`source_rows` /
> `blank_rows` / `invalid_rows` / `skipped_rows` / `skipped_note`），明细进
> `ingest` 段，终端也会照实打一行 —— 表里 8 行、校验了 5 行，另外 3 行是什么、
> 为什么不算，看得清清楚楚。
>
> **判定口径只有一处**：CSV 直跑与 Excel/WPS 走同一段判定代码，
> 同一张表从哪条入口进去，跳过了几行、行号对不对得上都一样。

### 科室主数据目录状态（`dept_directory` 段）

这段说明**这批结论的科室维度到底查没查过、用的是哪一份主数据**。核对过：

```json
{
  "dept_directory": {
    "checked": true, "source": "live", "source_zh": "实时拉取",
    "entry_count": 8, "fetched_at": "2026-11-07 06:01:22",
    "ttl_secs": 3600, "stale": false, "error_code": "",
    "note": "科室主数据已校验 · 目录 8 条 · 数据时间 2026-11-07 06:01:22（实时拉取）"
  }
}
```

没配 HIS 目录、或目录拉不到且缓存也没有 —— **`checked` 为 `false`**，
`summary.dept_master_note` 是「科室主数据未校验」，`dept_directory.note` 里带着原因：

```json
{
  "checked": false, "source": "unavailable", "source_zh": "不可用",
  "entry_count": 0, "fetched_at": "", "ttl_secs": 3600, "stale": false,
  "error_code": "DEPT_DIRECTORY_CONNECTION_FAILED",
  "note": "科室主数据未校验（目录拉取失败：DEPT_DIRECTORY_CONNECTION_FAILED · HIS 科室目录连接失败：error sending request for url (http://127.0.0.1:8799/api/v1/dept-directory)）"
}
```

拉取失败但有上次缓存时，用的是缓存那份，`source` 为 `cache`、`fetched_at` 是
**那份主数据的时间**（不是本轮时间），超过有效期会标 `stale: true`。
`regval dept` 看的就是这一段：

```text
科室主数据目录：上次缓存
科室主数据已校验（目录拉取失败：DEPT_DIRECTORY_CONNECTION_FAILED，本轮用上次缓存）· 目录 8 条 · 数据时间 2026-11-07 06:01:22
目录条目：8 条
  · 0301 病案统计室（别名：病案统计）
  · 0302 病案室（别名：病案科）
  · 0303 医技部（别名：医技科）
错误码：DEPT_DIRECTORY_CONNECTION_FAILED
```

> 上面的「8 条」是随包附带的协议替身里那 8 个示意科室；院内真实目录有多少条，
> 终端就显示多少条 —— 这一段只说明**格式与口径**，条数随院内主数据而变。

### 疑似重复候选对（`suspected_duplicates`）

重复检测按四级漏斗逐级判（确定性键 → 归一化 → 编辑距离 → 模型语义归并），
每一级只产出**候选对**，一律标「疑似重复」——**不合并、不推送**，
合并与否由经办人逐条勾选确认。真实运行（`tests/fixtures/dupe_exact.csv`）产出的一对：

```json
{
  "left_row_key": "2a3bd5604621fc6c635d7a752caa73cc9c93b13a111bce382d391402641d702e",
  "right_row_key": "b0e6ee0bb77dab01fb9e94f4f144b7c3530f6edeed85faf3fd13e2ee6c71cee6",
  "match_level": "exact",
  "basis": "本产品最小数据格式与字段要求（确定性键 = 指标编号 + 科室代码 + 报告期 + 业务主键）；同一确定性键下取值完全一致（分子 / 分母 / 填报值 / 计量单位等全部一致，单位按单位别名表归一后比较）的行标「疑似重复」，由经办人逐条勾选确认后才合并，本轮不合并、不上报",
  "business_key": "MER-TL-04|0301|2026-Q3|BK-DUPE-01",
  "left_reference": "dupe_exact 第 1 行",
  "right_reference": "dupe_exact 第 2 行",
  "cross_table": false,
  "state": "疑似重复"
}
```

对应的两行在原表「疑似重复项」列里各标出对方：

```text
疑似重复：与 dupe_exact 第 2 行（行键 b0e6ee0b） 取值完全一致
```

同一份表里**同键但取值不同**的两行不进这段，而是由实体绑定层标「待人工判定」
（问题类型列写「待人工判定」），引擎不替业务二选一。

第四级（模型语义归并）的候选对多带四个字段 —— 归并类型、置信度、理由、依据，
经办人逐条看的时候不用再去翻模型原始输出：

```json
{
  "match_level": "semantic",
  "source": "semantic",
  "state": "疑似重复",
  "mergeable": true,
  "semantic_kind": "same_indicator",
  "semantic_confidence": 0.92,
  "semantic_reason": "两处指标名指的都是主要诊断填写正确率，只是写法不同",
  "semantic_evidence": ["左侧指标名含『主要诊断填写正确率』", "右侧指标名含『主要诊断填写正确率』"]
}
```

置信度低于阈值（`REGVAL_MODEL_MIN_CONFIDENCE`，缺省 0.80）时，这一对**照样写进
契约**，但状态改成「疑似待人工判定」、`mergeable` 为 `false` —— 模型自己都不确定的
结论不进可合并候选，留给人逐条看。

### 编辑距离候选与距离分布直方（`dupe_distance` 段）

漏斗第三级用**字符级编辑距离**判「归一没吸收掉的写法差异，像不像同一件事」
（中文按字算，不按字节 —— 一个汉字算 1，不算 3）。真实运行
（`tests/fixtures/dupe_distance.csv`，15 行脱敏样例）的终端摘要：

```text
重复检测漏斗：确定性键 1 对 · 归一化候选 1 对（未归一 1 行 · 与确定性键重叠 1 对 · 数值不同未收 0 对）· 编辑距离候选 2 对（阈值 2 · 超出阈值 1 对 · 长度差过大跳过 1 对）
距离分布：距离 1 1 对 · 距离 2 1 对 · 超过阈值 1 对
```

结果契约里对应的一段（阈值来源、距离分布直方、逐项计数）：

```json
{
  "threshold": 2,
  "threshold_source": "内置默认值（阈值 2 · 长度差上限 40）",
  "max_length_gap": 40,
  "pairs": 2,
  "histogram": [
    { "label": "距离 0", "distance": 0, "pairs": 0 },
    { "label": "距离 1", "distance": 1, "pairs": 1 },
    { "label": "距离 2", "distance": 2, "pairs": 1 },
    { "label": "超过阈值", "distance": null, "pairs": 1 }
  ],
  "stats": {
    "eligible_rows": 14, "skipped_rows": 0, "skipped_empty_name_rows": 1,
    "groups": 7, "compared_pairs": 3, "pairs": 2,
    "beyond_threshold_pairs": 1, "skipped_long_gap_pairs": 1,
    "exact_overlap_pairs": 1, "normalize_overlap_pairs": 1, "value_conflict_pairs": 1
  }
}
```

候选对里带**距离值**与**哪几个字不同**的说明，经办人一眼能看出像在哪：

```json
{
  "match_level": "distance",
  "business_key": "MER-TL-04|0301|2026-Q3|BK-DIST-01",
  "left_reference": "dupe_distance 第 2 行",
  "right_reference": "dupe_distance 第 1 行",
  "left_original": "指标名称=病案统计室",
  "right_original": "指标名称=病案室",
  "distance": 2,
  "distance_detail": "左行多「统计」",
  "state": "疑似重复"
}
```

对应的两行在原表「疑似重复项」列里各标出对方：

```text
疑似重复：与 dupe_distance 第 1 行（行键 36816bdb）（字符级编辑距离 2：左行多「统计」）
```

导出可核对产物（候选对一行一对 + 距离分布直方）：

```bash
regval export dupe --contract 结果.json --out 编辑距离候选.csv   # 候选对表
regval export dupe --contract 结果.json --out 编辑距离候选.json   # 阈值 + 直方 + 候选对
```

### 批次汇总（`regval status`）

批次汇总回答「这批整体算不算数」：口径固定四项（已上报 / 被退回 / 待重试 / 问题项数），
供 `regval status` 与未上报名单共用，**不另建报表系统**。批次计数、汇总值与批次状态
在一次事务里落库 —— 中途失败不留半截状态，快照回到批前值。

```text
状态库：/tmp/regval-quickstart.db
2026-11-07 06:00:57 · validate · 表 1 张 · 行 5 · 问题 0 项 · 耗时 33 ms · clean-7867bdb11d18
批次汇总：
  clean-7867bdb11d18 · 状态 上报完成 · 修订 2 · 已上报 5 行 · 被退回 0 行 · 待重试 0 行 · 问题 0 项
模型调用审计：暂无记录（本轮还没有调用过模型端点）
行状态：已上报 5 行
```

`regval status` 末尾那行「行状态」是**行级状态机的分布**：每一行现在到哪一步了
（待校验 / 校验通过 / 待确认 / 已上报 / 被退回 / 待重试）。普通校验不会把已上报的行改回
待校验；被退回的行修正后回到待确认（需重新确认）。状态推进只走一个入口，
不在合法迁移表里的推进会被拒绝并报错 —— 完整说明见 `docs/architecture.md` 第 6 节。

| 口径 | 定义 | 出处 |
|------|------|------|
| 已上报行数 | 网关已受理的行数（本次受理 + 幂等命中） | 本机库行级上报状态 |
| 被退回行数 | 网关退回（4xx）的行数 | 本机库行级上报状态 |
| 待重试行数 | 可重试失败（5xx / 超时 / 连不上）的行数 | 本机库行级上报状态 |
| 问题项数 | 该批次问题条数（口径问题 + 上报退回问题） | 本机库问题表 |

批末条件回滚时，**汇总回到批前值**（批次状态记「批末回滚」），而**行级回执保留** ——
网关侧已受理的事实不可撤销。口径与事务口径的完整说明见 `docs/architecture.md`。

### 上报发送（`regval push` 的终端输出）

发送路径跑的是**条件批量提交程序**：第 i 步只在第 i-1 步成功后执行；
每一步的裁决逐行打印，回执落进本机库，「上报状态」列随之回填。
下面是一次真实运行（5 行全部被网关受理）的输出：

```text
上报发送 · 批次 clean-7867bdb11d18
提交计划：待上报 5 步 · 已跳过 0 行 · 待人工判定 0 行
第 1 步  行键 413cc083b659102806157b436588cf22988f8ee353d7f0237f05c2fa56658f1d  已执行  已上报 · 回执 RCPT-0001 · 受理时间 2026-11-07 06:01:03 · accepted
第 2 步  行键 4e0dff41d364e85202f1906fc7ca509988508aa5631c92ad2adecfa5acfccf5f  已执行  已上报 · 回执 RCPT-0002 · 受理时间 2026-11-07 06:01:03 · accepted
第 3 步  行键 4f1aa8d37af305e6bbf1886dcfdf7497ce27d449323fd2aa724f8d31901d81eb  已执行  已上报 · 回执 RCPT-0003 · 受理时间 2026-11-07 06:01:03 · accepted
第 4 步  行键 94901f1cc5c8500c7bec673fc078d272813625a75bac5770eec199e279d4033b  已执行  已上报 · 回执 RCPT-0004 · 受理时间 2026-11-07 06:01:03 · accepted
第 5 步  行键 ac53cd16e8bf7faa449c8c05263d100a0433a7d3fbf73e836be1ab5e85ba6037  已执行  已上报 · 回执 RCPT-0005 · 受理时间 2026-11-07 06:01:03 · accepted
本轮：计划 5 步 · 尝试 5 步 · 已上报 5 行（受理 5 · 幂等命中 0）· 被退回 0 行 · 待重试 0 行 · 未发出 0 行 · 回执待核对 0 行 · 未执行 0 行 · 续跑跳过 0 行
回执落库：行 5 · 回执 5 · 已上报 5 · 被退回 0 · 待重试 0 · 回执待核对 0 · 未发出 0
批次汇总：已上报 5 行 · 被退回 0 行 · 待重试 0 行 · 问题 0 项
```

同一个批次**再跑一次**，一行请求都不会再发 —— 载荷摘要没变就是重复提交：

```text
上报发送 · 批次 clean-7867bdb11d18
提交计划：待上报 0 步 · 已跳过 5 行 · 待人工判定 0 行
本批没有待上报的行 —— 已上报的行不重复推送（幂等命中）。
已跳过  行键 413cc083b659102806157b436588cf22988f8ee353d7f0237f05c2fa56658f1d  指标 MER-HR-01  科室 0301  业务主键 BK-CLEAN-05  幂等命中（载荷未变，不重复推送）（幂等键 87484f77723a5a71a2e792f872e71667456c714b2331697bb9575cfd17704d8f 的载荷摘要与已提交的那一版一致（修订号 1），不重复推送）
```

批次中途失败（网关 5xx / 超时）时，后面几行**不执行**、批次汇总**回到批前值**，
但已经拿到回执的行**保留**（网关侧已受理的事实不可撤销）。
修好之后**重跑同一条 `regval push` 即可续跑**：续跑点由本机库里的回执算出来，
已成功的行一个请求都不再发。失败分类与处置见下方「错误与降级」。

### 未上报名单（`regval export pending`）

还剩哪些行没报上去，逐行带**具体原因**（问题类型 + 规则编号 + 缺失列 + 错误码，或「未确认标记」），不写「待处理」这类没信息量的话。已上报的行不在表里 —— 已上报只由网关回执说了算。

```csv
来源表,行号,行键,指标编号,科室代码,科室名称,报告期,业务主键,行状态,待办原因,原因来源,原因编号,原因明细,错误码,缺失列,问题条数,已确认
pending_report,1,5a8a76becc75b4f3…,MER-TL-04,0301,病案统计室,2026-Q3,BK-PEND-01,校验通过,待确认,人工确认,,口径校验通过，等待经办人勾选确认（未确认标记）,,,0,否
pending_report,2,f405ec582375cf87…,MER-TQ-01,0301,病案统计室,2026Q3,BK-PEND-02,待校验,有问题待修,口径校验,R-ENUM-001,口径问题 1 项：枚举越界（规则 R-ENUM-001）,,,1,否
pending_report,5,6a252956c1789d7a…,MER-TQ-09,0302,病案室,2026-Q3,BK-PEND-05,待校验,有问题待修,口径校验,R-REQUIRED-001,口径问题 1 项：必填缺失（规则 R-REQUIRED-001，缺失列：指标名称）,,指标名称,1,否
```

> 行键是 64 位哈希，示例里截短；文件里是完整值，可与回执导出逐条对上。

催报对照（同一份数据里按报告期分组的「已收到 / 未收到」两列）：

```csv
报告期,已收到,未收到,说明
2026-Q3,0301 病案统计室、0302 病案室,0303 医技部,已收到 2 个科室 · 未收到 1 个科室 · 名册来源：名册文件 …/rules/roster.json
```

「未收到」一列取自 `REGVAL_RULE_DIR/roster.json`（医院维护的应交科室清单）；名册没有、院内科室目录缓存也没有时，这一列留空并写清为什么空。报告期写法不在枚举内的行（比如 `2026Q3`）不单独开一个催报分组，只在未上报名单里出现。

命令实跑（6 行脱敏样例；`--rule-dir` 指到有名册文件的那个目录）：

```bash
regval export pending --out 未上报_2026-Q3.csv --reminder-out 催报对照_2026-Q3.csv --rule-dir tests/fixtures
```

```text
未上报名单已写出：未上报_2026-Q3.csv
批次 pending_report-8c4a2ec7e7b2 · 共 6 行 · 未上报 6 行 · 已上报 0 行
  · 待确认 3 行
  · 有问题待修 3 行
催报对照已写出：催报对照_2026-Q3.csv
  · 报告期 2026-Q3：已收到 2 个科室 · 未收到 1 个科室（名册列出 3 个应交科室，已收到 2 个）
  · 注意：1 行的报告期写法不在枚举内，未计入催报分组（这些行在未上报名单里）
  · 应交科室名册来源：名册文件 tests/fixtures/roster.json —— 按医院维护的应交科室清单比对（名册里没有的科室不催）
```

> 规则目录里**没有** `roster.json` 时（默认的 `./rules` 就是这样），这一段会照实说
> 「未取到名册」，**未收到一列留空** —— 不拿本地见过的科室凑名册：

```text
  · 报告期 2026-Q3：已收到 2 个科室 · 未收到 0 个科室（名册里没有 2026-Q3 的应交科室条目，未收到一列不可用（名册来源：未取到名册））
  · 应交科室名册来源：未取到名册 —— 未取到应交科室名册：规则目录里没有 roster.json，院内科室目录缓存也没有 —— 未收到一列为空（不拿本地见过的科室凑名册）
```

### 依据举证包（`regval export evidence`）

每行一条，含命中规则、各自的官方依据出处、建议修改动作；命中 0 条规则的行也出现并标「无命中」，便于逐行核对。除规则级依据外，每行还带**逐项官方条款依据**（本行指标对应官方附件的第几条、怎么定义的），科室照着改就行。完整样例见 `docs/examples.md`（Phase 4 后续 task 补齐）。

真实运行产物（`tests/fixtures/four_kinds.csv`，7 行脱敏样例）导出的举证包计数与首行：

```json
{
  "schema_version": "regval.evidence/1.0.0",
  "batch_ref": "four_kinds-db43143d1a4f",
  "rule_set_version": "2021.1",
  "indicator_basis_note": "逐项官方条款依据取自口径种子（版本 2021.1）",
  "counts": {
    "rows": 7, "rows_with_hits": 6, "rows_without_hits": 1, "hits": 8,
    "by_issue_kind": { "单位不符": 1, "必填缺失": 2, "枚举越界": 1, "疑似重复": 4 },
    "by_severity": { "提示": 4, "阻断": 4 }
  }
}
```

未命中的那一行照样在包里，标「无命中」并写明无需修改：

```csv
来源表,行号,行键,指标编号,科室代码,报告期,业务主键,校验结论,命中状态,命中条数,官方条款号,指标口径依据,规则编号,问题类型,严重度,问题说明,官方依据,错误码,缺失列,建议修改动作
four_kinds,1,7593d589ccbf…,MER-TL-04,0301,2026-Q3,BK-FOUR-01,通过,无命中,0,指标七,国卫办医函〔2021〕28号 附件…指标七（MER-TL-04）：病案首页24小时内完成率,,,,,,,本行未命中任何口径规则，无需修改
```

### 回执台账（`regval export receipts`）

留档与第三方核对用的台账：**一次提交一条**。同一个业务行订正后重报，
幂等键不变、修订号 +1、网关给两个回执号 —— 按「幂等键 + 修订号」记一条，
两边才对得上。列名与顺序固定，第三方按这套列核对。

真实运行（5 次提交：2 行受理 + 1 行退回 + 1 行待重试 + 1 次订正重报）导出的台账计数与交叉核对结论：

```json
{
  "schema_version": "regval.receipts/1.0.0",
  "batch_ref": "mini-941b47da51bd",
  "counts": {
    "entries": 5, "rows": 4, "receipts": 3, "revised_rows": 1, "entries_without_row_state": 0,
    "by_status": { "已上报": 3, "待重试": 1, "被退回": 1 },
    "by_row_state": { "已上报": 2, "待重试": 1, "被退回": 1 }
  },
  "cross_check": {
    "identity_pairs": 5, "distinct_receipt_ids": 3, "receipt_pairs_one_to_one": true,
    "accepted_without_receipt": 0, "receipt_without_submission": 0,
    "state_disagreements": 0, "conflicts": []
  }
}
```

台账正文（CSV，列名与顺序固定）：

```csv
批次,行键,幂等键,修订号,载荷摘要,上报状态,回执号,网关状态,受理时间,错误码,错误字段,网关说明,尝试次数,提交时间,更新时间,行状态,口径版本
mini-941b47da51bd,ba4e8a37d508ca92…,0423b5e196a484a2…,1,a2152e26661f0feb…,已上报,RCPT-001,accepted,2026-11-07 09:12:00,,,,1,2026-11-07 01:57:49,2026-11-07 01:57:49,已上报,2021.1
mini-941b47da51bd,ba4e8a37d508ca92…,0423b5e196a484a2…,2,7267874e0a3130b9…,已上报,RCPT-003,accepted,2026-11-07 10:20:00,,,,1,2026-11-07 01:57:49,2026-11-07 01:57:49,已上报,2021.1
mini-941b47da51bd,c1d624c24db7a079…,51edb05934eda6c45…,1,791206f1e632190a…,被退回,,,,GATEWAY_REJECTED,reporting_period,上报被退回：错误码 E_REPORTING_PERIOD · 字段 reporting_period · 报告期格式不合法,1,…,被退回,2021.1
```

终端同时给出核对结论；**核对不通过时退出码非 0**（台账仍写出来供逐条查，
但先别归档）：

```text
回执台账已写出：/tmp/regval-44/receipts.csv
批次 mini-941b47da51bd · 提交 5 条 · 回执 3 条 · 涉及 4 行 · 订正重报 1 行
  · 上报状态 已上报 3 条
  · 上报状态 待重试 1 条
  · 上报状态 被退回 1 条
  · 行状态 已上报 2 行
  · 行状态 待重试 1 行
  · 行状态 被退回 1 行
  · 交叉核对：幂等键+修订号 5 组 ↔ 回执号 3 个 · 一一对应 成立
  · 已上报无回执 0 条 · 有回执无提交记录 0 条 · 上报状态与行状态对不上 0 行
```

### 模型端点状态与调用审计（`model` 段）

模型端点是**外部依赖**：它可能连不上、超时、返回一段不是 JSON 的东西。
这些情形没有一种该让整条主路径停下来 —— 上报表还得校验，口径问题还得列出来。

所以契约里固定有一段 `model`，说清「这一轮模型掺和了没有」；
每一次调用都记进本机库的模型调用审计表（只有元数据与计数，没有任何一行业务内容）。
真实运行（模型端点不可达时）产出的一段：

```json
{
  "attempted": true,
  "degraded": true,
  "error_code": "MODEL_CONNECTION_FAILED",
  "error_message": "模型端点连接失败：error sending request for url (http://127.0.0.1:9/chat/completions)",
  "candidate_source": "deterministic",
  "endpoint": "http://127.0.0.1:9",
  "requested_model": "stub-model",
  "served_model": "",
  "duration_ms": 0,
  "candidate_pairs": 1,
  "accepted_pairs": 0,
  "dropped_duplicate_pairs": 0,
  "prompt_tokens": null,
  "completion_tokens": null,
  "total_tokens": null,
  "proposals": []
}
```

读法：

- `degraded: true` → 这一轮模型**没参与**，疑似重复只来自确定性三级（精确键 / 归一化 / 编辑距离）；
- `candidate_source: deterministic` → 同一件事的另一种说法，写在每一对疑似项上；
- `candidate_pairs: 1` → 本来要问模型几对（分得清「没候选」与「没送成」）；
- `proposals` 为空 → 未通过结构校验的模型回答**一条都不进结果**。

同一轮调用在终端与 `regval status` 里也看得见：

```bash
$ regval validate --input 科室一表.csv --out 契约.json
已校验 7 行 · 9 项口径问题 · 涉及 7 行 · 2 项疑似重复待确认 → 契约.json
口径规则集：版本 2021.1 · 生效 11 条 · 指纹 8125c3aaa14219ba
模型端点：不可用（MODEL_CONNECTION_FAILED）—— 本轮只输出确定性候选，疑似项来源标记为 deterministic
注意：模型端点本轮不可用（MODEL_CONNECTION_FAILED）—— 疑似重复只来自确定性三级（精确键 / 归一化 / 编辑距离），模型结论缺失；调用已记入模型调用审计表

$ regval status
状态库：./.regval/state.db
2026-11-06 15:13:29 · validate · 表 1 张 · 行 7 · 问题 9 项 · 耗时 18 ms · dupe_exact-4db6910245f3
模型调用审计：模型调用 stub-model · 降级（MODEL_CONNECTION_FAILED）· 耗时 0 ms · 候选 1 对 · 来源 deterministic
```

要「模型不可用就不许降级」的场合（汇报前必须确认模型这一关也过了），加 `--require-model`：

```bash
$ regval validate --input 科室一表.csv --out 契约.json --require-model
错误：[MODEL_UNAVAILABLE] 模型端点不可用（MODEL_CONNECTION_FAILED）：… —— 已按「模型必须可用」的要求中止，本轮不产出结果契约、不改动业务状态
```

### 恢复报告（`regval recover --out` 的产物）

状态库损坏时引擎给出的恢复报告 —— 「恢复到哪一批、丢了什么」逐条写清，不含糊：

```json
{
  "schema_version": "regval.recovery/1.0.0",
  "state_db": ".regval/state.db",
  "recovered_db": ".regval/state.db.recovered",
  "outcome": "部分恢复（有内容未能恢复，见丢失项）",
  "last_consistent_batch": "batch-2026-Q3",
  "restored": { "receipts": 12, "submissions": 12, "audit_events": 31, "confirmations": 12 },
  "journal": { "records": 25, "skipped_lines": 0 },
  "lost": {
    "rows": true, "issues": true, "row_state": true,
    "tables": ["rows", "issues"],
    "batches": ["batch-2026-Q4"]
  },
  "losses": [
    "校验结论（原表各行的校验结果）未恢复：需对本批重新跑一次校验",
    "口径问题清单未恢复：需对本批重新跑一次校验",
    "行状态（待确认 / 已上报 …）未恢复：需对本批重新跑一次校验并重新确认",
    "批次 batch-2026-Q4 没跑到最后（没有批次提交标记）：回执已恢复，但批次汇总未落库，需重跑这一批的上报补齐汇总"
  ],
  "notes": [
    "恢复库已写出：.regval/state.db.recovered（原库 .regval/state.db 未做任何改动）",
    "恢复的是「报过没有」这条事实（幂等键 ↔ 回执 ID）与审计链；校验结论与行状态需重新跑一次校验"
  ]
}
```