//! Климат: глобальный сезонный ход температуры + постоянные карты
//! температуры и влажности по ячейке 2×2 тайла.
//!
//! `ambient_temperature_c` — среднемесячные температуры умеренного климата
//! с суточным ходом (синусоида с минимумом перед рассветом и пиком в
//! середине дня). Поверх — `ClimateMap` (широта, крупный шум, domain warp)
//! и `MoistureMap` (шум + прибрежная добавка); высота применяется при
//! чтении, поэтому правки рельефа не устаревают. Карты используют генерация
//! (биомы, пляжи) и будущая погода/потребности.

use super::noise::{fbm, value_noise};
use super::time::{GAME_SECS_PER_TICK, SECS_PER_DAY, SECS_PER_MONTH};

/// Сторона ячейки климатических карт, тайлов. 2×2 — компромисс между
/// плавностью биомов и памятью (0.25 Б/тайл для `i8`/`u8`).
pub const CLIMATE_CELL_TILES: i32 = 2;

/// Амплитуда domain warp климатических карт, тайлов: искривляет широтные
/// полосы и границы биомов, чтобы они не выглядели прямыми линиями.
const CLIMATE_WARP_AMPLITUDE: f32 = 9.0;
/// Масштаб шума domain warp, тайлов на клетку.
const CLIMATE_WARP_SCALE: f32 = 96.0;

/// Смещение координаты для климатической карты: крупный шум, одинаковый
/// для температуры и влажности в пределах карты. Считается по ячейкам,
/// поэтому не добавляет шумовых вызовов на каждый тайл.
fn climate_warp(x: f32, y: f32, seed: u64, salt: u64) -> (f32, f32) {
    let s = CLIMATE_WARP_SCALE;
    let wx = (value_noise(x / s + 7.0, y / s + 29.0, seed ^ salt) - 0.5)
        * 2.0
        * CLIMATE_WARP_AMPLITUDE;
    let wy = (value_noise(x / s + 53.0, y / s + 11.0, seed ^ (salt ^ 0x51ED_0001)) - 0.5)
        * 2.0
        * CLIMATE_WARP_AMPLITUDE;
    (wx, wy)
}

/// Среднегодовая температура глобального умеренного климата, ℃. Базовая
/// карта хранит отклонение от неё, а не абсолютную температуру.
pub const GLOBAL_ANNUAL_MEAN_C: f32 = 9.0;

/// Падение температуры с высотой, ℃ на уровень высоты. Уровень — единица
/// слоя высот (визуально 1/16 тайла); при полной амплитуде 255 уровней
/// температура падает примерно на 9.8 ℃.
pub const ALTITUDE_LAPSE_C_PER_LEVEL: f32 = 0.0384;

/// Постоянная карта базовой температуры: отклонение от `GLOBAL_ANNUAL_MEAN_C`
/// в °C, по ячейке 2×2 тайла. Широта + крупный шум; высота применяется
/// при чтении, поэтому правки рельефа не делают карту устаревшей.
#[derive(Clone, Debug)]
pub struct ClimateMap {
    side: usize,
    offsets: Vec<i8>,
}

impl ClimateMap {
    /// Строит карту из seed и размера мира. Вместе с влажностью — общий
    /// проход `build_climate_maps` (warp считается один раз на ячейку).
    #[allow(dead_code)] // используется тестами напрямую
    pub fn new(tiles: u32, seed: u64) -> Self {
        build_climate_maps(tiles, seed).0
    }

    /// Билинейное отклонение базовой температуры от глобальной средней, ℃.
    pub fn offset_at(&self, x: i32, y: i32) -> f32 {
        let fx = x as f32 / CLIMATE_CELL_TILES as f32 - 0.5;
        let fy = y as f32 / CLIMATE_CELL_TILES as f32 - 0.5;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let sample = |ix: i32, iy: i32| {
            let ix = ix.clamp(0, self.side as i32 - 1) as usize;
            let iy = iy.clamp(0, self.side as i32 - 1) as usize;
            self.offsets[iy * self.side + ix] as f32
        };
        let top = sample(x0, y0) + (sample(x0 + 1, y0) - sample(x0, y0)) * tx;
        let bottom = sample(x0, y0 + 1) + (sample(x0 + 1, y0 + 1) - sample(x0, y0 + 1)) * tx;
        top + (bottom - top) * ty
    }

    /// Среднегодовая температура на тайле с учётом высоты, ℃.
    pub fn annual_temperature_c(&self, x: i32, y: i32, height: u8) -> f32 {
        GLOBAL_ANNUAL_MEAN_C + self.offset_at(x, y) + altitude_lapse_c(height)
    }

    /// Локальная температура: глобальный сезонный ambient + широта + высота.
    pub fn local_temperature_c(&self, ambient_c: f32, x: i32, y: i32, height: u8) -> f32 {
        ambient_c + self.offset_at(x, y) + altitude_lapse_c(height)
    }

    /// Байты карты (без заголовка `Vec`).
    pub fn memory_bytes(&self) -> u64 {
        self.offsets.len() as u64
    }
}

/// Постоянная карта влажности по ячейке 2×2 тайла, `u8` (0..255): крупный
/// и средний шум плюс прибрежная добавка. Как и `ClimateMap`, статична и
/// не требует ленивого обновления; в отличие от температуры, прибрежная
/// часть зависит от итоговой карты воды, поэтому генерация строит карту
/// после гидрологии (и она сохраняется в сейв).
#[derive(Clone, Debug)]
pub struct MoistureMap {
    side: usize,
    values: Vec<u8>,
}

/// Строит обе постоянные климатические карты за один проход: domain warp
/// считается один раз на ячейку 2×2 и переиспользуется температурой и
/// влажностью (#2.2). Карты детерминированы seed и не сохраняются.
pub fn build_climate_maps(tiles: u32, seed: u64) -> (ClimateMap, MoistureMap) {
    let side = MoistureMap::side_for(tiles);
    let mut offsets = vec![0_i8; side * side];
    let mut values = vec![0_u8; side * side];
    let n = tiles.max(1) as f32;
    for cy in 0..side {
        for cx in 0..side {
            let x = cx as f32 * CLIMATE_CELL_TILES as f32 + 0.5;
            let y = cy as f32 * CLIMATE_CELL_TILES as f32 + 0.5;
            // Domain warp: широтные полосы и границы биомов волнистые.
            let (wx, wy) = climate_warp(x, y, seed, 0xC11A_7E00);
            let px = x + wx;
            let py = y + wy;

            // Температура: широта + крупный шум, без высоты.
            let latitude = ((py / n) * 2.0 - 1.0).abs().min(1.0);
            let base = 20.0 - 45.0 * latitude;
            let regional = (value_noise(
                px / 96.0 + 31.0,
                py / 96.0 + 17.0,
                seed ^ 0xC11A_7E00,
            ) - 0.5)
                * 10.0;
            let offset = (base - GLOBAL_ANNUAL_MEAN_C + regional).clamp(-45.0, 45.0);
            offsets[cy * side + cx] = offset.round() as i8;

            // Влажность: крупный и средний шум (та же искривлённая точка).
            let m = base_moisture(px, py, seed);
            values[cy * side + cx] = (m * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    (
        ClimateMap { side, offsets },
        MoistureMap { side, values },
    )
}

impl MoistureMap {
    /// Сторона карты для мира в `tiles` тайлов.
    pub fn side_for(tiles: u32) -> usize {
        (tiles as usize).div_ceil(CLIMATE_CELL_TILES as usize)
    }

    /// Базовая карта влажности из seed. Как и температура, строится из
    /// seed в `World::new` и не сохраняется.
    #[allow(dead_code)] // используется тестами напрямую
    pub fn new(tiles: u32, seed: u64) -> Self {
        build_climate_maps(tiles, seed).1
    }

    /// Сторона карты в ячейках.
    #[allow(dead_code)] // используется тестами напрямую
    pub fn side(&self) -> usize {
        self.side
    }

    /// Билинейная влажность в [0, 1] по координате тайла.
    pub fn moisture_at(&self, x: i32, y: i32) -> f32 {
        let cell = CLIMATE_CELL_TILES as f32;
        let fx = x as f32 / cell - 0.5;
        let fy = y as f32 / cell - 0.5;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let sample = |ix: i32, iy: i32| {
            let ix = ix.clamp(0, self.side as i32 - 1) as usize;
            let iy = iy.clamp(0, self.side as i32 - 1) as usize;
            self.values[iy * self.side + ix] as f32
        };
        let top = sample(x0, y0) + (sample(x0 + 1, y0) - sample(x0, y0)) * tx;
        let bottom = sample(x0, y0 + 1) + (sample(x0 + 1, y0 + 1) - sample(x0, y0 + 1)) * tx;
        (top + (bottom - top) * ty) / 255.0
    }

    /// Байты карты (без заголовка `Vec`).
    pub fn memory_bytes(&self) -> u64 {
        self.values.len() as u64
    }
}

/// Базовый шум влажности в [0, 1]: крупные + средние пятна.
fn base_moisture(x: f32, y: f32, seed: u64) -> f32 {
    let large = fbm(x / 48.0 + 17.0, y / 48.0 + 61.0, seed ^ 0x0A01_5EED, 2);
    let detail = fbm(x / 16.0, y / 16.0, seed ^ 0x0A02_5EED, 2);
    (large * 0.72 + detail * 0.28).clamp(0.0, 1.0)
}

/// Падение температуры с высотой, ℃.
#[inline]
pub fn altitude_lapse_c(height: u8) -> f32 {
    -(height as f32) * ALTITUDE_LAPSE_C_PER_LEVEL
}

/// Среднемесячная базовая температура, ℃. Индекс = месяц года минус 1
/// в календаре игры: месяцы 0–2 — весна, 3–5 — лето, 6–8 — осень,
/// 9–11 — зима (`Season::from_month`). Значения — умеренный пояс
/// (порядка центра Европы): от −5 ℃ в разгар зимы до +23.5 ℃ летом.
const MONTH_BASE_TEMP_C: [f32; 12] = [
    2.0, // весна: март
    9.0, // апрель
    16.0, // май
    21.0, // лето: июнь
    23.5, // июль
    23.0, // август
    18.0, // осень: сентябрь
    11.0, // октябрь
    4.0, // ноябрь
    -1.5, // зима: декабрь
    -5.0, // январь
    -3.5, // февраль
];

/// Амплитуда суточного хода температуры, ℃ (от базы до максимума).
const DIURNAL_AMPLITUDE_C: f32 = 6.0;

/// Час суток суточного максимума: земля прогрета к середине дня.
const DIURNAL_PEAK_HOUR: f32 = 15.0;

/// Температура окружения на тике `tick`, ℃. Непрерывна по времени:
/// месячные базы интерполируются между соседними месяцами, суточный ход —
/// косинусоида от доли суток. Детерминировано тиком календаря.
pub fn ambient_temperature_c(tick: u64) -> f32 {
    let game_secs = tick as f64 * GAME_SECS_PER_TICK as f64;

    // Дробный номер месяца с начала мира: интерполяция между базами
    // соседних месяцев даёт плавный сезонный ход без скачков на границе.
    let month_f = (game_secs / SECS_PER_MONTH as f64) as f32;
    let month_idx = (month_f.floor() as usize) % 12;
    let next_idx = (month_idx + 1) % 12;
    let frac = month_f.fract();
    let base =
        MONTH_BASE_TEMP_C[month_idx] + (MONTH_BASE_TEMP_C[next_idx] - MONTH_BASE_TEMP_C[month_idx]) * frac;

    // Суточный ход: пик в `DIURNAL_PEAK_HOUR`, минимум через полсуток.
    let secs_of_day = game_secs % SECS_PER_DAY as f64;
    let hours = secs_of_day as f32 / 3600.0;
    let angle = (hours - DIURNAL_PEAK_HOUR) / 24.0 * std::f32::consts::TAU;
    base + DIURNAL_AMPLITUDE_C * angle.cos()
}
