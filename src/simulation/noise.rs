//! Шумовые функции генерации: собственный value-noise без внешних крейтов.
//!
//! Все функции чистые и детерминированные от seed/координат. Порядок
//! вычислений фиксирован, поэтому одинаковый seed даёт одинаковую карту в
//! пределах платформы.

/// Хэш пары (x, y) с seed → float в [0, 1).
#[inline]
pub(crate) fn hash2(x: i32, y: i32, seed: u64) -> f32 {
    let mut h = (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ seed;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h & 0xFFFFFF) as f32) / (0xFFFFFF as f32)
}

/// 64-битный хэш для выбора архетипа карты и потоков RNG.
#[inline]
pub(crate) fn hash64(seed: u64) -> u64 {
    let mut h = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

/// 2D value noise с bilinear interpolation и smoothstep.
pub(crate) fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let v00 = hash2(x0, y0, seed);
    let v10 = hash2(x0 + 1, y0, seed);
    let v01 = hash2(x0, y0 + 1, seed);
    let v11 = hash2(x0 + 1, y0 + 1, seed);

    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);

    let v0 = v00 + (v10 - v00) * sx;
    let v1 = v01 + (v11 - v01) * sx;
    v0 + (v1 - v0) * sy
}

/// Фрактальный шум (сумма октав value noise, нормированная).
pub(crate) fn fbm(x: f32, y: f32, seed: u64, octaves: u32) -> f32 {
    let mut total = 0.0;
    let mut amp = 1.0;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for i in 0..octaves {
        let s = seed.wrapping_add((i as u64).wrapping_mul(1000));
        total += value_noise(x * freq, y * freq, s) * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    total / norm
}

/// Гребневый шум для горных хребтов: узкие вытянутые гребни вместо бугров.
pub(crate) fn ridge_noise(x: f32, y: f32, seed: u64, octaves: u32) -> f32 {
    let mut total = 0.0;
    let mut amp = 1.0;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for i in 0..octaves {
        let s = seed.wrapping_add((i as u64).wrapping_mul(1000));
        let v = 1.0 - (2.0 * value_noise(x * freq, y * freq, s) - 1.0).abs();
        total += v * v * amp;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    total / norm
}

/// Smoothstep в [0, 1] — плавная кривая без изломов.
#[inline]
pub(crate) fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
