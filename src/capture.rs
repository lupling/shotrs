//! 取帧层：通过 xdg-desktop-portal 抓一张全桌面截图。
//!
//! 为什么是 portal：GNOME Wayland 下客户端拿不到别人的像素，
//! grim / wlr-screencopy 在 Mutter 上不可用，portal 是唯一免扩展的通道。
//! 代价是每次一个完整往返，实测 ~0.5–1s。
//!
//! ## 为什么必须是一个常驻线程 + 一个 runtime
//!
//! ashpd 把 zbus 的 **session 连接缓存在进程级 `OnceLock`** 里（`ashpd/src/proxy.rs`），
//! 而那条连接的 IO 驱动跟着**第一次**创建它的 tokio runtime 走。早先的写法是「每截一张
//! 就 spawn 一个线程 + 新建一个 runtime」：第一张正常，线程结束 runtime 一 drop，
//! 缓存的连接就再也收不到回包 —— 后面的请求全部卡死（表面现象：点「截图」后窗口变透明、
//! 程序像消失了；实际是停在 Capturing 阶段等一个永远不会来的 D-Bus 回复）。
//!
//! 所以这里做成进程内唯一的取帧工作线程：一个线程、一个 runtime、一条连接，用完不销毁。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

/// 一帧冻结画面（RGBA8，行优先，自上而下）
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// 取帧来源（portal 落盘的临时文件，解码后已删除）
    pub source: PathBuf,
}

/// 单次取帧的总超时
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// 一次取帧任务：等多久再拍 + 回传通道
struct Job {
    hide_delay: Duration,
    reply: Sender<Result<Frame, String>>,
}

struct Worker {
    tx: Sender<Job>,
}

static WORKER: OnceLock<Worker> = OnceLock::new();

fn worker() -> Option<&'static Worker> {
    if let Some(w) = WORKER.get() {
        return Some(w);
    }
    let (tx, rx) = mpsc::channel::<Job>();
    let spawned = thread::Builder::new()
        .name("shotrs-capture".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    // 运行时都建不起来：把后续所有请求都直接失败掉
                    while let Ok(job) = rx.recv() {
                        let _ = job.reply.send(Err(format!("无法创建异步运行时: {e}")));
                    }
                    return;
                }
            };
            rt.block_on(async move {
                while let Ok(job) = rx.recv() {
                    if !job.hide_delay.is_zero() {
                        thread::sleep(job.hide_delay);
                    }
                    let started = Instant::now();
                    let result = match tokio::time::timeout(CAPTURE_TIMEOUT, capture_once()).await {
                        Ok(r) => r,
                        Err(_) => Err(format!(
                            "portal 取帧超时（>{:.0}s）",
                            CAPTURE_TIMEOUT.as_secs_f32()
                        )),
                    };
                    match &result {
                        Ok(f) => eprintln!(
                            "[shotrs] 取帧完成 {}x{}，耗时 {} ms",
                            f.width,
                            f.height,
                            started.elapsed().as_millis()
                        ),
                        Err(e) => eprintln!(
                            "[shotrs] 取帧失败（{} ms）：{e}",
                            started.elapsed().as_millis()
                        ),
                    }
                    let _ = job.reply.send(result);
                }
            });
        });
    if let Err(e) = spawned {
        eprintln!("[shotrs] 取帧线程起不来: {e}");
        return None;
    }
    Some(WORKER.get_or_init(|| Worker { tx }))
}

/// 提交一次取帧任务，结果通过 channel 回来。
///
/// `hide_delay`：先等这么久再发 portal 请求，留给调用方把自身窗口变透明
/// （Wayland 下没有「隐藏窗口」这回事，见 main.rs 里 `hiding()` 的说明）。
pub fn request(hide_delay: Duration) -> Receiver<Result<Frame, String>> {
    let (reply, rx) = mpsc::channel();
    match worker() {
        Some(w) => {
            if let Err(e) = w.tx.send(Job { hide_delay, reply }) {
                eprintln!("[shotrs] 取帧线程不可用: {e}");
            }
        }
        None => {
            eprintln!("[shotrs] 取帧线程不可用");
        }
    }
    rx
}

async fn capture_once() -> Result<Frame, String> {
    use ashpd::desktop::screenshot::Screenshot;

    // interactive(false) = 不弹「点一下屏幕」的对话框
    //
    // modal 必须是 false：我们走的是非交互截图，本来就没有「对话框」，而 modal(true)
    // 会要求 portal 把它的窗口挂到「当前窗口」上 —— 我们没设 parent window identifier
    // （Wayland 下要拿 XDG_ACTIVATION_TOKEN 那套），于是 portal-gnome 会一直打印
    //   Failed to associate portal window with parent window ''
    // 并且**永远不回包**（表现：点截图后窗口消失，卡到超时）。实测必现。
    let shot = Screenshot::request()
        .interactive(false)
        .modal(false)
        .send()
        .await
        .map_err(|e| format!("发起 portal 请求失败: {e}"))?;
    let response = shot
        .response()
        .map_err(|e| format!("portal 取帧被拒或失败: {e}"))?;

    let uri = response.uri().as_str().to_owned();
    let path = uri_to_path(&uri);
    if path.as_os_str().is_empty() {
        return Err(format!("portal 返回了空路径: {uri}"));
    }

    let img = image::open(&path).map_err(|e| format!("解码截图失败 {}: {e}", path.display()))?;
    // GNOME portal 会把截图落到 ~/图片/Screenshot*.png，且不回收；
    // 解码完就删，避免测试跑多了攒一堆文件。失败不影响取帧结果。
    let _ = std::fs::remove_file(&path);
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);

    Ok(Frame {
        width,
        height,
        rgba: rgba.into_raw(),
        source: path,
    })
}

/// portal 给的是 `file:///path/to/x.png`，可能带百分号转义
fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
