//! Разреженный индекс объектов по тайлам.
//!
//! ИИ ищет постройки и ресурсы в радиусе (костёр, склад, куст, камень).
//! Полный квадратный скан 32×32 тайлов на каждое «обдумывание» дорог и
//! плохо масштабируется. Индекс раскладывает объекты по ячейкам и смотрит
//! только ячейки, пересекающие радиус. Он не сохраняется: после загрузки
//! сейва и генерации строится заново одним проходом.
//!
//! Порядок обхода детерминирован: ячейки — по возрастанию координат,
//! элементы внутри ячейки — в порядке вставки (порядок чанков/слоёв).

use std::collections::HashMap;

use super::objects::ObjectKind;
use super::world::TilePos;

/// Ячейка индекса — 16×16 тайлов (половина чанка).
const CELL_TILES: i32 = 16;
/// Все стабильные виды объектов: 0–4 природные, 5–11 постройки.
const OBJECT_KINDS: usize = 12;

/// Индекс одного вида объектов.
#[derive(Debug, Default)]
struct TileIndex {
    cells: HashMap<(i32, i32), Vec<TilePos>>,
}

impl TileIndex {
    fn cell_key(pos: TilePos) -> (i32, i32) {
        (
            pos.x.div_euclid(CELL_TILES),
            pos.y.div_euclid(CELL_TILES),
        )
    }
    fn insert(&mut self, pos: TilePos) {
        self.cells.entry(Self::cell_key(pos)).or_default().push(pos);
    }
    fn remove(&mut self, pos: TilePos) {
        let key = Self::cell_key(pos);
        let Some(cell) = self.cells.get_mut(&key) else {
            return;
        };
        cell.retain(|&p| p != pos);
        if cell.is_empty() {
            self.cells.remove(&key);
        }
    }
    /// Ближайший тайл в радиусе, прошедший `accept`. Дистанция — евклидова
    /// в тайлах; при равенстве выигрывает меньший `(y, x)`.
    fn nearest(
        &self,
        center: TilePos,
        radius: i32,
        mut accept: impl FnMut(TilePos) -> bool,
    ) -> Option<TilePos> {
        if radius < 0 {
            return None;
        }
        let (min_x, min_y) = Self::cell_key(TilePos {
            x: center.x - radius,
            y: center.y - radius,
        });
        let (max_x, max_y) = Self::cell_key(TilePos {
            x: center.x + radius,
            y: center.y + radius,
        });
        let radius_sq = radius * radius;
        let mut best: Option<(i32, TilePos)> = None;
        for cell_y in min_y..=max_y {
            for cell_x in min_x..=max_x {
                let Some(cell) = self.cells.get(&(cell_x, cell_y)) else {
                    continue;
                };
                for &pos in cell {
                    let dx = pos.x - center.x;
                    let dy = pos.y - center.y;
                    let distance_sq = dx * dx + dy * dy;
                    if distance_sq > radius_sq || !accept(pos) {
                        continue;
                    }
                    let better = best.is_none_or(|(best_distance, best_pos)| {
                        (distance_sq, pos.y, pos.x) < (best_distance, best_pos.y, best_pos.x)
                    });
                    if better {
                        best = Some((distance_sq, pos));
                    }
                }
            }
        }
        best.map(|(_, pos)| pos)
    }
}

/// Индекс всех объектов мира. Для каждого стабильного вида — своя сетка,
/// поэтому поиск куста не перебирает деревья и постройки.
#[derive(Debug)]
pub(crate) struct ObjectIndex {
    by_kind: [TileIndex; OBJECT_KINDS],
}

impl Default for ObjectIndex {
    fn default() -> Self {
        Self {
            by_kind: std::array::from_fn(|_| TileIndex::default()),
        }
    }
}

impl ObjectIndex {
    fn slot(kind: ObjectKind) -> usize {
        (kind.discriminant() as usize).min(OBJECT_KINDS - 1)
    }
    pub(crate) fn insert(&mut self, pos: TilePos, kind: ObjectKind) {
        self.by_kind[Self::slot(kind)].insert(pos);
    }
    pub(crate) fn remove(&mut self, pos: TilePos, kind: ObjectKind) {
        self.by_kind[Self::slot(kind)].remove(pos);
    }
    pub(crate) fn clear(&mut self) {
        for index in &mut self.by_kind {
            index.cells.clear();
        }
    }
    pub(crate) fn nearest(
        &self,
        center: TilePos,
        radius: i32,
        kind: ObjectKind,
        accept: impl FnMut(TilePos) -> bool,
    ) -> Option<TilePos> {
        self.by_kind[Self::slot(kind)].nearest(center, radius, accept)
    }
}
