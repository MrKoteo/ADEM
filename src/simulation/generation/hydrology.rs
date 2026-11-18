//! Гидрология: море, озёра и реки на основе поля высот.
//!
//! Гибридная схема: на грубой сетке (шаг 4–8 тайлов) считается бессточное
//! заполнение депрессий, дерево стока и накопление водосбора. Затем реки,
//! моря и озёра доводятся на полной сетке.
//!
//! Ключевые инварианты:
//! - высота воды — это уровень **поверхности**, а не дна;
//! - море всегда на `SEA_LEVEL = 0`; внутренние моря, соединённые с
//!   краевым океаном, сливаются с ним (иначе на стыке остаётся ступенька);
//! - поверхность реки берётся из depression-filled высоты `filled`: она
//!   монотонно не растёт вниз по течению и не выше берегов — поэтому нет
//!   ям, подъёмов вверх и «висящей» воды;
//! - путь реки сглаживается (Chaikin), чтобы не было «гармошки» по сетке.

use std::collections::VecDeque;

use super::height::{HeightField, HEIGHT_MAX_SUB, HEIGHT_SUB};
use crate::simulation::heightmap::SEA_LEVEL;

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
    #[inline]
    pub(crate) fn is_sea(self) -> bool {
        matches!(self, Self::SeaShallow | Self::SeaDeep | Self::SeaOcean)
    }
    #[inline]
    pub(crate) fn is_lake(self) -> bool {
        matches!(self, Self::Lake | Self::LakeDeep)
    }
}

/// Минимальное накопление водосбора для истока реки, в грубых клетках.
const RIVER_MIN_ACCUM: u32 = 64;
/// Минимальная глубина депрессии, чтобы считать её озером, подуровней.
const LAKE_MIN_DEPTH: u16 = HEIGHT_SUB * 2;
/// Минимальная площадь озера в грубых клетках.
const LAKE_MIN_COARSE: usize = 4;
/// Минимальный водоём внутри суши: более мелкие «лужи» на нулевой высоте
/// поднимаются до суши, чтобы не оставлять однотайловые дыры.
const MIN_INLAND_WATER_TILES: usize = 14;
/// Верхний предел площади bbox для точного flood озера. Если депрессия
/// гигантская, точный flood в bbox занял бы слишком много памяти; тогда
/// используется ограниченная грубая область.
const MAX_LAKE_BBOX_TILES: usize = 4_000_000;

const NEIGHBORS_4: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Связность водоёмов — восьмисоседняя: диагональный контакт воды визуально
/// читается как один водоём.
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

/// Очередь по высоте 0..HEIGHT_MAX_SUB: priority-flood без `BinaryHeap`.
struct HeightQueue {
    buckets: Vec<Vec<u32>>,
    level: usize,
}

impl HeightQueue {
    fn new() -> Self {
        Self {
            buckets: (0..=HEIGHT_MAX_SUB as usize).map(|_| Vec::new()).collect(),
            level: 0,
        }
    }

    #[inline]
    fn push(&mut self, height: u16, index: u32) {
        self.buckets[height.min(HEIGHT_MAX_SUB) as usize].push(index);
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

    fn clear(&mut self) {
        for bucket in &mut self.buckets {
            bucket.clear();
        }
        self.level = 0;
    }
}

/// Строит воду на карте. Возвращает карту видов воды (1 Б/тайл).
/// `sea_depth` — глубина ниже уровня моря по исходной высоте (0 — суша).
pub(crate) fn build_hydrology(
    field: &mut HeightField,
    sea_depth: &[u8],
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
            let value = field.data()[row + x];
            sums[ci] += u32::from(value);
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
    // дерево стока. `filled` — монотонная поверхность воды-заполнения,
    // именно она используется как уровень рек.
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

    // 4. Озёра: связные группы заполненных депрессий.
    let mut is_lake = vec![false; cw * cw];
    for index in 0..cw * cw {
        is_lake[index] = filled[index] > coarse[index].saturating_add(LAKE_MIN_DEPTH);
    }
    let mut lake_region = vec![0_u32; cw * cw];
    let mut lake_seen = BitGrid::new(cw);
    let mut lake_seeds = Vec::new();
    let mut next_region = 1_u32;
    for start in 0..cw * cw {
        if !is_lake[start] || lake_seen.get(start) {
            continue;
        }
        lake_seen.set(start);
        let mut region = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start);
        let mut min_cx = cw;
        let mut min_cy = cw;
        let mut max_cx = 0_usize;
        let mut max_cy = 0_usize;
        while let Some(cell) = queue.pop_front() {
            region.push(cell);
            let cx = cell % cw;
            let cy = cell / cw;
            min_cx = min_cx.min(cx);
            min_cy = min_cy.min(cy);
            max_cx = max_cx.max(cx);
            max_cy = max_cy.max(cy);
            let cx = cx as i32;
            let cy = cy as i32;
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
        let level = region
            .iter()
            .map(|&cell| filled[cell])
            .max()
            .unwrap_or(SEA_LEVEL as u16);
        let region_id = next_region;
        next_region += 1;
        let mut lowest = region[0];
        for &cell in &region {
            lake_region[cell] = region_id;
            if coarse[cell] < coarse[lowest] {
                lowest = cell;
            }
        }
        let cx = (lowest % cw) as i32;
        let cy = (lowest / cw) as i32;
        // Bounding box региона в тайлах полной сетки (без отступа).
        let x0 = (min_cx * step as usize) as i32;
        let y0 = (min_cy * step as usize) as i32;
        let x1 = (((max_cx + 1) * step as usize) as i32).min(size);
        let y1 = (((max_cy + 1) * step as usize) as i32).min(size);
        let max_area = (region.len() * (step as usize) * (step as usize) * 3).max(256);
        lake_seeds.push((
            cx * step + step / 2,
            cy * step + step / 2,
            region_id,
            level,
            max_area,
            x0,
            y0,
            x1,
            y1,
        ));
    }

    // 5. Море: вода на уровне моря, достижимая от края. Высоты уже плоские
    // (генератор), BFS только классифицирует.
    flood_sea(sea_depth, &mut water, size, cancelled)?;

    // 6. Озёра на полной сетке: заливаем связные низины до уровня перелива.
    // Без произвольного `cap`: иначе край озера обрезается прямоугольником
    // по сетке, и получаются «квадратные» водоёмы.
    for (sx, sy, region_id, coarse_level, max_area, x0, y0, x1, y1) in lake_seeds {
        if !fill_lake_region(
            field,
            &mut water,
            &mut queue,
            sx,
            sy,
            region_id,
            coarse_level,
            max_area,
            &lake_region,
            cw,
            step,
            x0,
            y0,
            x1,
            y1,
            size,
            cancelled,
        ) {
            return None;
        }
    }

    // 7. Согласование стоячей воды: внутренние моря, соединённые с краевым
    // океаном, сливаются с ним; мелкие лужи на нулевой высоте поднимаются
    // до суши. Заодно поверхность каждого озера выравнивается и проверяется
    // по берегам.
    if !finalize_water_bodies(field, &mut water, size, cancelled) {
        return None;
    }

    // 8. Реки: поверхность = `filled` (монотонно вниз по течению), путь
    // сглаживается Chaikin. Русло не углубляем локально — иначе появляются
    // ямы и подъёмы вверх по склону.
    if !build_rivers(
        field,
        &mut water,
        &filled,
        &parent,
        &accumulation,
        &lake_region,
        cw,
        step,
        size,
        cancelled,
    ) {
        return None;
    }

    Some(water)
}
/// Размечает море: вода на уровне моря, достижимая от края. **Высоты не
/// пишет** — генератор уже сделал море плоским.
fn flood_sea(
    sea_depth: &[u8],
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
            if seen.get(index) || sea_depth[index] == 0 {
                continue;
            }
            seen.set(index);
            water[index] = WaterKind::SeaShallow;
            queue.push_back((x, y));
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
            if seen.get(index) || sea_depth[index] == 0 {
                continue;
            }
            seen.set(index);
            water[index] = WaterKind::SeaShallow;
            queue.push_back((nx, ny));
        }
    }
    Some(())
}

/// Заливает озеро на полной сетке до уровня перелива. Без `cap`: вода
/// останавливается только на уровне перелива, а не на границе произвольной
/// площади. `level` — поверхность озера в подуровнях.
/// 
/// Заливает озеро точным priority-flood на полной сетке в его bounding box.
/// Грубая область используется только как подсказка, где искать озеро:
/// граница воды — настоящий контур `height < уровень перелива`, без сетки
/// 4×4. Если вода дошла до края bbox, bbox расширяется и flood повторяется.
fn fill_lake_region(
    field: &mut HeightField,
    water: &mut [WaterKind],
    queue: &mut HeightQueue,
    seed_x: i32,
    seed_y: i32,
    region_id: u32,
    coarse_level: u16,
    max_area: usize,
    lake_region: &[u32],
    cw: usize,
    step: i32,
    mut x0: i32,
    mut y0: i32,
    mut x1: i32,
    mut y1: i32,
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    if seed_x < 0 || seed_y < 0 || seed_x >= size || seed_y >= size {
        return true;
    }
    for attempt in 0..4 {
        x0 = x0.clamp(0, size - 1);
        y0 = y0.clamp(0, size - 1);
        x1 = x1.clamp(x0 + 1, size);
        y1 = y1.clamp(y0 + 1, size);
        let w = (x1 - x0) as usize;
        let h = (y1 - y0) as usize;
        if w < 2 || h < 2 {
            return true;
        }
        if w * h > MAX_LAKE_BBOX_TILES {
            return fill_lake_bounded(
                field,
                water,
                size,
                seed_x,
                seed_y,
                coarse_level,
                region_id,
                lake_region,
                cw,
                step,
                cancelled,
            );
        }
        // Отступ от грубой области: bbox должен захватывать берега, а не
        // обрезать озеро. На повторной попытке отступ растёт.
        let margin = (w.max(h) / 2).clamp(8, 64) as i32;
        x0 = (x0 - margin).max(0);
        y0 = (y0 - margin).max(0);
        x1 = (x1 + margin).min(size);
        y1 = (y1 + margin).min(size);
        let w = (x1 - x0) as usize;
        let h = (y1 - y0) as usize;

        let Some(filled) = priority_flood_bbox(field, queue, x0, y0, x1, y1, cancelled) else {
            return false;
        };
        if seed_x < x0 || seed_y < y0 || seed_x >= x1 || seed_y >= y1 {
            return true;
        }
        let local_seed = (seed_y - y0) as usize * w + (seed_x - x0) as usize;
        let level = filled[local_seed].min(coarse_level);
        if level <= SEA_LEVEL as u16 {
            return true;
        }

        // Компонента воды от самого низкого тайла до контура `height < level`.
        let mut mask = vec![false; w * h];
        let mut queue_water = VecDeque::new();
        let seed_index = seed_y as usize * size as usize + seed_x as usize;
        if field.data()[seed_index] >= level || water[seed_index].is_water() {
            return true;
        }
        mask[local_seed] = true;
        queue_water.push_back((seed_x, seed_y));
        let mut touches_border = false;
        let mut component_tiles = 1_usize;
        let mut too_big = false;
        let mut steps = 0_u32;
        'water: while let Some((x, y)) = queue_water.pop_front() {
            steps += 1;
            if steps.is_multiple_of(65_536) && cancelled() {
                return false;
            }
            if x == x0 || y == y0 || x + 1 == x1 || y + 1 == y1 {
                touches_border = true;
            }
            for (dx, dy) in NEIGHBORS_8 {
                let nx = x + dx;
                let ny = y + dy;
                if nx < x0 || ny < y0 || nx >= x1 || ny >= y1 {
                    continue;
                }
                let local = (ny - y0) as usize * w + (nx - x0) as usize;
                if mask[local] {
                    continue;
                }
                let ni = ny as usize * size as usize + nx as usize;
                if water[ni].is_water() || field.data()[ni] >= level {
                    continue;
                }
                mask[local] = true;
                queue_water.push_back((nx, ny));
                component_tiles += 1;
                if component_tiles > max_area {
                    too_big = true;
                    break 'water;
                }
            }
        }

        // Точный бассейн оказался неправдоподобно большим (например, уровень
        // перелива заливает целую низменность). Для такого редкого случая
        // откатываемся к ограниченной грубой областью — иначе одна ошибка
        // съедает полкарты.
        if too_big || component_tiles > max_area {
            return fill_lake_bounded(
                field,
                water,
                size,
                seed_x,
                seed_y,
                level.min(coarse_level),
                region_id,
                lake_region,
                cw,
                step,
                cancelled,
            );
        }

        // Вода упёрлась в край bbox — значит, берег могли обрезать.
        // Расширяем область и повторяем точный flood.
        if touches_border && attempt < 3 {
            let grow = (w.max(h) / 2).clamp(16, 128) as i32;
            x0 = (x0 - grow).max(0);
            y0 = (y0 - grow).max(0);
            x1 = (x1 + grow).min(size);
            y1 = (y1 + grow).min(size);
            continue;
        }

        for local in 0..w * h {
            if !mask[local] {
                continue;
            }
            let x = x0 + (local % w) as i32;
            let y = y0 + (local / w) as i32;
            let index = y as usize * size as usize + x as usize;
            if water[index].is_water() {
                continue;
            }
            water[index] = WaterKind::Lake;
            field.set_index(index, level);
        }
        return true;
    }
    true
}

/// Priority-flood на полной сетке внутри bbox: возвращает поверхность
/// заполнения депрессий (`filled`) для каждой клетки bbox.
fn priority_flood_bbox(
    field: &HeightField,
    queue: &mut HeightQueue,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<u16>> {
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    let tiles = w * h;
    let mut filled = vec![0_u16; tiles];
    let mut visited = vec![false; tiles];
    queue.clear();
    for x in x0..x1 {
        push_bbox_seed(queue, field, x, y0, x0, y0, w, &mut filled, &mut visited);
        push_bbox_seed(
            queue,
            field,
            x,
            y1 - 1,
            x0,
            y0,
            w,
            &mut filled,
            &mut visited,
        );
    }
    for y in y0..y1 {
        push_bbox_seed(queue, field, x0, y, x0, y0, w, &mut filled, &mut visited);
        push_bbox_seed(
            queue,
            field,
            x1 - 1,
            y,
            x0,
            y0,
            w,
            &mut filled,
            &mut visited,
        );
    }
    let mut steps = 0_u32;
    while let Some(local) = queue.pop() {
        steps += 1;
        if steps.is_multiple_of(65_536) && cancelled() {
            return None;
        }
        let local = local as usize;
        let lx = (local % w) as i32 + x0;
        let ly = (local / w) as i32 + y0;
        let level = filled[local];
        for (dx, dy) in NEIGHBORS_4 {
            let nx = lx + dx;
            let ny = ly + dy;
            if nx < x0 || ny < y0 || nx >= x1 || ny >= y1 {
                continue;
            }
            let ni = (ny - y0) as usize * w + (nx - x0) as usize;
            if visited[ni] {
                continue;
            }
            visited[ni] = true;
            let height = field.get(nx, ny);
            let level = height.max(level);
            filled[ni] = level;
            queue.push(level, ni as u32);
        }
    }
    Some(filled)
}

fn push_bbox_seed(
    queue: &mut HeightQueue,
    field: &HeightField,
    x: i32,
    y: i32,
    x0: i32,
    y0: i32,
    w: usize,
    filled: &mut [u16],
    visited: &mut [bool],
) {
    let local = (y - y0) as usize * w + (x - x0) as usize;
    if visited[local] {
        return;
    }
    visited[local] = true;
    let height = field.get(x, y);
    filled[local] = height;
    queue.push(height, local as u32);
}

/// Ограниченная заливка озера по грубой области (+1 клетка буфера). Нужна
/// только как аварийный откат, если точный бассейн вышел за `max_area`.
fn fill_lake_bounded(
    field: &mut HeightField,
    water: &mut [WaterKind],
    size: i32,
    sx: i32,
    sy: i32,
    level: u16,
    region_id: u32,
    lake_region: &[u32],
    cw: usize,
    step: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    if sx < 0 || sy < 0 || sx >= size || sy >= size || level <= SEA_LEVEL as u16 {
        return true;
    }
    let width = size as usize;
    let start = sy as usize * width + sx as usize;
    if water[start].is_water() || field.data()[start] >= level {
        return true;
    }
    let in_region = |nx: i32, ny: i32| {
        let cx = (nx / step) as i32;
        let cy = (ny / step) as i32;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let mx = cx + dx;
                let my = cy + dy;
                if mx < 0 || my < 0 || mx >= cw as i32 || my >= cw as i32 {
                    continue;
                }
                if lake_region[my as usize * cw + mx as usize] == region_id {
                    return true;
                }
            }
        }
        false
    };
    let mut queue = VecDeque::new();
    queue.push_back((sx, sy));
    water[start] = WaterKind::Lake;
    field.set_index(start, level);
    let mut filled = 1_usize;
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in NEIGHBORS_8 {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= size || ny >= size || !in_region(nx, ny) {
                continue;
            }
            let index = ny as usize * width + nx as usize;
            if water[index].is_water() || field.data()[index] >= level {
                continue;
            }
            water[index] = WaterKind::Lake;
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

/// Согласует стоячую воду после морей и озёр:
/// - все `height == 0`, ещё не размеченные, считаются водой;
/// - компонента, соединённая с морем или краем карты, становится морем на 0;
/// - остальные — озёра, поверхность выравнивается по самому низкому берегу;
/// - совсем маленькие лужи на нулевой высоте поднимаются до суши.
fn finalize_water_bodies(
    field: &mut HeightField,
    water: &mut [WaterKind],
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
            if field.data()[index] == SEA_LEVEL as u16 && !water[index].is_water() {
                water[index] = WaterKind::Lake;
            }
        }
    }

    let mut visited = BitGrid::new(width);
    let mut queue = VecDeque::new();
    let mut component = Vec::new();
    let mut steps = 0_u32;
    for start_y in 0..size {
        if start_y % 64 == 0 && cancelled() {
            return false;
        }
        for start_x in 0..size {
            let start = start_y as usize * width + start_x as usize;
            if visited.get(start) || !water[start].is_water() || water[start] == WaterKind::River {
                continue;
            }
            visited.set(start);
            component.clear();
            queue.clear();
            queue.push_back((start_x, start_y));
            let mut is_sea = false;
            let mut touches_border = false;
            let mut level = 0_u16;
            let mut min_bank = u16::MAX;
            while let Some((x, y)) = queue.pop_front() {
                steps += 1;
                if steps.is_multiple_of(65_536) && cancelled() {
                    return false;
                }
                let index = y as usize * width + x as usize;
                component.push(index as u32);
                if water[index].is_sea() {
                    is_sea = true;
                }
                if x == 0 || y == 0 || x + 1 == size || y + 1 == size {
                    touches_border = true;
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
                    } else if nk == WaterKind::River {
                        // Река — граница, но не часть стоячего водоёма.
                        min_bank = min_bank.min(field.data()[ni]);
                    } else {
                        min_bank = min_bank.min(field.data()[ni]);
                    }
                }
            }

            if is_sea || touches_border {
                for &index in &component {
                    let index = index as usize;
                    field.set_index(index, SEA_LEVEL as u16);
                    water[index] = WaterKind::SeaShallow;
                }
            } else if level == SEA_LEVEL as u16 && component.len() < MIN_INLAND_WATER_TILES {
                for &index in &component {
                    let index = index as usize;
                    field.set_index(index, HEIGHT_SUB);
                    water[index] = WaterKind::None;
                }
            } else {
                let target = level
                    .min(if min_bank == u16::MAX { level } else { min_bank })
                    .max(SEA_LEVEL as u16);
                for &index in &component {
                    let index = index as usize;
                    field.set_index(index, target);
                    water[index] = WaterKind::Lake;
                }
            }
        }
    }
    true
}

/// Точка сглаженной линии реки: координаты в тайлах, уровень поверхности
/// и ширина русла.
#[derive(Clone, Copy)]
struct RiverPoint {
    x: f32,
    y: f32,
    level: f32,
    radius: f32,
}

/// Строит реки: источники по накоплению и реальному уклону, непрерывное
/// продолжение вниз по дереву стока, сглаживание пути и запись поверхности.
fn build_rivers(
    field: &mut HeightField,
    water: &mut [WaterKind],
    filled: &[u16],
    parent: &[u32],
    accumulation: &[u32],
    lake_region: &[u32],
    cw: usize,
    step: i32,
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let tiles = (size as usize) * (size as usize);
    // Источники: клетка с реальным уклоном и достаточным водосбором.
    let mut is_river = vec![false; cw * cw];
    for index in 0..cw * cw {
        let downstream = parent[index];
        if downstream == u32::MAX
            || lake_region[index] != 0
            || accumulation[index] < RIVER_MIN_ACCUM
        {
            continue;
        }
        if filled[index] > filled[downstream as usize] {
            is_river[index] = true;
        }
    }
    // Непрерывно продолжаем от истоков вниз по дереву.
    // `discovery` не передан: идём по parent от каждой клетки вниз, пока
    // не встретим уже помеченную реку. Детерминированный порядок.
    for index in 0..cw * cw {
        if !is_river[index] {
            continue;
        }
        let mut cell = index;
        while parent[cell] != u32::MAX {
            let downstream = parent[cell] as usize;
            // Вода (море/озеро) — устье, а не продолжение русла.
            if water[downstream].is_water() || is_river[downstream] {
                break;
            }
            is_river[downstream] = true;
            cell = downstream;
        }
    }

    // Число входящих речных детей: старт цепочки — клетка без них.
    let mut upstream_count = vec![0_u16; cw * cw];
    for index in 0..cw * cw {
        if is_river[index] {
            let downstream = parent[index];
            if downstream != u32::MAX {
                upstream_count[downstream as usize] =
                    upstream_count[downstream as usize].saturating_add(1);
            }
        }
    }

    let mut surface = vec![u16::MAX; tiles];
    for start in 0..cw * cw {
        if !is_river[start] || upstream_count[start] != 0 {
            continue;
        }
        let mut points = Vec::new();
        let mut cell = start;
        let mut guard = 0_usize;
        loop {
            guard += 1;
            if guard > cw * cw {
                break;
            }
            let cx = (cell % cw) as i32;
            let cy = (cell / cw) as i32;
            let radius = 1.0 + (accumulation[cell] / RIVER_MIN_ACCUM).ilog2().min(2) as f32;
            points.push(RiverPoint {
                x: (cx * step + step / 2) as f32,
                y: (cy * step + step / 2) as f32,
                level: filled[cell] as f32,
                radius: radius.clamp(1.0, 2.0),
            });
            let downstream = parent[cell];
            if downstream == u32::MAX {
                break;
            }
            let next = downstream as usize;
            if water[next].is_water() || !is_river[next] {
                // Устье: последний речной тайл подгонится к воде отдельным
                // проходом ниже. Точку воды в линию не добавляем, иначе
                // интерполяция к уровню моря рисует «пику» через сушу.
                break;
            }
            cell = next;
        }
        // Сглаживаем путь до Chaikin: убирает «гармошку» из шагов по сетке.
        smooth_path(&mut points, 2);
        // Монотонность: каждый речной участок не выше следующего вниз по
        // течению.
        for i in (0..points.len().saturating_sub(1)).rev() {
            points[i].level = points[i].level.min(points[i + 1].level);
        }
        if points.len() >= 2 {
            let smooth = chaikin(&points, 2);
            rasterize_river(&mut surface, &smooth, size);
        }
        if start % 256 == 0 && cancelled() {
            return false;
        }
    }

    for index in 0..tiles {
        if surface[index] == u16::MAX || water[index].is_water() {
            continue;
        }
        let level = surface[index].max(SEA_LEVEL as u16);
        field.set_index(index, level);
        water[index] = WaterKind::River;
    }
    // Устья: река, касающаяся моря/озера, должна стоять ровно на его
    // уровне, иначе на стыке остаётся ступенька.
    for y in 0..size {
        if y % 64 == 0 && cancelled() {
            return false;
        }
        for x in 0..size {
            let index = y as usize * size as usize + x as usize;
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
                let ni = ny as usize * size as usize + nx as usize;
                if water[ni].is_water() && water[ni] != WaterKind::River {
                    mouth = Some(mouth.map_or(field.data()[ni], |m: u16| m.min(field.data()[ni])));
                }
            }
            if let Some(level) = mouth {
                field.set_index(index, level);
            }
        }
    }
    // Ямы: речной тайл ниже всех соседей — артефакт грубой сетки. Поднимаем
    // до самого низкого соседа; несколько проходов, пока есть изменения.
    for _ in 0..4 {
        let mut changed = false;
        for y in 0..size {
            for x in 0..size {
                let index = y as usize * size as usize + x as usize;
                if water[index] != WaterKind::River {
                    continue;
                }
                let mut lowest = u16::MAX;
                for (dx, dy) in NEIGHBORS_8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= size || ny >= size {
                        continue;
                    }
                    let ni = ny as usize * size as usize + nx as usize;
                    lowest = lowest.min(field.data()[ni]);
                }
                if lowest != u16::MAX && field.data()[index] < lowest {
                    field.set_index(index, lowest);
                    changed = true;
                }
            }
        }
        if !changed || cancelled() {
            break;
        }
    }
    true
}

/// Сглаживание полилинии 1-2-1 (концы не двигаются). Убирает мелкую
/// «гармошку» до угловой срезки Чайкина.
fn smooth_path(points: &mut [RiverPoint], passes: u32) {
    if points.len() < 3 {
        return;
    }
    let mut tmp = points.to_vec();
    for _ in 0..passes {
        for i in 1..points.len() - 1 {
            let p0 = points[i - 1];
            let p1 = points[i];
            let p2 = points[i + 1];
            tmp[i] = RiverPoint {
                x: (p0.x + 2.0 * p1.x + p2.x) * 0.25,
                y: (p0.y + 2.0 * p1.y + p2.y) * 0.25,
                level: (p0.level + 2.0 * p1.level + p2.level) * 0.25,
                radius: (p0.radius + 2.0 * p1.radius + p2.radius) * 0.25,
            };
        }
        points.copy_from_slice(&tmp);
    }
}

/// Угловая срезка Чайкина: сглаживает полилинию, не сдвигая концы.
fn chaikin(points: &[RiverPoint], iterations: u32) -> Vec<RiverPoint> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut current = points.to_vec();
    for _ in 0..iterations {
        let mut next = Vec::with_capacity(current.len() * 2);
        next.push(current[0]);
        for pair in current.windows(2) {
            let p0 = pair[0];
            let p1 = pair[1];
            next.push(lerp_point(p0, p1, 0.25));
            next.push(lerp_point(p0, p1, 0.75));
        }
        next.push(*current.last().expect("non-empty"));
        current = next;
    }
    current
}

#[inline]
fn lerp_point(a: RiverPoint, b: RiverPoint, t: f32) -> RiverPoint {
    RiverPoint {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
        level: a.level + (b.level - a.level) * t,
        radius: a.radius + (b.radius - a.radius) * t,
    }
}

/// Рисует сглаженную линию реки дисками; поверхность — минимум из
/// перекрывающихся участков (ниже по течению вода не должна «висеть»).
fn rasterize_river(surface: &mut [u16], points: &[RiverPoint], size: i32) {
    for pair in points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let steps = ((b.x - a.x).abs().max((b.y - a.y).abs()).ceil() as i32).max(1);
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let p = lerp_point(a, b, t);
            paint_disc(
                surface,
                size,
                p.x.round() as i32,
                p.y.round() as i32,
                p.radius.round().clamp(1.0, 2.0) as i32,
                p.level.round().clamp(0.0, HEIGHT_MAX_SUB as f32) as u16,
            );
        }
    }
}

fn paint_disc(surface: &mut [u16], size: i32, x: i32, y: i32, radius: i32, level: u16) {
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
            if level < surface[index] {
                surface[index] = level;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_with<F: FnMut(i32, i32) -> u16>(side: i32, mut height: F) -> (HeightField, Vec<u8>) {
        let mut field = HeightField::new(side as usize);
        let mut sea_depth = vec![0_u8; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                let index = y as usize * side as usize + x as usize;
                let h = height(x, y);
                field.set_index(index, h);
                if h == SEA_LEVEL as u16 {
                    sea_depth[index] = 1;
                }
            }
        }
        (field, sea_depth)
    }

    #[test]
    fn lake_is_flat_at_spill_level() {
        // Чаша: дно 20 уровней, борта 200 уровней.
        let side = 64;
        let (mut field, sea_depth) = field_with(side, |x, y| {
            let dx = (x - 32).abs();
            let dy = (y - 32).abs();
            if dx < 12 && dy < 12 {
                20 * HEIGHT_SUB
            } else {
                200 * HEIGHT_SUB
            }
        });
        let water = build_hydrology(&mut field, &sea_depth, side, &|| false).expect("hydrology");
        let lake_count = water
            .iter()
            .filter(|w| w.is_lake())
            .count();
        assert!(lake_count > 0, "озеро не сформировалось");
        let level = field.get(32, 32);
        assert!(level > 20 * HEIGHT_SUB, "уровень озера должен быть выше дна");
        for y in 0..side {
            for x in 0..side {
                let index = y as usize * side as usize + x as usize;
                if water[index].is_lake() {
                    assert_eq!(field.get(x, y), level, "озеро должно быть плоским");
                }
            }
        }
    }

    #[test]
    fn river_surface_is_monotonic_and_below_banks() {
        // Наклонная плоскость с ложбиной по центру.
        let side = 256;
        let (mut field, sea_depth) = field_with(side, |x, y| {
            ((200 - y / 2 - (x - 128).abs() / 2).max(2) as u16) * HEIGHT_SUB
        });
        let water = build_hydrology(&mut field, &sea_depth, side, &|| false).expect("hydrology");
        let mut river_tiles = 0;
        for y in 0..side {
            for x in 0..side {
                let index = y as usize * side as usize + x as usize;
                if water[index] != WaterKind::River {
                    continue;
                }
                river_tiles += 1;
                if x < 8 || y < 8 || x + 8 >= side || y + 8 >= side {
                    continue;
                }
                // Вода не выше ни одного соседнего берега (8-соседняя проверка).
                for (dx, dy) in NEIGHBORS_8 {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= side || ny >= side {
                        continue;
                    }
                    let ni = ny as usize * side as usize + nx as usize;
                    if !water[ni].is_water() {
                        assert!(
                            field.data()[index] <= field.data()[ni],
                            "река висит над берегом в ({x}, {y})"
                        );
                    }
                }
            }
        }
        assert!(river_tiles > 0, "реки не сформировались");
    }
}
