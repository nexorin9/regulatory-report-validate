//! 第四级（语义归并）：把模型给的结论并进重复检测漏斗。
//!
//! # 这一层是「模型结论」进产品主路径的**唯一**入口
//!
//! 前三级（确定性键 / 归一化 / 编辑距离）都不需要模型，第四级需要。
//! 于是模型就成了一个外部依赖：它可能连不上、可能给出一个置信度很低的结论、
//! 可能提到一行我们根本没问过的行。这一层负责把这些问题**挡在业务之外**：
//!
//! | 模型那边的情况 | 这一层的处置 |
//! |---|---|
//! | 端点没接 / 不可用 | 本级候选为空，状态照实报「未接入 / 不可用」，不假装查过 |
//! | 结论置信度达标 | 进「可合并候选」，等经办人逐条勾选确认 |
//! | 结论置信度不够 | 降级为「疑似待人工判定」，**不进可合并候选**（但照样写进结果契约） |
//! | 提到的行键找不到 | 跳过并计数（防模型编行键的第二道闸） |
//!
//! # 铁律：模型只标，不合并不推送
//!
//! 这里的输出**只是标注**：写进结果契约的 `suspected_duplicates`、写进原表
//! 「疑似重复项」列。合并与上报由经办人勾选确认后才由引擎执行
//! （确认记录落 `confirmations` 表），**模型永远不推进任何业务状态**。
//!
//! 本模块**不碰数据库、不碰上报客户端** —— 它只把模型结论翻译成候选对。
//! 接入点只在 `src/dupe/` 里（`src/push/`、`src/state/` 里不出现模型归并，
//! 有静态用例盯着）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dupe::{
    default_candidate_source, pair_key, short_key, SemanticCandidateSource, SemanticLevelStats,
    SuspectedDuplicate, STATE_PENDING_MANUAL, STATE_SUSPECTED,
};
use crate::errors::{EngineError, ErrorCode};
use crate::ingest::{key, SourceRow};
use crate::model::audit::ModelProposal;

/// 控制「模型结论要达到多高的置信度才进可合并候选」的环境变量
pub const MIN_CONFIDENCE_ENV: &str = "REGVAL_MODEL_MIN_CONFIDENCE";

/// 缺省置信度阈值（0.80）
///
/// 为什么不是 0.5：模型给 0.6 的时候它自己都不确定，这种结论进「可合并候选」
/// 等于把「模型不确定」的责任推给经办人 —— 他要逐条看，但看的是模型的犹豫。
/// 0.80 以下一律降级为「疑似待人工判定」，写进契约、等人看，但不进候选。
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.80;

/// 第四级的配置（置信度阈值）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticMergeConfig {
    /// 进可合并候选所需的最低置信度
    pub min_confidence: f64,
}

impl Default for SemanticMergeConfig {
    fn default() -> SemanticMergeConfig {
        SemanticMergeConfig {
            min_confidence: DEFAULT_MIN_CONFIDENCE,
        }
    }
}

impl SemanticMergeConfig {
    /// 从环境变量读阈值（非法值报明确错误码，不静默兜底）
    ///
    /// 阈值是业务口径的一部分（多高才敢让经办人合并），不是技术参数 ——
    /// 写坏了要报错，而不是悄悄用默认值。
    pub fn from_env() -> Result<SemanticMergeConfig, EngineError> {
        let mut cfg = SemanticMergeConfig::default();
        let Some(raw) = std::env::var(MIN_CONFIDENCE_ENV).ok() else {
            return Ok(cfg);
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(cfg);
        }
        let value: f64 = trimmed.parse().map_err(|_| {
            EngineError::at(
                ErrorCode::ConfigInvalid,
                format!(
                    "环境变量 {} 不是合法的置信度阈值（0.0–1.0 之间的数）：{}",
                    MIN_CONFIDENCE_ENV, trimmed
                ),
                MIN_CONFIDENCE_ENV,
            )
        })?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(EngineError::at(
                ErrorCode::ConfigInvalid,
                format!(
                    "环境变量 {} 超出 0.0–1.0 范围：{}",
                    MIN_CONFIDENCE_ENV, trimmed
                ),
                MIN_CONFIDENCE_ENV,
            ));
        }
        cfg.min_confidence = value;
        Ok(cfg)
    }
}

/// 语义候选来源：模型结论已落地时的第四级。
///
/// 构造时就把「模型说了什么」翻译成候选对与行级标注 ——
/// 之后 [`SemanticCandidateSource`] 的两个方法只是把算好的东西取出来。
#[derive(Debug, Clone, Default)]
pub struct ModelSemanticSource {
    /// 候选对（只标疑似，不合并、不推送）
    pairs: Vec<SuspectedDuplicate>,
    /// 行级标注（行键 → 「疑似重复项」列里写的那句话）
    notes: BTreeMap<String, String>,
    /// 本轮模型端点是不是真的用上了
    connected: bool,
    /// 本级说明（人话：第四级这一轮是什么状态）
    describe_text: String,
    /// 来源侧计数
    stats: SemanticLevelStats,
}

impl ModelSemanticSource {
    /// 用模型给出的结论建一个语义候选来源。
    ///
    /// `proposals` 是**已通过结构校验**的模型结论（校验在模型层做：
    /// 编造行键、字段缺失、置信度越界的响应整次作废，根本到不了这里）。
    /// `connected` 为假时（端点没接或不可用）本级候选为空，
    /// 但状态照实报出来 —— 「没查」与「查了没查到」是两回事。
    pub fn build(
        rows: &[SourceRow],
        proposals: &[ModelProposal],
        cfg: &SemanticMergeConfig,
        connected: bool,
        describe_text: impl Into<String>,
    ) -> ModelSemanticSource {
        let mut source = ModelSemanticSource {
            pairs: Vec::new(),
            notes: BTreeMap::new(),
            connected,
            describe_text: describe_text.into(),
            stats: SemanticLevelStats::default(),
        };
        source.stats.proposals = proposals.len();
        if !connected {
            // 端点没接 / 不可用：候选为空，状态照实报（不假装查过）
            return source;
        }

        // 行键 → 行（用来取可读引用、业务主键、来源表）
        let mut by_key: BTreeMap<String, &SourceRow> = BTreeMap::new();
        for row in rows {
            by_key.insert(key::row_uid(row), row);
        }

        for proposal in proposals {
            let (Some(left), Some(right)) = (
                by_key.get(&proposal.left_row_key),
                by_key.get(&proposal.right_row_key),
            ) else {
                // 模型提到了一行我们找不到的行 —— 跳过并计数，不静默丢
                source.stats.skipped_unknown_rows += 1;
                continue;
            };
            // 左小右大：与其他三级同一套顺序，同一对行的键才一致
            let (left, right) = if key::row_uid(left) <= key::row_uid(right) {
                (left, right)
            } else {
                (right, left)
            };
            let left_key = key::row_uid(left);
            let right_key = key::row_uid(right);
            if left_key == right_key {
                source.stats.skipped_unknown_rows += 1;
                continue;
            }

            let high_confidence = proposal.confidence >= cfg.min_confidence;
            let state = if high_confidence {
                STATE_SUSPECTED.to_string()
            } else {
                STATE_PENDING_MANUAL.to_string()
            };
            let basis = if high_confidence {
                format!(
                    "模型判语义等价（{} · 置信度 {:.2}）：{}",
                    proposal.kind, proposal.confidence, proposal.reason
                )
            } else {
                format!(
                    "模型判疑似（{} · 置信度 {:.2}，低于阈值 {:.2}）：{}",
                    proposal.kind, proposal.confidence, cfg.min_confidence, proposal.reason
                )
            };

            source.pairs.push(SuspectedDuplicate {
                left_row_key: left_key.clone(),
                right_row_key: right_key.clone(),
                match_level: crate::dupe::SEMANTIC_MATCH_LEVEL.to_string(),
                basis,
                business_key: key::business_key(left),
                left_reference: reference(left),
                right_reference: reference(right),
                cross_table: left.source_ns != right.source_ns,
                state: state.clone(),
                left_original: format!(
                    "指标名称={}",
                    text(left, crate::ingest::Field::IndicatorName)
                ),
                right_original: format!(
                    "指标名称={}",
                    text(right, crate::ingest::Field::IndicatorName)
                ),
                left_normalized: String::new(),
                right_normalized: String::new(),
                normalize_detail: String::new(),
                distance: None,
                distance_detail: String::new(),
                // 模型语义级：候选来源是语义
                source: crate::model::audit::CandidateSource::Semantic
                    .as_str()
                    .to_string(),
                semantic_kind: proposal.kind.clone(),
                semantic_confidence: Some(proposal.confidence),
                semantic_reason: proposal.reason.clone(),
                semantic_evidence: proposal.evidence.clone(),
                mergeable: high_confidence,
            });

            // 「疑似重复项」列：写清和谁像、对方的行键、以及模型的把握程度
            let left_note = if high_confidence {
                format!(
                    "疑似重复：与 {}（行键 {}）模型判语义等价（{} · 置信度 {:.2}）",
                    reference(right),
                    short_key(&right_key),
                    proposal.kind,
                    proposal.confidence
                )
            } else {
                format!(
                    "疑似待人工判定：与 {}（行键 {}）模型置信度 {:.2}，低于阈值 {:.2}",
                    reference(right),
                    short_key(&right_key),
                    proposal.confidence,
                    cfg.min_confidence
                )
            };
            let right_note = if high_confidence {
                format!(
                    "疑似重复：与 {}（行键 {}）模型判语义等价（{} · 置信度 {:.2}）",
                    reference(left),
                    short_key(&left_key),
                    proposal.kind,
                    proposal.confidence
                )
            } else {
                format!(
                    "疑似待人工判定：与 {}（行键 {}）模型置信度 {:.2}，低于阈值 {:.2}",
                    reference(left),
                    short_key(&left_key),
                    proposal.confidence,
                    cfg.min_confidence
                )
            };
            source.notes.insert(left_key.clone(), left_note);
            source.notes.insert(right_key.clone(), right_note);
        }

        source
    }

    /// 候选对的规范键集合（供去重与断言用）
    pub fn pair_keys(&self) -> Vec<String> {
        self.pairs.iter().map(pair_key).collect()
    }

    /// 本轮模型端点是不是真的用上了（false = 未接入或不可用）
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 本级说明（人话：第四级这一轮是什么状态）
    pub fn describe(&self) -> String {
        self.describe_text.clone()
    }
}

/// 结果契约里的语义归并段（第四级这一轮是什么状态）。
///
/// 与第三级（编辑距离）的 `dupe_distance` 段同一形状：参数 + 计数 + 一句话说明。
/// 目的是让读契约的人一眼看出「这一批的疑似项里模型掺和了没有、
/// 掺和的那几条置信度够不够」。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SemanticStatusSection {
    /// 本轮模型端点是不是真的用上了（false = 未接入或不可用）
    pub connected: bool,
    /// 本级说明（人话）
    pub note: String,
    /// 进可合并候选所需的最低置信度
    pub min_confidence: f64,
    /// 逐级计数
    pub stats: SemanticLevelStats,
}

impl SemanticStatusSection {
    /// 用本级实际跑出来的计数拼一段状态（计数由漏斗编排层算出，不是来源侧猜的）
    pub fn new(
        connected: bool,
        note: impl Into<String>,
        cfg: &SemanticMergeConfig,
        stats: SemanticLevelStats,
    ) -> SemanticStatusSection {
        SemanticStatusSection {
            connected,
            note: note.into(),
            min_confidence: cfg.min_confidence,
            stats,
        }
    }
}

impl SemanticCandidateSource for ModelSemanticSource {
    fn available(&self) -> bool {
        self.connected
    }

    fn describe(&self) -> String {
        self.describe_text.clone()
    }

    fn candidates(
        &self,
        _rows: &[SourceRow],
        _prior: &[SuspectedDuplicate],
    ) -> Vec<SuspectedDuplicate> {
        self.pairs.clone()
    }

    fn row_notes(&self) -> BTreeMap<String, String> {
        self.notes.clone()
    }

    fn stats(&self) -> SemanticLevelStats {
        self.stats.clone()
    }
}

/// 行级可读引用（与其它三级同一套写法）
fn reference(row: &SourceRow) -> String {
    format!("{} 第 {} 行", row.source_ns, row.row_no)
}

/// 取字段的去空白取值
fn text(row: &SourceRow, field: crate::ingest::Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

/// 确定性来源的缺省值（供调用方在拼装候选来源时保持同一套说法）
pub fn deterministic_source() -> String {
    default_candidate_source()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Field;

    fn row(ns: &str, no: usize, name: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-HR-01".to_string());
        values.insert(Field::IndicatorName, name.to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::DeptName, "院感科".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, format!("BK-{}", no).to_string());
        values.insert(Field::Value, "1".to_string());
        SourceRow {
            source_ns: ns.to_string(),
            source_file: format!("{}.csv", ns),
            row_no: no,
            dup_ordinal: 0,
            values,
        }
    }

    fn proposal(left: &SourceRow, right: &SourceRow, confidence: f64) -> ModelProposal {
        ModelProposal {
            left_row_key: key::row_uid(left),
            right_row_key: key::row_uid(right),
            kind: "same_indicator".to_string(),
            confidence,
            reason: "两处指标名指的都是医院感染例数".to_string(),
            evidence: vec!["左侧含『感染』".to_string(), "右侧含『感染』".to_string()],
        }
    }

    #[test]
    fn 置信度达标进可合并候选_并保留归并类型与依据() {
        let rows = vec![row("表一", 1, "医院感染例数"), row("表二", 2, "院感例数")];
        let proposals = vec![proposal(&rows[0], &rows[1], 0.92)];
        let source = ModelSemanticSource::build(
            &rows,
            &proposals,
            &SemanticMergeConfig::default(),
            true,
            "模型语义归并",
        );
        assert_eq!(source.pairs.len(), 1);
        let pair = &source.pairs[0];
        assert_eq!(pair.match_level, "semantic");
        assert_eq!(pair.source, "semantic");
        assert_eq!(pair.state, STATE_SUSPECTED);
        assert!(pair.mergeable);
        assert_eq!(pair.semantic_kind, "same_indicator");
        assert_eq!(pair.semantic_confidence, Some(0.92));
        assert_eq!(pair.semantic_evidence.len(), 2);
        assert!(pair.basis.contains("模型判语义等价"));
        assert!(pair.cross_table);
    }

    #[test]
    fn 置信度不够降级为待人工判定_不进可合并候选() {
        let rows = vec![row("表一", 1, "医院感染例数"), row("表二", 2, "院感例数")];
        let proposals = vec![proposal(&rows[0], &rows[1], 0.42)];
        let source = ModelSemanticSource::build(
            &rows,
            &proposals,
            &SemanticMergeConfig::default(),
            true,
            "模型语义归并",
        );
        assert_eq!(source.pairs.len(), 1);
        assert_eq!(source.pairs[0].state, STATE_PENDING_MANUAL);
        assert!(!source.pairs[0].mergeable);
        assert!(source.pairs[0].basis.contains("低于阈值"));
        // 行级标注也写明是「待人工判定」，不是「疑似重复」
        let note = source.notes.get(&key::row_uid(&rows[0])).unwrap();
        assert!(note.starts_with("疑似待人工判定"));
    }

    #[test]
    fn 端点不可用时候选为空_状态照实报() {
        let rows = vec![row("表一", 1, "医院感染例数"), row("表二", 2, "院感例数")];
        let proposals = vec![proposal(&rows[0], &rows[1], 0.92)];
        let source = ModelSemanticSource::build(
            &rows,
            &proposals,
            &SemanticMergeConfig::default(),
            false,
            "本轮未接入模型端点",
        );
        assert!(source.pairs.is_empty());
        assert!(!source.available());
        assert_eq!(source.describe(), "本轮未接入模型端点");
        assert_eq!(source.stats.proposals, 1);
    }

    #[test]
    fn 模型编的行键跳过并计数() {
        let rows = vec![row("表一", 1, "医院感染例数"), row("表二", 2, "院感例数")];
        let mut bad = proposal(&rows[0], &rows[1], 0.92);
        bad.right_row_key = "row-ghost-999".to_string();
        let source = ModelSemanticSource::build(
            &rows,
            &[bad],
            &SemanticMergeConfig::default(),
            true,
            "模型语义归并",
        );
        assert!(source.pairs.is_empty());
        assert_eq!(source.stats.skipped_unknown_rows, 1);
    }

    #[test]
    fn 阈值可配_调低后原本降级的结论进候选() {
        let rows = vec![row("表一", 1, "医院感染例数"), row("表二", 2, "院感例数")];
        let proposals = vec![proposal(&rows[0], &rows[1], 0.42)];
        let cfg = SemanticMergeConfig {
            min_confidence: 0.40,
        };
        let source = ModelSemanticSource::build(&rows, &proposals, &cfg, true, "模型语义归并");
        assert!(source.pairs[0].mergeable);
        assert_eq!(source.pairs[0].state, STATE_SUSPECTED);
    }

    #[test]
    fn 阈值非法时报配置错误不静默兜底() {
        std::env::set_var(MIN_CONFIDENCE_ENV, "abc");
        let err = SemanticMergeConfig::from_env().unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        std::env::remove_var(MIN_CONFIDENCE_ENV);

        std::env::set_var(MIN_CONFIDENCE_ENV, "1.7");
        let err = SemanticMergeConfig::from_env().unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        std::env::remove_var(MIN_CONFIDENCE_ENV);
    }
}
