//! 把一张图存成 PNG。
//!
//! 「保存」按钮走的就是这里：只有点了按钮（或按 `S`）才落盘，
//! Enter 只进剪贴板，不再偷偷往磁盘写文件。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{png, Bitmap};

/// 写到指定路径（父目录会自动创建；已存在则拒绝覆盖）
pub fn save_png(bitmap: &Bitmap, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败: {e}", parent.display()))?;
    }
    // 截图内容不透明，存成不带 alpha 的 RGB PNG。
    let bytes = png::encode(bitmap)?;
    std::fs::write(path, bytes).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 存进某个目录，文件名用时间戳。返回实际路径
pub fn save_into(bitmap: &Bitmap, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败: {e}", dir.display()))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 同一秒内多次保存时补 -1 -2，避免互相覆盖
    let mut path = dir.join(format!("shotrs-{ts}.png"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("shotrs-{ts}-{n}.png"));
        n += 1;
    }
    save_png(bitmap, &path)?;
    Ok(path)
}
