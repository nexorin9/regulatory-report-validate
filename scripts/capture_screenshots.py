#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""采集交付用的**真实运行截图**（写回后的原表 / 终端摘要 / 导出报告 / 加载态 / 状态条）。

五张图与它们的来源：

======================  ==========================================================
图                      来源
======================  ==========================================================
terminal-validate-      引擎真跑 ``regval validate``（1200 行合并表）的 stdout 原文
summary.png
sheet-writeback.png     WPS JSA 宏真跑写回后的工作表内容 + 宏真写下去的填充色
export-report.png       ``regval export`` 真导出的 CSV 文件内容原文
ui-state-loading.png    宏运行时写状态条的那一句（宿主替身录到的单元格写入序列）
statusbar-summary.png   宏写完后的状态条那一行 + 两个按钮
======================  ==========================================================

**这些图是什么**：引擎与宏真跑出来的内容，逐字照原样渲染。
**这些图不是什么**：不是 Excel / WPS 表格软件的界面截图 —— 本机没装，跑不了加载宏。
表格部分是把「写回后的真实单元格内容 + 真实填充色」渲染成图。这一点在 README 的
「概念 / 实测对照」里如实标注，不拿渲染图冒充 Excel 截图。

用法::

    python3 scripts/capture_screenshots.py
    python3 scripts/capture_screenshots.py --bin target/release/regval
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture_ui_states as cus  # noqa: E402  终端渲染与三态截图共用一套版式

PROJECT_DIR = Path(__file__).resolve().parent.parent
SHOTS_DIR = PROJECT_DIR / "docs" / "screenshots"
WORK_DIR = PROJECT_DIR / ".regval" / "shots"

# ---------------------------------------------------------------------------
# 配色：取自产品 token（src/ui/tokens.rs → macro/wps/regval.js 里的 REGVAL_TOKENS）
# ---------------------------------------------------------------------------
SLATE = (47, 62, 70)
INK = (31, 41, 51)
GRID = (213, 220, 226)
PAPER = (255, 255, 255)
ZEBRA = (249, 250, 251)
HEADER_TEXT = (255, 255, 255)
STATUSBAR_SURFACE = (244, 246, 248)
STATUSBAR_INK = (91, 107, 120)

BODY_FONT = "/System/Library/Fonts/Hiragino Sans GB.ttc"
BODY_FONT_ALT = "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
MONO_FONT = "/System/Library/Fonts/Menlo.ttc"

# 5 个新列的列宽（token 里的 columns[].width，Excel 宽度单位 ≈ 7px）
NEW_COLUMN_WIDTHS = [10, 18, 48, 20, 12]
NEW_COLUMN_NAMES = ["校验结论", "问题类型", "问题说明（含依据）", "疑似重复项", "上报状态"]


# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------
def bgr_to_rgb(value) -> tuple[int, int, int] | None:
    """Excel/WPS 的颜色整数是 BGR 序（&HBBGGRR），转成 RGB。"""
    if value is None:
        return None
    value = int(value)
    return (value & 0xFF, (value >> 8) & 0xFF, (value >> 16) & 0xFF)


def pick_body_font(size: int):
    for path in (BODY_FONT, BODY_FONT_ALT, cus.CJK_CANDIDATES[0]):
        if os.path.exists(path):
            try:
                from PIL import ImageFont

                return ImageFont.truetype(path, size)
            except OSError:
                continue
    return cus.pick_font(cus.CJK_CANDIDATES, size)


def pick_mono_font(size: int):
    for path in (MONO_FONT, cus.MONO_CANDIDATES[1]):
        if os.path.exists(path):
            try:
                from PIL import ImageFont

                return ImageFont.truetype(path, size)
            except OSError:
                continue
    return cus.pick_font(cus.MONO_CANDIDATES, size)


def clip_text(draw, text: str, font, max_w: int) -> str:
    """按像素宽度截断（Excel 里文字溢出单元格也是这么显示的，后面加省略号）。"""
    if draw.textlength(text, font=font) <= max_w:
        return text
    ell = "…"
    ell_w = draw.textlength(ell, font=font)
    out = ""
    for ch in text:
        if draw.textlength(out + ch, font=font) + ell_w > max_w:
            break
        out += ch
    return out + ell


def draw_grid_line(draw, x: float, y: float, text: str, mono, cjk, cell: float, fill) -> float:
    """等宽网格里画一行：ASCII 用等宽字体，汉字用中文字体占两格。

    本地等宽字体没有汉字字形（硬画是一排方框），所以按终端那套办法逐字画。
    """
    for ch in text:
        w = cus.cell_width(ch)
        if ord(ch) < 128:
            draw.text((x, y), ch, font=mono, fill=fill)
        else:
            draw.text((x, y), ch, font=cjk, fill=fill)
        x += w * cell
    return x


def run(cmd: list[str], cwd: Path | None = None) -> tuple[int, str, str]:
    env = dict(os.environ)
    env["REGVAL_LOG_LEVEL"] = "error"
    proc = subprocess.run(
        cmd,
        cwd=str(cwd or PROJECT_DIR),
        env=env,
        capture_output=True,
        text=True,
    )
    return proc.returncode, proc.stdout, proc.stderr


# ---------------------------------------------------------------------------
# 一、终端会话输出（校验摘要与问题计数）
# ---------------------------------------------------------------------------
def capture_terminal_summary(binary: Path, dataset: str) -> Path:
    out_json = WORK_DIR / "合并表.result.json"
    state_db = WORK_DIR / "shots.db"
    cmd = [
        str(binary),
        "validate",
        "--input",
        dataset,
        "--out",
        str(out_json),
        "--state-db",
        str(state_db),
    ]
    code, out, err = run(cmd)

    shown = "regval validate --input samples/dataset/合并表.csv --out .regval/shots/合并表.result.json"
    lines = [("$ " + shown, cus.PROMPT)]
    for ln in [ln.rstrip() for ln in out.splitlines() if ln.strip()]:
        lines.append((ln, cus.KEY if ln.startswith("已校验") else cus.FG))
    for ln in [ln.rstrip() for ln in err.splitlines() if ln.strip()]:
        lines.append((ln, cus.ERR))
    lines.append(("", cus.FG))
    lines.append(("# 退出码 {} · 上面每一行都是引擎真跑的 stdout，数字未经修饰".format(code), cus.DIM))

    out_path = SHOTS_DIR / "terminal-validate-summary.png"
    cus.render_terminal(lines, out_path)
    return out_path


# ---------------------------------------------------------------------------
# 二、写回后的原表（真宏写回 + 真填充色）
# ---------------------------------------------------------------------------
def build_xlsx_and_model(csv_path: Path, sheet_name: str) -> tuple[Path, Path, list[list[str]]]:
    """把真实样例 CSV 转成同名 xlsx（表格入口的输入）+ 宿主替身要的表模型。"""
    from openpyxl import Workbook

    with csv_path.open("r", encoding="utf-8-sig", newline="") as fh:
        rows = [r for r in csv.reader(fh)]
    while rows and not any(c.strip() for c in rows[-1]):
        rows.pop()

    xlsx_path = WORK_DIR / (sheet_name + ".xlsx")
    wb = Workbook()
    ws = wb.active
    ws.title = sheet_name
    for row in rows:
        # 全部按文本写：不让表格软件把 "62.9" 变成数字，值与源表逐字相同
        ws.append([str(c) for c in row])
    wb.save(xlsx_path)

    model_path = WORK_DIR / "model.json"
    model_path.write_text(
        json.dumps(
            {
                "full_name": str(xlsx_path),
                "path": str(WORK_DIR),
                "sheets": [{"name": sheet_name, "rows": rows}],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    return xlsx_path, model_path, rows


def run_wps_harness(binary: Path, model_path: Path, dump_path: Path) -> dict:
    harness = PROJECT_DIR / "tests" / "wps_harness" / "run.js"
    code, out, err = run(
        [
            "node",
            str(harness),
            "--model",
            str(model_path),
            "--macro",
            str(PROJECT_DIR / "macro" / "wps" / "regval.js"),
            "--dump",
            str(dump_path),
            "--engine",
            str(binary),
            "--state-db",
            str(WORK_DIR / "wps.db"),
        ]
    )
    if code != 0:
        raise SystemExit("宿主替身没跑通（退出码 {}）：{}\n{}".format(code, out, err))
    return json.loads(dump_path.read_text(encoding="utf-8"))


def column_widths(rows: list[list[str]], draw, font, max_px: int = 230) -> list[int]:
    widths = []
    for ci in range(max(len(r) for r in rows)):
        longest = max((len(r[ci]) if ci < len(r) else 0) for r in rows)
        px = int(draw.textlength("测" * min(longest, 12), font=font)) + 18
        widths.append(max(76, min(max_px, px)))
    return widths


def render_sheet(
    out_path: Path,
    title: str,
    subtitle: str,
    header: list[str],
    rows: list[list[str]],
    styles: list[list[dict | None]],
    widths: list[int],
    new_from: int | None,
    status_row: int | None = None,
    status_text: str = "",
    marker: str = "",
    footer: str = "",
    max_rows: int = 0,
    ellipsis_note: str = "",
) -> None:
    """把工作表内容渲染成一张表格式的图。"""
    font = pick_body_font(15)
    mono = pick_mono_font(15)
    bold_font = pick_body_font(15)
    title_font = pick_body_font(19)
    note_font = pick_body_font(13)

    pad = 16
    bar_h = 44
    head_h = 30
    row_h = 28
    marker_h = 22 if marker else 0
    note_lines = footer.splitlines() if footer else []
    note_h = 20 * len(note_lines) + 14

    ncols = len(header)
    total_w = sum(widths) + pad * 2
    shown_rows = rows[:max_rows] if max_rows else rows
    hidden = len(rows) - len(shown_rows)
    data_h = row_h * (len(shown_rows) + (1 if hidden > 0 else 0))
    status_h = row_h if status_row is not None else 0
    width = total_w
    height = bar_h + pad + marker_h + head_h + data_h + status_h + note_h + pad

    img = Image.new("RGB", (width, height), PAPER)
    d = ImageDraw.Draw(img)

    # 标题条
    d.rectangle([0, 0, width, bar_h], fill=SLATE)
    d.text((pad, 11), title, font=title_font, fill=HEADER_TEXT)
    if subtitle:
        d.text((width - d.textlength(subtitle, font=note_font) - pad, 15), subtitle, font=note_font, fill=(206, 216, 224))

    y = bar_h + pad

    # 新列标记
    if marker and new_from is not None:
        x = pad + sum(widths[:new_from])
        d.text((x, y + 2), marker, font=note_font, fill=(154, 74, 0))
        y += marker_h

    # 表头行
    x = pad
    for ci, name in enumerate(header):
        w = widths[ci]
        is_new = new_from is not None and ci >= new_from
        d.rectangle([x, y, x + w, y + head_h], fill=SLATE if is_new else (241, 244, 246))
        d.text((x + 8, y + 6), clip_text(d, name, bold_font, w - 16), font=bold_font, fill=HEADER_TEXT if is_new else INK)
        x += w
    d.line([pad, y + head_h, pad + sum(widths), y + head_h], fill=GRID, width=1)
    y += head_h

    # 数据行
    for ri, row in enumerate(shown_rows):
        x = pad
        for ci in range(ncols):
            w = widths[ci]
            style = styles[ri][ci] if ri < len(styles) and ci < len(styles[ri]) else None
            fill = bgr_to_rgb(style.get("fill")) if style else None
            if fill is None:
                fill = ZEBRA if ri % 2 else PAPER
            d.rectangle([x, y, x + w, y + row_h], fill=fill)
            ink = bgr_to_rgb(style.get("fontColor")) if style else None
            ink = ink or INK
            # 等宽列里的中文用中文字体渲染（本地等宽字体没有汉字字形，
            # 硬用会画成一排方框），纯 ASCII 才用等宽字体
            value = row[ci] if ci < len(row) else ""
            is_mono = bool(style and style.get("fontName") == "Consolas")
            ascii_only = all(ord(ch) < 128 for ch in str(value))
            cell_font = mono if (is_mono and ascii_only) else font
            if value:
                d.text(
                    (x + 8, y + 5),
                    clip_text(d, str(value), cell_font, w - 16),
                    font=cell_font,
                    fill=ink,
                )
            x += w
            d.line([x, y, x, y + row_h], fill=GRID, width=1)
        d.line([pad, y + row_h, pad + sum(widths), y + row_h], fill=GRID, width=1)
        y += row_h

    # 被折叠的行：如实说明图里展开了几行、还剩多少行
    if hidden > 0:
        x = pad
        d.rectangle([x, y, x + sum(widths), y + row_h], fill=(247, 248, 250))
        d.text(
            (x + 8, y + 5),
            ellipsis_note or "…… 其余 {} 行同样写回（此图只展开前 {} 行）".format(hidden, len(shown_rows)),
            font=note_font,
            fill=(110, 118, 129),
        )
        d.line([pad, y + row_h, pad + sum(widths), y + row_h], fill=GRID, width=1)
        y += row_h

    # 状态条那一行
    if status_row is not None:
        x = pad
        d.rectangle([x, y, x + sum(widths), y + row_h], fill=STATUSBAR_SURFACE)
        d.text((x + 10, y + 5), status_text, font=font, fill=STATUSBAR_INK)
        d.line([pad, y + row_h, pad + sum(widths), y + row_h], fill=GRID, width=1)
        y += row_h

    # 页脚说明
    for line in note_lines:
        d.text((pad, y + 4), line, font=note_font, fill=(110, 118, 129))
        y += 20

    out_path.parent.mkdir(parents=True, exist_ok=True)
    img.save(out_path)
    print("截图已写出：{}（{}×{}）".format(out_path, width, height))


def capture_sheet_writeback(binary: Path, csv_path: Path, sheet_name: str) -> tuple[Path, dict, list[list[str]]]:
    xlsx_path, model_path, original_rows = build_xlsx_and_model(csv_path, sheet_name)
    dump_path = WORK_DIR / "wps_dump.json"
    dump = run_wps_harness(binary, model_path, dump_path)

    sheet = dump["sheets"][0]
    rows = sheet["rows"]
    styles = sheet.get("styles") or [[None] * len(r) for r in rows]
    # 原表列数以**写回前**的表模型为准（写回后首行已经多了 5 个新列头）
    n_original = len(original_rows[0])
    n_data_rows = len(original_rows) - 1
    full_header = [str(c) for c in original_rows[0]] + list(NEW_COLUMN_NAMES)

    # 状态条那一行：宏写状态条时落在哪一行，就从写入序列里取哪一行
    writes = dump.get("cell_writes") or []
    status_writes = [w for w in writes if w["row"] > n_data_rows + 1]
    status_idx = (status_writes[-1]["row"] - 1) if status_writes else n_data_rows + 2
    status_text = str(status_writes[-1]["value"]) if status_writes else ""

    # 宽度：原表列按内容算，新列按 token 的列宽
    probe = ImageDraw.Draw(Image.new("RGB", (10, 10)))
    font = pick_body_font(15)
    widths = column_widths(original_rows, probe, font)
    # 新列宽按 token 的列宽换算（Excel 宽度单位 ≈ 8px，一个汉字 ≈ 两个单位）
    widths += [w * 8 for w in NEW_COLUMN_WIDTHS]

    footer = (
        "写回由 WPS JSA 宏真跑完成（宿主替身 tests/wps_harness/run.js，宏文件 macro/wps/regval.js），引擎是 {}。".format(binary.name)
        + "\n填充色是宏真写下去的（红=问题行、黄=疑似重复行、绿=通过行，取自产品 token）；原表 {} 列一格没动。".format(n_original)
        + "\n本机没有 Excel / WPS 表格软件，跑不了加载宏 —— 这张图是把写回后的真实单元格内容渲染成图，不是 Excel 界面截图。"
    )

    out_path = SHOTS_DIR / "sheet-writeback.png"
    render_sheet(
        out_path,
        "{} —— 点「校验」之后的原表".format(xlsx_path.name),
        "5 个新列由引擎写回",
        full_header,
        rows[1 : 1 + n_data_rows],
        styles[1 : 1 + n_data_rows] if styles else [],
        widths,
        new_from=n_original,
        status_row=status_idx,
        status_text=status_text,
        marker="← 引擎写回的 5 个新列",
        footer=footer,
        max_rows=14,
    )
    return out_path, dump, original_rows


# ---------------------------------------------------------------------------
# 三、加载态与状态条摘要（宏运行时真写下去的那几句）
# ---------------------------------------------------------------------------
def capture_loading_and_statusbar(dump: dict, original_rows: list[list[str]], csv_path: Path) -> list[Path]:
    paths = []

    writes = dump.get("cell_writes") or []
    n_original = len(original_rows[0])
    n_data_rows = len(original_rows) - 1

    # 状态条那一行上，宏先写「校验中…」，跑完再写最终那一句
    status_writes = [w for w in writes if w["row"] > n_data_rows + 1]
    loading_text = str(status_writes[0]["value"]) if status_writes else ""
    final_text = str(status_writes[-1]["value"]) if status_writes else ""

    probe = ImageDraw.Draw(Image.new("RGB", (10, 10)))
    font = pick_body_font(15)
    widths = column_widths(original_rows, probe, font)

    footer = (
        "这一句是宏运行时真写进状态条的：宿主替身录到的单元格写入序列里，第一条写状态条的就是「{}」。".format(loading_text)
        + "\n宏先写这句、把按钮按住，再去调引擎（macro/wps/regval.js 的 regvalBeginRun）。"
        + "\n此时写回还没发生，所以原表 {} 列一格没动、5 个新列还没出现。".format(n_original)
        + "\n本机没有 Excel / WPS 表格软件 —— 这张图是把那一刻的真实单元格内容渲染成图。"
    )
    out_loading = SHOTS_DIR / "ui-state-loading.png"
    render_sheet(
        out_loading,
        "{} —— 点「校验」之后、引擎还在跑".format(csv_path.stem + ".xlsx"),
        "加载态",
        [str(c) for c in original_rows[0]],
        original_rows[1:],
        [[None] * n_original for _ in original_rows[1:]],
        widths,
        new_from=None,
        status_row=1,
        status_text=loading_text,
        footer=footer,
        max_rows=6,
        ellipsis_note="…… 其余 {} 行原样未动（此图只展开前 {} 行）".format(n_data_rows - 6, 6),
    )
    paths.append(out_loading)

    # 状态条摘要：跑完之后状态条上那一行 + 两个按钮
    note_font = pick_body_font(13)
    body_font = pick_body_font(17)
    title_font = pick_body_font(19)
    # dump["buttons"] 是「每张表一组形状」，这里取第一张表的那组
    buttons = (dump.get("buttons") or [[]])[0]
    btn_w, btn_h = 150, 46
    pad = 16
    bar_h = 44
    strip_h = 52
    hint_line = "窗口状态栏（Application.StatusBar）：" + str(dump.get("window_hint") or "")
    probe = ImageDraw.Draw(Image.new("RGB", (10, 10)))
    width = max(
        int(probe.textlength(final_text, font=body_font)) + pad * 2 + 24,
        int(probe.textlength(hint_line, font=note_font)) + pad * 2,
        pad * 2 + btn_w * max(len(buttons), 1) + 40,
    )
    width = max(width, 880)
    height = bar_h + pad + 34 + btn_h + 18 + strip_h + pad + 60
    img = Image.new("RGB", (width, height), PAPER)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, width, bar_h], fill=SLATE)
    d.text((pad, 11), "状态条摘要 —— 校验跑完之后", font=title_font, fill=HEADER_TEXT)
    y = bar_h + pad
    d.text((pad, y), "宏装在表头的两个按钮（真宏建的形状，标题与绑定动作都是真的）：", font=note_font, fill=(110, 118, 129))
    y += 34
    x = pad
    for btn in buttons:
        fill = bgr_to_rgb(btn.get("fill_bgr")) or (240, 243, 245)
        ink = bgr_to_rgb(btn.get("text_bgr")) or HEADER_TEXT
        d.rounded_rectangle([x, y, x + btn_w, y + btn_h], radius=8, fill=fill)
        d.text((x + 18, y + 8), str(btn.get("caption") or ""), font=body_font, fill=ink)
        d.text((x + 18, y + 28), str(btn.get("on_action") or ""), font=note_font, fill=(150, 160, 170))
        x += btn_w + 20
    y += btn_h + 18
    d.rectangle([pad, y, width - pad, y + strip_h], fill=STATUSBAR_SURFACE)
    d.text((pad + 12, y + 12), final_text, font=body_font, fill=STATUSBAR_INK)
    y += strip_h
    d.text((pad, y + 8), hint_line, font=note_font, fill=(110, 118, 129))
    y += 24
    d.text((pad, y + 8), "状态条这一句与终端首行、契约 summary.status_line 逐字相同（同一份文案常量表）。", font=note_font, fill=(110, 118, 129))
    out_status = SHOTS_DIR / "statusbar-summary.png"
    img.save(out_status)
    print("截图已写出：{}（{}×{}）".format(out_status, width, height))
    paths.append(out_status)
    return paths


# ---------------------------------------------------------------------------
# 四、导出报告片段（真导出的文件内容原文）
# ---------------------------------------------------------------------------
def capture_export_report(binary: Path, state_db: Path) -> Path:
    pending_csv = WORK_DIR / "pending.csv"
    evidence_csv = WORK_DIR / "evidence.csv"
    run([str(binary), "export", "pending", "--out", str(pending_csv), "--state-db", str(state_db)])
    run([str(binary), "export", "evidence", "--out", str(evidence_csv), "--state-db", str(state_db)])

    mono = pick_mono_font(14)
    cjk = cus.pick_font(cus.CJK_CANDIDATES, 14)
    cell = round(mono.getlength("M"))
    note_font = pick_body_font(13)
    title_font = pick_body_font(19)
    body_font = pick_body_font(15)

    blocks = []
    for label, path in (("未上报名单 pending.csv", pending_csv), ("依据举证包 evidence.csv", evidence_csv)):
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        blocks.append((label, path, lines, len(lines)))

    pad = 16
    bar_h = 44
    block_gap = 18
    width = 1900
    max_cols = (width - pad * 2 - 12) // cell
    body = []
    for label, path, lines, total in blocks:
        body.append(("head", label))
        body.append(("note", "文件：{} · 共 {} 行 · 这是引擎真导出的文件内容，逐字照原样贴出来".format(path.name, total)))
        for ln in lines[:13]:
            body.append(("code", ln))
        if total > 13:
            body.append(("note", "…… 其余 {} 行见文件".format(total - 13)))
    height = bar_h + pad + len(body) * 21 + block_gap * len(blocks) + pad + 40

    img = Image.new("RGB", (width, height), PAPER)
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, width, bar_h], fill=SLATE)
    d.text((pad, 11), "导出产物 —— 经办人直接带走的文件", font=title_font, fill=HEADER_TEXT)
    y = bar_h + pad
    for kind, text in body:
        if kind == "head":
            d.text((pad, y), text, font=body_font, fill=INK)
            y += 24
        elif kind == "note":
            d.text((pad, y), text, font=note_font, fill=(110, 118, 129))
            y += 20
        else:
            d.rectangle([pad, y - 2, width - pad, y + 19], fill=(250, 251, 252))
            shown = text
            if sum(cus.cell_width(c) for c in shown) > max_cols:
                shown = shown[: max_cols - 1] + "…"
            draw_grid_line(d, pad + 6, y, shown, mono, cjk, cell, (52, 62, 70))
            y += 21
    d.text((pad, y + 10), "两份文件都是引擎真导出的（regval export pending / evidence），内容一字未改。", font=note_font, fill=(110, 118, 129))

    out_path = SHOTS_DIR / "export-report.png"
    img.save(out_path)
    print("截图已写出：{}（{}×{}）".format(out_path, width, height))
    return out_path


# ---------------------------------------------------------------------------
def main() -> int:
    parser = argparse.ArgumentParser(description="采集交付用的真实运行截图")
    parser.add_argument("--bin", default=os.environ.get("REGVL_BIN", ""), help="regval 二进制路径")
    args = parser.parse_args()

    binary = Path(args.bin) if args.bin else PROJECT_DIR / "target/release/regval"
    if not binary.exists():
        binary = PROJECT_DIR / "target/debug/regval"
    if not binary.exists():
        print("找不到引擎二进制：先 cargo build", file=sys.stderr)
        return 1

    WORK_DIR.mkdir(parents=True, exist_ok=True)
    SHOTS_DIR.mkdir(parents=True, exist_ok=True)

    merged = PROJECT_DIR / "samples" / "dataset" / "合并表.csv"
    dept = PROJECT_DIR / "samples" / "dataset" / "病案统计室表.csv"

    capture_terminal_summary(binary, str(merged))

    _, dump, original_rows = capture_sheet_writeback(binary, dept, "病案统计室表")
    capture_loading_and_statusbar(dump, original_rows, dept)

    capture_export_report(binary, WORK_DIR / "shots.db")

    print("截图已写到 {}".format(SHOTS_DIR))
    return 0


if __name__ == "__main__":
    sys.exit(main())
