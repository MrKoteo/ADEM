//! Тесты климата: температура как чистая функция календаря — сезонный
//! ход, суточный ход и непрерывность.

use crate::simulation::climate::ambient_temperature_c;
use crate::simulation::time::{TICKS_PER_DAY, TICKS_PER_MONTH, TICKS_PER_YEAR, start_tick};

/// Сезонный ход: в разгар лета теплее, чем в разгар зимы; средние за месяц
/// значения соответствуют умеренному климату (отрицательные зимой).
#[test]
fn temperature_follows_seasons() {
    // Календарь начинается с первого весеннего месяца: месяцы 0–2 — весна,
    // 9–11 — зима. Берём середину каждого месяца (суточный ход в середине
    // дня сглажен амплитудой и не влияет на порядок).
    let mid_of = |month: u64| -> f32 {
        let tick = TICKS_PER_YEAR + month * TICKS_PER_MONTH + TICKS_PER_MONTH / 2;
        ambient_temperature_c(tick)
    };
    let winter = mid_of(10); // второй зимний месяц — самый холодный
    let summer = mid_of(4); // второй летний месяц — самый тёплый
    assert!(winter < 0.0, "зима отрицательная: {winter}");
    assert!(summer > 20.0, "лето тёплое: {summer}");
    assert!(summer - winter > 20.0, "годовая амплитуда велика: {summer} vs {winter}");

    // Осень холоднее лета и теплее зимы.
    let autumn = mid_of(7);
    assert!(autumn > winter && autumn < summer);
}

/// Суточный ход: после полудня теплее, чем перед рассветом, на величину
/// порядка удвоенной амплитуды.
#[test]
fn temperature_follows_day_night_cycle() {
    // Один и тот же день (середина лета): 15:00 и 3:00.
    use crate::simulation::time::SECS_PER_HOUR;
    let day_start = TICKS_PER_YEAR + 4 * TICKS_PER_MONTH + 7 * TICKS_PER_DAY;
    let noon = ambient_temperature_c(day_start + 15 * SECS_PER_HOUR / 12);
    let night = ambient_temperature_c(day_start + 3 * SECS_PER_HOUR / 12);
    assert!(noon > night, "день теплее ночи: {noon} vs {night}");
    assert!(noon - night > 8.0, "суточная амплитуда ощутима: {}", noon - night);
    assert!(noon - night < 13.0, "суточная амплитуда умеренна: {}", noon - night);
}

/// Непрерывность: температура на соседних тиках меняется незначительно,
/// стартовый тик мира даёт разумное весеннее утро.
#[test]
fn temperature_is_continuous_and_sane() {
    let a = ambient_temperature_c(1_000_000);
    let b = ambient_temperature_c(1_000_001);
    assert!((a - b).abs() < 0.1, "скачок за тик: {a} -> {b}");

    // Мир начинается в 8:00 первого весеннего дня: умеренное утро.
    let start = ambient_temperature_c(start_tick());
    assert!(start > 0.0 && start < 15.0, "стартовая температура: {start}");

    // Экстремумы ограничены: климат умеренный, без «сотни в тени».
    let mut t = 0_u64;
    while t < TICKS_PER_YEAR {
        let temp = ambient_temperature_c(t);
        assert!((-15.0..=35.0).contains(&temp), "температура вне климата: {temp}");
        t += TICKS_PER_DAY / 4;
    }
}
