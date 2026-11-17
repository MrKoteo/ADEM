//! Рельеф: базовое поле высот, кривая высот и термальная эрозия.
//!
//! `HeightField` — временный рабочий буфер генерации (2 Б/тайл). Он живёт
//! только в фоновом потоке и освобождается до установки мира, поэтому
//! постоянная память чанков не растёт.

use crate::simulation::noise::{fbm, hash64, ridge_noise, smoothstep, value_noise};
use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};

/// Уровень моря: ниже — мелководье, выше — суша.
pub(crate) const ELEV_SEA: f32 = 0.42;
/// Опорная точка камня. Выше старого порога: классификация теперь идёт по
/// итоговой высоте, поэтому камень/горы должны занимать разумную долю суши.
pub(crate) const ELEV_STONE: f32 = 0.68;
/// Опорная точка гор.
pub(crate) const ELEV_MOUNTAIN: f32 = 0.72;
/// Опорная точка высокогорья.
pub(crate) const ELEV_HIGH: f32 = 0.86;

/// Абсолютные пороги высоты (фиксированные для всех карт).
/// Море — 2 тайла, камень — 14 тайлов, горы — 34 тайла, снеговая линия —
/// 72 тайла при `HEIGHT_WORLD_UNIT = 0.5`.
pub(crate) const STONE_HEIGHT: u16 = 448;
pub(crate) const MOUNTAIN_HEIGHT: u16 = 1088;
pub(crate) const SNOW_HEIGHT: u16 = 2304;
/// Ширина пляжной полосы над локальным уровнем воды, уровней.
pub(crate) const BEACH_BAND: u16 = 8;
/// Верхняя граница леса, уровней.
pub(crate) const TREE_LINE: u16 = 1600;

/// Опорные точки перевода шумовой высоты `[0, 1]` в высоты слоя.
/// Море — 64 уровня (2 тайла), рельеф выше моря растянут примерно в 5 раз
/// относительно старой карты.
const HEIGHT_STOPS: [(f32, f32); 6] = [
    (0.0, 0.0),
    (ELEV_SEA, SEA_LEVEL as f32),
    (ELEV_STONE, STONE_HEIGHT as f32),
    (ELEV_MOUNTAIN, MOUNTAIN_HEIGHT as f32),
    (ELEV_HIGH, SNOW_HEIGHT as f32),
    (1.0, HEIGHT_MAX as f32),
];

/// Масштаб континентального шума, тайлов на клетку.
const CONTINENT_SCALE: f32 = 80.0;
/// Сила domain warp.
const WARP_STRENGTH: f32 = 0.9;
/// Доля стороны карты, на которой высота спадает к краю.
const EDGE_FALLOFF_FRACTION: f32 = 0.05;
const EDGE_FALLOFF_MAX: f32 = 320.0;
const EDGE_FALLOFF_FLOOR: f32 = 0.2;

/// Архетип карты, выбираемый из seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapArchetype {
    Continents,
    SingleContinent,
    Archipelago,
}

impl MapArchetype {
    /// Детерминированный выбор архетипа из seed.
    pub(crate) fn for_seed(seed: u64) -> Self {
        match hash64(seed) % 3 {
            0 => Self::Continents,
            1 => Self::SingleContinent,
            _ => Self::Archipelago,
        }
    }
}

/// Временное поле высот генерации, row-major.
pub(crate) struct HeightField {
    width: usize,
    data: Vec<u16>,
}

impl HeightField {
    pub(crate) fn new(width: usize) -> Self {
        Self {
            width,
            data: vec![0; width * width],
        }
    }

    #[inline]
    pub(crate) fn get(&self, x: i32, y: i32) -> u16 {
        self.data[y as usize * self.width + x as usize]
    }

    #[inline]
    pub(crate) fn set_index(&mut self, index: usize, height: u16) {
        self.data[index] = height.min(HEIGHT_MAX);
    }

    pub(crate) fn data(&self) -> &[u16] {
        &self.data
    }
}

/// Переводит шумовую высоту тайла в единицы слоя высот (0..4095).
/// Непрерывна, поэтому на границах полос нет скачков.
pub(crate) fn height_for_elevation(elevation: f32) -> u16 {
    let elevation = elevation.clamp(0.0, 1.0);
    for pair in HEIGHT_STOPS.windows(2) {
        let (e0, h0) = pair[0];
        let (e1, h1) = pair[1];
        if elevation <= e1 {
            let t = if e1 > e0 { (elevation - e0) / (e1 - e0) } else { 0.0 };
            let height = h0 + (h1 - h0) * t;
            return height.round().clamp(0.0, HEIGHT_MAX as f32) as u16;
        }
    }
    HEIGHT_MAX
}

/// Высота тайла в [0, 1]: континентальный шум, domain warp, архетип карты
/// и спад к краям. Горы — широкие области поднятия, а не узкий столб шума:
/// крупномасштабная зона орогенеза накапливает высоту на большом участке
/// суши, а гребневый шум добавляет только рельефную деталь.
pub(crate) fn elevation_at_for(
    x: i32,
    y: i32,
    size: i32,
    seed: u64,
    archetype: MapArchetype,
) -> f32 {
    let fx = x as f32 / CONTINENT_SCALE;
    let fy = y as f32 / CONTINENT_SCALE;
    // Domain warp одним октавом value-noise: форма берегов сохраняется,
    // а третий-четвёртый октавы на warp не влияют на вид.
    let wx = fx
        + (value_noise(fx * 0.5 + 37.2, fy * 0.5 + 11.7, seed ^ 0x5EED_0001) - 0.5)
            * WARP_STRENGTH;
    let wy = fy
        + (value_noise(fx * 0.5 + 91.3, fy * 0.5 + 53.9, seed ^ 0x5EED_0002) - 0.5)
            * WARP_STRENGTH;

    // Основа суши: крупные пологие области. Архипелаг — мельче и ниже.
    let base = match archetype {
        MapArchetype::Archipelago => {
            fbm(wx * 1.4 + 13.0, wy * 1.4 + 71.0, seed ^ 0xA5C1_0001, 4)
        }
        _ => fbm(wx * 0.8, wy * 0.8, seed, 4),
    };
    let detail = fbm(wx * 3.0, wy * 3.0, seed ^ 0x0D37_0A11, 2);

    // Широкие зоны орогенеза: крупный шум ~230 тайлов. Внутри них высота
    // накапливается на большой площади; ridge — только деталь поверхности.
    let large = fbm(
        wx * 0.35 + 43.0,
        wy * 0.35 + 19.0,
        seed ^ 0x0B16_0001,
        2,
    );
    let uplift_mask = smoothstep((large - 0.54) / 0.18);
    let ridge = ridge_noise(wx * 1.1, wy * 1.1, seed ^ 0x1D6E_5EED, 2);
    let mut uplift = uplift_mask * (0.20 + 0.20 * large + 0.08 * ridge);
    // Базовая суша держится ниже порога камня: горы/камень появляются
    // прежде всего там, где сработала крупномасштабная зона поднятия.
    let mut land = 0.16 + base * 0.54 + detail * 0.05;

    match archetype {
        MapArchetype::Continents => {}
        MapArchetype::SingleContinent => {
            let cx = size as f32 * 0.5;
            let dx = (x as f32 - cx) / (size as f32 * 0.5);
            let dy = (y as f32 - cx) / (size as f32 * 0.5);
            let r = (dx * dx + dy * dy).sqrt();
            let mask = 1.0 - smoothstep((r - 0.25) / 0.75);
            land = land * (0.97 + 0.05 * mask) + 0.05 * mask;
            // Один континент: приподнимаем мелководные низины до прибрежной
            // равнины, иначе вокруг материка получается слишком широкий шельф.
            land = land.max(0.435 + detail * 0.04);
            uplift *= 0.35 + 0.15 * mask;
        }
        MapArchetype::Archipelago => {
            land *= 1.08;
            uplift *= 0.75;
        }
    }
    land += uplift;

    // Спад к краям: у границы множитель EDGE_FALLOFF_FLOOR (край всегда
    // под водой), дальше `span` тайлов — 1.
    let span = (size as f32 * EDGE_FALLOFF_FRACTION).clamp(1.0, EDGE_FALLOFF_MAX);
    let dx = (x.min(size - 1 - x) as f32 / span).clamp(0.0, 1.0);
    let dy = (y.min(size - 1 - y) as f32 / span).clamp(0.0, 1.0);
    let edge = EDGE_FALLOFF_FLOOR + (1.0 - EDGE_FALLOFF_FLOOR) * smoothstep(dx) * smoothstep(dy);
    (land * edge).clamp(0.0, 1.0)
}

/// Публичная обёртка для тестов и диагностики: архетип выводится из seed.
#[allow(dead_code)]
pub(crate) fn elevation_at(x: i32, y: i32, size: i32, seed: u64) -> f32 {
    elevation_at_for(x, y, size, seed, MapArchetype::for_seed(seed))
}

/// Заполняет поле базовыми высотами. Проверка отмены — раз в строку.
pub(crate) fn fill_base_heights(
    field: &mut HeightField,
    size: i32,
    seed: u64,
    archetype: MapArchetype,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    for y in 0..size {
        if cancelled() {
            return false;
        }
        let row = y as usize * width;
        for x in 0..size {
            let elevation = elevation_at_for(x, y, size, seed, archetype);
            field.data[row + x as usize] = height_for_elevation(elevation);
        }
    }
    true
}

/// Максимальный перепад между соседями для термальной эрозии, уровней.
/// 16 уровней = 8 юнитов = 0.5 тайла по горизонтали: равнины сглаживаются,
/// а горы сохраняют склоны.
const THERMAL_TALUS: i32 = 16;
/// Число проходов термальной эрозии.
const THERMAL_PASSES: u32 = 4;

/// Термальная эрозия: осыпи на слишком крутых склонах. Детерминирована:
/// пары соседей обрабатываются один раз (вправо/вниз), изменения копятся в
/// scratch и применяются после прохода.
pub(crate) fn erode_thermal(
    field: &mut HeightField,
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    // Суммарный сдвиг за проход ограничен: максимум ~4·(4095−TALUS)/2,
    // поэтому i16 достаточно и вдвое дешевле i32 по памяти.
    let mut scratch = vec![0_i16; width * width];
    for _ in 0..THERMAL_PASSES {
        scratch.fill(0);
        for y in 0..size {
            if cancelled() {
                return false;
            }
            let row = y as usize * width;
            for x in 0..size {
                let index = row + x as usize;
                let height = field.data[index] as i32;
                // Правый сосед.
                if x + 1 < size {
                    let neighbor = index + 1;
                    let diff = height - field.data[neighbor] as i32;
                    if diff > THERMAL_TALUS {
                        let shift = ((diff - THERMAL_TALUS) / 2) as i16;
                        scratch[index] -= shift;
                        scratch[neighbor] += shift;
                    }
                }
                // Нижний сосед.
                if y + 1 < size {
                    let neighbor = index + width;
                    let diff = height - field.data[neighbor] as i32;
                    if diff > THERMAL_TALUS {
                        let shift = ((diff - THERMAL_TALUS) / 2) as i16;
                        scratch[index] -= shift;
                        scratch[neighbor] += shift;
                    }
                }
            }
        }
        for (index, &delta) in scratch.iter().enumerate() {
            let value = field.data[index] as i32 + delta as i32;
            field.data[index] = value.clamp(0, HEIGHT_MAX as i32) as u16;
        }
    }
    true
}
