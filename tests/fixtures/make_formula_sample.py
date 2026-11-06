#!/usr/bin/env python3
"""生成带公式的样例表（tests/fixtures/formula_sample.xlsx）。

用途：验证「写回只碰新列」—— 原表里的公式列与已有列在写回后必须原样不动。

数据只到**指标级监管数据**，不含任何患者可识别明细；业务主键统一带
`BK-SAMPLE-` 前缀以便与真实数据区分。

可重跑：每次生成的文件内容一致（openpyxl 写入的内容与时间戳无关）。
"""

from pathlib import Path

from openpyxl import Workbook

HERE = Path(__file__).resolve().parent
OUT = HERE / "formula_sample.xlsx"

HEADERS = [
    "指标编号",
    "指标名称",
    "科室代码",
    "科室名称",
    "报告期",
    "分子",
    "分母",
    "填报值",
    "计量单位",
    "业务主键",
    "备注",
    "合计校验",
]

# 前 11 列是业务列，第 12 列是公式列（= 分子 + 分母）
ROWS = [
    ["MER-TL-04", "病案首页24小时内完成率", "0301", "病案统计室", "2026-Q3", 980, 1000, 98, "%", "BK-SAMPLE-01", ""],
    ["MER-TQ-01", "出院患者病历2日归档率", "0301", "病案统计室", "2026-Q3", 960, 1000, 96, "%", "BK-SAMPLE-02", ""],
    ["MER-TQ-03", "主要诊断填写正确率", "0302", "病案室", "2026-Q3", 995, 1000, 99.5, "%", "BK-SAMPLE-03", "复核后填报"],
    ["MER-ME-01", "CT/MRI检查记录符合率", "0303", "医技部", "2026-Q3", 970, 1000, 97, "%", "", ""],
    ["MER-HR-01", "住院病案管理人员月均负担出院患者病历数", "0301", "病案统计室", "2026-Q3", 120, 10, 120, "", "BK-SAMPLE-05", ""],
]


def main() -> None:
    wb = Workbook()
    ws = wb.active
    ws.title = "上报模板"
    ws.append(HEADERS)
    for row in ROWS:
        ws.append(row)
    # 公式列：第 12 列（L 列）= 分子(F) + 分母(G)
    for r in range(2, len(ROWS) + 2):
        ws.cell(row=r, column=12).value = f"=F{r}+G{r}"
    wb.save(OUT)
    print(f"已生成 {OUT}（{len(ROWS)} 行 × {len(HEADERS)} 列，第 12 列为公式列）")


if __name__ == "__main__":
    main()
