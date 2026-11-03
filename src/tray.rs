//! 顶部栏托盘图标（StatusNotifierItem / AppIndicator）。
//!
//! GNOME 没有传统托盘，是靠 `ubuntu-appindicators` 扩展把 SNI 项画到顶部栏上的。
//! 这里用 ksni（纯 Rust，自带线程和 tokio runtime），菜单点击通过 mpsc 回传给主线程 ——
//! 主线程只管轮询事件，不碰 D-Bus。
//!
//! 为什么托盘是「最小化」的必要条件：Wayland 下窗口一旦 `set_minimized` 就
//! **无法再恢复**（winit 明确不支持 unminimize）。所以「最小化」在本程序里的实现是
//! **销毁窗口、进程留在后台**，恢复窗口只能靠托盘菜单或全局快捷键。

use std::sync::mpsc::{self, Receiver, Sender};

use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};

/// 托盘只能通过这个通道跟主线程说话
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// 截一张图（左键点图标 / 菜单「截图」）
    Screenshot,
    /// 把窗口重新显示出来
    ShowWindow,
    /// 隐藏到后台（销毁窗口）
    Minimize,
    /// 退出程序
    Quit,
    /// 菜单里切换「关闭时最小化」
    SetCloseToTray(bool),
    /// 菜单里选「截哪块屏」（None = 跟随窗口）
    SetCaptureDisplay(Option<String>),
}

const ICON_PNG: &[u8] = include_bytes!("../assets/shotrs-256.png");

pub struct Tray {
    tx: Sender<TrayEvent>,
    /// 菜单里「关闭时最小化」的勾选状态（由主线程同步过来）
    close_to_tray: bool,
    /// 窗口是否还在（决定「显示窗口」能不能点）
    has_window: bool,
    /// 当前快捷键文案
    hotkey: String,
    /// 菜单里「截哪块屏」的当前选择（None = 跟随窗口）
    capture_display: Option<String>,
    /// 可选显示器：(名字, 带分辨率的文案)。窗口不在时主线程查不到显示器，靠它缓存
    displays: Vec<(String, String)>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "shotrs".into()
    }

    fn title(&self) -> String {
        "shotrs".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        decode_icon()
    }

    // 左键点击图标：直接截一张，比先开菜单快
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayEvent::Screenshot);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "截图".into(),
                icon_name: "camera-photo-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::Screenshot);
                }),
                ..Default::default()
            }
            .into(),
            // 截哪块屏：从托盘/快捷键触发时窗口是新建的，合成器一般放主屏，
            // 所以副屏必须在这里显式选一次
            SubMenu {
                label: "截图屏幕".into(),
                icon_name: "video-display-symbolic".into(),
                submenu: {
                    let mut items: Vec<MenuItem<Self>> = vec![CheckmarkItem {
                        label: "跟随窗口".into(),
                        checked: self.capture_display.is_none(),
                        activate: Box::new(|t: &mut Self| {
                            let _ = t.tx.send(TrayEvent::SetCaptureDisplay(None));
                        }),
                        ..Default::default()
                    }
                    .into()];
                    for (name, label) in &self.displays {
                        let picked = name.clone();
                        items.push(
                            CheckmarkItem {
                                label: label.clone(),
                                checked: self.capture_display.as_deref() == Some(name.as_str()),
                                activate: Box::new(move |t: &mut Self| {
                                    let _ = t
                                        .tx
                                        .send(TrayEvent::SetCaptureDisplay(Some(picked.clone())));
                                }),
                                ..Default::default()
                            }
                            .into(),
                        );
                    }
                    items
                },
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.has_window {
                    "显示窗口（已在显示）".into()
                } else {
                    "显示窗口".into()
                },
                enabled: !self.has_window,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("快捷键：{}", self.hotkey),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "收进托盘（隐藏窗口）".into(),
                enabled: self.has_window,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::Minimize);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "关闭时最小化到托盘".into(),
                checked: self.close_to_tray,
                activate: Box::new(|t: &mut Self| {
                    let want = !t.close_to_tray;
                    let _ = t.tx.send(TrayEvent::SetCloseToTray(want));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "退出".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct TrayHandle {
    handle: ksni::blocking::Handle<Tray>,
    pub events: Receiver<TrayEvent>,
}

impl TrayHandle {
    /// 把主线程的状态同步进托盘菜单（勾选状态、窗口是否有、快捷键文案、显示器列表）
    pub fn sync(
        &self,
        close_to_tray: bool,
        has_window: bool,
        hotkey: &str,
        capture_display: Option<String>,
        displays: Vec<(String, String)>,
    ) {
        let hotkey = hotkey.to_owned();
        let _ = self.handle.update(move |tray| {
            tray.close_to_tray = close_to_tray;
            tray.has_window = has_window;
            tray.hotkey = hotkey;
            tray.capture_display = capture_display;
            tray.displays = displays;
        });
    }

    pub fn shutdown(&self) {
        let _ = self.handle.shutdown();
    }
}

/// 注册托盘。失败（比如没有 SNI 宿主）时返回 None，程序照常跑，只是没有托盘
pub fn spawn(close_to_tray: bool, has_window: bool, hotkey: String) -> Option<TrayHandle> {
    use ksni::blocking::TrayMethods;

    let (tx, events) = mpsc::channel();
    let tray = Tray {
        tx,
        close_to_tray,
        has_window,
        hotkey,
        capture_display: None,
        displays: Vec::new(),
    };
    match tray.spawn() {
        Ok(handle) => {
            eprintln!("[shotrs] 托盘图标已注册（顶部栏，靠 ubuntu-appindicators 显示）");
            Some(TrayHandle { handle, events })
        }
        Err(e) => {
            eprintln!("[shotrs] 托盘注册失败（没有 StatusNotifier 宿主？）：{e}");
            None
        }
    }
}

/// PNG → SNI 要的 ARGB32（网络字节序，预乘 alpha）
fn decode_icon() -> Vec<ksni::Icon> {
    let Ok(img) = image::load_from_memory(ICON_PNG) else {
        return Vec::new();
    };
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        let a16 = a as u16;
        // 预乘：SNI 要求 premultiplied ARGB32
        let pr = (r as u16 * a16 / 255) as u8;
        let pg = (g as u16 * a16 / 255) as u8;
        let pb = (b as u16 * a16 / 255) as u8;
        data.extend_from_slice(&[a, pr, pg, pb]);
    }
    vec![ksni::Icon {
        width: w as i32,
        height: h as i32,
        data,
    }]
}
