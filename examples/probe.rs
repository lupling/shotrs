//! 调试探针：走 portal 抓一张整桌面，打印真实像素尺寸。
//!
//! 用来回答「portal 到底给了多大一张图」，不跟 winit/窗口打交道
//! （已有实例在跑时主程序进不去，这个可以直接 `cargo run --example probe`）。
//! `SHOTRS_DUMP=1` 时另存一份到 /tmp/probe-frame.png。
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use ashpd::desktop::screenshot::Screenshot;
    let shot = Screenshot::request()
        .interactive(false)
        .modal(false)
        .send()
        .await
        .expect("发起 portal 请求失败");
    let response = shot.response().expect("portal 取帧失败");
    let uri = response.uri().as_str().to_owned();
    println!("uri = {uri}");
    let raw = uri.strip_prefix("file://").unwrap_or(&uri);
    let mut out: Vec<u8> = Vec::new();
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out).into_owned());
    let img = image::open(&path).expect("解码失败");
    println!("frame = {}x{}", img.width(), img.height());
    // portal 会把截图落到 ~/图片/Screenshot*.png 且不回收，和取帧层一样用完就删
    let _ = std::fs::remove_file(&path);
    if std::env::var("SHOTRS_DUMP").is_ok() {
        let rgba = img.to_rgba8();
        image::save_buffer(
            "/tmp/probe-frame.png",
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        println!("已存 /tmp/probe-frame.png");
    }
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
