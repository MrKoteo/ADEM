//! Лёгкий детерминированный RNG для решений, не требующих памяти состояния.
//!
//! SplitMix64 — это один `u64` состояния и три умножения на бросок. Для
//! решений ИИ (`decision_rng`/`wander_rng`) и событий растительности
//! `StdRng` с инициализацией ChaCha-состояния на каждое решение был
//! заметной долей стоимости при тысячах существ: состояние всё равно не
//! сохраняется, поток сеется заново от (seed, id/координаты, тик).
//! Качество распределения для игровых процентов и выбора среди соседей
//! достаточное, детерминизм — в пределах платформы (ARCHITECTURE 2.13).

/// Минимальный splitmix64: состояние 8 байт, бросок — O(1).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// RNG с заданным начальным состоянием.
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Следующее 64-битное число (генератор splitmix64).
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Бросок [0, 1); 24 бита мантиссы — как у `f32`-случайности rand.
    pub(crate) fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// Равномерный индекс [0, len); пустой диапазон не паникует.
    pub(crate) fn next_below(&mut self, len: usize) -> usize {
        (self.next_u64() % len.max(1) as u64) as usize
    }

    /// Равномерное целое из [min, max] включительно.
    pub(crate) fn next_range_inclusive(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + self.next_u64() % (max - min + 1)
    }
}

/// Соль/перемешивание seed'а перед созданием RNG решения: домен задачи
/// отделяется от соседних константой, чтобы близкие (id, тик) не давали
/// коррелированные потоки.
pub(crate) fn mix_seed(seed: u64, salt: u64) -> u64 {
    let mut x = seed ^ salt;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
