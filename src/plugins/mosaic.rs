//! 马赛克插件：拖出一块区域，把它变成像素块（打码）。
//!
//! 这是唯一需要读原图像素的工具，所以通过 `Tool::set_canvas` 拿到一份只读快照
//! （`CanvasSource`，由主程序在冻结帧就绪时推过来）。
//!
//! 实现不是「一格一格画方块」，而是**先把该区域按 `block` 分格、每格取平均色做成一张小纹理**，
//! 再用 `NEAREST` 放大铺到原位置 —— 一次 draw call，效果就是标准马赛克，
//! 而且导出时就是后缓冲里的这些像素，天然正确。

use egui::{Color32, Key, Painter, Pos2, Rect, Stroke, Ui};
use std::sync::Arc;

use super::{common, CanvasMap, CanvasSource, Draft, Settings, Shape, Tool};

#[derive(Clone)]
pub struct MosaicDraft {
    from: Pos2,
    to: Pos2,
    block: f32,
    source: Option<Arc<CanvasSource>>,
    /// 编辑器当前屏幕矩形（每帧由主程序同步）
    screen: Rect,
}

impl MosaicDraft {
    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.from, self.to)
    }
}

impl Shape for MosaicDraft {
    /// 拖动过程中只画一个半透明预览框，避免每帧都重算像素
    fn draw(&self, painter: &Painter) {
        let r = self.rect();
        painter.rect_filled(r, 0.0, Color32::from_rgba_premultiplied(0, 0, 0, 60));
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.0, Color32::from_rgb(230, 230, 240)),
            egui::StrokeKind::Inside,
        );
    }

    fn bounds(&self) -> Rect {
        self.rect()
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.from = rect.min;
        self.to = rect.max;
    }

    fn clone_box(&self) -> Box<dyn Shape> {
        // 草稿不会进 Scene，这个方法只是为了满足 trait
        Box::new(self.clone())
    }
    fn translate(&mut self, delta: egui::Vec2) {
        self.from += delta;
        self.to += delta;
    }
}

impl Draft for MosaicDraft {
    fn to(&mut self, at: Pos2) {
        self.to = at;
    }

    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>> {
        let rect = self.rect();
        if rect.width() < 6.0 || rect.height() < 6.0 {
            return None;
        }
        let source = self.source?;
        // 换算必须用「此刻」的屏幕矩形，不能用建快照时的尺寸
        let map = CanvasMap::new((source.width, source.height), self.screen);
        let tex = source.pixelate(rect, self.block, map)?;
        Some(Box::new(Mosaic { rect, tex }))
    }
}

/// 落定后的马赛克：一张小纹理铺在矩形上（NEAREST 放大 = 像素块）
#[derive(Clone)]
pub struct Mosaic {
    rect: Rect,
    tex: egui::TextureHandle,
}

impl Shape for Mosaic {
    fn draw(&self, painter: &Painter) {
        painter.image(
            self.tex.id(),
            self.rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn bounds(&self) -> Rect {
        self.rect
    }

    fn translate(&mut self, delta: egui::Vec2) {
        self.rect = self.rect.translate(delta);
    }

    fn set_bounds(&mut self, rect: Rect) {
        self.rect = rect;
    }

    fn clone_box(&self) -> Box<dyn Shape> {
        Box::new(self.clone())
    }
}

pub struct MosaicTool {
    block: f32,
    source: Option<Arc<CanvasSource>>,
    /// 编辑器当前屏幕矩形（每帧由主程序同步）
    screen: Rect,
}

impl Default for MosaicTool {
    fn default() -> Self {
        Self {
            block: 16.0,
            source: None,
            screen: Rect::NOTHING,
        }
    }
}

impl Settings for MosaicTool {
    fn ui(&mut self, ui: &mut Ui) {
        common::value_slider(ui, "块大小", &mut self.block, 4.0..=48.0, " px");
    }
}

impl Tool for MosaicTool {
    fn id(&self) -> &'static str {
        "mosaic"
    }

    fn label(&self) -> &'static str {
        "马赛克"
    }

    fn hotkey(&self) -> Option<Key> {
        Some(Key::M)
    }

    fn order(&self) -> i32 {
        40
    }

    fn begin(&mut self, at: Pos2) -> Option<Box<dyn Draft>> {
        Some(Box::new(MosaicDraft {
            from: at,
            to: at,
            block: self.block,
            source: self.source.clone(),
            screen: self.screen,
        }))
    }

    fn settings(&mut self) -> Option<&mut dyn Settings> {
        Some(self)
    }

    fn set_canvas(&mut self, source: Option<Arc<CanvasSource>>) {
        self.source = source;
    }

    fn set_canvas_screen(&mut self, screen: Rect) {
        self.screen = screen;
    }

    fn hint(&self) -> &'static str {
        if self.source.is_some() {
            "拖动 = 打码"
        } else {
            "拖动 = 打码（原图未就绪）"
        }
    }
}
