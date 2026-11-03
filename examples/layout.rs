//! 调试探针：打印 Mutter DisplayConfig 眼里的真实显示器布局。
//!
//! 三屏 / 竖屏 / 混合 DPI 裁错时先看这里：它给出**逻辑位置、分数缩放、transform**
//! 和当前模式尺寸，正是 `main.rs` 里 `DisplayLayout` 裁剪使用的几何。
//! 和 `examples/monitors.rs`（winit 视角，竖屏会报未旋转尺寸）对照着看最直观。
//!
//! 不开窗口，所以**已有实例在跑时也能用**。
use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

type Spec = (String, String, String, String);
type Mode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
type Monitor = (Spec, Vec<Mode>, HashMap<String, OwnedValue>);
type LogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<Spec>,
    HashMap<String, OwnedValue>,
);

fn main() -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.gnome.Mutter.DisplayConfig",
        "/org/gnome/Mutter/DisplayConfig",
        "org.gnome.Mutter.DisplayConfig",
    )?;
    let (serial, monitors, logical, _props): (
        u32,
        Vec<Monitor>,
        Vec<LogicalMonitor>,
        HashMap<String, OwnedValue>,
    ) = proxy.call("GetCurrentState", &())?;

    let mut current: HashMap<String, (i32, i32)> = HashMap::new();
    for (spec, modes, _) in &monitors {
        if let Some(mode) = modes.iter().find(|m| m.6.contains_key("is-current")) {
            current.insert(spec.0.clone(), (mode.1, mode.2));
        }
    }

    println!("serial = {serial}");
    for (x, y, scale, transform, primary, specs, _) in &logical {
        let scale = *scale as f32;
        let names: Vec<&str> = specs.iter().map(|s| s.0.as_str()).collect();
        println!(
            "logical ({x},{y}) scale={scale:.6} transform={transform} primary={primary} monitors={names:?}"
        );
        for spec in specs {
            if let Some(&(w, h)) = current.get(&spec.0) {
                let (bw, bh) = if transform % 2 == 1 { (h, w) } else { (w, h) };
                println!(
                    "  {} mode={}x{} → buffer={}x{} logical={}x{}",
                    spec.0,
                    w,
                    h,
                    bw,
                    bh,
                    bw as f32 / scale,
                    bh as f32 / scale
                );
            } else {
                println!("  {} mode=???", spec.0);
            }
        }
    }
    Ok(())
}
