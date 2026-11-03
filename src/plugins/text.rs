//! 文字插件：点一下出现输入框，敲完回车落定。
//!
//! 这是「点击型 + 内联编辑器 + 属性面板」工具的样板。两个关键点：
//!   * **必须挂一个真正的 egui 控件**（`TextEdit`）并让它拿到焦点，否则 fcitx5 的
//!     `text-input-v3` 不会激活，中文打不进去 —— 这是整个技术验证里唯一有风险的一条；
//!   * 字号/颜色存在工具自己身上，落定时快照进标注对象，之后改属性不影响已画的标注。

use egui::{Align2, Color32, FontId, Key, Painter, Pos2, Rect, Ui, Vec2};

use super::{common, InlineEditor, Settings, Shape, Tool, ACCENT};

#[derive(Clone, Debug)]
pub struct Text {
    pub pos: Pos2,
    pub text: String,
    pub size: f32,
    pub color: Color32,
}

impl Shape for Text {
    fn draw(&self, painter: &Painter) {
        if self.text.is_empty() {
            return;
        }
        painter.text(
            self.pos,
            Align2::LEFT_TOP,
            &self.text,
            FontId::proportional(self.size),
            self.color,
        );
    }

    fn bounds(&self) -> Rect {
        // 粗略估算：等宽假设够用了，只用于脏区/布局
        let w = (self.text.chars().count() as f32) * self.size * 0.62;
        Rect::from_min_size(self.pos, Vec2::new(w.max(self.size), self.size * 1.4))
    }

    fn set_bounds(&mut self, rect: Rect) {
        // 文字按高度等比缩放字号（bounds 高 ≈ size * 1.4）
        self.pos = rect.min;
        self.size = (rect.height() / 1.4).clamp(8.0, 400.0);
    }

    fn clone_box(&self) -> Box<dyn Shape> {
        Box::new(self.clone())
    }
    fn translate(&mut self, delta: egui::Vec2) {
        self.pos += delta;
    }
}

/// 编辑中的文字：持有一份缓冲，提交时才变成 [`Text`]
pub struct TextEditor {
    pos: Pos2,
    buf: String,
    size: f32,
    color: Color32,
    /// 首帧强制抢焦点：不抢的话 IME 不会挂上来
    first_frame: bool,
}

impl TextEditor {
    pub fn new(pos: Pos2, size: f32, color: Color32) -> Self {
        Self {
            pos,
            buf: String::new(),
            size,
            color,
            first_frame: true,
        }
    }
}

impl InlineEditor for TextEditor {
    fn ui(&mut self, ctx: &egui::Context) {
        let mut buf = std::mem::take(&mut self.buf);
        let mut wants_focus = false;
        egui::Area::new(egui::Id::unique("shotrs-editor"))
            .fixed_pos(self.pos)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut buf)
                        .id(egui::Id::unique("shotrs-text-input"))
                        .font(FontId::proportional(self.size))
                        .text_color(self.color)
                        .desired_width(320.0)
                        .hint_text("输入文字（试试中文）"),
                );
                if self.first_frame || !resp.has_focus() {
                    wants_focus = true;
                }
            });
        if wants_focus {
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique("shotrs-text-input")));
        }
        self.first_frame = false;
        self.buf = buf;
    }

    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>> {
        if self.buf.trim().is_empty() {
            return None;
        }
        Some(Box::new(Text {
            pos: self.pos,
            text: self.buf,
            size: self.size,
            color: self.color,
        }))
    }
}

/// 文字工具。跨帧状态就是它的样式参数
pub struct TextTool {
    color: Color32,
    size: f32,
}

impl Default for TextTool {
    fn default() -> Self {
        Self {
            color: ACCENT,
            size: 20.0,
        }
    }
}

impl Settings for TextTool {
    fn ui(&mut self, ui: &mut Ui) {
        common::color_row(ui, &mut self.color);
        common::value_slider(ui, "字号", &mut self.size, 10.0..=72.0, " px");
    }
}

impl Tool for TextTool {
    fn id(&self) -> &'static str {
        "text"
    }

    fn label(&self) -> &'static str {
        "文字"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::T)
    }

    fn order(&self) -> i32 {
        50
    }

    fn click(&mut self, at: Pos2) -> Option<Box<dyn InlineEditor>> {
        Some(Box::new(TextEditor::new(at, self.size, self.color)))
    }

    fn settings(&mut self) -> Option<&mut dyn Settings> {
        Some(self)
    }

    fn hint(&self) -> &'static str {
        "点击 = 写字"
    }
}
