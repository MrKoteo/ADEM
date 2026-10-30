//! Lazy-потребности: значение не тикает каждый тик, а вычисляется по запросу
//! (GAME_DESIGN 3.5). Модуль готовится к итерации «Существа и потребности»
//! и пока не имеет потребителей — поэтому allow(dead_code) на весь модуль.

#![allow(dead_code)]

/// Вид потребности (GAME_DESIGN 3.5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum NeedKind {
    Hunger,
    Thirst,
    Sleep,
    Temperature,
    Safety,
    Social,
}

impl NeedKind {
    /// Вид потребности по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Hunger),
            1 => Some(Self::Thirst),
            2 => Some(Self::Sleep),
            3 => Some(Self::Temperature),
            4 => Some(Self::Safety),
            5 => Some(Self::Social),
            _ => None,
        }
    }
}

/// Потребность существа. Хранятся только значение на тике последнего
/// пересчёта и скорость — актуальное значение на любой тик восстанавливается
/// по запросу. Это убирает O(существа) работу на каждом тике: пересчёт нужен
/// только при смене условий или чтении значения.
#[derive(Clone, Copy, Debug)]
pub struct Need {
    /// Значение на тик `since_tick`, всегда в [0, 100].
    value: f32,
    /// Тик последнего пересчёта value.
    since_tick: u64,
    /// Скорость изменения, единиц за тик: положительная — растёт, отрицательная — убывает.
    rate: f32,
}

impl Need {
    /// Создаёт потребность со значением `value` на тике 0 и скоростью `rate`.
    pub fn new(value: f32, rate: f32) -> Self {
        Self { value: value.clamp(0.0, 100.0), since_tick: 0, rate }
    }

    /// Актуальное значение на тик `now` без записи состояния.
    /// `now` меньше `since_tick` (например, после отката к более раннему
    /// сохранению) считается нулевым интервалом — арифметика тиков не паникует.
    pub fn value_at(&self, now: u64) -> f32 {
        (self.value + self.rate * now.saturating_sub(self.since_tick) as f32).clamp(0.0, 100.0)
    }

    /// Фиксирует накопленное изменение: после close значение актуально на `now`.
    /// Вызывается перед сменой скорости, порога или сохранением.
    pub fn close(&mut self, now: u64) {
        self.value = self.value_at(now);
        self.since_tick = now;
    }

    /// Меняет скорость, закрыв накопленное (начал бежать, лёг спать и т. п.).
    pub fn set_rate(&mut self, now: u64, rate: f32) {
        self.close(now);
        self.rate = rate;
    }

    /// Через сколько тиков значение достигнет `threshold` при текущей скорости.
    /// None — скорость нулевая или порог лежит в недостижимую сторону; тогда
    /// владелец потребности не планирует пороговое событие в планировщике.
    /// Вызывать после set_rate/close, чтобы значение было актуально.
    pub fn ticks_until(&self, threshold: f32) -> Option<u64> {
        if self.rate == 0.0 {
            return None;
        }
        let ticks = (threshold - self.value) / self.rate;
        if !ticks.is_finite() || ticks < 0.0 {
            return None;
        }
        Some(ticks.ceil() as u64)
    }
}
