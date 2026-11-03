//! 选区工具：不画东西，只操作选区（拖动平移）。
//!
//! 有了它，「刚框完就拖动」= 平移选区，而不是误画一根箭头；
//! 想标注时点「箭头 / 文字」即可。缩放在任何工具下都能用（拖边框/角）。
//!
//! 实现上它是个"空工具"：`begin` / `click` 都返回 None，主程序看到
//! `is_selection_tool() == true` 就把画布上的拖动交给选区平移逻辑。

use egui::Key;

use super::Tool;

pub struct SelectTool;

impl Tool for SelectTool {
    fn id(&self) -> &'static str {
        "select"
    }

    fn label(&self) -> &'static str {
        "选区"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::V)
    }

    fn order(&self) -> i32 {
        5
    }

    fn is_selection_tool(&self) -> bool {
        true
    }

    fn hint(&self) -> &'static str {
        "拖动 = 移动选区"
    }
}
