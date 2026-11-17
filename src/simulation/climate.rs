//! Климат: температура окружения как чистая функция календаря.
//!
//! Основа — среднемесячные температуры умеренного климата, поверх —
//! суточный ход (синусоида с минимумом перед рассветом и пиком в середине
//! дня). Значение одно на весь мир: локальный климат (высота, вода,
//! погода) появится вместе с погодой. Пока температура ни на что не
//! влияет — закладываем основу: поле `World::ambient_temperature_c`
//! обновляется каждый тик, читают UI и будущие потребности.

use super::noise::value_noise;
use super::time::{GAME_SECS_PER_TICK, SECS_PER_DAY, SECS_PER_MONTH};

/// Сторона ячейки карты температуры, тайлов. 2×2 — компромисс между
/// плавностью биомов и памятью (0.25 Б/тайл для `i8`).
pub const CLIMATE_CELL_TILES: i32 = 2;

/// Среднегодовая температура глобального умеренного климата, ℃. Базовая
/// карта хранит отклонение от неё, а не абсолютную температуру.
pub const GLOBAL_ANNUAL_MEAN_C: f32 = 9.0;

/// Падение температуры с высотой, ℃ на уровень. Полная амплитуда 4095
/// уровней даёт около −19.7 ℃ — этого достаточно для снеговой линии.
pub const ALTITUDE_LAPSE_C_PER_LEVEL: f32 = 0.0048;

/// Постоянная карта базовой температуры: отклонение от `GLOBAL_ANNUAL_MEAN_C`
/// в °C, по ячейке 2×2 тайла. Широта + крупный шум; высота применяется
/// при чтении, поэтому правки рельефа не делают карту устаревшей.
#[derive(Clone, Debug)]
pub struct ClimateMap {
    side: usize,
    offsets: Vec<i8>,
}

impl ClimateMap {
    /// Строит карту из seed и размера мира. O(тайлов/4), без высот.
    pub fn new(tiles: u32, seed: u64) -> Self {
        let side = (tiles as usize).div_ceil(CLIMATE_CELL_TILES as usize);
        let mut offsets = vec![0_i8; side * side];
        let n = tiles.max(1) as f32;
        for cy in 0..side {
            for cx in 0..side {
                let x = cx as f32 * CLIMATE_CELL_TILES as f32 + 0.5;
                let y = cy as f32 * CLIMATE_CELL_TILES as f32 + 0.5;
                // Широта: 0 на экваторе, 1 у полюсов. База +20…−25 ℃.
                let latitude = ((y / n) * 2.0 - 1.0).abs().min(1.0);
                let base = 20.0 - 45.0 * latitude;
                let regional = (value_noise(
                    x / 96.0 + 31.0,
                    y / 96.0 + 17.0,
                    seed ^ 0xC11A_7E00,
                ) - 0.5)
                    * 10.0;
                let offset = (base - GLOBAL_ANNUAL_MEAN_C + regional).clamp(-45.0, 45.0);
                offsets[cy * side + cx] = offset.round() as i8;
            }
        }
        Self { side, offsets }
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
    pub fn annual_temperature_c(&self, x: i32, y: i32, height: u16) -> f32 {
        GLOBAL_ANNUAL_MEAN_C + self.offset_at(x, y) + altitude_lapse_c(height)
    }

    /// Локальная температура: глобальный сезонный ambient + широта + высота.
    pub fn local_temperature_c(&self, ambient_c: f32, x: i32, y: i32, height: u16) -> f32 {
        ambient_c + self.offset_at(x, y) + altitude_lapse_c(height)
    }

    /// Байты карты (без заголовка `Vec`).
    pub fn memory_bytes(&self) -> u64 {
        self.offsets.len() as u64
    }
}

/// Падение температуры с высотой, ℃.
#[inline]
pub fn altitude_lapse_c(height: u16) -> f32 {
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
