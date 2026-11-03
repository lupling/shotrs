//! 箭头插件：拖动出一根带箭头的线段。
//!
//! 这是「拖动型 + 属性面板」工具的样板：`begin` 给草稿、拖动期间主程序每帧调
//! `Draft::to`、松手时 `Draft::finish` 决定落不落定。颜色和线宽存在工具自己身上，
//! 通过 `Settings` 摆成属性面板里的控件。

use egui::{Color32, Key, Painter, Pos2, Rect, Shape as EguiShape, Stroke, Ui};

use super::{common, Draft, Settings, Shape, Tool, ACCENT};

#[derive(Clone, Copy, Debug)]
pub struct Arrow {
    pub from: Pos2,
    pub to: Pos2,
    pub width: f32,
    pub color: Color32,
}

impl Arrow {
    pub const MIN_LEN: f32 = 8.0;
}

impl Shape for Arrow {
    fn draw(&self, painter: &Painter) {
        let dir = self.to - self.from;
        let len = dir.length();
        if len < 2.0 {
            return;
        }
        let unit = dir / len;
        // 箭头头部随线宽变大，但不超过线长的 60%，短箭头才不会变成一个圆点
        let head = (10.0 + self.width * 3.0).min(len * 0.6);
        let side = unit.rot90() * (head * 0.55);

        let tip = self.to;
        let left = tip - unit * head + side;
        let right = tip - unit * head - side;

        // 主干收在箭头根部附近，避免线头从尖端戳出来
        painter.line_segment(
            [self.from, tip - unit * head * 0.85],
            Stroke::new(self.width, self.color),
        );
        painter.add(EguiShape::convex_polygon(
            vec![tip, left, right],
            self.color,
            Stroke::NONE,
        ));
    }

    fn bounds(&self) -> Rect {
        Rect::from_two_pos(self.from, self.to).expand(self.width * 2.0 + 12.0)
    }

    /// 命中「线段本身附近」，而不是整块包围盒 —— 否则在箭头旁边就没法画新东西
    fn contains(&self, p: Pos2) -> bool {
        let seg = self.to - self.from;
        let len2 = seg.length_sq();
        if len2 < 1.0 {
            return self.from.distance(p) <= self.width + 6.0;
        }
        let t = ((p - self.from).dot(seg) / len2).clamp(0.0, 1.0);
        let closest = self.from + seg * t;
        closest.distance(p) <= self.width + 6.0
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

impl Draft for Arrow {
    fn to(&mut self, at: Pos2) {
        self.to = at;
    }

    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>> {
        if self.from.distance(self.to) < Self::MIN_LEN {
            None
        } else {
            Some(self)
        }
    }
}

/// 箭头工具。跨帧状态就是它的样式参数
pub struct ArrowTool {
    color: Color32,
    width: f32,
}

impl Default for ArrowTool {
    fn default() -> Self {
        Self {
            color: ACCENT,
            width: 3.0,
        }
    }
}

impl Settings for ArrowTool {
    fn ui(&mut self, ui: &mut Ui) {
        common::color_row(ui, &mut self.color);
        common::value_slider(ui, "线宽", &mut self.width, 1.0..=12.0, " px");
    }
}

impl Tool for ArrowTool {
    fn id(&self) -> &'static str {
        "arrow"
    }

    fn label(&self) -> &'static str {
        "箭头"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::A)
    }

    fn order(&self) -> i32 {
        10
    }

    fn begin(&mut self, at: Pos2) -> Option<Box<dyn Draft>> {
        Some(Box::new(Arrow {
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
        "拖动 = 画箭头"
    }
}
