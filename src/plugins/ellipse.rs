//! 椭圆插件：拖出一个描边椭圆（按住拖动，起点终点是外接矩形）。
//!
//! 和箭头同一个套路（拖动型 + 属性面板），绘制用 `Shape::ellipse_stroke`。

use egui::{Color32, Key, Painter, Pos2, Rect, Shape as EguiShape, Stroke, Ui};

use super::{common, Draft, Settings, Shape, Tool, ACCENT};

#[derive(Clone, Copy, Debug)]
pub struct EllipseShape {
    pub from: Pos2,
    pub to: Pos2,
    pub width: f32,
    pub color: Color32,
}

impl EllipseShape {
    pub const MIN_SIZE: f32 = 8.0;

    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.from, self.to)
    }
}

impl Shape for EllipseShape {
    fn draw(&self, painter: &Painter) {
        let r = self.rect();
        painter.add(EguiShape::ellipse_stroke(
            r.center(),
            r.size() * 0.5,
            Stroke::new(self.width, self.color),
        ));
    }

    fn bounds(&self) -> Rect {
        self.rect().expand(self.width * 2.0)
    }

    /// 命中「椭圆描边附近」（按归一化半径判断）
    fn contains(&self, p: Pos2) -> bool {
        let r = self.rect();
        let radius = r.size() * 0.5;
        if radius.x < 1.0 || radius.y < 1.0 {
            return false;
        }
        let v = (p - r.center()) / radius;
        let tol = (self.width * 0.5 + 5.0) / radius.x.min(radius.y);
        (v.length() - 1.0).abs() <= tol
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

impl Draft for EllipseShape {
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

pub struct EllipseTool {
    color: Color32,
    width: f32,
}

impl Default for EllipseTool {
    fn default() -> Self {
        Self {
            color: ACCENT,
            width: 3.0,
        }
    }
}

impl Settings for EllipseTool {
    fn ui(&mut self, ui: &mut Ui) {
        common::color_row(ui, &mut self.color);
        common::value_slider(ui, "线宽", &mut self.width, 1.0..=12.0, " px");
    }
}

impl Tool for EllipseTool {
    fn id(&self) -> &'static str {
        "ellipse"
    }

    fn label(&self) -> &'static str {
        "椭圆"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::E)
    }

    fn order(&self) -> i32 {
        30
    }

    fn begin(&mut self, at: Pos2) -> Option<Box<dyn Draft>> {
        Some(Box::new(EllipseShape {
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
        "拖动 = 画椭圆"
    }
}
