//! 合成输入脚本（`SHOTRS_DEMO=1`）。
//!
//! 没有鼠标/键盘也能验证「插件 → 工具栏 → 状态机 → 绘制 → 导出」整条链路：
//! 每帧往 `RawInput` 里塞一小段事件，模拟一次完整操作 ——
//! 点「截图」按钮 → 框选 → 拖边框缩放 → 平移选区 → R 重选
//! → 画箭头 → 按 T 切到文字工具 → 点击 + 中文逐字输入 → 回车
//! → 在属性面板里换颜色 → 再画一根（应变成新颜色）→ 点工具栏切工具
//! → 拖动按钮重排 → 导出。
//!
//! 走的是真实代码路径（不走任何后门），所以它能当成插件的回归测试用。

use egui::{Event, Key, Modifiers, PointerButton, Pos2};

/// 控制窗口里「应用」按钮的中心（逻辑点，把保存目录写回 JSON）
const APPLY_BUTTON: Pos2 = Pos2::new(441.3, 254.2);
/// 控制窗口里「截图」按钮的中心（逻辑点）
const SHOT_BUTTON: Pos2 = Pos2::new(246.0, 99.8);
/// 属性面板里「蓝」色块的中心（逻辑点）。按钮栏/面板几何变了要跟着改
const BLUE_CHIP: Pos2 = Pos2::new(163.0, 134.2);
/// 按钮栏里第二个按钮（文字）的中心
const TEXT_BUTTON: Pos2 = Pos2::new(419.8, 87.2);

pub struct Script {
    step: usize,
    /// 点完「截图」后要等 portal 取帧 + 全屏就绪，期间不推进脚本
    waiting_editor: bool,
}

impl Script {
    pub fn new() -> Self {
        Self {
            step: 0,
            waiting_editor: false,
        }
    }

    /// 当前步号（调试 dump 用）
    pub fn step(&self) -> usize {
        self.step
    }

    /// 注入这一帧的事件。`editor_ready` = 冻结帧已上屏（全屏编辑器就绪）。
    /// 返回 true 表示脚本已播完（调用方可以导出了）
    pub fn tick(&mut self, raw: &mut egui::RawInput, editor_ready: bool) -> bool {
        if self.waiting_editor {
            if !editor_ready {
                return false;
            }
            self.waiting_editor = false;
        }

        let step = self.step;
        self.step += 1;

        // 注意：egui 的命中测试用的是「上一帧」的指针位置，所以点控件必须
        // 移动 → 按下 → 抬起 分三帧，挤在一帧里只会有 hover 没有 click。
        let events = match step {
            // 空跑一帧：egui 是多趟布局，界面上第一帧还没有任何控件（Area 第二帧才出现）
            0 => Vec::new(),
            // 在控制窗口里点「应用」，把保存目录写回本地 JSON（验证配置落盘）
            1 => vec![Event::PointerMoved(APPLY_BUTTON)],
            2 => press(APPLY_BUTTON),
            3 => release(APPLY_BUTTON),
            // 再走真实路径：合成点击「截图」按钮
            4 => vec![Event::PointerMoved(SHOT_BUTTON)],
            5 => press(SHOT_BUTTON),
            6 => {
                // 点完就等取帧 + 全屏，别把后面的步骤挤进「正在取帧」阶段
                self.waiting_editor = true;
                release(SHOT_BUTTON)
            }
            // 框选出一块区域
            7 => press(Pos2::new(300.0, 200.0)),
            8 => vec![Event::PointerMoved(Pos2::new(760.0, 420.0))],
            9 => release(Pos2::new(760.0, 420.0)),
            // 拖右边框中点：选区应向右长到 830
            10 => vec![Event::PointerMoved(Pos2::new(760.0, 310.0))],
            11 => press(Pos2::new(760.0, 310.0)),
            12 => vec![Event::PointerMoved(Pos2::new(830.0, 310.0))],
            13 => release(Pos2::new(830.0, 310.0)),
            // 选区内拖动（此时工具是「选区」）：整体平移
            14 => vec![Event::PointerMoved(Pos2::new(400.0, 250.0))],
            15 => press(Pos2::new(400.0, 250.0)),
            16 => vec![Event::PointerMoved(Pos2::new(350.0, 220.0))],
            17 => release(Pos2::new(350.0, 220.0)),
            // 在选区外面按下并拖动 = 直接重选一块
            18 => vec![Event::PointerMoved(Pos2::new(100.0, 900.0))],
            19 => press(Pos2::new(100.0, 900.0)),
            20 => vec![Event::PointerMoved(Pos2::new(500.0, 1000.0))],
            21 => release(Pos2::new(500.0, 1000.0)),
            // R 重选：回到框选阶段，重新拉一块
            22 => key(Key::R),
            23 => press(Pos2::new(300.0, 200.0)),
            24 => vec![Event::PointerMoved(Pos2::new(760.0, 420.0))],
            25 => release(Pos2::new(760.0, 420.0)),
            // 切到箭头工具，画一根
            26 => key(Key::A),
            27 => press(Pos2::new(340.0, 240.0)),
            28 => vec![Event::PointerMoved(Pos2::new(700.0, 380.0))],
            29 => release(Pos2::new(700.0, 380.0)),
            // 切到文字工具，点击 + 逐字输入中文
            30 => key(Key::T),
            31 => press(Pos2::new(380.0, 300.0)),
            32 => release(Pos2::new(380.0, 300.0)),
            33 => text("插"),
            34 => text("件"),
            35 => text("化"),
            36 => text("标"),
            37 => text("注"),
            38 => key(Key::Enter),
            // 切回箭头，在属性面板里点「蓝」色块
            39 => key(Key::A),
            40 => vec![Event::PointerMoved(BLUE_CHIP)],
            41 => press(BLUE_CHIP),
            42 => release(BLUE_CHIP),
            // 再画一根箭头 —— 应该是刚选的蓝色
            43 => press(Pos2::new(310.0, 310.0)),
            44 => vec![Event::PointerMoved(Pos2::new(720.0, 400.0))],
            45 => release(Pos2::new(720.0, 400.0)),
            // 点工具栏「文字」按钮：属性面板应切成字号
            46 => vec![Event::PointerMoved(TEXT_BUTTON)],
            47 => press(TEXT_BUTTON),
            48 => release(TEXT_BUTTON),
            // 拖动「文字」按钮到最左边：验证拖动排序
            49 => vec![Event::PointerMoved(TEXT_BUTTON)],
            50 => press(TEXT_BUTTON),
            51 => vec![Event::PointerMoved(Pos2::new(60.0, 87.5))],
            52 => release(Pos2::new(60.0, 87.5)),
            // 矩形
            53 => key(Key::R),
            54 => press(Pos2::new(320.0, 215.0)),
            55 => vec![Event::PointerMoved(Pos2::new(520.0, 300.0))],
            56 => release(Pos2::new(520.0, 300.0)),
            // 椭圆
            57 => key(Key::E),
            58 => press(Pos2::new(545.0, 215.0)),
            59 => vec![Event::PointerMoved(Pos2::new(720.0, 300.0))],
            60 => release(Pos2::new(720.0, 300.0)),
            // 马赛克（要真的读原图像素，所以这块最值得看）
            61 => key(Key::M),
            62 => press(Pos2::new(320.0, 335.0)),
            63 => vec![Event::PointerMoved(Pos2::new(620.0, 405.0))],
            64 => release(Pos2::new(620.0, 405.0)),
            // 拖动已有标注：抓住矩形上边框往下拖 40,25（验证 Shape::translate）
            65 => key(Key::A),
            66 => vec![Event::PointerMoved(Pos2::new(420.0, 215.0))],
            67 => press(Pos2::new(420.0, 215.0)),
            68 => vec![Event::PointerMoved(Pos2::new(460.0, 240.0))],
            69 => release(Pos2::new(460.0, 240.0)),
            // 选中矩形（点它的上边框）
            70 => vec![Event::PointerMoved(Pos2::new(460.0, 240.0))],
            71 => press(Pos2::new(460.0, 240.0)),
            72 => release(Pos2::new(460.0, 240.0)),
            // 拖它的右下角把手 → 放大
            73 => vec![Event::PointerMoved(Pos2::new(560.0, 325.0))],
            74 => press(Pos2::new(560.0, 325.0)),
            75 => vec![Event::PointerMoved(Pos2::new(640.0, 390.0))],
            76 => release(Pos2::new(640.0, 390.0)),
            // Backspace 删除选中，再 Z 撤回、Shift+Z 恢复、Z 再撤回（最终它还在）
            77 => key(Key::Backspace),
            78 => Vec::new(),
            79 => key(Key::Z),
            80 => Vec::new(),
            81 => shift_key(Key::Z),
            82 => clear_modifiers(),
            83 => key(Key::Z),
            84 => Vec::new(),
            // 最后再测一次「浮层不吃画布事件」：点开取色器并在弹窗里点一下
            // （放最后是因为弹窗里的输入框会抢走键盘焦点，会影响上面的 Z / Backspace）
            85 => vec![Event::PointerMoved(Pos2::new(251.8, 134.2))],
            86 => press(Pos2::new(251.8, 134.2)),
            87 => release(Pos2::new(251.8, 134.2)),
            88 => vec![Event::PointerMoved(Pos2::new(350.0, 350.0))],
            89 => press(Pos2::new(350.0, 350.0)),
            90 => release(Pos2::new(350.0, 350.0)),
            // 按 S 保存到配置里的目录（落盘，留在编辑器里）
            91 => key(Key::S),
            // 空跑两帧，让「关覆盖层 → 读回 → 保存」这两拍走完
            92 => Vec::new(),
            93 => Vec::new(),
            _ => return true,
        };

        raw.events.extend(events);
        false
    }
}

fn press(pos: Pos2) -> Vec<Event> {
    vec![
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
    ]
}

fn release(pos: Pos2) -> Vec<Event> {
    vec![Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    }]
}

fn key(key: Key) -> Vec<Event> {
    vec![Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]
}

/// Shift + 某键：真实环境里 winit 会先发 `ModifiersChanged`，egui 的
/// `InputState::modifiers` 就是从它更新的，所以合成事件也要照做。
fn shift_key(key: Key) -> Vec<Event> {
    vec![
        Event::ModifiersChanged(Modifiers::SHIFT),
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        },
    ]
}

/// 松开所有修饰键（同样走 ModifiersChanged）
fn clear_modifiers() -> Vec<Event> {
    vec![Event::ModifiersChanged(Modifiers::NONE)]
}

fn text(s: &str) -> Vec<Event> {
    vec![Event::Text(s.to_owned())]
}
