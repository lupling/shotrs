//! 全局快捷键：通过 `org.freedesktop.portal.GlobalShortcuts` 注册。
//!
//! Wayland 下没有别的办法 —— `XGrabKey` 只对 X11 客户端有效，而我们是原生 Wayland 客户端。
//! 走 portal 的好处是**快捷键由用户（GNOME）自己决定**：我们只给一个「首选按键」，
//! shell 会弹一次确认框，用户可以在里面改，之后还能在「设置 → 键盘 → 自定义快捷键」里改。
//!
//! 和 capture 一样，这里也是**常驻线程 + 单个 runtime**：ashpd 把 zbus 连接缓存在进程级
//! OnceLock 里，连接跟着第一次创建它的 runtime 走，runtime 一 drop 就再也收不到信号。
//!
//! 主线程通过 mpsc 收事件、通过 tokio 的 unbounded channel 发命令（改名/重绑/退出）。

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::CreateSessionOptions;
use futures_util::StreamExt;

/// 快捷键在 portal 里的稳定标识
const SHORTCUT_ID: &str = "screenshot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// 用户按下了快捷键
    Activated,
    /// 绑定成功，附实际绑定的按键描述（例如 "Ctrl+Alt+S"）
    Bound(String),
    /// 出错（portal 不可用、用户取消等）
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    /// 打开系统的快捷键配置对话框
    Configure,
    /// 结束监听线程
    Stop,
}

pub struct Hotkey {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    pub events: Receiver<HotkeyEvent>,
}

impl Hotkey {
    pub fn configure(&self) {
        let _ = self.tx.send(Command::Configure);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }
}

/// 启动监听线程。失败（portal 不支持）时返回 None，程序照常跑，只是没有全局快捷键
pub fn spawn(preferred: String) -> Option<Hotkey> {
    let (evt_tx, evt_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let spawned = thread::Builder::new()
        .name("shotrs-hotkey".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = evt_tx.send(HotkeyEvent::Failed(format!("运行时: {e}")));
                    return;
                }
            };
            rt.block_on(run(preferred, evt_tx, cmd_rx));
        });
    match spawned {
        Ok(_) => Some(Hotkey {
            tx: cmd_tx,
            events: evt_rx,
        }),
        Err(e) => {
            eprintln!("[shotrs] 快捷键线程起不来: {e}");
            None
        }
    }
}

async fn run(
    preferred: String,
    evt: Sender<HotkeyEvent>,
    mut cmd: tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    loop {
        // ---- 建会话 + 绑定（每次 Rebind 都重新走一遍）
        let Some((portal, session, bound)) = bind(&preferred, &evt).await else {
            // 绑定失败：等命令（可能是用户改了首选按键再试），或直接退出
            match cmd.recv().await {
                Some(Command::Stop) | None => return,
                Some(Command::Configure) => continue,
            }
        };
        let _ = evt.send(HotkeyEvent::Bound(bound));

        let mut activated = match portal.receive_activated().await {
            Ok(s) => s,
            Err(e) => {
                let _ = evt.send(HotkeyEvent::Failed(format!("订阅 Activated 失败: {e}")));
                continue;
            }
        };

        // ---- 等按键 / 等命令
        loop {
            tokio::select! {
                Some(_) = activated.next() => {
                    let _ = evt.send(HotkeyEvent::Activated);
                }
                Some(c) = cmd.recv() => match c {
                    Command::Configure => {
                        if let Err(e) = portal
                            .configure_shortcuts(&session, None, Default::default())
                            .await
                        {
                            let _ = evt.send(HotkeyEvent::Failed(format!("打开快捷键设置失败: {e}")));
                        }
                    }
                    Command::Stop => return,
                },
                else => return,
            }
        }
    }
}

/// 建会话并绑定一个快捷键，返回（portal、会话、绑定到的按键描述）
async fn bind(
    preferred: &str,
    evt: &Sender<HotkeyEvent>,
) -> Option<(GlobalShortcuts, ashpd::desktop::Session<GlobalShortcuts>, String)> {
    let portal = match GlobalShortcuts::new().await {
        Ok(p) => p,
        Err(e) => {
            let _ = evt.send(HotkeyEvent::Failed(format!("portal 不支持全局快捷键: {e}")));
            return None;
        }
    };
    if portal.version() < 1 {
        let _ = evt.send(HotkeyEvent::Failed("portal 全局快捷键版本过低".into()));
        return None;
    }
    let session = match portal.create_session(CreateSessionOptions::default()).await {
        Ok(s) => s,
        Err(e) => {
            let _ = evt.send(HotkeyEvent::Failed(format!("创建会话失败: {e}")));
            return None;
        }
    };

    let shortcut =
        NewShortcut::new(SHORTCUT_ID, "shotrs 截图").preferred_trigger(Some(preferred));
    let request = match portal
        .bind_shortcuts(&session, &[shortcut], None, BindShortcutsOptions::default())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let _ = evt.send(HotkeyEvent::Failed(format!("绑定快捷键失败: {e}")));
            return None;
        }
    };
    // 这一步会弹 GNOME 的确认框，用户可以在里面改键（这就是「自定义」的入口）
    let response = match request.response() {
        Ok(r) => r,
        Err(e) => {
            let _ = evt.send(HotkeyEvent::Failed(format!("快捷键确认失败/被取消: {e}")));
            return None;
        }
    };
    let bound = response
        .shortcuts()
        .first()
        .map(|s| s.trigger_description().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| preferred.to_string());
    Some((portal, session, bound))
}
