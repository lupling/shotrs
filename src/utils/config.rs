//! 客户端配置：从本地 JSON 文件读，改了再写回去。
//!
//! 文件位置（按顺序取第一个可用的）：
//!   1. `$SHOTRS_CONFIG`            —— 显式的完整路径，测试/多套配置时方便
//!   2. `$XDG_CONFIG_HOME/shotrs/config.json`
//!   3. `$HOME/.config/shotrs/config.json`
//!
//! 文件不存在、或 JSON 解析失败，都退回默认值并把原因打到 stderr ——
//! 配置坏掉不该让截图工具起不来。

use std::path::{Path, PathBuf};

/// Enter 复制时往剪贴板放什么。远端协议的能力不一样，得让用户能选：
/// 本机图像用 image；远程位图用 bitmap；file 是否传文件取决于客户端；path 仅复制路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardMode {
    /// PNG + BMP（默认）；缺少 GTK helper 时只提供 BMP
    Image,
    /// 仅提供 24 位 BMP，用于远程位图粘贴
    Bitmap,
    /// text/uri-list（远程客户端须另外支持文件传输）
    File,
    /// 纯文本绝对路径
    Path,
}

impl ClipboardMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "图像",
            Self::Bitmap => "远程位图",
            Self::File => "文件",
            Self::Path => "路径",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// 「保存」按钮把图存到哪；clipboard_mode = file/path 时也存这里
    pub save_dir: PathBuf,
    /// Enter 复制到剪贴板时放什么载荷
    pub clipboard_mode: ClipboardMode,
    /// 点窗口右上角「关闭」时：true = 收进托盘（销毁窗口、进程留在后台），false = 直接退出
    pub close_to_tray: bool,
    /// 全局截图快捷键的「首选按键」（XDG shortcuts 语法，如 "CTRL+ALT+S"）。
    /// 实际绑定由 GNOME 的确认框决定，用户可以在里面改
    pub hotkey: String,
    /// 复制到剪贴板时把最长边限制到多少像素（0 = 原尺寸）。
    /// 远端剪贴板链路对大图敏感，缩到 1920 通常更稳
    pub clipboard_max_side: u32,
    /// 截哪块屏（显示器名字，如 `"DP-2"`）。`None` = 跟随控制窗口所在的那块屏。
    ///
    /// 从托盘 / 全局快捷键触发时窗口是刚建出来的，合成器一般把它放在主屏，
    /// 不指定就只能截主屏 —— 先得把窗口拖过去。所以这里存一个显式选择。
    pub capture_display: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_dir: default_save_dir(),
            close_to_tray: true,
            hotkey: "CTRL+ALT+S".to_string(),
            clipboard_mode: ClipboardMode::Image,
            clipboard_max_side: 0,
            capture_display: None,
        }
    }
}

impl Config {
    /// 配置文件路径
    pub fn path() -> PathBuf {
        if let Ok(p) = std::env::var("SHOTRS_CONFIG") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"));
        base.join("shotrs").join("config.json")
    }

    /// 读配置；任何问题都退回默认值
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(cfg) => {
                    eprintln!(
                        "[shotrs] 已加载配置 {}（保存目录 {}）",
                        path.display(),
                        cfg.save_dir.display()
                    );
                    cfg
                }
                Err(e) => {
                    eprintln!(
                        "[shotrs] 配置 {} 解析失败（{e}），使用默认值",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "[shotrs] 无配置文件 {}，使用默认值（保存目录 {}）",
                    path.display(),
                    Self::default().save_dir.display()
                );
                Self::default()
            }
            Err(e) => {
                eprintln!("[shotrs] 读配置 {} 失败（{e}），使用默认值", path.display());
                Self::default()
            }
        }
    }

    /// 写回 JSON（pretty，方便手改）
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建配置目录 {} 失败: {e}", parent.display()))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化配置失败: {e}"))?;
        std::fs::write(&path, text + "\n")
            .map_err(|e| format!("写配置 {} 失败: {e}", path.display()))
    }
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// 默认保存目录：优先本机已有的图片目录（中文桌面是 ~/图片，英文是 ~/Pictures）
fn default_save_dir() -> PathBuf {
    let h = home();
    for name in ["图片", "Pictures"] {
        let dir = h.join(name);
        if dir.is_dir() {
            return dir.join("shotrs");
        }
    }
    Path::new("/tmp").join("shotrs")
}
