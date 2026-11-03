//! 单实例 + 进程间命令通道（unix socket）。
//!
//! 用途：让**全局快捷键**能把「截一张图」的请求交给已经在跑的实例。
//!
//! 为什么不用 portal 的 GlobalShortcuts：`xdg-desktop-portal` 1.21 对非沙箱（host）应用
//! 建 session 时直接报 `NotAllowed: An app id is required` —— host 应用没有 app id，
//! 重启 portal、带 `GIO_LAUNCHED_DESKTOP_FILE` 都不行。
//!
//! 于是走 GNOME 官方支持的经典做法：用户在「设置 → 键盘 → 自定义快捷键」里绑一条命令
//! `shotrs --screenshot`，由 shell 负责按键，我们只负责接收。这样快捷键天然可自定义
//! （就是那个条目），而且不依赖任何 portal 版本。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEvent {
    /// 外部请求截一张图
    Screenshot,
    /// 外部请求把窗口显示出来（`shotrs --show`）
    Show,
}

fn socket_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let uid = unsafe { libc_getuid() };
    PathBuf::from(base).join(format!("shotrs-{uid}.sock"))
}

// 不引入 libc 依赖：直接用 /proc/self 拿 uid 的替代 —— 用环境变量兜底
unsafe fn libc_getuid() -> u32 {
    std::os::unix::fs::MetadataExt::uid(
        &std::fs::metadata("/proc/self").expect("读取 /proc/self"),
    ) as u32
}

/// 监听 socket。已有实例在跑时返回 None（调用方应当把命令转给它）
pub fn listen() -> Option<(PathBuf, Receiver<IpcEvent>)> {
    let path = socket_path();

    // 已有实例？能连上就说明该退出，避免出现两个托盘图标
    if UnixStream::connect(&path).is_ok() {
        eprintln!("[shotrs] 已有实例在运行（{}）", path.display());
        return None;
    }
    let _ = std::fs::remove_file(&path); // 清掉上次异常退出留下的文件

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[shotrs] 监听 {} 失败: {e}", path.display());
            return None;
        }
    };
    let (tx, rx): (Sender<IpcEvent>, Receiver<IpcEvent>) = mpsc::channel();
    let path_for_thread = path.clone();
    thread::Builder::new()
        .name("shotrs-ipc".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let mut line = String::new();
                if BufReader::new(&conn).read_line(&mut line).is_err() {
                    continue;
                }
                let ev = match line.trim() {
                    "show" => IpcEvent::Show,
                    _ => IpcEvent::Screenshot,
                };
                if tx.send(ev).is_err() {
                    break;
                }
                let _ = (&conn).write_all(b"ok\n");
            }
            let _ = std::fs::remove_file(&path_for_thread);
        })
        .ok()?;
    eprintln!("[shotrs] 命令通道：{}", path.display());
    Some((path, rx))
}

/// 给已经在跑的实例发一条命令。返回是否送达
pub fn send(command: &str) -> bool {
    let path = socket_path();
    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            if stream.write_all(command.as_bytes()).is_err() {
                return false;
            }
            let _ = stream.flush();
            true
        }
        Err(_) => false,
    }
}
