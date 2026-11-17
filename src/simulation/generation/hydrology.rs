//! Гидрология: море, озёра и реки на основе поля высот.
//!
//! Гибридная схема: на грубой сетке (шаг 4–8 тайлов) считается бессточное
//! заполнение депрессий, дерево стока и накопление водосбора. Затем реки и
//! озёра доводятся на полной сетке. Это даёт реалистичные водосборы без
//! полноразмерных `u32`-сеток на больших картах.

use std::collections::VecDeque;

use super::height::HeightField;
use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};

/// Вид воды на тайле. `None` — суша.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WaterKind {
    None,
    SeaShallow,
    SeaDeep,
    SeaOcean,
    Lake,
    LakeDeep,
    River,
}

impl WaterKind {
    #[inline]
    pub(crate) fn is_water(self) -> bool {
        self != Self::None
    }
}

/// Минимальное накопление водосбора для реки, в грубых клетках.
const RIVER_MIN_ACCUM: u32 = 32;
/// Минимальная глубина депрессии, чтобы считать её озером, уровней.
const LAKE_MIN_DEPTH: u16 = 4;
/// Минимальная площадь озера в грубых клетках.
const LAKE_MIN_COARSE: usize = 4;
/// Глубина русла: от 8 до 32 уровней (4–16 юнитов, 0.25–1 тайл).
const RIVER_DEPTH_MIN: u16 = 8;
const RIVER_DEPTH_STEP: u16 = 8;

const NEIGHBORS_4: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Связность водоёмов — восьмисоседняя: диагональный контакт воды визуально
/// читается как один водоём, а не как «внутреннее море» выше океана.
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

/// Шаг грубой сетки: на больших картах крупнее, чтобы временные сетки
/// оставались около 1 млн клеток.
fn coarse_step(size: i32) -> i32 {
    if size <= 4096 { 4 } else { 8 }
}

struct BitGrid {
    words: Vec<u64>,
}

impl BitGrid {
    fn new(side: usize) -> Self {
        Self {
            words: vec![0; side * side / 64 + 1],
        }
    }

    #[inline]
    fn get(&self, index: usize) -> bool {
        self.words[index / 64] & (1 << (index % 64)) != 0
    }

    #[inline]
    fn set(&mut self, index: usize) {
        self.words[index / 64] |= 1 << (index % 64);
    }
}

/// Очередь по высоте 0..4095: priority-flood без `BinaryHeap`.
struct HeightQueue {
    buckets: Vec<Vec<u32>>,
    level: usize,
}

impl HeightQueue {
    fn new() -> Self {
        Self {
            buckets: (0..=HEIGHT_MAX as usize).map(|_| Vec::new()).collect(),
            level: 0,
        }
    }

    #[inline]
    fn push(&mut self, height: u16, index: u32) {
        self.buckets[height.min(HEIGHT_MAX) as usize].push(index);
    }

    fn pop(&mut self) -> Option<u32> {
        while self.level < self.buckets.len() {
            if let Some(value) = self.buckets[self.level].pop() {
                return Some(value);
            }
            self.level += 1;
        }
        None
    }
}

/// Строит воду на карте. Возвращает карту видов воды (1 Б/тайл).
pub(crate) fn build_hydrology(
    field: &mut HeightField,
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<WaterKind>> {
    let width = size as usize;
    let tiles = width * width;
    let mut water = vec![WaterKind::None; tiles];

    let step = coarse_step(size);
    let cw = width.div_ceil(step as usize);

    // 1. Грубые высоты: среднее по клетке.
    let mut coarse = vec![0_u16; cw * cw];
    let mut sums = vec![0_u32; cw * cw];
    let mut counts = vec![0_u32; cw * cw];
    for y in 0..width {
        if y % 64 == 0 && cancelled() {
            return None;
        }
        let cy = y / step as usize;
        let row = y * width;
        for x in 0..width {
            let cx = x / step as usize;
            let ci = cy * cw + cx;
            sums[ci] += field.data()[row + x] as u32;
            counts[ci] += 1;
        }
    }
    for (index, slot) in coarse.iter_mut().enumerate() {
        let count = counts[index].max(1);
        *slot = (sums[index] / count) as u16;
    }
    drop(sums);
    drop(counts);

    // 2. Priority-flood на грубой сетке: заполняем депрессии и строим
    // дерево стока. Родитель — клетка, из которой впервые пришли.
    let mut filled = coarse.clone();
    let mut parent = vec![u32::MAX; cw * cw];
    let mut visited = BitGrid::new(cw);
    let mut queue = HeightQueue::new();
    let mut discovery = Vec::with_capacity(cw * cw);
    let mut seeds = Vec::new();
    for cy in 0..cw {
        for cx in 0..cw {
            if cx == 0 || cy == 0 || cx + 1 == cw || cy + 1 == cw {
                let index = cy * cw + cx;
                if !visited.get(index) {
                    visited.set(index);
                    seeds.push(index as u32);
                }
            }
        }
    }
    for &seed in &seeds {
        let seed = seed as usize;
        queue.push(filled[seed], seed as u32);
        discovery.push(seed as u32);
    }
    let mut steps = 0_u32;
    while let Some(cell) = queue.pop() {
        steps += 1;
        if steps.is_multiple_of(65_536) && cancelled() {
            return None;
        }
        let cell = cell as usize;
        let cx = (cell % cw) as i32;
        let cy = (cell / cw) as i32;
        let cell_filled = filled[cell];
        for (dx, dy) in NEIGHBORS_4 {
            let nx = cx + dx;
            let ny = cy + dy;
            if nx < 0 || ny < 0 || nx >= cw as i32 || ny >= cw as i32 {
                continue;
            }
            let neighbor = ny as usize * cw + nx as usize;
            if visited.get(neighbor) {
                continue;
            }
            visited.set(neighbor);
            let level = filled[neighbor].max(cell_filled);
            filled[neighbor] = level;
            parent[neighbor] = cell as u32;
            discovery.push(neighbor as u32);
            queue.push(level, neighbor as u32);
        }
    }

    // 3. Накопление водосбора: reverse discovery order гарантирует, что
    // ребёнок обработан раньше родителя (дерево строилось от устья вверх).
    let mut accumulation = vec![1_u32; cw * cw];
    for &cell in discovery.iter().rev() {
        let cell = cell as usize;
        let downstream = parent[cell];
        if downstream != u32::MAX {
            accumulation[downstream as usize] =
                accumulation[downstream as usize].saturating_add(accumulation[cell]);
        }
    }

    // 4. Озёра: связные группы заполненных депрессий. Мелкие лужи не
    // превращаем в воду — оставляем сухими.
    let mut is_lake = vec![false; cw * cw];
    for index in 0..cw * cw {
        // Бессточные впадины ниже уровня моря тоже вода: иначе внутри суши
        // оставались бы «дыры» ниже моря с травой на дне. Море уже занято
        // отдельным BFS и в озеро не попадёт.
        is_lake[index] = filled[index] > coarse[index].saturating_add(LAKE_MIN_DEPTH);
    }
    let mut lake_level = vec![0_u16; cw * cw];
    let mut lake_seen = BitGrid::new(cw);
    let mut lake_seeds = Vec::new();
    for start in 0..cw * cw {
        if !is_lake[start] || lake_seen.get(start) {
            continue;
        }
        lake_seen.set(start);
        let mut region = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        while let Some(cell) = queue.pop_front() {
            region.push(cell);
            let cx = (cell % cw) as i32;
            let cy = (cell / cw) as i32;
            for (dx, dy) in NEIGHBORS_8 {
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= cw as i32 || ny >= cw as i32 {
                    continue;
                }
                let neighbor = ny as usize * cw + nx as usize;
                if is_lake[neighbor] && !lake_seen.get(neighbor) {
                    lake_seen.set(neighbor);
                    queue.push_back(neighbor);
                }
            }
        }
        if region.len() < LAKE_MIN_COARSE {
            continue;
        }
        let level = region.iter().map(|&cell| filled[cell]).max().unwrap_or(SEA_LEVEL);
        let mut lowest = region[0];
        for &cell in &region {
            lake_level[cell] = level;
            if coarse[cell] < coarse[lowest] {
                lowest = cell;
            }
        }
        let cx = (lowest % cw) as i32;
        let cy = (lowest / cw) as i32;
        lake_seeds.push((
            cx * step + step / 2,
            cy * step + step / 2,
            level,
            // Запас на уточнение границы на полной сетке; иначе BFS
            // останавливается внутри чаши и оставляет «остров»/яму.
            region.len() * step as usize * step as usize * 4,
        ));
    }

    // 5. Море: вода, достижимая от края и лежащая ниже уровня моря.
    // Глубину определяем до выравнивания поверхности.
    flood_sea(field, &mut water, size, cancelled)?;

    // 6. Озёра на полной сетке: заливаем связные низины до уровня перелива.
    for (sx, sy, level, cap) in lake_seeds {
        if !flood_lake(field, &mut water, size, sx, sy, level, cap, cancelled) {
            return None;
        }
    }

    // 7. Реки: русло углубляем относительно местного рельефа. Ширина и
    // глубина растут с накоплением водосбора. Море и озёра не трогаем.
    let mut river_depth = vec![0_u8; tiles];
    for cy in 0..cw {
        if cy % 64 == 0 && cancelled() {
            return None;
        }
        for cx in 0..cw {
            let index = cy * cw + cx;
            if lake_level[index] > 0
                || filled[index] <= SEA_LEVEL
                || accumulation[index] < RIVER_MIN_ACCUM
                || parent[index] == u32::MAX
            {
                continue;
            }
            let downstream = parent[index] as usize;
            let width_cells =
                1 + (accumulation[index] / RIVER_MIN_ACCUM).ilog2().min(2) as i32;
            let depth = river_depth_for(accumulation[index]);
            let x0 = cx as i32 * step + step / 2;
            let y0 = cy as i32 * step + step / 2;
            let x1 = (downstream % cw) as i32 * step + step / 2;
            let y1 = (downstream / cw) as i32 * step + step / 2;
            // Ширина русла: 3 или 5 тайлов. Радиус 0 оставлял короткие
            // обрывки русел, не доходящие до водоёма; диск радиуса 3 — уже
            // семь тайлов, это слишком широко для карты 256.
            let radius = width_cells.clamp(1, 2);
            paint_river_segment(&mut river_depth, size, x0, y0, x1, y1, radius, depth);
        }
    }
    for y in 0..size {
        if cancelled() {
            return None;
        }
        let row = y as usize * width;
        for x in 0..size {
            let index = row + x as usize;
            let depth = river_depth[index];
            if depth == 0 || water[index].is_water() {
                continue;
            }
            // Русло не должно уходить ниже моря: устье сливается с морем.
            let carved = field.data()[index].saturating_sub(depth as u16).max(SEA_LEVEL);
            field.set_index(index, carved);
            water[index] = WaterKind::River;
        }
    }

    if !flatten_water_bodies(field, &water, size, cancelled) {
        return None;
    }
    Some(water)
}

/// Выравнивает поверхность каждого водоёма: море — `SEA_LEVEL`, озеро — его
/// уровень перелива. Реки исключены: у них наклонная поверхность русла.
/// Восьмисоседняя связность убирает «внутренние моря» выше океана, если
/// водоём касается моря по диагонали.
fn flatten_water_bodies(
    field: &mut HeightField,
    water: &[WaterKind],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    let mut visited = BitGrid::new(width);
    let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
    let mut component: Vec<u32> = Vec::new();
    let mut steps = 0_u32;

    for start_y in 0..size {
        if start_y % 64 == 0 && cancelled() {
            return false;
        }
        for start_x in 0..size {
            let start = start_y as usize * width + start_x as usize;
            if visited.get(start) {
                continue;
            }
            let kind = water[start];
            if !kind.is_water() || kind == WaterKind::River {
                continue;
            }
            visited.set(start);
            component.clear();
            queue.clear();
            queue.push_back((start_x, start_y));
            let mut is_sea = false;
            let mut level = 0_u16;
            let mut boundary: Option<u16> = None;
            while let Some((x, y)) = queue.pop_front() {
                steps += 1;
                if steps.is_multiple_of(65_536) && cancelled() {
                    return false;
                }
                let index = y as usize * width + x as usize;
                component.push(index as u32);
                if matches!(
                    water[index],
                    WaterKind::SeaShallow | WaterKind::SeaDeep | WaterKind::SeaOcean
                ) {
                    is_sea = true;
                }
                level = level.max(field.data()[index]);
                for (dx, dy) in NEIGHBORS_8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= size || ny >= size {
                        continue;
                    }
                    let ni = ny as usize * width + nx as usize;
                    let nk = water[ni];
                    if nk.is_water() && nk != WaterKind::River {
                        if !visited.get(ni) {
                            visited.set(ni);
                            queue.push_back((nx, ny));
                        }
                    } else {
                        // Берег или река: уровень озера не должен быть выше
                        // самой низкой границы, иначе вода «висит» над сушей.
                        let h = field.data()[ni];
                        boundary = Some(boundary.map_or(h, |b| b.min(h)));
                    }
                }
            }
            let target = if is_sea {
                SEA_LEVEL
            } else {
                level.min(boundary.unwrap_or(level))
            };
            for &index in &component {
                field.set_index(index as usize, target);
            }
        }
    }

    // Устье реки должно совпадать с поверхностью водоёма, иначе на стыке
    // реки и озера/моря остаётся ступенька.
    for y in 0..size {
        if y % 64 == 0 && cancelled() {
            return false;
        }
        for x in 0..size {
            let index = y as usize * width + x as usize;
            if water[index] != WaterKind::River {
                continue;
            }
            let mut mouth = None;
            for (dx, dy) in NEIGHBORS_8 {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= size || ny >= size {
                    continue;
                }
                let ni = ny as usize * width + nx as usize;
                if water[ni].is_water() && water[ni] != WaterKind::River {
                    let h = field.data()[ni];
                    mouth = Some(mouth.map_or(h, |m: u16| m.min(h)));
                }
            }
            if let Some(mouth) = mouth {
                field.set_index(index, mouth);
            }
        }
    }
    true
}

/// Глубина русла по накоплению водосбора.
fn river_depth_for(accumulation: u32) -> u8 {
    let ratio = accumulation / RIVER_MIN_ACCUM;
    let steps = ratio.ilog2().min(3) as u16;
    (RIVER_DEPTH_MIN + steps * RIVER_DEPTH_STEP).min(32) as u8
}

/// Рисует отрезок русла на полной сетке с диском радиуса `radius`.
fn paint_river_segment(
    river_depth: &mut [u8],
    size: i32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    radius: i32,
    depth: u8,
) {
    let steps = ((x1 - x0).abs().max((y1 - y0).abs())).max(1);
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let x = (x0 as f32 + (x1 - x0) as f32 * t).round() as i32;
        let y = (y0 as f32 + (y1 - y0) as f32 * t).round() as i32;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy > radius * radius {
                    continue;
                }
                let px = x + dx;
                let py = y + dy;
                if px < 0 || py < 0 || px >= size || py >= size {
                    continue;
                }
                let index = py as usize * size as usize + px as usize;
                if river_depth[index] < depth {
                    river_depth[index] = depth;
                }
            }
        }
    }
}

/// Размечает море: вода, достижимая от края и лежащая ниже `SEA_LEVEL`.
/// Поверхность моря плоская; глубина — по высоте дна до выравнивания.
fn flood_sea(
    field: &mut HeightField,
    water: &mut [WaterKind],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> Option<()> {
    let width = size as usize;
    let mut queue = VecDeque::new();
    let mut seen = BitGrid::new(width);
    for i in 0..size {
        for &(x, y) in &[(i, 0), (i, size - 1), (0, i), (size - 1, i)] {
            let index = y as usize * width + x as usize;
            if seen.get(index) {
                continue;
            }
            if field.data()[index] <= SEA_LEVEL {
                seen.set(index);
                let d = SEA_LEVEL - field.data()[index];
                water[index] = sea_kind(d);
                queue.push_back((x, y));
            }
        }
    }
    let mut steps = 0_u32;
    while let Some((x, y)) = queue.pop_front() {
        steps += 1;
        if steps.is_multiple_of(65_536) && cancelled() {
            return None;
        }
        for (dx, dy) in NEIGHBORS_8 {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= size || ny >= size {
                continue;
            }
            let index = ny as usize * width + nx as usize;
            if seen.get(index) || field.data()[index] > SEA_LEVEL {
                continue;
            }
            seen.set(index);
            let d = SEA_LEVEL - field.data()[index];
            water[index] = sea_kind(d);
            queue.push_back((nx, ny));
        }
    }
    for y in 0..size {
        if y % 64 == 0 && cancelled() {
            return None;
        }
        for x in 0..size {
            let index = y as usize * width + x as usize;
            if water[index].is_water() {
                field.set_index(index, SEA_LEVEL);
            }
        }
    }
    Some(())
}

#[inline]
fn sea_kind(depth: u16) -> WaterKind {
    if depth >= 48 {
        WaterKind::SeaOcean
    } else if depth >= 24 {
        WaterKind::SeaDeep
    } else {
        WaterKind::SeaShallow
    }
}

/// Заливает озеро на полной сетке до уровня перелива. Ограничение `cap`
/// защищает от «улёта» воды по случайному низкому проходу.
fn flood_lake(
    field: &mut HeightField,
    water: &mut [WaterKind],
    size: i32,
    sx: i32,
    sy: i32,
    level: u16,
    cap: usize,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    if sx < 0 || sy < 0 || sx >= size || sy >= size {
        return true;
    }
    let width = size as usize;
    let start = sy as usize * width + sx as usize;
    if water[start].is_water() || field.data()[start] > level {
        return true;
    }
    let max_area = cap.max(64).min(width * width);
    let mut queue = VecDeque::new();
    queue.push_back((sx, sy));
    let deep = level.saturating_sub(field.data()[start]) >= 24;
    water[start] = if deep { WaterKind::LakeDeep } else { WaterKind::Lake };
    field.set_index(start, level);
    let mut filled = 1_usize;
    while let Some((x, y)) = queue.pop_front() {
        if filled >= max_area {
            break;
        }
        for (dx, dy) in NEIGHBORS_8 {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= size || ny >= size {
                continue;
            }
            let index = ny as usize * width + nx as usize;
            if water[index].is_water() || field.data()[index] > level {
                continue;
            }
            let deep = level.saturating_sub(field.data()[index]) >= 24;
            water[index] = if deep { WaterKind::LakeDeep } else { WaterKind::Lake };
            field.set_index(index, level);
            queue.push_back((nx, ny));
            filled += 1;
            if filled.is_multiple_of(4096) && cancelled() {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_with<F: FnMut(i32, i32) -> u16>(side: i32, mut height: F) -> HeightField {
        let mut field = HeightField::new(side as usize);
        for y in 0..side {
            for x in 0..side {
                field.set_index(y as usize * side as usize + x as usize, height(x, y));
            }
        }
        field
    }

    #[test]
    fn lake_is_flat_at_spill_level() {
        // Чаша: дно 900, борта 2000. Вода должна встать на уровень перелива,
        // а не на дно, и быть плоской.
        let side = 64;
        let mut field = field_with(side, |x, y| {
            let dx = (x - 32).abs();
            let dy = (y - 32).abs();
            if dx < 12 && dy < 12 { 900 } else { 2000 }
        });
        let water = build_hydrology(&mut field, side, &|| false).expect("hydrology");
        let lake_count = water
            .iter()
            .filter(|w| matches!(w, WaterKind::Lake | WaterKind::LakeDeep))
            .count();
        assert!(lake_count > 0, "озеро не сформировалось");
        let level = field.get(32, 32);
        assert!(level > 900, "уровень озера должен быть выше дна");
        for y in 0..side {
            for x in 0..side {
                let index = y as usize * side as usize + x as usize;
                if matches!(water[index], WaterKind::Lake | WaterKind::LakeDeep) {
                    assert_eq!(field.get(x, y), level, "озеро должно быть плоским");
                }
            }
        }
    }

    #[test]
    fn river_surface_is_below_its_banks() {
        // Наклонная плоскость: сток идёт к нижнему краю, накопление
        // превращается в реки. Русло должно быть ниже соседней суши.
        let side = 256;
        let mut field = field_with(side, |_x, y| (3000 - y * 8).max(200) as u16);
        let original = field.data().to_vec();
        let water = build_hydrology(&mut field, side, &|| false).expect("hydrology");
        let mut river_tiles = 0;
        for y in 0..side {
            for x in 0..side {
                let index = y as usize * side as usize + x as usize;
                if water[index] != WaterKind::River {
                    continue;
                }
                river_tiles += 1;
                // У самого устья берег может быть только ниже по течению —
                // проверяем внутренние участки русла.
                if x < 8 || y < 8 || x + 8 >= side || y + 8 >= side {
                    continue;
                }
                assert!(
                    field.get(x, y) < original[index],
                    "русло не углублено в ({x}, {y})"
                );
                let mut bank = false;
                let mut land_neighbors = 0;
                for (dx, dy) in NEIGHBORS_4 {
                    let nx = x + dx;
                    let ny = y + dy;
                    let ni = ny as usize * side as usize + nx as usize;
                    if !water[ni].is_water() {
                        land_neighbors += 1;
                        if field.get(nx, ny) > field.get(x, y) {
                            bank = true;
                        }
                    }
                }
                // Внутри широкой реки соседей-суши может не быть — это не
                // ошибка, берег проверяем только на кромке русла.
                if land_neighbors == 0 {
                    continue;
                }
                assert!(bank, "у реки нет берега выше воды в ({x}, {y})");
            }
        }
        assert!(river_tiles > 0, "реки не сформировались");
    }
}
