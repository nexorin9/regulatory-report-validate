#!/usr/bin/env node
// ======================================================================
// WPS JSA 宿主模拟器 —— 把 macro/wps/regval.js 装进一个最小宿主里真跑一遍
// ======================================================================
//
// 【为什么有这个文件】
//
// 三入口同构要证明「WPS JSA 宏写回的东西与 Excel 宏、与 CSV 直跑一致」。
// 光读宏源码证明不了写回：宏里那段分块写回、状态条定位、版本闸门都是
// 运行时才知道对不对。所以这里做一个**宿主替身**：把 WPS 表格软件提供的
// 几个对象（Application / FileSystem / Workbook / Worksheet / Shape）按
// 宏用到的那些接口实现出来，把真宏原封不动装进去，点一次「校验」，
// 然后把表模型、契约、状态条、错误信息一起倒出来给测试核对。
//
// 【它是什么，不是什么】
//
//   · 它是**测试替身**，不是产品代码，也不进产品目录 —— 只服务本测试。
//   · 它替的是**宿主**，不是引擎：引擎调的是真的 regval 二进制，
//     跑的是真的口径规则、真的契约文件。
//   · 宏本身（macro/wps/regval.js）是产品代码，本文件一个字都不改它。
//
// 【用法】
//
//   node run.js --model <表模型.json> --macro <regval.js> --dump <结果.json>
//               [--engine <regval 二进制>] [--inject-contract <改过的契约>]
//
//   --model  表模型 JSON：{"full_name": "...xlsx", "path": "...",
//                          "sheets": [{"name": "mini", "rows": [["表头", ...], ...]}]}
//   --dump   结果 JSON：表模型（写回后）、契约、状态条、错误信息、按钮
//   --inject-contract  把引擎产出的契约替换成这个文件（测版本闸门用）
//
// ======================================================================

'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const vm = require('vm');
const { spawnSync } = require('child_process');

// ----------------------------------------------------------------------
// 命令行参数
// ----------------------------------------------------------------------
function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.slice(0, 2) !== '--') continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.slice(0, 2) === '--') {
      out[key] = true;
    } else {
      out[key] = next;
      i++;
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
if (!args.model || !args.macro || !args.dump) {
  console.error('用法：node run.js --model <表模型.json> --macro <regval.js> --dump <结果.json>');
  process.exit(2);
}

// ----------------------------------------------------------------------
// 录痕：宏做了什么，统统记下来给测试核对
// ----------------------------------------------------------------------
const trace = {
  msgboxes: [],        // 弹过的框（提示层级用得上）
  windowHint: '',      // 窗口状态栏最后一句
  engineCmds: [],      // 调引擎的命令行
  fileReads: [],       // 读过的文件
  fileWrites: [],      // 写过的文件
  cell_writes: [],      // 单元格写入序列（加载态取证用）
};

// ----------------------------------------------------------------------
// 单元格 / 区域 / 形状
// ----------------------------------------------------------------------

// A1 式地址 → [行, 列]（列是 1 起的）
function parseAddr(a1) {
  const m = /^([A-Za-z]+)(\d+)$/.exec(String(a1).trim());
  if (!m) return [1, 1];
  let col = 0;
  const letters = m[1].toUpperCase();
  for (let i = 0; i < letters.length; i++) {
    col = col * 26 + (letters.charCodeAt(i) - 64);
  }
  return [parseInt(m[2], 10), col];
}

// ----------------------------------------------------------------------
// 样式槽：宏往单元格里写的填充色/字体，记下来随 dump 一起导出
// ----------------------------------------------------------------------

function styleSlot(sheet, row, col) {
  const key = row + ',' + col;
  if (!sheet._styles[key]) {
    sheet._styles[key] = {
      fill: null,
      fontColor: null,
      fontName: null,
      fontSize: null,
      bold: null,
    };
  }
  return sheet._styles[key];
}

function makeInteriorProxy(sheet, coords) {
  return {
    get Color() {
      const s = styleSlot(sheet, coords[0][0], coords[0][1]);
      return s.fill === null ? 0 : s.fill;
    },
    set Color(v) {
      for (const [r, c] of coords) styleSlot(sheet, r, c).fill = v;
    },
  };
}

function makeFontProxy(sheet, coords) {
  const read = function (key, fallback) {
    const s = styleSlot(sheet, coords[0][0], coords[0][1]);
    return s[key] === null || s[key] === undefined ? fallback : s[key];
  };
  const write = function (key, v) {
    for (const [r, c] of coords) styleSlot(sheet, r, c)[key] = v;
  };
  return {
    get Color() { return read('fontColor', 0); },
    set Color(v) { write('fontColor', v); },
    get Name() { return read('fontName', ''); },
    set Name(v) { write('fontName', v); },
    get Size() { return read('fontSize', 0); },
    set Size(v) { write('fontSize', v); },
    get Bold() { return read('bold', false); },
    set Bold(v) { write('bold', v); },
  };
}

function makeCell(sheet, row, col) {
  const cell = {
    // 样式代理：宏往上写什么，这里就如实记到单元格的样式槽里
    // （录下来的填充色/字体是宏真写下去的，不是替身自己编的）
    Interior: makeInteriorProxy(sheet, [[row, col]]),
    Font: makeFontProxy(sheet, [[row, col]]),
    Borders: {
      Item: function () {
        return { LineStyle: 0, Weight: 0, Color: 0 };
      },
    },
  };
  Object.defineProperty(cell, 'Value', {
    get: function () { return sheet.getCell(row, col); },
    set: function (v) { sheet.setCell(row, col, v); },
    enumerable: true,
  });
  return cell;
}

function rangeCoords(row1, col1, row2, col2) {
  const coords = [];
  for (let r = row1; r <= row2; r++) {
    for (let c = col1; c <= col2; c++) coords.push([r, c]);
  }
  return coords;
}

function makeRange(sheet, addr) {
  const parts = String(addr).split(':');
  const r1c1 = parseAddr(parts[0]);
  const r2c2 = parseAddr(parts[1] || parts[0]);
  const row1 = r1c1[0];
  const col1 = r1c1[1];
  const row2 = r2c2[0];
  const col2 = r2c2[1];

  const range = {
    // 区间上的样式设置同样落到覆盖到的每个单元格上
    Interior: makeInteriorProxy(sheet, rangeCoords(row1, col1, row2, col2)),
    Font: makeFontProxy(sheet, rangeCoords(row1, col1, row2, col2)),
    HorizontalAlignment: 0,
    VerticalAlignment: 0,
    merged: false,
    Merge: function () { range.merged = true; },
  };

  Object.defineProperty(range, 'Value', {
    get: function () {
      if (row1 === row2 && col1 === col2) return sheet.getCell(row1, col1);
      const out = [];
      for (let r = row1; r <= row2; r++) {
        const line = [];
        for (let c = col1; c <= col2; c++) line.push(sheet.getCell(r, c));
        out.push(line);
      }
      return out;
    },
    set: function (v) {
      if (Array.isArray(v)) {
        for (let i = 0; i < v.length; i++) {
          const line = v[i];
          if (Array.isArray(line)) {
            for (let j = 0; j < line.length; j++) sheet.setCell(row1 + i, col1 + j, line[j]);
          } else {
            sheet.setCell(row1 + i, col1, line);
          }
        }
      } else {
        sheet.setCell(row1, col1, v);
      }
    },
    enumerable: true,
  });
  return range;
}

function makeShapes() {
  const list = [];
  return {
    get Count() { return list.length; },
    Item: function (i) {
      if (typeof i === 'number') return list[i - 1];
      for (let k = 0; k < list.length; k++) {
        if (list[k].Name === i) return list[k];
      }
      return undefined;
    },
    AddShape: function (type, left, top, width, height) {
      const shp = {
        Name: '',
        OnAction: '',
        Type: type,
        Left: left,
        Top: top,
        Width: width,
        Height: height,
        TextFrame: {
          Characters: {
            Text: '',
            Font: { Name: '', Size: 0, Bold: false, Color: 0 },
          },
          HorizontalAlignment: 0,
          VerticalAlignment: 0,
        },
        Fill: { ForeColor: { RGB: 0 } },
        Line: { Visible: true },
        Delete: function () {
          const idx = list.indexOf(shp);
          if (idx >= 0) list.splice(idx, 1);
        },
      };
      list.push(shp);
      return shp;
    },
    _list: list,
  };
}

function makeSheet(spec) {
  const rows = (spec.rows || []).map(function (r) { return r.slice(); });
  const sheet = {
    Name: spec.name,
    _rows: rows,
    _styles: {},
    getCell: function (r, c) {
      const row = rows[r - 1];
      if (!row) return '';
      const v = row[c - 1];
      return v === undefined ? '' : v;
    },
    setCell: function (r, c, v) {
      while (rows.length < r) rows.push([]);
      const row = rows[r - 1];
      while (row.length < c) row.push('');
      row[c - 1] = v;
      // 写入序列：谁先写、谁后写，录下来 —— 加载态那一句就靠它取证
      trace.cell_writes.push({ sheet: sheet.Name, row: r, col: c, value: v });
    },
    Cells: function (r, c) { return makeCell(sheet, r, c); },
    Range: function (addr) { return makeRange(sheet, addr); },
    Columns: function () { return { ColumnWidth: 8.43 }; },
    Rows: function () { return { RowHeight: 15 }; },
  };
  sheet.Shapes = makeShapes();
  return sheet;
}

// ----------------------------------------------------------------------
// 表模型 → 工作簿
// ----------------------------------------------------------------------
const model = JSON.parse(fs.readFileSync(args.model, 'utf8'));
const sheets = (model.sheets || []).map(makeSheet);
const book = {
  FullName: model.full_name,
  Path: model.path,
  Saved: true,
  Worksheets: {
    get Count() { return sheets.length; },
    Item: function (i) {
      if (typeof i === 'number') return sheets[i - 1];
      for (let k = 0; k < sheets.length; k++) {
        if (sheets[k].Name === i) return sheets[k];
      }
      return null;
    },
  },
};

// ----------------------------------------------------------------------
// FileSystem（WPS 的文件读写接口）
// ----------------------------------------------------------------------
const FileSystem = {
  FileExists: function (p) {
    try { return fs.existsSync(p); } catch (e) { return false; }
  },
  DeleteFile: function (p) {
    try { if (fs.existsSync(p)) fs.unlinkSync(p); } catch (e) { /* 删不掉就留着 */ }
  },
  OpenTextFile: function (p) {
    let content = '';
    try { content = fs.existsSync(p) ? fs.readFileSync(p, 'utf8') : ''; } catch (e) { content = ''; }
    let pos = 0;
    const stream = {
      ReadAll: function () { return content; },
      ReadLine: function () {
        if (pos >= content.length) return null;
        const idx = content.indexOf('\n', pos);
        if (idx < 0) { const line = content.slice(pos); pos = content.length; return line; }
        const line = content.slice(pos, idx);
        pos = idx + 1;
        return line;
      },
      Close: function () {},
    };
    trace.fileReads.push(p);
    return stream;
  },
  CreateTextFile: function (p) {
    const chunks = [];
    const stream = {
      Write: function (t) { chunks.push(String(t)); },
      Close: function () {
        fs.writeFileSync(p, chunks.join(''));
        trace.fileWrites.push(p);
      },
    };
    return stream;
  },
};

// ----------------------------------------------------------------------
// Application（宿主入口）
// ----------------------------------------------------------------------
function outPathFromCmd(cmd) {
  const m = /--out\s+"([^"]+)"/.exec(cmd);
  return m ? m[1] : null;
}

const Application = {
  StatusBar: '',
  DisplayAlerts: true,
  TempPath: os.tmpdir(),
  PathSeparator: path.sep,
  Workbooks: { Count: 1 },
  ActiveWorkbook: book,
  ActiveSheet: sheets[0],
  MsgBox: function (text) {
    trace.msgboxes.push(String(text));
    return 'yes';
  },
  // 调引擎：宏给的是一整条命令行，退出码就是它对不对的信号
  Run: function (cmd) {
    trace.engineCmds.push(String(cmd));
    const res = spawnSync(String(cmd), { shell: true, encoding: 'utf8' });
    const status = res.status === null || res.status === undefined ? 1 : res.status;
    // 版本闸门演练：把引擎产出的契约换成另一份（引擎本身跑的是真的）
    if (status === 0 && args['inject-contract']) {
      const outPath = outPathFromCmd(String(cmd));
      if (outPath) {
        fs.copyFileSync(String(args['inject-contract']), outPath);
        trace.engineCmds.push('[inject-contract] ' + outPath);
      }
    }
    return status;
  },
};

// ----------------------------------------------------------------------
// 装宏
// ----------------------------------------------------------------------
const sandbox = {
  FileSystem: FileSystem,
  Application: Application,
  console: console,
  // 引擎路径走环境变量（宏里优先读它，读不到才用文件顶部的常量）
  process: { env: { REGVAL_ENGINE_PATH: args.engine || '' } },
};
sandbox.global = sandbox;
vm.createContext(sandbox);

const macroCode = fs.readFileSync(args.macro, 'utf8');
vm.runInContext(macroCode, sandbox, { filename: String(args.macro) });

// 部署常量：宏顶部的引擎路径与状态库路径是「装在哪台机器上」的配置，
// 院内部署时改那两行。宿主模拟器等价地做同一件事（不改宏的逻辑）。
if (args['state-db']) sandbox.REGVAL_STATE_DB = String(args['state-db']);

if (typeof sandbox.RegVal_Validate !== 'function') {
  console.error('宏里没有 RegVal_Validate，装载失败');
  process.exit(3);
}

// 点一次「校验」
let threw = null;
try {
  sandbox.RegVal_Validate();
} catch (e) {
  threw = String(e && e.stack ? e.stack : e);
}

// ----------------------------------------------------------------------
// 倒出结果
// ----------------------------------------------------------------------
const outPath = path.join(model.path, '.regval_result.json');
let contract = null;
let contractText = '';
try {
  contractText = fs.readFileSync(outPath, 'utf8');
  contract = JSON.parse(contractText);
} catch (e) {
  contractText = '';
}

const dump = {
  threw: threw,
  contract_path: outPath,
  contract_text: contractText,
  contract: contract,
  error: {
    has_error: !!sandbox.gHasError,
    code: sandbox.gErrorCode || '',
    code_zh: sandbox.gErrorZh || '',
    message: sandbox.gErrorMessage || '',
    advice: sandbox.gErrorAdvice || '',
  },
  style_error: sandbox.gStyleError || '',
  status_error: sandbox.gStatusError || '',
  ui_error: sandbox.gUiError || '',
  batch_ref: sandbox.gLastBatchRef || '',
  written_sheets: sandbox.gWrittenSheets || '',
  msgboxes: trace.msgboxes,
  window_hint: Application.StatusBar,
  engine_cmds: trace.engineCmds,
  file_reads: trace.fileReads,
  file_writes: trace.fileWrites,
  cell_writes: trace.cell_writes,
  buttons: sheets.map(function (s) {
    return s.Shapes._list.map(function (shp) {
      return {
        name: shp.Name,
        caption: shp.TextFrame.Characters.Text,
        on_action: shp.OnAction,
        fill_bgr: shp.Fill.ForeColor.RGB,
        text_bgr: shp.TextFrame.Characters.Font.Color,
      };
    });
  }),
  sheets: sheets.map(function (s) {
    // 样式是宏真写下去的（填充色/字体/加粗），按格导出来给截图脚本用
    const styles = s._rows.map(function (_row, ri) {
      return (s._rows[ri] || []).map(function (_v, ci) {
        return s._styles[(ri + 1) + ',' + (ci + 1)] || null;
      });
    });
    return { name: s.Name, rows: s._rows, styles: styles };
  }),
};

fs.writeFileSync(String(args.dump), JSON.stringify(dump, null, 2));
console.log('WPS 宿主模拟器跑完 · 结果写入 ' + args.dump);
