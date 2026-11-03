# AGENTS.md

给在这个仓库里干活的人 / AI 代理看的工程文档：架构、约定、踩过的坑、自检方式。
面向用户的简介和操作说明在 `README.md`。

技术栈：Rust + `winit 0.30` + `glutin/egui_glow`（自绘 OpenGL）+ `egui 0.37`，
取帧走 `ashpd`（xdg-desktop-portal），托盘走 `ksni`，剪贴板走独立 GTK helper。
目标环境：**GNOME 50 / Wayland**（本机实测；X11 下大部分功能可用但没专门调）。

---

## 构建 / 运行 / 回归

```bash
cargo run --release                      # 首次要编译 100 多个 crate，几分钟；之后秒级
SHOTRS_SMOKE=1 ./target/release/shotrs   # 全自动截一张 + 复制 + 退出（链路自检）
SHOTRS_DEMO=1 SHOTRS_DUMP=1 ./target/release/shotrs   # 插件/交互的回归脚本
SHOTRS_LOOP=2 ./target/release/shotrs    # 连拍自检（复现「截图卡死」那类问题）
```

**改完一定要跑：** `SHOTRS_SMOKE=1`（链路）+ `SHOTRS_DEMO=1`（插件/输入状态机）+
`SHOTRS_LOOP=2`（常驻取帧线程）。三者都 `exit=0` 才算没写坏。

**主程序已有实例在跑时进不去**（单实例 socket 会把握手转给它）。要另起一个做测试，用独立的
runtime 目录，别把用户正在用的那个托盘实例挤掉：

```bash
mkdir -p /tmp/shotrs-test && chmod 700 /tmp/shotrs-test
XDG_RUNTIME_DIR=/tmp/shotrs-test WAYLAND_DISPLAY=/run/user/1000/wayland-0 \
  SHOTRS_SMOKE=1 ./target/release/shotrs
```

`cargo clippy` 里有 6 个警告 + 1 个**既有**错误：`src/main.rs` 取帧结果轮询那处 `never_loop`
（deny 级 correctness lint，与这些功能无关）；`cargo build` 本身无警告。

---

## 结构

```
src/main.rs             窗口 / GL 上下文 / egui 集成 / 输入状态机 / 导出
src/capture.rs          取帧层：常驻线程 + 单个 runtime → xdg-desktop-portal，channel 回传
src/scene.rs            文档模型：标注集合（只存 Box<dyn Shape>，不关心类型）
src/toolbar/mod.rs      工具栏：按钮栏（可拖动排序）+ 属性面板（子工具栏）+ 快捷键
src/selection.rs        选区编辑：命中测试 / 平移 / 缩放 / 把手绘制
src/plugins/mod.rs      插件协议（Tool / Draft / InlineEditor / Settings / Shape）+ 注册表
src/plugins/common.rs   属性面板复用的控件（调色板、数值滑杆）
src/plugins/select.rs   选区工具（不画东西，只平移选区）
src/plugins/arrow.rs    箭头插件（颜色、线宽）
src/plugins/rect.rs     矩形插件（颜色、线宽）
src/plugins/ellipse.rs  椭圆插件（颜色、线宽）
src/plugins/mosaic.rs   马赛克插件（块大小；靠 CanvasSource 读原图）
src/plugins/text.rs     文字插件（颜色、字号）
src/utils/mod.rs        通用能力入口 + Bitmap（界面层与工具层的解耦类型）
src/utils/clipboard.rs  复制到剪贴板：PNG + BMP 多格式（走 GTK helper）/ BMP / URI / 路径
src/utils/clipboard_owner.py  GTK 剪贴板持有进程（通过 XWayland 同时宣告 png/bmp）
src/utils/save.rs       存成 PNG（「保存」按钮 / S）
src/utils/png.rs        PNG 编解码（统一输出无 alpha 的 RGB）
src/utils/config.rs     本地 JSON 配置：加载 / 写回
src/tray.rs             顶部栏图标（StatusNotifierItem）+ 小菜单
src/hotkey.rs           portal GlobalShortcuts 的尝试实现（host 应用多半会被拒，见下）
src/ipc.rs              单实例 + unix socket 命令通道（--screenshot / --show）
src/demo.rs             合成输入脚本（回归测试 / 全自动验收）
examples/monitors.rs    探针：winit 眼里的显示器尺寸/位置/整数缩放
examples/probe.rs       探针：portal 实际给的多大一张图
assets/shotrs.svg       程序图标源文件（另有 32/48/64/128/256/512 PNG 与 .ico）
assets/shotrs.desktop   .desktop 条目（GNOME 靠 app_id 匹配它的 Icon=）
assets/install-icon.sh  把图标与 .desktop 装进 ~/.local/share
assets/architecture.svg 架构图源文件（PNG 版同名 .png）
tools/remmina-x11.sh    用 XWayland 启动 Remmina（对照排查剪贴板问题）
```

状态机四态：`Idle`（小窗口）→ `Capturing`（隐身 + 取帧）→ `Select`（全屏框选）→ `Annotate` → 导出后回 `Idle`。

---

## 取帧层（`capture.rs`）

**必须是一个常驻线程 + 一个 runtime（不是优化，是 bug）。** `ashpd` 把 zbus 的 session 连接缓存在
进程级 `OnceLock`（`ashpd/src/proxy.rs`），那条连接的 IO 驱动跟着**第一次**创建它的 tokio runtime 走。
早先「每截一张就 spawn 一个线程 + 新建 runtime」的写法：第一张正常，线程结束 runtime 一 drop，
缓存的连接就再也收不到回包 —— 后面的请求全部卡死（表现：点「截图」后窗口变透明、程序像消失了）。
现在：一个线程、一个 runtime、一条连接，任务通过 channel 投递，用完不销毁。

外面还套了两层兜底：单次调用 10s 超时（`tokio::time::timeout`）和主线程 15s 看门狗
（超时就恢复成控制窗口并报错，绝不留在透明态），点一次重试即可。

诊断时看三处：`取帧完成/失败` 的耗时、`journalctl --user | grep "associate portal window"`、
以及 `journalctl --user | grep xdg-desktop-portal-gnome`。实测期间还撞到过 **portal-gnome 自己
SEGV**（`status=11/SEGV`，systemd 随后自动重启），那一轮会超时 —— 属于系统侧问题，重试即可。

### 「点截图后客户端消失」的两个坑

1. **`modal(true)` 但没有 parent window identifier（真凶）。** 走非交互截图时本来就没有对话框，
   而 `modal(true)` 会要求 portal 把它的窗口挂到「当前窗口」上；我们没设 parent（Wayland 下要
   `XDG_ACTIVATION_TOKEN` 那一套），于是 `xdg-desktop-portal-gnome` 每秒打印

   ```
   Failed to associate portal window with parent window ''
   ```

   并且**永远不回包** —— 主线程停在 `Capturing`（窗口此时全透明），看起来就是「客户端消失了」。
   这个坑是间歇性的（portal 有时不走关联那条路），所以表现为「有时成功、再点又消失」。
   修法：`.modal(false)`。

2. **取帧线程必须常驻**：见上。

### 副作用

GNOME portal 取帧默认会把截图落到 `~/图片/Screenshot.png`、`Screenshot-1.png`…。
`capture.rs` 在解码完成后立即删除该文件，不留副作用（删除失败也不影响取帧）。
HUD 里那行「portal 临时文件 Screenshot-N.png 已清理」就是这条。

---

## 「隐身」只能靠透明，不能靠隐藏

窗口一旦上屏就会被 portal 截进去，而 Wayland 下没有任何办法把它藏起来：

| 手段 | 结果 |
|---|---|
| `setExcludedFromCapture` | 空实现 |
| `Window::set_visible(false)` | **空实现**（winit 源码注释：`Not possible on Wayland.`） |
| `Window::set_minimized(true)` | 能最小化，但**无法恢复**（`Unminimizing is ignored on Wayland.`） |
| `set_outer_position` | 空实现，挪不走 |

真正可行的做法：窗口创建时带 `with_transparent(true)`（否则 winit 会给 surface 设 opaque region，
合成器直接无视 alpha），取帧期间什么都不画、把 framebuffer 清成预乘全透明 `(0,0,0,0)`，
等一帧被合成器合成之后再发 portal 请求。实测冻结帧里再也看不到自己。

踩过的两个错版：先 `set_fullscreen` 再 `set_visible(false)`（unmap 被全屏过渡吞掉，截到自己的全屏黑窗）；
以及只加 alpha 而没加 `with_transparent(true)`（合成器按 opaque 处理，截到一块白/黑的方块）。

---

## 裁剪：等全屏尺寸生效后再做

取到帧时窗口还是小窗，`inner_size()` 不可用；所以帧先放进 `pending`，进全屏后每帧比对
`inner_size()` 是否**等于当前显示器的 `monitor.size()`**，生效了才裁剪 + 上传纹理。
超过 1.5s（配 120 帧上限）还没生效就放弃裁剪、直接铺整桌面 —— 拿旧的小窗尺寸去裁只会得到
「屏幕一角」，不如退化成整桌面。

**「全屏生效了吗」不能用「比小窗口大」判断。** 上一张截图结束时会 `set_fullscreen(None)`，
而 `inner_size()` 在下一帧才回到小窗尺寸；用户紧接着再点「截图」时读到的还是上一块屏的全屏尺寸，
于是被误判成「已经全屏」→ 等 120 帧 → 错误地退回整桌面。现在按 `monitor.size()` 比，并按时间等，
期间每 15 帧重发一次全屏请求（自愈「请求被合成器忽略」）。

---

## 坐标与多屏（最大的坑）

### 四层坐标

portal 返回的是**整桌面一张位图**，按「会话里最大的那个分数缩放」渲染。于是同时存在四层坐标
（下表是副屏改 1.5 之后的实测，主屏 eDP-2 2560×1600@1.333 + 副屏 DP-2 1920×1080）：

| 来源 | 单位 | 实测 |
|---|---|---|
| `window.inner_size()` | 设备像素（全屏时 = 该屏物理分辨率） | 主屏 2560×1600，副屏 1920×1080 |
| `window.scale_factor()` | 该屏的**分数**缩放 | 主屏 1.333，副屏 1.5 |
| `monitor.scale_factor()` | 该屏的**整数**缩放（wl_output 报不出分数） | 两块屏都是 2 |
| `monitor.position()` | 逻辑坐标 **×** 上面那个整数缩放 | 主屏 (0,0)，副屏 (1920×2 = **3840**,0) |
| portal 位图 | 逻辑 × 会话最大缩放 | 4800×1800（= (1920+1280)×1.5） |

`monitor.position()` 的语义来自 winit 0.30 的
`platform_impl/linux/wayland/output.rs`：`LogicalPosition::to_physical(output.scale)`，
而 `output.scale` 只有整数（1.333 → 2、1.5 → 2、1.0 → 1）。

所以**不能按「物理像素 ÷ scale = 逻辑像素」套公式**（主屏会算成 1280×800，实测踩过），
也不能直接拿 `monitor.position()` 当逻辑坐标（副屏 1.5 时是 3840 而不是 1920）：

- 裁剪尺寸 = `inner_size / window.scale_factor()`
- 裁剪偏移 = `monitor.position() / monitor.scale_factor()`（`monitor_logical_origin()`）

### 踩过的两个坑

1. **副屏裁错区（早期）**：直接拿 `inner_size` 当位图像素去裁。在主屏上因为「窗口 1.333 恰好等于
   位图 1.333」而正确，一搬到副屏就变成「拿 1920×1080 去裁本该是 2560×1440 的区域」，画面整体错位，
   看着就像「找不到截图在哪」。
2. **副屏改 1.5 之后（两连坑）**：
   - **位移翻倍**：`monitor.position()` 从 1920 变 3840（整数缩放 1 → 2），裁剪区整块跑到帧外面，
     只能退回整桌面。解法就是上面的 `monitor_logical_origin()`。
   - **主屏截出来比屏幕大 12.5%**：位图按会话最大缩放（副屏的 1.5）渲染，主屏（1.333）的内容在帧里
     是被放大过的：裁 2880×1800，而屏幕只有 2560×1600。多出来的像素是纯插值，所以裁完再按
     `inner_size` 重采样回设备像素（Lanczos3，方向只会是缩小，85–130 ms），日志打
     `帧 2880x1800 → 2560x1600（缩回该屏设备像素…）`。

### 现在的算法

**位图矩形 = (逻辑位置, 逻辑尺寸) × 桌面缩放**，裁完再重采样到 `inner_size`。
桌面缩放优先取「全屏时观测到的分数缩放的历史最大值」；如果按它估出来的桌面比帧还小
（典型场景：程序一开始就落在缩放较小的副屏上，从没见过主屏的 1.333），就用「让这块屏刚好装进帧」
的上界反推一次，并打印 `桌面缩放改用反推值`。

顺带处理了 `ScaleFactorChanged`：换屏后主动按当前 `inner_size` 重配一次 GL surface
（有些合成器不发 `Resized`），并打一行 `缩放变化` 日志。

### 探针（排查这类问题先看真值）

```bash
cargo run --release --example monitors             # winit 眼里的尺寸 / 位置 / 整数缩放
SHOTRS_DUMP=1 cargo run --release --example probe  # portal 实际给的多大一张图
```

两个都不开窗口，所以**已有实例在跑时也能用**。另外 `SHOTRS_FAKE_MONITOR` 可以在单屏上假装自己是
另一块屏，复现混合缩放的裁切。

### 想「跟随鼠标」时的可行路线（未实现）

Wayland 客户端拿不到全局指针坐标。可用的旁路是 XWayland：`XQueryPointer(root)` 能读到指针
（Mutter 会把真实指针位置同步进 X11），坐标空间 = **逻辑 × 整数缩放**，正好和 `monitor.position()`
同一套；矩形用 RandR 1.5 的 `xrandr --listmonitors`（`XRRGetMonitors`，名字 atom 用 `XGetAtomName`
转字符串，与 `monitor.name()` 对得上）。要点：**在用户按键/点托盘的那一刻采样**，把结果存下来，
`enter_editor` 与「全屏未生效重试」都复用它。`x11-dl` 已在 `Cargo.lock` 里（winit 的间接依赖），
它靠 dlopen 加载 `libX11.so.6`，不需要 libX11 开发包。实测限制：`XWarpPointer` 会被 Mutter 夹在
主屏内（读可靠、写不可靠）；有的配置下 `--listmonitors` 只报一块「全覆盖」的 monitor（那就得走
`XRRGetScreenResources` + `XRRGetCrtcInfo` + `XRRGetOutputInfo` 那条老路）。

---

## 绘制 / GL 的几个硬约束

**纹理要切瓦片。** `GL_MAX_TEXTURE_SIZE` 在不同上下文里不一样（本机见过 2048，也见过 32768），
而一屏是 2560×1600，整幅上传在 2048 的上下文里会直接 panic。现在按 `Painter::max_texture_side()`
切块，每块一个纹理、画一个四边形。

**瓦片坐标是「帧像素」，画笔吃的是「逻辑点」。** 二者差一个 `scale_factor`（本机 1.333）。
漏了换算的话图会被放大 1.333 倍再被窗口裁掉一圈，看起来像「自己桌面的放大残片」，根本不像是截图
（实测踩过，用「后缓冲 vs 帧」按不同缩放系数采样比对定位：k=1.0 误差 1.8，k=0.75 误差 38.9）。

**一帧内的 `textures_delta` 必须在这一帧交给 painter。** 瓦片纹理是在 `redraw()` 开头新建的，
靠同一帧的 `paint_and_update_textures` 上传；如果中间因为 `inner_size()==0` 之类提前 `return`，
delta 就永久丢了 —— 表现是整屏黑且永不恢复。所以「能不能渲染」的检查全部提到建纹理之前。

**新出现的 `Area` 会晚一帧上屏。** egui 0.37 是多趟布局（multi-pass）：一趟里新出现的 Area 尺寸未知，
egui 会让这一趟作废重排；而我们每帧只跑一趟 `run_pass`，所以工具栏第一帧不画，第二帧才出现
（16ms，无感）。调试时别被这个骗了 —— 要 dump 就 dump 稳定后的帧（`SHOTRS_DUMP` 现在等 3 帧）。

**导出走读回，分两拍。** 选区内没有压暗遮罩，直接从后缓冲 `read_screen_rgba` 裁出选区就是成品
（含标注），分辨率是物理像素，比 1x 的源图还高。但第一拍只把覆盖层（工具栏 / HUD / 提示 / 选框边框
/ 文字输入框）关掉并请求重绘，第二拍才真正读回 —— 否则选中区域盖住工具栏时，工具栏会被拍进图里。

**空闲时让事件循环睡下去。** `ControlFlow::Wait` 在 `Idle` 用，取帧/编辑期间才切 `Poll`；
否则一个小工具会空转一个核。注意取帧期间窗口是透明的、可能收不到重绘事件，所以 channel 是在
`about_to_wait` 里轮询的。

**析构顺序。** egui_glow 的 `Painter::drop` 要 `delete` program/buffer/texture，必须赶在 GL context
销毁之前，否则退出时在已销毁的上下文上调用 GL 函数 → 段错误（实测 `exit=139`）。所以给 `App` 写了
显式 `Drop`，顺序是 painter → gl/surface → egui_state → window。

---

## 单屏而非跨屏

抓的是整桌面，但按全屏窗口的 `inner_size()` 裁出当前屏。每个窗口只有自己的 `devicePixelRatio`，
混合 DPI 天然隔离，重绘面积也减半。

---

## 标注是插件

主程序**不认识**「箭头」「文字」，只认五个 trait：

| trait | 谁实现 | 职责 |
|---|---|---|
| `Tool` | 每种标注工具一个 | 声明名字/快捷键/**排序权重**/提示；按下时给出草稿（`begin`）或内联编辑器（`click`）；可选地交出属性面板（`settings`） |
| `Draft` | 拖动型工具 | 拖动中每帧被调 `to()`，松手 `finish()` 决定落不落定 |
| `InlineEditor` | 需要 IME 的工具 | 自己摆 `Area + TextEdit`，`finish()` 交出标注 |
| `Settings` | 有可调参数的工具 | 把工具自己身上的参数（颜色/线宽/字号/马赛克块大小）摆成控件 |
| `Shape` | 落定后的标注 | `draw()` / `bounds()` / `contains()`（命中测试）/ `translate()`（拖动移动）、`set_bounds()`（缩放）；主程序负责顺序、裁剪、撤销 |

现有工具：`选区`（平移）、`箭头`、`矩形`、`椭圆`、`马赛克`、`文字`。

**新增一种标注只需要**：在 `src/plugins/` 下加一个文件实现对应 trait，再在 `plugins::register()` 里加一行。
不需要动 `main.rs` 的状态机、工具栏或绘制代码。「颜色/线宽」这类参数存在工具结构体里，
所以 `Tool::settings()` 直接把 `self` 交出来即可（`impl Settings for ArrowTool`）。

**标注可以选中 / 拖动 / 缩放 / 删除**：

- 点一下就**选中**它 —— 右上角浮出「×」删除按钮，包围盒出现 8 个缩放手柄
- 拖它本体 = 移动；拖手柄 = 缩放（`Shape::set_bounds`，各插件自己决定怎么响应：
  矩形/椭圆改对角点、文字改字号、马赛克改矩形）
- 点「×」或按 `Backspace`/`Delete` = 删除选中的那条
- 撤回/恢复用**整屏快照**（`Shape::clone_box`），栈深 64；右上角「撤回 / 恢复」按钮与
  `Z` / `Shift+Z` 等价
- **画完不会自动选中**：否则新标注的「×」就悬在右上角，下一次落笔只要靠近那里就会误删
  （实测踩过这个坑）

按下时的优先级：选区手柄 > 平移选区 > 删除按钮 > 标注手柄 > 标注本体 > 画新东西 > 重选；
光标跟着变（缩放箭头 / 抓手 / 移动 / 手型）。

**标注的命中测试**：按下时先做命中测试，点到已有标注就拖动它，点空白才画新的。命中测试不是简单用
包围盒，而是贴着形状本身 —— 箭头 = 线段的带宽、矩形/椭圆 = 描边带、马赛克/文字 = 区域。
否则画一块大矩形之后，里面的任何位置都会被它「吃掉」，没法再画别的东西。

**标注不做成独立组件。** egui 是立即模式，一帧里所有标注就是几个 `Shape`，GPU 一起批处理。
不存在 mark-shot 那种「父控件要为透明子控件补背景」的开销。

### 马赛克要读原图像素

马赛克是唯一需要读原图像素的工具，所以协议里多了两个可选钩子：

- `Tool::set_canvas(Option<Arc<CanvasSource>>)`：冻结帧就绪时把**帧 RGBA**（`Arc` 零拷贝共享）推给工具
- `Tool::set_canvas_screen(Rect)`：**每帧**同步编辑器当前屏幕矩形

马赛克落定时按块大小把区域分格、每格取源图平均色做成一张小纹理，再用 `NEAREST` 放大铺回去 ——
一次 draw call，导出时就是后缓冲里的真实像素。

> 为什么换算要每帧同步，而不是建快照时算一次：建快照那一帧窗口可能还没完成全屏切换，
> `viewport_rect()` 还是小窗口尺寸，换算系数会被放大好几倍 —— 采样全跑到画面外，
> 马赛克就变成一块和背景无关的暗斑（用户反馈「没有效果」就是这么来的）。现在换算放在
> `CanvasMap::new(frame_size, screen)` 里，用「此刻」的屏幕矩形算，采样才对着正确的位置。

---

## 选区可以反复调

选区是个正经的可编辑对象，实现在 `src/selection.rs`（只管「一个矩形 + 指针」，跟标注插件无关）：

- **命中测试**：`hit(sel, p)` 返回 `Handle(边/角)` / `Inside` / `Outside`，角优先于边；
  抓取带宽 8pt，所以「点边框」不需要点得很准
- **缩放**：`drag_rect(rect0, Grab::Resize(h), from, to, screen)` —— 以**按下那一刻**的矩形为
  基准算（否则每帧叠加会飞），最小尺寸 12pt，拖过头会把对边推回来而不是翻转，最后夹进屏幕
- **平移**：`Grab::Move` 保持尺寸，只把位置夹回屏幕
- **重选**：拖到选区外即 `Grab::New`（`R` 已经分给矩形工具了）

优先级（画布上按下时）：把手 > 平移（选区工具 / 空格）> 插件画东西 > 选区外重选。
光标形状就是这个规则的提示：边框上是缩放箭头，能平移时是抓手，否则是十字。

**为什么加了「选区」工具**：默认工具如果是箭头，刚框完手一抖拖动就画出一根箭头。
现在「选区」是工具栏第一项且默认选中（`Tool::order() = 5`），框完直接拖动就是平移；
要标注时点「箭头 / 文字」或按 `A` / `T`。缩放不受工具影响，随时可用。

---

## 工具栏

`src/toolbar/mod.rs` 里的 `Toolbox` 负责三件事，主程序只管调 `toolbox.ui(ctx, screen)` 然后
拿回它占用的矩形（用来把「点在工具栏上」从画布交互里排除）：

- **按钮栏**：条目、文案、快捷键全部来自注册表，用自绘按钮（需要 drag 语义）
- **排序**：出厂顺序按 `Tool::order()`；运行期可**拖动按钮重排**，鼠标越过谁的中心就跟谁换位，
  当前工具会跟着自己的按钮走（排序不做持久化）
- **属性面板（子工具栏）**：`Tool::settings()` 返回 `Some` 时，在按钮栏正下方画一块子工具栏；
  返回 `None` 的工具就没有面板。每个工具一套 `push_id` 作用域，切工具时滑块/取色器状态不会串

**别把浮层点击漏给画布。** 判断「这一下是不是点在 UI 上」不能只看工具栏矩形：
egui 的取色器是**独立的弹出 Area**，弹在面板之外，按老写法点它会被当成画布按下 ——
顺手就把选区边框拖走了。现在用 `ctx.layer_id_at(pos)`（+ 工具栏矩形兜底）：
指针只要落在任何非 `Background` 的层上（弹窗、菜单、提示），画布交互一律让开。

按钮命中还有一个坑（写自动化测试时会撞上）：egui 的命中测试用的是**上一帧**的指针位置，
所以「移动 → 按下 → 抬起」必须分三帧，挤在一帧里只有 hover 没有 click。

**`demo.rs` 是这套插件的回归测试**：它往 `RawInput` 里逐帧塞合成事件（点「截图」按钮 → 框选 →
拖边框缩放 → 平移选区 → 选区外拖动重选 → 画箭头 → 按 `T` → 点击 → 逐字输入中文 → 回车 →
换颜色 → 再画一根 → 点工具栏切工具 → 拖动排序），走的是完全真实的代码路径，最后导出一张图。
改完插件跑一遍就知道有没有写坏：

```bash
SHOTRS_DEMO=1 SHOTRS_DUMP=1 ./target/release/shotrs   # 导出 + dump 帧/后缓冲
```

---

## 托盘 / 最小化 / 全局快捷键 / 单实例

**最小化 ≠ `set_minimized`。** Wayland 下窗口最小化后无法恢复（winit 原文：
`Unminimizing is ignored on Wayland.`），所以「收进托盘」的实现是**销毁窗口、进程留在后台**，
靠托盘菜单 / 快捷键把窗口建回来。

**恢复时 GL 上下文和 Painter 是复用的**：`GlState` 里把「上下文」和「surface」拆开，
销毁时 `make_not_current()` 挂进 `idle`，恢复时新建 window surface 再 `make_current`。
这不是优化，是修两个 bug 的结果：

| 现象 | 原因 |
|---|---|
| 恢复后是**黑屏窗口** | 早先每次恢复都重建 GL 上下文 + Painter，纹理全空；而 `egui::Context::set_fonts` 对**相同定义会跳过**（egui 0.37 源码 `context.rs:2508-2515`），字体图集不会再上传。egui_glow 连纯色三角形都要采样那张图集（`white_uv`），缺了它整帧什么都画不出来，只剩 `glClear` 的黑 |
| 恢复**很慢**、要等鼠标划过才出现 | 新建窗口不会自己产生 `RedrawRequested`，没主动 `request_redraw()` 就一帧也不画；另外重建中文图集本身要 ~100ms |

现在恢复从 ~165ms 降到 **~23ms**，第一帧就是完整界面（实测日志 `控制窗口恢复 640x496，耗时 23 ms（复用 GL 上下文）`）。

连带修掉的两个问题：没有窗口时 `RedrawRequested` 不会再来，所以「退出」请求改在
`about_to_wait` 里处理（否则收进托盘后从菜单点「退出」没反应）；无窗口时上下文处于挂起态，
析构前不能调 GL —— 直接 `mem::forget` 掉 Painter，避免在非当前上下文上调用 GL 函数。

**顶部栏图标**走 StatusNotifierItem（ksni），GNOME 靠 `ubuntu-appindicators` 扩展显示。
左键点图标 = 直接截一张；右键菜单：`截图 / 截图屏幕（子菜单）/ 显示窗口 / 收进托盘 / （勾选）关闭时最小化到托盘 / 退出`。

### 截哪块屏

`Fullscreen::Borderless(None)` 由合成器定屏，而它看的是控制窗口在哪 —— 从托盘/快捷键触发时
窗口是刚建出来的，Mutter 一般把它放主屏，于是**副屏永远截不到**，除非先把窗口拖过去。

现在：控制窗口和托盘菜单里都有「截图屏幕」（配置项 `capture_display`），选了就
`Fullscreen::Borderless(Some(m))` 直接钉在那块屏上（Mutter 认这个参数，实测 eDP-2 / DP-2 都能选），
没选就还是「跟随窗口」（`pick_monitor`：`SHOTRS_MONITOR` > 配置 > `current_monitor()`）。

显示器列表由主线程缓存（`App::displays`）：窗口销毁后查不到 `available_monitors()`，托盘菜单还得靠它；
另外改过缩放/插拔之后 winit 会把同一块屏报两遍，按名字去重。

**托盘发起的截图导完回托盘**：`request_screenshot` 记下「窗口本来在不在」（`return_to_tray`），
导出/取消后走 `destroy_window()` 而不是退回小窗口。不然 Mutter 还会放一段退全屏动画，
看起来就是「截完图冒出一个客户端」。

### 全局快捷键

`org.freedesktop.portal.GlobalShortcuts` 在本机（xdg-desktop-portal 1.21）对**非沙箱应用直接拒绝**
（`NotAllowed: An app id is required`，host 应用没有 app_id；重启 portal、带
`GIO_LAUNCHED_DESKTOP_FILE`、用 `gtk-launch` 启动都不行）。所以走 GNOME 官方支持的
经典做法 —— 让 shell 管按键：

1. 控制窗口里那行命令（也有「复制命令」按钮）：
   ```
   <仓库目录>/target/release/shotrs --screenshot
   ```
2. **设置 → 键盘 → 键盘快捷键 → 查看及自定义快捷键 → 自定义快捷键 → +**，粘到「命令」里，
   再按下想要的组合键 —— 这就是「自定义快捷键」。
3. 命令通过 `$XDG_RUNTIME_DIR/shotrs-<uid>.sock` 转给已运行实例；窗口若在托盘里会**自动重建再截图**。

**单实例**：重复启动不会出现第二个托盘图标，而是把已有窗口叫出来（socket 发 `show`）。
代码里仍保留 portal 的实现（`src/hotkey.rs`），在支持 host 应用的环境里会自动绑上。

---

## 复制 / 保存 / 配置

三件事各管各的，代码分别在 `utils/` 下：

| 动作 | 触发 | 实现 | 落盘？ |
|---|---|---|---|
| 复制 | `Enter` | `utils::clipboard::copy(Payload)`，载荷由 `clipboard_mode` 决定 | `image` 模式否，其余落盘 |
| 保存 | 右上角「保存」按钮 或 `S` | `utils::save::save_into(&Bitmap, dir)` | 是，存到配置目录 |
| 配置 | 控制窗口里改设置 | `utils::config::Config::save()` | 是，写回 JSON |

界面层（`egui::ColorImage`）和这两件事之间用 `utils::Bitmap` 解耦，所以剪贴板/存盘都不认识 egui。

**配置文件**（`utils::config::Config`，JSON）：

```json
{
  "save_dir": "/home/you/图片/shotrs",
  "close_to_tray": true,
  "hotkey": "CTRL+ALT+S",
  "clipboard_mode": "image",
  "clipboard_max_side": 1920,
  "capture_display": "DP-2"
}
```

`clipboard_mode` 取 `image` / `bitmap` / `file` / `path`；`clipboard_max_side` 为 0 表示原尺寸。
`close_to_tray` 决定 `×` 的行为；`hotkey` 是 portal 那条路的首选按键（portal 不可用时不起作用，
真正的键位在 GNOME 的自定义快捷键里）；`capture_display` 是「截哪块屏」的显示器名字，
`null` = 跟随控制窗口所在的那块屏。

查找顺序：

1. `$SHOTRS_CONFIG` —— 显式路径，测试/多套配置用
2. `$XDG_CONFIG_HOME/shotrs/config.json`
3. `$HOME/.config/shotrs/config.json`

文件不存在或 JSON 坏掉，都退回默认值（`~/图片/shotrs`，英文系统 `~/Pictures/shotrs`）并把原因打到
stderr —— 配置坏掉不该让截图工具起不来。控制窗口里那行小字显示的就是当前用的配置文件路径。

**为什么不自动保存**：Enter 是「拿了就走」（粘贴到别处），磁盘上不该悄悄多出文件；
需要留档时按 `S` 或点「保存」，保存完仍留在编辑器里，可以接着改选区再存一次。

### 复制到远端（Remmina / RDP / VNC）

图像复制现在通过独立的 GTK 剪贴板进程，同时宣告 `image/png`、`image/bmp` 等可转换格式。
像普通图片应用一样，粘贴方可以选择自己支持的格式；内容均为 RGB 无 alpha。
子进程在下一次复制替换剪贴板时退出，关闭截图窗口后仍可粘贴。

之前仅用 `wl-copy --type image/png`，即使 PNG 已改为 RGB，仍然只宣告 PNG。
[Remmina 1.4.43 源码](https://gitlab.com/Remmina/Remmina/-/blob/v1.4.43/plugins/rdp/rdp_cliprdr.c)
明确按 MIME 映射：`image/png` → `CB_FORMAT_PNG`，`image/bmp` → `CF_DIB`；
**PNG 的 alpha 不决定宣告 CF_DIB / CF_DIBV5。** 因而原先将根因归于 alpha 的解释不成立。
「先粘到微信再复制」可能改变了可用格式，必须对比格式列表，不能只比较 PNG 颜色类型。

多格式复制需要系统 Python 的 PyGObject / GTK 3（Ubuntu 包：`python3-gi`、`gir1.2-gtk-3.0`）
和 X11 / XWayland。helper 通过 XWayland 持有剪贴板，Wayland 应用可通过合成器桥接读取。
如果 helper 不可用，则交给 `wl-copy --type image/bmp` 持有 **24 位 BMP**，状态中会说明。
两种方式均失败时明确报告错误。

| 模式 | 剪贴板内容 | 注意 |
|---|---|---|
| `image`（默认） | PNG + BMP 等图像格式 | 远程客户端须开启并支持图像同步；本机宣告正确不等于远端验证通过 |
| `bitmap`（远程位图） | 仅 `image/bmp`，24 位 RGB | 用于远程画图等程序；减少宣告格式仍不能消除大位图的传输等待 |
| `file` | `text/uri-list` 本地文件 URI | 是否传文件取决于客户端，可能仅粘出路径；不保证 Windows CF_HDROP |
| `path` | `text/plain;charset=utf-8` 本地绝对路径 | 只复制路径，不传输文件 |

`file` / `path` 模式先将 PNG 存到 `save_dir`，`image` / `bitmap` 模式不落盘。
文件模式写入失败会直接报错，不再偷偷降级成路径文本。

控制窗口的「复制尺寸」可选原图 / ≤1920 / ≤1280 / ≤640 / ≤320；
缩小会降低复制图像的分辨率，存盘仍保留原尺寸。

本机验证可用 `wl-paste --list-types` 和 `xclip -selection clipboard -out -target TARGETS`，
应同时看到 `image/png` 和 `image/bmp`；还应实际读取、解码这两种格式。
端到端测试请在同一远程连接先向画图粘贴，再向目标应用粘贴，以区分同步通道和应用兼容性。

如果远端画图提示「剪贴板上的信息无法插入画图」，不要仅凭本机解码成功判断远端兼容。
本机曾记录到 Remmina 原生 Wayland 进程退出，最后一条日志为
`Error flushing display: 资源暂时不可用`；可用 `sh tools/remmina-x11.sh`
启动 XWayland 版本作对照。脚本使用独立应用 ID 启动新进程，不向旧进程转交请求，
也不主动断开已有连接；连接远程电脑仍由用户在新窗口中操作。
已在本机验证：用户通过 X11 实例向远端画图粘贴成功；多格式复制每次等待超过 30 秒，
同一张 1028×926 图临时改为仅 BMP 后等待 5–30 秒，数据为 2,855,838 字节。
保持 BMP 格式缩到 128×115（44,214 字节）后，用户反馈 1–2 秒即可粘贴。
大小对照支持「图像数据量影响剩余等待」的判断；仅 BMP 对照还改变了持有后端，
不能单凭这轮对照断定格式协商就是唯一原因。
本机同一张图取 PNG 约 6ms、BMP 约 17ms；
GTK `wait_for_image()` 约 22ms，GdkPixbuf 解码和重编码均约 20ms。
现有日志尚不足以定位剩余的远端等待；脚本额外启用 cliprdr 的 DEBUG 日志，
用于记录服务端请求的格式及客户端响应大小。

`tools/org.remmina.Remmina.ShotrsX11.desktop` 提供单独的「Remmina（X11 剪贴板）」入口，
可安装到 `~/.local/share/applications/`；使用独立应用 ID，避免启动请求被原生 Wayland 旧进程接管。

---

## 图标 / app_id

窗口图标走 `Window::set_window_icon`（内嵌 `assets/shotrs-256.png`），**只在 X11/XWayland 下真正显示** ——
Wayland 没有「窗口图标」这个概念，那条路是 `app_id`：

- 程序启动时把 app_id 设成 `shotrs`（`WindowAttributesExtWayland::with_name`），启动日志会打印 `app_id = shotrs`
- GNOME 在 `~/.local/share/applications/` 里找同名 `.desktop`，读它的 `Icon=` 来在 dock / 活动视图里显示图标

所以想让图标出现在 dock 上，跑一次：

```bash
./assets/install-icon.sh     # 拷贝各尺寸 PNG 到 ~/.local/share/icons/hicolor/*/apps/
                             # 并安装 shotrs.desktop（Exec 指向本机 target/release/shotrs）
```

**不装就什么都不会变** —— 程序没法给 Wayland 窗口设置图标，必须靠系统里那份 `.desktop` 加 PNG。
装完后再启动 `./target/release/shotrs`（或 `cargo run`），dock 里就会显示这个图标并带「运行中」小圆点。
已实测确认：`WAYLAND_DEBUG=1` 里能看到 `xdg_toplevel.set_app_id("shotrs")`，与 `StartupWMClass=shotrs` 对得上。

`Categories` 只能写一个主分类（`Graphics;`）：写成 `Graphics;Utility;` 会被 `desktop-file-validate`
警告「可能重复出现在应用菜单里」。

改图标：编辑 `assets/shotrs.svg`，再重新导出（用 Chrome headless 渲染，比 ImageMagick 自带的
SVG 渲染器靠谱，后者会把渐变画成灰度）：

```bash
google-chrome --headless --disable-gpu --default-background-color=00000000 \
  --window-size=512,512 --screenshot=assets/shotrs-512.png "file://$PWD/assets/shotrs.svg"
for s in 32 48 64 128 256; do magick assets/shotrs-512.png -resize ${s}x${s} -strip assets/shotrs-$s.png; done
```

---

## 实测数据（2026-11-02 / 11-03，GNOME 50.1 / Wayland / fcitx5）

别的环境未必一致。两块屏：主屏 eDP-2 2560×1600@1.333 + 副屏 DP-2 1920×1080；
副屏 11-03 期间从 1.0 改到过 1.5（后来又改回 1.0），两套数据都留着：

| 项 | 结果 |
|---|---|
| portal 取帧画幅 | 副屏@1.0：**5120×1600**；副屏@1.5：**4800×1800**（= (1920+1280)×1.5 —— 位图**不是** 1x，是「逻辑 × 会话最大缩放」） |
| 裁剪后 | 副屏@1.0：主屏 2560×1600；副屏@1.5：主屏裁到 2880×1800 再重采样回 **2560×1600**，副屏 1920×1080 |
| 重采样耗时 | 2880×1800 → 2560×1600（Lanczos3）85–130 ms |
| 单次取帧耗时 | 约 0.6–1 秒（portal 往返，与语言无关） |
| 窗口 buffer | 主屏 2560×1600，`scale_factor()` 报 1.333（viewport 1920×1200 逻辑点）；副屏 1920×1080 @1.5（viewport 1280×720） |
| GL 纹理上限 | 见过 2048 与 32768 两种（故必须切瓦片） |
| 导出分辨率 | 主屏 2560×1600（从后缓冲读回，含标注）；副屏 1920×1080 |
| 导出正确性 | 与源图同区域平均绝对差 **0.35**；垂直翻转版 13.55 → 方向正确 |
| 托盘恢复耗时 | 165ms → **23ms**（复用 GL 上下文 + 主动 request_redraw） |
| 连拍稳定性 | `SHOTRS_LOOP=20` 实测 21 张 0 失败 |
| 中文输入 | fcitx5 在 `text-input-v3` 上可用（按 `T` 点一下切中文即可输入） |

---

## 自检 / 调试开关

| 环境变量 | 作用 |
|---|---|
| `SHOTRS_SMOKE=1` | 全自动：启动即取帧、整屏框选、导出后退出（验证链路） |
| `SHOTRS_AUTOSHOT=1` | 启动约 2s 后自动取帧，但**不**自动导出、不回控制窗口（无鼠标看显示效果） |
| `SHOTRS_DEMO=1` | 全自动：点「应用」写配置 → 点「截图」按钮 → 框选 → 拖边框缩放 → 平移 → 选区外拖动重选 → 箭头 → 文字（中文）→ 换颜色 → 再画一根 → 矩形 → 椭圆 → 马赛克 → 切工具 → 拖按钮排序 → `S` 存盘 → 复制并退出 |
| `SHOTRS_MONITOR=<名字片段>` | 调试：强行把全屏窗口钉到某块屏（如 `DP-2`），优先于配置里的「截图屏幕」 |
| `SHOTRS_CONFIG=<path>` | 指定配置文件路径（默认 `~/.config/shotrs/config.json`） |
| `SHOTRS_FAKE_MONITOR="x,y,逻辑宽,逻辑高,ppp[,桌面缩放]"` | 假装当前屏是这块屏，用来在单屏上复现副屏/混合缩放的裁切 |
| `SHOTRS_LOOP=n` | 连拍自检：自动截 n+1 张、每张自动复制，跑完退出（复现「截图卡死」那类问题，`exit=0` 即通过） |
| `SHOTRS_DUMP=1` | 冻结帧 → `/tmp/shotrs-frame.png`；编辑器稳定后 → `/tmp/shotrs-backbuffer.png`；空闲态 → `/tmp/shotrs-control.png`（控制窗口 UI 快照）；演示模式则每帧一张 `/tmp/shotrs-demo-NN.png` |

`SHOTRS_DUMP` 那两张图是排「显示不对」最快的手段：可直接按不同缩放系数采样比对，判断是自己画错了
还是帧本身就是错的。混合缩放的坑优先用 `examples/monitors.rs` / `examples/probe.rs` 这两个探针对真值
（见上文「探针」）。

`SHOTRS_SMOKE=1` 会打印 `已复制 2560x1600 到剪贴板` 然后自己退出（正常模式是回到小窗口）。
脚本里想验证存盘，就再按一次 `S`。

---

## 已知限制

- 全局快捷键要靠 GNOME 的「自定义快捷键」绑定命令（portal 对 host 应用不放行）
- 截哪块屏只能显式指定（跟随窗口 / 菜单里选一块），没有「跟随鼠标」：Wayland 客户端拿不到全局指针坐标；
  可行路线见上文「想『跟随鼠标』时的可行路线」（未实现）
- portal 取帧约 1s，这是链路固有成本，换语言不会更快；要压掉它得预热一条 PipeWire 常驻流
- 裁剪按 logical 坐标算；portal 位图按「会话最大缩放」渲染，缩放比当前屏大的屏裁完要重采样回设备像素
  （多约 100 ms）
- 滚动截图、OCR、录屏都没有
- 没有 `--display` 之类的命令行选择：截哪块屏走配置 / 托盘菜单
