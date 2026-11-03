//! 图像同时提供 PNG / BMP 等格式，让远程客户端能选择 Windows 位图格式。
//!
//! Remmina 1.4.43 把 image/png 映射为 PNG，把 image/bmp 映射为 CF_DIB，
//! 仅改变 PNG 的 alpha 并不会让它宣告 CF_DIB。
//! GTK 子进程持有图像直到下一次复制；不可用时用 wl-copy 持有 24 位 BMP。
//! 文件 URI 只表示本地文件，是否传文件取决于远程客户端，不能保证映射成 CF_HDROP。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use image::codecs::bmp::BmpEncoder;
use image::{ExtendedColorType, RgbImage};

use super::{png, Bitmap};

pub enum Payload<'a> {
    /// 图像本身。`max_side > 0` 时先把最长边缩到该值。
    Image {
        bitmap: &'a Bitmap,
        max_side: u32,
        bitmap_only: bool,
    },
    /// 把图片作为「文件」放上剪贴板（需要文件已经落盘）
    File(&'a Path),
    /// 纯文本（一般是图片的绝对路径）
    Text(String),
}

/// 往系统剪贴板放数据。返回实际用了哪种方式，便于在界面上说明
pub fn copy(payload: Payload<'_>) -> Result<String, String> {
    match payload {
        Payload::Image {
            bitmap,
            max_side,
            bitmap_only,
        } => {
            let img = png::scaled_rgb_image(bitmap, max_side)?;
            let (w, h) = img.dimensions();
            if bitmap_only {
                let bmp = encode_bmp(&img)?;
                copy_via_wl_copy_bytes(&bmp, "image/bmp")?;
                return Ok(format!(
                    "远程位图 BMP（24 位）{w}x{h}，{} KiB",
                    bmp.len() / 1024
                ));
            }
            let bytes = png::encode_rgb(&img)?;
            match copy_image_via_gtk(&bytes) {
                Ok(()) => Ok(format!(
                    "PNG + BMP（24 位）{w}x{h}，PNG {} KiB",
                    bytes.len() / 1024
                )),
                Err(e) => {
                    eprintln!("[shotrs] 多格式剪贴板不可用（{e}），改用 image/bmp");
                    let bmp = encode_bmp(&img)?;
                    copy_via_wl_copy_bytes(&bmp, "image/bmp")
                        .map_err(|bmp_error| format!("多格式剪贴板：{e}；BMP：{bmp_error}"))?;
                    Ok(format!("BMP（24 位）{w}x{h}，{} KiB", bmp.len() / 1024))
                }
            }
        }
        Payload::File(path) => {
            let uri = format!("file://{}\r\n", percent_encode_path(path));
            copy_via_wl_copy_bytes(uri.as_bytes(), "text/uri-list")?;
            Ok("本地文件 URI（远程客户端须支持文件传输）".to_string())
        }
        Payload::Text(text) => copy_text(&text).map(|()| "文本（text/plain）".to_string()),
    }
}

fn copy_text(text: &str) -> Result<(), String> {
    if copy_via_wl_copy_bytes(text.as_bytes(), "text/plain;charset=utf-8").is_ok() {
        return Ok(());
    }
    let mut board = arboard::Clipboard::new().map_err(|e| format!("剪贴板不可用: {e}"))?;
    board
        .set_text(text.to_owned())
        .map_err(|e| format!("写入剪贴板失败: {e}"))
}

fn encode_bmp(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    BmpEncoder::new(&mut bytes)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("BMP 编码失败: {e}"))?;
    Ok(bytes)
}

/// GTK 的 set_image 会列出所有可转换格式，并处理 X11 大图的 INCR 传输。
/// 在 XWayland 上持有，供 X11 远程客户端直接读取，也由合成器桥接给 Wayland。
fn copy_image_via_gtk(data: &[u8]) -> Result<(), String> {
    if std::env::var_os("DISPLAY").is_none() {
        return Err("没有 X11 / XWayland 显示服务".to_string());
    }
    let python = if Path::new("/usr/bin/python3").exists() {
        "/usr/bin/python3"
    } else {
        "python3"
    };
    let mut command = Command::new(python);
    command
        .args(["-c", include_str!("clipboard_owner.py")])
        .env("GDK_BACKEND", "x11")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("启动 GTK 剪贴板失败: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("无法打开剪贴板输入管道")?;
    let stdout = child.stdout.take().ok_or("无法打开剪贴板握手管道")?;
    let bytes = data.to_vec();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            stdin
                .write_all(&bytes)
                .map_err(|e| format!("写入 GTK 剪贴板失败: {e}"))?;
            drop(stdin);
            let mut reply = String::new();
            BufReader::new(stdout)
                .read_line(&mut reply)
                .map_err(|e| format!("读取剪贴板握手失败: {e}"))?;
            if reply.trim() == "READY" {
                Ok(())
            } else {
                Err(format!("GTK 剪贴板未就绪: {}", reply.trim()))
            }
        })();
        let _ = tx.send(result);
    });
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| format!("等待剪贴板就绪失败: {e}"))
        .and_then(|r| r);
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    } else {
        // 所有权丢失时 helper 自己退出，回收子进程以免连续截图留下僵尸。
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

/// 用 `wl-copy` 以指定 MIME 类型持有二进制数据
fn copy_via_wl_copy_bytes(data: &[u8], mime: &str) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .arg("--type")
        .arg(mime)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("启动 wl-copy 失败: {e}"))?;
    let written = child
        .stdin
        .as_mut()
        .ok_or("无法打开 wl-copy 输入管道")?
        .write_all(data);
    drop(child.stdin.take());
    if let Err(e) = written {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("写入 wl-copy 失败: {e}"));
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("wl-copy 退出码 {status}")),
        Err(e) => Err(format!("等待 wl-copy 失败: {e}")),
    }
}

/// `text/uri-list` 里的路径要按 RFC 3986 做百分号转义（中文目录名必须转）
fn percent_encode_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "需要桌面会话，会将 SHOTRS_TEST_PNG 指定的图片放到系统剪贴板"]
    fn desktop_clipboard_roundtrip() {
        let path = std::env::var("SHOTRS_TEST_PNG").expect("请设置 SHOTRS_TEST_PNG");
        let input = image::open(path).unwrap().to_rgba8();
        let bitmap = Bitmap::new(
            input.width() as usize,
            input.height() as usize,
            input.into_raw(),
        );
        let expected = png::scaled_rgb_image(&bitmap, 0).unwrap();
        let status = copy(Payload::Image {
            bitmap: &bitmap,
            max_side: 0,
            bitmap_only: false,
        })
        .unwrap();
        assert!(status.starts_with("PNG + BMP"), "{status}");
        for (mime, format) in [
            ("image/png", image::ImageFormat::Png),
            ("image/bmp", image::ImageFormat::Bmp),
        ] {
            let output = Command::new("xclip")
                .args(["-selection", "clipboard", "-out", "-target", mime])
                .output()
                .unwrap();
            assert!(output.status.success());
            let decoded = image::load_from_memory_with_format(&output.stdout, format).unwrap();
            assert_eq!(decoded.to_rgb8(), expected);
        }
    }

    #[test]
    #[ignore = "需要桌面会话，会将 SHOTRS_TEST_PNG 指定的图片以 BMP 放到系统剪贴板"]
    fn desktop_bitmap_clipboard_roundtrip() {
        let path = std::env::var("SHOTRS_TEST_PNG").expect("请设置 SHOTRS_TEST_PNG");
        let max_side = std::env::var("SHOTRS_TEST_MAX_SIDE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let input = image::open(path).unwrap().to_rgba8();
        let bitmap = Bitmap::new(
            input.width() as usize,
            input.height() as usize,
            input.into_raw(),
        );
        let expected = png::scaled_rgb_image(&bitmap, max_side).unwrap();
        let status = copy(Payload::Image {
            bitmap: &bitmap,
            max_side,
            bitmap_only: true,
        })
        .unwrap();
        assert!(status.starts_with("远程位图 BMP"), "{status}");
        let targets = Command::new("xclip")
            .args(["-selection", "clipboard", "-out", "-target", "TARGETS"])
            .output()
            .unwrap();
        assert!(targets.status.success());
        let types = String::from_utf8(targets.stdout).unwrap();
        assert!(types.lines().any(|t| t == "image/bmp"));
        assert!(!types.lines().any(|t| t == "image/png"));
        let output = Command::new("xclip")
            .args(["-selection", "clipboard", "-out", "-target", "image/bmp"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let decoded =
            image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Bmp).unwrap();
        assert_eq!(decoded.to_rgb8(), expected);
    }

    #[test]
    fn clipboard_formats_are_rgb_and_preserve_pixels_with_bmp_row_padding() {
        // 3 像素宽会触发 BMP 每行补齐；上下两行不同可检出倒置、通道顺序错误。
        let bitmap = Bitmap::new(
            3,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8,
                9, 255,
            ],
        );
        let rgb = png::scaled_rgb_image(&bitmap, 0).unwrap();
        let png_bytes = png::encode_rgb(&rgb).unwrap();
        let bmp_bytes = encode_bmp(&rgb).unwrap();
        assert_eq!(png_bytes[25], 2); // PNG IHDR: RGB，无 alpha
        assert_eq!(
            u32::from_le_bytes(bmp_bytes[14..18].try_into().unwrap()),
            40
        );
        assert_eq!(
            u16::from_le_bytes(bmp_bytes[28..30].try_into().unwrap()),
            24
        );
        assert_eq!(u32::from_le_bytes(bmp_bytes[30..34].try_into().unwrap()), 0);
        for (bytes, format) in [
            (&png_bytes, image::ImageFormat::Png),
            (&bmp_bytes, image::ImageFormat::Bmp),
        ] {
            let decoded = image::load_from_memory_with_format(bytes, format).unwrap();
            assert_eq!(decoded.to_rgb8(), rgb);
        }
        assert_eq!(
            png::scaled_rgb_image(&bitmap, 2).unwrap().dimensions(),
            (2, 1)
        );
        assert_eq!(
            png::scaled_rgb_image(&bitmap, 100).unwrap().dimensions(),
            (3, 2)
        );
    }
}
