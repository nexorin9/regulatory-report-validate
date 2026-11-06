Attribute VB_Name = "RegVal"
Option Explicit

'======================================================================
' 卫健上报校验系统 —— Excel 加载宏（完整版）
'
' 界面 = 两个按钮 + 底部状态条，科室仍在自己那张表里干活：
'   · 「校验」：阻塞调用 regval validate，把结果契约写回原表右侧 5 个新列
'   · 「上报」：阻塞调用 regval push，把回执驱动的行状态回填
'   · 状态条：数据下面一行浅灰，写「已校验 N 行 · M 项口径问题 · K 项疑似重复待确认」
'
' 四条纪律（与引擎同一套口径）：
'   1. 不自行统计 —— 状态条上的数与整句话都取自契约 JSON 的 summary 段；
'      宏只负责搬，不负责算（引擎算好一句成品话，宏原样显示）。
'   2. 只写新列 —— 原表已有列、单元格格式与公式一律不动。
'   3. 版本不符即中止 —— 契约 schema_version 与本宏常量不一致时提示
'      「引擎与宏版本不一致，请升级」，不做部分写回。
'   4. 文案只有一处 —— 面向用户的中文全部取自 RegValMessages.bas，
'      本文件一个字都不写死（tests/ui_states_test.rs 盯着这件事）。
'
' 三态与提示层级：
'   加载态：调引擎**之前**先把「校验中…」写上状态条并禁用两个按钮，
'           避免用户以为「按了没反应」；引擎跑完再覆盖。
'   空结果态：一句成品话由引擎算好 —— 「表里没有数据行 · 未做校验」
'           与「已校验 N 行 · 未发现口径问题」是两种空，不能混成一句。
'   错误态：引擎退出码非 0 时状态条写「错误码：XXX · 中文说明」，
'           同时弹一次对话框把错误码、中文说明与处理建议说全。
'
'   提示层级（同一件事只用它该用的渠道，不把三样叠在一起）：
'     · 结果级（行不通过 / 疑似重复）→ 单元格填充 + 状态条摘要（不弹框）
'     · 阻断级错误（引擎退出码非 0）  → 状态条错误行 + 对话框（不碰单元格）
'     · 样式没套上（数据写回了、外观没跟上）→ 状态条摘要 + 一句对话框提示
'
' 出错时怎么说话：引擎失败会按 --error-out 写一份错误报告 JSON
' （code / code_zh / message / advice），宏按字段读 —— 不解析人类可读的
' stderr，终端文案一改就断的那种做法不采用。
'
' 样式（配色 / 字体 / 列宽 / 边框）不在本文件里写死：token 的唯一出处是
' 引擎的 src/ui/tokens.rs，由 `regval tokens --vba` 生成到
' macro/vba/RegValTokens.bas。**RegValTokens.bas 必须与本文件一起导入**，
' 否则编译报「变量未定义」。文案同理，见 macro/vba/RegValMessages.bas。
'======================================================================

' ---- 契约常量（与引擎 src/contract.rs 同源；改动须同步升 schema_version）----
Public Const REGVAL_SCHEMA_VERSION As String = "regval.contract/1.0.0"
Public Const REGVAL_COLUMN_NAMES As String = "校验结论|问题类型|问题说明（含依据）|疑似重复项|上报状态"

' ---- 部署常量（安装时按院内实际路径改这两行）----
' 引擎二进制路径。含空格与中文都没问题：命令行里用 Chr$(34) 包引号。
Public Const REGVAL_ENGINE_PATH As String = "C:\Program Files\RegVal\regval.exe"
' 本机校验库路径（状态留存处）
Public Const REGVAL_STATE_DB As String = "C:\Program Files\RegVal\state\state.db"

' 状态条在数据下面空几行（与引擎 src/ui/statusbar.rs 的 STATUS_ROW_GAP 一致）
Private Const REGVAL_STATUS_ROW_GAP As Long = 2

' 分块写回的行数：每块一次 Range.Value2 赋值，避免大表逐格写回卡顿
Private Const REGVAL_CHUNK_ROWS As Long = 200

' 按钮尺寸（Excel 磅）
Private Const REGVAL_BUTTON_W As Double = 88
Private Const REGVAL_BUTTON_H As Double = 26
Private Const REGVAL_BUTTON_GAP As Double = 8

' ---- 提示层级（同一件事只用它该用的渠道，不把三样叠在一起）----
'   结果级（行不通过 / 疑似重复）→ 单元格填充 + 状态条摘要（不弹框）
'   阻断级错误（引擎退出码非 0）  → 状态条错误行 + 对话框（不碰单元格）
Private Const HINT_RESULT_FILL As Boolean = True
Private Const HINT_RESULT_DIALOG As Boolean = False
Private Const HINT_ERROR_FILL As Boolean = False
Private Const HINT_ERROR_DIALOG As Boolean = True

' ---- 本轮的错误信息（引擎退出码非 0 或本地闸门拦下时填上）----
Private gHasError As Boolean
Private gErrorCode As String
Private gErrorZh As String
Private gErrorMessage As String
Private gErrorAdvice As String

' 样式套用失败时的说明（空 = 没失败）。数据写回与样式套用是两回事：
' 样式坏了不能把已经写好的数据说成「没改动」，所以单独记一句。
Private gStyleError As String

' 状态条没写上时的说明（与样式分开记：两件事的成因不一样）
Private gStatusError As String

' 本轮是否正在跑（防重复提交：宏阻塞执行引擎时用户可能再点一次按钮）
Private gBusy As Boolean

' 最近一次校验的批次号与契约文件路径（「上报」按钮按批次走）
Private gLastBatchRef As String
Private gLastContractPath As String

' 本轮写过结果的工作表（竖线分隔；状态条写到这些表上）
Private gWrittenSheets As String


'======================================================================
' 界面装配：两个按钮 + 起始态状态条
'======================================================================

' 装界面（Alt+F8 选这个跑）：表右侧出现「校验」「上报」两个按钮，
' 数据下面出现一条浅灰状态条。重复运行不会堆按钮。
Public Sub RegVal_InstallBar()
    Dim book As Workbook
    Dim ws As Worksheet

    Set book = TargetBook()
    If book Is Nothing Then
        MsgBox REGVAL_MSG_NO_WORKBOOK, vbExclamation, REGVAL_MSG_DIALOG_TITLE
        Exit Sub
    End If
    If Len(book.Path) = 0 Then
        MsgBox REGVAL_MSG_NOT_SAVED, vbExclamation, REGVAL_MSG_DIALOG_TITLE
        Exit Sub
    End If

    Set ws = ActiveSheet
    EnsureBar book, ws

    MsgBox REGVAL_MSG_INSTALLED & vbCrLf & REGVAL_MSG_INSTALLED_HINT, _
           vbInformation, REGVAL_MSG_DIALOG_TITLE
End Sub

' 静默装配：按钮不在就补上，不弹框。校验与上报都会先调它 ——
' 用户即使没跑过 RegVal_InstallBar，按一次 Alt+F8 的校验也会把按钮装好。
Private Sub EnsureBar(ByVal book As Workbook, ByVal ws As Worksheet)
    If ws Is Nothing Then Exit Sub
    If Not ShapeExists(ws, "BtnRegValValidate") Then
        AddActionButton ws, "BtnRegValValidate", REGVAL_MSG_BUTTON_VALIDATE, "RegVal_Validate", _
                        ws.Cells(1, 1).Left + 320, ws.Cells(1, 1).Top + 2
    End If
    If Not ShapeExists(ws, "BtnRegValPush") Then
        AddActionButton ws, "BtnRegValPush", REGVAL_MSG_BUTTON_PUSH, "RegVal_Push", _
                        ws.Cells(1, 1).Left + 320 + REGVAL_BUTTON_W + REGVAL_BUTTON_GAP, _
                        ws.Cells(1, 1).Top + 2
    End If
End Sub

' 画一个圆角按钮（样式取自 token 模块，不写死色值）
Private Sub AddActionButton(ByVal ws As Worksheet, ByVal btnName As String, ByVal caption As String, _
                             ByVal onAction As String, ByVal leftPos As Double, ByVal topPos As Double)
    Dim shp As Shape

    RemoveShapeIfExists ws, btnName

    Set shp = ws.Shapes.AddShape(msoShapeRoundedRectangle, leftPos, topPos, _
                                 REGVAL_BUTTON_W, REGVAL_BUTTON_H)
    shp.Name = btnName
    shp.OnAction = onAction
    shp.TextFrame.Characters.Text = caption
    With shp.TextFrame.Characters.Font
        .Name = T_BODY_FONT
        .Size = T_BODY_SIZE
        .Bold = True
        .Color = RegValTextForRole("header")
    End With
    shp.Fill.ForeColor.RGB = RegValFillForRole("header")
    shp.Line.Visible = msoFalse
    shp.Placement = xlFreeFloating
    shp.TextFrame.HorizontalAlignment = xlHAlignCenter
    shp.TextFrame.VerticalAlignment = xlVAlignCenter
End Sub

' 同名按钮先删掉再画（重复装界面不留残影）
Private Sub RemoveShapeIfExists(ByVal ws As Worksheet, ByVal btnName As String)
    Dim i As Long
    On Error Resume Next
    For i = ws.Shapes.Count To 1 Step -1
        If ws.Shapes(i).Name = btnName Then ws.Shapes(i).Delete
    Next i
    On Error GoTo 0
End Sub

Private Function ShapeExists(ByVal ws As Worksheet, ByVal btnName As String) As Boolean
    Dim i As Long
    On Error Resume Next
    For i = 1 To ws.Shapes.Count
        If ws.Shapes(i).Name = btnName Then
            ShapeExists = True
            Exit Function
        End If
    Next i
    On Error GoTo 0
End Function


'======================================================================
' 「校验」按钮
'======================================================================
Public Sub RegVal_Validate()
    Dim book As Workbook
    Dim ws As Worksheet
    Dim contractText As String
    Dim summaryText As String
    Dim statusLine As String

    ResetRunInfo

    ' 上一轮还没跑完就别再点：宏阻塞执行引擎时界面不刷新，
    ' 用户很可能以为没反应就再按一次。
    If gBusy Then
        SetWindowHint REGVAL_MSG_BUSY
        Exit Sub
    End If

    If Not EnsureWorkbook(book) Then Exit Sub
    If Not EnsureSaved(book) Then Exit Sub

    Set ws = ActiveSheet
    EnsureBar book, ws

    ' 加载态：先把「校验中…」写上、把按钮按住，再去调引擎
    BeginRun ws, REGVAL_MSG_LOADING

    contractText = RunValidateAndWriteBack(book)

    EndRun

    If Len(contractText) = 0 Then
        ' 阻断级错误：状态条已写错误行（HINT_ERROR_FILL = False，不碰单元格），
        ' 这里补一次对话框，把错误码、中文说明与处理建议说全。
        WriteErrorStatusLine ws
        If HINT_ERROR_DIALOG Then ShowErrorDialog
        Exit Sub
    End If

    ' 结果级：不弹框。单元格填充 + 状态条摘要已经说清楚了 ——
    ' 同一件事不叠第三种提示（对话框留给阻断级错误）。
    summaryText = JsonObjectText(contractText, "summary")
    statusLine = JsonStringValue(summaryText, "status_line")
    If Len(statusLine) = 0 Then statusLine = REGVAL_MSG_SUMMARY_MISSING
    ApplyStatusLineToWrittenSheets book, statusLine

    If Len(gStyleError) > 0 Or Len(gStatusError) > 0 Then ShowStyleNotice
End Sub


'======================================================================
' 「上报」按钮
'======================================================================
Public Sub RegVal_Push()
    Dim book As Workbook
    Dim ws As Worksheet
    Dim batchRef As String
    Dim enginePath As String
    Dim stateDb As String
    Dim errPath As String
    Dim cmd As String
    Dim exitCode As Long
    Dim contractText As String
    Dim summaryText As String
    Dim submissionLine As String

    ResetRunInfo

    If gBusy Then
        SetWindowHint REGVAL_MSG_BUSY
        Exit Sub
    End If

    If Not EnsureWorkbook(book) Then Exit Sub
    If Not EnsureSaved(book) Then Exit Sub

    Set ws = ActiveSheet
    EnsureBar book, ws

    ' 先校验一次：上报按批次走，批次要与当前表里的内容对得上
    BeginRun ws, REGVAL_MSG_LOADING_PUSH

    contractText = RunValidateAndWriteBack(book)
    If Len(contractText) = 0 Then
        EndRun
        WriteErrorStatusLine ws
        If HINT_ERROR_DIALOG Then ShowErrorDialog
        Exit Sub
    End If

    batchRef = JsonStringValue(contractText, "batch_ref")
    If Len(batchRef) = 0 Then
        EndRun
        SetErrorInfo "NO_BATCH_REF", "", REGVAL_MSG_NO_BATCH, ""
        WriteErrorStatusLine ws
        If HINT_ERROR_DIALOG Then ShowErrorDialog
        Exit Sub
    End If

    enginePath = REGVAL_ENGINE_PATH
    stateDb = REGVAL_STATE_DB
    errPath = WorkDirFor(book) & ".regval_error.json"
    If FileExists(errPath) Then Kill errPath

    cmd = Chr$(34) & enginePath & Chr$(34) & " push" & _
          " --batch " & Chr$(34) & batchRef & Chr$(34) & _
          " --state-db " & Chr$(34) & stateDb & Chr$(34) & _
          " --error-out " & Chr$(34) & errPath & Chr$(34)

    exitCode = RunEngine(cmd)
    If exitCode <> 0 Then
        ReadErrorReport errPath
        EndRun
        WriteErrorStatusLine ws
        If HINT_ERROR_DIALOG Then ShowErrorDialog
        Exit Sub
    End If

    ' 上报成功：再校验一次，把回执驱动的行状态回填到「上报状态」列
    contractText = RunValidateAndWriteBack(book)
    EndRun
    If Len(contractText) = 0 Then
        WriteErrorStatusLine ws
        If HINT_ERROR_DIALOG Then ShowErrorDialog
        Exit Sub
    End If

    summaryText = JsonObjectText(contractText, "summary")
    submissionLine = JsonStringValue(summaryText, "submission_line")
    If Len(submissionLine) = 0 Then submissionLine = REGVAL_MSG_SUBMIT_DONE_FALLBACK
    ApplyStatusLineToWrittenSheets book, submissionLine

    If Len(gStyleError) > 0 Or Len(gStatusError) > 0 Then ShowStyleNotice
End Sub


'======================================================================
' 加载态：开跑之前先把状态条写上、把按钮按住
'======================================================================
Private Sub BeginRun(ByVal ws As Worksheet, ByVal loadingText As String)
    gBusy = True
    ' 先写状态条再调引擎：宏阻塞执行引擎时界面不重绘，
    ' 这一句必须先画出来，用户才看得出「在跑」而不是「卡住了」。
    If Not ws Is Nothing Then SetStatusLine ws, loadingText, 0
    Application.StatusBar = loadingText
    DoEvents
    SetButtonsEnabled ws, False
End Sub

Private Sub EndRun()
    gBusy = False
    SetButtonsEnabled ActiveSheet, True
End Sub

' 按钮启停：执行期间解掉 OnAction（点了没反应），底色换成中性灰
Private Sub SetButtonsEnabled(ByVal ws As Worksheet, ByVal enabled As Boolean)
    If ws Is Nothing Then Exit Sub
    SetOneButtonEnabled ws, "BtnRegValValidate", "RegVal_Validate", enabled
    SetOneButtonEnabled ws, "BtnRegValPush", "RegVal_Push", enabled
End Sub

Private Sub SetOneButtonEnabled(ByVal ws As Worksheet, ByVal btnName As String, _
                                ByVal actionName As String, ByVal enabled As Boolean)
    Dim shp As Shape

    On Error Resume Next
    Set shp = ws.Shapes(btnName)
    On Error GoTo 0
    If shp Is Nothing Then Exit Sub

    On Error Resume Next
    If enabled Then
        shp.OnAction = actionName
        shp.Fill.ForeColor.RGB = RegValFillForRole("header")
        shp.TextFrame.Characters.Font.Color = RegValTextForRole("header")
    Else
        shp.OnAction = ""
        shp.Fill.ForeColor.RGB = RegValFillForRole("neutral")
        shp.TextFrame.Characters.Font.Color = RegValTextForRole("neutral")
    End If
    On Error GoTo 0
End Sub

' 窗口底部的提示（不弹框的轻量提示走这里）
Private Sub SetWindowHint(ByVal text As String)
    Application.StatusBar = text
End Sub


'======================================================================
' 校验 → 写回
'
' 返回结果契约原文（成功）或空串（失败）。
' 失败时错误信息已经记在 gError* 里 —— 由调用方决定用哪个渠道说
' （状态条错误行 + 对话框），本函数自己不弹框。
'======================================================================
Private Function RunValidateAndWriteBack(ByVal book As Workbook) As String
    Dim enginePath As String
    Dim outPath As String
    Dim errPath As String
    Dim stateDb As String
    Dim cmd As String
    Dim exitCode As Long
    Dim jsonText As String
    Dim contractVersion As String
    Dim expectedColumns As Variant
    Dim actualColumns As Variant

    ResetErrorInfo

    enginePath = REGVAL_ENGINE_PATH
    outPath = WorkDirFor(book) & ".regval_result.json"
    errPath = WorkDirFor(book) & ".regval_error.json"
    stateDb = REGVAL_STATE_DB

    If Not FileExists(enginePath) Then
        SetErrorInfo "ENGINE_NOT_FOUND", "", _
                     RegValFmt(REGVAL_MSG_ENGINE_MISSING, "path", enginePath), _
                     REGVAL_MSG_ENGINE_MISSING_HINT
        Exit Function
    End If

    ' 上一次的错误报告先清掉：这一轮没报错的话，那个文件就是过期的
    If FileExists(errPath) Then Kill errPath

    ' 绝对路径一律用 Chr$(34) 包引号：路径里有空格或中文也不会散架
    cmd = Chr$(34) & enginePath & Chr$(34) & " validate" & _
          " --input " & Chr$(34) & book.FullName & Chr$(34) & _
          " --out " & Chr$(34) & outPath & Chr$(34) & _
          " --state-db " & Chr$(34) & stateDb & Chr$(34) & _
          " --error-out " & Chr$(34) & errPath & Chr$(34)

    ' 阻塞执行：等引擎跑完再读结果（0 = 不弹窗，True = 等待结束）
    exitCode = RunEngine(cmd)
    If exitCode <> 0 Then
        ReadErrorReport errPath
        Exit Function
    End If

    If Not FileExists(outPath) Then
        SetErrorInfo "CONTRACT_NOT_PRODUCED", "", REGVAL_MSG_NO_CONTRACT, ""
        Exit Function
    End If

    jsonText = ReadUtf8File(outPath)

    ' 版本闸门：不匹配就中止，绝不部分写回
    contractVersion = JsonStringValue(jsonText, "schema_version")
    If contractVersion <> REGVAL_SCHEMA_VERSION Then
        SetErrorInfo "CONTRACT_VERSION_MISMATCH", RegValErrorText("CONTRACT_VERSION_MISMATCH"), _
                     RegValFmt(REGVAL_MSG_EXPECT_VALUE, "value", REGVAL_SCHEMA_VERSION) & vbCrLf & _
                     RegValFmt(REGVAL_MSG_ACTUAL_VALUE, "value", contractVersion) & vbCrLf & _
                     REGVAL_MSG_UNTOUCHED, _
                     RegValErrorText("CONTRACT_VERSION_MISMATCH")
        Exit Function
    End If

    ' 列名闸门：契约里的 5 个新列必须与宏常量一致，否则同样中止
    expectedColumns = Split(REGVAL_COLUMN_NAMES, "|")
    actualColumns = JsonColumnNames(jsonText)
    If Not ColumnsMatch(expectedColumns, actualColumns) Then
        SetErrorInfo "CONTRACT_VERSION_MISMATCH", RegValErrorText("CONTRACT_VERSION_MISMATCH"), _
                     RegValFmt(REGVAL_MSG_COLUMNS_EXPECTED, "value", REGVAL_COLUMN_NAMES) & vbCrLf & _
                     RegValFmt(REGVAL_MSG_COLUMNS_ACTUAL, "value", Join(actualColumns, "|")) & vbCrLf & _
                     REGVAL_MSG_UNTOUCHED, _
                     RegValErrorText("CONTRACT_VERSION_MISMATCH")
        Exit Function
    End If

    WriteBackNewColumns book, jsonText

    ' 批次号留档（「上报」按钮按批次走）
    gLastBatchRef = JsonStringValue(jsonText, "batch_ref")
    gLastContractPath = outPath

    RunValidateAndWriteBack = jsonText
End Function


'======================================================================
' 写回：只写新列，原表已有列、格式与公式一律不动
'======================================================================
Private Sub WriteBackNewColumns(ByVal book As Workbook, ByVal jsonText As String)
    Dim rowObjects As Variant
    Dim expected As Variant
    Dim i As Long
    Dim gStart As Long
    Dim gEnd As Long
    Dim sheetName As String
    Dim targetSheet As Worksheet
    Dim firstNewCol As Long
    Dim chunkStart As Long
    Dim chunkEnd As Long
    Dim nRows As Long
    Dim arr() As Variant
    Dim j As Long
    Dim lastRow As Long

    expected = Split(REGVAL_COLUMN_NAMES, "|")
    rowObjects = JsonObjectList(JsonArrayText(jsonText, "rows"))
    If UBound(rowObjects) < 0 Then Exit Sub

    gWrittenSheets = ""

    i = 0
    Do While i <= UBound(rowObjects)
        ' 按来源表分组（契约里的行按工作表顺序排列）
        sheetName = JsonStringValue(CStr(rowObjects(i)), "source_ns")
        gStart = i
        Do While i <= UBound(rowObjects) And _
                 JsonStringValue(CStr(rowObjects(i)), "source_ns") = sheetName
            i = i + 1
        Loop
        gEnd = i - 1

        Set targetSheet = SheetByName(book, sheetName)
        If Not targetSheet Is Nothing Then
            firstNewCol = FirstFreeColumn(targetSheet, 1)
            WriteHeaders targetSheet, firstNewCol, expected

            ' 分块写回：每块一次 Range.Value2 批量赋值
            ' chunkEnd 是这一块的**最后一行**（闭区间），不是「下一块的起点」——
            ' 差一行就会去读表外的行，小表直接越界。
            chunkStart = gStart
            Do While chunkStart <= gEnd
                chunkEnd = chunkStart
                Do While chunkEnd < gEnd And (chunkEnd - chunkStart + 1) < REGVAL_CHUNK_ROWS
                    chunkEnd = chunkEnd + 1
                Loop
                nRows = chunkEnd - chunkStart + 1
                ReDim arr(0 To nRows - 1, 0 To UBound(expected))
                For j = 0 To nRows - 1
                    arr(j, 0) = JsonStringValue(CStr(rowObjects(chunkStart + j)), "conclusion")
                    arr(j, 1) = JsonStringValue(CStr(rowObjects(chunkStart + j)), "issue_types")
                    arr(j, 2) = JsonStringValue(CStr(rowObjects(chunkStart + j)), "issue_detail")
                    arr(j, 3) = JsonStringValue(CStr(rowObjects(chunkStart + j)), "suspected_duplicate")
                    arr(j, 4) = JsonStringValue(CStr(rowObjects(chunkStart + j)), "submission_status")
                Next j

                targetSheet.Range( _
                    targetSheet.Cells(2 + (chunkStart - gStart), firstNewCol), _
                    targetSheet.Cells(2 + (chunkStart - gStart) + nRows - 1, firstNewCol + UBound(expected)) _
                ).Value2 = arr

                chunkStart = chunkEnd + 1
            Loop

            lastRow = 2 + (gEnd - gStart)

            ' 样式：值写完之后再套（业务逻辑与样式分离，见文件末尾「样式应用」一节）
            If HINT_RESULT_FILL Then
                ApplyResultColumnStyles targetSheet, firstNewCol, 2, lastRow
            End If

            gWrittenSheets = gWrittenSheets & sheetName & "|"
        End If
    Loop
End Sub

' 列头写回原表首行之后（接在原表已有列之后，不动原表任何一格）
Private Sub WriteHeaders(ws As Worksheet, ByVal firstNewCol As Long, ByVal expected As Variant)
    Dim i As Long
    For i = LBound(expected) To UBound(expected)
        ws.Cells(1, firstNewCol + i).Value = expected(i)
    Next i
End Sub


'======================================================================
' 底部状态条
'
' 状态条写在数据下面一行（隔 REGVAL_STATUS_ROW_GAP 行留白），
' 横跨 5 个新列 —— 不碰原表已有列。
'
' 句子由引擎算好放在契约的 summary 段，宏原样显示；宏不自行统计、
' 不自行拼句子（表里与终端里必须是一句话）。
'======================================================================
Private Sub ApplyStatusLineToWrittenSheets(ByVal book As Workbook, ByVal text As String)
    Dim names As Variant
    Dim i As Long
    Dim ws As Worksheet
    Dim wroteAny As Boolean

    names = Split(gWrittenSheets, "|")
    For i = LBound(names) To UBound(names)
        If Len(names(i)) > 0 Then
            Set ws = SheetByName(book, CStr(names(i)))
            If Not ws Is Nothing Then
                SetStatusLine ws, text, 0
                wroteAny = True
            End If
        End If
    Next i

    ' 一行都没写回（空表）时，状态条仍要落到当前表上 ——
    ' 「没有行」也是一句话，不能因为没东西可写就不说。
    If Not wroteAny Then
        If Not ActiveSheet Is Nothing Then SetStatusLine ActiveSheet, text, 0
    End If
End Sub

' 写一行状态条（合并 5 个新列的那一行，浅灰底 + 深灰字）
Private Sub SetStatusLine(ByVal ws As Worksheet, ByVal text As String, ByVal lastDataRow As Long)
    Dim firstNewCol As Long
    Dim statusRow As Long
    Dim rng As Range

    On Error GoTo StatusFail

    firstNewCol = FirstFreeColumn(ws, 1)
    statusRow = StatusRowFor(ws, lastDataRow)

    Set rng = ws.Range(ws.Cells(statusRow, firstNewCol), ws.Cells(statusRow, firstNewCol + 4))
    ' 先解开再合 —— 重复校验不报「无法合并单元格」
    rng.MergeCells = False
    rng.Merge
    rng.Cells(1, 1).Value = text

    With rng
        .Interior.Color = RegValFillForRole("statusbar")
        .Font.Color = RegValTextForRole("statusbar")
        .Font.Name = T_BODY_FONT
        .Font.Size = T_BODY_SIZE
        .HorizontalAlignment = xlLeft
        .VerticalAlignment = xlCenter
        .IndentLevel = 1
    End With

    ' 窗口底部的状态栏也报一句：不看表头的人也能知道这一轮跑了什么
    Application.StatusBar = text
    Exit Sub

StatusFail:
    gStatusError = Err.Description
End Sub

' 状态条落在第几行：数据末行往下空几行
Private Function StatusRowFor(ByVal ws As Worksheet, ByVal lastDataRow As Long) As Long
    Dim lastUsed As Long
    lastUsed = ws.Cells(ws.Rows.Count, 1).End(xlUp).Row
    If lastUsed < lastDataRow Then lastUsed = lastDataRow
    If lastUsed < 2 Then lastUsed = 2
    StatusRowFor = lastUsed + REGVAL_STATUS_ROW_GAP
End Function


'======================================================================
' 错误态：错误报告 → 状态条错误行 + 对话框
'======================================================================
' 一轮开始前清掉上一轮的残留（样式 / 状态条 / 错误信息）
Private Sub ResetRunInfo()
    gStyleError = ""
    gStatusError = ""
    ResetErrorInfo
End Sub

Private Sub ResetErrorInfo()
    gHasError = False
    gErrorCode = ""
    gErrorZh = ""
    gErrorMessage = ""
    gErrorAdvice = ""
End Sub

Private Sub SetErrorInfo(ByVal code As String, ByVal codeZh As String, _
                          ByVal message As String, ByVal advice As String)
    gHasError = True
    gErrorCode = code
    gErrorZh = codeZh
    gErrorMessage = message
    gErrorAdvice = advice
End Sub

' 读引擎写的错误报告 JSON
'
' 中文说明取本地表（RegValErrorText，与 src/errors.rs 同源）——
' 表里没有才退回引擎给的那句，不自己编。
Private Sub ReadErrorReport(ByVal errPath As String)
    Dim jsonText As String
    Dim code As String
    Dim codeZh As String
    Dim msg As String
    Dim advice As String
    Dim localZh As String

    If Not FileExists(errPath) Then
        SetErrorInfo "NO_ERROR_REPORT", "", REGVAL_MSG_NO_ERROR_REPORT, ""
        Exit Sub
    End If

    jsonText = ReadUtf8File(errPath)
    code = JsonStringValue(jsonText, "code")
    codeZh = JsonStringValue(jsonText, "code_zh")
    msg = JsonStringValue(jsonText, "message")
    advice = JsonStringValue(jsonText, "advice")

    localZh = RegValErrorText(code)
    If Len(localZh) = 0 Or localZh = code Then localZh = codeZh

    SetErrorInfo code, localZh, msg, advice
End Sub

' 状态条上的错误行：「错误码：XXX · 中文说明」
Private Function ErrorStatusText() As String
    Dim template As String
    Dim zh As String

    zh = gErrorZh
    If Len(zh) = 0 Then
        template = REGVAL_MSG_ERROR_LINE_UNKNOWN
    Else
        template = REGVAL_MSG_ERROR_LINE
    End If
    ErrorStatusText = RegValFmt(RegValFmt(template, "code", gErrorCode), "code_zh", zh)
End Function

' 错误行写到状态条上（阻断级错误的第一层渠道）
Private Sub WriteErrorStatusLine(ByVal ws As Worksheet)
    If ws Is Nothing Then Exit Sub
    SetStatusLine ws, ErrorStatusText(), 0
    Application.StatusBar = ErrorStatusText()
End Sub

' 阻断级错误的第二层渠道：对话框把错误码、中文说明与处理建议说全
Private Sub ShowErrorDialog()
    Dim text As String

    If Not gHasError Then Exit Sub

    text = ErrorStatusText()
    If Len(gErrorMessage) > 0 Then text = text & vbCrLf & gErrorMessage
    If Len(gErrorAdvice) > 0 Then text = text & vbCrLf & REGVAL_MSG_ADVICE_PREFIX & gErrorAdvice

    MsgBox text, vbCritical, REGVAL_MSG_DIALOG_TITLE
End Sub

' 样式没套上：数据写回去了、外观没跟上 —— 如实说一句（不重填单元格）
Private Sub ShowStyleNotice()
    Dim text As String

    text = ""
    If Len(gStyleError) > 0 Then
        text = RegValFmt(REGVAL_MSG_STYLE_ERROR, "detail", gStyleError)
    End If
    If Len(gStatusError) > 0 Then
        If Len(text) > 0 Then text = text & vbCrLf
        text = text & RegValFmt(REGVAL_MSG_STATUSBAR_FAILED, "detail", gStatusError)
    End If
    If Len(text) = 0 Then Exit Sub

    MsgBox text, vbExclamation, REGVAL_MSG_DIALOG_TITLE
End Sub


'======================================================================
' 引擎调用
'======================================================================
Private Function RunEngine(ByVal cmd As String) As Long
    Dim shell As Object
    Set shell = CreateObject("WScript.Shell")
    ' 0 = 不显示窗口，True = 阻塞等待引擎结束
    RunEngine = shell.Run(cmd, 0, True)
End Function

' 前置条件：得先有一张打开着的表
'
' 这一句属于「还没开始」而不是「有结果了」，所以单独放在这里说 ——
' 结果级的提示（填充 + 状态条）不与它混在一起。
Private Function EnsureWorkbook(ByRef book As Workbook) As Boolean
    Set book = TargetBook()
    If book Is Nothing Then
        MsgBox REGVAL_MSG_NO_WORKBOOK, vbExclamation, REGVAL_MSG_DIALOG_TITLE
        Exit Function
    End If
    EnsureWorkbook = True
End Function

' 表里有未保存的改动时先问一句：引擎读的是磁盘上的文件
Private Function EnsureSaved(ByVal book As Workbook) As Boolean
    If book.Saved Then
        EnsureSaved = True
        Exit Function
    End If

    If MsgBox(REGVAL_MSG_UNSAVED_CHANGES & vbCrLf & _
              REGVAL_MSG_UNSAVED_CHANGES_HINT & vbCrLf & _
              REGVAL_MSG_UNSAVED_CHANGES_QUESTION, _
              vbYesNo + vbQuestion, REGVAL_MSG_DIALOG_TITLE) = vbYes Then
        Application.DisplayAlerts = False
        book.Save
        Application.DisplayAlerts = True
        EnsureSaved = True
    End If
End Function


'======================================================================
' 列位置
'======================================================================
' 原表首行最后一个有内容的列号
Private Function LastUsedColumn(ws As Worksheet, ByVal headerRow As Long) As Long
    LastUsedColumn = ws.Cells(headerRow, ws.Columns.Count).End(xlToLeft).Column
End Function

' 新列首列号：已经写过一遍时沿用原位置（重跑不重复堆列）
Private Function FirstFreeColumn(ws As Worksheet, ByVal headerRow As Long) As Long
    Dim expected As Variant
    Dim i As Long
    Dim lastCol As Long

    expected = Split(REGVAL_COLUMN_NAMES, "|")
    lastCol = LastUsedColumn(ws, headerRow)
    For i = 1 To lastCol
        If Trim$(CStr(ws.Cells(headerRow, i).Value)) = CStr(expected(0)) Then
            FirstFreeColumn = i
            Exit Function
        End If
    Next i
    FirstFreeColumn = lastCol + 1
End Function

' 按名字取工作表（取不到返回 Nothing）
Private Function SheetByName(ByVal book As Workbook, ByVal sheetName As String) As Worksheet
    Dim ws As Worksheet
    On Error Resume Next
    Set ws = book.Worksheets(sheetName)
    On Error GoTo 0
    Set SheetByName = ws
End Function

' 操作对象：当前激活的工作簿（宏装在模板里时就是模板本身；
' 宏装成加载项时就是用户正在填的那张表）
Private Function TargetBook() As Workbook
    If Application.Workbooks.Count = 0 Then Exit Function
    Set TargetBook = ActiveWorkbook
End Function

' 结果契约与错误报告放在哪：表格所在目录（表格没保存时放临时目录）
Private Function WorkDirFor(ByVal book As Workbook) As String
    If Len(book.Path) = 0 Then
        WorkDirFor = Environ$("TEMP")
        If Len(WorkDirFor) = 0 Then WorkDirFor = CurDir$
    Else
        WorkDirFor = book.Path & Application.PathSeparator
    End If
End Function


'======================================================================
' JSON 读取（只取本宏需要的字段；不引外部库）
'======================================================================
' 读 UTF-8 文件（契约 JSON 是 UTF-8，按 UTF-16 解码，避免 ANSI 读取乱码）
Private Function ReadUtf8File(ByVal path As String) As String
    Dim fnum As Integer
    Dim size As Long
    Dim raw() As Byte
    Dim i As Long
    Dim b As Long
    Dim out As String
    Dim codePoint As Long

    fnum = FreeFile
    Open path For Binary As #fnum
    size = LOF(fnum)
    If size = 0 Then
        Close #fnum
        ReadUtf8File = ""
        Exit Function
    End If
    ReDim raw(1 To size)
    Get #fnum, , raw
    Close #fnum

    ' 跳过 UTF-8 BOM
    i = 1
    If size >= 3 Then
        If raw(1) = 239 And raw(2) = 187 And raw(3) = 191 Then i = 4
    End If

    Do While i <= size
        b = raw(i)
        If b < 128 Then
            out = out & Chr$(b)
            i = i + 1
        ElseIf b < 224 And i + 1 <= size Then
            codePoint = (b And 31) * 64
            codePoint = codePoint + (raw(i + 1) And 63)
            out = out & ChrW$(codePoint)
            i = i + 2
        ElseIf i + 2 <= size Then
            codePoint = (b And 15) * 4096
            codePoint = codePoint + ((raw(i + 1) And 63) * 64)
            codePoint = codePoint + (raw(i + 2) And 63)
            out = out & ChrW$(codePoint)
            i = i + 3
        Else
            out = out & ChrW$(65533)   ' 截断字节按替换字符处理，不中断
            i = i + 1
        End If
    Loop

    ReadUtf8File = out
End Function

' 取字符串字段值（找不到返回空串）
Private Function JsonStringValue(ByVal jsonText As String, ByVal key As String) As String
    Dim k As Long
    Dim p As Long
    Dim ch As String
    Dim out As String

    k = InStr(1, jsonText, """" & key & """")
    If k = 0 Then
        JsonStringValue = ""
        Exit Function
    End If
    p = InStr(k + Len(key) + 2, jsonText, ":")
    If p = 0 Then
        JsonStringValue = ""
        Exit Function
    End If
    p = p + 1
    Do While p <= Len(jsonText) And Mid$(jsonText, p, 1) <> """"
        p = p + 1
    Loop
    If p > Len(jsonText) Then
        JsonStringValue = ""
        Exit Function
    End If
    p = p + 1
    Do While p <= Len(jsonText)
        ch = Mid$(jsonText, p, 1)
        If ch = "\"" Then
            If Mid$(jsonText, p + 1, 1) = "\"" Then
                out = out & """"
                p = p + 2
            Else
                p = p + 1
                Exit Do
            End If
        ElseIf ch = "\" Then
            out = out & Mid$(jsonText, p + 1, 1)
            p = p + 2
        Else
            out = out & ch
            p = p + 1
        End If
    Loop
    JsonStringValue = out
End Function

' 取数组字段的原文（"key": [ ... ]）
Private Function JsonArrayText(ByVal jsonText As String, ByVal key As String) As String
    Dim k As Long
    Dim p As Long
    Dim depth As Long
    Dim ch As String
    Dim inQuote As Boolean
    Dim startPos As Long

    k = InStr(1, jsonText, """" & key & """")
    If k = 0 Then
        JsonArrayText = ""
        Exit Function
    End If
    p = InStr(k + Len(key) + 2, jsonText, ":")
    If p = 0 Then
        JsonArrayText = ""
        Exit Function
    End If
    p = p + 1
    Do While p <= Len(jsonText) And Mid$(jsonText, p, 1) <> "["
        p = p + 1
    Loop
    If p > Len(jsonText) Then
        JsonArrayText = ""
        Exit Function
    End If
    startPos = p
    depth = 0
    inQuote = False
    Do While p <= Len(jsonText)
        ch = Mid$(jsonText, p, 1)
        If ch = """" Then
            If inQuote Then
                If Mid$(jsonText, p + 1, 1) = """" Then
                    p = p + 1
                Else
                    inQuote = False
                End If
            Else
                inQuote = True
            End If
        ElseIf Not inQuote Then
            If ch = "[" Then depth = depth + 1
            If ch = "]" Then
                depth = depth - 1
                If depth = 0 Then
                    JsonArrayText = Mid$(jsonText, startPos, p - startPos + 1)
                    Exit Function
                End If
            End If
        End If
        p = p + 1
    Loop
    JsonArrayText = ""
End Function

' 取对象字段的原文（"key": { ... }）—— 状态条那段话就在 summary 对象里
Private Function JsonObjectText(ByVal jsonText As String, ByVal key As String) As String
    Dim k As Long
    Dim p As Long
    Dim depth As Long
    Dim ch As String
    Dim inQuote As Boolean
    Dim startPos As Long

    k = InStr(1, jsonText, """" & key & """")
    If k = 0 Then
        JsonObjectText = ""
        Exit Function
    End If
    p = InStr(k + Len(key) + 2, jsonText, ":")
    If p = 0 Then
        JsonObjectText = ""
        Exit Function
    End If
    p = p + 1
    Do While p <= Len(jsonText) And Mid$(jsonText, p, 1) <> "{"
        p = p + 1
    Loop
    If p > Len(jsonText) Then
        JsonObjectText = ""
        Exit Function
    End If
    startPos = p
    depth = 0
    inQuote = False
    Do While p <= Len(jsonText)
        ch = Mid$(jsonText, p, 1)
        If ch = """" Then
            If inQuote Then
                If Mid$(jsonText, p + 1, 1) = """" Then
                    p = p + 1
                Else
                    inQuote = False
                End If
            Else
                inQuote = True
            End If
        ElseIf Not inQuote Then
            If ch = "{" Then depth = depth + 1
            If ch = "}" Then
                depth = depth - 1
                If depth = 0 Then
                    JsonObjectText = Mid$(jsonText, startPos, p - startPos + 1)
                    Exit Function
                End If
            End If
        End If
        p = p + 1
    Loop
    JsonObjectText = ""
End Function

' 把数组拆成顶层对象（每个对象一段原文）
Private Function JsonObjectList(ByVal arrayText As String) As Variant
    Dim result() As String
    Dim count As Long
    Dim p As Long
    Dim depth As Long
    Dim ch As String
    Dim inQuote As Boolean
    Dim itemStart As Long

    count = 0
    ReDim result(0 To 0)

    p = 1
    Do While p <= Len(arrayText)
        ch = Mid$(arrayText, p, 1)
        If ch = """" Then
            If inQuote Then
                If Mid$(arrayText, p + 1, 1) = """" Then
                    p = p + 1
                Else
                    inQuote = False
                End If
            Else
                inQuote = True
            End If
        ElseIf Not inQuote Then
            If ch = "{" Then
                If depth = 0 Then itemStart = p
                depth = depth + 1
            ElseIf ch = "}" Then
                depth = depth - 1
                If depth = 0 Then
                    count = count + 1
                    ReDim Preserve result(0 To count)
                    result(count) = Mid$(arrayText, itemStart, p - itemStart + 1)
                End If
            End If
        End If
        p = p + 1
    Loop

    If count = 0 Then
        JsonObjectList = Array()
    Else
        JsonObjectList = result
    End If
End Function

' 取契约里的新列名（columns 数组里的 name）
Private Function JsonColumnNames(ByVal jsonText As String) As Variant
    Dim objects As Variant
    Dim names() As String
    Dim i As Long
    Dim count As Long

    objects = JsonObjectList(JsonArrayText(jsonText, "columns"))
    count = 0
    ReDim names(0 To 0)
    For i = LBound(objects) To UBound(objects)
        count = count + 1
        ReDim Preserve names(0 To count)
        names(count) = JsonStringValue(CStr(objects(i)), "name")
    Next i
    If count = 0 Then
        JsonColumnNames = Array()
    Else
        JsonColumnNames = names
    End If
End Function

' 两个列名数组是否一致
Private Function ColumnsMatch(ByVal expected As Variant, ByVal actual As Variant) As Boolean
    Dim i As Long
    If UBound(expected) <> UBound(actual) Then
        ColumnsMatch = False
        Exit Function
    End If
    For i = LBound(expected) To UBound(actual)
        If CStr(expected(i)) <> CStr(actual(i)) Then
            ColumnsMatch = False
            Exit Function
        End If
    Next i
    ColumnsMatch = True
End Function


'======================================================================
' 样式应用（按 token 填充与设字体）
'
' 业务逻辑与样式分离：前面几节只管「算什么、写哪里」，
' 这一节只管「长什么样」。
'
' 配色、字体、字号、列宽、边框**全部**取自 RegValTokens 模块 ——
' 那是引擎按 src/ui/tokens.rs 生成的常量模块，本文件里不写死任何色值
' （契约测试会 grep 本文件，发现裸 #RRGGBB 直接判失败）。
'
' 取值口径（与引擎 token 一一对应）：
'   · 校验结论列：结论自己的颜色（通过=绿 / 有提示=黄 / 不通过=红），加粗
'   · 问题类型 / 问题说明两列：跟着问题类型走（口径问题红、疑似重复黄）
'   · 疑似重复项列：有疑似项才黄底，字用等宽（列里有行键，便于对齐核对）
'   · 上报状态列：回执的颜色（已上报绿 / 被退回橙 / 待重试灰），加粗
'   · 左侧 3px 色条：口径问题与疑似重复的行加一条竖色条，最显眼
'   · 底部状态条：浅灰底 + 深灰字，横跨 5 个新列
'
' 只碰新列那几格：原表已有列的单元格值、格式与公式一律不动。
' 行高是**行级**属性（新列所在行会一起变），这是视觉目标卡要求的
' 「行高 1.3」；除此之外原表外观不变。
'======================================================================

' 给 5 个新列套样式（表头 + 数据行）
Private Sub ApplyResultColumnStyles(ws As Worksheet, ByVal firstNewCol As Long, _
                                    ByVal firstDataRow As Long, ByVal lastDataRow As Long)
    Dim nCols As Long
    Dim conclusionCol As Long
    Dim issueCol As Long
    Dim detailCol As Long
    Dim suspectCol As Long
    Dim statusCol As Long
    Dim i As Long
    Dim rowRole As String
    Dim typeRole As String
    Dim statusRole As String
    Dim suspectRole As String

    On Error GoTo StyleFail

    nCols = UBound(Split(REGVAL_COLUMN_NAMES, "|"))
    conclusionCol = firstNewCol
    issueCol = firstNewCol + 1
    detailCol = firstNewCol + 2
    suspectCol = firstNewCol + 3
    statusCol = firstNewCol + 4

    ' 整块基础排版：字体、字号、细线、行高（一次套完，别逐格调用）
    ApplyColumnMetrics ws, firstNewCol, nCols, firstDataRow, lastDataRow

    ' 表头：深青灰底 + 白字
    ApplyHeaderStyle ws, firstNewCol, nCols

    If lastDataRow < firstDataRow Then Exit Sub

    For i = firstDataRow To lastDataRow
        rowRole = RegValRoleForConclusion(CStr(ws.Cells(i, conclusionCol).Value))
        typeRole = RegValRoleForIssueTypes(CStr(ws.Cells(i, issueCol).Value))
        statusRole = RegValRoleForSubmissionStatus(CStr(ws.Cells(i, statusCol).Value))
        suspectRole = RegValRoleForSuspected(CStr(ws.Cells(i, suspectCol).Value))

        ' 校验结论列：绿/黄/红 + 加粗
        ApplyRoleStyle ws.Range(ws.Cells(i, conclusionCol)), rowRole, True

        ' 问题类型 / 问题说明两列：跟着问题类型走
        ApplyRoleStyle ws.Range(ws.Cells(i, issueCol)), typeRole, False
        ApplyRoleStyle ws.Range(ws.Cells(i, detailCol)), typeRole, False

        ' 疑似重复项列：有疑似项才上黄底，字用等宽
        If suspectRole = "suspect" Then
            ApplyRoleStyle ws.Range(ws.Cells(i, suspectCol)), suspectRole, False
            ApplyMonoFont ws.Range(ws.Cells(i, suspectCol))
        End If

        ' 上报状态列：回执的颜色 + 加粗
        If statusRole <> "neutral" Then
            ApplyRoleStyle ws.Range(ws.Cells(i, statusCol)), statusRole, True
        End If

        ' 左侧 3px 色条：问题行最显眼
        If rowRole = "issue" Or rowRole = "suspect" Then
            ApplyLeftBar ws.Range(ws.Cells(i, conclusionCol)), rowRole
        End If
    Next i

    Exit Sub

StyleFail:
    ' 样式失败不影响已经写回的数据，但要如实说一声，不能闷声吞掉
    gStyleError = Err.Description
End Sub

' 整块基础排版：列宽、正文字体、字号、细线、行高
Private Sub ApplyColumnMetrics(ws As Worksheet, ByVal firstNewCol As Long, ByVal nCols As Long, _
                               ByVal firstDataRow As Long, ByVal lastDataRow As Long)
    Dim block As Range

    ' 列宽（不论有没有数据行都要设 —— 空表也得把列摆好）
    ws.Columns(firstNewCol).ColumnWidth = T_WIDTH_CONCLUSION
    ws.Columns(firstNewCol + 1).ColumnWidth = T_WIDTH_ISSUE
    ws.Columns(firstNewCol + 2).ColumnWidth = T_WIDTH_DETAIL
    ws.Columns(firstNewCol + 3).ColumnWidth = T_WIDTH_SUSPECT
    ws.Columns(firstNewCol + 4).ColumnWidth = T_WIDTH_STATUS

    If lastDataRow < firstDataRow Then Exit Sub

    Set block = ws.Range(ws.Cells(firstDataRow, firstNewCol), ws.Cells(lastDataRow, firstNewCol + nCols))

    With block.Font
        .Name = T_BODY_FONT
        .Size = T_BODY_SIZE
    End With

    ' 新列区自身的细线（不动原表已有格子的边框）
    With block.Borders
        .LineStyle = xlContinuous
        .Weight = T_GRID_WEIGHT
        .Color = T_GRID_COLOR
    End With

    ' 行高（行级属性：新列所在行一起变，这是视觉目标卡要的「行高 1.3」）
    ws.Range(ws.Cells(firstDataRow, 1), ws.Cells(lastDataRow, 1)).RowHeight = _
        Application.StandardHeight * T_ROW_HEIGHT_RATIO
End Sub

' 表头样式：深青灰底 + 白字 + 居中
Private Sub ApplyHeaderStyle(ws As Worksheet, ByVal firstNewCol As Long, ByVal nCols As Long)
    Dim header As Range

    Set header = ws.Range(ws.Cells(1, firstNewCol), ws.Cells(1, firstNewCol + nCols))

    With header
        .Interior.Color = RegValFillForRole("header")
        .Font.Color = RegValTextForRole("header")
        .Font.Name = T_BODY_FONT
        .Font.Size = T_BODY_SIZE
        .Font.Bold = True
        .HorizontalAlignment = xlCenter
        .VerticalAlignment = xlCenter
    End With
End Sub

' 按角色上色：底色 + 字色（字重由调用处决定）
Private Sub ApplyRoleStyle(rng As Range, ByVal role As String, ByVal bold As Boolean)
    rng.Interior.Color = RegValFillForRole(role)
    rng.Font.Color = RegValTextForRole(role)
    If bold Then rng.Font.Bold = True
End Sub

' 键值列用等宽字体（行键、幂等键这类哈希串，便于肉眼对齐核对）
Private Sub ApplyMonoFont(rng As Range)
    rng.Font.Name = T_MONO_FONT
    rng.Font.Size = T_MONO_SIZE
End Sub

' 左侧 3px 色条：Excel 标准边框里最接近的是 xlThick
Private Sub ApplyLeftBar(rng As Range, ByVal role As String)
    With rng.Borders(xlEdgeLeft)
        .LineStyle = xlContinuous
        .Weight = T_BAR_WEIGHT
        .Color = RegValTextForRole(role)
    End With
End Sub
