#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""采集表格内插件三态的**真实运行截图**。

三态（与 ``macro/vba/RegVal.bas`` 的三态一一对应）：

===========  ===================================================  ==========================================
状态         触发样例                                             状态条上的那句话
===========  ===================================================  ==========================================
正常         ``tests/fixtures/mini.csv``（有口径问题）             「已校验 6 行 · 2 项口径问题 · 0 项疑似重复待确认」
空结果       ``tests/fixtures/clean.csv``（零问题）               「已校验 5 行 · 未发现口径问题」
             ``tests/fixtures/header_only.csv``（没有行）         「表里没有数据行 · 未做校验」
错误         ``--expect-schema-version regval.contract/9.9.9``    「错误码：CONTRACT_VERSION_MISMATCH · 引擎与宏版本不一致，请升级」
===========  ===================================================  ==========================================

**这些图是什么**：引擎侧的真实运行输出（本脚本真的跑了引擎，把 stdout/stderr
照原样渲染成终端画面）。宏侧状态条上写的就是同一句话 —— 同一份 ``status_line``、
同一份文案常量表（``src/ui/messages.rs`` → ``macro/vba/RegValMessages.bas``），
两边不会各说一套。

**这些图不是什么**：不是 Excel 里的实际渲染 —— 本机没有 Excel / WPS，跑不了加载宏。
这一点在 README 的「概念 / 实测对照」里如实标注，不拿终端截图冒充 Excel 截图。

用法::

    python3 scripts/capture_ui_states.py            # 用 target/debug/regval
    python3 scripts/capture_ui_states.py --bin target/release/regval
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import unicodedata
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

PROJECT_DIR = Path(__file__).resolve().parent.parent
SHOTS_DIR = PROJECT_DIR / "docs" / "screenshots"

# 终端配色（深色底 + 等宽字）
BG = (22, 27, 34)
TITLE_BAR = (13, 17, 23)
FG = (201, 209, 217)
PROMPT = (126, 231, 135)
ERR = (248, 81, 73)
KEY = (255, 196, 87)
DIM = (110, 118, 129)

MONO_CANDIDATES = [
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Courier.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
]
CJK_CANDIDATES = [
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
]


def pick_font(candidates: list[str], size: int) -> ImageFont.FreeTypeFont:
    for path in candidates:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
    raise SystemExit("找不到可用的字体：{}".format(", ".join(candidates)))


def cell_width(ch: str) -> int:
    """一个字符占几个终端格（中日韩字符占两格）。"""
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


def render_terminal(lines: list[tuple[str, tuple[int, int, int]]], out_path: Path, size: int = 15) -> None:
    """把 [(文本, 颜色)] 渲染成一张终端窗口截图。"""
    mono = pick_font(MONO_CANDIDATES, size)
    cjk = pick_font(CJK_CANDIDATES, size)

    cell = round(mono.getlength("M"))
    line_h = round(size * 1.55)
    pad = 18
    bar_h = 30

    cols = max((sum(cell_width(c) for c in text) for text, _ in lines), default=40)
    width = pad * 2 + cols * cell
    height = bar_h + pad * 2 + line_h * len(lines)

    img = Image.new("RGB", (width, height), BG)
    d = ImageDraw.Draw(img)

    # 标题条
    d.rectangle([0, 0, width, bar_h], fill=TITLE_BAR)
    for i, color in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
        d.ellipse([14 + i * 18, bar_h // 2 - 5, 24 + i * 18, bar_h // 2 + 5], fill=color)

    y = bar_h + pad
    for text, color in lines:
        x = pad
        for ch in text:
            w = cell_width(ch)
            if ord(ch) < 128:
                d.text((x, y), ch, font=mono, fill=color)
            else:
                d.text((x, y), ch, font=cjk, fill=color)
            x += w * cell
        y += line_h

    out_path.parent.mkdir(parents=True, exist_ok=True)
    img.save(out_path)
    print("截图已写出：{}（{}×{}）".format(out_path, width, height))


def run_engine(binary: Path, args: list[str]) -> tuple[int, str, str]:
    env = dict(os.environ)
    # 结构化日志走 stderr；这一档只看人话，日志行留给排障时看
    env["REGVAL_LOG_LEVEL"] = "error"
    proc = subprocess.run(
        [str(binary)] + args,
        cwd=str(PROJECT_DIR),
        env=env,
        capture_output=True,
        text=True,
    )
    return proc.returncode, proc.stdout, proc.stderr


def shell_quote(arg: str) -> str:
    return '"{}"'.format(arg) if (" " in arg or "中" in arg) else arg


def lines_from(text: str) -> list[str]:
    return [ln.rstrip() for ln in text.splitlines() if ln.strip()]


def scenario_normal(binary: Path) -> list[tuple[str, tuple[int, int, int]]]:
    cmd = "regval validate --input tests/fixtures/mini.csv --out .regval/ui-states/result.json"
    code, out, err = run_engine(
        binary,
        [
            "validate",
            "--input",
            "tests/fixtures/mini.csv",
            "--out",
            ".regval/ui-states/result.json",
            "--state-db",
            ".regval/ui-states/ui-states-normal.db",
        ],
    )
    lines = [("$ " + cmd, PROMPT)]
    for ln in lines_from(out):
        lines.append((ln, KEY if ln.startswith("已校验") else FG))
    for ln in lines_from(err):
        lines.append((ln, ERR))
    lines.append(("", FG))
    lines.append(("# 退出码 {} · 状态条那一行与宏里显示的逐字相同".format(code), DIM))
    return lines


def scenario_empty(binary: Path) -> list[tuple[str, tuple[int, int, int]]]:
    cmd = "regval validate --input tests/fixtures/clean.csv --out .regval/ui-states/result.json"
    code, out, err = run_engine(
        binary,
        [
            "validate",
            "--input",
            "tests/fixtures/clean.csv",
            "--out",
            ".regval/ui-states/result.json",
            "--state-db",
            ".regval/ui-states/ui-states-empty.db",
        ],
    )
    lines = [("$ " + cmd, PROMPT)]
    for ln in lines_from(out):
        lines.append((ln, KEY if ln.startswith("已校验") else FG))

    cmd2 = "regval validate --input tests/fixtures/header_only.csv --out .regval/ui-states/result.json"
    code2, out2, err2 = run_engine(
        binary,
        [
            "validate",
            "--input",
            "tests/fixtures/header_only.csv",
            "--out",
            ".regval/ui-states/result.json",
            "--state-db",
            ".regval/ui-states/ui-states-no-rows.db",
        ],
    )
    lines.append(("", FG))
    lines.append(("$ " + cmd2, PROMPT))
    for ln in lines_from(out2):
        lines.append((ln, KEY if ln.startswith("表里没有数据行") else FG))
    for ln in lines_from(err2):
        lines.append((ln, ERR))
    lines.append(("", FG))
    lines.append(
        (
            "# 退出码 {} / {} · 「没有问题」与「没有行」是两种空，说法不一样".format(code, code2),
            DIM,
        )
    )
    return lines


def scenario_version_mismatch(binary: Path) -> list[tuple[str, tuple[int, int, int]]]:
    cmd = (
        "regval validate --input tests/fixtures/mini.csv --out .regval/ui-states/result.json "
        "--expect-schema-version regval.contract/9.9.9"
    )
    code, out, err = run_engine(
        binary,
        [
            "validate",
            "--input",
            "tests/fixtures/mini.csv",
            "--out",
            ".regval/ui-states/result.json",
            "--state-db",
            ".regval/ui-states/ui-states-mismatch.db",
            "--expect-schema-version",
            "regval.contract/9.9.9",
            "--error-out",
            ".regval/ui-states/ui-states-mismatch.error.json",
        ],
    )
    lines = [("$ " + cmd, PROMPT)]
    for ln in lines_from(out):
        lines.append((ln, FG))
    for ln in lines_from(err):
        lines.append((ln, ERR))

    err_path = PROJECT_DIR / ".regval/ui-states/ui-states-mismatch.error.json"
    if err_path.exists():
        import json

        report = json.loads(err_path.read_text(encoding="utf-8"))
        lines.append(("", FG))
        lines.append(("# 引擎写的错误报告（宏按字段读，不解析 stderr）：", DIM))
        lines.append(('  code     = "{}"'.format(report.get("code", "")), KEY))
        lines.append(('  code_zh  = "{}"'.format(report.get("code_zh", "")), KEY))
        lines.append(("  → 状态条写：错误码：{} · {}".format(report.get("code", ""), report.get("code_zh", "")), KEY))

    lines.append(("", FG))
    lines.append(("# 退出码 {} · 宏侧中止写回并弹一次对话框，不做部分写回".format(code), DIM))
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description="采集表格内插件三态的真实运行截图")
    parser.add_argument(
        "--bin",
        default=os.environ.get("REGVL_BIN", ""),
        help="regval 二进制路径（缺省用 target/debug/regval）",
    )
    args = parser.parse_args()

    binary = Path(args.bin) if args.bin else PROJECT_DIR / "target/debug/regval"
    if not binary.exists():
        print("找不到引擎二进制：{}（先 cargo build）".format(binary), file=sys.stderr)
        return 1

    for name, builder in [
        ("ui-state-normal.png", scenario_normal),
        ("ui-state-empty.png", scenario_empty),
        ("ui-state-version-mismatch.png", scenario_version_mismatch),
    ]:
        render_terminal(builder(binary), SHOTS_DIR / name)

    print("三态截图已写出到 {}".format(SHOTS_DIR))
    return 0


if __name__ == "__main__":
    sys.exit(main())
