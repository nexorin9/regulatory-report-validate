// ======================================================================
// 卫健上报校验系统 —— WPS JSA 宏（表格内插件 · WPS 入口）
//
// 【本文件是模板】契约常量、视觉 token、提示文案由引擎注入：
//     cargo run -- macro --out macro/wps/regval.js
// 改逻辑改本模板，改样式改 src/ui/tokens.rs，改文案改 src/ui/messages.rs，
// 然后重新生成。宏里不写第二套口径 —— 常量只有引擎侧一处出处。
//
// 与 Excel 侧（macro/vba/RegVal.bas）同构：
//   · 两个按钮（校验 / 上报）+ 底部状态条，科室仍在自己那张表里干活
//   · 引擎产出的结果契约 JSON 是唯一交接面，schema_version 对不上就中止写回
//   · 写回只碰原表右侧 5 个新列，原表已有列、格式与公式一律不动
//   · 状态条句子取自契约 summary 段，宏不自行统计、不自行拼句子
//   · 引擎失败按 --error-out 写的错误报告 JSON 取错误码与中文说明
//     （不解析人类可读的 stderr —— 终端文案一改就断）
//
// 文件读写走 WPS 的 FileSystem API，调引擎走 Application.Run。
//
// 三入口（Excel 宏 / WPS JSA 宏 / CSV 直跑）产出同一份契约 JSON，
// 由 tests/three_entry_consistency_test.rs 逐字段核对。
// ======================================================================

// @regval:begin contract
// @regval:end contract

// @regval:begin tokens
// @regval:end tokens

// @regval:begin messages
// @regval:end messages

// ---- 部署常量（安装时按院内实际路径改这两行）----
// 引擎二进制路径。路径里有空格或中文也没问题：命令行里用引号包起来。
var REGVAL_ENGINE_PATH = "C:\\Program Files\\RegVal\\regval.exe";
// 本机校验库路径（状态留存处）
var REGVAL_STATE_DB = "C:\\Program Files\\RegVal\\state\\state.db";

// 状态条在数据下面空几行（与引擎 src/ui/statusbar.rs 的 STATUS_ROW_GAP 一致）
var REGVAL_STATUS_ROW_GAP = 2;
// 分块写回的行数：每块一次批量赋值，避免大表逐格写回卡顿
var REGVAL_CHUNK_ROWS = 200;
// 按钮尺寸
var REGVAL_BUTTON_W = 88;
var REGVAL_BUTTON_H = 26;
var REGVAL_BUTTON_GAP = 8;
// 表头行号
var REGVAL_HEADER_ROW = 1;
// 扫描表头时的列数上限（防止在空表上无限扫）
var REGVAL_SCAN_COLS = 256;

// ---- 本轮状态（与 Excel 侧同名变量一一对应）----
var gBusy = false;
var gHasError = false;
var gErrorCode = "";
var gErrorZh = "";
var gErrorMessage = "";
var gErrorAdvice = "";
// 样式套用失败时的说明（空 = 没失败）。数据写回与样式套用是两回事：
// 样式坏了不能把已经写好的数据说成「没改动」，所以单独记一句。
var gStyleError = "";
// 状态条没写上时的说明（与样式分开记：两件事的成因不一样）
var gStatusError = "";
// 界面装配失败时的说明（按钮装不上：用户点不到，等于没有界面）
var gUiError = "";
// 最近一次校验的批次号（「上报」按钮按批次走）
var gLastBatchRef = "";
// 本轮写过结果的工作表（竖线分隔；状态条写到这些表上）
var gWrittenSheets = "";


// ======================================================================
// 界面装配：两个按钮 + 起始态状态条
// ======================================================================

// 装界面（宏列表里选这个跑）：表右侧出现「校验」「上报」两个按钮，
// 数据下面出现一条浅灰状态条。重复运行不会堆按钮。
function RegVal_InstallBar() {
  var book = regvalTargetBook();
  if (!book) {
    regvalMsgBox(REGVAL_MESSAGES["no_workbook"]);
    return;
  }
  if (!book.Path) {
    regvalMsgBox(REGVAL_MESSAGES["not_saved"]);
    return;
  }

  regvalEnsureBar(book, regvalActiveSheet());

  regvalMsgBox(REGVAL_MESSAGES["installed"] + "\n" + REGVAL_MESSAGES["installed_hint"]);
}

// 静默装配：按钮不在就补上，不弹框。校验与上报都会先调它 ——
// 用户即使没跑过 RegVal_InstallBar，点一次校验也会把按钮装好。
function regvalEnsureBar(book, ws) {
  if (!ws) return;
  try {
    if (!regvalShapeExists(ws, "BtnRegValValidate")) {
      regvalAddButton(ws, "BtnRegValValidate", REGVAL_MESSAGES["button_validate"], "RegVal_Validate",
                      320, 2);
    }
    if (!regvalShapeExists(ws, "BtnRegValPush")) {
      regvalAddButton(ws, "BtnRegValPush", REGVAL_MESSAGES["button_push"], "RegVal_Push",
                      320 + REGVAL_BUTTON_W + REGVAL_BUTTON_GAP, 2);
    }
  } catch (e) {
    gUiError = RegValAsText(e);
  }
}

// 画一个圆角按钮（样式取自 token，不写死色值）
function regvalAddButton(ws, btnName, caption, action, left, top) {
  if (!ws.Shapes || typeof ws.Shapes.AddShape !== "function") {
    throw new Error("当前 WPS 版本不支持 Shapes.AddShape");
  }
  // 同名按钮先删掉再画（重复装界面不留残影）
  regvalRemoveShape(ws, btnName);
  var shp = ws.Shapes.AddShape(5 /* msoShapeRoundedRectangle */, left, top,
                               REGVAL_BUTTON_W, REGVAL_BUTTON_H);
  shp.Name = btnName;
  shp.OnAction = action;
  shp.TextFrame.Characters.Text = caption;
  var font = shp.TextFrame.Characters.Font;
  font.Name = REGVAL_TOKENS.fonts.body.name;
  font.Size = REGVAL_TOKENS.fonts.body.size;
  font.Bold = true;
  font.Color = RegValTextForRole("header");
  shp.Fill.ForeColor.RGB = RegValFillForRole("header");
  shp.Line.Visible = false;
  shp.TextFrame.HorizontalAlignment = -4108 /* xlHAlignCenter */;
  shp.TextFrame.VerticalAlignment = -4108 /* xlHAlignCenter */;
}

// 同名按钮先删掉再画（重复装界面不留残影）
function regvalRemoveShape(ws, btnName) {
  try {
    if (!ws.Shapes) return;
    var count = ws.Shapes.Count;
    for (var i = count; i >= 1; i--) {
      if (ws.Shapes.Item(i).Name === btnName) ws.Shapes.Item(i).Delete();
    }
  } catch (e) {
    // 删不掉不算失败：装按钮时会覆盖同名形状
  }
}

function regvalShapeExists(ws, btnName) {
  try {
    if (!ws.Shapes) return false;
    var count = ws.Shapes.Count;
    for (var i = 1; i <= count; i++) {
      if (ws.Shapes.Item(i).Name === btnName) return true;
    }
  } catch (e) {
    return false;
  }
  return false;
}


// ======================================================================
// 「校验」按钮
// ======================================================================

function RegVal_Validate() {
  regvalResetRunInfo();

  // 上一轮还没跑完就别再点：宏阻塞执行引擎时用户可能再点一次按钮
  if (gBusy) {
    regvalSetWindowHint(REGVAL_MESSAGES["busy"]);
    return;
  }

  var book = regvalTargetBook();
  if (!book) {
    regvalMsgBox(REGVAL_MESSAGES["no_workbook"]);
    return;
  }
  if (!regvalEnsureSaved(book)) return;

  var ws = regvalActiveSheet();
  regvalEnsureBar(book, ws);

  // 加载态：先把「校验中…」写上状态条、把按钮按住，再去调引擎
  regvalBeginRun(ws, REGVAL_MESSAGES["loading"]);

  var contractText = regvalRunValidateAndWriteBack(book);

  regvalEndRun();

  if (!contractText) {
    // 阻断级错误：状态条已写错误行，这里补一次对话框把错误码与处理建议说全
    regvalWriteErrorStatusLine(ws);
    regvalShowErrorDialog();
    return;
  }

  // 结果级：不弹框。单元格填充 + 状态条摘要已经说清楚了
  var contract = regvalParseJson(contractText);
  var statusLine = regvalSummaryString(contract, "status_line");
  if (!statusLine) statusLine = REGVAL_MESSAGES["summary_missing"];
  regvalApplyStatusLineToWrittenSheets(book, statusLine);
  regvalWriteRunSummary(book, contract, statusLine);

  if (gStyleError || gStatusError || gUiError) regvalShowStyleNotice();
}


// ======================================================================
// 「上报」按钮
// ======================================================================

function RegVal_Push() {
  regvalResetRunInfo();

  if (gBusy) {
    regvalSetWindowHint(REGVAL_MESSAGES["busy"]);
    return;
  }

  var book = regvalTargetBook();
  if (!book) {
    regvalMsgBox(REGVAL_MESSAGES["no_workbook"]);
    return;
  }
  if (!regvalEnsureSaved(book)) return;

  var ws = regvalActiveSheet();
  regvalEnsureBar(book, ws);

  // 先校验一次：上报按批次走，批次要与当前表里的内容对得上
  regvalBeginRun(ws, REGVAL_MESSAGES["loading_push"]);

  var contractText = regvalRunValidateAndWriteBack(book);
  if (!contractText) {
    regvalEndRun();
    regvalWriteErrorStatusLine(ws);
    regvalShowErrorDialog();
    return;
  }

  var contract = regvalParseJson(contractText);
  var batchRef = RegValAsText(contract.batch_ref);
  if (!batchRef) {
    regvalEndRun();
    regvalSetErrorInfo("NO_BATCH_REF", "", REGVAL_MESSAGES["no_batch"], "");
    regvalWriteErrorStatusLine(ws);
    regvalShowErrorDialog();
    return;
  }

  var enginePath = regvalResolveEnginePath();
  var errPath = regvalWorkDirFor(book) + ".regval_error.json";
  regvalDeleteFile(errPath);

  var cmd = regvalQuote(enginePath) + " push" +
            " --batch " + regvalQuote(batchRef) +
            " --state-db " + regvalQuote(REGVAL_STATE_DB) +
            " --error-out " + regvalQuote(errPath);

  var exitCode = regvalRunEngine(cmd);
  if (exitCode !== 0) {
    regvalReadErrorReport(errPath);
    regvalEndRun();
    regvalWriteErrorStatusLine(ws);
    regvalShowErrorDialog();
    return;
  }

  // 上报成功：再校验一次，把回执驱动的行状态回填到「上报状态」列
  contractText = regvalRunValidateAndWriteBack(book);
  regvalEndRun();
  if (!contractText) {
    regvalWriteErrorStatusLine(ws);
    regvalShowErrorDialog();
    return;
  }

  contract = regvalParseJson(contractText);
  var submissionLine = regvalSummaryString(contract, "submission_line");
  if (!submissionLine) submissionLine = REGVAL_MESSAGES["submit_done_fallback"];
  regvalApplyStatusLineToWrittenSheets(book, submissionLine);
  regvalWriteRunSummary(book, contract, submissionLine);

  if (gStyleError || gStatusError || gUiError) regvalShowStyleNotice();
}


// ======================================================================
// 加载态：开跑之前先把状态条写上、把按钮按住
// ======================================================================

function regvalBeginRun(ws, loadingText) {
  gBusy = true;
  // 先写状态条再调引擎：宏阻塞执行引擎时界面不重绘，
  // 这一句必须先画出来，用户才看得出「在跑」而不是「卡住了」
  if (ws) regvalSetStatusLine(ws, loadingText);
  regvalSetWindowHint(loadingText);
  regvalSetButtonsEnabled(ws, false);
}

function regvalEndRun() {
  gBusy = false;
  regvalSetButtonsEnabled(regvalActiveSheet(), true);
}

// 按钮启停：执行期间解掉 OnAction（点了没反应），底色换成中性灰
function regvalSetButtonsEnabled(ws, enabled) {
  if (!ws) return;
  regvalSetOneButtonEnabled(ws, "BtnRegValValidate", "RegVal_Validate", enabled);
  regvalSetOneButtonEnabled(ws, "BtnRegValPush", "RegVal_Push", enabled);
}

function regvalSetOneButtonEnabled(ws, btnName, actionName, enabled) {
  try {
    if (!ws.Shapes) return;
    var shp = null;
    var count = ws.Shapes.Count;
    for (var i = 1; i <= count; i++) {
      if (ws.Shapes.Item(i).Name === btnName) { shp = ws.Shapes.Item(i); break; }
    }
    if (!shp) return;
    if (enabled) {
      shp.OnAction = actionName;
      shp.Fill.ForeColor.RGB = RegValFillForRole("header");
      shp.TextFrame.Characters.Font.Color = RegValTextForRole("header");
    } else {
      shp.OnAction = "";
      shp.Fill.ForeColor.RGB = RegValFillForRole("neutral");
      shp.TextFrame.Characters.Font.Color = RegValTextForRole("neutral");
    }
  } catch (e) {
    gUiError = RegValAsText(e);
  }
}

// 窗口底部的提示（不弹框的轻量提示走这里）
function regvalSetWindowHint(text) {
  try {
    Application.StatusBar = text;
  } catch (e) {
    // 窗口状态栏不可用不影响主路径
  }
}


// ======================================================================
// 校验 → 写回
//
// 返回结果契约原文（成功）或空串（失败）。
// 失败时错误信息已经记在 gError* 里 —— 由调用方决定用哪个渠道说
// （状态条错误行 + 对话框），本函数自己不弹框。
// ======================================================================

function regvalRunValidateAndWriteBack(book) {
  regvalResetErrorInfo();

  var enginePath = regvalResolveEnginePath();
  var outPath = regvalWorkDirFor(book) + ".regval_result.json";
  var errPath = regvalWorkDirFor(book) + ".regval_error.json";

  if (!regvalFileExists(enginePath)) {
    regvalSetErrorInfo("ENGINE_NOT_FOUND", "",
                       RegValFmt(REGVAL_MESSAGES["engine_missing"], "path", enginePath),
                       REGVAL_MESSAGES["engine_missing_hint"]);
    return "";
  }

  // 上一次的错误报告先清掉：这一轮没报错的话，那个文件就是过期的
  regvalDeleteFile(errPath);

  // 绝对路径一律用引号包起来：路径里有空格或中文也不会散架
  var cmd = regvalQuote(enginePath) + " validate" +
            " --input " + regvalQuote(book.FullName) +
            " --out " + regvalQuote(outPath) +
            " --state-db " + regvalQuote(REGVAL_STATE_DB) +
            " --error-out " + regvalQuote(errPath);

  // 阻塞执行：等引擎跑完再读结果
  var exitCode = regvalRunEngine(cmd);
  if (exitCode !== 0) {
    regvalReadErrorReport(errPath);
    return "";
  }

  if (!regvalFileExists(outPath)) {
    regvalSetErrorInfo("CONTRACT_NOT_PRODUCED", "", REGVAL_MESSAGES["no_contract"], "");
    return "";
  }

  var jsonText = regvalReadTextFile(outPath);
  var contract = regvalParseJson(jsonText);
  if (!contract) {
    regvalSetErrorInfo("CONTRACT_NOT_PARSED", "", REGVAL_MESSAGES["no_contract"], "");
    return "";
  }

  // 版本闸门：不匹配就中止，绝不部分写回
  if (RegValAsText(contract.schema_version) !== REGVAL_SCHEMA_VERSION) {
    regvalSetErrorInfo("CONTRACT_VERSION_MISMATCH", RegValErrorText("CONTRACT_VERSION_MISMATCH"),
                       RegValFmt(REGVAL_MESSAGES["expect_value"], "value", REGVAL_SCHEMA_VERSION) + "\n" +
                       RegValFmt(REGVAL_MESSAGES["actual_value"], "value", contract.schema_version) + "\n" +
                       REGVAL_MESSAGES["untouched"],
                       RegValErrorText("CONTRACT_VERSION_MISMATCH"));
    return "";
  }

  // 列名闸门：契约里的 5 个新列必须与宏常量一致，否则同样中止
  var expected = REGVAL_COLUMN_NAMES.split("|");
  var actual = regvalColumnNames(contract);
  if (!regvalColumnsMatch(expected, actual)) {
    regvalSetErrorInfo("CONTRACT_VERSION_MISMATCH", RegValErrorText("CONTRACT_VERSION_MISMATCH"),
                       RegValFmt(REGVAL_MESSAGES["columns_expected"], "value", REGVAL_COLUMN_NAMES) + "\n" +
                       RegValFmt(REGVAL_MESSAGES["columns_actual"], "value", actual.join("|")) + "\n" +
                       REGVAL_MESSAGES["untouched"],
                       RegValErrorText("CONTRACT_VERSION_MISMATCH"));
    return "";
  }

  regvalWriteBackNewColumns(book, contract);

  // 批次号留档（「上报」按钮按批次走）
  gLastBatchRef = RegValAsText(contract.batch_ref);

  return jsonText;
}


// ======================================================================
// 写回：只写新列，原表已有列、格式与公式一律不动
// ======================================================================

function regvalWriteBackNewColumns(book, contract) {
  var expected = REGVAL_COLUMN_NAMES.split("|");
  var rows = contract.rows || [];
  if (!rows.length) return;

  gWrittenSheets = "";

  var i = 0;
  while (i < rows.length) {
    // 按来源表分组（契约里的行按工作表顺序排列）
    var sheetName = RegValAsText(rows[i].source_ns);
    var start = i;
    while (i < rows.length && RegValAsText(rows[i].source_ns) === sheetName) i++;
    var end = i - 1;

    var sheet = regvalSheetByName(book, sheetName);
    if (!sheet) continue;

    var firstNewCol = regvalFirstFreeColumn(sheet);
    regvalWriteHeaders(sheet, firstNewCol, expected);

    // 分块写回：每块一次批量赋值
    // chunkEnd 是这一块的**最后一行**（闭区间），不是「下一块的起点」——
    // 差一行就会去读表外的行，小表直接越界。
    var chunkStart = start;
    while (chunkStart <= end) {
      var chunkEnd = chunkStart;
      while (chunkEnd < end && (chunkEnd - chunkStart + 1) < REGVAL_CHUNK_ROWS) chunkEnd++;
      var nRows = chunkEnd - chunkStart + 1;
      var arr = [];
      for (var j = 0; j < nRows; j++) {
        var r = rows[chunkStart + j];
        arr.push([
          RegValAsText(r.conclusion),
          RegValAsText(r.issue_types),
          RegValAsText(r.issue_detail),
          RegValAsText(r.suspected_duplicate),
          RegValAsText(r.submission_status)
        ]);
      }
      regvalSetRangeValues(sheet, 2 + (chunkStart - start), firstNewCol, arr);
      chunkStart = chunkEnd + 1;
    }

    var lastRow = 2 + (end - start);

    // 样式：值写完之后再套（业务逻辑与样式分离）
    regvalApplyResultColumnStyles(sheet, firstNewCol, 2, lastRow);

    gWrittenSheets += sheetName + "|";
  }
}

// 列头写回原表首行之后（接在原表已有列之后，不动原表任何一格）
function regvalWriteHeaders(sheet, firstNewCol, expected) {
  for (var i = 0; i < expected.length; i++) {
    regvalSetCellValue(sheet, REGVAL_HEADER_ROW, firstNewCol + i, expected[i]);
  }
}


// ======================================================================
// 底部状态条
//
// 状态条写在数据下面一行（隔 REGVAL_STATUS_ROW_GAP 行留白），横跨 5 个新列
// —— 不碰原表已有列。
//
// 句子由引擎算好放在契约的 summary 段，宏原样显示；宏不自行统计、
// 不自行拼句子（表里与终端里必须是一句话）。
// ======================================================================

function regvalApplyStatusLineToWrittenSheets(book, text) {
  var names = gWrittenSheets ? gWrittenSheets.split("|") : [];
  var wroteAny = false;
  for (var i = 0; i < names.length; i++) {
    if (!names[i]) continue;
    var ws = regvalSheetByName(book, names[i]);
    if (ws) {
      regvalSetStatusLine(ws, text);
      wroteAny = true;
    }
  }

  // 一行都没写回（空表）时，状态条仍要落到当前表上 ——
  // 「没有行」也是一句话，不能因为没东西可写就不说
  if (!wroteAny) {
    var active = regvalActiveSheet();
    if (active) regvalSetStatusLine(active, text);
  }
}

// 写一行状态条（合并 5 个新列的那一行，浅灰底 + 深灰字）
function regvalSetStatusLine(sheet, text) {
  try {
    var firstNewCol = regvalFirstFreeColumn(sheet);
    var statusRow = regvalStatusRowFor(sheet);
    var rng = sheet.Range(regvalAddr(statusRow, firstNewCol) + ":" +
                          regvalAddr(statusRow, firstNewCol + 4));
    // 先写值再合并：合并后再给多格区域赋字符串，各家电子表格行为不一致
    regvalSetCellValue(sheet, statusRow, firstNewCol, text);
    if (typeof rng.Merge === "function") rng.Merge();
    rng.Interior.Color = RegValFillForRole("statusbar");
    rng.Font.Color = RegValTextForRole("statusbar");
    rng.Font.Name = REGVAL_TOKENS.fonts.body.name;
    rng.Font.Size = REGVAL_TOKENS.fonts.body.size;
    rng.HorizontalAlignment = -4131 /* xlLeft */;
    rng.VerticalAlignment = -4108 /* xlHAlignCenter */;

    // 窗口底部的状态栏也报一句：不看表头的人也能知道这一轮跑了什么
    regvalSetWindowHint(text);
  } catch (e) {
    gStatusError = RegValAsText(e);
  }
}

// 状态条落在第几行：数据末行往下空几行
function regvalStatusRowFor(sheet) {
  var lastUsed = regvalLastUsedRow(sheet);
  if (lastUsed < 2) lastUsed = 2;
  return lastUsed + REGVAL_STATUS_ROW_GAP;
}

// 第一列最后一个有内容的行（自上而下扫，连续空 30 行就收工）
function regvalLastUsedRow(sheet) {
  var last = 1;
  var blanks = 0;
  for (var row = 2; row <= 4096; row++) {
    if (RegValAsText(regvalCellValue(sheet, row, 1)) !== "") {
      last = row;
      blanks = 0;
    } else {
      blanks++;
      if (blanks >= 30) break;
    }
  }
  return last;
}


// ======================================================================
// 错误态：错误报告 → 状态条错误行 + 对话框
// ======================================================================

function regvalResetRunInfo() {
  gStyleError = "";
  gStatusError = "";
  gUiError = "";
  regvalResetErrorInfo();
}

function regvalResetErrorInfo() {
  gHasError = false;
  gErrorCode = "";
  gErrorZh = "";
  gErrorMessage = "";
  gErrorAdvice = "";
}

function regvalSetErrorInfo(code, codeZh, message, advice) {
  gHasError = true;
  gErrorCode = RegValAsText(code);
  gErrorZh = RegValAsText(codeZh);
  gErrorMessage = RegValAsText(message);
  gErrorAdvice = RegValAsText(advice);
}

// 读引擎写的错误报告 JSON
//
// 中文说明取本地表（RegValErrorText，与 src/errors.rs 同源）——
// 表里没有才退回引擎给的那句，不自己编。
function regvalReadErrorReport(errPath) {
  if (!regvalFileExists(errPath)) {
    regvalSetErrorInfo("NO_ERROR_REPORT", "", REGVAL_MESSAGES["no_error_report"], "");
    return;
  }
  var report = regvalParseJson(regvalReadTextFile(errPath));
  if (!report) {
    regvalSetErrorInfo("NO_ERROR_REPORT", "", REGVAL_MESSAGES["no_error_report"], "");
    return;
  }
  var code = RegValAsText(report.code);
  var localZh = RegValErrorText(code);
  if (!localZh || localZh === code) localZh = RegValAsText(report.code_zh);
  regvalSetErrorInfo(code, localZh, RegValAsText(report.message), RegValAsText(report.advice));
}

// 状态条上的错误行：「错误码：XXX · 中文说明」
function regvalErrorStatusText() {
  var template = gErrorZh ? REGVAL_MESSAGES["error_line"] : REGVAL_MESSAGES["error_line_unknown"];
  return RegValFmt(RegValFmt(template, "code", gErrorCode), "code_zh", gErrorZh);
}

// 错误行写到状态条上（阻断级错误的第一层渠道）
function regvalWriteErrorStatusLine(sheet) {
  if (!sheet) return;
  regvalSetStatusLine(sheet, regvalErrorStatusText());
}

// 阻断级错误的第二层渠道：对话框把错误码、中文说明与处理建议说全
function regvalShowErrorDialog() {
  if (!gHasError) return;
  var text = regvalErrorStatusText();
  if (gErrorMessage) text = text + "\n" + gErrorMessage;
  if (gErrorAdvice) text = text + "\n" + REGVAL_MESSAGES["advice_prefix"] + gErrorAdvice;
  regvalMsgBox(text);
}

// 样式没套上：数据写回去了、外观没跟上 —— 如实说一句（不重填单元格）
function regvalShowStyleNotice() {
  var text = "";
  if (gStyleError) {
    text = RegValFmt(REGVAL_MESSAGES["style_error"], "detail", gStyleError);
  }
  if (gStatusError) {
    if (text) text = text + "\n";
    text = text + RegValFmt(REGVAL_MESSAGES["statusbar_failed"], "detail", gStatusError);
  }
  if (gUiError) {
    if (text) text = text + "\n";
    text = text + RegValFmt(REGVAL_MESSAGES["style_error"], "detail", gUiError);
  }
  if (!text) return;
  regvalMsgBox(text);
}


// ======================================================================
// 引擎调用
// ======================================================================

// 引擎路径：环境变量优先（便于院内多套安装），否则用文件顶部的常量
function regvalResolveEnginePath() {
  try {
    if (typeof process !== "undefined" && process.env && process.env.REGVAL_ENGINE_PATH) {
      return process.env.REGVAL_ENGINE_PATH;
    }
  } catch (e) {
    // 宿主没有 process（真 WPS）：用常量
  }
  return REGVAL_ENGINE_PATH;
}

// 阻塞调引擎，返回退出码
function regvalRunEngine(cmd) {
  var exitCode = 0;
  try {
    exitCode = Application.Run(cmd);
  } catch (e) {
    // 调不动引擎：当作失败，错误报告与状态条会接手
    regvalSetErrorInfo("ENGINE_RUN_FAILED", "", RegValAsText(e), "");
    return 1;
  }
  return typeof exitCode === "number" ? exitCode : 0;
}


// ======================================================================
// 列位置与单元格读写
// ======================================================================

// 原表首行最后一个有内容的列号
function regvalLastUsedColumn(sheet) {
  var last = 0;
  for (var col = 1; col <= REGVAL_SCAN_COLS; col++) {
    if (RegValAsText(regvalCellValue(sheet, REGVAL_HEADER_ROW, col)) !== "") last = col;
  }
  return last;
}

// 新列首列号：已经写过一遍时沿用原位置（重跑不重复堆列）
function regvalFirstFreeColumn(sheet) {
  var expected = REGVAL_COLUMN_NAMES.split("|");
  var last = 0;
  for (var col = 1; col <= REGVAL_SCAN_COLS; col++) {
    var text = RegValAsText(regvalCellValue(sheet, REGVAL_HEADER_ROW, col));
    if (text === expected[0]) return col;
    if (text !== "") last = col;
  }
  return last + 1;
}

// 按名字取工作表（取不到返回 null）
function regvalSheetByName(book, sheetName) {
  try {
    if (!book || !book.Worksheets) return null;
    var ws = book.Worksheets.Item ? book.Worksheets.Item(sheetName) : book.Worksheets(sheetName);
    return ws || null;
  } catch (e) {
    return null;
  }
}

// 操作对象：当前激活的工作簿
function regvalTargetBook() {
  try {
    if (typeof Application === "undefined") return null;
    if (!Application.Workbooks || Application.Workbooks.Count === 0) return null;
    return Application.ActiveWorkbook || null;
  } catch (e) {
    return null;
  }
}

function regvalActiveSheet() {
  try {
    return Application.ActiveSheet || null;
  } catch (e) {
    return null;
  }
}

// 表里有未保存的改动时先问一句：引擎读的是磁盘上的文件
function regvalEnsureSaved(book) {
  if (book.Saved) return true;
  var answer = regvalMsgBox(REGVAL_MESSAGES["unsaved_changes"] + "\n" +
                            REGVAL_MESSAGES["unsaved_changes_hint"] + "\n" +
                            REGVAL_MESSAGES["unsaved_changes_question"]);
  if (String(answer).toLowerCase() === "no") return false;
  try {
    Application.DisplayAlerts = false;
    book.Save();
    Application.DisplayAlerts = true;
  } catch (e) {
    Application.DisplayAlerts = true;
  }
  return true;
}

// 结果契约与错误报告放在哪：表格所在目录（表格没保存时放临时目录）
function regvalWorkDirFor(book) {
  if (!book.Path) {
    try {
      var temp = Application.TempPath;
      if (temp) return temp;
    } catch (e) {
      // 拿不到临时目录就用当前目录
    }
    return "./";
  }
  return book.Path + regvalPathSeparator();
}

function regvalPathSeparator() {
  try {
    if (Application.PathSeparator) return Application.PathSeparator;
  } catch (e) {
    // 宿主不认这个属性
  }
  return "/";
}


// ======================================================================
// 文件读写（WPS FileSystem API）
// ======================================================================

function regvalFileExists(path) {
  try {
    return FileSystem.FileExists(path);
  } catch (e) {
    return false;
  }
}

function regvalDeleteFile(path) {
  try {
    if (FileSystem.FileExists(path)) FileSystem.DeleteFile(path);
  } catch (e) {
    // 删不掉就留着：引擎会覆盖它
  }
}

function regvalReadTextFile(path) {
  var stream = null;
  try {
    stream = FileSystem.OpenTextFile(path, 1 /* read */, false);
  } catch (e) {
    return "";
  }
  try {
    if (typeof stream.ReadAll === "function") return RegValAsText(stream.ReadAll());
    var lines = [];
    var line = stream.ReadLine();
    while (line !== null && line !== undefined) {
      lines.push(line);
      line = stream.ReadLine();
    }
    return lines.join("\n");
  } catch (e) {
    return "";
  } finally {
    try { stream.Close(); } catch (e) { /* 关不掉就算了 */ }
  }
}

function regvalWriteTextFile(path, text) {
  var stream = null;
  try {
    stream = FileSystem.CreateTextFile(path, true /* overwrite */);
    stream.Write(text);
    stream.Close();
    return true;
  } catch (e) {
    try { if (stream) stream.Close(); } catch (e2) { /* 忽略 */ }
    return false;
  }
}

// 本轮小结：批次号 + 状态条句子，落到表格同目录。
// 写回口径与 Excel 侧一致；这一份只是让科室在表格外也能核对本轮跑了什么。
function regvalWriteRunSummary(book, contract, statusLine) {
  var summary = {
    batch_ref: RegValAsText(contract ? contract.batch_ref : ""),
    schema_version: RegValAsText(contract ? contract.schema_version : ""),
    status_line: RegValAsText(statusLine),
    at: regvalNow()
  };
  regvalWriteTextFile(regvalWorkDirFor(book) + ".regval_last_run.json",
                      JSON.stringify(summary));
}

function regvalNow() {
  try {
    return new Date().toISOString();
  } catch (e) {
    return "";
  }
}


// ======================================================================
// 样式应用（按 token 填充与设字体）
//
// 业务逻辑与样式分离：前面几节只管「算什么、写哪里」，这一节只管「长什么样」。
//
// 配色、字体、字号、列宽、边框全部取自注入的 token —— 宏里不写死任何色值。
// 只碰新列那几格：原表已有列的单元格值、格式与公式一律不动。
// ======================================================================

function regvalApplyResultColumnStyles(sheet, firstNewCol, firstDataRow, lastDataRow) {
  try {
    var columns = REGVAL_TOKENS.columns;

    // 列宽（不论有没有数据行都要设 —— 空表也得把列摆好）
    for (var c = 0; c < columns.length; c++) {
      sheet.Columns(firstNewCol + c).ColumnWidth = columns[c].width;
    }

    // 表头：深青灰底 + 白字
    var header = sheet.Range(regvalAddr(REGVAL_HEADER_ROW, firstNewCol) + ":" +
                             regvalAddr(REGVAL_HEADER_ROW, firstNewCol + columns.length - 1));
    header.Interior.Color = RegValFillForRole("header");
    header.Font.Color = RegValTextForRole("header");
    header.Font.Name = REGVAL_TOKENS.fonts.body.name;
    header.Font.Size = REGVAL_TOKENS.fonts.body.size;
    header.Font.Bold = true;
    header.HorizontalAlignment = -4108 /* xlHAlignCenter */;
    header.VerticalAlignment = -4108 /* xlHAlignCenter */;

    if (lastDataRow < firstDataRow) return;

    // 整块基础排版：字体、字号、细线、行高（一次套完，别逐格调用）
    var block = sheet.Range(regvalAddr(firstDataRow, firstNewCol) + ":" +
                            regvalAddr(lastDataRow, firstNewCol + columns.length - 1));
    block.Font.Name = REGVAL_TOKENS.fonts.body.name;
    block.Font.Size = REGVAL_TOKENS.fonts.body.size;

    for (var row = firstDataRow; row <= lastDataRow; row++) {
      var conclusion = RegValAsText(regvalCellValue(sheet, row, firstNewCol));
      var issueTypes = RegValAsText(regvalCellValue(sheet, row, firstNewCol + 1));
      var suspect = RegValAsText(regvalCellValue(sheet, row, firstNewCol + 3));
      var status = RegValAsText(regvalCellValue(sheet, row, firstNewCol + 4));

      var rowRole = RegValRoleForConclusion(conclusion);
      var typeRole = RegValRoleForIssueTypes(issueTypes);
      var statusRole = RegValRoleForSubmissionStatus(status);

      // 校验结论列：绿/黄/红 + 加粗
      regvalApplyRoleStyle(sheet, row, firstNewCol, rowRole, true);
      // 问题类型 / 问题说明两列：跟着问题类型走
      regvalApplyRoleStyle(sheet, row, firstNewCol + 1, typeRole, false);
      regvalApplyRoleStyle(sheet, row, firstNewCol + 2, typeRole, false);
      // 疑似重复项列：有疑似项才黄底，字用等宽
      if (RegValRoleForSuspected(suspect) === "suspect") {
        regvalApplyRoleStyle(sheet, row, firstNewCol + 3, "suspect", false);
        regvalApplyMonoFont(sheet, row, firstNewCol + 3);
      }
      // 上报状态列：回执的颜色 + 加粗
      if (statusRole !== "neutral") {
        regvalApplyRoleStyle(sheet, row, firstNewCol + 4, statusRole, true);
      }
      // 左侧 3px 色条：问题行最显眼（边框 API 各家差异大，单独兜住）
      if (rowRole === "issue" || rowRole === "suspect") {
        regvalApplyLeftBar(sheet, row, firstNewCol, rowRole);
      }
    }
  } catch (e) {
    // 样式失败不影响已经写回的数据，但要如实说一声，不能闷声吞掉
    gStyleError = RegValAsText(e);
  }
}

// 按角色上色：底色 + 字色（字重由调用处决定）
function regvalApplyRoleStyle(sheet, row, col, role, bold) {
  var cell = sheet.Cells(row, col);
  cell.Interior.Color = RegValFillForRole(role);
  cell.Font.Color = RegValTextForRole(role);
  if (bold) cell.Font.Bold = true;
}

// 键值列用等宽字体（行键、幂等键这类哈希串，便于肉眼对齐核对）
function regvalApplyMonoFont(sheet, row, col) {
  var cell = sheet.Cells(row, col);
  cell.Font.Name = REGVAL_TOKENS.fonts.mono.name;
  cell.Font.Size = REGVAL_TOKENS.fonts.mono.size;
}

// 左侧 3px 色条：各家的边框 API 差别大，装不上就跳过（不拖累其余样式）
function regvalApplyLeftBar(sheet, row, col, role) {
  try {
    var cell = sheet.Cells(row, col);
    if (!cell.Borders) return;
    var edge = cell.Borders.Item ? cell.Borders.Item(7 /* xlEdgeLeft */) : null;
    if (!edge) return;
    edge.LineStyle = 1; // xlContinuous
    edge.Weight = REGVAL_TOKENS.borders.bar.weight;
    edge.Color = RegValTextForRole(role);
  } catch (e) {
    // 边框装不上不影响数据与底色
  }
}


// ======================================================================
// 单元格读写（Excel / WPS 对象模型）
// ======================================================================

function regvalCellValue(sheet, row, col) {
  try {
    return sheet.Cells(row, col).Value;
  } catch (e) {
    return "";
  }
}

function regvalSetCellValue(sheet, row, col, value) {
  sheet.Cells(row, col).Value = value;
}

// 批量赋值：一次给一片区域，避免大表逐格写回卡顿
function regvalSetRangeValues(sheet, row1, col1, values) {
  if (!values || !values.length) return;
  var row2 = row1 + values.length - 1;
  var col2 = col1 + (values[0] ? values[0].length : 1) - 1;
  var rng = sheet.Range(regvalAddr(row1, col1) + ":" + regvalAddr(row2, col2));
  rng.Value = values;
}

// 列号 → 字母（1 → A，27 → AA）
function regvalColLetter(col) {
  var n = col;
  var out = "";
  while (n > 0) {
    var rem = (n - 1) % 26;
    out = String.fromCharCode(65 + rem) + out;
    n = Math.floor((n - 1) / 26);
  }
  return out;
}

// 单元格地址（1 起的行列号 → A1 式地址）
function regvalAddr(row, col) {
  return regvalColLetter(col) + String(row);
}


// ======================================================================
// 契约读取辅助
// ======================================================================

// summary 段里的字符串字段
function regvalSummaryString(contract, key) {
  if (!contract || !contract.summary) return "";
  var value = contract.summary[key];
  return value === null || value === undefined ? "" : String(value);
}

// 契约里的新列名（columns 数组里的 name）
function regvalColumnNames(contract) {
  var names = [];
  var columns = (contract && contract.columns) ? contract.columns : [];
  for (var i = 0; i < columns.length; i++) {
    names.push(RegValAsText(columns[i].name));
  }
  return names;
}

// 两个列名数组是否一致
function regvalColumnsMatch(expected, actual) {
  if (!expected || !actual || expected.length !== actual.length) return false;
  for (var i = 0; i < expected.length; i++) {
    if (String(expected[i]) !== String(actual[i])) return false;
  }
  return true;
}


// ======================================================================
// JSON 解析（不依赖宿主的 JSON 能力，与 Excel 侧同样手写一套）
// ======================================================================

function RegValParseJson(text) {
  try {
    return regvalParseJson(String(text));
  } catch (e) {
    return null;
  }
}

function regvalParseJson(text) {
  var s = text;
  var i = 0;

  function fail(msg) {
    throw new Error("JSON 解析失败：" + msg + "（位置 " + i + "）");
  }
  function skipWs() {
    while (i < s.length && " \t\r\n".indexOf(s.charAt(i)) >= 0) i++;
  }
  function expect(word) {
    if (s.substr(i, word.length) !== word) fail("期望 " + word);
    i += word.length;
  }
  function parseString() {
    if (s.charAt(i) !== "\"") fail("期望字符串");
    i++;
    var out = "";
    while (i < s.length) {
      var ch = s.charAt(i);
      if (ch === "\"") { i++; return out; }
      if (ch === "\\") {
        i++;
        var esc = s.charAt(i);
        if (esc === "n") out += "\n";
        else if (esc === "t") out += "\t";
        else if (esc === "r") out += "\r";
        else if (esc === "b") out += "\b";
        else if (esc === "f") out += "\f";
        else if (esc === "u") {
          var hex = s.substr(i + 1, 4);
          out += String.fromCharCode(parseInt(hex, 16));
          i += 4;
        } else out += esc;
        i++;
        continue;
      }
      out += ch;
      i++;
    }
    fail("字符串没有结束引号");
    return "";
  }
  function parseNumber() {
    var start = i;
    while (i < s.length && "0123456789+-.eE".indexOf(s.charAt(i)) >= 0) i++;
    if (start === i) fail("这里不是数字");
    return Number(s.slice(start, i));
  }
  function parseArray() {
    var out = [];
    i++; // [
    skipWs();
    if (s.charAt(i) === "]") { i++; return out; }
    while (i < s.length) {
      out.push(parseValue());
      skipWs();
      var ch = s.charAt(i);
      if (ch === ",") { i++; skipWs(); continue; }
      if (ch === "]") { i++; return out; }
      fail("数组里既不是逗号也不是右括号");
    }
    fail("数组没有收尾");
    return out;
  }
  function parseObject() {
    var out = {};
    i++; // {
    skipWs();
    if (s.charAt(i) === "}") { i++; return out; }
    while (i < s.length) {
      skipWs();
      var key = parseString();
      skipWs();
      if (s.charAt(i) !== ":") fail("键后面缺冒号");
      i++;
      skipWs();
      out[key] = parseValue();
      skipWs();
      var ch = s.charAt(i);
      if (ch === ",") { i++; skipWs(); continue; }
      if (ch === "}") { i++; return out; }
      fail("对象里既不是逗号也不是右花括号");
    }
    fail("对象没有收尾");
    return out;
  }
  function parseValue() {
    skipWs();
    if (i >= s.length) fail("内容提前结束");
    var ch = s.charAt(i);
    if (ch === "{") return parseObject();
    if (ch === "[") return parseArray();
    if (ch === "\"") return parseString();
    if (ch === "t") { expect("true"); return true; }
    if (ch === "f") { expect("false"); return false; }
    if (ch === "n") { expect("null"); return null; }
    return parseNumber();
  }

  var value = parseValue();
  skipWs();
  if (i !== s.length) fail("后面还有没读完的内容");
  return value;
}


// ======================================================================
// 基础工具
// ======================================================================

// 任何值转成字符串（null / undefined 一律转成空串）
function RegValAsText(value) {
  if (value === null || value === undefined) return "";
  return String(value);
}

// 命令行参数加引号：路径里有空格或中文也不会散架
function regvalQuote(path) {
  return "\"" + String(path) + "\"";
}

// 弹一句提示给用户（宿主有 MsgBox 就用 MsgBox，没有就退回 alert）
function regvalMsgBox(text) {
  try {
    if (typeof Application !== "undefined" && typeof Application.MsgBox === "function") {
      return Application.MsgBox(text);
    }
  } catch (e) {
    // 落到 alert
  }
  try {
    if (typeof alert === "function") return alert(text);
  } catch (e2) {
    // 宿主没有弹框能力：提示就留在状态条上
  }
  return "yes";
}
