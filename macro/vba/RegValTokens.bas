'======================================================================
' RegValTokens —— 表格内插件的视觉 token（由 regval tokens --vba 生成）
'
' 唯一出处：src/ui/tokens.rs。改样式请改那里，再重新生成本文件：
'   regval tokens --vba --out macro/vba/RegValTokens.bas
' tests/ui_tokens_test.rs 会把本文件与引擎 token 逐字比对，改歪了会被测出来。
'
' 编码：本文件是 UTF-8。中文 Windows 的 VBE 按系统 ANSI 代码页读模块文件，
' 直接导入可能出现乱码 —— 按 macro/README.md 的说明另存为 ANSI/GBK 再导入。
'======================================================================
Option Explicit

Public Const TOKENS_SCHEMA_VERSION As String = "regval.tokens/1.0.0"

' ---- 调色板（Excel 用 BGR 长整型：&HBBGGRR）----
Public Const T_WHITE_COLOR As Long = &HFFFFFF
Public Const T_INK_COLOR As Long = &H33291F
Public Const T_SLATE_COLOR As Long = &H463E2F
Public Const T_ISSUE_SURFACE_COLOR As Long = &HE9E7FD
Public Const T_ISSUE_INK_COLOR As Long = &H2A20A3
Public Const T_SUSPECT_SURFACE_COLOR As Long = &HDBF4FF
Public Const T_SUSPECT_INK_COLOR As Long = &H00618A
Public Const T_OK_SURFACE_COLOR As Long = &HE8F3E4
Public Const T_OK_INK_COLOR As Long = &H396B1E
Public Const T_WARN_SURFACE_COLOR As Long = &HD2E3FB
Public Const T_WARN_INK_COLOR As Long = &H004A9A
Public Const T_NEUTRAL_SURFACE_COLOR As Long = &HF1ECE8
Public Const T_NEUTRAL_INK_COLOR As Long = &H675A4A
Public Const T_STATUSBAR_SURFACE_COLOR As Long = &HF8F6F4
Public Const T_STATUSBAR_INK_COLOR As Long = &H786B5B
Public Const T_GRID_COLOR As Long = &HE2DCD5

' ---- 字体 ----
Public Const T_BODY_FONT As String = "Microsoft YaHei"
Public Const T_BODY_SIZE As Double = 10.5
Public Const T_MONO_FONT As String = "Consolas"
Public Const T_MONO_SIZE As Double = 10.5

' ---- 行高与边框 ----
Public Const T_ROW_HEIGHT_RATIO As Double = 1.3
Public Const T_BAR_WEIGHT As Long = 4   ' xlThick：左侧 3px 色条
Public Const T_GRID_WEIGHT As Long = 1  ' xlHairline：新列区细线

' ---- 列宽与字重 ----
Public Const T_WIDTH_CONCLUSION As Double = 10#
Public Const T_WIDTH_ISSUE As Double = 18#
Public Const T_WIDTH_DETAIL As Double = 48#
Public Const T_WIDTH_SUSPECT As Double = 20#
Public Const T_WIDTH_STATUS As Double = 12#
Public Const T_BOLD_CONCLUSION As Boolean = True
Public Const T_BOLD_ISSUE As Boolean = False
Public Const T_BOLD_DETAIL As Boolean = False
Public Const T_BOLD_SUSPECT As Boolean = False
Public Const T_BOLD_STATUS As Boolean = True

' ---- 角色 → 填充色 ----
Public Function RegValFillForRole(ByVal role As String) As Long
    Select Case role
        Case "surface": RegValFillForRole = T_WHITE_COLOR
        Case "header": RegValFillForRole = T_SLATE_COLOR
        Case "issue": RegValFillForRole = T_ISSUE_SURFACE_COLOR
        Case "suspect": RegValFillForRole = T_SUSPECT_SURFACE_COLOR
        Case "passed": RegValFillForRole = T_OK_SURFACE_COLOR
        Case "submitted": RegValFillForRole = T_OK_SURFACE_COLOR
        Case "rejected": RegValFillForRole = T_WARN_SURFACE_COLOR
        Case "retry": RegValFillForRole = T_NEUTRAL_SURFACE_COLOR
        Case "neutral": RegValFillForRole = T_WHITE_COLOR
        Case "statusbar": RegValFillForRole = T_STATUSBAR_SURFACE_COLOR
        Case Else: RegValFillForRole = T_WHITE_COLOR
    End Select
End Function

' ---- 角色 → 字色 ----
Public Function RegValTextForRole(ByVal role As String) As Long
    Select Case role
        Case "surface": RegValTextForRole = T_INK_COLOR
        Case "header": RegValTextForRole = T_WHITE_COLOR
        Case "issue": RegValTextForRole = T_ISSUE_INK_COLOR
        Case "suspect": RegValTextForRole = T_SUSPECT_INK_COLOR
        Case "passed": RegValTextForRole = T_OK_INK_COLOR
        Case "submitted": RegValTextForRole = T_OK_INK_COLOR
        Case "rejected": RegValTextForRole = T_WARN_INK_COLOR
        Case "retry": RegValTextForRole = T_NEUTRAL_INK_COLOR
        Case "neutral": RegValTextForRole = T_INK_COLOR
        Case "statusbar": RegValTextForRole = T_STATUSBAR_INK_COLOR
        Case Else: RegValTextForRole = T_INK_COLOR
    End Select
End Function

' ---- 校验结论 → 角色 ----
Public Function RegValRoleForConclusion(ByVal conclusion As String) As String
    Select Case conclusion
        Case "不通过": RegValRoleForConclusion = "issue"
        Case "有提示": RegValRoleForConclusion = "suspect"
        Case "通过": RegValRoleForConclusion = "passed"
        Case Else: RegValRoleForConclusion = "neutral"
    End Select
End Function

' ---- 问题类型（多类以「、」连接）→ 角色 ----
' 按优先级取第一个命中的：阻断级在前，提示级在后。
Public Function RegValRoleForIssueTypes(ByVal issueTypes As String) As String
    If Trim$(issueTypes) = "" Then
        RegValRoleForIssueTypes = "neutral"
        Exit Function
    End If
    If InStr(issueTypes, "必填缺失") > 0 Or InStr(issueTypes, "枚举越界") > 0 Or InStr(issueTypes, "单位不符") > 0 Or InStr(issueTypes, "公式不符") > 0 Or InStr(issueTypes, "数据形态错误") > 0 Or InStr(issueTypes, "科室主数据不一致") > 0 Or InStr(issueTypes, "科室名称写法不一致") > 0 Then
        RegValRoleForIssueTypes = "issue"
    ElseIf InStr(issueTypes, "疑似重复") > 0 Or InStr(issueTypes, "待人工判定") > 0 Then
        RegValRoleForIssueTypes = "suspect"
    Else
        RegValRoleForIssueTypes = "neutral"
    End If
End Function

' ---- 上报状态 → 角色（前缀匹配）----
Public Function RegValRoleForSubmissionStatus(ByVal statusText As String) As String
    If Trim$(statusText) = "" Then
        RegValRoleForSubmissionStatus = "neutral"
        Exit Function
    End If
    If Left$(statusText, Len("已上报")) = "已上报" Then
        RegValRoleForSubmissionStatus = "submitted"
        Exit Function
    End If
    If Left$(statusText, Len("幂等命中")) = "幂等命中" Then
        RegValRoleForSubmissionStatus = "submitted"
        Exit Function
    End If
    If Left$(statusText, Len("被退回")) = "被退回" Then
        RegValRoleForSubmissionStatus = "rejected"
        Exit Function
    End If
    If Left$(statusText, Len("待重试")) = "待重试" Then
        RegValRoleForSubmissionStatus = "retry"
        Exit Function
    End If
    If Left$(statusText, Len("未发出")) = "未发出" Then
        RegValRoleForSubmissionStatus = "issue"
        Exit Function
    End If
    If Left$(statusText, Len("回执待人工核对")) = "回执待人工核对" Then
        RegValRoleForSubmissionStatus = "suspect"
        Exit Function
    End If
    If Left$(statusText, Len("未执行")) = "未执行" Then
        RegValRoleForSubmissionStatus = "neutral"
        Exit Function
    End If
    If Left$(statusText, Len("未上报")) = "未上报" Then
        RegValRoleForSubmissionStatus = "neutral"
        Exit Function
    End If
    RegValRoleForSubmissionStatus = "neutral"
End Function

' ---- 疑似重复项列 → 角色（空即没有疑似项）----
Public Function RegValRoleForSuspected(ByVal suspectedText As String) As String
    If Trim$(suspectedText) = "" Then
        RegValRoleForSuspected = "neutral"
    Else
        RegValRoleForSuspected = "suspect"
    End If
End Function
