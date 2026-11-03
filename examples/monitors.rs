//! 调试探针：打印 winit 眼里的显示器信息（尺寸 / 位置 / 整数缩放）。
//!
//! 位置报的是「逻辑坐标 × 该屏整数缩放」，见 main.rs 的 `monitor_logical_origin`。
//! 混合缩放排查时拿它对一下 ==portal 位图== 的尺寸就知道帧是按哪一档渲染的。
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};

struct App;
impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        for m in el.available_monitors() {
            let p = m.position();
            let sz = m.size();
            println!(
                "monitor {:?} size={}x{} pos=({},{}) scale={}",
                m.name(),
                sz.width,
                sz.height,
                p.x,
                p.y,
                m.scale_factor()
            );
        }
        el.exit();
    }
    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        _: winit::event::WindowEvent,
    ) {
    }
}

fn main() {
    let el = EventLoop::new().unwrap();
    let mut app = App;
    el.run_app(&mut app).unwrap();
}
