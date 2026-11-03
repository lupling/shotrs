//! 矩形框插件：拖出一个描边矩形。
//!
//! 和箭头同一个套路（拖动型 + 属性面板），只是绘制用 `rect_stroke`。

use egui::{Color32, Key, Painter, Pos2, Rect, Stroke, StrokeKind, Ui};

use super::{common, Draft, Settings, Shape, Tool, ACCENT};

#[derive(Clone, Copy, Debug)]
pub struct RectShape {
    pub from: Pos2,
    pub to: Pos2,
    pub width: f32,
    pub color: Color32,
}

impl RectShape {
    pub const MIN_SIZE: f32 = 8.0;

    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.from, self.to)
    }
}

impl Shape for RectShape {
    fn draw(&self, painter: &Painter) {
        painter.rect_stroke(
            self.rect(),
            0.0,
            Stroke::new(self.width, self.color),
            StrokeKind::Middle,
        );
    }

    fn bounds(&self) -> Rect {
        self.rect().expand(self.width * 2.0)
    }

    /// 命中「描边带」：点在边框附近才算拖它，内部留空好继续画别的
    fn contains(&self, p: Pos2) -> bool {
        let r = self.rect();
        let band = self.width * 0.5 + 5.0;
        r.expand(band).contains(p) && !r.shrink(band).contains(p)
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.from = rect.min;
        self.to = rect.max;
    }

    fn clone_box(&self) -> Box<dyn Shape> {
        Box::new(self.clone())
    }
    fn translate(&mut self, delta: egui::Vec2) {
        self.from += delta;
        self.to += delta;
    }
}

impl Draft for RectShape {
    fn to(&mut self, at: Pos2) {
        self.to = at;
    }

    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>> {
        let r = self.rect();
        if r.width() < Self::MIN_SIZE || r.height() < Self::MIN_SIZE {
            None
        } else {
            Some(self)
        }
    }
}

pub struct RectTool {
    color: Color32,
    width: f32,
}

impl Default for RectTool {
    fn default() -> Self {
        Self {
            color: ACCENT,
            width: 3.0,
        }
    }
}

impl Settings for RectTool {
    fn ui(&mut self, ui: &mut Ui) {
        common::color_row(ui, &mut self.color);
        common::value_slider(ui, "线宽", &mut self.width, 1.0..=12.0, " px");
    }
}

impl Tool for RectTool {
    fn id(&self) -> &'static str {
        "rect"
    }

    fn label(&self) -> &'static str {
        "矩形"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::R)
    }

    fn order(&self) -> i32 {
        20
    }

    fn begin(&mut self, at: Pos2) -> Option<Box<dyn Draft>> {
        Some(Box::new(RectShape {
            from: at,
            to: at,
            width: self.width,
            color: self.color,
        }))
    }

    fn settings(&mut self) -> Option<&mut dyn Settings> {
        Some(self)
    }

    fn hint(&self) -> &'static str {
        "拖动 = 画矩形"
    }
}
