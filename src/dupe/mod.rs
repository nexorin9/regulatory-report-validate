//! 重复检测四级漏斗：判断「这几行是不是同一件事」。
//!
//! # 为什么分四级
//!
//! 同一份上报表里，「同一件事被填了两遍」有四种不同的写法。判据从严到松，
//! 逐级放开，每一级都只多认一点、但绝不认错：
//!
//! | 级别 | 判据 | 典型现场 | 实现 |
//! |---|---|---|---|
//! | **L1 确定性键** | 键完全相同 + 取值完全一致 | 同一行被复制粘贴了两次 | [`exact`]（本轮） |
//! | L2 归一化 | 归一后（空白 / 全半角 / 单位别名 / 名称别名）一致 | 「例数」与「例」 | 任务 21 |
//! | L3 编辑距离 | 字符级近邻 | 「病案室」与「病案统计室」 | 任务 22 |
//! | L4 语义归并 | 模型判语义等价 | 「CT 检查」与「计算机断层检查」 | 任务 26 |
//!
//! # 漏斗的铁律：只标「疑似」，永不合并、永不推送
//!
//! 每一级产出的都是**候选对**，不是结论。合并与否由经办人逐条勾选确认
//! （确认流见任务 36）。引擎不替业务做决定 —— 同一段活如果引擎替人定了，
//! 出了错连是谁填错都查不出来。
//!
//! # 取值签名只有一份
//!
//! 「两行取值是否完全一致」这个判据，**实体绑定层与漏斗共用 [`value_signature`]**。
//! 两处各写一份的后果很具体：同一对行，绑定层说「疑似重复」、漏斗说「待人工判定」，
//! 同一张表里出现两套说法，经办人不知道该信哪个。
//!
//! 签名里单位按别名表归一后比较（「％」与「%」、「例数」与「例」是同一个单位），
//! 数值按数值比（「12」与「12.0」是同一个数），比不了数的按原文比
//! （空串与「abc」不是一回事）。

pub mod candidates;
pub mod distance;
pub mod eval;
pub mod exact;
pub mod merge;
pub mod normalize;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::ingest::{Field, SourceRow};
use crate::rules::normalize::AliasTable;

/// 单个取值桶超过此行数时改为「星形」成对（首行与其余各行），折叠掉的对数另行计数。
///
/// 为什么要折：一行被复制粘贴 5000 次是完全可能的（整表粘贴漏了筛选），
/// 逐对展开是 1200 多万对 —— 机器算得动，经办人看不完，也写不进结果契约。
/// 折成星形后每一行仍然都能看到「自己和谁重复了」，只是不再两两配对。
pub const PAIR_EXPANSION_MAX: usize = 8;

/// 「疑似重复项」列的前缀（各层级共用一句前缀，合并时只留一次）
pub const SUSPECTED_PREFIX: &str = "疑似重复：";

/// 疑似重复对：漏斗每一级都产出这种结构。
///
/// 一对 = 两行「疑似是同一件事」。`left_row_key` / `right_row_key` 是行键
/// （定位到一行的稳定标识），`match_level` 是命中哪一级，`basis` 是判定依据。
///
/// 不派生 `Eq`：语义级带模型给的置信度（`f64`），浮点数没有全序等价关系。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuspectedDuplicate {
    /// 左侧行键
    pub left_row_key: String,
    /// 右侧行键
    pub right_row_key: String,
    /// 命中层级：`exact` / `normalize` / `distance` / `semantic`
    pub match_level: String,
    /// 判定依据（为什么判成疑似重复）
    pub basis: String,
    /// 参与比对的确定性键（业务主键：指标编号｜科室代码｜报告期｜业务主键）
    pub business_key: String,
    /// 左侧行的可读引用（"表名 第 N 行"）
    pub left_reference: String,
    /// 右侧行的可读引用
    pub right_reference: String,
    /// 是否跨来源表（同一业务记录在几张科室表里各填了一次）
    pub cross_table: bool,
    /// 判定状态：恒为「疑似重复」—— 漏斗只标，不合并不推送
    pub state: String,
    /// 左侧行的原值摘要（举证用；确定性键那一级不填）
    #[serde(default)]
    pub left_original: String,
    /// 右侧行的原值摘要（举证用）
    #[serde(default)]
    pub right_original: String,
    /// 左侧行的归一后取值摘要（举证用）
    #[serde(default)]
    pub left_normalized: String,
    /// 右侧行的归一后取值摘要（举证用）
    #[serde(default)]
    pub right_normalized: String,
    /// 归一详情：写法差异是怎么被吸收的（哪一项归一后一致、原文分别是什么）
    #[serde(default)]
    pub normalize_detail: String,
    /// 字符级编辑距离（命中编辑距离那一级时有值；前两级不填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<usize>,
    /// 编辑距离的差异说明（哪几个字不同）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub distance_detail: String,
    /// 候选来源等级（`deterministic` / `semantic`）：这一对行是哪一级判出来的。
    ///
    /// 缺省为 `deterministic` —— 确定性三级（精确键 / 归一化 / 编辑距离）判出来的
    /// 对一律标确定性；只有模型语义级判出来的才标 `semantic`。
    /// 模型不可用时降级跑完，这一轮的对全是 `deterministic` ——
    /// 读的人一眼就知道「这批疑似项里模型没掺和」。
    #[serde(default = "default_candidate_source")]
    pub source: String,
    /// 语义归并类型（`same_indicator` / `duplicate_record`）；非语义级为空串
    #[serde(default)]
    pub semantic_kind: String,
    /// 模型给的置信度（0.0–1.0）；非语义级为空
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_confidence: Option<f64>,
    /// 模型给的理由（中文）；非语义级为空串
    #[serde(default)]
    pub semantic_reason: String,
    /// 模型给的可核对依据；非语义级为空数组
    #[serde(default)]
    pub semantic_evidence: Vec<String>,
    /// 是否可进入人工确认的**可合并候选**。
    ///
    /// 语义级里只有置信度达标的才是 `true`；置信度不够的降级为「疑似待人工判定」，
    /// 不进可合并候选 —— 但**照样写进结果契约**，让人看得见模型提了什么、
    /// 为什么没采纳。确定性三级判出来的对默认就是可合并候选。
    #[serde(default = "default_mergeable")]
    pub mergeable: bool,
}

/// 候选来源等级的缺省值（确定性三级）
pub fn default_candidate_source() -> String {
    "deterministic".to_string()
}

/// 「是否可合并」的缺省值：确定性三级判出来的对默认可进人工确认
pub fn default_mergeable() -> bool {
    true
}

/// 取值签名：判定「两行取值是否完全一致」用。
///
/// **实体绑定层与重复检测漏斗共用这一份**（见本模块顶部说明）。
///
/// - 数值列按数值比：`12` 与 `12.0` 是同一个数；
/// - 比不了数的按原文比：空串与 `abc` 是两回事，不该当成一致；
/// - 单位按别名表归一后比：`％` 与 `%`、`例数` 与 `例` 是同一个单位。
pub fn value_signature(row: &SourceRow) -> String {
    let parts = vec![
        text(row, Field::OrgCode),
        text(row, Field::IndicatorCode),
        text(row, Field::IndicatorName),
        text(row, Field::DeptCode),
        text(row, Field::DeptName),
        text(row, Field::ReportingPeriod),
        text(row, Field::BusinessKey),
        numeric_signature(row, Field::Numerator),
        numeric_signature(row, Field::Denominator),
        numeric_signature(row, Field::Value),
        unit_signature(row),
    ];
    sha256_hex(parts.join("\u{1}").as_bytes())
}

/// 数值部分签名：只看 分子 / 分母 / 填报值 三个数，不看文字。
///
/// 重复检测第二级（归一化候选）用它判断「两行的数是不是同一个数」——
/// 写法不同但数一样，才是「书写差异」；数不一样，那是取值冲突，
/// 归实体绑定层的「待人工判定」，本级不碰。
///
/// 与 [`value_signature`] 同源（同一套数值归一：能算成数就按数算，
/// 空串与非数值原文分开记）。
pub fn values_signature(row: &SourceRow) -> String {
    let parts = vec![
        numeric_signature(row, Field::Numerator),
        numeric_signature(row, Field::Denominator),
        numeric_signature(row, Field::Value),
    ];
    sha256_hex(parts.join("\u{1}").as_bytes())
}

/// 内置单位别名表（只建一次）
fn alias_table() -> &'static AliasTable {
    static TABLE: OnceLock<AliasTable> = OnceLock::new();
    TABLE.get_or_init(AliasTable::builtin)
}

/// 单位签名：归一到规范单位后取。
///
/// 归一表来自口径层（[`crate::rules::normalize`]）—— 单位检查器与漏斗用同一张表，
/// 运维改口径不用改代码。
pub fn unit_signature(row: &SourceRow) -> String {
    alias_table().canonicalize(&text(row, Field::Unit))
}

/// 数值列的签名取值：能算成数就按数算，算不成按原文（区分「空」与「非数值」）。
fn numeric_signature(row: &SourceRow, field: Field) -> String {
    let raw = row.get_trimmed(field).unwrap_or("");
    if raw.is_empty() {
        return "∅".to_string();
    }
    match row.number(field) {
        Some(v) => format!("#{}", v),
        None => format!("?{}", raw),
    }
}

/// 取值（去首尾空白；空串算空）
fn text(row: &SourceRow, field: Field) -> String {
    row.get_trimmed(field).unwrap_or("").to_string()
}

/// 行键短码：写进「疑似重复项」列给人看的前 8 位（完整行键在结果契约里）
pub fn short_key(row_key: &str) -> String {
    row_key.chars().take(8).collect()
}

/// 合并「疑似重复项」列的多条标注。
///
/// 不同层级各写一条（确定性键说「取值完全一致」、归一化说「写法不同但归一后一致」），
/// 列里只留一个格子 —— 两条都写进去，读的人会以为是两件不同的事。
/// 合并时前缀只留一次：句式还是一句话，后半句用「；」接上。
pub fn merge_suspected_text(existing: &str, addition: &str) -> String {
    if existing.is_empty() {
        return addition.to_string();
    }
    if addition.is_empty() {
        return existing.to_string();
    }
    match (
        existing.strip_prefix(SUSPECTED_PREFIX),
        addition.strip_prefix(SUSPECTED_PREFIX),
    ) {
        (Some(head), Some(tail)) => format!("{}{}{}{}", SUSPECTED_PREFIX, head, "；", tail),
        _ => format!("{}；{}", existing, addition),
    }
}

fn sha256_hex(input: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

/// 疑似重复的判定状态（漏斗恒用这一个取值）
pub const STATE_SUSPECTED: &str = "疑似重复";

/// 语义级置信度不够时的判定状态：疑似，但**不进可合并候选**。
///
/// 为什么单列一个状态：模型给了个 0.4 的结论，若与 0.95 的结论用同一个状态，
/// 经办人分不出「模型很有把握」和「模型自己都不确定」。分开了，就得逐条看。
pub const STATE_PENDING_MANUAL: &str = "疑似待人工判定";

// ============================================================================
// 漏斗编排：四级依次跑，逐级累加
// ============================================================================
//
// # 为什么要有编排层
//
// 三个检测器各管一级、各自带一套计数。缺了编排层，「这一轮到底报了多少疑似重复」
// 得靠人把三份计数加起来 —— 而「加起来」这一步一旦各算各的，就会出两种事：
// 同一对行被两级重复报出去，或者某一级把上一级已经认下的候选弄丢。
//
// 编排层只做三件事，每件都对应一个具体后果：
//
// 1. **逐级累加**：后一级只增不删，候选集合单调不减（[`FunnelOutcome::cumulative`]）；
// 2. **来源标记**：每一对行记住是哪一级判出来的（`match_level`）；同一对行被两级
//    同时认下时，**只留最先认下的那一级** —— 证据最硬的那一级说了算；
// 3. **逐级可数**：每一级新增多少、累计多少都单独计数 —— 这组数字是评估漏斗的
//    原料（见 [`eval`]：逐级召回率与精确率）。
//
// # 第四级为什么留成接口
//
// 第四级是语义归并（模型级）。模型调用在后面的任务里落地，落地之前这一级
// **必须存在但候选为空**，而不是被悄悄删掉 —— 「这一级没接」和「这一级查了、
// 没查到」是两回事，前者要写在结论里，后者才是漏斗的成绩。
// 语义候选的入口是 [`SemanticCandidateSource`]：本轮用 [`DisconnectedSemanticSource`]
// （照实报「未接入」），模型端点落地后换成真实现，漏斗编排一行不用改。

/// 第四级（语义归并）的命中层级取值
pub const SEMANTIC_MATCH_LEVEL: &str = "semantic";

/// 漏斗层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FunnelLevel {
    /// L1 确定性键：键与取值完全一致
    Exact,
    /// L2 归一化：写法不同但归一后一致
    Normalize,
    /// L3 编辑距离：归一吸收不掉的近似写法
    Distance,
    /// L4 语义归并：模型判语义等价
    Semantic,
}

impl FunnelLevel {
    /// 层级短名（与结果契约里 `match_level` 的取值一致）
    pub fn match_level(self) -> &'static str {
        match self {
            FunnelLevel::Exact => exact::MATCH_LEVEL_EXACT,
            FunnelLevel::Normalize => normalize::MATCH_LEVEL_NORMALIZE,
            FunnelLevel::Distance => distance::MATCH_LEVEL_DISTANCE,
            FunnelLevel::Semantic => SEMANTIC_MATCH_LEVEL,
        }
    }

    /// 层级中文名（给人看：对比表与结论里都用它）
    pub fn title(self) -> &'static str {
        match self {
            FunnelLevel::Exact => "L1 确定性键",
            FunnelLevel::Normalize => "L2 归一化",
            FunnelLevel::Distance => "L3 编辑距离",
            FunnelLevel::Semantic => "L4 语义归并",
        }
    }

    /// 这一级判定的依据（人话，给结论段落引用）
    pub fn basis_hint(self) -> &'static str {
        match self {
            FunnelLevel::Exact => "键与取值完全一致",
            FunnelLevel::Normalize => "写法差异经归一后一致",
            FunnelLevel::Distance => "归一吸收不掉的差异按字符级编辑距离判",
            FunnelLevel::Semantic => "模型判语义等价",
        }
    }

    /// 全部层级（顺序即漏斗顺序）
    pub const ALL: [FunnelLevel; 4] = [
        FunnelLevel::Exact,
        FunnelLevel::Normalize,
        FunnelLevel::Distance,
        FunnelLevel::Semantic,
    ];
}

/// 第四级（语义归并）的候选来源：模型级归并的入口。
///
/// 业务层只依赖这个接口，不依赖任何厂商 SDK —— 模型端点怎么连是模型层的事，
/// 漏斗只关心「这一级给了哪些候选对」。
pub trait SemanticCandidateSource {
    /// 本轮有没有可用的语义候选来源（没接模型端点时为否）
    fn available(&self) -> bool;
    /// 来源说明（人话：这轮第四级是什么状态）
    fn describe(&self) -> String;
    /// 产出语义候选对（只标疑似，不合并、不推送）
    fn candidates(
        &self,
        rows: &[SourceRow],
        prior: &[SuspectedDuplicate],
    ) -> Vec<SuspectedDuplicate>;
    /// 本级在「疑似重复项」列的行级标注（行键 → 给人看的那句话）。
    ///
    /// 默认没有 —— 语义级的标注由 [`merge`] 那一层给出（它知道模型说了什么）。
    fn row_notes(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    /// 来源侧自己知道的计数（模型提了几条、有几条的行键对不上）。
    ///
    /// 默认全零 —— 没接模型端点时这一级本来就没有候选。
    fn stats(&self) -> SemanticLevelStats {
        SemanticLevelStats::default()
    }
}

/// 未接入模型端点时的第四级：候选为空，但状态照实报出来。
///
/// **不静默**：这一级在对比表里占一行，注明「未接入」，免得读的人以为
/// 「查过了、没查到」。
#[derive(Debug, Clone, Copy, Default)]
pub struct DisconnectedSemanticSource;

impl SemanticCandidateSource for DisconnectedSemanticSource {
    fn available(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "本轮未接入模型端点（语义归并的模型调用尚未落地），第四级候选为空".to_string()
    }

    fn candidates(
        &self,
        _rows: &[SourceRow],
        _prior: &[SuspectedDuplicate],
    ) -> Vec<SuspectedDuplicate> {
        Vec::new()
    }
}

/// 一级的结果
#[derive(Debug, Clone)]
pub struct FunnelLevelResult {
    /// 层级
    pub level: FunnelLevel,
    /// 本级**新增**的候选对（不含上级已认下的）
    pub new_pairs: Vec<SuspectedDuplicate>,
    /// 行键 → 本级的行级标注
    pub suspected: BTreeMap<String, String>,
    /// 本级说明（这一轮这一级是什么状态）
    pub note: String,
}

impl FunnelLevelResult {
    /// 本级新增候选对数
    pub fn new_pair_count(&self) -> usize {
        self.new_pairs.len()
    }
}

/// 漏斗各级的运行计数（逐级可观测：结果契约与日志里都能看到）
#[derive(Debug, Clone, Default)]
pub struct FunnelStats {
    /// L1 计数
    pub exact: exact::ExactStats,
    /// L2 计数
    pub normalize: normalize::NormalizeStats,
    /// L3 计数
    pub distance: distance::DistanceStats,
    /// L3 的距离分布直方
    pub distance_histogram: Vec<distance::DistanceBucket>,
}

/// 漏斗跑完一次的产出
#[derive(Debug, Clone, Default)]
pub struct FunnelOutcome {
    /// 逐级结果（顺序即层级顺序）
    pub levels: Vec<FunnelLevelResult>,
    /// 逐级累加的候选对（单调不减：后一级只增不删）
    pub cumulative: Vec<SuspectedDuplicate>,
    /// 行键 → 「疑似重复项」列取值（各级的标注合并成一句）
    pub suspected: BTreeMap<String, String>,
    /// 逐级运行计数
    pub stats: FunnelStats,
}

impl FunnelOutcome {
    /// 累加到指定层级为止的候选对数（该级及之前所有级的新增之和）
    pub fn cumulative_len_at(&self, level: FunnelLevel) -> usize {
        let mut total = 0usize;
        for l in &self.levels {
            total += l.new_pair_count();
            if l.level == level {
                return total;
            }
        }
        total
    }

    /// 某一级的新增候选对数
    pub fn new_pairs_at(&self, level: FunnelLevel) -> usize {
        self.levels
            .iter()
            .find(|l| l.level == level)
            .map(|l| l.new_pair_count())
            .unwrap_or(0)
    }
}

/// 候选对的规范键：两行行键排序后拼接。
///
/// 与产出的 `left` / `right` 顺序一致（左小右大），所以同一对行不论被哪一级
/// 判出来，键都一样 —— 漏斗据此去重。
pub fn pair_key(pair: &SuspectedDuplicate) -> String {
    if pair.left_row_key <= pair.right_row_key {
        format!("{}|{}", pair.left_row_key, pair.right_row_key)
    } else {
        format!("{}|{}", pair.right_row_key, pair.left_row_key)
    }
}

/// 跑一遍重复检测四级漏斗。
///
/// 顺序固定：**确定性键 → 归一化 → 编辑距离 → 语义归并**。
///
/// 每一级只把自己**新认下**的候选对并进累加集合（已被上级认下的不再重复报），
/// 因此累加集合单调不减。语义级用传入的 [`SemanticCandidateSource`] 取候选 ——
/// 基准集评估（`regval bench`）传 [`DisconnectedSemanticSource`]（未接入模型端点，
/// 候选为空）；校验主路径传 [`merge::ModelSemanticSource`]（模型结论已落地的）。
pub fn run_funnel(
    rows: &[SourceRow],
    distance_cfg: &distance::DistanceConfig,
    semantic: &dyn SemanticCandidateSource,
) -> FunnelOutcome {
    let mut outcome = run_deterministic_levels(rows, distance_cfg);
    push_semantic_level(&mut outcome, rows, semantic);
    outcome
}

/// 跑确定性三级（键 → 归一化 → 编辑距离）。
///
/// 为什么单独拆一个函数：主路径里第四级要**等模型调用完**才有候选 ——
/// 先跑确定性三级拿到「前三级已认下哪些对」，才知道该拿哪几对去问模型
/// （已被认下的证据更硬，不该拿去问）。跑完模型再把第四级并进来。
pub fn run_deterministic_levels(
    rows: &[SourceRow],
    distance_cfg: &distance::DistanceConfig,
) -> FunnelOutcome {
    let exact = exact::detect(rows);
    let normalized = normalize::detect(rows);
    let distanced = distance::detect(rows, distance_cfg);

    let mut outcome = FunnelOutcome {
        stats: FunnelStats {
            exact: exact.stats.clone(),
            normalize: normalized.stats.clone(),
            distance: distanced.stats.clone(),
            distance_histogram: distanced.histogram.clone(),
        },
        ..FunnelOutcome::default()
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();

    push_level(
        &mut outcome,
        &mut seen,
        FunnelLevel::Exact,
        exact.pairs,
        exact.suspected,
        FunnelLevel::Exact.basis_hint().to_string(),
    );
    push_level(
        &mut outcome,
        &mut seen,
        FunnelLevel::Normalize,
        normalized.pairs,
        normalized.suspected,
        FunnelLevel::Normalize.basis_hint().to_string(),
    );
    push_level(
        &mut outcome,
        &mut seen,
        FunnelLevel::Distance,
        distanced.pairs,
        distanced.suspected,
        format!(
            "{}（阈值 {} · 来源：{}）",
            FunnelLevel::Distance.basis_hint(),
            distance_cfg.threshold,
            distance_cfg.source
        ),
    );

    outcome
}

/// 第四级（语义归并）的运行计数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticLevelStats {
    /// 模型提了几条结论
    pub proposals: usize,
    /// 置信度达标、可进人工确认的可合并候选（对）
    pub mergeable: usize,
    /// 置信度不够、降级为「疑似待人工判定」（对）
    pub pending_manual: usize,
    /// 前三级已认下、不重复报的对
    pub skipped_claimed: usize,
    /// 模型提到、但本轮找不到对应行的对（防模型编行键的第二道闸）
    pub skipped_unknown_rows: usize,
}

impl SemanticLevelStats {
    /// 本级新增候选对数（可合并 + 待人工判定）
    pub fn pair_count(&self) -> usize {
        self.mergeable + self.pending_manual
    }

    /// 一行中文摘要（终端输出与结果契约里用）
    pub fn summary_line(&self) -> String {
        format!(
            "语义归并：模型提出 {} 条 · 可合并候选 {} 对 · 置信度不足待人工判定 {} 对",
            self.proposals, self.mergeable, self.pending_manual
        )
    }
}

/// 把第四级（语义归并）的结果并进漏斗。
///
/// **只增不删**：前三级已认下的对，语义级不重复报（证据更硬的那一级说了算）。
/// **只标不推**：本级产出的候选对只是标注 —— 合并与否由经办人逐条勾选确认
/// （确认记录见 `confirmations` 表），模型永远不推进任何业务状态。
pub fn push_semantic_level(
    outcome: &mut FunnelOutcome,
    rows: &[SourceRow],
    semantic: &dyn SemanticCandidateSource,
) -> SemanticLevelStats {
    // 来源侧自己知道的计数（模型提了几条、有几条的行键对不上）
    let mut stats = semantic.stats();
    let prior = outcome.cumulative.clone();
    let prior_keys: BTreeSet<String> = prior.iter().map(pair_key).collect();
    let semantic_pairs = semantic.candidates(rows, &prior);
    let semantic_note = semantic.describe();
    let notes = semantic.row_notes();

    // 已被前三级认下的对，语义级不重复报（证据更硬的那一级说了算）
    let mut fresh: Vec<SuspectedDuplicate> = Vec::new();
    for pair in semantic_pairs {
        if prior_keys.contains(&pair_key(&pair)) {
            stats.skipped_claimed += 1;
            continue;
        }
        if pair.mergeable {
            stats.mergeable += 1;
        }
        if pair.state == STATE_PENDING_MANUAL {
            stats.pending_manual += 1;
        }
        fresh.push(pair);
    }

    let mut seen = prior_keys;
    push_level(
        outcome,
        &mut seen,
        FunnelLevel::Semantic,
        fresh,
        notes,
        semantic_note,
    );
    stats
}

/// 把一级的结果并进累加集合：只留新认下的对，并把行级标注合并进总标注
fn push_level(
    outcome: &mut FunnelOutcome,
    seen: &mut BTreeSet<String>,
    level: FunnelLevel,
    pairs: Vec<SuspectedDuplicate>,
    suspected: BTreeMap<String, String>,
    note: String,
) {
    let mut fresh = Vec::new();
    for pair in pairs {
        let pk = pair_key(&pair);
        // 已被上一级认下 → 不重复报（证据最硬的那一级说了算）
        if seen.insert(pk) {
            fresh.push(pair);
        }
    }
    // 逐级累加：只增不删
    outcome.cumulative.extend(fresh.iter().cloned());
    // 行级标注合并：同一格里只留一句（各级的说法用「；」接上）
    for (row_key, text) in &suspected {
        match outcome.suspected.get_mut(row_key) {
            Some(current) => *current = merge_suspected_text(current, text),
            None => {
                outcome.suspected.insert(row_key.clone(), text.clone());
            }
        }
    }
    outcome.levels.push(FunnelLevelResult {
        level,
        new_pairs: fresh,
        suspected,
        note,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(unit: &str, value: &str) -> SourceRow {
        let mut values = BTreeMap::new();
        values.insert(Field::OrgCode, "ORG-001".to_string());
        values.insert(Field::IndicatorCode, "MER-TL-04".to_string());
        values.insert(Field::IndicatorName, "病案首页24小时内完成率".to_string());
        values.insert(Field::DeptCode, "0301".to_string());
        values.insert(Field::DeptName, "病案统计室".to_string());
        values.insert(Field::ReportingPeriod, "2026-Q3".to_string());
        values.insert(Field::BusinessKey, "BK-01".to_string());
        values.insert(Field::Numerator, "980".to_string());
        values.insert(Field::Denominator, "1000".to_string());
        values.insert(Field::Value, value.to_string());
        values.insert(Field::Unit, unit.to_string());
        SourceRow {
            source_ns: "科室一表".to_string(),
            source_file: "科室一表.csv".to_string(),
            row_no: 1,
            dup_ordinal: 0,
            values,
        }
    }

    #[test]
    fn 数值写法不同但数值相同签名一致() {
        assert_eq!(
            value_signature(&row("%", "98")),
            value_signature(&row("%", "98.0"))
        );
    }

    #[test]
    fn 单位写法不同经归一后签名一致() {
        assert_eq!(
            value_signature(&row("%", "98")),
            value_signature(&row("％", "98")),
            "全角百分号与半角百分号是同一个单位"
        );
        assert_eq!(
            value_signature(&row("例数", "98")),
            value_signature(&row("例", "98")),
            "例数与例是同一个单位"
        );
    }

    #[test]
    fn 空值与非数值原文不是一回事() {
        let mut empty = row("%", "98");
        empty.values.insert(Field::Value, String::new());
        let mut text_value = row("%", "98");
        text_value.values.insert(Field::Value, "待补".to_string());
        assert_ne!(value_signature(&empty), value_signature(&text_value));
    }

    #[test]
    fn 指标名称不同签名不同() {
        let mut other = row("%", "98");
        other
            .values
            .insert(Field::IndicatorName, "病案首页3日内完成率".to_string());
        assert_ne!(value_signature(&row("%", "98")), value_signature(&other));
    }

    #[test]
    fn 短行键取前八位() {
        assert_eq!(short_key("0123456789abcdef"), "01234567");
    }
}
