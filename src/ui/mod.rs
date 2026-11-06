//! 界面层：表格内插件的视觉 token。
//!
//! 本产品的界面落在 **Excel / WPS 原生能力**上（单元格填充色、字体、列宽、
//! 边框），不自绘控件 —— 科室在自己的表格里干活，界面不能抢戏。
//!
//! 样式不写在宏里：token 的唯一出处是 [`tokens`]，宏侧的常量模块由引擎生成
//! （`regval tokens --vba --out macro/vba/RegValTokens.bas`），宏里只引用
//! `T_*` 常量与 `RegVal*ForRole` 函数。
//!
//! 文案同理不写在宏里：提示文案的唯一出处是 [`messages`]，宏侧的常量模块由
//! 引擎生成（`regval messages --vba --out macro/vba/RegValMessages.bas`），
//! 宏里只引用 `REGVAL_MSG_*` 常量与 `RegValErrorText` 函数。
//!
//! 状态条上那几句话（加载 / 空结果 / 错误 / 校验完成）在 [`statusbar`]。
//!
//! WPS JSA 侧（表格内插件的第三个入口）由 [`wps`] 装配：手写模板 + 引擎侧的
//! 契约常量、token、文案 → 一份可独立加载的宏文件。

pub mod messages;
pub mod statusbar;
pub mod tokens;
pub mod wps;
