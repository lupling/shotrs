//! shotrs — 截图工具的最小 Rust 实现（技术验证用）
//!
//! 验证四件事：
//!   1. winit 能否开一个真正的单屏全屏窗口
//!   2. ashpd 能否从 portal 取到一帧，拿到的是什么分辨率
//!   3. egui 能否把这一帧铺满并在上面实时画箭头（GPU 合成，不再是 CPU 光栅）
//!   4. fcitx5 的中文输入在 text-input-v3 下能不能用
//!
//! 用法：
//!   cargo run --release
//!   小窗口里点「截图」→ 全屏框选 → 工具栏/快捷键选工具标注 → Enter 导出 → 回到小窗口
//!
//! 标注工具是插件：协议在 `plugins/mod.rs`，实现按工具分文件放在 `plugins/` 下，
//! 主程序只认 `Tool` / `Draft` / `InlineEditor` / `Shape` 四个 trait。
//!
//! 流程要点：点「截图」后先把自己变成全透明（Wayland 下 set_visible 是空实现），
//! 再走 portal 取帧，取到后才进全屏。

mod capture;
mod demo;
mod plugins;
mod scene;
mod hotkey;
mod ipc;
mod selection;
mod toolbar;
mod tray;
mod utils;

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2};
use egui_glow::glow;
use egui_glow::painter::Painter;
use egui_glow::glow::HasContext;
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentContext, NotCurrentGlContext,
    PossiblyCurrentContext, PossiblyCurrentGlContext,
};
use glutin::surface::GlSurface;
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Fullscreen, Window, WindowId, WindowLevel};

use capture::Frame;
use plugins::{Draft, InlineEditor};
use scene::Scene;
use hotkey::Hotkey;
use ipc::IpcEvent;
use toolbar::Toolbox;
use tray::{TrayEvent, TrayHandle};
use utils::config::{ClipboardMode, Config};
use utils::{clipboard, save, Bitmap};

// ---------------------------------------------------------------- 常量

const DIM: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 150);
const ACCENT: Color32 = Color32::from_rgb(255, 64, 64);
const BG: Color32 = Color32::from_rgb(26, 27, 32);
/// 选中标注右上角「删除」按钮的半径（逻辑点）
const DELETE_ICON_R: f32 = 9.0;
const FG_DIM: Color32 = Color32::from_rgb(150, 155, 170);

/// Wayland 上的应用标识：GNOME 用它把窗口和 `shotrs.desktop` 关联起来（图标/任务栏分组）
const APP_ID: &str = "shotrs";
/// 内嵌的图标（1024 太大，256 足够窗口图标；.desktop 用 assets/ 下的大图）
const ICON_PNG: &[u8] = include_bytes!("../assets/shotrs-256.png");

/// 控制窗口的逻辑尺寸
const CONTROL_SIZE: LogicalSize<f64> = LogicalSize::new(480.0, 424.0);

/// 取帧前等多久：让自己的「透明帧」被合成器真正合成一次，再让 portal 去拍。
/// 无需很久 —— 隐身靠的是下面 hiding() 里的 alpha=0，不是 unmap。
const HIDE_DELAY: Duration = Duration::from_millis(300);
const HIDE_DELAY_STARTUP: Duration = Duration::from_millis(600);

// ---------------------------------------------------------------- 状态

/// GL 状态。
///
/// **上下文跨窗口复用**：最小化只是销毁窗口，GL context 和 egui 的 Painter（连同所有纹理、
/// 字体图集、着色器）都留着；恢复时只新建窗口 surface 再把上下文 make_current 上去。
/// 这样恢复没有黑屏、也不用重传字体图集 —— 早先「每次重建上下文」时 painter 手里一张纹理
/// 都没有，而 egui 又认为字体图集早已上传（`set_fonts` 对相同定义会跳过），于是整帧什么都
/// 画不出来，只剩清屏的黑色。
struct GlState {
    gl: Arc<glow::Context>,
    config: glutin::config::Config,
    /// 有窗口时的 (surface, current context)
    live: Option<(Surface<WindowSurface>, PossiblyCurrentContext)>,
    /// 没有窗口时把上下文挂起在这里
    idle: Option<NotCurrentContext>,
}

/// 冻结帧的一个瓦片：纹理边长受 GL_MAX_TEXTURE_SIZE 限制，
/// 大图必须切成多块分别上传、分别绘制（本机 2560×1600 超出 2048 上限）
struct Tile {
    tex: egui::TextureHandle,
    /// 在整幅帧内的偏移与尺寸（帧像素坐标）
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

/// 拿到成品图之后做什么
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// 只进剪贴板（Enter）
    Copy,
    /// 存到配置里的目录（「保存」按钮 / S）
    Save,
}

#[derive(Clone, PartialEq, Eq)]
enum Phase {
    /// 小控制窗口（空闲）
    Idle,
    /// 已隐藏窗口、正在取帧；取到后进全屏
    Capturing,
    /// 全屏框选
    Select,
    /// 全屏标注
    Annotate,
}

struct App {
    window: Option<Arc<Window>>,
    gl: Option<GlState>,
    painter: Option<Painter>,
    ctx: egui::Context,
    egui_state: Option<egui_winit::State>,

    capture_rx: Option<Receiver<Result<Frame, String>>>,
    /// 已取到但还没裁剪/上传的帧（等窗口进全屏、尺寸生效）
    pending: Option<Frame>,
    pending_frames: u32,
    /// 进全屏前记下的控制窗口物理尺寸，用来判断「全屏尺寸还没生效」
    control_size: (u32, u32),
    /// 冻结帧的像素尺寸（瓦片坐标基于此，绘制时要换算成逻辑点）
    frame_size: (usize, usize),
    /// portal 位图用的缩放：GNOME 把整桌面按会话里最大的分数缩放渲染成一张位图。
    /// 我们能在全屏时观察到「当前屏的分数缩放」，取历史最大值当它
    /// （起点是控制窗口所在的屏，通常就是主屏 = 最大缩放）。
    desktop_scale: f32,
    tiles: Vec<Tile>,

    phase: Phase,
    monitor_note: String,
    image_note: String,

    /// 工具栏：插件实例 + 按钮栏 + 属性面板（都在 toolbar 模块里）
    toolbox: Toolbox,
    /// 正在拖的草稿（箭头之类）
    draft: Option<Box<dyn Draft>>,
    /// 正在编辑的内联控件（文字）
    editor: Option<Box<dyn InlineEditor>>,
    /// 已落定的标注，按落定顺序
    scene: Scene,

    sel_start: Option<Pos2>,
    sel: Option<Rect>,
    /// 进入 Capturing 的时刻（看门狗用）
    capturing_since: Option<Instant>,
    /// 帧到手后开始等「全屏尺寸生效」的时刻
    pending_since: Option<Instant>,
    /// 复现用：还有几次「自动再截一张」（SHOTRS_LOOP=n）
    loop_remaining: u32,
    /// 复现模式：跑完就退出（自检用）
    loop_mode: bool,
    /// 卡在 Capturing 时的诊断计数
    stuck_frames: u32,
    /// 正在进行的选区拖动（平移 / 缩放 / 拉新的）
    grab: Option<selection::Grab>,
    /// 拖动开始时的选区（缩放/平移都以它为基准，避免每帧叠加）
    sel_base: Option<Rect>,
    /// 正在拖动的已有标注：(下标, 上一帧指针位置)
    drag_shape: Option<(usize, Pos2)>,
    /// 正在缩放已有标注：(下标, 把手, 按下时的包围盒)
    resize_shape: Option<(usize, selection::Handle, Rect)>,

    /// 待执行的成品动作：Enter = 进剪贴板，「保存」= 落盘
    action: Option<Action>,
    /// 动作那一帧先把覆盖层（工具栏/HUD/提示/输入框）藏起来，读回的才是干净成品
    overlay_off: bool,
    smoke: bool,
    /// 导出后直接退出（自检/演示模式用），否则回控制窗口
    exit_after_export: bool,
    demo: Option<demo::Script>,
    status: String,
    quit: bool,

    /// 顶部栏托盘（可选：没有 SNI 宿主时为 None）
    tray: Option<TrayHandle>,
    /// 全局快捷键监听（可选：portal 不支持时为 None）
    hotkey: Option<Hotkey>,
    /// 快捷键当前绑定到的按键描述（显示用）
    hotkey_bound: String,
    /// 外部命令通道（`shotrs --screenshot` 之类）
    ipc_rx: Option<std::sync::mpsc::Receiver<IpcEvent>>,
    /// socket 路径（显示给用户，方便写自定义快捷键）
    ipc_path: String,
    /// 首次启动标记（resumed 可能被多次调用）
    started: bool,
    /// 本次截图是从托盘点/按快捷键发起的（窗口本来是销毁状态）：
    /// 导出后应该回托盘，而不是把控制窗口弹出来
    return_to_tray: bool,
    /// 显示器缓存：(名字, 带分辨率的文案)。托盘菜单要用，而窗口销毁后查不到显示器
    displays: Vec<(String, String)>,

    /// 控制窗口里点出来的待处理动作（这些需要 event_loop，放到 about_to_wait 里做）
    pending_minimize: bool,
    pending_close: bool,
    pending_show: bool,
    /// 客户端配置（保存目录等），启动时从本地 JSON 读
    config: Config,
    /// 控制窗口里那个路径输入框的编辑缓冲
    save_dir_text: String,

    last_tick: Instant,
    frame_ms: VecDeque<f32>,

    // 临时调试：SHOTRS_AUTOSHOT=1 时自动开始取帧（不自动导出），用于无鼠标验证显示效果
    auto_shot: bool,
    auto_shot_ticks: u32,
    logged_layout: bool,
    dumped_backbuffer: bool,
    /// 冻结帧上屏后经过的帧数（调试 dump 用）
    ready_frames: u32,
    /// 空闲帧计数（调试 dump 控制窗口用）
    idle_frames: u32,
}

impl App {
    fn new(ipc_rx: Option<std::sync::mpsc::Receiver<IpcEvent>>, ipc_path: String) -> Self {
        let smoke = std::env::var("SHOTRS_SMOKE").is_ok();
        let demo_on = std::env::var("SHOTRS_DEMO").is_ok();
        let config = Config::load();
        let loop_n: u32 = std::env::var("SHOTRS_LOOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Self {
            window: None,
            gl: None,
            painter: None,
            ctx: egui::Context::default(),
            egui_state: None,

            capture_rx: None,
            pending: None,
            pending_frames: 0,
            control_size: (0, 0),
            frame_size: (0, 0),
            desktop_scale: 1.0,
            tiles: Vec::new(),

            phase: Phase::Idle,
            monitor_note: String::new(),
            image_note: String::new(),

            toolbox: Toolbox::new(),
            draft: None,
            editor: None,
            scene: Scene::default(),

            sel_start: None,
            sel: None,
            capturing_since: None,
            pending_since: None,
            loop_remaining: loop_n,
            loop_mode: std::env::var("SHOTRS_LOOP").is_ok(),
            stuck_frames: 0,
            grab: None,
            sel_base: None,
            drag_shape: None,
            resize_shape: None,

            action: None,
            overlay_off: false,
            smoke,
            exit_after_export: smoke || demo_on,
            demo: demo_on.then(demo::Script::new),
            status: String::from("点「截图」开始"),
            quit: false,

            tray: None,
            hotkey: None,
            hotkey_bound: String::new(),
            ipc_rx,
            ipc_path,
            started: false,
            return_to_tray: false,
            displays: Vec::new(),
            pending_minimize: false,
            pending_close: false,
            pending_show: false,
            config: config.clone(),
            save_dir_text: config.save_dir.display().to_string(),

            last_tick: Instant::now(),
            frame_ms: VecDeque::with_capacity(128),

            auto_shot: std::env::var("SHOTRS_AUTOSHOT").is_ok(),
            auto_shot_ticks: 0,
            logged_layout: false,
            dumped_backbuffer: false,
            ready_frames: 0,
            idle_frames: 0,
        }
    }

    // ------------------------------------------------ GL / 窗口初始化

    /// 创建控制窗口（可重复调用：最小化会销毁窗口，显示时再建一次）
    ///
    /// 恢复路径会**复用已经存在的 GL 上下文和 Painter**，只新建窗口 surface ——
    /// 纹理、字体图集、着色器全都还在，所以又快又不会黑屏。
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let t0 = Instant::now();
        if self.gl.is_some() {
            self.attach_surface(event_loop, t0);
            return self.window.is_some();
        }
        let template = ConfigTemplateBuilder::new().with_alpha_size(8);
        let attrs = Window::default_attributes()
            .with_title("shotrs")
            .with_name(APP_ID, APP_ID)
            .with_decorations(false)
            // 必须声明成透明窗口：winit 否则会给 surface 设 opaque region，
            // 合成器就无视 alpha，取帧期间的「隐身」也就失效了。
            .with_transparent(true)
            .with_resizable(false)
            .with_inner_size(CONTROL_SIZE)
            .with_window_level(WindowLevel::AlwaysOnTop);

        let (window, gl_config) = DisplayBuilder::new()
            .with_window_attributes(Some(attrs))
            .build(event_loop, template, |configs| {
                configs.into_iter().next().expect("没有可用的 GL 配置")
            })
            .expect("创建窗口失败");
        let window = window.expect("窗口未创建");

        let gl_display = gl_config.display();
        let handle = window
            .window_handle()
            .expect("拿不到 window handle")
            .as_raw();

        let ctx_attrs = ContextAttributesBuilder::new().build(Some(handle));
        let fallback_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(None))
            .build(Some(handle));

        let not_current = unsafe {
            gl_display
                .create_context(&gl_config, &ctx_attrs)
                .or_else(|_| gl_display.create_context(&gl_config, &fallback_attrs))
                .expect("创建 GL 上下文失败")
        };

        let surface_attrs = window
            .build_surface_attributes(Default::default())
            .expect("构建 surface 属性失败");
        let surface = unsafe { gl_display.create_window_surface(&gl_config, &surface_attrs) }
            .expect("创建 surface 失败");
        let context = not_current
            .make_current(&surface)
            .expect("切换当前上下文失败");

        let gl = unsafe { glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s)) };
        let gl = Arc::new(gl);
        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::new(1).unwrap()));

        let painter = Painter::new(gl.clone(), "", None, false).expect("初始化 egui 绘制器失败");
        eprintln!("[shotrs] max_texture_side = {}", painter.max_texture_side());
        self.painter = Some(painter);
        self.gl = Some(GlState {
            gl,
            config: gl_config,
            live: Some((surface, context)),
            idle: None,
        });

        // 注意：这里**必须**每次建窗都重装字体。
        //
        // 新建窗口 = 新的 GL context + 新的 egui_glow::Painter，它手里一张纹理都没有；
        // 而 egui 的纹理管理器以为字体图集早就上传过了，不会再发增量。
        // egui_glow 连纯色三角形都要采样那张图集（white_uv），图集缺失 → 整帧什么都画不出来，
        // 只剩 glClear 的黑 —— 这就是「从托盘恢复后是黑屏窗口」的真正原因。
        // set_fonts 会重建图集并重新发一次完整纹理增量，代价约 100ms。
        install_cjk_fonts(&self.ctx);
        let mut state = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        // 关键：允许 IME，否则中文输入法根本不会激活
        state.set_allow_ime(true);
        self.egui_state = Some(state);

        // Wayland 下窗口刚创建时可能还查不到 monitor（实测会返回 None），
        // 这里只是 HUD 文案，取帧裁剪会在全屏之后重新查。
        self.monitor_note = describe_monitor(&window);

        let inner = window.inner_size();
        self.control_size = (inner.width, inner.height);
        eprintln!("[shotrs] 控制窗口 {}x{}，{}", inner.width, inner.height, self.monitor_note);

        // 窗口图标：X11/XWayland 会真的显示；Wayland 下窗口没有图标概念，
        // 那儿靠 app_id 去匹配 shotrs.desktop 里的 Icon=shotrs（见 assets/）。
        if let Some(icon) = load_icon() {
            window.set_window_icon(Some(icon));
        }
        eprintln!("[shotrs] app_id = {}", APP_ID);

        let window = Arc::new(window);
        self.window = Some(window.clone());
        self.refresh_displays(&window);
        self.phase = Phase::Idle;

        // 关键：新窗口不会自己产生 RedrawRequested。不主动请求的话，这块 surface
        // 一直没被画过 —— 从托盘恢复时看到的就是「黑屏窗口」，直到鼠标划过才突然出现。
        window.request_redraw();

        eprintln!("[shotrs] 窗口就绪，耗时 {} ms", t0.elapsed().as_millis());
        if self.smoke || self.loop_remaining > 0 {
            // 自检 / 连拍复现：不用人点按钮，直接开拍
            self.start_capture(&window);
        }
        true
    }

    /// 恢复窗口：复用现有 GL 上下文/Painter，只新建窗口和 surface
    fn attach_surface(&mut self, event_loop: &ActiveEventLoop, t0: Instant) {
        let attrs = Window::default_attributes()
            .with_title("shotrs")
            .with_name(APP_ID, APP_ID)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_inner_size(CONTROL_SIZE)
            .with_window_level(WindowLevel::AlwaysOnTop);
        let Ok(window) = event_loop.create_window(attrs) else {
            eprintln!("[shotrs] 恢复窗口失败");
            return;
        };

        let Some(gl) = self.gl.as_mut() else { return };
        let surface_attrs = match window.build_surface_attributes(Default::default()) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[shotrs] 构建 surface 属性失败: {e}");
                return;
            }
        };
        let display = gl.config.display();
        let surface = match unsafe { display.create_window_surface(&gl.config, &surface_attrs) } {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[shotrs] 创建 surface 失败: {e}");
                return;
            }
        };
        let Some(idle) = gl.idle.take() else {
            eprintln!("[shotrs] GL 上下文状态异常（idle 为空）");
            return;
        };
        let context = match idle.make_current(&surface) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[shotrs] 切换上下文失败: {e}");
                return;
            }
        };
        let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::new(1).unwrap()));
        gl.live = Some((surface, context));

        // 上下文复用，纹理都在 —— 这里不用重装字体（set_fonts 对相同定义会跳过，
        // 而我们已经不需要它重建图集了）
        let mut state = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        state.set_allow_ime(true);
        self.egui_state = Some(state);

        self.monitor_note = describe_monitor(&window);
        let inner = window.inner_size();
        self.control_size = (inner.width, inner.height);
        eprintln!(
            "[shotrs] 控制窗口恢复 {}x{}，耗时 {} ms（复用 GL 上下文）",
            inner.width,
            inner.height,
            t0.elapsed().as_millis()
        );

        let window = Arc::new(window);
        self.window = Some(window.clone());
        self.refresh_displays(&window);
        self.phase = Phase::Idle;
        window.request_redraw();
        self.sync_tray();
    }

    /// 销毁窗口（= 「最小化到托盘」）：释放 GL 资源后进程继续留在后台，
    /// 只能靠托盘菜单或全局快捷键把窗口建回来。
    ///
    /// 为什么不直接 `set_minimized(true)`：Wayland 下**无法恢复**（winit 原文：
    /// `Unminimizing is ignored on Wayland.`），那样窗口就再也回不来了。
    fn destroy_window(&mut self) {
        if self.window.is_none() {
            return;
        }
        self.pending = None;
        self.pending_since = None;
        self.capture_rx = None;
        self.draft = None;
        self.editor = None;
        self.scene.clear();
        self.tiles.clear();
        self.frame_size = (0, 0);
        self.sel = None;
        self.sel_base = None;
        self.grab = None;
        self.phase = Phase::Idle;
        self.overlay_off = false;
        // 只放掉「和窗口绑定」的东西：surface + egui_state + window。
        // GL 上下文和 Painter（连同纹理/字体图集/着色器）留着，恢复时才不用重建。
        if let Some(gl) = self.gl.as_mut() {
            if let Some((surface, context)) = gl.live.take() {
                match context.make_not_current() {
                    Ok(idle) => gl.idle = Some(idle),
                    Err(e) => eprintln!("[shotrs] 挂起 GL 上下文失败: {e}"),
                }
                drop(surface);
            }
        }
        self.egui_state = None;
        self.window = None;
        self.logged_layout = false;
        self.ready_frames = 0;
        self.idle_frames = 0;
        self.status = String::from("已收进托盘（点顶部栏图标或按快捷键）");
        eprintln!("[shotrs] 窗口已销毁，进程留在后台（托盘 + 全局快捷键仍可用）");
        self.sync_tray();
    }

    /// 关闭按钮：按配置决定「收进托盘」还是「退出」
    fn request_close(&mut self) {
        if self.config.close_to_tray && self.tray.is_some() {
            self.destroy_window();
        } else {
            self.quit = true;
        }
    }

    fn sync_tray(&self) {
        if let Some(tray) = self.tray.as_ref() {
            let label = if self.hotkey_bound.is_empty() {
                "未绑定（GNOME 自定义快捷键）".to_string()
            } else {
                self.hotkey_bound.clone()
            };
            tray.sync(
                self.config.close_to_tray,
                self.window.is_some(),
                &label,
                self.config.capture_display.clone(),
                self.displays.clone(),
            );
        }
    }

    /// 缓存显示器列表（托盘菜单要用；窗口销毁后主线程就查不到 available_monitors 了）
    fn refresh_displays(&mut self, window: &Window) {
        let mut out: Vec<(String, String)> = Vec::new();
        for m in window.available_monitors() {
            let name = m.name().unwrap_or_else(|| "?".into());
            // 改过缩放/插拔之后 winit 会把同一块屏报两遍（实测），按名字去重
            if out.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let sz = m.size();
            out.push((name.clone(), format!("{name} {}×{}", sz.width, sz.height)));
        }
        self.displays = out;
    }

    /// 截图要全屏到哪块屏。
    ///
    /// 优先级：`SHOTRS_MONITOR`（调试） > 配置的「截图屏幕」 > 控制窗口当前所在的屏。
    /// 为什么需要显式指定：从托盘/快捷键触发时窗口是刚建出来的，合成器一般把它放在主屏，
    /// 副屏就永远截不到 —— 以前只能先把控制窗口拖到副屏。
    fn pick_monitor(&self, window: &Window) -> Option<winit::monitor::MonitorHandle> {
        let want = env_monitor().or_else(|| self.config.capture_display.clone());
        if let Some(want) = want {
            match window
                .available_monitors()
                .find(|m| m.name().map(|n| n.contains(&want)).unwrap_or(false))
            {
                Some(m) => return Some(m),
                None => eprintln!("[shotrs] 找不到显示器「{want}」，改用控制窗口所在的屏"),
            }
        }
        window.current_monitor()
    }

    /// 托盘 / 全局快捷键请求截图：没有窗口就先建回来，再走正常流程
    fn request_screenshot(&mut self, event_loop: &ActiveEventLoop) {
        if !matches!(self.phase, Phase::Idle) {
            return; // 已经在截图流程里，忽略重复请求
        }
        // 窗口本来不在（在托盘）→ 截完也要回托盘，不把控制窗口弹出来
        let from_tray = self.window.is_none();
        self.return_to_tray =
            from_tray && !(self.smoke || self.demo.is_some() || self.loop_mode || self.auto_shot);
        if self.window.is_none() && !self.create_window(event_loop) {
            self.return_to_tray = false;
            return;
        }
        self.status = String::from("点「截图」开始");
        self.sync_tray();
        if let Some(w) = self.window.clone() {
            self.start_capture(&w);
        }
    }

    // ------------------------------------------------ 截图流程

    /// 点「截图」：先把自己变成全透明（见下），再起后台线程取帧
    fn start_capture(&mut self, window: &Window) {
        if matches!(self.phase, Phase::Capturing | Phase::Select | Phase::Annotate) {
            return;
        }
        let inner = window.inner_size();
        self.control_size = (inner.width, inner.height);
        self.status = String::from("正在取帧…（约 1s）");
        // 注意：这里不能用 set_visible / set_minimized 来「躲开镜头」——
        //   * set_visible 在 winit 的 Wayland 后端是空实现（源码里就写着 Not possible）
        //   * set_minimized 只能最小化，无法恢复
        // 真正管用的是切到 Capturing 阶段后什么都不画 + 清屏成全透明（见 ui() / hiding()），
        // 前提是窗口创建时带 with_transparent(true)，否则合成器会按 opaque 处理。
        self.phase = Phase::Capturing;
        self.capturing_since = Some(Instant::now());
        let delay = if self.smoke || self.auto_shot || self.demo.is_some() {
            HIDE_DELAY_STARTUP
        } else {
            HIDE_DELAY
        };
        self.capture_rx = Some(capture::request(delay));
    }

    /// 取帧结果到了：先记下帧，再进全屏，等尺寸生效后由 resolve_pending 处理
    fn poll_capture(&mut self, window: &Window) {
        let Some(rx) = self.capture_rx.take() else {
            return;
        };

        let mut result = None;
        loop {
            match rx.try_recv() {
                Ok(r) => {
                    result = Some(r);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // 后台线程 panic 且没发结果：必须给用户一个交代，不能干等
                    result = Some(Err("取帧线程异常终止".to_string()));
                    break;
                }
            }
        }

        let Some(result) = result else {
            // 还在跑，把接收端放回去
            self.capture_rx = Some(rx);
            return;
        };

        match result {
            Ok(frame) => {
                let full = (frame.width, frame.height);
                self.image_note = format!(
                    "取帧 {}x{}（整桌面 {}x{}；portal 临时文件 {} 已清理）",
                    frame.width,
                    frame.height,
                    full.0,
                    full.1,
                    frame
                        .source
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "?".into())
                );
                eprintln!("[shotrs] {}", self.image_note);
                self.pending = Some(frame);
                self.pending_since = Some(Instant::now());
                self.pending_frames = 0;
                self.enter_editor(window);
            }
            Err(e) => {
                eprintln!("[shotrs] 取帧失败: {e}");
                self.status = format!("取帧失败：{e}");
                self.back_to_control(window);
            }
        }
    }

    /// 进全屏编辑器：把窗口显示出来并切全屏（取帧已完成，此时切换不会影响画面）
    fn enter_editor(&mut self, window: &Window) {
        let monitor = self.pick_monitor(window);
        eprintln!(
            "[shotrs] 全屏到 {}",
            monitor
                .as_ref()
                .and_then(|m| m.name())
                .unwrap_or_else(|| "（合成器决定）".into())
        );
        window.set_window_level(WindowLevel::Normal);
        window.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
        self.phase = Phase::Capturing;
        self.monitor_note = describe_monitor(window);
        window.request_redraw();
    }

    /// 回到小控制窗口（导出完成 / 取消 / 出错）
    fn back_to_control(&mut self, window: &Window) {
        self.pending = None;
        self.pending_frames = 0;
        self.sel = None;
        self.sel_start = None;
        self.draft = None;
        self.editor = None;
        self.grab = None;
        self.sel_base = None;
        self.scene.clear();
        self.tiles.clear();
        self.toolbox.set_canvas(None);
        self.frame_size = (0, 0);
        self.ready_frames = 0;
        self.dumped_backbuffer = false;
        self.logged_layout = false;
        self.frame_ms.clear();
        self.phase = Phase::Idle;
        self.capturing_since = None;
        self.pending_since = None;
        self.pending_since = None;
        self.overlay_off = false;

        if self.exit_after_export && !self.loop_mode {
            // 自检/演示模式导完就退，方便脚本判断
            self.quit = true;
            return;
        }

        // 从托盘 / 全局快捷键发起的截图：窗口本来就不该存在，导完直接销毁回托盘。
        // 否则会先退回小窗口（Mutter 还会放一段退全屏动画），看起来就像「截完图冒出一个客户端」。
        if self.return_to_tray {
            self.return_to_tray = false;
            let status = std::mem::take(&mut self.status);
            let note = std::mem::take(&mut self.image_note);
            self.destroy_window();
            self.status = if status.is_empty() {
                String::from("已收进托盘（点顶部栏图标或按快捷键）")
            } else {
                format!("{status}（已收进托盘）")
            };
            self.image_note = note;
            eprintln!("[shotrs] 本次截图来自托盘，导完直接回托盘（不弹控制窗口）");
            return;
        }

        window.set_fullscreen(None);
        window.set_window_level(WindowLevel::AlwaysOnTop);
        window.request_redraw();

        if self.loop_remaining > 0 {
            self.loop_remaining -= 1;
            eprintln!("[shotrs] 复现循环：再截一张（剩 {} 次）", self.loop_remaining);
            self.start_capture(window);
        } else if self.loop_mode {
            eprintln!("[shotrs] 连拍复现跑完，退出");
            self.quit = true;
        }
    }

    /// 全屏尺寸生效后，把 pending 帧裁成当前屏、切瓦片、进框选阶段
    fn resolve_pending(&mut self, window: &Window) {
        let Some(mut frame) = self.pending.take() else {
            return;
        };

        // 「全屏生效了吗」= 窗口尺寸是否已经等于当前显示器的尺寸。
        //
        // 不能用「比记录的控制窗口大」来判断：上一张截图刚结束时 inner_size 还是上一块屏的
        // 全屏尺寸，于是第二次点「截图」会被误判成「已经全屏」，随后错误地退回整桌面。
        let inner = window.inner_size();
        let monitor_size = window
            .current_monitor()
            .map(|m| (m.size().width, m.size().height));
        let fullscreen_applied = match monitor_size {
            Some((mw, mh)) => inner.width == mw && inner.height == mh,
            // 拿不到显示器信息时退回弱判断
            None => inner.width > self.control_size.0 || inner.height > self.control_size.1,
        };
        let waited = self.pending_since.map(|t| t.elapsed()).unwrap_or_default();
        if !fullscreen_applied && waited < Duration::from_millis(1500) {
            // 每 15 帧重发一次全屏请求，自愈「请求被合成器忽略」的情况
            if self.pending_frames % 15 == 0 {
                if self.pending_frames > 0 {
                    eprintln!(
                        "[shotrs] 全屏未生效（inner={}x{} 目标={:?}），重试",
                        inner.width, inner.height, monitor_size
                    );
                }
                window.set_fullscreen(Some(Fullscreen::Borderless(self.pick_monitor(window))));
            }
            self.pending = Some(frame);
            self.pending_frames += 1;
            return;
        }

        let full = (frame.width, frame.height);
        if !fullscreen_applied {
            // 等了半天全屏尺寸都没生效（某些合成器不支持/被拒）：不裁剪，直接铺整桌面。
            // 用旧的小窗尺寸去裁会把“屏幕一角”当成一屏，宁可退化成整桌面。
            let msg = "全屏尺寸未生效，本帧改用整桌面".to_string();
            eprintln!("[shotrs] {msg}");
            self.status = msg;
        } else {
            match monitor_bitmap_rect(window, self.desktop_scale, full) {
                Some((cx, cy, cw, ch)) => {
                    match crop_rgba(&frame.rgba, frame.width, frame.height, cx, cy, cw, ch) {
                        Some(cropped) => {
                            frame.rgba = cropped;
                            frame.width = cw;
                            frame.height = ch;
                            // 帧按「会话最大缩放」渲染，当前屏缩放更小时裁出来的区域是被放大的
                            // （主屏 1.333 + 副屏 1.5 → 主屏裁到 2880×1800，而屏幕只有 2560×1600）。
                            // 多出来的像素是插值，缩回该屏的设备像素才是截图该有的尺寸。
                            // 方向只会是「缩小」（桌面缩放 ≥ 当前屏缩放）。
                            let (sw, sh) = (inner.width as usize, inner.height as usize);
                            if sw > 0
                                && sh > 0
                                && (cw != sw || ch != sh)
                                && frame.rgba.len() == cw * ch * 4
                            {
                                let started = Instant::now();
                                if let Some(buf) =
                                    resize_rgba(std::mem::take(&mut frame.rgba), (cw, ch), (sw, sh))
                                {
                                    eprintln!(
                                        "[shotrs] 帧 {cw}x{ch} → {sw}x{sh}（缩回该屏设备像素，桌面缩放大于当前屏缩放，{} ms）",
                                        started.elapsed().as_millis()
                                    );
                                    frame.rgba = buf;
                                    frame.width = sw;
                                    frame.height = sh;
                                }
                            }
                        }
                        None => {
                            let msg = format!(
                                "裁剪区域 {cw}x{ch}@({cx},{cy}) 超出取帧 {}x{}，已改用整桌面",
                                full.0, full.1
                            );
                            eprintln!("[shotrs] {msg}");
                            self.status = msg;
                        }
                    }
                }
                None => {
                    eprintln!("[shotrs] 拿不到当前显示器，使用整桌面");
                }
            }
        }

        let max_side = self
            .painter
            .as_ref()
            .map(|p| p.max_texture_side())
            .unwrap_or(2048)
            .max(64);
        self.frame_size = (frame.width, frame.height);
        // 像素所有权交给 Arc：瓦片构建和「画布快照」共用同一份，不做拷贝
        let rgba = std::sync::Arc::new(std::mem::take(&mut frame.rgba));
        self.tiles = build_tiles(&self.ctx, &rgba, frame.width, frame.height, max_side);

        // 给需要读原图像素的工具（马赛克）一份只读快照。
        // 注意这里只给像素；「帧像素 ↔ 逻辑点」的换算每帧用当前屏幕矩形算
        // （建快照这一帧可能还没完成全屏切换，viewport 还是小窗口尺寸）。
        self.toolbox.set_canvas(Some(Arc::new(plugins::CanvasSource {
            ctx: self.ctx.clone(),
            rgba: rgba.clone(),
            width: frame.width,
            height: frame.height,
        })));

        // 临时调试：把裁剪后的帧原样存一份
        if std::env::var("SHOTRS_DUMP").is_ok() {
            let _ = image::save_buffer_with_format(
                "/tmp/shotrs-frame.png",
                rgba.as_slice(),
                frame.width as u32,
                frame.height as u32,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            );
            eprintln!("[shotrs] 已 dump 帧 → /tmp/shotrs-frame.png");
        }
        eprintln!(
            "[shotrs] 裁到 {}x{}，切成 {} 块瓦片（max_texture_side={max_side}）",
            frame.width,
            frame.height,
            self.tiles.len()
        );

        if self.smoke {
            self.sel = Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(frame.width as f32, frame.height as f32),
            ));
            self.phase = Phase::Annotate;
            self.action = Some(Action::Copy);
        } else {
            self.phase = Phase::Select;
            // 连拍复现模式：不等键盘，直接当作按了 Enter
            if self.loop_remaining > 0 {
                self.action = Some(Action::Copy);
            }
        }
        window.request_redraw();
    }

    // ------------------------------------------------ 主循环

    fn redraw(&mut self, window: &Window) {
        // 任何在 ctx.run_pass 之后、painter.paint_and_update_textures 之前的提前返回，
        // 都会把这一帧的 textures_delta 丢掉 —— 而瓦片纹理正是在 resolve_pending 里新建、
        // 靠这一帧上传的。丢了就永远传不上去（表现：整屏黑，永不恢复）。
        // 所以先把「能不能渲染」检查完，再动纹理。
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            // 刚建好的窗口第一帧可能还是 0×0（Wayland 的 configure 还没到），
            // 这时不能直接返回 —— 否则永远等不到下一次重绘
            window.request_redraw();
            return;
        }
        if self.gl.is_none() || self.painter.is_none() || self.egui_state.is_none() {
            return;
        }

        self.desktop_scale = self.desktop_scale.max(window.scale_factor() as f32);

        self.resolve_pending(window);
        if !self.tiles.is_empty() {
            self.ready_frames = self.ready_frames.saturating_add(1);
        } else if matches!(self.phase, Phase::Idle) {
            self.idle_frames = self.idle_frames.saturating_add(1);
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32() * 1000.0;
        self.last_tick = now;
        if (0.0..2000.0).contains(&dt) {
            self.frame_ms.push_back(dt);
            if self.frame_ms.len() > 120 {
                self.frame_ms.pop_front();
            }
        }

        let ctx = self.ctx.clone();
        let mut raw = match self.egui_state.as_mut() {
            Some(s) => s.take_egui_input(window),
            None => return,
        };
        // 演示模式：把合成事件塞进这一帧的输入，走真实代码路径（连「点截图按钮」都是合成的）
        let mut demo_done = false;
        if let Some(script) = self.demo.as_mut() {
            demo_done = script.tick(&mut raw, !self.tiles.is_empty());
        }
        if demo_done {
            self.action = Some(Action::Copy);
            self.demo = None;
        }
        let win = self.window.clone();
        let mut out = ctx.run_pass(raw, |ctx| {
            if let Some(w) = win.as_ref() {
                self.ui(ctx, w);
            }
        });
        if let Some(s) = self.egui_state.as_mut() {
            s.handle_platform_output(window, out.platform_output);
        }
        let primitives = ctx.tessellate(out.shapes, out.pixels_per_point);

        let ppp = out.pixels_per_point;

        // 导出：直接从后缓冲读回，选区内部没有压暗遮罩，裁出来就是成品。
        // GL 借用都留在这个块里，出去之后再改 self。
        // 调试 dump：演示模式每帧存一张（回看每一步），否则稳定后存一张
        let dump_to: Option<String> = if std::env::var("SHOTRS_DUMP").is_ok() {
            match (self.demo.as_ref(), &self.phase) {
                // 演示模式：每帧一张，方便回看每一步
                (Some(script), _) => Some(format!("/tmp/shotrs-demo-{:02}.png", script.step())),
                // 空闲态：等界面稳定后存一张控制窗口的样子（UI 快照）
                (None, Phase::Idle) => {
                    (self.idle_frames == 30).then(|| "/tmp/shotrs-control.png".to_string())
                }
                // 编辑器：稳定后存一张
                (None, _) => (self.ready_frames >= 3
                    && !self.dumped_backbuffer
                    && self.action.is_none())
                .then(|| "/tmp/shotrs-backbuffer.png".to_string()),
            }
        } else {
            None
        };

        // 拿成品图分两拍：第一拍只把覆盖层关掉，第二拍才读回像素（否则工具栏/HUD 会被拍进去）
        let exporting_now = self.action.is_some() && self.overlay_off;
        self.overlay_off = self.action.is_some();

        let hiding = self.hiding();
        let shot = {
            let Some(gl) = self.gl.as_ref() else { return };
            let Some(painter) = self.painter.as_mut() else {
                return;
            };

            // Wayland 的 buffer 是预乘 alpha：全透明 = (0,0,0,0)
            let (r_, g_, b_, alpha) = if hiding { (0.0, 0.0, 0.0, 0.0) } else { (0.0, 0.0, 0.0, 1.0) };
            unsafe {
                gl.gl.clear_color(r_, g_, b_, alpha);
                gl.gl.clear(glow::COLOR_BUFFER_BIT);
            }
            painter.paint_and_update_textures(
                [size.width, size.height],
                ppp,
                &primitives,
                &mut out.textures_delta,
            );

            let shot = if exporting_now || dump_to.is_some() {
                Some(painter.read_screen_rgba([size.width, size.height]))
            } else {
                None
            };
            if let Some((surface, context)) = gl.live.as_ref() {
                let _ = surface.swap_buffers(context);
            }
            shot
        };

        if let Some(path) = &dump_to {
            if let Some(shot) = &shot {
                if let Ok(bm) = bitmap_of(shot) {
                    let _ = utils::save::save_png(&bm, std::path::Path::new(path));
                }
                self.dumped_backbuffer = true;
            }
        }

        if exporting_now {
            let Some(shot) = shot else { return };
            let Some(action) = self.action.take() else { return };
            let bitmap = match self.grab_bitmap(&shot, ppp) {
                Ok(b) => b,
                Err(e) => {
                    self.status = format!("取图失败: {e}");
                    eprintln!("[shotrs] {}", self.status);
                    return;
                }
            };
            match action {
                // Enter：进剪贴板。放什么载荷取决于配置（本机/RDP/VNC 需求不同）
                Action::Copy => {
                    let result = match self.config.clipboard_mode {
                        // 图像模式：不落盘，同时提供 PNG / BMP 等格式
                        ClipboardMode::Image | ClipboardMode::Bitmap => clipboard::copy(clipboard::Payload::Image {
                            bitmap: &bitmap,
                            max_side: self.config.clipboard_max_side,
                            bitmap_only: self.config.clipboard_mode == ClipboardMode::Bitmap,
                        }),
                        // 文件 / 路径模式：先落盘，再把文件（uri-list）或路径文本放上剪贴板
                        mode => match save::save_into(&bitmap, &self.config.save_dir) {
                            Ok(path) => match mode {
                                ClipboardMode::File => clipboard::copy(clipboard::Payload::File(&path)),
                                _ => clipboard::copy(clipboard::Payload::Text(
                                    path.display().to_string(),
                                )),
                            },
                            Err(e) => Err(e),
                        },
                    };
                    let msg = match result {
                        Ok(how) => format!(
                            "已复制 {}x{} 到剪贴板（{how}）",
                            bitmap.width, bitmap.height
                        ),
                        Err(e) => format!("复制失败：{e}"),
                    };
                    eprintln!("[shotrs] {msg}");
                    self.status = msg;
                    self.back_to_control(window);
                }
                // 保存：落盘，留在编辑器里可以接着改
                Action::Save => {
                    let msg = match utils::save::save_into(&bitmap, &self.config.save_dir) {
                        Ok(path) => format!(
                            "已保存 {}x{} → {}",
                            bitmap.width,
                            bitmap.height,
                            path.display()
                        ),
                        Err(e) => format!("保存失败：{e}"),
                    };
                    eprintln!("[shotrs] {msg}");
                    self.status = msg;
                    self.overlay_off = false;
                    window.request_redraw();
                }
            }
        }
    }

    /// 画布上的光标形状：把「按下时会做什么」如实反映出来
    fn canvas_cursor(&self, sel: Rect, p: Pos2, space_held: bool) -> egui::CursorIcon {
        if self.point_over_delete(p, sel) {
            return egui::CursorIcon::PointingHand;
        }
        // 选中标注的手柄优先（它可能压在选区边框附近）
        if let Some(i) = self.scene.selected {
            if let Some(selection::Hit::Handle(h)) =
                self.scene.bounds_of(i).map(|b| selection::hit(b, p))
            {
                return h.cursor();
            }
        }
        match selection::hit(sel, p) {
            selection::Hit::Handle(h) => h.cursor(),
            selection::Hit::Inside if self.toolbox.active().is_selection_tool() || space_held => {
                egui::CursorIcon::Grab
            }
            selection::Hit::Inside if self.scene.hit(p).is_some() => egui::CursorIcon::Move,
            _ => egui::CursorIcon::Crosshair,
        }
    }

    /// 指针是否落在「选中标注的删除按钮」上（光标提示用）
    fn point_over_delete(&self, p: Pos2, sel: Rect) -> bool {
        self.scene
            .selected
            .and_then(|i| self.scene.bounds_of(i))
            .map(|b| delete_icon_center(b, sel).distance(p) <= DELETE_ICON_R)
            .unwrap_or(false)
    }

    /// 从后缓冲里裁出选区，转成 utils 认的 Bitmap
    fn grab_bitmap(&self, shot: &egui::ColorImage, ppp: f32) -> Result<Bitmap, String> {
        let full = Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(shot.size[0] as f32, shot.size[1] as f32),
        );
        let r = self.sel.unwrap_or(full);
        let x = (r.min.x * ppp).round().clamp(0.0, full.width()) as usize;
        let y = (r.min.y * ppp).round().clamp(0.0, full.height()) as usize;
        let w = (r.width() * ppp).round().clamp(1.0, full.width() - x as f32) as usize;
        let h = (r.height() * ppp).round().clamp(1.0, full.height() - y as f32) as usize;
        if w == 0 || h == 0 {
            return Err("选区为空".into());
        }
        let mut buf = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            for col in 0..w {
                let px = shot.pixels[(y + row) * shot.size[0] + (x + col)];
                buf.extend_from_slice(&[px.r(), px.g(), px.b(), 255]);
            }
        }
        Ok(Bitmap::new(w, h, buf))
    }

    // ------------------------------------------------ 界面

    fn ui(&mut self, ctx: &egui::Context, window: &Window) {
        match self.phase {
            Phase::Idle => self.ui_control(ctx, window),
            // 取帧期间什么都不画：整个窗口清成全透明，合成后 portal 就拍不到我们。
            // （Wayland 下 set_visible(false) 是空实现，这是唯一可靠的「隐身」办法）
            Phase::Capturing => {}
            _ => self.ui_editor(ctx, window),
        }
    }

    /// 取帧期间窗口必须完全不可见，见 ui() 里的说明
    fn hiding(&self) -> bool {
        // 只在「还没拿到帧」时隐身：帧一到手就可以正常画（此时 portal 早已拍完）
        matches!(self.phase, Phase::Capturing) && self.tiles.is_empty() && self.pending.is_none()
    }

    /// 空闲态：一个带按钮的小窗口
    fn ui_control(&mut self, ctx: &egui::Context, window: &Window) {
        let screen = ctx.viewport_rect();

        // 背景自己画：这块不依赖任何 panel/frame API，尺寸永远和窗口一致
        ctx.layer_painter(egui::LayerId::background())
            .rect_filled(screen, CornerRadius::ZERO, BG);

        egui::Area::new(egui::Id::unique("shotrs-control"))
            .fixed_pos(screen.min + Vec2::new(16.0, 14.0))
            .show(ctx, |ui| {
                ui.set_width((screen.width() - 32.0).max(120.0));

                // 标题条 = 拖动区 + 两个窗口按钮。
                // 无边框窗口只能靠 xdg_toplevel.move 拖动（Wayland 下 set_outer_position 无效）。
                ui.horizontal(|ui| {
                    let btn = Vec2::new(26.0, 24.0);
                    let drag_w = (ui.available_width() - btn.x * 2.0 - 12.0).max(60.0);
                    let bar = ui.allocate_response(Vec2::new(drag_w, 30.0), egui::Sense::drag());
                    if bar.drag_started() {
                        let _ = window.drag_window();
                    }
                    ui.painter().text(
                        Pos2::new(bar.rect.left() + 2.0, bar.rect.center().y),
                        Align2::LEFT_CENTER,
                        "shotrs",
                        FontId::proportional(20.0),
                        ACCENT,
                    );

                    if ui
                        .add_sized(btn, egui::Button::new(RichText::new("—").size(14.0)))
                        .on_hover_text("最小化到托盘（Wayland 窗口最小化无法恢复，所以这里是销毁窗口、进程留后台）")
                        .clicked()
                    {
                        self.pending_minimize = true;
                    }
                    if ui
                        .add_sized(btn, egui::Button::new(RichText::new("×").size(16.0)))
                        .on_hover_text(if self.config.close_to_tray {
                            "关闭 = 收进托盘（可在下面改）"
                        } else {
                            "关闭 = 退出程序（可在下面改）"
                        })
                        .clicked()
                    {
                        self.pending_close = true;
                    }
                });

                ui.label(
                    RichText::new("Rust 截图 · 技术验证版")
                        .size(12.0)
                        .color(FG_DIM),
                );
                ui.add_space(12.0);

                let btn = egui::Button::new(RichText::new("截图").size(16.0))
                    .min_size(Vec2::new(ui.available_width(), 40.0));
                let shot_btn = ui.add(btn);
                if shot_btn.clicked() {
                    self.start_capture(window);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.start_capture(window);
                }

                ui.add_space(10.0);
                ui.label(RichText::new(&self.status).size(11.0).color(FG_DIM));
                if !self.image_note.is_empty() {
                    ui.label(
                        RichText::new(&self.image_note)
                            .size(10.0)
                            .color(Color32::from_rgb(110, 115, 125)),
                    );
                }

                // ---- 截哪块屏：从托盘/快捷键触发时窗口是新建的，合成器一般把它放主屏，
                // 不显式指定就只能截主屏（以前得先把窗口拖到副屏）。
                self.refresh_displays(window);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("截图屏幕")
                            .size(10.0)
                            .color(Color32::from_rgb(120, 125, 140)),
                    );
                    let mut picked: Option<Option<String>> = None;
                    if ui
                        .selectable_label(self.config.capture_display.is_none(), "跟随窗口")
                        .on_hover_text("截控制窗口当前所在的那块屏")
                        .clicked()
                    {
                        picked = Some(None);
                    }
                    for (name, label) in &self.displays {
                        let selected = self.config.capture_display.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(selected, name)
                            .on_hover_text(label)
                            .clicked()
                        {
                            picked = Some(Some(name.clone()));
                        }
                    }
                    if let Some(v) = picked {
                        self.config.capture_display = v.clone();
                        let label = v.unwrap_or_else(|| "跟随窗口".into());
                        self.status = match self.config.save() {
                            Ok(()) => format!("截图屏幕改为「{label}」"),
                            Err(e) => format!("配置写入失败：{e}"),
                        };
                        self.sync_tray();
                    }
                });

                ui.add_space(6.0);
                // ---- 剪贴板载荷：本机 / RDP 图像 / RDP 文件 / VNC 路径，能力不一样
                ui.label(
                    RichText::new("Enter 复制的内容")
                        .size(10.0)
                        .color(Color32::from_rgb(120, 125, 140)),
                );
                ui.horizontal(|ui| {
                    for mode in [
                        ClipboardMode::Image,
                        ClipboardMode::Bitmap,
                        ClipboardMode::File,
                        ClipboardMode::Path,
                    ] {
                        if ui
                            .selectable_label(self.config.clipboard_mode == mode, mode.label())
                            .on_hover_text(match mode {
                                ClipboardMode::Image => "同时提供 PNG / BMP，供本机和远程客户端选择图像格式",
                                ClipboardMode::Bitmap => "仅提供 24 位 BMP：用于远程画图等程序，减少格式协商；传输仍取决于远程链路",
                                ClipboardMode::File => {
                                    "本地文件 URI：远程客户端须支持文件传输，否则可能只粘出路径"
                                }
                                ClipboardMode::Path => "纯文本路径：VNC 这类只同步文本的协议用",
                            })
                            .clicked()
                        {
                            self.config.clipboard_mode = mode;
                            self.status = match self.config.save() {
                                Ok(()) => format!("复制方式改为「{}」已保存", mode.label()),
                                Err(e) => format!("配置写入失败：{e}"),
                            };
                        }
                    }
                });

                // 复制到剪贴板时的最大边长：远端链路（RDP cliprdr）对大图敏感
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("复制尺寸")
                            .size(10.0)
                            .color(Color32::from_rgb(120, 125, 140)),
                    );
                    for (v, label) in [
                        (0u32, "原图"),
                        (1920, "≤1920"),
                        (1280, "≤1280"),
                        (640, "≤640"),
                        (320, "≤320"),
                    ] {
                        if ui
                            .selectable_label(self.config.clipboard_max_side == v, label)
                            .on_hover_text(match v {
                                0 => "按选区原尺寸复制（最清晰，但远端可能吃不消）".to_string(),
                                n => format!("复制图像的最长边限制为 {n}，缩小可减少位图传输量；保存仍为原图"),
                            })
                            .clicked()
                        {
                            self.config.clipboard_max_side = v;
                            self.status = match self.config.save() {
                                Ok(()) => format!("复制尺寸改为「{label}」已保存"),
                                Err(e) => format!("配置写入失败：{e}"),
                            };
                        }
                    }
                });

                // ---- 配置区：保存目录（点「保存」按钮才用到）+ 写回 JSON
                ui.label(
                    RichText::new("保存目录（点「保存」时用）")
                        .size(10.0)
                        .color(Color32::from_rgb(120, 125, 140)),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_dir_text)
                            .desired_width(ui.available_width() - 62.0)
                            .font(FontId::proportional(12.0)),
                    );
                    let apply_btn = ui.add(egui::Button::new(RichText::new("应用").size(11.0)));
                    if apply_btn.clicked() {
                        self.config.save_dir = PathBuf::from(self.save_dir_text.trim());
                        let msg = match self.config.save() {
                            Ok(()) => format!("配置已写入 {}", Config::path().display()),
                            Err(e) => format!("配置写入失败：{e}"),
                        };
                        eprintln!("[shotrs] {msg}");
                        self.status = msg;
                    }
                });
                ui.label(
                    RichText::new(format!("配置文件 {}", Config::path().display()))
                        .size(9.0)
                        .color(Color32::from_rgb(100, 105, 115)),
                );

                // ---- 关闭按钮行为
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("关闭按钮")
                            .size(10.0)
                            .color(Color32::from_rgb(120, 125, 140)),
                    );
                    for (v, label) in [(true, "收进托盘"), (false, "直接退出")] {
                        if ui
                            .selectable_label(self.config.close_to_tray == v, label)
                            .clicked()
                        {
                            self.config.close_to_tray = v;
                            self.status = match self.config.save() {
                                Ok(()) => format!("关闭按钮行为改为「{label}」"),
                                Err(e) => format!("配置写入失败：{e}"),
                            };
                            self.sync_tray();
                        }
                    }
                });

                // ---- 截图快捷键
                // portal 的 GlobalShortcuts 对 host 应用会报 `An app id is required`（xdp 1.21 实测），
                // 所以主路径是 GNOME 官方支持的「自定义快捷键 → 执行命令」，shell 负责按键。
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("截图快捷键")
                            .size(10.0)
                            .color(Color32::from_rgb(120, 125, 140)),
                    );
                    if self.hotkey_bound.is_empty() {
                        ui.label(
                            RichText::new("未绑定（用下面的命令）")
                                .size(10.0)
                                .color(Color32::from_rgb(230, 170, 90)),
                        );
                    } else {
                        ui.label(RichText::new(&self.hotkey_bound).size(10.0).color(FG_DIM));
                        if ui
                            .add(egui::Button::new(RichText::new("改键").size(10.0)))
                            .on_hover_text("打开 GNOME 的快捷键对话框")
                            .clicked()
                        {
                            if let Some(hk) = self.hotkey.as_ref() {
                                hk.configure();
                            }
                        }
                    }
                });
                // 可直接粘进「设置 → 键盘 → 自定义快捷键」的命令
                ui.horizontal(|ui| {
                    let cmd = format!("{} --screenshot", exe_path());
                    ui.add(
                        egui::TextEdit::singleline(&mut cmd.clone())
                            .desired_width(ui.available_width() - 64.0)
                            .font(FontId::monospace(10.0)),
                    );
                    if ui
                        .add(egui::Button::new(RichText::new("复制命令").size(10.0)))
                        .on_hover_text(format!(
                            "复制后到 设置 → 键盘 → 自定义快捷键 → +，粘到「命令」里\n（命令通道：{}）",
                            self.ipc_path
                        ))
                        .clicked()
                    {
                        self.status = match utils::clipboard::copy(
                            utils::clipboard::Payload::Text(cmd.clone()),
                        ) {
                            Ok(_) => format!("已复制命令：{cmd}"),
                            Err(e) => format!("复制失败：{e}"),
                        };
                    }
                });

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Enter 截图　Esc 退出")
                            .size(10.0)
                            .color(Color32::from_rgb(100, 105, 115)),
                    );
                    if ui
                        .add(egui::Button::new(RichText::new("退出").size(11.0)))
                        .clicked()
                    {
                        self.quit = true;
                    }
                });

                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.quit = true;
                }
            });
    }

    /// 编辑器里的动作按钮（目前只有「保存」）。返回它占用的矩形
    fn ui_actions(&mut self, ctx: &egui::Context) -> Rect {
        let dir = self.config.save_dir.display().to_string();
        let mut clicked = false;
        let mut undo_clicked = false;
        let mut redo_clicked = false;
        let area = egui::Area::new(egui::Id::unique("shotrs-actions"))
            .anchor(Align2::RIGHT_TOP, Vec2::new(-16.0, 44.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_black_alpha(200))
                    .inner_margin(6.0)
                    .corner_radius(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let can_undo = self.scene.can_undo();
                            let can_redo = self.scene.can_redo();
                            if ui
                                .add_enabled(
                                    can_undo,
                                    egui::Button::new(RichText::new("撤回").size(12.0))
                                        .min_size(Vec2::new(46.0, 26.0)),
                                )
                                .on_hover_text("撤回上一步（Z）")
                                .clicked()
                            {
                                undo_clicked = true;
                            }
                            if ui
                                .add_enabled(
                                    can_redo,
                                    egui::Button::new(RichText::new("恢复").size(12.0))
                                        .min_size(Vec2::new(46.0, 26.0)),
                                )
                                .on_hover_text("恢复刚撤回的（Shift+Z）")
                                .clicked()
                            {
                                redo_clicked = true;
                            }
                            clicked = ui
                                .add(
                                    egui::Button::new(RichText::new("保存").size(13.0))
                                        .min_size(Vec2::new(58.0, 26.0)),
                                )
                                .on_hover_text(format!("保存到 {dir}（S）"))
                                .clicked();
                        });
                    });
            });
        if undo_clicked {
            self.scene.undo();
        }
        if redo_clicked {
            self.scene.redo();
        }
        if clicked {
            self.action = Some(Action::Save);
        }
        area.response.rect
    }

    /// 全屏编辑器：框选 + 标注
    fn ui_editor(&mut self, ctx: &egui::Context, window: &Window) {
        let screen = ctx.viewport_rect();
        let phase = self.phase.clone();

        if !self.logged_layout && !self.tiles.is_empty() {
            self.logged_layout = true;
            let inner = window.inner_size();
            let ts: Vec<String> = self
                .tiles
                .iter()
                .map(|t| format!("{}+{} {}x{}", t.x, t.y, t.w, t.h))
                .collect();
            eprintln!(
                "[shotrs] 呈现：viewport={:.0}x{:.0}@{:.0},{:.0} ppp={:.3} buffer={}x{} tiles=[{}]",
                screen.width(),
                screen.height(),
                screen.min.x,
                screen.min.y,
                ctx.pixels_per_point(),
                inner.width,
                inner.height,
                ts.join(" ")
            );
        }

        let (ptr, down, pressed, released) = ctx.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_down(),
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
            )
        });
        let typing = ctx.egui_wants_keyboard_input();
        let space_held = ctx.input(|i| i.key_down(egui::Key::Space));

        // 导出那一帧把覆盖层全部藏起来，读回的像素才是「干净的成品」
        let exporting = self.overlay_off;

        // 马赛克这类工具要按「当前」屏幕矩形把逻辑点换算成帧像素
        self.toolbox.set_canvas_screen(screen);

        // ---- 工具栏（按钮栏 + 当前工具的属性面板）。布局/排序/快捷键都在 toolbar 模块里，
        // 这里只拿回它占用的区域，好把「点在工具栏上」从画布交互里排除。
        let mut toolbar_rect = if exporting {
            Rect::NOTHING
        } else {
            self.toolbox.ui(ctx, screen)
        };

        // ---- 动作按钮（保存）：右上角，和工具栏一样不许触发画布操作
        if !exporting {
            toolbar_rect = toolbar_rect.union(self.ui_actions(ctx));
        }

        // ---- 内联编辑器（文字等）：Enter 提交，Esc 丢弃
        if let Some(mut editor) = self.editor.take().filter(|_| !exporting) {
            let (enter, escape) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if enter {
                let shape = editor.finish();
                self.scene.push(shape);
            } else if !escape {
                editor.ui(ctx);
                self.editor = Some(editor);
            }
        }

        // ---- 输入状态机。插件只负责「按下给什么、拖到哪、松手成不成」，
        //      选中哪个工具、撤销、导出这些公共逻辑都留在这里。
        if self.editor.is_none() {
            self.toolbox.handle_hotkeys(ctx);
            if self.toolbox.take_tool_switched() {
                // 换工具就不该再显示上一个标注的删除按钮/手柄
                self.scene.selected = None;
            }

            if !typing {
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    // 编辑器里 Esc = 放弃这次截图，回控制窗口
                    self.back_to_control(window);
                    return;
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.action = Some(Action::Copy);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::S)) {
                    self.action = Some(Action::Save);
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Z)) {
                    // 修饰键既可能来自 InputState.modifiers（真实的 ModifiersChanged），
                    // 也可能只带在 Key 事件上（合成事件/某些输入法）—— 两边都认
                    let shift = ctx.input(|i| {
                        i.modifiers.shift
                            || i.events.iter().any(|e| {
                                matches!(
                                    e,
                                    egui::Event::Key {
                                        modifiers,
                                        pressed: true,
                                        ..
                                    } if modifiers.shift
                                )
                            })
                    });
                    if shift {
                        self.scene.redo();
                    } else {
                        self.scene.undo();
                    }
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Y)) {
                    self.scene.redo();
                }
                if ctx.input(|i| {
                    i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)
                }) {
                    // 删除当前选中的标注（点标注会选中它，右上角也有 × 按钮）
                    self.scene.remove_selected();
                }
                // 注：原来这里用 R 做「重选」，现在 R 分给了矩形工具；
                // 重选改成「拖到选区外面」（见选区命中测试的 Outside 分支）
            }

            // 点在「任何 UI 层」上都不算画布操作。
            //
            // 只判断 toolbar_rect 不够：egui 的取色器是**独立的弹出 Area**，
            // 弹在面板外面，按原来的写法点它会被当成画布按下 —— 于是顺手把选区边框
            // 拖走了。`layer_id_at` 能覆盖所有浮层（弹窗、菜单、提示）。
            let over_ui = ptr
                .map(|p| {
                    toolbar_rect.contains(p)
                        || ctx
                            .layer_id_at(p)
                            .map(|l| l.order != egui::Order::Background)
                            .unwrap_or(false)
                })
                .unwrap_or(false);
            if !over_ui {
                match phase {
                    Phase::Select => {
                        if pressed {
                            self.sel_start = ptr;
                            self.sel = ptr.map(|p| Rect::from_min_max(p, p));
                        }
                        if down {
                            if let (Some(s), Some(p)) = (self.sel_start, ptr) {
                                self.sel = Some(Rect::from_two_pos(s, p));
                            }
                        }
                        if released {
                            let too_small = self
                                .sel
                                .map(|r| r.width() < 8.0 || r.height() < 8.0)
                                .unwrap_or(true);
                            if too_small {
                                self.sel = Some(screen);
                            }
                            self.sel_start = None;
                            self.phase = Phase::Annotate;
                        }
                    }
                    Phase::Annotate => {
                        // 画布按下时的判定顺序（从最具体到最泛）：
                        //   选中标注的删除按钮 > 选中标注的缩放手柄 > 选区手柄
                        //   > 平移选区（选区工具/空格）> 选中/拖动已有标注 > 插件画新东西 > 重选
                        if pressed {
                            if let Some(p) = ptr {
                                let sel = self.sel.unwrap_or(screen);
                                let want_move =
                                    self.toolbox.active().is_selection_tool() || space_held;
                                let sel_hit = selection::hit(sel, p);

                                // ① 选中标注右上角的删除按钮
                                let delete_hit = match self.scene.selected {
                                    Some(i) => self
                                        .scene
                                        .bounds_of(i)
                                        .map(|b| delete_icon_center(b, sel).distance(p) <= DELETE_ICON_R)
                                        .unwrap_or(false),
                                    None => false,
                                };
                                // ② 选中标注的缩放手柄
                                let shape_handle = match self.scene.selected {
                                    Some(i) => self.scene.bounds_of(i).and_then(|b| {
                                        match selection::hit(b, p) {
                                            selection::Hit::Handle(h) => Some((i, h, b)),
                                            _ => None,
                                        }
                                    }),
                                    None => None,
                                };

                                if delete_hit {
                                    if let Some(i) = self.scene.selected {
                                        self.scene.remove(i);
                                    }
                                } else if let Some((index, handle, bounds)) = shape_handle {
                                    self.scene.begin_edit();
                                    self.resize_shape = Some((index, handle, bounds));
                                    self.sel_start = Some(p);
                                } else {
                                    match sel_hit {
                                        selection::Hit::Handle(h) => {
                                            self.scene.selected = None;
                                            self.sel_base = self.sel;
                                            self.sel_start = Some(p);
                                            self.grab = Some(selection::Grab::Resize(h));
                                        }
                                        selection::Hit::Inside if want_move => {
                                            self.scene.selected = None;
                                            self.sel_base = self.sel;
                                            self.sel_start = Some(p);
                                            self.grab = Some(selection::Grab::Move);
                                        }
                                        selection::Hit::Inside => {
                                            if let Some(i) = self.scene.hit(p) {
                                                // 点到已有标注：选中并准备拖动（改动前压快照）
                                                self.scene.selected = Some(i);
                                                self.scene.begin_edit();
                                                self.drag_shape = Some((i, p));
                                            } else {
                                                self.scene.selected = None;
                                                if let Some(draft) =
                                                    self.toolbox.active_mut().begin(p)
                                                {
                                                    self.draft = Some(draft);
                                                } else if let Some(editor) =
                                                    self.toolbox.active_mut().click(p)
                                                {
                                                    self.editor = Some(editor);
                                                }
                                            }
                                        }
                                        selection::Hit::Outside => {
                                            self.scene.selected = None;
                                            self.sel_base = None;
                                            self.sel_start = Some(p);
                                            self.grab = Some(selection::Grab::New);
                                        }
                                    }
                                }
                            }
                        }

                        if down {
                            if let (Some(grab), Some(from), Some(p)) =
                                (self.grab, self.sel_start, ptr)
                            {
                                let base = self
                                    .sel_base
                                    .unwrap_or_else(|| Rect::from_min_max(from, from));
                                self.sel = Some(selection::drag_rect(base, grab, from, p, screen));
                            }
                            if let (Some(draft), Some(p)) = (self.draft.as_mut(), ptr) {
                                draft.to(p);
                            }
                            if let (Some((index, last)), Some(p)) = (self.drag_shape, ptr) {
                                let delta = p - last;
                                if delta != egui::Vec2::ZERO {
                                    self.scene.translate(index, delta);
                                    self.drag_shape = Some((index, p));
                                }
                            }
                            if let (Some((index, handle, bounds)), Some(from), Some(p)) =
                                (self.resize_shape, self.sel_start, ptr)
                            {
                                let new =
                                    selection::drag_rect(bounds, selection::Grab::Resize(handle), from, p, screen);
                                self.scene.set_bounds(index, new);
                            }
                        }

                        if released {
                            // 新拉的选区太小就当没发生：退回整屏，避免出现 0 尺寸
                            if matches!(self.grab, Some(selection::Grab::New)) {
                                if let Some(r) = self.sel {
                                    if r.width() < 8.0 || r.height() < 8.0 {
                                        self.sel = Some(screen);
                                    }
                                }
                            }
                            self.grab = None;
                            self.sel_base = None;
                            self.sel_start = None;
                            self.drag_shape = None;
                            self.resize_shape = None;
                            if let Some(draft) = self.draft.take() {
                                let shape = draft.finish();
                                self.scene.push(shape);
                            }
                        }
                    }
                    Phase::Idle | Phase::Capturing => {}
                }
            }
        }

        // ---- 绘制底图
        let p = ctx.layer_painter(egui::LayerId::background());

        if !self.tiles.is_empty() {
            // 瓦片坐标是「帧像素」，画笔吃的是「逻辑点」，必须换算。
            // 漏了这步图会被放大 1/ppp 倍再被窗口裁掉一圈（实测 1.333 倍）。
            let (fw, fh) = (self.frame_size.0 as f32, self.frame_size.1 as f32);
            let sx = if fw > 0.0 { screen.width() / fw } else { 1.0 };
            let sy = if fh > 0.0 { screen.height() / fh } else { 1.0 };
            for t in &self.tiles {
                let dst = Rect::from_min_max(
                    Pos2::new(screen.min.x + t.x as f32 * sx, screen.min.y + t.y as f32 * sy),
                    Pos2::new(
                        screen.min.x + (t.x + t.w) as f32 * sx,
                        screen.min.y + (t.y + t.h) as f32 * sy,
                    ),
                );
                // 纹理尺寸 = 瓦片尺寸，UV 直接取满 0..1
                p.image(
                    t.tex.id(),
                    dst,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        } else {
            p.rect_filled(screen, CornerRadius::ZERO, BG);
        }

        // 压暗
        if let Some(sel) = self.sel {
            p.rect_filled(
                Rect::from_min_max(screen.min, Pos2::new(screen.max.x, sel.min.y)),
                CornerRadius::ZERO,
                DIM,
            );
            p.rect_filled(
                Rect::from_min_max(Pos2::new(screen.min.x, sel.max.y), screen.max),
                CornerRadius::ZERO,
                DIM,
            );
            p.rect_filled(
                Rect::from_min_max(Pos2::new(screen.min.x, sel.min.y), Pos2::new(sel.min.x, sel.max.y)),
                CornerRadius::ZERO,
                DIM,
            );
            p.rect_filled(
                Rect::from_min_max(Pos2::new(sel.max.x, sel.min.y), Pos2::new(screen.max.x, sel.max.y)),
                CornerRadius::ZERO,
                DIM,
            );
        } else {
            p.rect_filled(screen, CornerRadius::ZERO, DIM);
        }

        // 标注（裁剪到选区内）：怎么画由插件自己实现，这里只管顺序
        if let Some(sel) = self.sel {
            let clipped = p.with_clip_rect(sel);
            self.scene.draw(&clipped);
            Scene::draw_draft(&clipped, &self.draft);
            // 选中标注的示意框 + 手柄 + 右上角删除按钮
            if !exporting {
                if let Some(i) = self.scene.selected {
                    if let Some(b) = self.scene.bounds_of(i) {
                        p.rect_stroke(
                            b,
                            CornerRadius::ZERO,
                            Stroke::new(1.0, Stroke::new(1.0, ACCENT).color.gamma_multiply(0.9)),
                            StrokeKind::Outside,
                        );
                        selection::draw_handles(&clipped, b, ACCENT);
                        let c = delete_icon_center(b, sel);
                        clipped.circle_filled(c, DELETE_ICON_R, Color32::from_black_alpha(220));
                        clipped.circle_stroke(c, DELETE_ICON_R, Stroke::new(1.2, ACCENT));
                        let d = DELETE_ICON_R * 0.5;
                        clipped.line_segment(
                            [c + Vec2::new(-d, -d), c + Vec2::new(d, d)],
                            Stroke::new(1.6, Color32::WHITE),
                        );
                        clipped.line_segment(
                            [c + Vec2::new(d, -d), c + Vec2::new(-d, d)],
                            Stroke::new(1.6, Color32::WHITE),
                        );
                    }
                }
            }
            if !exporting {
                p.rect_stroke(sel, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
                // 指针挨着边框（或正在缩放）时画把手，平时不画免得挡视线
                let hovering_border = ptr
                    .map(|p| matches!(selection::hit(sel, p), selection::Hit::Handle(_)))
                    .unwrap_or(false);
                if hovering_border || matches!(self.grab, Some(selection::Grab::Resize(_))) {
                    selection::draw_handles(&p, sel, ACCENT);
                }
            }
        }

        if exporting {
            // 导出帧：覆盖层全部不画，直接返回（底图 + 标注已经在上面画好了）
            return;
        }

        // HUD
        let avg = if self.frame_ms.is_empty() {
            0.0
        } else {
            self.frame_ms.iter().sum::<f32>() / self.frame_ms.len() as f32
        };
        let fps = if avg > 0.0 { 1000.0 / avg } else { 0.0 };
        let hud = format!("{} 条标注   {avg:.1} ms  {fps:.0} fps", self.scene.len());
        p.text(
            Pos2::new(screen.max.x - 16.0, 14.0),
            Align2::RIGHT_TOP,
            hud,
            FontId::monospace(13.0),
            Color32::from_rgb(120, 255, 160),
        );
        p.text(
            Pos2::new(screen.min.x + 16.0, 14.0),
            Align2::LEFT_TOP,
            format!("{}\n{}\n{}", self.monitor_note, self.image_note, self.status),
            FontId::proportional(12.0),
            Color32::from_rgb(210, 215, 225),
        );

        // 底部提示
        let hint = match &phase {
            Phase::Capturing => if self.pending.is_some() {
                "已经取到帧，正在进入全屏…".to_string()
            } else {
                "正在取帧…（通常 ~1s）".to_string()
            },
            Phase::Select => "拖动选择区域（单击 = 全屏）　Esc 取消".to_string(),
            Phase::Annotate => {
                let tool = self.toolbox.active();
                format!(
                    "工具：{}　{}　|　拖边框/角 = 缩放　空格拖动 = 移动选区　R 重选　\
                     Z 撤销　Enter 复制　S 保存　Esc 取消",
                    tool.label(),
                    tool.hint()
                )
            }
            Phase::Idle => String::new(),
        };
        p.text(
            Pos2::new(screen.center().x, screen.max.y - 28.0),
            Align2::CENTER_CENTER,
            hint,
            FontId::proportional(14.0),
            Color32::from_rgb(235, 235, 240),
        );

        if matches!(self.phase, Phase::Select | Phase::Annotate) {
            // 光标形状就是最好的可见性提示：边框上变缩放箭头、能平移时变抓手
            let icon = match (self.grab, self.sel, ptr) {
                (Some(grab), _, _) => grab.cursor(),
                (None, Some(sel), Some(p)) => self.canvas_cursor(sel, p, space_held),
                _ => egui::CursorIcon::Crosshair,
            };
            ctx.set_cursor_icon(icon);
        }
        ctx.request_repaint();
    }

}

impl Drop for App {
    fn drop(&mut self) {
        // 退出前收掉托盘和快捷键线程（不然 D-Bus 上会留一个僵着的图标）
        if let Some(hk) = self.hotkey.as_ref() {
            hk.stop();
        }
        if let Some(tray) = self.tray.as_ref() {
            tray.shutdown();
        }
        // 正常退出时把命令通道的 socket 也删掉（异常退出留下的会在下次启动时清理）
        if !self.ipc_path.is_empty() {
            let _ = std::fs::remove_file(&self.ipc_path);
        }
        // 释放顺序很关键：egui_glow 的 Painter 在 Drop 里要 delete program/buffer/texture，
        // 必须赶在 GL 上下文被销毁之前、而且在上下文**当前**的时候。
        // 但如果此时窗口已经被收进托盘（上下文挂在 gl.idle 上、不是 current），
        // 就不能让它去调 GL —— 直接忘掉，交给进程退出时的驱动清理。
        let context_current = self.gl.as_ref().map(|g| g.live.is_some()).unwrap_or(true);
        if context_current {
            self.painter = None;
        } else if let Some(painter) = self.painter.take() {
            std::mem::forget(painter);
            eprintln!("[shotrs] 退出时没有窗口：跳过 GL 资源释放（上下文处于挂起态）");
        }
        self.gl = None;
        self.egui_state = None;
        self.window = None;
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() && !self.started {
            self.started = true;
            self.create_window(event_loop);

            // 托盘：顶部栏图标 + 小菜单（GNOME 靠 ubuntu-appindicators 扩展显示 SNI）
            self.tray = tray::spawn(
                self.config.close_to_tray,
                self.window.is_some(),
                self.config.hotkey.clone(),
            );
            eprintln!(
                "[shotrs] 自定义快捷键可绑定命令：{} --screenshot",
                exe_path()
            );

            // 全局快捷键：portal 的 GlobalShortcuts，绑定由用户在 GNOME 的确认框里定
            self.hotkey = hotkey::spawn(self.config.hotkey.clone());
            self.sync_tray();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // ---- 托盘菜单事件
        let mut tray_events = Vec::new();
        if let Some(tray) = self.tray.as_ref() {
            while let Ok(ev) = tray.events.try_recv() {
                tray_events.push(ev);
            }
        }
        for ev in tray_events {
            match ev {
                TrayEvent::Screenshot => self.request_screenshot(event_loop),
                TrayEvent::ShowWindow => self.pending_show = true,
                TrayEvent::Minimize => self.pending_minimize = true,
                TrayEvent::Quit => self.quit = true,
                TrayEvent::SetCloseToTray(v) => {
                    self.config.close_to_tray = v;
                    let msg = match self.config.save() {
                        Ok(()) => format!(
                            "关闭按钮行为改为「{}」",
                            if v { "收进托盘" } else { "直接退出" }
                        ),
                        Err(e) => format!("配置写入失败：{e}"),
                    };
                    self.status = msg;
                    self.sync_tray();
                    if let Some(w) = self.window.clone() {
                        w.request_redraw();
                    }
                }
                TrayEvent::SetCaptureDisplay(v) => {
                    let label = v.clone().unwrap_or_else(|| "跟随窗口".into());
                    self.config.capture_display = v;
                    let msg = match self.config.save() {
                        Ok(()) => format!("截图屏幕改为「{label}」"),
                        Err(e) => format!("配置写入失败：{e}"),
                    };
                    eprintln!("[shotrs] {msg}");
                    self.status = msg;
                    self.sync_tray();
                    if let Some(w) = self.window.clone() {
                        w.request_redraw();
                    }
                }
            }
        }

        // ---- 外部命令（GNOME 自定义快捷键 / 第二次启动的 --screenshot）
        let mut ipc_events = Vec::new();
        if let Some(rx) = self.ipc_rx.as_ref() {
            while let Ok(ev) = rx.try_recv() {
                ipc_events.push(ev);
            }
        }
        for ev in ipc_events {
            match ev {
                IpcEvent::Screenshot => {
                    eprintln!("[shotrs] 收到外部截图请求");
                    self.request_screenshot(event_loop);
                }
                IpcEvent::Show => self.pending_show = true,
            }
        }

        // ---- 全局快捷键事件
        let mut hk_events = Vec::new();
        if let Some(hk) = self.hotkey.as_ref() {
            while let Ok(ev) = hk.events.try_recv() {
                hk_events.push(ev);
            }
        }
        for ev in hk_events {
            match ev {
                hotkey::HotkeyEvent::Activated => self.request_screenshot(event_loop),
                hotkey::HotkeyEvent::Bound(desc) => {
                    self.hotkey_bound = desc.clone();
                    eprintln!("[shotrs] 全局快捷键已绑定：{desc}");
                    self.sync_tray();
                }
                hotkey::HotkeyEvent::Failed(e) => {
                    eprintln!("[shotrs] portal 全局快捷键不可用：{e}");
                    // 界面上只给一句可执行的话，详细原因在 stderr
                    self.status = String::from("全局快捷键：用下方命令绑定（portal 不支持本应用）");
                }
            }
        }

        // ---- 控制窗口里点出来的动作（需要 event_loop，所以挪到这里做）
        if self.pending_show {
            self.pending_show = false;
            // 用户明确要把窗口叫出来，那它就该留着（不受上一轮「从托盘截图」的影响）
            self.return_to_tray = false;
            if self.window.is_none() {
                self.create_window(event_loop);
                self.status = String::from("点「截图」开始");
                self.sync_tray();
            }
        }
        if self.pending_minimize {
            self.pending_minimize = false;
            self.destroy_window();
        }
        if self.pending_close {
            self.pending_close = false;
            self.request_close();
        }

        // 没有窗口时不会再来 RedrawRequested，退出请求必须在这里处理
        // （典型场景：收进托盘后从托盘菜单点「退出」）
        if self.quit {
            event_loop.exit();
            return;
        }

        // 取帧期间窗口是隐藏的，可能收不到重绘事件，所以在这里轮询结果
        if let Some(window) = self.window.clone() {
            self.poll_capture(&window);

            if matches!(self.phase, Phase::Capturing) {
                self.stuck_frames = self.stuck_frames.saturating_add(1);
                if self.stuck_frames % 120 == 0 {
                    let size = window.inner_size();
                    eprintln!(
                        "[shotrs] 仍在 Capturing：{}帧 inner={}x{} control_size={:?} 有接收端={} 有pending={} tiles={} 已过{:.1}s",
                        self.stuck_frames,
                        size.width,
                        size.height,
                        self.control_size,
                        self.capture_rx.is_some(),
                        self.pending.is_some(),
                        self.tiles.len(),
                        self.capturing_since.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0),
                    );
                }
            } else {
                self.stuck_frames = 0;
            }
            // 看门狗：卡在 Capturing（窗口是透明的，看起来像程序没了）就强制回控制窗口
            if matches!(self.phase, Phase::Capturing) {
                if let Some(since) = self.capturing_since {
                    if since.elapsed() > Duration::from_secs(15) {
                        eprintln!("[shotrs] Capturing 超过 15s 没进展，放弃本次取帧");
                        self.status = String::from("取帧卡住了，已放弃（可再试一次）");
                        self.back_to_control(&window);
                    }
                }
            }

            // 临时调试开关：空闲 2s 后自动开拍
            if self.auto_shot {
                self.auto_shot_ticks += 1;
                if self.auto_shot_ticks == 120 && matches!(self.phase, Phase::Idle) {
                    self.start_capture(&window);
                }
            }
        }
        // 空闲时让事件循环睡下去（不然 Poll 会空转一个核）；取帧/编辑期间才全速跑
        // 托盘和全局快捷键的事件是从别的线程来的，不会唤醒 winit 的事件循环，
        // 所以「空闲 / 无窗口」时用 WaitUntil 定时醒来轮询（代价是 20 次/秒的空转检查）。
        event_loop.set_control_flow(if matches!(self.phase, Phase::Idle) && !self.auto_shot {
            ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50))
        } else {
            ControlFlow::Poll
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if let Some(state) = self.egui_state.as_mut() {
            let resp = state.on_window_event(&window, &event);
            if resp.repaint {
                window.request_redraw();
            }
        }
        match event {
            // 「关闭」按配置走：收进托盘 或 退出
            WindowEvent::CloseRequested => self.request_close(),
            WindowEvent::ScaleFactorChanged { .. } => {
                // 拖到另一块缩放不同的屏上：重新按当前 inner_size 配一次 surface
                let size = window.inner_size();
                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    if let Some(gl) = self.gl.as_ref() {
                        if let Some((surface, context)) = gl.live.as_ref() {
                            surface.resize(context, w, h);
                        }
                    }
                }
                eprintln!(
                    "[shotrs] 缩放变化 → scale_factor={:.3} inner={}x{}",
                    window.scale_factor(),
                    size.width,
                    size.height
                );
                window.request_redraw();
            }
            WindowEvent::Resized(size) => {
                if let Some(gl) = self.gl.as_ref() {
                    if let (Some(w), Some(h)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                    {
                        if let Some((surface, context)) = gl.live.as_ref() {
                            surface.resize(context, w, h);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw(&window);
                if self.quit {
                    event_loop.exit();
                } else if !matches!(self.phase, Phase::Idle) || self.ctx.has_requested_repaint() {
                    // 取帧/框选/标注期间持续重绘（FPS HUD 与拖动反馈都要）；
                    // 空闲态只在 egui 自己要（例如悬停动画）时才继续
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- 工具函数

/// 选中标注时，「删除」按钮的位置：贴在选择项右上角，并且不会跑到选区外面
fn delete_icon_center(bounds: Rect, sel: Rect) -> Pos2 {
    Pos2::new(
        (bounds.max.x + DELETE_ICON_R + 4.0).min(sel.max.x - DELETE_ICON_R - 2.0),
        (bounds.min.y - DELETE_ICON_R - 4.0).max(sel.min.y + DELETE_ICON_R + 2.0),
    )
}

/// 当前可执行文件的绝对路径（写进 GNOME 自定义快捷键的命令里）
fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "shotrs".into())
}

/// 把内嵌的 PNG 解成 winit 的窗口图标（Wayland 下不会用到，X11/XWayland 有效）
fn load_icon() -> Option<winit::window::Icon> {
    let img = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// 截图目标屏：`SHOTRS_MONITOR=<名字片段>` 指定时用那块，否则 None（= 交给合成器，
/// 通常就是控制窗口所在的那块屏）。
fn env_monitor() -> Option<String> {
    let want = std::env::var("SHOTRS_MONITOR").ok()?;
    let want = want.trim().to_string();
    (!want.is_empty()).then_some(want)
}

fn describe_monitor(window: &Window) -> String {
    match window.current_monitor() {
        Some(m) => {
            let (px, py) = monitor_logical_origin(&m);
            let sz = m.size();
            // ppp = 窗口报的分数缩放（真的那个）；int = monitor 报的整数缩放（位置被它乘过）
            format!(
                "显示器 {} {}x{} ppp={:.3} int={} pos({px},{py})",
                m.name().unwrap_or_else(|| "?".into()),
                sz.width,
                sz.height,
                window.scale_factor(),
                m.scale_factor(),
            )
        }
        None => "显示器未知（稍后在全屏时重查）".into(),
    }
}

/// 显示器在桌面布局里的**逻辑**位置。
///
/// winit 0.30 的 `MonitorHandle::position()` 在 Wayland 上返回的是
/// `LogicalPosition::to_physical(output.scale)`（见 winit `platform_impl/linux/wayland/output.rs`），
/// 而 `output.scale` 只有整数（分数缩放在 wl_output 里报不出来：1.333 报 2、1.5 报 2、1.0 报 1）。
/// 所以它其实是「逻辑坐标 × 该屏整数缩放」，除以 `scale_factor()` 才是真正的逻辑位置。
/// 之前副屏 1.0（整数缩放 1）时两者恰好相同，副屏改成 1.5（整数缩放 2）后就翻倍了 ——
/// 副屏 1920 被当成 3840，裁剪区整个跑到帧外面，只能退回「整桌面」。
fn monitor_logical_origin(m: &winit::monitor::MonitorHandle) -> (f32, f32) {
    let p = m.position();
    let int_scale = (m.scale_factor() as f32).max(0.5);
    (p.x as f32 / int_scale, p.y as f32 / int_scale)
}

/// 当前屏在 portal 位图里的矩形（位图像素）。
///
/// 坐标系有四层，实测（GNOME 50 + winit 0.30，2560×1600@1.333 主屏 + 1920×1080@1.5 副屏）：
///   * `inner_size()`           全屏时 = 该屏的设备像素        （主屏 2560×1600，副屏 1920×1080）
///   * `window.scale_factor()`  该屏的分数缩放                 （主屏 1.333，副屏 1.5）
///   * `monitor.position()`     逻辑坐标 × 该屏**整数**缩放     （主屏 0,0；副屏 1920×2 = 3840,0）
///   * `monitor.scale_factor()` 那个整数缩放                    （两块屏都是 2）
///   * portal 位图              整桌面按**会话最大**分数缩放渲染 （4800×1800 = (1920+1280)×1.5）
///
/// 所以位图矩形 = (逻辑位置, 逻辑尺寸) × 桌面缩放，其中逻辑尺寸 = 设备像素 ÷ 当前屏缩放。
/// 注意桌面缩放是「所有屏里最大的那个」，一般**不等于**当前屏的缩放：主屏 1.333 的内容在帧里
/// 被渲染成 1.5 倍（2880×1800，比屏幕的 2560×1600 大一圈），裁完要缩回 `inner_size`
/// （见 `resize_rgba` 的调用处）。
///
/// 早先直接用 `inner_size` 当位图像素：在主屏上因为两个 1.333 恰好相消而正确，
/// 到副屏就变成「拿 1920×1080 去裁 2560×1440 的区域」，整个画面错位。
fn monitor_bitmap_rect(
    window: &Window,
    desktop_scale: f32,
    frame: (usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let inner = window.inner_size();
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let mut ppp = window.scale_factor() as f32;
    if ppp <= 0.0 {
        return None;
    }
    // 当前屏的逻辑位置与逻辑尺寸
    let (mut px, mut py) = match window.current_monitor() {
        Some(m) => monitor_logical_origin(&m),
        None => (0.0, 0.0),
    };
    let mut logical_w = inner.width as f32 / ppp;
    let mut logical_h = inner.height as f32 / ppp;

    // 临时调试：SHOTRS_FAKE_MONITOR="x,y,逻辑宽,逻辑高,ppp" 强行指定屏几何
    let mut s_override: Option<f32> = None;
    if let Ok(spec) = std::env::var("SHOTRS_FAKE_MONITOR") {
        let v: Vec<f32> = spec.split(',').filter_map(|t| t.trim().parse().ok()).collect();
        if v.len() >= 5 {
            px = v[0];
            py = v[1];
            logical_w = v[2];
            logical_h = v[3];
            ppp = v[4];
        }
        if v.len() >= 6 {
            s_override = Some(v[5]);
        }
    }

    // 桌面缩放：优先用观测到的分数缩放（窗口在全屏时就是该屏的分数缩放，取历史最大）。
    // 但如果「按这个缩放估出来的桌面」比帧还小，说明观测值偏小 —— 典型场景：程序一开始
    // 就落在缩放较小的副屏上，从没见过主屏的 1.333。这时用「让这块屏刚好装进帧」的上界反推。
    let mut s = s_override.unwrap_or(desktop_scale.max(1.0));
    let fits = |s: f32| {
        let w = (px.max(0.0) + logical_w) * s;
        let h = (py.max(0.0) + logical_h) * s;
        w <= frame.0 as f32 + 0.5 && h <= frame.1 as f32 + 0.5
    };
    let (union_w, union_h) = desktop_union_logical(window);
    let frame_bigger = union_w * s + 1.0 < frame.0 as f32 || union_h * s + 1.0 < frame.1 as f32;
    if s_override.is_none() && (frame_bigger || !fits(s)) {
        let bound = (frame.0 as f32 / (px.max(0.0) + logical_w))
            .min(frame.1 as f32 / (py.max(0.0) + logical_h));
        if bound > 0.5 && fits(bound) && (bound - s).abs() > 0.01 {
            eprintln!(
                "[shotrs] 桌面缩放改用反推值 {bound:.3}（观测 {s:.3} 与帧 {}x{} 不符）",
                frame.0, frame.1
            );
            s = bound;
        }
    }
    // 按「边缘取整」而不是「尺寸取整」，避免 1px 累计误差
    let x0 = (px.max(0.0) * s).round() as usize;
    let y0 = (py.max(0.0) * s).round() as usize;
    let x1 = ((px.max(0.0) + logical_w) * s).round() as usize;
    let y1 = ((py.max(0.0) + logical_h) * s).round() as usize;
    let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let (cx, cy) = (x0, y0);

    eprintln!(
        "[shotrs] 裁剪来源 inner={}x{} ppp={ppp:.3} → 逻辑 {}x{}@{px},{py} × 桌面缩放 {s:.3} = {cw}x{ch}@{cx},{cy}",
        inner.width, inner.height, logical_w, logical_h
    );
    Some((cx, cy, cw, ch))
}

/// 按 winit 给的信息粗估桌面逻辑尺寸。
///
/// 只用来判断「帧是不是比预计的大」：Wayland 分数缩放下 winit 报的是整数缩放（偏大），
/// 所以逻辑尺寸会估**偏小** —— 这个方向的误差是安全的（宁可多触发一次反推）。
/// 位置用的是 `monitor.position()`（逻辑 × 整数缩放，见 `monitor_logical_origin`），不除回来，
/// 因为这个函数只要一个「量级」：偏大偏小都会影响判断，宁可在宽度上偏大、高度上偏小。
/// 真正拿来算裁剪的是 `monitor_bitmap_rect` 里那套精确几何，别引用这里的值。
fn desktop_union_logical(window: &Window) -> (f32, f32) {
    let mut w = 1.0f32;
    let mut h = 1.0f32;
    for m in window.available_monitors() {
        let p = m.position();
        let sz = m.size();
        let sc = (m.scale_factor() as f32).max(0.5);
        let lw = sz.width as f32 / sc;
        let lh = sz.height as f32 / sc;
        w = w.max(p.x.max(0) as f32 + lw);
        h = h.max(p.y.max(0) as f32 + lh);
    }
    (w, h)
}

/// 把整幅帧切成不超过 max_side 的瓦片，每块单独成一个纹理
fn build_tiles(
    ctx: &egui::Context,
    rgba: &[u8],
    w: usize,
    h: usize,
    max_side: usize,
) -> Vec<Tile> {
    let mut tiles = Vec::new();
    let mut y = 0;
    while y < h {
        let th = (h - y).min(max_side);
        let mut x = 0;
        while x < w {
            let tw = (w - x).min(max_side);
            let mut buf = Vec::with_capacity(tw * th * 4);
            for row in 0..th {
                let start = ((y + row) * w + x) * 4;
                buf.extend_from_slice(&rgba[start..start + tw * 4]);
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([tw, th], &buf);
            let tex = ctx.load_texture(
                format!("frozen-{x}-{y}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            tiles.push(Tile { tex, x, y, w: tw, h: th });
            x += tw;
        }
        y += th;
    }
    tiles
}

fn crop_rgba(
    src: &[u8],
    sw: usize,
    sh: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || x + w > sw || y + h > sh {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = ((y + row) * sw + x) * 4;
        out.extend_from_slice(&src[start..start + w * 4]);
    }
    Some(out)
}

/// RGBA8 重采样（位图像素 → 该屏设备像素）。Lanczos3 用于纯缩小，不引入可见振铃。
fn resize_rgba(rgba: Vec<u8>, from: (usize, usize), to: (usize, usize)) -> Option<Vec<u8>> {
    let (fw, fh) = from;
    let (tw, th) = to;
    let img = image::RgbaImage::from_raw(fw as u32, fh as u32, rgba)?;
    let out = image::imageops::resize(
        &img,
        tw as u32,
        th as u32,
        image::imageops::FilterType::Lanczos3,
    );
    Some(out.into_raw())
}

/// 整张 ColorImage → Bitmap（调试 dump 用）
fn bitmap_of(shot: &egui::ColorImage) -> Result<Bitmap, String> {
    let (w, h) = (shot.size[0], shot.size[1]);
    let mut buf = Vec::with_capacity(w * h * 4);
    for px in &shot.pixels {
        buf.extend_from_slice(&[px.r(), px.g(), px.b(), 255]);
    }
    Ok(Bitmap::new(w, h, buf))
}

fn install_cjk_fonts(ctx: &egui::Context) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(format!(
            "{home}/.local/share/fonts/NotoSansCJK/NotoSansCJKsc-Medium.otf"
        )));
        candidates.push(PathBuf::from(format!(
            "{home}/.local/share/fonts/NotoSansCJK/NotoSansCJKsc-Regular.otf"
        )));
    }
    candidates.push(PathBuf::from(
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ));
    candidates.push(PathBuf::from(
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    ));

    let mut fonts = egui::FontDefinitions::default();
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let name = "cjk".to_owned();
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, name.clone());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(name);
        eprintln!("[shotrs] 载入中文字体: {}", path.display());
        break;
    }
    ctx.set_fonts(fonts);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `shotrs --screenshot` / `--show`：把命令转给已经在跑的实例（给 GNOME 自定义快捷键用）
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--screenshot" || a == "screenshot") {
        if ipc::send("screenshot") {
            return Ok(());
        }
        eprintln!("[shotrs] 没有在运行的实例；先启动 shotrs 再用这个命令");
        return Ok(());
    }
    if args.iter().any(|a| a == "--show") {
        if ipc::send("show") {
            return Ok(());
        }
        eprintln!("[shotrs] 没有在运行的实例");
        return Ok(());
    }

    // 单实例：socket 抢不到就说明已有实例，把它的窗口叫出来然后自己退出
    let Some((ipc_path, ipc_rx)) = ipc::listen() else {
        ipc::send("show");
        eprintln!("[shotrs] 已经有一个实例在运行，已通知它显示窗口");
        return Ok(());
    };

    eprintln!("[shotrs] 启动中…（控制窗口里点「截图」）");
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(Some(ipc_rx), ipc_path.display().to_string());
    event_loop.run_app(&mut app)?;
    Ok(())
}
