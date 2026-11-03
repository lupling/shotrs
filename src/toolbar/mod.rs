//! 工具栏：一排工具按钮 + 当前工具的属性面板（子工具栏）。
//!
//! 这里只跟 `plugins` 的协议打交道（`Tool` / `Settings`），不知道具体有哪些工具。
//! 职责：
//!   * 持有插件实例（出厂排序按 `Tool::order()`）
//!   * 画按钮栏，处理点击切换、**拖动排序**
//!   * 当前工具有属性面板（`Tool::settings() != None`）时，在按钮栏下面画子工具栏
//!   * 快捷键切换
//!
//! 返回一个 `ToolbarOutput`，主程序据此把「指针落在工具栏上」的点击从画布交互里排除。

use egui::{Color32, FontId, Id, Pos2, Rect, Sense, Vec2};

use crate::plugins::{self, Settings, Tool};

const ACTIVE_BG: Color32 = Color32::from_rgb(46, 96, 168);
const HOVER_BG: Color32 = Color32::from_rgba_premultiplied(28, 28, 28, 28);
const FG: Color32 = Color32::from_gray(222);
const FG_ACTIVE: Color32 = Color32::WHITE;

pub struct Toolbox {
    tools: Vec<Box<dyn Tool>>,
    active: usize,
    /// 正在被拖动的按钮下标（拖动排序用）
    dragging: Option<usize>,
    /// 这一帧是否切换过工具（主程序据此清掉标注选中态）
    tool_switched: bool,
}

impl Toolbox {
    pub fn new() -> Self {
        let mut tools = plugins::register();
        // 出厂顺序由插件自己声明；用户拖动排序只在本次运行内有效（没做持久化）
        tools.sort_by_key(|t| t.order());
        Self {
            tools,
            active: 0,
            dragging: None,
            tool_switched: false,
        }
    }

    /// 每帧把「编辑器当前屏幕矩形」同步给所有工具（马赛克换算用）
    pub fn set_canvas_screen(&mut self, screen: egui::Rect) {
        for tool in &mut self.tools {
            tool.set_canvas_screen(screen);
        }
    }

    /// 画布快照变化时转发给所有工具（只有马赛克这类工具关心）
    pub fn set_canvas(&mut self, source: Option<std::sync::Arc<plugins::CanvasSource>>) {
        for tool in &mut self.tools {
            tool.set_canvas(source.clone());
        }
    }

    pub fn active(&self) -> &dyn Tool {
        &*self.tools[self.active]
    }

    pub fn active_mut(&mut self) -> &mut dyn Tool {
        &mut *self.tools[self.active]
    }

    /// 快捷键切换。主程序在「没有内联编辑器、也不在打字」时每帧调一次
    pub fn handle_hotkeys(&mut self, ctx: &egui::Context) {
        let mut hotkey_switched = false;
        for (i, tool) in self.tools.iter().enumerate() {
            if let Some(key) = tool.hotkey() {
                if ctx.input(|inp| inp.key_pressed(key)) {
                    if self.active != i {
                        hotkey_switched = true;
                    }
                    self.active = i;
                }
            }
        }
        if hotkey_switched {
            self.tool_switched = true;
        }
    }

    /// 取出「这一帧切换过工具」的标志并清零
    pub fn take_tool_switched(&mut self) -> bool {
        std::mem::take(&mut self.tool_switched)
    }

    /// 画按钮栏和属性面板，返回它们占用的屏幕区域
    pub fn ui(&mut self, ctx: &egui::Context, screen: Rect) -> Rect {
        let mut clicked: Option<usize> = None;
        let mut slots: Vec<Rect> = Vec::new();
        let mut drag_started: Option<usize> = None;
        let mut drag_stopped = false;

        let bar = egui::Area::new(Id::unique("shotrs-toolbar"))
            .fixed_pos(screen.left_top() + Vec2::new(16.0, 64.0))
            .show(ctx, |ui| {
                panel_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for i in 0..self.tools.len() {
                            let text = button_text(&*self.tools[i]);
                            let (rect, resp) = tool_button(ui, &text, i == self.active);
                            if resp.clicked() {
                                clicked = Some(i);
                            }
                            if resp.drag_started() {
                                drag_started = Some(i);
                            }
                            if resp.drag_stopped() {
                                drag_stopped = true;
                            }
                            slots.push(rect);
                        }
                    });
                });
            });

        if let Some(i) = clicked {
            self.active = i;
        }

        // ---- 拖动排序：指针越过谁的中心，就跟谁换位
        if drag_started.is_some() {
            self.dragging = drag_started;
        }
        if let Some(from) = self.dragging {
            if let Some(px) = ctx.pointer_interact_pos() {
                // 目标位置 = 「中心点在自己左边的最后一个槽位」；指针在最左侧时落到 0
                let mut to = 0;
                for (i, rect) in slots.iter().enumerate() {
                    if px.x >= rect.center().x {
                        to = i;
                    }
                }
                if to != from {
                    self.tools.swap(from, to);
                    // 当前工具跟着自己的按钮走
                    if self.active == from {
                        self.active = to;
                    } else if self.active == to {
                        self.active = from;
                    }
                    self.dragging = Some(to);
                }
            }
        }
        if drag_stopped {
            self.dragging = None;
        }

        let mut rect = bar.response.rect;

        // ---- 属性面板：只有当前工具有可调项时才出现；拖动排序期间不画，免得跟着跳
        if self.dragging.is_none() {
            let tool_id = self.tools[self.active].id();
            if let Some(settings) = self.tools[self.active].settings() {
                let settings_rect = settings_panel(
                    ctx,
                    rect.left_bottom() + Vec2::new(0.0, 6.0),
                    tool_id,
                    settings,
                );
                rect = rect.union(settings_rect);
            }
        }

        rect
    }
}

fn panel_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::from_black_alpha(200))
        .inner_margin(8.0)
        .corner_radius(8.0)
}

fn settings_panel(
    ctx: &egui::Context,
    pos: Pos2,
    tool_id: &'static str,
    settings: &mut dyn Settings,
) -> Rect {
    egui::Area::new(Id::unique("shotrs-toolbar-settings"))
        .fixed_pos(pos)
        .show(ctx, |ui| {
            panel_frame().show(ui, |ui| {
                // 每个工具一套 Id 作用域，切工具时滑块/取色器的状态互不干扰
                ui.push_id(tool_id, |ui| {
                    ui.horizontal(|ui| {
                        settings.ui(ui);
                    });
                });
            });
        })
        .response
        .rect
}

/// 按钮文案：名字 + 快捷键（有的话）
fn button_text(tool: &dyn Tool) -> String {
    match tool.hotkey() {
        Some(key) => format!("{}  {}", tool.label(), key.name()),
        None => tool.label().to_string(),
    }
}

/// 自绘按钮：需要 drag 语义（拖动排序）和跟随选中态的高亮，所以不用 egui 的 Button
fn tool_button(ui: &mut egui::Ui, text: &str, active: bool) -> (Rect, egui::Response) {
    let color = if active { FG_ACTIVE } else { FG };
    let galley = ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(text.to_owned(), FontId::proportional(13.0), color));
    let size = galley.size() + Vec2::new(22.0, 12.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());

    let bg = if active {
        ACTIVE_BG
    } else if resp.hovered() {
        HOVER_BG
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 6.0, bg);
    }
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, color);

    if resp.hovered() && !active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    (rect, resp.on_hover_text("点击切换 · 拖动可排序"))
}
