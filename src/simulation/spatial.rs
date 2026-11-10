//! Пространственная сетка для поиска ближайших существ и предметов.
//!
//! Симуляция детерминированна и однопоточна, поэтому «индекс» — обычная
//! `HashMap` из ячейки в индексы элементов. Сетка переиспользуется между
//! тиками (scratch-буфер): каждый тик ячейки очищаются, а не создаются
//! заново. Поиск ближайшего в радиусе — константное число ячеек, а не
//! полный перебор всех существ (O(n²) на волков × кроликов и т.п.).

use std::collections::HashMap;

use super::world::WorldPos;

/// Сетка элементов `(id, позиция)`. При равном расстоянии ближайшим
/// считается элемент с меньшим id — результат детерминирован.
#[derive(Debug)]
pub(crate) struct SpatialGrid {
    cell_size: f32,
    items: Vec<(u64, WorldPos)>,
    cells: HashMap<(i32, i32), Vec<u32>>,
}

impl Default for SpatialGrid {
    fn default() -> Self {
        Self::new(8.0)
    }
}

impl SpatialGrid {
    pub(crate) fn new(cell_size: f32) -> Self {
        Self {
            cell_size: cell_size.max(f32::EPSILON),
            items: Vec::new(),
            cells: HashMap::new(),
        }
    }

    /// Очищает сетку, сохраняя ёмкость ячеек. Пустые ячейки не удаляются
    /// сразу (это освобождало бы их `Vec` и заставляло аллоцировать заново
    /// каждый тик); карта сбрасывается только при заметном разрастании от
    /// миграций существ.
    pub(crate) fn clear(&mut self) {
        self.items.clear();
        for cell in self.cells.values_mut() {
            cell.clear();
        }
        let limit = self.items.capacity().max(64).saturating_mul(2);
        if self.cells.len() > limit {
            self.cells.clear();
        }
    }

    pub(crate) fn insert(&mut self, id: u64, pos: WorldPos) {
        let index = self.items.len() as u32;
        self.items.push((id, pos));
        self.cells.entry(self.cell_key(pos)).or_default().push(index);
    }

    /// Убирает элемент из выдачи (например, убитый кролик), не перестраивая
    /// сетку целиком. Сама запись в `items` остаётся мусором до `clear`.
    pub(crate) fn remove(&mut self, id: u64) {
        for cell in self.cells.values_mut() {
            cell.retain(|&index| self.items[index as usize].0 != id);
        }
    }

    /// Ближайший элемент в радиусе `radius` тайлов.
    pub(crate) fn nearest(&self, pos: WorldPos, radius: f32) -> Option<(u64, WorldPos)> {
        let (min_x, min_y) = self.cell_key(WorldPos {
            x: pos.x - radius,
            y: pos.y - radius,
        });
        let (max_x, max_y) = self.cell_key(WorldPos {
            x: pos.x + radius,
            y: pos.y + radius,
        });
        let mut best: Option<(f32, u64, WorldPos)> = None;
        for cell_y in min_y..=max_y {
            for cell_x in min_x..=max_x {
                let Some(cell) = self.cells.get(&(cell_x, cell_y)) else {
                    continue;
                };
                for &index in cell {
                    let (id, item_pos) = self.items[index as usize];
                    let distance = item_pos.dist(pos);
                    if distance > radius {
                        continue;
                    }
                    let better = best.is_none_or(|(best_distance, best_id, _)| {
                        distance
                            .total_cmp(&best_distance)
                            .then_with(|| id.cmp(&best_id))
                            .is_lt()
                    });
                    if better {
                        best = Some((distance, id, item_pos));
                    }
                }
            }
        }
        best.map(|(_, id, item_pos)| (id, item_pos))
    }

    fn cell_key(&self, pos: WorldPos) -> (i32, i32) {
        (
            (pos.x / self.cell_size).floor() as i32,
            (pos.y / self.cell_size).floor() as i32,
        )
    }
}
