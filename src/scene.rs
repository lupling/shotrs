//! 文档模型：一屏截图上的一堆标注。
//!
//! 只保存 `Box<dyn Shape>`，不关心具体类型 —— 具体怎么画由插件自己实现。
//!
//! 撤回/恢复用的是**整屏快照**：标注数量很少（最多几十条），每次改动前把整份 items
//! 克隆一份压栈即可，实现简单且不会出现「反向操作写错」的 bug。快照靠
//! `Shape::clone_box`，所以每个插件都要实现它。

use egui::{Painter, Pos2, Rect, Vec2};

use crate::plugins::{Draft, Shape};

#[derive(Default)]
pub struct Scene {
    items: Vec<Box<dyn Shape>>,
    /// 撤回栈：每一项是「改动前」的完整标注列表
    undo: Vec<Vec<Box<dyn Shape>>>,
    /// 恢复栈
    redo: Vec<Vec<Box<dyn Shape>>>,
    /// 当前选中的标注下标（选中后才显示删除按钮和缩放手柄）
    pub selected: Option<usize>,
}

/// 历史上限，防止长时间编辑把内存吃满
const MAX_HISTORY: usize = 64;

impl Scene {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn bounds_of(&self, index: usize) -> Option<Rect> {
        self.items.get(index).map(|s| s.bounds())
    }

    /// 开始一次可撤回的改动：把当前状态压进撤回栈，并清空恢复栈
    pub fn begin_edit(&mut self) {
        let snapshot: Vec<Box<dyn Shape>> = self.items.iter().map(|s| s.clone_box()).collect();
        self.undo.push(snapshot);
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// 追加一条（`None` = 插件判定作废，比如拖得太短）
    pub fn push(&mut self, shape: Option<Box<dyn Shape>>) {
        if let Some(shape) = shape {
            self.begin_edit();
            self.items.push(shape);
            // 不自动选中：否则「刚画的标注」的删除按钮就悬在右上角，
            // 下一次落笔只要靠近那里就会把它误删（实测踩过）。
            // 想删/想缩放，先点它一下选中。
            self.selected = None;
        }
    }

    /// 删除第 index 条
    pub fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.begin_edit();
        self.items.remove(index);
        self.selected = None;
        eprintln!("[shotrs] 删除标注 #{index}，剩 {} 条", self.items.len());

    }

    /// 删除当前选中的那条
    pub fn remove_selected(&mut self) -> bool {
        match self.selected {
            Some(i) => {
                self.remove(i);
                true
            }
            None => false,
        }
    }

    /// 撤回。返回是否真的撤了
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.items, prev);
        self.redo.push(current);
        self.selected = None;
        eprintln!("[shotrs] 撤回 → 剩 {} 条", self.items.len());
        true
    }

    /// 恢复
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.items, next);
        self.undo.push(current);
        self.selected = None;
        eprintln!("[shotrs] 恢复 → 剩 {} 条", self.items.len());
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// 命中测试：从上往下找，返回最上面那条被点中的标注
    pub fn hit(&self, p: Pos2) -> Option<usize> {
        self.items.iter().rposition(|item| item.contains(p))
    }

    /// 整体平移第 i 条（拖动已有标注；调用方负责在前面 `begin_edit`）
    pub fn translate(&mut self, index: usize, delta: Vec2) {
        if let Some(item) = self.items.get_mut(index) {
            item.translate(delta);
        }
    }

    /// 把第 i 条缩放到新的包围盒（拖角把手；调用方负责 `begin_edit`）
    pub fn set_bounds(&mut self, index: usize, rect: Rect) {
        if let Some(item) = self.items.get_mut(index) {
            item.set_bounds(rect);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.undo.clear();
        self.redo.clear();
        self.selected = None;
    }

    pub fn draw(&self, painter: &Painter) {
        for item in &self.items {
            item.draw(painter);
        }
    }

    /// 拖动中的草稿（画在最上面）
    pub fn draw_draft(painter: &Painter, draft: &Option<Box<dyn Draft>>) {
        if let Some(d) = draft {
            d.draw(painter);
        }
    }
}
