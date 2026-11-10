//! Фазы луны: ночное освещение зависит от фазы, фазы сменяются каждые
//! двое игровых суток в полдень (чтобы скачок светлоты не был виден ночью).
//! Лунный цикл — 15 фаз (30 суток): новолуние → полнолуние → новолуние,
//! яркость растёт 0.03 … 0.24 и так же плавно убывает. Мир создаётся в
//! третьей фазе.
//!
//! Функции чистые от тика: состояние не хранится, сохранение не нужно, а
//! после загрузки сейва фаза восстанавливается по абсолютному тику.

use super::time::{
    GAME_SECS_PER_TICK, SECS_PER_DAY, SECS_PER_HOUR, START_HOUR_OF_DAY, start_tick,
};

/// Сколько фаз в лунном цикле: 8 растущих + 8 убывающих.
pub const MOON_PHASES: u8 = 15;

/// Фаза полнолуния (пик ночной светлоты).
#[allow(dead_code)] // контракт индексации: пик массива яркости и тесты
pub const MOON_FULL_PHASE: u8 = 7;

/// Фаза, в которой создаётся новый мир.
pub const MOON_START_PHASE: u8 = 3;

/// Сколько игровых суток держится одна фаза.
pub const MOON_PHASE_DAYS: u64 = 2;

/// Ночная светлота мира по фазам: новолуние → полнолуние → новолуние.
/// Значения — доля дневной освещённости (та же шкала, что у `daylight_factor`).
/// После полнолуния яркость убывает теми же шагами, что и росла, а две
/// последние фазы — новолуние (луна не видна).
pub const MOON_NIGHT_DAYLIGHT: [f32; MOON_PHASES as usize] = [
    0.03, 0.06, 0.09, 0.12, 0.15, 0.18, 0.21, 0.24, // рост
    0.21, 0.18, 0.15, 0.12, 0.09, 0.06, 0.03, // убывание
];

/// Названия фаз для UI (индекс — номер фазы).
const MOON_PHASE_LABELS: [&str; MOON_PHASES as usize] = [
    "Новолуние",
    "Молодой месяц",
    "Молодая луна",
    "Первая четверть",
    "Растущая луна",
    "Прибывающая луна",
    "Почти полная",
    "Полнолуние",
    "Спадающая луна",
    "Убывающая луна",
    "Убывающая луна",
    "Последняя четверть",
    "Старая луна",
    "Ветхая луна",
    "Тёмная луна",
];

/// Секунды от старта мира до первого полудня: мир стартует в 8:00, поэтому
/// первая смена фазы приходится на полдень вторых суток.
fn first_noon_secs() -> u64 {
    const START: u64 = START_HOUR_OF_DAY as u64 * SECS_PER_HOUR;
    const NOON: u64 = SECS_PER_DAY / 2;
    if START <= NOON {
        NOON - START
    } else {
        NOON + SECS_PER_DAY - START
    }
}

/// Номер шага лунного цикла с начала мира: смена каждые
/// `MOON_PHASE_DAYS` суток, в полдень. Первый шаг начинается в полдень
/// вторых суток — до него действует стартовая фаза.
fn moon_step(tick: u64) -> u64 {
    let secs = tick.wrapping_mul(GAME_SECS_PER_TICK as u64);
    let start = start_tick() * GAME_SECS_PER_TICK as u64;
    let elapsed = secs.saturating_sub(start) as i64;
    let phase_secs = (MOON_PHASE_DAYS * SECS_PER_DAY) as i64;
    ((elapsed - first_noon_secs() as i64) / phase_secs) as u64
}

/// Фаза луны (0 — новолуние, 7 — полнолуние, 8…15 — убывание). Сменяется
/// каждые `MOON_PHASE_DAYS` суток в полдень; мир создаётся в фазе
/// `MOON_START_PHASE` и далее луна прибывает.
pub fn phase(tick: u64) -> u8 {
    ((MOON_START_PHASE as u64 + moon_step(tick)) % MOON_PHASES as u64) as u8
}

/// Ночная светлота для тика: доля дневной освещённости (0.03…0.24).
pub fn night_daylight(tick: u64) -> f32 {
    MOON_NIGHT_DAYLIGHT[phase(tick) as usize]
}

/// Название фазы для F3-панели.
pub fn phase_label(phase: u8) -> &'static str {
    MOON_PHASE_LABELS[(phase as usize) % MOON_PHASES as usize]
}
