//! Оценка времени генерации мира по прошлым замерам.
//!
//! Генерация на большой карте занимает секунды, и прогресс-бару нужно
//! показать, сколько осталось. Время растёт примерно как площадь карты
//! (×4 при удвоении стороны), поэтому замеры нормализуются на площадь и
//! усредняются: оценка для любого размера — среднее «секунд на тайл»
//! умноженное на площадь.
//!
//! Файл `generation_times.json` читается один раз при старте и пишется
//! **только при выходе** — никаких записей на диск во время игры.
//! Хранится не больше `MAX_SAMPLES` последних замеров.

use std::fs;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Сколько замеров храним. Больше не нужно: значения усредняются с равным
/// весом, а старые быстро вытесняются новыми (FIFO).
const MAX_SAMPLES: usize = 20;

const TIMES_PATH: &str = "generation_times.json";

/// Один замер: сторона карты в тайлах и полное время генерации (с посевом
/// растительности) в секундах.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct Sample {
    side: u32,
    secs: f32,
}

/// Таблица замеров времени генерации (ресурс). Пишется на диск при выходе
/// из игры (`Drop` ресурса — Last-система `AppExit` не гарантирует кадра
/// после закрытия окна), читается один раз при старте.
#[derive(Resource, Default)]
pub struct GenerationTimes {
    samples: Vec<Sample>,
    /// Куда писать при выходе; пусто у таблицы, созданной не из файла
    /// (тесты, `Default`).
    path: PathBuf,
}

impl GenerationTimes {
    /// Читает таблицу с диска. Битый файл — пустая таблица (оценки просто
    /// не будет, прогресс-бар покажет проценты без «осталось»).
    pub fn load(path: &Path) -> Self {
        let samples = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<Sample>>(&text).ok())
            .unwrap_or_default();
        let mut times = Self {
            samples: samples
                .into_iter()
                .filter(|sample| sample.side > 0 && sample.secs.is_finite() && sample.secs > 0.0)
                .collect(),
            path: path.to_path_buf(),
        };
        if times.samples.len() > MAX_SAMPLES {
            let drop = times.samples.len() - MAX_SAMPLES;
            times.samples.drain(..drop);
        }
        times
    }

    /// Пишет таблицу на диск. Ошибка не фатальна.
    pub fn save(&self, path: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(&self.samples) {
            let _ = fs::write(path, json);
        }
    }

    /// Добавляет замер полного времени генерации карты `side`.
    pub fn record(&mut self, side: u32, secs: f32) {
        if side == 0 || !secs.is_finite() || secs <= 0.0 {
            return;
        }
        if self.samples.len() >= MAX_SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push(Sample { side, secs });
    }

    /// Ожидаемое время генерации карты `side`, секунды. `None` — замеров
    /// ещё нет. Нормировка на площадь делает замеры 256² и 4096²
    /// сопоставимыми; усреднение гасит разброс железа и нагрузки.
    pub fn estimate_secs(&self, side: u32) -> Option<f32> {
        if self.samples.is_empty() || side == 0 {
            return None;
        }
        let per_tile: f64 = self
            .samples
            .iter()
            .map(|sample| {
                let tiles = (sample.side as f64) * (sample.side as f64);
                sample.secs as f64 / tiles
            })
            .sum::<f64>()
            / self.samples.len() as f64;
        Some((per_tile * (side as f64) * (side as f64)) as f32)
    }

    /// Число замеров (для тестов и отладки).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

impl Drop for GenerationTimes {
    /// Запись на диск только при выходе: ресурс уничтожается вместе с App.
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            self.save(&self.path);
        }
    }
}

/// Путь к generation_times.json — рядом с settings.json: замеры не должны
/// разъезжаться с настройками при разных способах запуска (cargo run из
/// корня проекта или exe напрямую).
pub fn times_path() -> PathBuf {
    let settings = crate::app::settings::settings_path();
    match settings.parent() {
        Some(directory) => directory.join(TIMES_PATH),
        None => PathBuf::from(TIMES_PATH),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Оценка масштабируется по площади: замер 256² ≈ 1 с даёт ~16 с для 1024²
    /// (площадь в 16 раз больше). Несколько замеров усредняются.
    #[test]
    fn estimate_scales_with_area() {
        let mut times = GenerationTimes::default();
        times.record(256, 1.0);
        let estimate = times.estimate_secs(1024).expect("замер есть");
        assert!((estimate - 16.0).abs() < 0.01, "оценка {estimate}");

        times.record(256, 3.0);
        let estimate = times.estimate_secs(512).expect("замеры есть");
        assert!((estimate - 8.0).abs() < 0.01, "оценка {estimate}");
    }

    /// Хранится не больше 20 замеров; новые вытесняют старые.
    #[test]
    fn samples_are_capped() {
        let mut times = GenerationTimes::default();
        for index in 0..30 {
            times.record(256 + index, 1.0);
        }
        assert_eq!(times.len(), MAX_SAMPLES);
    }

    /// Битые (отрицательные/нулевые) замеры не принимаются.
    #[test]
    fn invalid_samples_are_ignored() {
        let mut times = GenerationTimes::default();
        times.record(0, 1.0);
        times.record(256, 0.0);
        times.record(256, f32::NAN);
        assert_eq!(times.len(), 0);
        assert!(times.estimate_secs(256).is_none());
    }
}
