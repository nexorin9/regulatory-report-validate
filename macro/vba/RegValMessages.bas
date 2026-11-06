'======================================================================
' RegValMessages —— 表格内插件的提示文案常量表（由 regval messages --vba 生成）
'
' 唯一出处：src/ui/messages.rs（提示文案）+ src/errors.rs（错误码中文说明）。
' 改文案请改那里，再重新生成本文件：
'   cargo run -- messages --vba --out macro/vba/RegValMessages.bas
' tests/ui_states_test.rs 会把本文件与引擎逐字比对，改歪了会被测出来。
'
' 编码：本文件是 UTF-8。中文 Windows 的 VBE 按系统 ANSI 代码页读模块文件，
' 直接导入可能出现乱码 —— 按 macro/README.md 的说明另存为 ANSI/GBK 再导入。
'======================================================================
Option Explicit

Public Const MESSAGES_SCHEMA_VERSION As String = "regval.messages/1.0.0"

' ---- 提示文案（宏里只引用这些常量，不写死中文）----
Public Const REGVAL_MSG_LOADING As String = "校验中…"
Public Const REGVAL_MSG_LOADING_PUSH As String = "上报中…"
Public Const REGVAL_MSG_EMPTY_ROWS As String = "表里没有数据行 · 未做校验"
Public Const REGVAL_MSG_EMPTY_ISSUES As String = "已校验 {rows} 行 · 未发现口径问题"
Public Const REGVAL_MSG_ERROR_LINE As String = "错误码：{code} · {code_zh}"
Public Const REGVAL_MSG_ERROR_LINE_UNKNOWN As String = "错误码：{code}"
Public Const REGVAL_MSG_DIALOG_TITLE As String = "卫健上报校验"
Public Const REGVAL_MSG_BUTTON_VALIDATE As String = "校验"
Public Const REGVAL_MSG_BUTTON_PUSH As String = "上报"
Public Const REGVAL_MSG_NO_WORKBOOK As String = "请先打开要校验的上报表。"
Public Const REGVAL_MSG_NOT_SAVED As String = "请先把表格保存到磁盘（引擎读的是磁盘上的文件）。"
Public Const REGVAL_MSG_INSTALLED As String = "已装好「校验」「上报」两个按钮。"
Public Const REGVAL_MSG_INSTALLED_HINT As String = "点「校验」把结果写回右侧 5 个新列；点「上报」把已确认的行推到院内上报网关。"
Public Const REGVAL_MSG_ENGINE_MISSING As String = "找不到引擎程序：{path}"
Public Const REGVAL_MSG_ENGINE_MISSING_HINT As String = "请修改 RegVal.bas 顶部的 REGVAL_ENGINE_PATH，或参照 macro/README.md 的安装说明。"
Public Const REGVAL_MSG_NO_CONTRACT As String = "引擎没有产出结果契约文件，原表未被改动。"
Public Const REGVAL_MSG_EXPECT_VALUE As String = "宏要求：{value}"
Public Const REGVAL_MSG_ACTUAL_VALUE As String = "引擎产出：{value}"
Public Const REGVAL_MSG_COLUMNS_EXPECTED As String = "宏期望的列：{value}"
Public Const REGVAL_MSG_COLUMNS_ACTUAL As String = "引擎产出的列：{value}"
Public Const REGVAL_MSG_UNTOUCHED As String = "原表未被改动。"
Public Const REGVAL_MSG_VALIDATE_FAILED As String = "校验未完成，原表未被改动。"
Public Const REGVAL_MSG_PUSH_FAILED As String = "上报未完成，原表未被改动。"
Public Const REGVAL_MSG_NO_BATCH As String = "这一批没有批次号，上报无从谈起。请检查引擎版本是否与宏一致。"
Public Const REGVAL_MSG_SUBMIT_DONE_FALLBACK As String = "上报完成，回执已回填"
Public Const REGVAL_MSG_UNSAVED_CHANGES As String = "表格有未保存的改动。"
Public Const REGVAL_MSG_UNSAVED_CHANGES_HINT As String = "引擎读的是磁盘上的文件，未保存的改动它看不到。"
Public Const REGVAL_MSG_UNSAVED_CHANGES_QUESTION As String = "要现在保存再继续吗？"
Public Const REGVAL_MSG_NO_ERROR_REPORT As String = "（引擎没有留下错误报告，请到命令行窗口查看详情）"
Public Const REGVAL_MSG_ADVICE_PREFIX As String = "处理建议："
Public Const REGVAL_MSG_STYLE_ERROR As String = "样式未完全套上：{detail}"
Public Const REGVAL_MSG_STATUSBAR_FAILED As String = "状态条未写上：{detail}"
Public Const REGVAL_MSG_SUMMARY_MISSING As String = "已校验（未取到摘要）"
Public Const REGVAL_MSG_BUSY As String = "上一轮还没跑完，请等它结束再点。"
Public Const REGVAL_MSG_WRITEBACK_NOTE As String = "结果已写回原表右侧 5 个新列（原表格式与公式未动）。"
Public Const REGVAL_MSG_PUSH_NOTE As String = "「上报状态」列已按回执回填（绿=已上报 / 橙=被退回 / 灰=待重试）。"

' ---- 错误码中文说明（与引擎 src/errors.rs 的 ErrorCode::zh() 同源）----
Public Const REGVAL_ERR_INPUT_UNREADABLE As String = "输入表读取失败"
Public Const REGVAL_ERR_UNSUPPORTED_FORMAT As String = "不支持的输入格式"
Public Const REGVAL_ERR_MISSING_COLUMN As String = "表头缺少必填列"
Public Const REGVAL_ERR_EMPTY_TABLE As String = "表中没有数据行"
Public Const REGVAL_ERR_RULE_SYNTAX_ERROR As String = "口径规则语法不通过"
Public Const REGVAL_ERR_RULE_LOAD_FAILED As String = "口径规则表装载失败"
Public Const REGVAL_ERR_RULE_BASIS_MISSING As String = "口径规则缺官方依据"
Public Const REGVAL_ERR_RULE_EXEC_ERROR As String = "口径规则执行失败"
Public Const REGVAL_ERR_RULE_SWITCH_UNCONFIRMED As String = "口径换版未经质控岗确认"
Public Const REGVAL_ERR_RULE_EFFECTIVE_DATE_NOT_REACHED As String = "口径版本的生效日期未到"
Public Const REGVAL_ERR_RULE_VERSION_NOT_FOUND As String = "规则集里没有该版本的口径规则"
Public Const REGVAL_ERR_DENOMINATOR_ZERO As String = "分母为 0"
Public Const REGVAL_ERR_INVALID_NUMERIC As String = "数值列不是合法数值"
Public Const REGVAL_ERR_CONFIG_MISSING As String = "缺少必需的环境变量"
Public Const REGVAL_ERR_CONFIG_INVALID As String = "环境变量取值不合法"
Public Const REGVAL_ERR_CONTRACT_WRITE_ERROR As String = "结果契约写出失败"
Public Const REGVAL_ERR_CONTRACT_VERSION_MISMATCH As String = "引擎与宏版本不一致，请升级"
Public Const REGVAL_ERR_IDEMPOTENCY_MISSING As String = "缺幂等键"
Public Const REGVAL_ERR_INVALID_STATE_TRANSITION As String = "行级状态迁移不合法"
Public Const REGVAL_ERR_ROW_STATE_CONFLICT As String = "行状态版本冲突"
Public Const REGVAL_ERR_UNCONFIRMED_ROW As String = "该行未经人工确认，不能上报"
Public Const REGVAL_ERR_CONFIRM_ACTOR_MISSING As String = "人工确认缺确认人"
Public Const REGVAL_ERR_GATEWAY_REJECTED As String = "上报网关退回"
Public Const REGVAL_ERR_GATEWAY_SERVER_ERROR As String = "上报网关返回服务端错误"
Public Const REGVAL_ERR_GATEWAY_TIMEOUT As String = "上报网关调用超时"
Public Const REGVAL_ERR_GATEWAY_CONNECTION_FAILED As String = "上报网关连不上"
Public Const REGVAL_ERR_GATEWAY_RESPONSE_INVALID As String = "上报网关回执结构不合法"
Public Const REGVAL_ERR_RECEIPT_CONFLICT As String = "幂等键与回执 ID 不是一一对应"
Public Const REGVAL_ERR_RECEIPT_LEDGER_MISMATCH As String = "回执台账交叉核对不通过"
Public Const REGVAL_ERR_DEPT_NOT_FOUND As String = "科室主数据不一致"
Public Const REGVAL_ERR_DEPT_NAME_MISMATCH As String = "科室名称写法与主数据不一致"
Public Const REGVAL_ERR_DEPT_DIRECTORY_UNAVAILABLE As String = "科室主数据目录不可用"
Public Const REGVAL_ERR_DEPT_DIRECTORY_CONNECTION_FAILED As String = "科室主数据目录连不上"
Public Const REGVAL_ERR_DEPT_DIRECTORY_TIMEOUT As String = "科室主数据目录调用超时"
Public Const REGVAL_ERR_DEPT_DIRECTORY_BAD_REQUEST As String = "科室主数据目录拒绝了请求"
Public Const REGVAL_ERR_DEPT_DIRECTORY_SERVER_ERROR As String = "科室主数据目录返回服务端错误"
Public Const REGVAL_ERR_DEPT_DIRECTORY_RESPONSE_INVALID As String = "科室主数据目录响应结构不合法"
Public Const REGVAL_ERR_MODEL_UNAVAILABLE As String = "模型端点不可达"
Public Const REGVAL_ERR_MODEL_CONNECTION_FAILED As String = "模型端点连不上"
Public Const REGVAL_ERR_MODEL_TIMEOUT As String = "模型端点调用超时"
Public Const REGVAL_ERR_MODEL_BAD_REQUEST As String = "模型端点拒绝了请求"
Public Const REGVAL_ERR_MODEL_SERVER_ERROR As String = "模型端点返回服务端错误"
Public Const REGVAL_ERR_MODEL_RESPONSE_INVALID As String = "模型响应未通过结构校验"
Public Const REGVAL_ERR_DB_ERROR As String = "本机校验库操作失败"
Public Const REGVAL_ERR_DB_CORRUPTED As String = "状态库文件损坏"
Public Const REGVAL_ERR_DB_VERSION_UNSUPPORTED As String = "状态库版本不被本引擎识别"
Public Const REGVAL_ERR_DB_MIGRATION_FAILED As String = "状态库迁移失败"
Public Const REGVAL_ERR_AUTHORITY_KEY_CONFLICT As String = "权威键重复"
Public Const REGVAL_ERR_BATCH_SUMMARY_CONFLICT As String = "批次汇总重放冲突"
Public Const REGVAL_ERR_IO_ERROR As String = "文件读写失败"
Public Const REGVAL_ERR_CAPABILITY_NOT_READY As String = "该能力尚未在本构建中提供"
Public Const REGVAL_ERR_BENCH_SET_INVALID As String = "标注基准集不合法"

' ---- 按错误码取中文说明（表里没有就原样返回错误码，不吞掉）----
Public Function RegValErrorText(ByVal code As String) As String
    Select Case UCase$(Trim$(code))
    Case "INPUT_UNREADABLE"
        RegValErrorText = REGVAL_ERR_INPUT_UNREADABLE
    Case "UNSUPPORTED_FORMAT"
        RegValErrorText = REGVAL_ERR_UNSUPPORTED_FORMAT
    Case "MISSING_COLUMN"
        RegValErrorText = REGVAL_ERR_MISSING_COLUMN
    Case "EMPTY_TABLE"
        RegValErrorText = REGVAL_ERR_EMPTY_TABLE
    Case "RULE_SYNTAX_ERROR"
        RegValErrorText = REGVAL_ERR_RULE_SYNTAX_ERROR
    Case "RULE_LOAD_FAILED"
        RegValErrorText = REGVAL_ERR_RULE_LOAD_FAILED
    Case "RULE_BASIS_MISSING"
        RegValErrorText = REGVAL_ERR_RULE_BASIS_MISSING
    Case "RULE_EXEC_ERROR"
        RegValErrorText = REGVAL_ERR_RULE_EXEC_ERROR
    Case "RULE_SWITCH_UNCONFIRMED"
        RegValErrorText = REGVAL_ERR_RULE_SWITCH_UNCONFIRMED
    Case "RULE_EFFECTIVE_DATE_NOT_REACHED"
        RegValErrorText = REGVAL_ERR_RULE_EFFECTIVE_DATE_NOT_REACHED
    Case "RULE_VERSION_NOT_FOUND"
        RegValErrorText = REGVAL_ERR_RULE_VERSION_NOT_FOUND
    Case "DENOMINATOR_ZERO"
        RegValErrorText = REGVAL_ERR_DENOMINATOR_ZERO
    Case "INVALID_NUMERIC"
        RegValErrorText = REGVAL_ERR_INVALID_NUMERIC
    Case "CONFIG_MISSING"
        RegValErrorText = REGVAL_ERR_CONFIG_MISSING
    Case "CONFIG_INVALID"
        RegValErrorText = REGVAL_ERR_CONFIG_INVALID
    Case "CONTRACT_WRITE_ERROR"
        RegValErrorText = REGVAL_ERR_CONTRACT_WRITE_ERROR
    Case "CONTRACT_VERSION_MISMATCH"
        RegValErrorText = REGVAL_ERR_CONTRACT_VERSION_MISMATCH
    Case "IDEMPOTENCY_MISSING"
        RegValErrorText = REGVAL_ERR_IDEMPOTENCY_MISSING
    Case "INVALID_STATE_TRANSITION"
        RegValErrorText = REGVAL_ERR_INVALID_STATE_TRANSITION
    Case "ROW_STATE_CONFLICT"
        RegValErrorText = REGVAL_ERR_ROW_STATE_CONFLICT
    Case "UNCONFIRMED_ROW"
        RegValErrorText = REGVAL_ERR_UNCONFIRMED_ROW
    Case "CONFIRM_ACTOR_MISSING"
        RegValErrorText = REGVAL_ERR_CONFIRM_ACTOR_MISSING
    Case "GATEWAY_REJECTED"
        RegValErrorText = REGVAL_ERR_GATEWAY_REJECTED
    Case "GATEWAY_SERVER_ERROR"
        RegValErrorText = REGVAL_ERR_GATEWAY_SERVER_ERROR
    Case "GATEWAY_TIMEOUT"
        RegValErrorText = REGVAL_ERR_GATEWAY_TIMEOUT
    Case "GATEWAY_CONNECTION_FAILED"
        RegValErrorText = REGVAL_ERR_GATEWAY_CONNECTION_FAILED
    Case "GATEWAY_RESPONSE_INVALID"
        RegValErrorText = REGVAL_ERR_GATEWAY_RESPONSE_INVALID
    Case "RECEIPT_CONFLICT"
        RegValErrorText = REGVAL_ERR_RECEIPT_CONFLICT
    Case "RECEIPT_LEDGER_MISMATCH"
        RegValErrorText = REGVAL_ERR_RECEIPT_LEDGER_MISMATCH
    Case "DEPT_NOT_FOUND"
        RegValErrorText = REGVAL_ERR_DEPT_NOT_FOUND
    Case "DEPT_NAME_MISMATCH"
        RegValErrorText = REGVAL_ERR_DEPT_NAME_MISMATCH
    Case "DEPT_DIRECTORY_UNAVAILABLE"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_UNAVAILABLE
    Case "DEPT_DIRECTORY_CONNECTION_FAILED"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_CONNECTION_FAILED
    Case "DEPT_DIRECTORY_TIMEOUT"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_TIMEOUT
    Case "DEPT_DIRECTORY_BAD_REQUEST"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_BAD_REQUEST
    Case "DEPT_DIRECTORY_SERVER_ERROR"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_SERVER_ERROR
    Case "DEPT_DIRECTORY_RESPONSE_INVALID"
        RegValErrorText = REGVAL_ERR_DEPT_DIRECTORY_RESPONSE_INVALID
    Case "MODEL_UNAVAILABLE"
        RegValErrorText = REGVAL_ERR_MODEL_UNAVAILABLE
    Case "MODEL_CONNECTION_FAILED"
        RegValErrorText = REGVAL_ERR_MODEL_CONNECTION_FAILED
    Case "MODEL_TIMEOUT"
        RegValErrorText = REGVAL_ERR_MODEL_TIMEOUT
    Case "MODEL_BAD_REQUEST"
        RegValErrorText = REGVAL_ERR_MODEL_BAD_REQUEST
    Case "MODEL_SERVER_ERROR"
        RegValErrorText = REGVAL_ERR_MODEL_SERVER_ERROR
    Case "MODEL_RESPONSE_INVALID"
        RegValErrorText = REGVAL_ERR_MODEL_RESPONSE_INVALID
    Case "DB_ERROR"
        RegValErrorText = REGVAL_ERR_DB_ERROR
    Case "DB_CORRUPTED"
        RegValErrorText = REGVAL_ERR_DB_CORRUPTED
    Case "DB_VERSION_UNSUPPORTED"
        RegValErrorText = REGVAL_ERR_DB_VERSION_UNSUPPORTED
    Case "DB_MIGRATION_FAILED"
        RegValErrorText = REGVAL_ERR_DB_MIGRATION_FAILED
    Case "AUTHORITY_KEY_CONFLICT"
        RegValErrorText = REGVAL_ERR_AUTHORITY_KEY_CONFLICT
    Case "BATCH_SUMMARY_CONFLICT"
        RegValErrorText = REGVAL_ERR_BATCH_SUMMARY_CONFLICT
    Case "IO_ERROR"
        RegValErrorText = REGVAL_ERR_IO_ERROR
    Case "CAPABILITY_NOT_READY"
        RegValErrorText = REGVAL_ERR_CAPABILITY_NOT_READY
    Case "BENCH_SET_INVALID"
        RegValErrorText = REGVAL_ERR_BENCH_SET_INVALID
    Case Else
        RegValErrorText = code
    End Select
End Function

' ---- 文案占位符替换（{rows} 这类标记换成实值）----
Public Function RegValFmt(ByVal template As String, ByVal key As String, ByVal value As String) As String
    RegValFmt = Replace(template, "{" & key & "}", value)
End Function
