//! 选区：命中测试、平移、缩放、把手绘制。
//!
//! 独立成模块的原因：这套东西只跟「一个矩形 + 指针」有关，跟标注插件完全无关。
//! `main.rs` 只负责决定「这一次拖动该交给谁」——拖边框给本模块，拖内部给当前插件。

use egui::{Color32, CornerRadius, CursorIcon, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// 边框/把手的抓取带宽（逻辑点）
pub const BAND: f32 = 8.0;
/// 全屏选区时加宽的抓取带宽：边框正好压在屏幕边缘，8pt 基本点不中
pub const EDGE_BAND: f32 = 16.0;
/// 选区最小尺寸
pub const MIN_SIZE: f32 = 12.0;

/// 选区是不是正好铺满整个屏幕（全屏截图的默认选区）
pub fn covers_screen(sel: Rect, screen: Rect) -> bool {
    const EPS: f32 = 1.0;
    (sel.min.x - screen.min.x).abs() <= EPS
        && (sel.min.y - screen.min.y).abs() <= EPS
        && (sel.max.x - screen.max.x).abs() <= EPS
        && (sel.max.y - screen.max.y).abs() <= EPS
}

/// 八个把手
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Handle {
    pub fn cursor(self) -> CursorIcon {
        match self {
            Self::Left | Self::Right => CursorIcon::ResizeHorizontal,
            Self::Top | Self::Bottom => CursorIcon::ResizeVertical,
            Self::TopLeft | Self::BottomRight => CursorIcon::ResizeNwSe,
            Self::TopRight | Self::BottomLeft => CursorIcon::ResizeNeSw,
        }
    }
}

/// 指针落在选区的哪一部分
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Handle(Handle),
    Inside,
    Outside,
}

/// 一次拖动要干什么
#[derive(Clone, Copy, Debug)]
pub enum Grab {
    /// 拉一个新选区
    New,
    /// 整体平移
    Move,
    /// 拖某个边/角
    Resize(Handle),
}

impl Grab {
    pub fn cursor(self) -> CursorIcon {
        match self {
            Self::New => CursorIcon::Crosshair,
            Self::Move => CursorIcon::Grabbing,
            Self::Resize(h) => h.cursor(),
        }
    }
}

/// 命中测试：角优先于边，边优先于内部
pub fn hit(sel: Rect, p: Pos2) -> Hit {
    hit_with_band(sel, p, BAND)
}

/// 选区自己的命中测试：全屏选区时边框就在屏幕边缘，把抓取带加宽，
/// 否则用户几乎不可能在 8pt 内点中「缩回去」的那条边。
pub fn hit_selection(sel: Rect, p: Pos2, screen: Rect) -> Hit {
    let band = if covers_screen(sel, screen) {
        EDGE_BAND
    } else {
        BAND
    };
    hit_with_band(sel, p, band)
}

fn hit_with_band(sel: Rect, p: Pos2, b: f32) -> Hit {
    let near_l = (p.x - sel.min.x).abs() <= b;
    let near_r = (p.x - sel.max.x).abs() <= b;
    let near_t = (p.y - sel.min.y).abs() <= b;
    let near_b = (p.y - sel.max.y).abs() <= b;
    let within_x = p.x >= sel.min.x - b && p.x <= sel.max.x + b;
    let within_y = p.y >= sel.min.y - b && p.y <= sel.max.y + b;

    if within_x && within_y {
        match (near_l, near_r, near_t, near_b) {
            (true, _, true, _) => return Hit::Handle(Handle::TopLeft),
            (_, true, true, _) => return Hit::Handle(Handle::TopRight),
            (true, _, _, true) => return Hit::Handle(Handle::BottomLeft),
            (_, true, _, true) => return Hit::Handle(Handle::BottomRight),
            (true, _, false, false) => return Hit::Handle(Handle::Left),
            (_, true, false, false) => return Hit::Handle(Handle::Right),
            (false, false, true, _) => return Hit::Handle(Handle::Top),
            (false, false, _, true) => return Hit::Handle(Handle::Bottom),
            _ => {}
        }
    }

    if sel.contains(p) {
        Hit::Inside
    } else {
        Hit::Outside
    }
}

/// 拖动后的新选区。`rect0` 是按下那一刻的选区，`from`/`to` 是按下点与当前点
pub fn drag_rect(rect0: Rect, grab: Grab, from: Pos2, to: Pos2, screen: Rect) -> Rect {
    let delta = to - from;
    match grab {
        Grab::New => {
            let r = Rect::from_two_pos(from, to);
            clamp_to_screen(r, screen)
        }
        Grab::Move => {
            // 整体平移：先按位移算，再整体夹回屏幕内（保持尺寸不变）
            let moved = rect0.translate(delta);
            clamp_move(moved, screen)
        }
        Grab::Resize(handle) => {
            let mut min = rect0.min;
            let mut max = rect0.max;
            if matches!(
                handle,
                Handle::Left | Handle::TopLeft | Handle::BottomLeft
            ) {
                min.x += delta.x;
            }
            if matches!(
                handle,
                Handle::Right | Handle::TopRight | Handle::BottomRight
            ) {
                max.x += delta.x;
            }
            if matches!(
                handle,
                Handle::Top | Handle::TopLeft | Handle::TopRight
            ) {
                min.y += delta.y;
            }
            if matches!(
                handle,
                Handle::Bottom | Handle::BottomLeft | Handle::BottomRight
            ) {
                max.y += delta.y;
            }

            // 先保证最小尺寸（被拖过头时把对边推回来，而不是翻转）
            if max.x - min.x < MIN_SIZE {
                if matches!(
                    handle,
                    Handle::Left | Handle::TopLeft | Handle::BottomLeft
                ) {
                    min.x = max.x - MIN_SIZE;
                } else {
                    max.x = min.x + MIN_SIZE;
                }
            }
            if max.y - min.y < MIN_SIZE {
                if matches!(
                    handle,
                    Handle::Top | Handle::TopLeft | Handle::TopRight
                ) {
                    min.y = max.y - MIN_SIZE;
                } else {
                    max.y = min.y + MIN_SIZE;
                }
            }

            clamp_to_screen(Rect::from_min_max(min, max), screen)
        }
    }
}

/// 夹进屏幕，且不小于最小尺寸
fn clamp_to_screen(r: Rect, screen: Rect) -> Rect {
    let mut r = r.intersect(screen);
    if r.width() < MIN_SIZE {
        let x = r.min.x.clamp(screen.min.x, (screen.max.x - MIN_SIZE).max(screen.min.x));
        r = Rect::from_min_max(Pos2::new(x, r.min.y), Pos2::new(x + MIN_SIZE.min(screen.width()), r.max.y));
    }
    if r.height() < MIN_SIZE {
        let y = r.min.y.clamp(screen.min.y, (screen.max.y - MIN_SIZE).max(screen.min.y));
        r = Rect::from_min_max(Pos2::new(r.min.x, y), Pos2::new(r.max.x, y + MIN_SIZE.min(screen.height())));
    }
    r
}

/// 平移时保持尺寸，只把位置夹回屏幕
fn clamp_move(r: Rect, screen: Rect) -> Rect {
    let dx = if r.min.x < screen.min.x {
        screen.min.x - r.min.x
    } else if r.max.x > screen.max.x {
        screen.max.x - r.max.x
    } else {
        0.0
    };
    let dy = if r.min.y < screen.min.y {
        screen.min.y - r.min.y
    } else if r.max.y > screen.max.y {
        screen.max.y - r.max.y
    } else {
        0.0
    };
    r.translate(Vec2::new(dx, dy))
}

/// 画八个把手 + 一圈内描边。只在「指针挨着边框」时画，免得挡视线
pub fn draw_handles(painter: &Painter, sel: Rect, accent: Color32) {
    const HALF: f32 = 4.0;
    let corners = [
        sel.left_top(),
        sel.right_top(),
        sel.left_bottom(),
        sel.right_bottom(),
        sel.center_top(),
        sel.center_bottom(),
        sel.left_center(),
        sel.right_center(),
    ];
    for c in corners {
        let rect = Rect::from_center_size(c, Vec2::splat(HALF * 2.0));
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, accent),
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0))
    }

    #[test]
    fn covers_screen_认全屏选区() {
        assert!(covers_screen(screen(), screen()));
        // 少数几个像素的误差仍算全屏（裁剪/取整会带一点）
        assert!(covers_screen(screen().shrink(0.5), screen()));
        assert!(!covers_screen(screen().shrink(2.0), screen()));
        assert!(!covers_screen(
            Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(800.0, 600.0)),
            screen()
        ));
    }

    #[test]
    fn 全屏选区的边框仍然能抓住() {
        let sel = screen();
        // 距上边缘 12pt：普通带宽（8pt）够不到，全屏带宽（16pt）能抓住
        let p = Pos2::new(960.0, 12.0);
        assert!(matches!(hit(sel, p), Hit::Inside));
        assert_eq!(hit_selection(sel, p, screen()), Hit::Handle(Handle::Top));
        // 屏幕正中间还是内部
        assert_eq!(
            hit_selection(sel, screen().center(), screen()),
            Hit::Inside
        );
    }
}
