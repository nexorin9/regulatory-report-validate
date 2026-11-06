//! 视觉 token 的验收测试：**一处定义、宏与文档共用、映射表齐全**。
//!
//! 这一层的意义在验收上很实：
//! - 色值散落在宏里 → 换配色要改十几处，改漏一处就有一行颜色不对；
//! - 映射表散落在宏里 → 「必填缺失算红还是黄」两处各写一份就会打架；
//! - 视觉目标卡与实现脱节 → 文档画的和表里看到的不是一回事。
//!
//! 所以这里不测「颜色好不好看」，只测「定义是不是只有一处、映射是不是齐、
//! 宏里有没有偷写色值、生成物有没有跟引擎漂」。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use regval::bind::dept::DEPT_NAME_ISSUE_KIND;
use regval::rules::model::RuleKind;
use regval::ui::tokens::{self, StyleRole};

fn project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读不到 {}：{}", path.display(), e))
}

/// 递归列出目录下的所有文件
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in std::fs::read_dir(&cur).expect("目录可读") {
            let path = entry.expect("目录项可读").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// 抽出文本里所有 `#RRGGBB` 形式的色值
fn hex_colors(text: &str) -> Vec<String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == '#' && i + 6 < bytes.len() {
            let candidate: String = bytes[i + 1..i + 7].iter().collect();
            if candidate.len() == 6 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
                out.push(format!("#{}", candidate.to_uppercase()));
                i += 7;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[test]
fn 色值全仓只在token模块里定义一次() {
    let src_dir = project_dir().join("src");
    let mut hits: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for file in files_under(&src_dir) {
        for color in hex_colors(&read(&file)) {
            hits
                .entry(color)
                .or_default()
                .push(file.strip_prefix(&project_dir()).unwrap().display().to_string());
        }
    }

    assert!(
        !hits.is_empty(),
        "token 模块里应有色值定义，实际一个都没扫到"
    );

    let tokens_rel = "src/ui/tokens.rs";
    for (color, files) in &hits {
        assert_eq!(
            files.len(),
            1,
            "色值 {} 在多处出现（{}）—— 色值只能有一处出处",
            color,
            files.join(" / ")
        );
        assert_eq!(
            files[0], tokens_rel,
            "色值 {} 出现在 {}，应当只出现在 {}",
            color,
            files[0],
            tokens_rel
        );
    }

    // 同一色值在 token 模块里也只能写一次
    let tokens_src = read(&src_dir.join("ui/tokens.rs"));
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for color in hex_colors(&tokens_src) {
        *seen.entry(color).or_insert(0) += 1;
    }
    for (color, count) in &seen {
        assert_eq!(*count, 1, "色值 {} 在 token 模块里被写了 {} 次", color, count);
    }
}

#[test]
fn 调色板条目名与色值都唯一() {
    let mut names: Vec<&str> = tokens::PALETTE.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "调色板里有重名的条目");

    let mut hexes: Vec<&str> = tokens::PALETTE.iter().map(|(_, h)| *h).collect();
    hexes.sort_unstable();
    let before = hexes.len();
    hexes.dedup();
    assert_eq!(before, hexes.len(), "调色板里有重复的色值");
}

#[test]
fn 四类口径问题都有配色归属() {
    // 产品说明里承诺的四类：必填缺失 / 枚举越界 / 单位不符 / 疑似重复
    assert_eq!(tokens::role_for_issue_kind("必填缺失"), StyleRole::Issue);
    assert_eq!(tokens::role_for_issue_kind("枚举越界"), StyleRole::Issue);
    assert_eq!(tokens::role_for_issue_kind("单位不符"), StyleRole::Issue);
    assert_eq!(tokens::role_for_issue_kind("疑似重复"), StyleRole::Suspect);
}

#[test]
fn 映射表覆盖引擎能产出的全部问题类型() {
    // 规则引擎能写进「问题类型」列的每一个取值，映射表里都得有
    let kinds = [
        RuleKind::RequiredField.as_issue_kind(),
        RuleKind::ConditionalRequired.as_issue_kind(),
        RuleKind::EnumBound.as_issue_kind(),
        RuleKind::UnitCheck.as_issue_kind(),
        RuleKind::FormulaRecalc.as_issue_kind(),
        RuleKind::DataShapeError.as_issue_kind(),
        RuleKind::SuspectedDuplicate.as_issue_kind(),
        // 实体绑定层写进问题的两类
        DEPT_NAME_ISSUE_KIND,
        "科室主数据不一致",
        "待人工判定",
    ];
    for kind in kinds {
        assert!(
            tokens::ISSUE_KIND_ROLES.iter().any(|(k, _)| *k == kind),
            "问题类型「{}」没有配色归属，映射表漏了一类",
            kind
        );
        assert_ne!(
            tokens::role_for_issue_kind(kind),
            StyleRole::Surface,
            "问题类型「{}」不该落到 surface（等于没上色）",
            kind
        );
    }
}

#[test]
fn 结论与上报状态各有归属() {
    for value in ["不通过", "有提示", "通过"] {
        assert_ne!(
            tokens::role_for_conclusion(value),
            StyleRole::Surface,
            "校验结论「{}」没有配色归属",
            value
        );
    }
    // 上报状态的取值见 src/push/status.rs（只取前缀匹配，这里逐个对一遍）
    for value in [
        "已上报",
        "幂等命中（已上报，不重复推送）",
        "被退回",
        "待重试",
        "未发出（数据不合格）",
        "回执待人工核对",
        "未执行（前面的步骤没成功）",
        "未上报",
    ] {
        assert_ne!(
            tokens::role_for_submission_status(value),
            StyleRole::Surface,
            "上报状态「{}」没有配色归属",
            value
        );
    }
}

#[test]
fn 宏里没有裸色值() {
    let source = read(&project_dir().join("macro/vba/RegVal.bas"));
    let colors = hex_colors(&source);
    assert!(
        colors.is_empty(),
        "宏里出现了裸色值 {} —— 色值只该在 src/ui/tokens.rs 里定义",
        colors.join(" / ")
    );
    assert!(
        !source.contains("RGB("),
        "宏里不该出现 RGB() 调用：颜色取自 token 模块的常量"
    );
    // 宏只引用 token 模块的常量与角色函数
    assert!(
        source.contains("RegValFillForRole") && source.contains("RegValTextForRole"),
        "宏里的样式必须走 token 模块的角色函数"
    );
    assert!(
        source.contains("RegValRoleForConclusion")
            && source.contains("RegValRoleForIssueTypes")
            && source.contains("RegValRoleForSubmissionStatus"),
        "宏里的角色判定必须走 token 模块生成的映射函数"
    );
}

#[test]
fn 宏引用的每个token符号都在生成模块里存在() {
    let macro_src = read(&project_dir().join("macro/vba/RegVal.bas"));
    let tokens_src = read(&project_dir().join("macro/vba/RegValTokens.bas"));
    // 样式 token 在 RegValTokens.bas，提示文案在 RegValMessages.bas —— 两个模块一起导入
    let messages_src = read(&project_dir().join("macro/vba/RegValMessages.bas"));

    let mut used: Vec<String> = Vec::new();
    for line in macro_src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("'") {
            continue; // 注释里提到的名字不算引用
        }
        for token in trimmed.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let is_token_symbol = (token.starts_with("T_") || token.starts_with("RegVal"))
                && !token.starts_with("RegVal_")
                && token.len() > 2;
            if is_token_symbol && !used.iter().any(|u| u == token) {
                used.push(token.to_string());
            }
        }
    }

    assert!(
        used.len() >= 8,
        "宏里应引用 token 模块的常量与角色函数，实际只认出 {} 个：{:?}",
        used.len(),
        used
    );
    for symbol in &used {
        assert!(
            tokens_src.contains(symbol.as_str()) || messages_src.contains(symbol.as_str()),
            "宏里用到 {}，但两个生成模块里都没有 —— 宏在院内机器上会编译不过",
            symbol
        );
    }
}

#[test]
fn 生成的宏常量模块与引擎token逐字一致() {
    let on_disk = read(&project_dir().join("macro/vba/RegValTokens.bas"));
    let rendered = tokens::render_vba_module();
    assert_eq!(
        on_disk, rendered,
        "macro/vba/RegValTokens.bas 与引擎 token 漂移了 —— 重新生成：\n  regval tokens --vba --out macro/vba/RegValTokens.bas"
    );
}

#[test]
fn 生成的宏常量模块里色值是excel要的bgr序() {
    let vba = tokens::render_vba_module();
    // #FDE7E9（浅红底）→ Excel 的 &HE9E7FD
    assert!(
        vba.contains("T_ISSUE_SURFACE_COLOR As Long = &HE9E7FD"),
        "色值没转成 BGR 序，红蓝会互换：\n{}",
        vba
    );
    // 表头 #2F3E46 → &H463E2F
    assert!(
        vba.contains("T_SLATE_COLOR As Long = &H463E2F"),
        "表头色没转成 BGR 序"
    );
}

#[test]
fn 视觉目标卡的配色全部有token覆盖() {
    let card = read(&project_dir().join("docs/concepts/visual-goal.md"));
    let card_colors = hex_colors(&card);
    assert!(
        card_colors.len() >= 10,
        "视觉目标卡里应至少有十来个色值，实际扫到 {} 个",
        card_colors.len()
    );
    for color in card_colors {
        assert!(
            tokens::PALETTE.iter().any(|(_, hex)| hex.eq_ignore_ascii_case(&color)),
            "视觉目标卡里的颜色 {} 在 token 调色板里没有对应条目",
            color
        );
    }
}

#[test]
fn 视觉目标卡的字体与字号全部有token覆盖() {
    let card = read(&project_dir().join("docs/concepts/visual-goal.md"));
    let body = tokens::FONTS
        .iter()
        .find(|f| f.kind == tokens::FontKind::Body)
        .expect("正文字体 token");
    let mono = tokens::FONTS
        .iter()
        .find(|f| f.kind == tokens::FontKind::Mono)
        .expect("等宽字体 token");

    assert!(
        card.contains("微软雅黑"),
        "视觉目标卡应写明正文字体"
    );
    assert!(
        body.display_name.contains("微软雅黑"),
        "正文字体 token 与视觉目标卡对不上：{}",
        body.display_name
    );
    assert_eq!(body.size, 10.5, "正文字号与视觉目标卡对不上");
    assert_eq!(mono.vba_name, "Consolas", "键值列应等宽");
    assert!(
        card.contains("Consolas"),
        "视觉目标卡应写明键值列用等宽字体"
    );
    // 状态列加粗
    let status = tokens::column_style("上报状态").expect("上报状态列");
    assert!(status.bold, "上报状态列应加粗（视觉目标卡的信息层级）");
    // 问题类型列宽约 18 字符
    let issue = tokens::column_style("问题类型").expect("问题类型列");
    assert_eq!(issue.width, 18.0, "问题类型列宽与视觉目标卡对不上");
    // 行高 1.3
    assert_eq!(tokens::ROW_HEIGHT_RATIO, 1.3, "行高与视觉目标卡对不上");
    // 左侧 3px 色条
    assert_eq!(
        tokens::BORDER_BAR.weight, 4,
        "左侧色条应取 Excel 最接近 3px 的 xlThick(=4)"
    );
}

#[test]
fn 契约列宽与token一致() {
    let widths = regval::contract::columns()
        .iter()
        .map(|c| c.width)
        .collect::<Vec<_>>();
    assert_eq!(
        widths,
        tokens::column_widths().to_vec(),
        "契约里的列宽与 token 不一致：宏按 token 设宽、契约按契约报宽，两边会不一样"
    );
}

#[test]
fn token清单可解析且字段齐全() {
    let manifest = tokens::manifest_json();
    let text = serde_json::to_string_pretty(&manifest).expect("token 清单可序列化");
    let parsed: Value = serde_json::from_str(&text).expect("token 清单是合法 JSON");

    assert_eq!(
        parsed["schema_version"],
        Value::String(tokens::TOKENS_SCHEMA_VERSION.to_string())
    );
    assert_eq!(
        parsed["palette"].as_array().map(|a| a.len()),
        Some(tokens::PALETTE.len())
    );
    assert_eq!(
        parsed["roles"].as_array().map(|a| a.len()),
        Some(tokens::ROLE_STYLES.len())
    );
    assert_eq!(
        parsed["columns"].as_array().map(|a| a.len()),
        Some(tokens::COLUMN_STYLES.len())
    );
    // 映射段逐条给出问题类型 → 角色
    let issue_roles = parsed["mappings"]["issue_kind"].as_array().expect("问题类型映射段");
    assert_eq!(
        issue_roles.len(),
        tokens::ISSUE_KIND_ROLES.len(),
        "映射表里的问题类型数应与 token 定义一致"
    );
    for entry in issue_roles {
        let kind = entry["kind"].as_str().expect("问题类型名");
        let role = entry["role"].as_str().expect("角色名");
        assert_eq!(
            tokens::role_for_issue_kind(kind).name(),
            role,
            "问题类型「{}」在清单里的角色与 token 定义对不上",
            kind
        );
    }
}
