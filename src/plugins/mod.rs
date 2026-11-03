//! 标注插件协议 + 注册表。
//!
//! 一个插件 = 一种标注工具，负责四件事：
//!   1. 把一次指针操作变成「草稿」（拖动型，如箭头）或「内联编辑器」（点击型，如文字）
//!   2. 决定自己产出的标注怎么画
//!   3. 声明显示名、快捷键、排序权重、提示文案
//!   4. 可选地提供自己的属性面板（颜色、线宽、字号……）
//!
//! 主程序（main.rs）只认下面这些 trait，完全不知道「箭头」「文字」的存在；
//! 按钮栏/属性面板的布局由 `crate::toolbar` 负责，也不在这里。
//!
//! 新增一种标注 = 在 plugins/ 下加一个文件 + 在 [`register`] 里加一行。

use std::sync::Arc;

use egui::{Color32, Key, Painter, Pos2, Rect, Ui};

pub mod arrow;
pub mod common;
pub mod ellipse;
pub mod mosaic;
pub mod rect;
pub mod select;
pub mod text;

/// 所有插件共用的强调色
pub const ACCENT: Color32 = Color32::from_rgb(255, 64, 64);

/// 已经落定的标注。自己知道怎么画；主程序只负责排序、裁剪、撤销、拖动。
pub trait Shape {
    fn draw(&self, painter: &Painter);

    /// 包围盒（脏区裁剪 / 序列化 / 默认命中测试用）
    fn bounds(&self) -> Rect;

    /// 命中测试：指针是否点在这条标注上。默认用包围盒，
    /// 大部分工具会覆写成「贴着形状本身」，这样点空白处仍然是画新的，而不是拖走旧的。
    fn contains(&self, p: Pos2) -> bool {
        self.bounds().contains(p)
    }

    /// 整体平移（拖动已有标注）
    fn translate(&mut self, delta: egui::Vec2);

    /// 把标注缩放/挪到新的包围盒（拖角把手时用）
    fn set_bounds(&mut self, rect: Rect);

    /// 克隆一份（撤回/恢复用的是整屏快照，所以需要它）
    fn clone_box(&self) -> Box<dyn Shape>;
}

/// 拖动过程中的临时对象（还没落定），自己就是一张可以边拖边画的 Shape
pub trait Draft: Shape {
    /// 拖动到新位置
    fn to(&mut self, at: Pos2);
    /// 落定。返回 None 表示作废（比如拖得太短）
    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>>;
}

/// 内联编辑器：像文字这种需要挂真实控件（才能用 IME 中文输入）的标注
pub trait InlineEditor {
    /// 画内容控件。由插件自己摆 Area / TextEdit（位置、样式都是插件的事）
    fn ui(&mut self, ctx: &egui::Context);
    /// 提交。返回 None 表示内容为空，整条作废
    fn finish(self: Box<Self>) -> Option<Box<dyn Shape>>;
}

/// 画布快照：给「马赛克」这类需要读原图像素的工具用。
///
/// 冻结帧是按当前屏裁好的 RGBA，编辑器把它铺满整个窗口；这里的 `px_per_point`
/// 就是两者之间的换算（帧像素 / 逻辑点），插件据此把逻辑矩形映射回图像像素。
pub struct CanvasSource {
    /// plugins 拿它创建纹理（egui::Context 是 Clone + Send + Sync 的）
    pub ctx: egui::Context,
    pub rgba: Arc<Vec<u8>>,
    pub width: usize,
    pub height: usize,
}

/// 帧像素 ↔ 逻辑点的换算（每次都按**当前**屏幕矩形算）。
///
/// 早先把这个换算固定成「建纹理那一刻」的值，结果那时窗口还没完成全屏切换，
/// `viewport_rect()` 还是小窗口尺寸 → 换算系数被放大 5 倍多 → 采样跑到画面外，
/// 马赛克看起来像一块和背景无关的暗斑（用户反馈「没有效果」）。
#[derive(Clone, Copy)]
pub struct CanvasMap {
    /// 1 逻辑点对应多少帧像素
    pub px_per_point: egui::Vec2,
    /// 逻辑原点在帧里的像素位置
    pub origin: Pos2,
}

impl CanvasMap {
    pub fn new(frame: (usize, usize), screen: Rect) -> Self {
        Self {
            px_per_point: egui::Vec2::new(
                frame.0 as f32 / screen.width().max(1.0),
                frame.1 as f32 / screen.height().max(1.0),
            ),
            origin: screen.min,
        }
    }
}

impl CanvasSource {
    /// 把一块「逻辑点矩形」按 `block` 大小分格，每格取源图平均色，做成一张小纹理。
    /// 之后用 NEAREST 放大绘制就是马赛克效果。
    pub fn pixelate(
        &self,
        rect: Rect,
        block: f32,
        map: CanvasMap,
    ) -> Option<egui::TextureHandle> {
        let block = block.max(2.0);
        let cols = (rect.width() / block).ceil().max(1.0) as usize;
        let rows = (rect.height() / block).ceil().max(1.0) as usize;
        let mut pixels = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            for col in 0..cols {
                let cell = Rect::from_min_size(
                    rect.min + egui::vec2(col as f32 * block, row as f32 * block),
                    egui::vec2(block, block),
                );
                pixels.push(self.average(cell, map));
            }
        }
        let img = egui::ColorImage {
            size: [cols, rows],
            pixels,
            source_size: egui::vec2(cols as f32, rows as f32),
        };
        Some(self.ctx.load_texture(
            format!("mosaic-{}-{}", cols, rows),
            img,
            egui::TextureOptions::NEAREST,
        ))
    }

    /// 取某个逻辑矩形内源图的平均色（按步长采样，避免逐像素扫）
    fn average(&self, rect: Rect, map: CanvasMap) -> Color32 {
        let x0 = ((rect.min.x - map.origin.x) * map.px_per_point.x) as isize;
        let y0 = ((rect.min.y - map.origin.y) * map.px_per_point.y) as isize;
        let x1 = ((rect.max.x - map.origin.x) * map.px_per_point.x) as isize;
        let y1 = ((rect.max.y - map.origin.y) * map.px_per_point.y) as isize;
        let (x0, y0) = (x0.clamp(0, self.width as isize - 1), y0.clamp(0, self.height as isize - 1));
        let (x1, y1) = (x1.clamp(x0 + 1, self.width as isize), y1.clamp(y0 + 1, self.height as isize));
        let step_x = (((x1 - x0) / 12).max(1)) as usize;
        let step_y = (((y1 - y0) / 12).max(1)) as usize;
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        let mut y = y0 as usize;
        while y < y1 as usize {
            let mut x = x0 as usize;
            while x < x1 as usize {
                let o = (y * self.width + x) * 4;
                if o + 2 < self.rgba.len() {
                    r += self.rgba[o] as u32;
                    g += self.rgba[o + 1] as u32;
                    b += self.rgba[o + 2] as u32;
                    n += 1;
                }
                x += step_x;
            }
            y += step_y;
        }
        if n == 0 {
            return Color32::from_gray(80);
        }
        Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
    }
}

/// 属性面板。工具自己实现，画在按钮栏下面的子工具栏里。
///
/// 参数（颜色/线宽……）就存在工具结构体里，本 trait 只负责把它们摆成控件 ——
/// 所以 `Tool::settings` 直接把 `self` 交出来即可。
pub trait Settings {
    fn ui(&mut self, ui: &mut Ui);
}

/// 一种标注工具
pub trait Tool {
    /// 稳定标识，用作属性面板控件与按钮的 egui Id
    fn id(&self) -> &'static str;
    /// 按钮上显示的名字
    fn label(&self) -> &'static str;
    /// 快捷键（可选）
    fn hotkey(&self) -> Option<Key> {
        None
    }
    /// 按钮栏里的排序权重，小的在左边。按钮栏也支持拖动重排（运行期会覆盖它）
    fn order(&self) -> i32 {
        0
    }

    /// 指针按下：拖动型工具在这里给出草稿
    fn begin(&mut self, _at: Pos2) -> Option<Box<dyn Draft>> {
        None
    }

    /// 指针按下：点击型工具在这里开一个内联编辑器
    fn click(&mut self, _at: Pos2) -> Option<Box<dyn InlineEditor>> {
        None
    }

    /// 属性面板。返回 None = 这个工具没有可调项，按钮栏就不显示子工具栏
    fn settings(&mut self) -> Option<&mut dyn Settings> {
        None
    }

    /// 这个工具是不是「只操作选区」——不产生标注，画布上的拖动用来平移选区
    fn is_selection_tool(&self) -> bool {
        false
    }

    /// 画布快照就绪/失效时会调用一次（只有需要读原图像素的工具要实现）
    fn set_canvas(&mut self, _source: Option<Arc<CanvasSource>>) {}

    /// 每帧同步「编辑器当前屏幕矩形」：帧像素 ↔ 逻辑点的换算依赖它，
    /// 不能用建纹理那一刻的尺寸（那时可能还没全屏）
    fn set_canvas_screen(&mut self, _screen: Rect) {}

    /// 底部提示里的工具说明
    fn hint(&self) -> &'static str {
        ""
    }
}

/// 插件注册表 —— 新增标注工具只需要在这里加一行
pub fn register() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(select::SelectTool),
        Box::new(arrow::ArrowTool::default()),
        Box::new(rect::RectTool::default()),
        Box::new(ellipse::EllipseTool::default()),
        Box::new(mosaic::MosaicTool::default()),
        Box::new(text::TextTool::default()),
    ]
}
