//! 属性面板里复用的通用控件：调色板、数值滑杆。
//!
//! 抽在这里是因为「颜色」「线宽/字号」几乎所有标注工具都要用，
//! 各插件只负责决定自己需要哪几项、以及这些值怎么用。

use egui::{Color32, Sense, Stroke, StrokeKind, Ui, Vec2};

/// 预设色板。想加颜色只改这一个数组
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(255, 64, 64),   // 红
    Color32::from_rgb(255, 150, 40),  // 橙
    Color32::from_rgb(255, 214, 0),   // 黄
    Color32::from_rgb(72, 214, 96),   // 绿
    Color32::from_rgb(40, 190, 240),  // 青
    Color32::from_rgb(80, 130, 255),  // 蓝
    Color32::from_rgb(255, 255, 255), // 白
    Color32::from_rgb(28, 28, 32),    // 黑
];

/// 一排色块 + 一个任意取色按钮。返回是否改动过
pub fn color_row(ui: &mut Ui, color: &mut Color32) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for preset in PALETTE {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
            ui.painter().rect_filled(rect, 4.0, preset);
            if *color == preset {
                ui.painter().rect_stroke(
                    rect.expand(1.0),
                    4.0,
                    Stroke::new(1.5, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
            if resp.clicked() {
                *color = preset;
                changed = true;
            }
        }
        // 色块之外还想要任意色：交给 egui 自带的取色器
        if ui.color_edit_button_srgba(color).changed() {
            changed = true;
        }
    });
    changed
}

/// 数值滑杆（线宽、字号……）。返回是否改动过
pub fn value_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(11.0));
        changed = ui
            .add(
                egui::Slider::new(value, range)
                    .show_value(true)
                    .suffix(suffix)
                    .trailing_fill(true),
            )
            .changed();
    });
    changed
}
