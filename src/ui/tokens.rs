//! 视觉 token —— 表格内插件样式的**唯一出处**。
//!
//! # 这一层为什么存在
//!
//! 本产品的界面落在 Excel 原生能力上（单元格填充色、字体、列宽、边框），
//! 不自绘 UI。自绘 UI 里的「设计 token」只给前端用；这里的 token 多两个去处：
//!
//! 1. **宏**：VBA 只能写死数值，所以 token 由引擎生成成
//!    `macro/vba/RegValTokens.bas`（`regval tokens --vba --out ...`）。
//!    宏里只引用 `T_*` 常量与 `RegVal*ForRole` 函数，**不写死任何色值**。
//! 2. **文档**：README 与 `docs/concepts/visual-goal.md` 里的配色表直接来自
//!    本模块（`regval tokens --out tokens.json`），文档与界面不会各说一套。
//!
//! # 一处定义
//!
//! 全仓的色值只在本模块的 [`PALETTE`] 里出现，且每种色值只出现一次；
//! 语义角色（issue / suspect / passed …）只**引用**调色板条目，不复制色值。
//! 改一个颜色只改一处，宏与文档同时跟着变 —— `tests/ui_tokens_test.rs`
//! 会逐字比对生成物与本模块，改歪了立刻被测出来。
//!
//! # 映射表也只写一处
//!
//! 「问题类型 → 填充色与字色」的映射写在 [`ISSUE_KIND_ROLES`]，
//! 校验结论与上报状态的映射写在 [`CONCLUSION_ROLES`] /
//! [`SUBMISSION_STATUS_ROLES`]。宏侧的对应函数是**生成**出来的
//! （`RegValRoleForIssueTypes` 等），宏代码里不散落任何映射。

use serde_json::{json, Value};

/// 视觉 token 版本（token 的结构变了才升；样式取值变了不动版本）
pub const TOKENS_SCHEMA_VERSION: &str = "regval.tokens/1.0.0";

/// 调色板 —— 全仓唯一的色值出处。
///
/// 每个色值在本文件里只出现一次。取自 `docs/concepts/visual-goal.md`
/// 的视觉目标卡（起始态 / 首个可用瞬间 / 成形态 三态用到的颜色）。
///
/// 命名约定：`*_surface` 是底色，`*_ink` 是字色。
///
/// 调色板里查不到色值时的兜底色。它是**兜底**、不是配色 —— 但色值字面量
/// 仍然只许出现一次（全仓唯一出处的规矩），所以给它一个名字。
const FALLBACK_COLOR: &str = "#000000";

pub const PALETTE: &[(&str, &str)] = &[
    // 原表白底与正文
    ("white", "#FFFFFF"),
    ("ink", "#1F2933"),
    // 表头深青灰
    ("slate", "#2F3E46"),
    // 口径问题：浅红底 + 深红字
    ("issue_surface", "#FDE7E9"),
    ("issue_ink", "#A3202A"),
    // 疑似重复待确认：浅黄底 + 深琥珀字
    ("suspect_surface", "#FFF4DB"),
    ("suspect_ink", "#8A6100"),
    // 校验通过 / 已上报：浅绿底 + 深绿字
    ("ok_surface", "#E4F3E8"),
    ("ok_ink", "#1E6B39"),
    // 被退回：浅橙底 + 深橙字（成形态「橙底」）
    ("warn_surface", "#FBE3D2"),
    ("warn_ink", "#9A4A00"),
    // 待重试：浅灰底 + 中灰字
    ("neutral_surface", "#E8ECF1"),
    ("neutral_ink", "#4A5A67"),
    // 底部状态条
    ("statusbar_surface", "#F4F6F8"),
    ("statusbar_ink", "#5B6B78"),
    // 表格细线
    ("grid", "#D5DCE2"),
];

/// 样式角色 —— 样式按**角色**取，不按具体问题类型各写一套。
///
/// 一个角色对应一组「底色 + 字色」，宏按角色名查表即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StyleRole {
    /// 原表底 / 无异常
    Surface,
    /// 表头
    Header,
    /// 口径问题（阻断级）
    Issue,
    /// 疑似重复待确认 / 提示级
    Suspect,
    /// 校验通过
    Passed,
    /// 已上报（回执说已上报）
    Submitted,
    /// 被退回（回执说被退回）
    Rejected,
    /// 待重试
    Retry,
    /// 中性（未上报 / 未执行）
    Neutral,
    /// 底部状态条
    StatusBar,
}

impl StyleRole {
    /// 角色名（ASCII；宏与 JSON 契约都用它）
    pub fn name(self) -> &'static str {
        match self {
            StyleRole::Surface => "surface",
            StyleRole::Header => "header",
            StyleRole::Issue => "issue",
            StyleRole::Suspect => "suspect",
            StyleRole::Passed => "passed",
            StyleRole::Submitted => "submitted",
            StyleRole::Rejected => "rejected",
            StyleRole::Retry => "retry",
            StyleRole::Neutral => "neutral",
            StyleRole::StatusBar => "statusbar",
        }
    }

    /// 全部角色（顺序即 [`ROLE_STYLES`] 的顺序）
    pub fn all() -> &'static [StyleRole] {
        &[
            StyleRole::Surface,
            StyleRole::Header,
            StyleRole::Issue,
            StyleRole::Suspect,
            StyleRole::Passed,
            StyleRole::Submitted,
            StyleRole::Rejected,
            StyleRole::Retry,
            StyleRole::Neutral,
            StyleRole::StatusBar,
        ]
    }
}

/// 一个角色的配色（底色与字色都指向调色板条目）
#[derive(Debug, Clone, Copy)]
pub struct RoleStyle {
    pub role: StyleRole,
    /// 底色（调色板条目名）
    pub fill: &'static str,
    /// 字色（调色板条目名）
    pub text: &'static str,
}

/// 角色 → 配色。**唯一一处**。
///
/// 通过与已上报共用同一套绿（`ok_surface` / `ok_ink`）—— 两处语义不同，
/// 但「成了」在视觉上就该是同一个颜色；分开写两遍色值就会各改各的。
pub const ROLE_STYLES: &[RoleStyle] = &[
    RoleStyle { role: StyleRole::Surface, fill: "white", text: "ink" },
    RoleStyle { role: StyleRole::Header, fill: "slate", text: "white" },
    RoleStyle { role: StyleRole::Issue, fill: "issue_surface", text: "issue_ink" },
    RoleStyle { role: StyleRole::Suspect, fill: "suspect_surface", text: "suspect_ink" },
    RoleStyle { role: StyleRole::Passed, fill: "ok_surface", text: "ok_ink" },
    RoleStyle { role: StyleRole::Submitted, fill: "ok_surface", text: "ok_ink" },
    RoleStyle { role: StyleRole::Rejected, fill: "warn_surface", text: "warn_ink" },
    RoleStyle { role: StyleRole::Retry, fill: "neutral_surface", text: "neutral_ink" },
    RoleStyle { role: StyleRole::Neutral, fill: "white", text: "ink" },
    RoleStyle { role: StyleRole::StatusBar, fill: "statusbar_surface", text: "statusbar_ink" },
];

/// 字体族
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    /// 正文与中文
    Body,
    /// 键值列（行键、幂等键等哈希串）—— 等宽，便于肉眼对齐核对
    Mono,
}

/// 字体 token
#[derive(Debug, Clone, Copy)]
pub struct FontToken {
    pub kind: FontKind,
    /// 文档里的写法（给人看）
    pub display_name: &'static str,
    /// 宏里用的字体名（ASCII；中文 Windows 上照样认得）
    pub vba_name: &'static str,
    /// 字号（磅）
    pub size: f64,
}

/// 字体 token。**唯一一处**。
pub const FONTS: &[FontToken] = &[
    FontToken {
        kind: FontKind::Body,
        display_name: "微软雅黑 / PingFang SC",
        vba_name: "Microsoft YaHei",
        size: 10.5,
    },
    FontToken {
        kind: FontKind::Mono,
        display_name: "Consolas（行键、幂等键等键值列）",
        vba_name: "Consolas",
        size: 10.5,
    },
];

/// 一个写回新列的排版 token
#[derive(Debug, Clone, Copy)]
pub struct ColumnStyle {
    /// 列名（与契约的 5 个新列同名）
    pub name: &'static str,
    /// ASCII 别名（宏常量名与生成物用；中文列名不适合做标识符）
    pub key: &'static str,
    /// 列宽（Excel 字符宽）
    pub width: f64,
    /// 字体族
    pub font: FontKind,
    /// 是否加粗（状态列加粗，是视觉目标卡里的信息层级）
    pub bold: bool,
}

/// 5 个新列的排版 token。**唯一一处**。
///
/// 宽度同时是契约里 `columns[].width` 的出处 —— 契约与宏说的是同一套列宽。
pub const COLUMN_STYLES: &[ColumnStyle] = &[
    ColumnStyle { name: "校验结论", key: "conclusion", width: 10.0, font: FontKind::Body, bold: true },
    ColumnStyle { name: "问题类型", key: "issue", width: 18.0, font: FontKind::Body, bold: false },
    ColumnStyle { name: "问题说明（含依据）", key: "detail", width: 48.0, font: FontKind::Body, bold: false },
    ColumnStyle { name: "疑似重复项", key: "suspect", width: 20.0, font: FontKind::Mono, bold: false },
    ColumnStyle { name: "上报状态", key: "status", width: 12.0, font: FontKind::Body, bold: true },
];

/// 边框 token
#[derive(Debug, Clone, Copy)]
pub struct BorderToken {
    /// 用途名
    pub name: &'static str,
    /// Excel `LineStyle` 值
    pub weight: i64,
    /// 颜色（调色板条目名）
    pub color: &'static str,
}

/// 左侧色条：问题行最显眼的那 3px。
///
/// Excel 标准边框里最接近 3px 的是 `xlThick`（=4）—— 比它更细的
/// `xlThin`（=2）看不出来，比它更粗就没有了。
pub const BORDER_BAR: BorderToken = BorderToken {
    name: "bar",
    weight: 4,
    color: "issue_ink",
};

/// 表格细线：新列区自身的分隔线（不动原表已有格子的边框）
pub const BORDER_GRID: BorderToken = BorderToken {
    name: "grid",
    weight: 1,
    color: "grid",
};

/// 行高比例（相对 Excel 标准行高；视觉目标卡里的「行高 1.3」）
pub const ROW_HEIGHT_RATIO: f64 = 1.3;

/// 问题类型 → 样式角色。**唯一一处映射表**。
///
/// 顺序即优先级：阻断级在前、提示级在后。一行同时命中多类时取先命中的那类，
/// 行的底色跟着**最严重**的那类走（口径问题优先于疑似重复）。
pub const ISSUE_KIND_ROLES: &[(&str, StyleRole)] = &[
    ("必填缺失", StyleRole::Issue),
    ("枚举越界", StyleRole::Issue),
    ("单位不符", StyleRole::Issue),
    ("公式不符", StyleRole::Issue),
    ("数据形态错误", StyleRole::Issue),
    ("科室主数据不一致", StyleRole::Issue),
    ("科室名称写法不一致", StyleRole::Issue),
    ("疑似重复", StyleRole::Suspect),
    ("待人工判定", StyleRole::Suspect),
];

/// 校验结论 → 样式角色。
pub const CONCLUSION_ROLES: &[(&str, StyleRole)] = &[
    ("不通过", StyleRole::Issue),
    ("有提示", StyleRole::Suspect),
    ("通过", StyleRole::Passed),
];

/// 上报状态 → 样式角色（**前缀匹配**：状态串后面可能带补充说明）。
///
/// 前缀取自 `src/push/status.rs` 的状态取值；顺序即优先级。
pub const SUBMISSION_STATUS_ROLES: &[(&str, StyleRole)] = &[
    ("已上报", StyleRole::Submitted),
    ("幂等命中", StyleRole::Submitted),
    ("被退回", StyleRole::Rejected),
    ("待重试", StyleRole::Retry),
    ("未发出", StyleRole::Issue),
    ("回执待人工核对", StyleRole::Suspect),
    ("未执行", StyleRole::Neutral),
    ("未上报", StyleRole::Neutral),
];

/// 调色板条目名 → 色值
pub fn palette_hex(name: &str) -> Option<&'static str> {
    PALETTE.iter().find(|(n, _)| *n == name).map(|(_, hex)| *hex)
}

/// 角色 → 配色
pub fn role_style(role: StyleRole) -> &'static RoleStyle {
    ROLE_STYLES
        .iter()
        .find(|s| s.role == role)
        .unwrap_or_else(|| panic!("角色 {:?} 没有配色定义", role))
}

/// 角色名 → 角色
pub fn role_by_name(name: &str) -> Option<StyleRole> {
    StyleRole::all().iter().copied().find(|r| r.name() == name)
}

/// 单个问题类型 → 角色
pub fn role_for_issue_kind(kind: &str) -> StyleRole {
    ISSUE_KIND_ROLES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, r)| *r)
        .unwrap_or(StyleRole::Surface)
}

/// 一行的「问题类型」列取值 → 角色（多类时取优先级最高的）
pub fn role_for_issue_types(types: &str) -> StyleRole {
    if types.trim().is_empty() {
        return StyleRole::Surface;
    }
    for (kind, role) in ISSUE_KIND_ROLES {
        if types.contains(kind) {
            return *role;
        }
    }
    StyleRole::Surface
}

/// 校验结论 → 角色
pub fn role_for_conclusion(conclusion: &str) -> StyleRole {
    CONCLUSION_ROLES
        .iter()
        .find(|(k, _)| *k == conclusion)
        .map(|(_, r)| *r)
        .unwrap_or(StyleRole::Surface)
}

/// 上报状态 → 角色（前缀匹配）
pub fn role_for_submission_status(status: &str) -> StyleRole {
    let trimmed = status.trim();
    if trimmed.is_empty() {
        return StyleRole::Neutral;
    }
    for (prefix, role) in SUBMISSION_STATUS_ROLES {
        if trimmed.starts_with(prefix) {
            return *role;
        }
    }
    StyleRole::Neutral
}

/// 「疑似重复项」列取值 → 角色（空即没有疑似项）
pub fn role_for_suspected(suspected: &str) -> StyleRole {
    if suspected.trim().is_empty() {
        StyleRole::Surface
    } else {
        StyleRole::Suspect
    }
}

/// 列名 → 排版 token
pub fn column_style(name: &str) -> Option<&'static ColumnStyle> {
    COLUMN_STYLES.iter().find(|c| c.name == name)
}

/// 5 个新列的列宽（契约的 `columns[].width` 也从这里出）
pub const fn column_widths() -> [f64; 5] {
    [
        COLUMN_STYLES[0].width,
        COLUMN_STYLES[1].width,
        COLUMN_STYLES[2].width,
        COLUMN_STYLES[3].width,
        COLUMN_STYLES[4].width,
    ]
}

/// 字体 token
pub fn font(kind: FontKind) -> &'static FontToken {
    FONTS.iter().find(|f| f.kind == kind).expect("字体 token 齐全")
}

/// `#RRGGBB` → Excel 用的 BGR 长整型（`&HBBGGRR`）
///
/// Excel 的 `Interior.Color` 是 BGR 序，写成 RGB 会红蓝互换 —— 这种错在
/// 单元格里看不出来，只有肉眼比对才发现，所以转换只在这一处做。
pub fn vba_color(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    assert_eq!(h.len(), 6, "色值应为 #RRGGBB：{}", hex);
    let r = &h[0..2];
    let g = &h[2..4];
    let b = &h[4..6];
    format!("&H{}{}{}", b, g, r).to_uppercase()
}

/// `#RRGGBB` → Excel / JS 用的 BGR 数值（`Interior.Color` 要的是 BGR 序）
///
/// 与 [`vba_color`] 同一套换算，只是载体不同：VBA 写常量（`&HBBGGRR`），
/// WPS JSA 侧写数值。两种载体都只在这一处换算，别各写一遍。
pub fn bgr_number(hex: &str) -> u32 {
    let h = hex.trim_start_matches('#');
    assert_eq!(h.len(), 6, "色值应为 #RRGGBB：{}", hex);
    let r = u32::from_str_radix(&h[0..2], 16).expect("色值应为十六进制");
    let g = u32::from_str_radix(&h[2..4], 16).expect("色值应为十六进制");
    let b = u32::from_str_radix(&h[4..6], 16).expect("色值应为十六进制");
    (b << 16) | (g << 8) | r
}

/// token 清单（JSON）—— 文档与插件（WPS JSA 侧）共用同一份
pub fn manifest_json() -> Value {
    json!({
        "schema_version": TOKENS_SCHEMA_VERSION,
        "palette": PALETTE
            .iter()
            .map(|(name, hex)| json!({"name": name, "hex": hex, "vba": vba_color(hex)}))
            .collect::<Vec<_>>(),
        "roles": ROLE_STYLES
            .iter()
            .map(|s| json!({
                "role": s.role.name(),
                "fill": s.fill,
                "fill_hex": palette_hex(s.fill),
                "text": s.text,
                "text_hex": palette_hex(s.text),
            }))
            .collect::<Vec<_>>(),
        "fonts": FONTS
            .iter()
            .map(|f| json!({
                "kind": match f.kind { FontKind::Body => "body", FontKind::Mono => "mono" },
                "display_name": f.display_name,
                "vba_name": f.vba_name,
                "size": f.size,
            }))
            .collect::<Vec<_>>(),
        "columns": COLUMN_STYLES
            .iter()
            .map(|c| json!({
                "name": c.name,
                "key": c.key,
                "width": c.width,
                "font": match c.font { FontKind::Body => "body", FontKind::Mono => "mono" },
                "bold": c.bold,
            }))
            .collect::<Vec<_>>(),
        "borders": [
            json!({"name": BORDER_BAR.name, "weight": BORDER_BAR.weight, "color": BORDER_BAR.color}),
            json!({"name": BORDER_GRID.name, "weight": BORDER_GRID.weight, "color": BORDER_GRID.color}),
        ],
        "row_height_ratio": ROW_HEIGHT_RATIO,
        "mappings": {
            "issue_kind": ISSUE_KIND_ROLES
                .iter()
                .map(|(kind, role)| json!({"kind": kind, "role": role.name()}))
                .collect::<Vec<_>>(),
            "conclusion": CONCLUSION_ROLES
                .iter()
                .map(|(name, role)| json!({"value": name, "role": role.name()}))
                .collect::<Vec<_>>(),
            "submission_status": SUBMISSION_STATUS_ROLES
                .iter()
                .map(|(prefix, role)| json!({"prefix": prefix, "role": role.name()}))
                .collect::<Vec<_>>(),
        },
    })
}

/// 文档用的配色表（Markdown）
pub fn render_markdown() -> String {
    let mut s = String::new();
    s.push_str("| 角色 | 用途 | 底色 | 字色 |\n");
    s.push_str("|------|------|------|------|\n");
    for s_ in ROLE_STYLES {
        s.push_str(&format!(
            "| `{}` | {} | `{}` | `{}` |\n",
            s_.role.name(),
            role_purpose(s_.role),
            palette_hex(s_.fill).unwrap_or(""),
            palette_hex(s_.text).unwrap_or(""),
        ));
    }
    s
}

/// 角色的中文用途（文档与宏注释共用）
fn role_purpose(role: StyleRole) -> &'static str {
    match role {
        StyleRole::Surface => "原表底 / 无异常",
        StyleRole::Header => "表头",
        StyleRole::Issue => "口径问题（阻断级）",
        StyleRole::Suspect => "疑似重复待确认 / 提示级",
        StyleRole::Passed => "校验通过",
        StyleRole::Submitted => "已上报",
        StyleRole::Rejected => "被退回",
        StyleRole::Retry => "待重试",
        StyleRole::Neutral => "中性（未上报 / 未执行）",
        StyleRole::StatusBar => "底部状态条",
    }
}

/// 生成宏用的 VBA 常量模块。
///
/// 宏里不该出现色值字面量 —— 但 VBA 只能写死数值，所以由引擎把 token
/// 翻译成常量模块。宏引用 `T_*` 与 `RegVal*ForRole`，色值只存在于本模块。
pub fn render_vba_module() -> String {
    let mut s = String::new();
    s.push_str("'======================================================================\n");
    s.push_str("' RegValTokens —— 表格内插件的视觉 token（由 regval tokens --vba 生成）\n");
    s.push_str("'\n");
    s.push_str("' 唯一出处：src/ui/tokens.rs。改样式请改那里，再重新生成本文件：\n");
    s.push_str("'   regval tokens --vba --out macro/vba/RegValTokens.bas\n");
    s.push_str("' tests/ui_tokens_test.rs 会把本文件与引擎 token 逐字比对，改歪了会被测出来。\n");
    s.push_str("'\n");
    s.push_str("' 编码：本文件是 UTF-8。中文 Windows 的 VBE 按系统 ANSI 代码页读模块文件，\n");
    s.push_str("' 直接导入可能出现乱码 —— 按 macro/README.md 的说明另存为 ANSI/GBK 再导入。\n");
    s.push_str("'======================================================================\n");
    s.push_str("Option Explicit\n\n");
    s.push_str(&format!(
        "Public Const TOKENS_SCHEMA_VERSION As String = \"{}\"\n\n",
        TOKENS_SCHEMA_VERSION
    ));

    s.push_str("' ---- 调色板（Excel 用 BGR 长整型：&HBBGGRR）----\n");
    for (name, hex) in PALETTE {
        s.push_str(&format!(
            "Public Const T_{}_COLOR As Long = {}\n",
            name.to_uppercase(),
            vba_color(hex)
        ));
    }
    s.push('\n');

    s.push_str("' ---- 字体 ----\n");
    for f in FONTS {
        let const_name = match f.kind {
            FontKind::Body => "BODY",
            FontKind::Mono => "MONO",
        };
        s.push_str(&format!(
            "Public Const T_{}_FONT As String = \"{}\"\n",
            const_name, f.vba_name
        ));
        s.push_str(&format!(
            "Public Const T_{}_SIZE As Double = {}\n",
            const_name, f.size
        ));
    }
    s.push('\n');

    s.push_str("' ---- 行高与边框 ----\n");
    s.push_str(&format!(
        "Public Const T_ROW_HEIGHT_RATIO As Double = {}\n",
        ROW_HEIGHT_RATIO
    ));
    s.push_str(&format!(
        "Public Const T_BAR_WEIGHT As Long = {}   ' xlThick：左侧 3px 色条\n",
        BORDER_BAR.weight
    ));
    s.push_str(&format!(
        "Public Const T_GRID_WEIGHT As Long = {}  ' xlHairline：新列区细线\n",
        BORDER_GRID.weight
    ));
    s.push('\n');

    s.push_str("' ---- 列宽与字重 ----\n");
    for c in COLUMN_STYLES {
        s.push_str(&format!(
            "Public Const T_WIDTH_{} As Double = {}#\n",
            c.key.to_uppercase(),
            c.width
        ));
    }
    for c in COLUMN_STYLES {
        s.push_str(&format!(
            "Public Const T_BOLD_{} As Boolean = {}\n",
            c.key.to_uppercase(),
            if c.bold { "True" } else { "False" }
        ));
    }
    s.push('\n');

    s.push_str("' ---- 角色 → 填充色 ----\n");
    s.push_str("Public Function RegValFillForRole(ByVal role As String) As Long\n");
    s.push_str("    Select Case role\n");
    for st in ROLE_STYLES {
        s.push_str(&format!(
            "        Case \"{}\": RegValFillForRole = T_{}_COLOR\n",
            st.role.name(),
            st.fill.to_uppercase()
        ));
    }
    s.push_str("        Case Else: RegValFillForRole = T_WHITE_COLOR\n");
    s.push_str("    End Select\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 角色 → 字色 ----\n");
    s.push_str("Public Function RegValTextForRole(ByVal role As String) As Long\n");
    s.push_str("    Select Case role\n");
    for st in ROLE_STYLES {
        s.push_str(&format!(
            "        Case \"{}\": RegValTextForRole = T_{}_COLOR\n",
            st.role.name(),
            st.text.to_uppercase()
        ));
    }
    s.push_str("        Case Else: RegValTextForRole = T_INK_COLOR\n");
    s.push_str("    End Select\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 校验结论 → 角色 ----\n");
    s.push_str("Public Function RegValRoleForConclusion(ByVal conclusion As String) As String\n");
    s.push_str("    Select Case conclusion\n");
    for (value, role) in CONCLUSION_ROLES {
        s.push_str(&format!(
            "        Case \"{}\": RegValRoleForConclusion = \"{}\"\n",
            value,
            role.name()
        ));
    }
    s.push_str("        Case Else: RegValRoleForConclusion = \"neutral\"\n");
    s.push_str("    End Select\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 问题类型（多类以「、」连接）→ 角色 ----\n");
    s.push_str("' 按优先级取第一个命中的：阻断级在前，提示级在后。\n");
    s.push_str("Public Function RegValRoleForIssueTypes(ByVal issueTypes As String) As String\n");
    s.push_str("    If Trim$(issueTypes) = \"\" Then\n");
    s.push_str("        RegValRoleForIssueTypes = \"neutral\"\n");
    s.push_str("        Exit Function\n");
    s.push_str("    End If\n");
    let blocking: Vec<&str> = ISSUE_KIND_ROLES
        .iter()
        .filter(|(_, r)| *r == StyleRole::Issue)
        .map(|(k, _)| *k)
        .collect();
    let suspect: Vec<&str> = ISSUE_KIND_ROLES
        .iter()
        .filter(|(_, r)| *r == StyleRole::Suspect)
        .map(|(k, _)| *k)
        .collect();
    s.push_str(&format!(
        "    If {} Then\n",
        blocking
            .iter()
            .map(|k| format!("InStr(issueTypes, \"{}\") > 0", k))
            .collect::<Vec<_>>()
            .join(" Or ")
    ));
    s.push_str("        RegValRoleForIssueTypes = \"issue\"\n");
    s.push_str("    ElseIf ");
    s.push_str(
        &suspect
            .iter()
            .map(|k| format!("InStr(issueTypes, \"{}\") > 0", k))
            .collect::<Vec<_>>()
            .join(" Or "),
    );
    s.push_str(" Then\n");
    s.push_str("        RegValRoleForIssueTypes = \"suspect\"\n");
    s.push_str("    Else\n");
    s.push_str("        RegValRoleForIssueTypes = \"neutral\"\n");
    s.push_str("    End If\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 上报状态 → 角色（前缀匹配）----\n");
    s.push_str("Public Function RegValRoleForSubmissionStatus(ByVal statusText As String) As String\n");
    s.push_str("    If Trim$(statusText) = \"\" Then\n");
    s.push_str("        RegValRoleForSubmissionStatus = \"neutral\"\n");
    s.push_str("        Exit Function\n");
    s.push_str("    End If\n");
    for (prefix, role) in SUBMISSION_STATUS_ROLES {
        s.push_str(&format!(
            "    If Left$(statusText, Len(\"{}\")) = \"{}\" Then\n",
            prefix, prefix
        ));
        s.push_str(&format!(
            "        RegValRoleForSubmissionStatus = \"{}\"\n",
            role.name()
        ));
        s.push_str("        Exit Function\n");
        s.push_str("    End If\n");
    }
    s.push_str("    RegValRoleForSubmissionStatus = \"neutral\"\n");
    s.push_str("End Function\n\n");

    s.push_str("' ---- 疑似重复项列 → 角色（空即没有疑似项）----\n");
    s.push_str("Public Function RegValRoleForSuspected(ByVal suspectedText As String) As String\n");
    s.push_str("    If Trim$(suspectedText) = \"\" Then\n");
    s.push_str("        RegValRoleForSuspected = \"neutral\"\n");
    s.push_str("    Else\n");
    s.push_str("        RegValRoleForSuspected = \"suspect\"\n");
    s.push_str("    End If\n");
    s.push_str("End Function\n");

    s
}

/// 生成 WPS JSA 侧的 token 块（注入 `macro/wps/regval.js`）。
///
/// 与 [`render_vba_module`] **同源**：同一份调色板、同一套角色映射，
/// 只是载体从「VBA 常量」换成「JS 对象字面量」。
///
/// 两侧的宏函数名也刻意取成一样的（`RegValFillForRole` / `RegValRoleFor…`）——
/// 看宏代码的人不用先分清这是 VBA 还是 JS 才知道口径没变。
pub fn render_jsa_module() -> String {
    let mut s = String::new();
    s.push_str("// ----------------------------------------------------------------------\n");
    s.push_str("// 视觉 token（唯一出处：src/ui/tokens.rs，由 `regval macro` 注入）\n");
    s.push_str("// 与 Excel 侧的 RegValTokens.bas 同源：同一份调色板、同一套角色映射。\n");
    s.push_str("// ----------------------------------------------------------------------\n");
    s.push_str("var REGVAL_TOKENS = {\n");
    s.push_str(&format!("  schema_version: \"{}\",\n", TOKENS_SCHEMA_VERSION));
    s.push_str("  palette: {\n");
    for (name, hex) in PALETTE {
        s.push_str(&format!(
            "    \"{}\": {{ \"hex\": \"{}\", \"bgr\": {} }},\n",
            name,
            hex,
            bgr_number(hex)
        ));
    }
    s.push_str("  },\n");
    s.push_str("  roles: {\n");
    for st in ROLE_STYLES {
        s.push_str(&format!(
            "    \"{}\": {{ \"fill\": \"{}\", \"fill_bgr\": {}, \"text\": \"{}\", \"text_bgr\": {} }},\n",
            st.role.name(),
            st.fill,
            bgr_number(palette_hex(st.fill).unwrap_or(FALLBACK_COLOR)),
            st.text,
            bgr_number(palette_hex(st.text).unwrap_or(FALLBACK_COLOR)),
        ));
    }
    s.push_str("  },\n");
    s.push_str("  fonts: {\n");
    for f in FONTS {
        let key = match f.kind {
            FontKind::Body => "body",
            FontKind::Mono => "mono",
        };
        s.push_str(&format!(
            "    \"{}\": {{ \"name\": \"{}\", \"size\": {} }},\n",
            key, f.vba_name, f.size
        ));
    }
    s.push_str("  },\n");
    s.push_str("  columns: [\n");
    for c in COLUMN_STYLES {
        let font = match c.font {
            FontKind::Body => "body",
            FontKind::Mono => "mono",
        };
        s.push_str(&format!(
            "    {{ \"key\": \"{}\", \"name\": \"{}\", \"width\": {}, \"font\": \"{}\", \"bold\": {} }},\n",
            c.key, c.name, c.width, font, c.bold
        ));
    }
    s.push_str("  ],\n");
    s.push_str(&format!(
        "  borders: {{ \"bar\": {{ \"weight\": {}, \"color\": \"{}\" }}, \"grid\": {{ \"weight\": {}, \"color\": \"{}\" }} }},\n",
        BORDER_BAR.weight,
        palette_hex(BORDER_BAR.color).unwrap_or(""),
        BORDER_GRID.weight,
        palette_hex(BORDER_GRID.color).unwrap_or("")
    ));
    s.push_str(&format!("  row_height_ratio: {}\n", ROW_HEIGHT_RATIO));
    s.push_str("};\n\n");

    // ---- 角色 → 配色 ----
    s.push_str("function RegValFillForRole(role) {\n");
    s.push_str("  var r = REGVAL_TOKENS.roles[role];\n");
    s.push_str("  return r ? r.fill_bgr : REGVAL_TOKENS.palette[\"white\"].bgr;\n");
    s.push_str("}\n\n");
    s.push_str("function RegValTextForRole(role) {\n");
    s.push_str("  var r = REGVAL_TOKENS.roles[role];\n");
    s.push_str("  return r ? r.text_bgr : REGVAL_TOKENS.palette[\"ink\"].bgr;\n");
    s.push_str("}\n\n");

    // ---- 校验结论 → 角色 ----
    s.push_str("function RegValRoleForConclusion(conclusion) {\n");
    s.push_str("  var map = {");
    for (value, role) in CONCLUSION_ROLES {
        s.push_str(&format!(" \"{}\": \"{}\",", value, role.name()));
    }
    s.push_str(" };\n");
    s.push_str("  var key = RegValAsText(conclusion);\n");
    s.push_str("  return Object.prototype.hasOwnProperty.call(map, key) ? map[key] : \"neutral\";\n");
    s.push_str("}\n\n");

    // ---- 问题类型（多类以「、」连接）→ 角色：按优先级取第一个命中的 ----
    s.push_str("function RegValRoleForIssueTypes(issueTypes) {\n");
    s.push_str("  var t = RegValAsText(issueTypes);\n");
    s.push_str("  if (t.replace(/^\\s+|\\s+$/g, \"\") === \"\") return \"neutral\";\n");
    for (kind, role) in ISSUE_KIND_ROLES {
        s.push_str(&format!(
            "  if (t.indexOf(\"{}\") >= 0) return \"{}\";\n",
            kind,
            role.name()
        ));
    }
    s.push_str("  return \"neutral\";\n");
    s.push_str("}\n\n");

    // ---- 上报状态 → 角色（前缀匹配）----
    //
    // 映射写成一张表（数据），不写成一串 if（代码）：表是唯一出处，
    // 与 VBA 侧 [`SUBMISSION_STATUS_ROLES`] 同源，改一处两侧同时变。
    // 匹配语义仍是**前缀匹配** —— 表的键序即优先级（先命中先返回）。
    s.push_str("var REGVAL_SUBMISSION_STATUS_PREFIXES = {\n");
    for (prefix, role) in SUBMISSION_STATUS_ROLES {
        s.push_str(&format!("  \"{}\": \"{}\",\n", prefix, role.name()));
    }
    s.push_str("};\n\n");
    s.push_str("function RegValRoleForSubmissionStatus(statusText) {\n");
    s.push_str("  var t = RegValAsText(statusText);\n");
    s.push_str("  if (t.replace(/^\\s+|\\s+$/g, \"\") === \"\") return \"neutral\";\n");
    s.push_str("  var prefixes = Object.keys(REGVAL_SUBMISSION_STATUS_PREFIXES);\n");
    s.push_str("  for (var i = 0; i < prefixes.length; i++) {\n");
    s.push_str("    if (t.indexOf(prefixes[i]) === 0) return REGVAL_SUBMISSION_STATUS_PREFIXES[prefixes[i]];\n");
    s.push_str("  }\n");
    s.push_str("  return \"neutral\";\n");
    s.push_str("}\n\n");

    // ---- 疑似重复项列 → 角色（空即没有疑似项）----
    s.push_str("function RegValRoleForSuspected(suspectedText) {\n");
    s.push_str("  var t = RegValAsText(suspectedText);\n");
    s.push_str("  if (t.replace(/^\\s+|\\s+$/g, \"\") === \"\") return \"neutral\";\n");
    s.push_str("  return \"suspect\";\n");
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 调色板条目名与色值都唯一() {
        let mut names: Vec<&str> = PALETTE.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "调色板里有重名的条目");

        let mut hexes: Vec<&str> = PALETTE.iter().map(|(_, h)| *h).collect();
        hexes.sort_unstable();
        let before = hexes.len();
        hexes.dedup();
        assert_eq!(before, hexes.len(), "调色板里有重复的色值");
    }

    #[test]
    fn 角色的底色与字色都在调色板里() {
        for st in ROLE_STYLES {
            assert!(
                palette_hex(st.fill).is_some(),
                "角色 {} 的底色 {} 不在调色板里",
                st.role.name(),
                st.fill
            );
            assert!(
                palette_hex(st.text).is_some(),
                "角色 {} 的字色 {} 不在调色板里",
                st.role.name(),
                st.text
            );
        }
    }

    #[test]
    fn 色值转bgr不串色() {
        // 输入取自调色板（全仓色值只能在那儿出现一次），期望值写死 BGR 结果：
        // 白 → FFFFFF；表头深青灰 R=2F G=3E B=46 → BGR = 463E2F；
        // 问题浅红底 R=FD G=E7 B=E9 → BGR = E9E7FD
        assert_eq!(vba_color(palette_hex("white").unwrap()), "&HFFFFFF");
        assert_eq!(vba_color(palette_hex("slate").unwrap()), "&H463E2F");
        assert_eq!(
            vba_color(palette_hex("issue_surface").unwrap()),
            "&HE9E7FD"
        );
    }

    #[test]
    fn 四类口径问题都有配色归属() {
        assert_eq!(role_for_issue_kind("必填缺失"), StyleRole::Issue);
        assert_eq!(role_for_issue_kind("枚举越界"), StyleRole::Issue);
        assert_eq!(role_for_issue_kind("单位不符"), StyleRole::Issue);
        assert_eq!(role_for_issue_kind("疑似重复"), StyleRole::Suspect);
    }

    #[test]
    fn 一行多类问题时按优先级取最严重的() {
        // 口径问题优先于疑似重复
        assert_eq!(
            role_for_issue_types("疑似重复、必填缺失"),
            StyleRole::Issue
        );
        assert_eq!(role_for_issue_types("疑似重复"), StyleRole::Suspect);
        assert_eq!(role_for_issue_types(""), StyleRole::Surface);
    }

    #[test]
    fn 结论三档各归各的() {
        assert_eq!(role_for_conclusion("不通过"), StyleRole::Issue);
        assert_eq!(role_for_conclusion("有提示"), StyleRole::Suspect);
        assert_eq!(role_for_conclusion("通过"), StyleRole::Passed);
    }

    #[test]
    fn 上报状态按前缀匹配() {
        assert_eq!(role_for_submission_status("已上报"), StyleRole::Submitted);
        assert_eq!(
            role_for_submission_status("幂等命中（已上报，不重复推送）"),
            StyleRole::Submitted
        );
        assert_eq!(role_for_submission_status("被退回"), StyleRole::Rejected);
        assert_eq!(role_for_submission_status("待重试"), StyleRole::Retry);
        assert_eq!(role_for_submission_status("未上报"), StyleRole::Neutral);
        assert_eq!(role_for_submission_status(""), StyleRole::Neutral);
    }

    #[test]
    fn 疑似项列空即无样式() {
        assert_eq!(role_for_suspected(""), StyleRole::Surface);
        assert_eq!(role_for_suspected("   "), StyleRole::Surface);
        assert_eq!(role_for_suspected("疑似重复：与 表一 第 2 行一致"), StyleRole::Suspect);
    }

    #[test]
    fn 列宽与列定义对得上() {
        assert_eq!(COLUMN_STYLES.len(), 5);
        assert_eq!(column_widths(), [10.0, 18.0, 48.0, 20.0, 12.0]);
        assert_eq!(column_style("疑似重复项").unwrap().font, FontKind::Mono);
        assert!(column_style("上报状态").unwrap().bold);
    }

    #[test]
    fn 色值转bgr数值与vba常量同一套换算() {
        // 白 → FFFFFF；表头深青灰 R=2F G=3E B=46 → BGR 序 = 0x463E2F
        assert_eq!(bgr_number(palette_hex("white").unwrap()), 0xFFFFFF);
        assert_eq!(bgr_number(palette_hex("slate").unwrap()), 0x463E2F);
        assert_eq!(bgr_number(palette_hex("issue_surface").unwrap()), 0xE9E7FD);
    }

    #[test]
    fn jsa_token块与vba常量块同源() {
        let jsa = render_jsa_module();
        let vba = render_vba_module();

        // 调色板：每个色值在两侧都出现，且 BGR 数值与 VBA 常量一致
        for (name, hex) in PALETTE {
            assert!(
                jsa.contains(&format!("\"{}\": {{ \"hex\": \"{}\", \"bgr\": {} }}", name, hex, bgr_number(hex))),
                "JSA token 块里少了调色板条目 {}",
                name
            );
            assert!(
                vba.contains(&format!(
                    "Public Const T_{}_COLOR As Long = {}",
                    name.to_uppercase(),
                    vba_color(hex)
                )),
                "VBA 常量块里少了调色板条目 {}",
                name
            );
        }

        // 角色映射：两侧函数名一致，取值一致
        for fn_name in [
            "RegValFillForRole",
            "RegValTextForRole",
            "RegValRoleForConclusion",
            "RegValRoleForIssueTypes",
            "RegValRoleForSubmissionStatus",
            "RegValRoleForSuspected",
        ] {
            assert!(jsa.contains(&format!("function {}(", fn_name)), "JSA 块缺 {}", fn_name);
            assert!(vba.contains(&format!("Function {}(", fn_name)), "VBA 块缺 {}", fn_name);
        }

        // 结论三档、状态前缀、问题类型优先级：两侧逐项一致
        for (value, role) in CONCLUSION_ROLES {
            assert!(
                jsa.contains(&format!("\"{}\": \"{}\"", value, role.name())),
                "JSA 块缺结论映射 {}",
                value
            );
        }
        for (prefix, role) in SUBMISSION_STATUS_ROLES {
            assert!(
                jsa.contains(&format!("\"{}\": \"{}\"", prefix, role.name())),
                "JSA 块缺状态前缀映射 {}",
                prefix
            );
        }
        for (kind, role) in ISSUE_KIND_ROLES {
            assert!(
                jsa.contains(&format!("t.indexOf(\"{}\") >= 0) return \"{}\"", kind, role.name())),
                "JSA 块缺问题类型映射 {}",
                kind
            );
        }
    }
}
