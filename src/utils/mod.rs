//! 与界面无关的通用功能：剪贴板、存盘、配置。
//!
//! 这三件事都只依赖「一张图」或「一个路径」，和 egui / winit 没有关系，
//! 所以放在这里，`main.rs` 只负责在合适的时机调用它们。

pub mod clipboard;
pub mod config;
pub mod png;
pub mod save;

/// 一张裁好的成品图（RGBA8，行优先，自上而下）。
///
/// 界面层（egui 的 `ColorImage`）和这些功能之间用这个结构解耦 ——
/// 复制到剪贴板、存成 PNG 都只认它。
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Bitmap {
    pub fn new(width: usize, height: usize, rgba: Vec<u8>) -> Self {
        debug_assert_eq!(rgba.len(), width * height * 4);
        Self {
            width,
            height,
            rgba,
        }
    }
}
