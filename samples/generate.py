#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""合成生成器：生成脱敏的卫健专项上报表样例集。

# 这套数据是什么

真实院内行级数据拿不到，所以本项目的样例数据由**脚本化合成**产出：
固定随机种子、同一份种子两次生成的字节完全一致、不联网、不依赖任何外部数据集。

数据只到**指标级监管数据**（指标编号、指标名称、科室、报告期、分子分母、填报值、
计量单位、业务主键）—— **没有患者可识别明细**：没有姓名、身份证、住院号、病案号、
性别、年龄、联系方式、床位号。机构代码用 `ORG-001` / `ORG-002` 这样的明显合成值，
业务主键一律带 `BK-` 前缀，一眼能看出这是合成数据而不是真数据。

# 产出两套

| 套 | 位置 | 规模 | 给谁用 |
|---|---|---|---|
| 基线样例集 | `samples/dataset/` | 6 张科室表 × 200 行 = 1200 行 | 主干演示、规则回归、漏斗逐级对比 |
| 规模样例集 | `samples/dataset-scale/` | 10 张科室表 × 500 行 = 5000 行 | 性能基准（耗时、内存、正确性） |

每套都额外给一张 `合并表.csv` —— 6 张（10 张）科室表并进同一个模板后的样子，
正是信息科现场的操作形态，也是 `regval validate` 一次就能吃下的输入。

# 埋了什么（脏数据形态）

四类口径问题（必填缺失 / 枚举越界 / 单位不符 / 疑似重复）之外，还埋了
单位别名、指标名称别名、公式不符、分母为零、非数值、生僻字、全角/半角写法、
首尾空白、科室名称写法不一致，以及一张**GBK/UTF-8 混编**的表。
每个埋点的条数由清单 `manifest.json` 的 `defects` 段如实记下来（不是估的）。

# 怎么跑

```bash
# 生成（默认写到 samples/dataset/ 与 samples/dataset-scale/）
python3 samples/generate.py

# 只生成一套 / 换输出目录 / 换种子
python3 samples/generate.py --only baseline
python3 samples/generate.py --out-dir /tmp/regval-samples
python3 samples/generate.py --seed 20261107

# 自检一：同一份种子生成两次，字节逐字相同（不落盘）
python3 samples/generate.py --self-test

# 自检二：按清单里的 sha256 复核已落盘的文件（不重写）
python3 samples/generate.py --check

# 自检三：样例里不含患者可识别字段名
grep -nE '姓名|身份证|住院号|病案号|证件号|手机|电话|银行卡|就诊号|床号|年龄|性别' samples/dataset/*.csv
```

`--check` 与 `--self-test` 都不写任何文件，可以随时重跑。

# 边界

本目录属于**测试数据层**，不是产品的最终对接方式。真实院内数据要从
HIS / 报表系统导出后走 `regval validate --input <表>` 进产品；
这里生成的数据只用来把规则、漏斗、状态机和性能基准**跑起来、看清楚**。
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import random
import shutil
import sys
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Sequence, Tuple

# --------------------------------------------------------------------------
# 常量
# --------------------------------------------------------------------------

#: 默认随机种子。改了就换一套数据，但同一份种子永远生成同一套字节。
SEED = 20261107

#: 列顺序（与产品最小数据格式一致，也与 tests/fixtures/mini.csv 一致）
COLUMNS: List[str] = [
    "机构代码",
    "指标编号",
    "指标名称",
    "科室代码",
    "科室名称",
    "报告期",
    "报告期类型",
    "分子",
    "分母",
    "填报值",
    "计量单位",
    "业务主键",
    "备注",
]

#: 27 项官方指标（《病案管理质量控制指标（2021年版）》，国卫办医函〔2021〕28号 附件）。
#:
#: 编号与名称以官方附件为准，不改写、不缩写。``unit=None`` 表示官方附件**未写明**
#: 计量单位（指标一至指标三，人均负担型）—— 本轮标「口径待补」，生成器也不替它猜单位。
INDICATORS: List[Tuple[str, str, str, Optional[str]]] = [
    ("MER-HR-01", "住院病案管理人员月均负担出院患者病历数", "burden", None),
    ("MER-HR-02", "门诊病案管理人员月均负担门诊患者病历数", "burden", None),
    ("MER-HR-03", "病案编码人员月均负担出院患者病历数", "burden", None),
    ("MER-TL-01", "入院记录24小时内完成率", "ratio", "%"),
    ("MER-TL-02", "手术记录24小时内完成率", "ratio", "%"),
    ("MER-TL-03", "出院记录24小时内完成率", "ratio", "%"),
    ("MER-TL-04", "病案首页24小时内完成率", "ratio", "%"),
    ("MER-ME-01", "CT/MRI检查记录符合率", "ratio", "%"),
    ("MER-ME-02", "病理检查记录符合率", "ratio", "%"),
    ("MER-ME-03", "细菌培养检查记录符合率", "ratio", "%"),
    ("MER-D&T-01", "抗菌药物使用记录符合率", "ratio", "%"),
    ("MER-D&T-02", "恶性肿瘤化学治疗记录符合率", "ratio", "%"),
    ("MER-D&T-03", "恶性肿瘤放射治疗记录符合率", "ratio", "%"),
    ("MER-D&T-04", "手术相关记录完整率", "ratio", "%"),
    ("MER-D&T-05", "植入物相关记录完整率", "ratio", "%"),
    ("MER-D&T-06", "临床用血相关记录符合率", "ratio", "%"),
    ("MER-D&T-07", "医师查房记录完整率", "ratio", "%"),
    ("MER-D&T-08", "患者抢救记录及时完成率", "ratio", "%"),
    ("MER-TQ-01", "出院患者病历2日归档率", "ratio", "%"),
    ("MER-TQ-02", "出院患者病历归档完整率", "ratio", "%"),
    ("MER-TQ-03", "主要诊断填写正确率", "ratio", "%"),
    ("MER-TQ-04", "主要诊断编码正确率", "ratio", "%"),
    ("MER-TQ-05", "主要手术填写正确率", "ratio", "%"),
    ("MER-TQ-06", "主要手术编码正确率", "ratio", "%"),
    ("MER-TQ-07", "不合理复制病历发生率", "ratio", "%"),
    ("MER-TQ-08", "知情同意书规范签署率", "ratio", "%"),
    ("MER-TQ-09", "甲级病历率", "ratio", "%"),
]

#: 指标编号 → (名称, 类别, 官方单位)
INDICATOR_BY_CODE: Dict[str, Tuple[str, str, Optional[str]]] = {
    code: (name, category, unit) for code, name, category, unit in INDICATORS
}

#: 样例科室目录（脱敏合成值）。
#:
#: 科室代码沿用产品内既有的样例代码（0301…0306），别名是医院常见的括注写法。
#: 这份目录同时写成 `dept_directory.json`，格式与院内 HIS 目录接口的响应一致，
#: 可直接当协议替身喂给目录客户端。
DEPT_ROSTER: List[Dict[str, object]] = [
    {
        "dept_code": "0301",
        "dept_name": "病案统计室",
        "aliases": ["病案统计室（本部）", "病案统计室（东区）"],
        "parent_code": "0300",
        "org_code": "ORG-001",
    },
    {
        "dept_code": "0302",
        "dept_name": "病案室",
        "aliases": ["病案室（住院部）"],
        "parent_code": "0300",
        "org_code": "ORG-001",
    },
    {
        "dept_code": "0303",
        "dept_name": "医技部",
        "aliases": ["医技部（本部）"],
        "parent_code": "0300",
        "org_code": "ORG-001",
    },
    {
        "dept_code": "0304",
        "dept_name": "临床科室",
        "aliases": [],
        "parent_code": "0300",
        "org_code": "ORG-001",
    },
    {
        "dept_code": "0305",
        "dept_name": "院感科",
        "aliases": ["医院感染管理科"],
        "parent_code": "0300",
        "org_code": "ORG-002",
    },
    {
        "dept_code": "0306",
        "dept_name": "门诊病案室",
        "aliases": [],
        "parent_code": "0300",
        "org_code": "ORG-002",
    },
]

#: 目录里**没有**的科室代码 —— 用来触发「科室主数据不一致」（DEPT_NOT_FOUND）。
UNKNOWN_DEPT_CODES: List[Tuple[str, str]] = [
    ("0999", "查无此科"),
    ("0888", "已撤销科室"),
]

#: 计量单位别名（口径层同一张表的子集）。规范单位 ∈ 例 / 例次 / 人次 / 人 / %。
#:
#: 比率型指标的期望单位是 %，所以比率型行只写 % 的别名（归一后仍是 %）；
#: 非 % 的别名（例数、例次数、人次数、人数）只出现在人均负担型指标上 ——
#: 那三项官方附件未写明单位，本轮标「口径待补」，不判单位是否匹配。
UNIT_ALIAS_PERCENT: List[str] = ["％", "﹪", "百分比"]
UNIT_ALIAS_OTHER: List[str] = ["例数", "例次数", "人次数", "人数"]

#: 指标名称别名（口径层同一张表的子集）：同一个指标的另一种写法。
INDICATOR_NAME_ALIASES: List[Tuple[str, str]] = [
    ("CT/MRI检查记录符合率", "CT、MRI检查记录符合率"),
    ("CT/MRI检查记录符合率", "CT和MRI检查记录符合率"),
    ("出院患者病历2日归档率", "出院病历2日归档率"),
    ("出院患者病历2日归档率", "患者病历2日归档率"),
    ("出院患者病历2日归档率", "出院患者病历2天内归档率"),
    ("主要诊断填写正确率", "主要诊断正确率"),
    ("入院记录24小时内完成率", "入院记录24H内完成率"),
    ("手术记录24小时内完成率", "手术记录24H内完成率"),
    ("出院记录24小时内完成率", "出院记录24H内完成率"),
    ("细菌培养检查记录符合率", "院感细菌培养检查记录符合率"),
]

#: 生僻字（都落在 GBK 码位内，混编表里也能写）。
#:
#: 现实里这些字出现在科室自定的括注、备注里；本轮放在备注与少数科室名称写法里，
#: 用来验「按字符处理、不按字节处理」的归一与截断路径。
RARE_CHARS: List[str] = ["淼", "焱", "燚", "犇", "骉", "赑", "灥", "龘", "髹", "昶"]

#: 备注里会出现的自由文本池（都不含患者可识别字段）。
REMARKS: List[str] = [
    "口径按属地 2026 版",
    "科室台账补录",
    "与上期口径一致",
    "已按院内质控要求复核",
    "手工补录，待复核",
    "分母按同期在册数取",
]

#: 报告期 → 报告期类型（合法组合）
PERIODS: List[Tuple[str, str]] = [
    ("2026-Q3", "季报"),
    ("2026-Q3", "季报"),
    ("2026-Q3", "季报"),
    ("2026-H1", "半年报"),
    ("2026", "年报"),
    ("2026-M03", "月报"),
]

#: 非法报告期写法（枚举越界埋点）
BAD_PERIODS: List[str] = ["2026Q3", "2026 年第三季度", "2026-M13", "2026-Q5", "26-Q3"]

#: 非法报告期类型（枚举越界埋点）
BAD_PERIOD_TYPES: List[str] = ["季度报", "年报(全年)", "半年"]

#: 非法计量单位（枚举越界埋点）
BAD_UNITS: List[str] = ["台", "个", "床日"]

#: 非法指标编号（枚举越界埋点）
BAD_INDICATOR_CODES: List[str] = ["MER-XX-99", "MER-TL-09"]

#: 比率型指标写成别的单位（单位不符埋点 —— 值合法、口径不符）
UNIT_MISMATCH_UNITS: List[str] = ["例", "例次"]

#: 分母取值池
DENOMINATORS: List[int] = [500, 800, 1000, 1200, 2000, 240]

#: 患者可识别字段名（自检用；样例里一个都不该有）
FORBIDDEN_FIELD_WORDS: List[str] = [
    "姓名",
    "身份证",
    "住院号",
    "病案号",
    "证件号",
    "手机",
    "电话",
    "银行卡",
    "就诊号",
    "床号",
    "年龄",
    "性别",
]


# --------------------------------------------------------------------------
# 数据集规格
# --------------------------------------------------------------------------


class TableSpec:
    """一张科室上报表的规格。"""

    def __init__(
        self,
        filename: str,
        dept_code: str,
        indicators: Sequence[str],
        gbk_mixed: bool = False,
        org_code: Optional[str] = None,
    ) -> None:
        self.filename = filename
        self.dept_code = dept_code
        self.indicators = list(indicators)
        #: 该表是否按「GBK/UTF-8 混编」写盘（部分行 GBK、部分行 UTF-8）
        self.gbk_mixed = gbk_mixed
        #: 机构代码。留空则取科室目录里的默认机构；两个院区用同一套科室代码时
        #: 就是靠它区分（业务主键与权威键都含机构代码）。
        self.org_code = org_code

    def effective_org(self) -> str:
        if self.org_code:
            return self.org_code
        return str(dept_by_code(self.dept_code)["org_code"])


def _baseline_specs() -> List[TableSpec]:
    """基线集：6 张科室表 × 200 行。"""
    return [
        TableSpec("病案统计室表.csv", "0301", [
            "MER-TL-01", "MER-TL-02", "MER-TL-03", "MER-TL-04",
            "MER-TQ-01", "MER-TQ-02", "MER-TQ-03", "MER-TQ-04",
            "MER-TQ-09", "MER-HR-01", "MER-HR-03",
        ]),
        TableSpec("病案室表.csv", "0302", [
            "MER-TL-01", "MER-TL-03", "MER-TL-04",
            "MER-TQ-02", "MER-TQ-03", "MER-TQ-05", "MER-TQ-06", "MER-TQ-08",
        ]),
        # 这一张按 GBK/UTF-8 混编写盘：中文 Windows 导出的 CSV 常见形态
        TableSpec("医技部表.csv", "0303", [
            "MER-ME-01", "MER-ME-02", "MER-ME-03",
            "MER-D&T-01", "MER-D&T-02", "MER-D&T-03", "MER-D&T-04",
        ], gbk_mixed=True),
        TableSpec("临床科室表.csv", "0304", [
            "MER-D&T-05", "MER-D&T-06", "MER-D&T-07", "MER-D&T-08",
            "MER-TL-02", "MER-TL-03", "MER-TQ-05", "MER-TQ-07",
        ]),
        TableSpec("院感科表.csv", "0305", [
            "MER-ME-03", "MER-D&T-01", "MER-TQ-08", "MER-TQ-09", "MER-HR-02",
        ]),
        TableSpec("门诊病案室表.csv", "0306", [
            "MER-TL-01", "MER-TQ-01", "MER-TQ-02", "MER-TQ-03", "MER-TQ-04",
            "MER-HR-02",
        ]),
    ]


def _scale_specs() -> List[TableSpec]:
    """规模集：10 张科室表 × 500 行 = 5000 行。

    比基线集多四张「院区二」的表 —— 两个院区用**同一套科室代码**（0301/0303/0304/0306），
    机构代码不同。权威键与业务主键都含机构代码，所以这四张表不会跟院区一的表撞车；
    真撞上的是「同院区同科室两套科室代码」那种情况，不在样例里制造。
    """
    specs = _baseline_specs()
    specs.append(TableSpec("院区二病案统计室表.csv", "0301", [
        "MER-TL-01", "MER-TL-02", "MER-TL-03", "MER-TL-04",
        "MER-TQ-01", "MER-TQ-02", "MER-TQ-03", "MER-TQ-04",
        "MER-TQ-09", "MER-HR-01", "MER-HR-03",
    ], gbk_mixed=True, org_code="ORG-002"))
    specs.append(TableSpec("院区二医技部表.csv", "0303", [
        "MER-ME-01", "MER-ME-02", "MER-ME-03",
        "MER-D&T-01", "MER-D&T-02", "MER-D&T-03", "MER-D&T-04",
    ], org_code="ORG-002"))
    specs.append(TableSpec("院区二临床科室表.csv", "0304", [
        "MER-D&T-01", "MER-D&T-02", "MER-D&T-03", "MER-D&T-04", "MER-D&T-05",
        "MER-D&T-06", "MER-D&T-07", "MER-D&T-08",
        "MER-TL-01", "MER-TL-02", "MER-TL-03", "MER-TL-04",
        "MER-TQ-01", "MER-TQ-02", "MER-TQ-03", "MER-TQ-04", "MER-TQ-05",
        "MER-TQ-06", "MER-TQ-07", "MER-TQ-08", "MER-TQ-09",
    ], org_code="ORG-002"))
    specs.append(TableSpec("院区二门诊病案室表.csv", "0306", [
        "MER-TL-01", "MER-TQ-01", "MER-TQ-02", "MER-TQ-03", "MER-TQ-04",
        "MER-HR-02",
    ], org_code="ORG-002"))
    return specs


# --------------------------------------------------------------------------
# 随机源
# --------------------------------------------------------------------------


def derive_seed(master_seed: int, key: str) -> int:
    """由主种子与表名派生出一个稳定的整数种子。

    用 sha256 而不是 ``hash()``：``hash()`` 带进程级随机盐，换一次进程就换一个数，
    「同一份种子两次生成字节相同」这条就守不住了。
    """
    digest = hashlib.sha256(f"{master_seed}|{key}".encode("utf-8")).hexdigest()
    return int(digest[:16], 16)


# --------------------------------------------------------------------------
# 行构造
# --------------------------------------------------------------------------


def dept_by_code(code: str) -> Dict[str, object]:
    for dept in DEPT_ROSTER:
        if dept["dept_code"] == code:
            return dept
    raise KeyError(f"科室目录里没有 {code}")


def clean_row(
    rng: random.Random,
    org_code: str,
    dept_code: str,
    indicator_code: str,
    seq: int,
    table_key: str,
) -> Dict[str, str]:
    """造一行「没毛病」的指标级监管数据。"""
    name, category, unit = INDICATOR_BY_CODE[indicator_code]
    dept = dept_by_code(dept_code)
    period, period_type = PERIODS[rng.randrange(len(PERIODS))]

    if category == "ratio":
        denominator = DENOMINATORS[rng.randrange(len(DENOMINATORS))]
        # 分子落在分母的 60%–100% 之间：完成率/符合率类指标的真实区间
        numerator = int(denominator * (60 + rng.randrange(41)) / 100)
        value = round(numerator * 100.0 / denominator, 1)
        unit_text = unit or "%"
    else:
        # 人均负担型：分子是病历总数，分母是「月数 × 人数」的乘积量级
        denominator = 12 * (1 + rng.randrange(9))
        numerator = denominator * (30 + rng.randrange(51))
        value = round(numerator * 100.0 / denominator, 1)
        unit_text = ""

    return {
        "机构代码": org_code,
        "指标编号": indicator_code,
        "指标名称": name,
        "科室代码": dept_code,
        "科室名称": str(dept["dept_name"]),
        "报告期": period,
        "报告期类型": period_type,
        "分子": str(numerator),
        "分母": str(denominator),
        "填报值": str(value),
        "计量单位": unit_text,
        "业务主键": f"BK-{table_key}-{seq:04d}",
        "备注": REMARKS[rng.randrange(len(REMARKS))] if rng.random() < 0.25 else "",
    }


def copy_row(row: Dict[str, str]) -> Dict[str, str]:
    return dict(row)


# --------------------------------------------------------------------------
# 埋点注入
# --------------------------------------------------------------------------


class DefectCounter:
    """埋点计数器。每一类埋点记条数，最后如实写进 manifest。"""

    def __init__(self) -> None:
        self.counts: Dict[str, int] = {}

    def hit(self, kind: str, n: int = 1) -> None:
        self.counts[kind] = self.counts.get(kind, 0) + n

    def as_dict(self) -> Dict[str, int]:
        return dict(sorted(self.counts.items()))


def inject_defects(
    rng: random.Random,
    row: Dict[str, str],
    counter: DefectCounter,
    indicator_code: str,
) -> Dict[str, str]:
    """给一行数据按概率注入一处脏数据形态。

    每次调用**最多**改一处：一处问题对应一条可核对的问题行，混在一起就分不清
    「这条为什么不合格」了。
    """
    name, category, unit = INDICATOR_BY_CODE[indicator_code]
    draw = rng.random()

    # 1. 必填缺失（约 4%）：空串与纯空白都算缺失
    if draw < 0.040:
        which = rng.randrange(4)
        column = ["指标名称", "业务主键", "科室代码", "报告期"][which]
        if rng.random() < 0.25:
            row[column] = "　" if rng.random() < 0.5 else "  "
            counter.hit("必填缺失（纯空白值）")
        else:
            row[column] = ""
            counter.hit("必填缺失")
        return row

    # 2. 枚举越界（约 3%）
    if draw < 0.070:
        which = rng.randrange(4)
        if which == 0:
            row["报告期"] = BAD_PERIODS[rng.randrange(len(BAD_PERIODS))]
            counter.hit("枚举越界（报告期格式）")
        elif which == 1:
            row["报告期类型"] = BAD_PERIOD_TYPES[rng.randrange(len(BAD_PERIOD_TYPES))]
            counter.hit("枚举越界（报告期类型）")
        elif which == 2:
            row["计量单位"] = BAD_UNITS[rng.randrange(len(BAD_UNITS))]
            counter.hit("枚举越界（计量单位）")
        else:
            row["指标编号"] = BAD_INDICATOR_CODES[rng.randrange(len(BAD_INDICATOR_CODES))]
            counter.hit("枚举越界（指标编号不在册）")
        return row

    # 3. 单位不符（约 2.5%）：比率型指标写成别的单位
    if draw < 0.095 and category == "ratio":
        row["计量单位"] = UNIT_MISMATCH_UNITS[rng.randrange(len(UNIT_MISMATCH_UNITS))]
        counter.hit("单位不符")
        return row

    # 4. 单位别名（约 3%）：同一个单位的另一种写法，归一后应当一致
    if draw < 0.125:
        if category == "ratio":
            row["计量单位"] = UNIT_ALIAS_PERCENT[rng.randrange(len(UNIT_ALIAS_PERCENT))]
        else:
            row["计量单位"] = UNIT_ALIAS_OTHER[rng.randrange(len(UNIT_ALIAS_OTHER))]
        counter.hit("单位别名")
        return row

    # 5. 指标名称别名（约 2%）：同一个指标的另一种写法
    if draw < 0.145:
        aliases = [alias for canon, alias in INDICATOR_NAME_ALIASES if canon == name]
        if aliases:
            row["指标名称"] = aliases[rng.randrange(len(aliases))]
            counter.hit("指标名称别名")
            return row

    # 6. 公式不符（约 2%）：分子分母与填报值对不上
    if draw < 0.165 and category == "ratio":
        value = float(row["填报值"])
        row["填报值"] = str(round(max(0.0, value - (3 + rng.randrange(20))), 1))
        counter.hit("公式不符")
        return row

    # 7. 分母为零（约 0.8%）
    if draw < 0.173 and category == "ratio":
        row["分母"] = "0"
        counter.hit("分母为零")
        return row

    # 8. 非数值（约 0.8%）
    if draw < 0.181 and category == "ratio":
        row["分子"] = ["abc", "N/A", "—", "约120"][rng.randrange(4)]
        counter.hit("非数值")
        return row

    # 9. 科室名称写法不一致（约 1.5%）：目录里有这个代码，但名字对不上
    if draw < 0.196:
        dept = dept_by_code(row["科室代码"])
        pool = [str(dept["dept_name"])] + [str(a) for a in dept["aliases"]]
        row["科室名称"] = pool[1] if len(pool) > 1 else str(dept["dept_name"]) + "（临时）"
        counter.hit("科室名称写法不一致")
        return row

    # 10. 生僻字（约 1.5%）：备注里带一个生僻字
    if draw < 0.211:
        rare = RARE_CHARS[rng.randrange(len(RARE_CHARS))]
        row["备注"] = f"科室自定口径标记 {rare}"
        counter.hit("生僻字")
        return row

    # 11. 首尾空白 / 全角空格（约 1.5%）：归一化路径
    if draw < 0.226:
        if rng.random() < 0.5:
            row["科室名称"] = "　" + row["科室名称"] + "　"
            counter.hit("首尾全角空白")
        else:
            row["备注"] = " " + row["备注"] + " " if row["备注"] else " "
            counter.hit("首尾半角空白")
        return row

    # 12. 科室代码不在目录（约 1%）：触发「科室主数据不一致」
    if draw < 0.236:
        code, dept_name = UNKNOWN_DEPT_CODES[rng.randrange(len(UNKNOWN_DEPT_CODES))]
        row["科室代码"] = code
        row["科室名称"] = dept_name
        counter.hit("科室代码不在目录")
        return row

    return row


# --------------------------------------------------------------------------
# 重复行（疑似重复三个层级 + 待人工判定）
# --------------------------------------------------------------------------


def pick_sources(rng: random.Random, candidates: Sequence[int], needed: int) -> List[int]:
    """从候选下标里挑出 ``needed`` 个当重复源行。

    用 sha256 排序而不是 ``random.sample``：``sample``/``shuffle`` 的内部实现
    在不同 Python 版本里可能变一变，「同一份种子两次生成字节相同」这条就守不住。
    """
    salt = rng.getrandbits(64)
    scored = sorted(
        candidates,
        key=lambda i: hashlib.sha256(f"{salt}|{i}".encode("utf-8")).digest(),
    )
    return sorted(scored[:needed])


def derive_duplicates(
    rng: random.Random,
    sources: Sequence[Dict[str, str]],
    counter: DefectCounter,
    pair_count: int,
) -> List[Dict[str, str]]:
    """从源行派生重复行，覆盖重复检测漏斗的三级 + 待人工判定。

    | 层级 | 派生方式 | 漏斗那一级会报 |
    |---|---|---|
    | L1 确定性键 | 原样复制（单位可换别名写法） | 确定性键 |
    | L2 归一化 | 指标名称换成别名写法 / 全角半角 / 多一个全角空格 | 归一化候选 |
    | L3 编辑距离 | 指标名称差 1 个字 | 编辑距离候选 |
    | 待人工判定 | 同键但取值不同 | 实体绑定层（不算重复） |

    重复行的业务主键与源行**逐字相同** —— 现场就是同一件事填了两遍。
    """
    extra: List[Dict[str, str]] = []
    levels = ["L1", "L2", "L3", "L3"]
    for i in range(pair_count):
        level = levels[i % len(levels)]
        src = sources[i % len(sources)]
        dup = copy_row(src)

        if level == "L1":
            # 取值逐字一致；单位换一种写法也算一致（别名表归一后同单位）
            if src["计量单位"] == "%" and rng.random() < 0.5:
                dup["计量单位"] = UNIT_ALIAS_PERCENT[rng.randrange(len(UNIT_ALIAS_PERCENT))]
            counter.hit("疑似重复 · 确定性键（L1）")
        elif level == "L2":
            name = src["指标名称"]
            aliases = [alias for canon, alias in INDICATOR_NAME_ALIASES if canon == name]
            if aliases:
                dup["指标名称"] = aliases[rng.randrange(len(aliases))]
            elif "/" in name:
                # 全角斜杠与半角斜杠是同一份检查项（CT／MRI 与 CT/MRI）
                dup["指标名称"] = name.replace("/", "／")
            else:
                # 名称中间多一个全角空格：归一化会把它吸收掉
                cut = len(name) // 2
                dup["指标名称"] = name[:cut] + "　" + name[cut:]
            counter.hit("疑似重复 · 归一化候选（L2）")
        else:
            # 少一个字：编辑距离 1，落在阈值内
            name = src["指标名称"]
            cut = rng.randrange(1, max(2, len(name) - 2))
            dup["指标名称"] = name[:cut] + name[cut + 1:]
            counter.hit("疑似重复 · 编辑距离候选（L3）")

        dup["备注"] = "疑似重复埋点：" + level
        extra.append(dup)

    # 待人工判定：同一业务主键，取值不同（订正没改键）
    conflict_count = max(1, pair_count // 4)
    for i in range(conflict_count):
        src = sources[(i * 3) % len(sources)]
        conflict = copy_row(src)
        denominator = int(float(conflict["分母"]))
        numerator = int(float(conflict["分子"]))
        if numerator <= 0:
            numerator = 1
        conflict["分子"] = str(max(0, numerator - 1 - (i % 5)))
        conflict["填报值"] = str(round(int(conflict["分子"]) * 100.0 / max(1, denominator), 1))
        conflict["备注"] = "待人工判定埋点：同键异值"
        extra.append(conflict)
        counter.hit("待人工判定（同键异值）")

    return extra


# --------------------------------------------------------------------------
# 一张表的生成
# --------------------------------------------------------------------------


def build_table(
    spec: TableSpec,
    master_seed: int,
    row_count: int,
    pair_count: int,
) -> Tuple[List[Dict[str, str]], DefectCounter, List[int]]:
    """生成一张科室表。

    顺序是**先造干净行、再挑重复源行、最后才往剩下的行里注入脏数据** ——
    顺序反过来的话，脏数据会把源行毁掉（指标名称被清空、业务主键被清空），
    重复埋点就派生不出来了。

    返回 ``(行, 埋点计数, 走 GBK 编码的行下标)``。
    """
    rng = random.Random(derive_seed(master_seed, spec.filename))
    org_code = spec.effective_org()
    table_key = "S" + spec.dept_code + spec.filename[:2]

    counter = DefectCounter()
    conflict_count = max(1, pair_count // 4)
    needed = pair_count + conflict_count
    base_count = row_count - needed
    if base_count < needed + 1:
        raise ValueError(f"{spec.filename} 行数 {row_count} 太小，容不下 {needed} 条重复埋点")

    # 1. 干净行
    base_rows: List[Dict[str, str]] = []
    indicator_of: List[str] = []
    for seq in range(1, base_count + 1):
        indicator_code = spec.indicators[rng.randrange(len(spec.indicators))]
        base_rows.append(clean_row(rng, org_code, spec.dept_code, indicator_code, seq, table_key))
        indicator_of.append(indicator_code)

    # 2. 挑重复源行（源行不注入脏数据，保证它俩都键齐、值可比）
    source_idx = pick_sources(rng, list(range(base_count)), needed)
    sources = [base_rows[i] for i in source_idx]

    # 3. 派生重复行与待人工判定行
    extra_rows = derive_duplicates(rng, sources, counter, pair_count)

    # 4. 往剩下的行里注入脏数据形态
    source_set = set(source_idx)
    for i, row in enumerate(base_rows):
        if i in source_set:
            continue
        inject_defects(rng, row, counter, indicator_of[i])

    rows = base_rows + extra_rows
    assert len(rows) == row_count, f"{spec.filename} 行数 {len(rows)} != {row_count}"

    # GBK 混编：挑一部分「GBK 能编」的行走 GBK，其余留 UTF-8。
    gbk_rows: List[int] = []
    if spec.gbk_mixed:
        eligible = [i for i, row in enumerate(rows) if gbk_encodable(row)]
        # 固定取样间隔（不是随机）：同一份种子生成同一批 GBK 行，可复现
        stride = 5
        gbk_rows = eligible[2::stride]
        counter.hit(f"GBK 混编行（{spec.filename}）", len(gbk_rows))

    return rows, counter, gbk_rows


def gbk_encodable(row: Dict[str, str]) -> bool:
    """这一行能不能用 GBK 写出来（不能就留 UTF-8，不硬编）。"""
    try:
        csv_line(row).encode("gbk")
    except UnicodeEncodeError:
        return False
    return True


# --------------------------------------------------------------------------
# 写盘
# --------------------------------------------------------------------------


def csv_line(row: Dict[str, str]) -> str:
    """一行 CSV（不带换行符）。用 csv 模块做引号转义，避免自己拼错。"""
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="")
    writer.writerow([row.get(column, "") for column in COLUMNS])
    return buf.getvalue()


def table_bytes(rows: Sequence[Dict[str, str]], gbk_rows: Iterable[int]) -> bytes:
    """把一张表写成字节：表头 UTF-8，数据行按指定下标走 GBK，其余 UTF-8。"""
    gbk_set = set(gbk_rows)
    out = bytearray()
    out += (",".join(COLUMNS) + "\n").encode("utf-8")
    for index, row in enumerate(rows):
        line = csv_line(row)
        if index in gbk_set:
            out += line.encode("gbk")
        else:
            out += line.encode("utf-8")
        out += b"\n"
    return bytes(out)


def merged_bytes(tables: Sequence[Tuple[bytes, Sequence[Dict[str, str]], Iterable[int]]]) -> bytes:
    """把多张表并进一个模板：表头只留一份，数据行按原顺序接上（编码也照原样保留）。"""
    out = bytearray()
    out += (",".join(COLUMNS) + "\n").encode("utf-8")
    for _header, rows, gbk_rows in tables:
        gbk_set = set(gbk_rows)
        for index, row in enumerate(rows):
            line = csv_line(row)
            if index in gbk_set:
                out += line.encode("gbk")
            else:
                out += line.encode("utf-8")
            out += b"\n"
    return bytes(out)


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# --------------------------------------------------------------------------
# 一套数据集
# --------------------------------------------------------------------------


class Dataset:
    """一套数据集：若干张表 + 一张合并表 + 科室目录 + 清单。"""

    def __init__(self, name: str, specs: Sequence[TableSpec], row_count: int,
                 pair_count: int, master_seed: int) -> None:
        self.name = name
        self.specs = list(specs)
        self.row_count = row_count
        self.pair_count = pair_count
        self.master_seed = master_seed
        self.files: Dict[str, bytes] = {}
        self.manifest: Dict[str, object] = {}

    def build(self) -> None:
        counter = DefectCounter()
        table_entries: List[Dict[str, object]] = []
        merged_parts: List[Tuple[bytes, Sequence[Dict[str, str]], Iterable[int]]] = []
        total_rows = 0

        for spec in self.specs:
            rows, table_counter, gbk_rows = build_table(
                spec, self.master_seed, self.row_count, self.pair_count
            )
            for kind, count in table_counter.counts.items():
                counter.hit(kind, count)

            data = table_bytes(rows, gbk_rows)
            self.files[spec.filename] = data
            merged_parts.append((data, rows, gbk_rows))
            total_rows += len(rows)

            table_entries.append({
                "file": spec.filename,
                "dept_code": spec.dept_code,
                "org_code": spec.effective_org(),
                "rows": len(rows),
                "encoding": "utf-8+gbk 混编" if gbk_rows else "utf-8",
                "gbk_rows": len(gbk_rows),
                "sha256": sha256_hex(data),
            })

        merged = merged_bytes(merged_parts)
        self.files["合并表.csv"] = merged

        directory = [
            {
                "dept_code": dept["dept_code"],
                "dept_name": dept["dept_name"],
                "aliases": list(dept["aliases"]),  # type: ignore[arg-type]
                "parent_code": dept["parent_code"],
            }
            for dept in DEPT_ROSTER
        ]
        directory_bytes = (json.dumps(directory, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
        self.files["dept_directory.json"] = directory_bytes

        self.manifest = {
            "dataset": self.name,
            "seed": self.master_seed,
            "generator": "samples/generate.py",
            "source": "全部为合成数据，非真实院内数据；只含指标级监管数据，无患者可识别明细",
            "table_count": len(self.specs),
            "rows_per_table": self.row_count,
            "total_rows": total_rows,
            "tables": table_entries,
            "merged": {
                "file": "合并表.csv",
                "rows": total_rows,
                "sha256": sha256_hex(merged),
            },
            "dept_directory": {
                "file": "dept_directory.json",
                "entries": len(directory),
                "sha256": sha256_hex(directory_bytes),
            },
            "defects": counter.as_dict(),
        }
        self.files["manifest.json"] = (
            json.dumps(self.manifest, ensure_ascii=False, indent=2) + "\n"
        ).encode("utf-8")

    # -- 落盘 / 校验 ---------------------------------------------------

    def write(self, out_dir: Path) -> None:
        out_dir.mkdir(parents=True, exist_ok=True)
        for name, data in self.files.items():
            (out_dir / name).write_bytes(data)

    def check(self, out_dir: Path) -> List[str]:
        """按清单复核已落盘的文件：sha256 与行数都要对上。"""
        problems: List[str] = []
        manifest_path = out_dir / "manifest.json"
        if not manifest_path.exists():
            return [f"{out_dir} 下没有 manifest.json，先跑一次生成"]
        on_disk = json.loads(manifest_path.read_text(encoding="utf-8"))
        for entry in on_disk.get("tables", []):
            path = out_dir / str(entry["file"])
            if not path.exists():
                problems.append(f"缺文件 {entry['file']}")
                continue
            data = path.read_bytes()
            if sha256_hex(data) != entry["sha256"]:
                problems.append(f"{entry['file']} 的 sha256 与清单不一致")
            rows = sum(1 for line in data.split(b"\n")[1:] if line.strip())
            if rows != entry["rows"]:
                problems.append(f"{entry['file']} 行数 {rows} != 清单 {entry['rows']}")
        merged = on_disk.get("merged", {})
        merged_path = out_dir / str(merged.get("file", "合并表.csv"))
        if merged_path.exists():
            if sha256_hex(merged_path.read_bytes()) != merged.get("sha256"):
                problems.append("合并表.csv 的 sha256 与清单不一致")
        return problems


# --------------------------------------------------------------------------
# 命令行
# --------------------------------------------------------------------------


def build_datasets(seed: int) -> Dict[str, Dataset]:
    """造两套数据集（基线 + 规模）。"""
    baseline = Dataset(
        "regval-sample-2026.1-baseline",
        _baseline_specs(),
        row_count=200,
        pair_count=8,
        master_seed=seed,
    )
    baseline.build()

    scale = Dataset(
        "regval-sample-2026.1-scale",
        _scale_specs(),
        row_count=500,
        pair_count=20,
        master_seed=seed + 1,
    )
    scale.build()

    return {"baseline": baseline, "scale": scale}


def scan_forbidden(text: str) -> List[str]:
    """查文本里有没有患者可识别字段名。"""
    hits: List[str] = []
    for word in FORBIDDEN_FIELD_WORDS:
        if word in text:
            hits.append(word)
    return hits


def self_test(seed: int) -> int:
    """同一份种子生成两次，字节逐字相同 —— 这一条守不住，整套样例就不可复现。"""
    first = build_datasets(seed)
    second = build_datasets(seed)
    problems: List[str] = []
    for key in first:
        for name, data in first[key].files.items():
            other = second[key].files.get(name)
            if other is None:
                problems.append(f"{key}/{name} 第二次没生成出来")
            elif other != data:
                problems.append(f"{key}/{name} 两次生成的字节不同（种子 {seed}）")
    if problems:
        for problem in problems:
            print(f"[FAIL] {problem}", file=sys.stderr)
        return 1
    total = sum(len(d.files) for d in first.values())
    print(f"[OK] 同一份种子（{seed}）生成两次，{total} 个文件逐字节相同")
    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="生成脱敏的卫健专项上报表样例集（固定种子、可重跑、断网可用）"
    )
    parser.add_argument("--seed", type=int, default=SEED, help="随机种子（默认 %(default)s）")
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="输出根目录（默认 <项目根>/samples）",
    )
    parser.add_argument(
        "--only",
        choices=["baseline", "scale"],
        default=None,
        help="只生成其中一套",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="按清单里的 sha256 与行数复核已落盘的文件（不重写）",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="同一份种子生成两次并比对字节（不落盘）",
    )
    args = parser.parse_args(argv)

    project_root = Path(__file__).resolve().parent.parent
    out_root = args.out_dir if args.out_dir else project_root / "samples"

    if args.self_test:
        return self_test(args.seed)

    datasets = build_datasets(args.seed)
    if args.only:
        datasets = {args.only: datasets[args.only]}

    if args.check:
        problems: List[str] = []
        for key, dataset in datasets.items():
            sub_dir = out_root / ("dataset" if key == "baseline" else "dataset-scale")
            problems.extend(dataset.check(sub_dir))
        if problems:
            for problem in problems:
                print(f"[FAIL] {problem}", file=sys.stderr)
            return 1
        print("[OK] 已落盘的样例文件与清单一致（sha256 与行数都对得上）")
        return 0

    for key, dataset in datasets.items():
        sub_dir = out_root / ("dataset" if key == "baseline" else "dataset-scale")
        if sub_dir.exists():
            shutil.rmtree(sub_dir)
        dataset.write(sub_dir)
        manifest = dataset.manifest
        print(
            f"已生成 {dataset.name}：{manifest['table_count']} 张表 × "
            f"{manifest['rows_per_table']} 行 = {manifest['total_rows']} 行 → {sub_dir}"
        )
        for kind, count in manifest["defects"].items():  # type: ignore[union-attr]
            print(f"  · {kind}：{count} 行")
    return 0


if __name__ == "__main__":
    sys.exit(main())
