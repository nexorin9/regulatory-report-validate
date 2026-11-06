//! 口径规则引擎。
//!
//! 分五层：
//! - [`model`]：规则与问题的数据结构（规则是**数据**：SQL 是数据，取值走命名参数）；
//! - [`checks`]：四类检查器（把口径编译成规则；见 [`checks::required`]、[`checks::enum_`]、
//!   [`checks::unit`]）；
//! - [`indicators`]：官方 27 项指标口径登记表（指标类别与在册编号的单一事实来源）；
//! - [`seed`]：官方 27 项指标口径种子（同一份口径的文件形态，编译成四类检查器的规则）；
//! - [`normalize`]：口径归一化层（空白字符集与计量单位别名表，判定用归一后的值）；
//! - [`syntax`]：规则 SQL 的语法预校验与出错定位（装载即预校验，语法错不生效）；
//! - [`loader`]：版本化规则表装载（从 `REGVAL_RULE_DIR` 装载、版本归并、规则清单）；
//! - [`version`]：换版闸门（生效日期 + 质控岗确认）与口径差异标注；
//! - [`executor`]：参数化执行器（规则以绑定参数执行，不拼字符串）。

pub mod checks;
pub mod executor;
pub mod indicators;
pub mod loader;
pub mod model;
pub mod normalize;
pub mod seed;
pub mod syntax;
pub mod version;
