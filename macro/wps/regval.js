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
// 契约常量（与引擎 src/contract.rs 同源；改动须同步升 schema_version）
var REGVAL_SCHEMA_VERSION = "regval.contract/1.0.0";
var REGVAL_COLUMN_NAMES = "校验结论|问题类型|问题说明（含依据）|疑似重复项|上报状态";
// @regval:end contract

// @regval:begin tokens
// ----------------------------------------------------------------------
// 视觉 token（唯一出处：src/ui/tokens.rs，由 `regval macro` 注入）
// 与 Excel 侧的 RegValTokens.bas 同源：同一份调色板、同一套角色映射。
// ----------------------------------------------------------------------
var REGVAL_TOKENS = {
  schema_version: "regval.tokens/1.0.0",
  palette: {
    "white": { "hex": "#FFFFFF", "bgr": 16777215 },
    "ink": { "hex": "#1F2933", "bgr": 3352863 },
    "slate": { "hex": "#2F3E46", "bgr": 4603439 },
    "issue_surface": { "hex": "#FDE7E9", "bgr": 15329277 },
    "issue_ink": { "hex": "#A3202A", "bgr": 2760867 },
    "suspect_surface": { "hex": "#FFF4DB", "bgr": 14415103 },
    "suspect_ink": { "hex": "#8A6100", "bgr": 24970 },
    "ok_surface": { "hex": "#E4F3E8", "bgr": 15266788 },
    "ok_ink": { "hex": "#1E6B39", "bgr": 3762974 },
    "warn_surface": { "hex": "#FBE3D2", "bgr": 13820923 },
    "warn_ink": { "hex": "#9A4A00", "bgr": 19098 },
    "neutral_surface": { "hex": "#E8ECF1", "bgr": 15854824 },
    "neutral_ink": { "hex": "#4A5A67", "bgr": 6773322 },
    "statusbar_surface": { "hex": "#F4F6F8", "bgr": 16316148 },
    "statusbar_ink": { "hex": "#5B6B78", "bgr": 7891803 },
    "grid": { "hex": "#D5DCE2", "bgr": 14867669 },
  },
  roles: {
    "surface": { "fill": "white", "fill_bgr": 16777215, "text": "ink", "text_bgr": 3352863 },
    "header": { "fill": "slate", "fill_bgr": 4603439, "text": "white", "text_bgr": 16777215 },
    "issue": { "fill": "issue_surface", "fill_bgr": 15329277, "text": "issue_ink", "text_bgr": 2760867 },
    "suspect": { "fill": "suspect_surface", "fill_bgr": 14415103, "text": "suspect_ink", "text_bgr": 24970 },
    "passed": { "fill": "ok_surface", "fill_bgr": 15266788, "text": "ok_ink", "text_bgr": 3762974 },
    "submitted": { "fill": "ok_surface", "fill_bgr": 15266788, "text": "ok_ink", "text_bgr": 3762974 },
    "rejected": { "fill": "warn_surface", "fill_bgr": 13820923, "text": "warn_ink", "text_bgr": 19098 },
    "retry": { "fill": "neutral_surface", "fill_bgr": 15854824, "text": "neutral_ink", "text_bgr": 6773322 },
    "neutral": { "fill": "white", "fill_bgr": 16777215, "text": "ink", "text_bgr": 3352863 },
    "statusbar": { "fill": "statusbar_surface", "fill_bgr": 16316148, "text": "statusbar_ink", "text_bgr": 7891803 },
  },
  fonts: {
    "body": { "name": "Microsoft YaHei", "size": 10.5 },
    "mono": { "name": "Consolas", "size": 10.5 },
  },
  columns: [
    { "key": "conclusion", "name": "校验结论", "width": 10, "font": "body", "bold": true },
    { "key": "issue", "name": "问题类型", "width": 18, "font": "body", "bold": false },
    { "key": "detail", "name": "问题说明（含依据）", "width": 48, "font": "body", "bold": false },
    { "key": "suspect", "name": "疑似重复项", "width": 20, "font": "mono", "bold": false },
    { "key": "status", "name": "上报状态", "width": 12, "font": "body", "bold": true },
  ],
  borders: { "bar": { "weight": 4, "color": "#A3202A" }, "grid": { "weight": 1, "color": "#D5DCE2" } },
  row_height_ratio: 1.3
};

function RegValFillForRole(role) {
  var r = REGVAL_TOKENS.roles[role];
  return r ? r.fill_bgr : REGVAL_TOKENS.palette["white"].bgr;
}

function RegValTextForRole(role) {
  var r = REGVAL_TOKENS.roles[role];
  return r ? r.text_bgr : REGVAL_TOKENS.palette["ink"].bgr;
}

function RegValRoleForConclusion(conclusion) {
  var map = { "不通过": "issue", "有提示": "suspect", "通过": "passed", };
  var key = RegValAsText(conclusion);
  return Object.prototype.hasOwnProperty.call(map, key) ? map[key] : "neutral";
}

function RegValRoleForIssueTypes(issueTypes) {
  var t = RegValAsText(issueTypes);
  if (t.replace(/^\s+|\s+$/g, "") === "") return "neutral";
  if (t.indexOf("必填缺失") >= 0) return "issue";
  if (t.indexOf("枚举越界") >= 0) return "issue";
  if (t.indexOf("单位不符") >= 0) return "issue";
  if (t.indexOf("公式不符") >= 0) return "issue";
  if (t.indexOf("数据形态错误") >= 0) return "issue";
  if (t.indexOf("科室主数据不一致") >= 0) return "issue";
  if (t.indexOf("科室名称写法不一致") >= 0) return "issue";
  if (t.indexOf("疑似重复") >= 0) return "suspect";
  if (t.indexOf("待人工判定") >= 0) return "suspect";
  return "neutral";
}

var REGVAL_SUBMISSION_STATUS_PREFIXES = {
  "已上报": "submitted",
  "幂等命中": "submitted",
  "被退回": "rejected",
  "待重试": "retry",
  "未发出": "issue",
  "回执待人工核对": "suspect",
  "未执行": "neutral",
  "未上报": "neutral",
};

function RegValRoleForSubmissionStatus(statusText) {
  var t = RegValAsText(statusText);
  if (t.replace(/^\s+|\s+$/g, "") === "") return "neutral";
  var prefixes = Object.keys(REGVAL_SUBMISSION_STATUS_PREFIXES);
  for (var i = 0; i < prefixes.length; i++) {
    if (t.indexOf(prefixes[i]) === 0) return REGVAL_SUBMISSION_STATUS_PREFIXES[prefixes[i]];
  }
  return "neutral";
}

function RegValRoleForSuspected(suspectedText) {
  var t = RegValAsText(suspectedText);
  if (t.replace(/^\s+|\s+$/g, "") === "") return "neutral";
  return "suspect";
}
// @regval:end tokens

// @regval:begin messages
// ----------------------------------------------------------------------
// 提示文案常量表（唯一出处：src/ui/messages.rs + src/errors.rs，由 `regval macro` 注入）
// 与 Excel 侧的 RegValMessages.bas 同源：同一批文案，同一批错误码中文说明。
// ----------------------------------------------------------------------
var REGVAL_MESSAGES = {
  "loading": "校验中…",
  "loading_push": "上报中…",
  "empty_rows": "表里没有数据行 · 未做校验",
  "empty_issues": "已校验 {rows} 行 · 未发现口径问题",
  "error_line": "错误码：{code} · {code_zh}",
  "error_line_unknown": "错误码：{code}",
  "dialog_title": "卫健上报校验",
  "button_validate": "校验",
  "button_push": "上报",
  "no_workbook": "请先打开要校验的上报表。",
  "not_saved": "请先把表格保存到磁盘（引擎读的是磁盘上的文件）。",
  "installed": "已装好「校验」「上报」两个按钮。",
  "installed_hint": "点「校验」把结果写回右侧 5 个新列；点「上报」把已确认的行推到院内上报网关。",
  "engine_missing": "找不到引擎程序：{path}",
  "engine_missing_hint": "请修改 RegVal.bas 顶部的 REGVAL_ENGINE_PATH，或参照 macro/README.md 的安装说明。",
  "no_contract": "引擎没有产出结果契约文件，原表未被改动。",
  "expect_value": "宏要求：{value}",
  "actual_value": "引擎产出：{value}",
  "columns_expected": "宏期望的列：{value}",
  "columns_actual": "引擎产出的列：{value}",
  "untouched": "原表未被改动。",
  "validate_failed": "校验未完成，原表未被改动。",
  "push_failed": "上报未完成，原表未被改动。",
  "no_batch": "这一批没有批次号，上报无从谈起。请检查引擎版本是否与宏一致。",
  "submit_done_fallback": "上报完成，回执已回填",
  "unsaved_changes": "表格有未保存的改动。",
  "unsaved_changes_hint": "引擎读的是磁盘上的文件，未保存的改动它看不到。",
  "unsaved_changes_question": "要现在保存再继续吗？",
  "no_error_report": "（引擎没有留下错误报告，请到命令行窗口查看详情）",
  "advice_prefix": "处理建议：",
  "style_error": "样式未完全套上：{detail}",
  "statusbar_failed": "状态条未写上：{detail}",
  "summary_missing": "已校验（未取到摘要）",
  "busy": "上一轮还没跑完，请等它结束再点。",
  "writeback_note": "结果已写回原表右侧 5 个新列（原表格式与公式未动）。",
  "push_note": "「上报状态」列已按回执回填（绿=已上报 / 橙=被退回 / 灰=待重试）。",
};

var REGVAL_ERROR_ZH = {
  "INPUT_UNREADABLE": "输入表读取失败",
  "UNSUPPORTED_FORMAT": "不支持的输入格式",
  "MISSING_COLUMN": "表头缺少必填列",
  "EMPTY_TABLE": "表中没有数据行",
  "RULE_SYNTAX_ERROR": "口径规则语法不通过",
  "RULE_LOAD_FAILED": "口径规则表装载失败",
  "RULE_BASIS_MISSING": "口径规则缺官方依据",
  "RULE_EXEC_ERROR": "口径规则执行失败",
  "RULE_SWITCH_UNCONFIRMED": "口径换版未经质控岗确认",
  "RULE_EFFECTIVE_DATE_NOT_REACHED": "口径版本的生效日期未到",
  "RULE_VERSION_NOT_FOUND": "规则集里没有该版本的口径规则",
  "DENOMINATOR_ZERO": "分母为 0",
  "INVALID_NUMERIC": "数值列不是合法数值",
  "CONFIG_MISSING": "缺少必需的环境变量",
  "CONFIG_INVALID": "环境变量取值不合法",
  "CONTRACT_WRITE_ERROR": "结果契约写出失败",
  "CONTRACT_VERSION_MISMATCH": "引擎与宏版本不一致，请升级",
  "IDEMPOTENCY_MISSING": "缺幂等键",
  "INVALID_STATE_TRANSITION": "行级状态迁移不合法",
  "ROW_STATE_CONFLICT": "行状态版本冲突",
  "UNCONFIRMED_ROW": "该行未经人工确认，不能上报",
  "CONFIRM_ACTOR_MISSING": "人工确认缺确认人",
  "GATEWAY_REJECTED": "上报网关退回",
  "GATEWAY_SERVER_ERROR": "上报网关返回服务端错误",
  "GATEWAY_TIMEOUT": "上报网关调用超时",
  "GATEWAY_CONNECTION_FAILED": "上报网关连不上",
  "GATEWAY_RESPONSE_INVALID": "上报网关回执结构不合法",
  "RECEIPT_CONFLICT": "幂等键与回执 ID 不是一一对应",
  "RECEIPT_LEDGER_MISMATCH": "回执台账交叉核对不通过",
  "DEPT_NOT_FOUND": "科室主数据不一致",
  "DEPT_NAME_MISMATCH": "科室名称写法与主数据不一致",
  "DEPT_DIRECTORY_UNAVAILABLE": "科室主数据目录不可用",
  "DEPT_DIRECTORY_CONNECTION_FAILED": "科室主数据目录连不上",
  "DEPT_DIRECTORY_TIMEOUT": "科室主数据目录调用超时",
  "DEPT_DIRECTORY_BAD_REQUEST": "科室主数据目录拒绝了请求",
  "DEPT_DIRECTORY_SERVER_ERROR": "科室主数据目录返回服务端错误",
  "DEPT_DIRECTORY_RESPONSE_INVALID": "科室主数据目录响应结构不合法",
  "MODEL_UNAVAILABLE": "模型端点不可达",
  "MODEL_CONNECTION_FAILED": "模型端点连不上",
  "MODEL_TIMEOUT": "模型端点调用超时",
  "MODEL_BAD_REQUEST": "模型端点拒绝了请求",
  "MODEL_SERVER_ERROR": "模型端点返回服务端错误",
  "MODEL_RESPONSE_INVALID": "模型响应未通过结构校验",
  "DB_ERROR": "本机校验库操作失败",
  "DB_CORRUPTED": "状态库文件损坏",
  "DB_VERSION_UNSUPPORTED": "状态库版本不被本引擎识别",
  "DB_MIGRATION_FAILED": "状态库迁移失败",
  "AUTHORITY_KEY_CONFLICT": "权威键重复",
  "BATCH_SUMMARY_CONFLICT": "批次汇总重放冲突",
  "IO_ERROR": "文件读写失败",
  "CAPABILITY_NOT_READY": "该能力尚未在本构建中提供",
  "BENCH_SET_INVALID": "标注基准集不合法",
};

// 按错误码取中文说明（表里没有就原样返回错误码，不吞掉）
function RegValErrorText(code) {
  var c = RegValAsText(code).replace(/^\s+|\s+$/g, "").toUpperCase();
  return Object.prototype.hasOwnProperty.call(REGVAL_ERROR_ZH, c) ? REGVAL_ERROR_ZH[c] : code;
}

// 文案占位符替换（{rows} 这类标记换成实值）
function RegValFmt(template, key, value) {
  return String(template).split("{" + key + "}").join(value);
}
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
