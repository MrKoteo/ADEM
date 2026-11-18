//! Рельеф: базовое поле высот, кривая высот, сглаживание и термальная эрозия.
//!
//! Внутри генерации высоты хранятся в `u16` с шагом `HEIGHT_SUB` подуровней
//! на один уровень финального слоя `u8`. Это позволяет сглаживать и
//! эродировать рельеф **до** квантования: в чанки попадают уже плавные
//! высоты, без локальных «заплаток» и обрывов на границе обработки.
//!
//! `HeightField` — временный буфер (2 Б/тайл). Он живёт только в фоновом
//! потоке и освобождается до установки мира.
//!
//! Океан и море — всегда `SEA_LEVEL = 0`: всё, что ниже `ELEV_SEA`,
//! генератор сразу кладёт на 0. Отдельная карта `sea_depth` нужна только
//! чтобы отличить подводные впадины от суши.

use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};
use crate::simulation::noise::{fbm, hash64, ridge_noise, smoothstep, value_noise};

/// Подуровней на один уровень финального `u8`-слоя.
pub(crate) const HEIGHT_SUB: u16 = 16;
/// Максимум поля высот генерации (255 * 16).
pub(crate) const HEIGHT_MAX_SUB: u16 = HEIGHT_MAX as u16 * HEIGHT_SUB;
/// Подуровней на один уровень — удобный множитель для констант.
const fn sub(levels: u8) -> u16 {
    levels as u16 * HEIGHT_SUB
}

/// Уровень моря: ниже — мелководье, выше — суша.
pub(crate) const ELEV_SEA: f32 = 0.42;
/// Опорная точка камня.
pub(crate) const ELEV_STONE: f32 = 0.68;
/// Опорная точка гор.
pub(crate) const ELEV_MOUNTAIN: f32 = 0.72;
/// Опорная точка высокогорья.
pub(crate) const ELEV_HIGH: f32 = 0.86;

/// Абсолютные пороги высоты (фиксированные для всех карт), уровней слоя.
/// Визуально (при `HEIGHT_WORLD_UNIT = 1`): камень — 24 юнита (1.5 тайла),
/// горы — 65 (≈4 тайла), снеговая линия — 142 (≈8.9 тайла), максимум — 255
/// (≈16 тайлов).
pub(crate) const STONE_HEIGHT: u8 = 24;
pub(crate) const MOUNTAIN_HEIGHT: u8 = 65;
#[allow(dead_code)] // используется тестами кривой высот и комментарием шейдера
pub(crate) const SNOW_HEIGHT: u8 = 142;
/// Верхняя граница леса, уровней.
pub(crate) const TREE_LINE: u8 = 97;
/// Ширина пляжной полосы над локальным уровнем воды, уровней.
pub(crate) const BEACH_BAND: u8 = 2;

/// Те же пороги в подуровнях генерации.
pub(crate) const STONE_HEIGHT_SUB: u16 = sub(STONE_HEIGHT);
pub(crate) const TREE_LINE_SUB: u16 = sub(TREE_LINE);
pub(crate) const BEACH_BAND_SUB: u16 = sub(BEACH_BAND);

/// Опорные точки перевода шумовой высоты `[0, 1]` в подуровни высот.
///
/// Ниже `ELEV_SEA` — 0 (океан). Сразу над уровнем моря добавлен пологий
/// «шельф»: первые проценты высоты дают 1–2 уровня, поэтому песчаные и
/// земляные берега получаются плавными, а не обрывом на всю высоту.
const HEIGHT_STOPS: [(f32, f32); 8] = [
    (0.0, 0.0),
    (ELEV_SEA, 0.0),
    (ELEV_SEA + 0.03, 2.0 * HEIGHT_SUB as f32),
    (ELEV_SEA + 0.08, 6.0 * HEIGHT_SUB as f32),
    (ELEV_STONE, 24.0 * HEIGHT_SUB as f32),
    (ELEV_MOUNTAIN, 65.0 * HEIGHT_SUB as f32),
    (ELEV_HIGH, 142.0 * HEIGHT_SUB as f32),
    (1.0, HEIGHT_MAX_SUB as f32),
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

/// Временное поле высот генерации, row-major, в подуровнях.
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
        self.data[index] = height.min(HEIGHT_MAX_SUB);
    }

    pub(crate) fn data(&self) -> &[u16] {
        &self.data
    }

    pub(crate) fn data_mut(&mut self) -> &mut [u16] {
        &mut self.data
    }
}

/// Переводит шумовую высоту тайла в подуровни слоя высот (0..4080).
/// Непрерывна, поэтому на границах полос нет скачков.
pub(crate) fn height_for_elevation_sub(elevation: f32) -> u16 {
    let elevation = elevation.clamp(0.0, 1.0);
    for pair in HEIGHT_STOPS.windows(2) {
        let (e0, h0) = pair[0];
        let (e1, h1) = pair[1];
        if elevation <= e1 {
            let t = if e1 > e0 {
                (elevation - e0) / (e1 - e0)
            } else {
                0.0
            };
            let height = h0 + (h1 - h0) * t;
            return height.round().clamp(0.0, HEIGHT_MAX_SUB as f32) as u16;
        }
    }
    HEIGHT_MAX_SUB
}

/// Публичная кривая в уровнях финального слоя (для тестов и диагностики).
#[allow(dead_code)]
pub(crate) fn height_for_elevation(elevation: f32) -> u8 {
    let sub = height_for_elevation_sub(elevation);
    ((sub + HEIGHT_SUB / 2) / HEIGHT_SUB).min(HEIGHT_MAX as u16) as u8
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
            // Оставляем орогенез заметным: иначе на части seed'ов материк
            // получается совершенно плоским (ни камня, ни гор).
            uplift *= 0.50 + 0.30 * mask;
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

/// Заполняет поле базовыми высотами. Всё ниже уровня моря сразу получает
/// `SEA_LEVEL` (0), а `sea_depth` запоминает глубину по исходной высоте.
/// Проверка отмены — раз в строку. `progress` получает долю пройденных
/// строк (самый долгий проход генерации — планка не должна стоять на нуле).
pub(crate) fn fill_base_heights(
    field: &mut HeightField,
    sea_depth: &mut [u8],
    size: i32,
    seed: u64,
    archetype: MapArchetype,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> bool {
    let width = size as usize;
    for y in 0..size {
        if cancelled() {
            return false;
        }
        if y % 16 == 0 {
            progress(y as f32 / size as f32);
        }
        let row = y as usize * width;
        for x in 0..size {
            let index = row + x as usize;
            let elevation = elevation_at_for(x, y, size, seed, archetype);
            if elevation <= ELEV_SEA {
                // Океан/море плоские по построению: никакого BFS-выравнивания.
                sea_depth[index] = (((ELEV_SEA - elevation) / ELEV_SEA) * HEIGHT_MAX as f32)
                    .round()
                    .clamp(1.0, HEIGHT_MAX as f32) as u8;
                field.data[index] = SEA_LEVEL as u16;
            } else {
                sea_depth[index] = 0;
                // Суша не бывает на высоте моря: 0 зарезервирован под воду.
                field.data[index] = height_for_elevation_sub(elevation).max(HEIGHT_SUB);
            }
        }
    }
    progress(1.0);
    true
}

/// Сколько проходов сглаживания делать до квантования.
const SMOOTH_PASSES: u32 = 3;

/// Глобальное сглаживание поля высот: сепарабельный биномиальный фильтр
/// (1-2-1) по горизонтали и вертикали. Вода считается нулём, поэтому
/// прибрежная суша плавно опускается к уровню воды. Операция глобальная —
/// у неё нет границы, на которой мог бы появиться обрыв.
pub(crate) fn smooth_heights(
    field: &mut HeightField,
    sea_depth: &[u8],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
    let mut tmp = vec![0_u16; width * width];
    for _ in 0..SMOOTH_PASSES {
        // Горизонтальный проход.
        for y in 0..size {
            if cancelled() {
                return false;
            }
            let row = y as usize * width;
            for x in 0..size {
                let index = row + x as usize;
                if sea_depth[index] != 0 {
                    tmp[index] = 0;
                    continue;
                }
                let left = if x > 0 { field.data[index - 1] } else { field.data[index] };
                let right = if x + 1 < size {
                    field.data[index + 1]
                } else {
                    field.data[index]
                };
                tmp[index] = ((left + 2 * field.data[index] + right + 2) / 4).max(HEIGHT_SUB);
            }
        }
        // Вертикальный проход (из tmp обратно в field).
        for y in 0..size {
            if cancelled() {
                return false;
            }
            let row = y as usize * width;
            for x in 0..size {
                let index = row + x as usize;
                if sea_depth[index] != 0 {
                    field.data[index] = 0;
                    continue;
                }
                let top = if y > 0 { tmp[index - width] } else { tmp[index] };
                let bottom = if y + 1 < size {
                    tmp[index + width]
                } else {
                    tmp[index]
                };
                field.data[index] =
                    ((top + 2 * tmp[index] + bottom + 2) / 4).max(HEIGHT_SUB);
            }
        }
    }
    true
}

/// Максимальный перепад между соседями для термальной эрозии, подуровней.
/// 1 уровень финального слоя (16 подуровней) — склоны мягче одного уровня
/// не трогаем, более крутые осыпаются.
const THERMAL_TALUS_SUB: i32 = HEIGHT_SUB as i32;
/// Число проходов термальной эрозии.
const THERMAL_PASSES: u32 = 3;

/// Термальная эрозия: осыпи на слишком крутых склонах. Детерминирована:
/// пары соседей обрабатываются один раз (вправо/вниз), изменения копятся в
/// scratch и применяются после прохода. Вода (`sea_depth > 0`) не участвует
/// и всегда остаётся на `SEA_LEVEL`.
pub(crate) fn erode_thermal(
    field: &mut HeightField,
    sea_depth: &[u8],
    size: i32,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let width = size as usize;
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
                if sea_depth[index] != 0 {
                    continue;
                }
                let height = field.data[index] as i32;
                // Правый сосед.
                if x + 1 < size {
                    let neighbor = index + 1;
                    if sea_depth[neighbor] == 0 {
                        let diff = height - field.data[neighbor] as i32;
                        if diff > THERMAL_TALUS_SUB {
                            let shift = ((diff - THERMAL_TALUS_SUB) / 2) as i16;
                            scratch[index] -= shift;
                            scratch[neighbor] += shift;
                        }
                    }
                }
                // Нижний сосед.
                if y + 1 < size {
                    let neighbor = index + width;
                    if sea_depth[neighbor] == 0 {
                        let diff = height - field.data[neighbor] as i32;
                        if diff > THERMAL_TALUS_SUB {
                            let shift = ((diff - THERMAL_TALUS_SUB) / 2) as i16;
                            scratch[index] -= shift;
                            scratch[neighbor] += shift;
                        }
                    }
                }
            }
        }
        for (index, &delta) in scratch.iter().enumerate() {
            if sea_depth[index] != 0 {
                field.data[index] = SEA_LEVEL as u16;
                continue;
            }
            let value = field.data[index] as i32 + delta as i32;
            field.data[index] = value.clamp(HEIGHT_SUB as i32, HEIGHT_MAX_SUB as i32) as u16;
        }
    }
    true
}
