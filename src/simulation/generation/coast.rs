//! Береговая карта: единое расстояние до воды/суши и уровень ближайшей воды.
//!
//! Один проход от границы «суша/вода» даёт сразу:
//! - `distance`: положительное — расстояние суши до воды, отрицательное —
//!   глубина воды (расстояние до суши), в тайлах;
//! - `max_water_level` / `max_water_kind`: самый высокий водоём в радиусе
//!   `SHORE_RAMP_TILES` (для суши). Берём максимум, а не ближайший, чтобы
//!   суша между низким морем и высоким озером не проседала ниже озера.
//!
//! Это заменяет отдельные BFS для пляжей, прибрежной влажности и глубины
//! моря (#2.6) и гарантирует согласованную классификацию глубины у краевого
//! океана и внутренних морей: обе стороны используют одно расстояние.

use std::collections::VecDeque;

use super::height::{HeightField, HEIGHT_SUB, STONE_HEIGHT_SUB};
use super::hydrology::WaterKind;
use crate::simulation::heightmap::SEA_LEVEL;
use crate::simulation::noise::smoothstep;

/// Максимальное расстояние, которое хранится в `i8`.
const MAX_DISTANCE: i16 = i8::MAX as i16;

/// Соседи по 4 направлениям: расстояние до воды/суши по манхэттену.
const NEIGHBORS_4: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
/// Соседи по 8 направлениям: уклон почвы и проверка берегов.
const NEIGHBORS_8: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// Ширина берегового пандуса, тайлов: почва плавно опускается к воде.
const SHORE_RAMP_TILES: u8 = 8;

/// Знаковая карта расстояний до границы суши и воды.
pub(crate) struct CoastField {
    distance: Vec<i8>,
    max_water_level: Vec<u16>,
    max_water_kind: Vec<WaterKind>,
}

impl CoastField {
    /// Строит карту от границы суша/вода. Вода — любая (море, озеро, река).
    pub(crate) fn build(
        field: &HeightField,
        water: &[WaterKind],
        size: i32,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Self> {
        let width = size as usize;
        let tiles = width * width;
        let mut coastline = Self {
            distance: vec![0; tiles],
            max_water_level: vec![0; tiles],
            max_water_kind: vec![WaterKind::None; tiles],
        };
        let mut queue = VecDeque::new();

        // 1. Знаковое расстояние до границы суша/вода.
        for y in 0..size {
            if y % 64 == 0 && cancelled() {
                return None;
            }
            for x in 0..size {
                let index = y as usize * width + x as usize;
                if water[index].is_water() {
                    if has_land_neighbor(water, size, x, y) {
                        coastline.distance[index] = -1;
                        queue.push_back((x, y, true, 1_i16));
                    }
                } else if has_water_neighbor(water, size, x, y) {
                    coastline.distance[index] = 1;
                    queue.push_back((x, y, false, 1_i16));
                }
            }
        }

        let mut steps = 0_u32;
        while let Some((x, y, on_water, distance)) = queue.pop_front() {
            steps += 1;
            if steps.is_multiple_of(65_536) && cancelled() {
                return None;
            }
            if distance >= MAX_DISTANCE {
                continue;
            }
            let next = distance + 1;
            for (dx, dy) in NEIGHBORS_4 {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= size || ny >= size {
                    continue;
                }
                let ni = ny as usize * width + nx as usize;
                if coastline.distance[ni] != 0 {
                    continue;
                }
                if on_water {
                    if !water[ni].is_water() {
                        continue;
                    }
                    coastline.distance[ni] = -(next.min(MAX_DISTANCE) as i8);
                    queue.push_back((nx, ny, true, next));
                } else {
                    if water[ni].is_water() {
                        continue;
                    }
                    coastline.distance[ni] = next.min(MAX_DISTANCE) as i8;
                    queue.push_back((nx, ny, false, next));
                }
            }
        }

        // 2. Максимальный уровень воды в радиусе пандуса. Для воды — свой
        // уровень/вид.
        let mut queue: VecDeque<(i32, i32, u16, WaterKind, u8)> = VecDeque::new();
        for y in 0..size {
            if y % 64 == 0 && cancelled() {
                return None;
            }
            for x in 0..size {
                let index = y as usize * width + x as usize;
                if water[index].is_water() {
                    coastline.max_water_level[index] = field.data()[index];
                    coastline.max_water_kind[index] = water[index];
                    continue;
                }
                let mut best_level = 0_u16;
                let mut best_kind = WaterKind::None;
                for (dx, dy) in NEIGHBORS_8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= size || ny >= size {
                        continue;
                    }
                    let ni = ny as usize * width + nx as usize;
                    if !water[ni].is_water() {
                        continue;
                    }
                    let level = field.data()[ni];
                    if level > best_level
                        || (level == best_level && water[ni] == WaterKind::River)
                    {
                        best_level = level;
                        best_kind = water[ni];
                    }
                }
                if best_kind != WaterKind::None {
                    coastline.max_water_level[index] = best_level;
                    coastline.max_water_kind[index] = best_kind;
                    queue.push_back((x, y, best_level, best_kind, 1));
                }
            }
        }
        while let Some((x, y, level, kind, distance)) = queue.pop_front() {
            if distance >= SHORE_RAMP_TILES {
                continue;
            }
            for (dx, dy) in NEIGHBORS_8 {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= size || ny >= size {
                    continue;
                }
                let ni = ny as usize * width + nx as usize;
                if water[ni].is_water() {
                    continue;
                }
                let better = level > coastline.max_water_level[ni]
                    || (level == coastline.max_water_level[ni]
                        && kind == WaterKind::River
                        && coastline.max_water_kind[ni] != WaterKind::River);
                if better {
                    coastline.max_water_level[ni] = level;
                    coastline.max_water_kind[ni] = kind;
                    queue.push_back((nx, ny, level, kind, distance + 1));
                }
            }
        }

        Some(coastline)
    }

    /// Расстояние суши до воды в тайлах (0 — вода или далеко от воды).
    #[inline]
    pub(crate) fn land_distance(&self, index: usize) -> u8 {
        self.distance[index].max(0) as u8
    }

    /// Глубина воды в тайлах (0 — суша или нет границы).
    #[inline]
    pub(crate) fn water_depth(&self, index: usize) -> u8 {
        (-self.distance[index]).max(0) as u8
    }

    /// Самый высокий водоём в радиусе пандуса.
    #[inline]
    pub(crate) fn max_water_level(&self, index: usize) -> u16 {
        self.max_water_level[index]
    }

    /// Вид самого высокого водоёма в радиусе пандуса.
    #[inline]
    pub(crate) fn max_water_kind(&self, index: usize) -> WaterKind {
        self.max_water_kind[index]
    }
}

#[inline]
fn has_land_neighbor(water: &[WaterKind], size: i32, x: i32, y: i32) -> bool {
    has_neighbor(water, size, x, y, false)
}

#[inline]
fn has_water_neighbor(water: &[WaterKind], size: i32, x: i32, y: i32) -> bool {
    has_neighbor(water, size, x, y, true)
}

#[inline]
fn has_neighbor(water: &[WaterKind], size: i32, x: i32, y: i32, want_water: bool) -> bool {
    for (dx, dy) in NEIGHBORS_4 {
        let nx = x + dx;
        let ny = y + dy;
        if nx < 0 || ny < 0 || nx >= size || ny >= size {
            continue;
        }
        let ni = ny as usize * size as usize + nx as usize;
        if water[ni].is_water() == want_water {
            return true;
        }
    }
    false
}

/// Назначает глубину воды по расстоянию до суши: море и внутренние моря
/// используют одну и ту же шкалу, поэтому на их стыке нет пояса мелководья.
pub(crate) fn apply_water_depth(water: &mut [WaterKind], coast: &CoastField) {
    for (index, kind) in water.iter_mut().enumerate() {
        if kind.is_sea() {
            let depth = coast.water_depth(index).max(1);
            *kind = if depth >= 8 {
                WaterKind::SeaOcean
            } else if depth >= 4 {
                WaterKind::SeaDeep
            } else {
                WaterKind::SeaShallow
            };
        } else if kind.is_lake() {
            let depth = coast.water_depth(index).max(1);
            *kind = if depth >= 3 {
                WaterKind::LakeDeep
            } else {
                WaterKind::Lake
            };
        }
    }
}

/// Глобальный береговой пандус: почва плавно опускается к уровню ближайшей
/// воды. В отличие от локального `cap`, это функция расстояния с нулевой
/// производной на внешней границе, поэтому не создаёт обрывов и «квадратов».
/// Камень и горы не трогаем — у них обрывы разрешены.
pub(crate) fn apply_shore_ramp(
    field: &mut HeightField,
    coast: &CoastField,
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    for y in 0..size {
        if y % 64 == 0 && cancelled() {
            return false;
        }
        for x in 0..size {
            let index = y as usize * width + x as usize;
            let distance = coast.land_distance(index);
            if distance == 0 || distance > SHORE_RAMP_TILES {
                continue;
            }
            let height = field.data()[index];
            if height >= STONE_HEIGHT_SUB {
                continue;
            }
            let level = coast.max_water_level(index);
            if height <= level {
                // Суша не должна оказаться ниже воды: поднимаем до одного
                // уровня над поверхностью.
                field.set_index(index, level.saturating_add(HEIGHT_SUB));
                continue;
            }
            let factor = smoothstep(distance as f32 / SHORE_RAMP_TILES as f32);
            let lowered = level as f32 + (height - level) as f32 * factor;
            let value = (lowered.round() as u16)
                .max(level.saturating_add(HEIGHT_SUB))
                .min(height);
            field.set_index(index, value);
        }
    }
    true
}

/// Ограничивает уклон почвы: соседние тайлы ниже камня не отличаются больше
/// чем на один уровень. Камень/горы не трогаем — обрывы остаются только там.
/// Проходы глобальные, без локальных границ.
pub(crate) fn limit_soil_slope(
    field: &mut HeightField,
    water: &[WaterKind],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    const PASSES: u32 = 4;
    let width = size as usize;
    let mut next = vec![0_u16; width * width];
    for _ in 0..PASSES {
        next.copy_from_slice(field.data());
        for y in 0..size {
            if y % 64 == 0 && cancelled() {
                return false;
            }
            for x in 0..size {
                let index = y as usize * width + x as usize;
                if water[index].is_water() || field.data()[index] >= STONE_HEIGHT_SUB {
                    continue;
                }
                let mut lowest = None;
                for (dx, dy) in NEIGHBORS_8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= size || ny >= size {
                        continue;
                    }
                    let ni = ny as usize * width + nx as usize;
                    if water[ni].is_water() {
                        continue;
                    }
                    lowest = Some(lowest.map_or(field.data()[ni], |h: u16| h.min(field.data()[ni])));
                }
                if let Some(lowest) = lowest {
                    let cap = lowest.saturating_add(HEIGHT_SUB);
                    if field.data()[index] > cap {
                        next[index] = cap;
                    }
                }
            }
        }
        field.data_mut().copy_from_slice(&next);
    }
    true
}

/// Гарантирует, что суша не ниже соседней воды: иначе вода «висит» над
/// берегом. Поднимает только те тайлы, которые оказались ниже уровня
/// ближайшего водоёма (например, между морем и озером разного уровня).
pub(crate) fn raise_land_above_water(
    field: &mut HeightField,
    water: &[WaterKind],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    for y in 0..size {
        if y % 64 == 0 && cancelled() {
            return false;
        }
        for x in 0..size {
            let index = y as usize * width + x as usize;
            if water[index].is_water() {
                continue;
            }
            let mut highest_water = None;
            for dy in -1..=1 {
                let ny = y + dy;
                if ny < 0 || ny >= size {
                    continue;
                }
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x + dx;
                    if nx < 0 || nx >= size {
                        continue;
                    }
                    let ni = ny as usize * width + nx as usize;
                    if water[ni].is_water() {
                        highest_water = Some(
                            highest_water.map_or(field.data()[ni], |h: u16| h.max(field.data()[ni])),
                        );
                    }
                }
            }
            if let Some(level) = highest_water {
                let bank = level.saturating_add(HEIGHT_SUB);
                if field.data()[index] < bank {
                    field.set_index(index, bank);
                }
            }
        }
    }
    true
}

/// Уровень моря для внешних потребителей (чтобы не тянуть heightmap).
#[allow(dead_code)]
pub(crate) const COAST_SEA_LEVEL: u16 = SEA_LEVEL as u16;
