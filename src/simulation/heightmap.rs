//! Слой высот чанка: точное значение 0..4095 на каждый тайл.
//!
//! Хранение — по чанку 16×16 с локальным минимумом и минимально возможной
//! битовой шириной дельт (`ceil(log2(max - min + 1))`). Однородные чанки
//! (океан, плато) хранят одно значение и не занимают массив дельт.
//! Схема точная: `height = min + delta`, без квантования и потерь.
//!
//! Доступ O(1): одно-два чтения `u64` и битовые операции. Правка тайла
//! внутри текущего диапазона чанка — запись на месте; при выходе за
//! диапазон чанк переупаковывается (256 значений, редко).
//!
//! Слой принадлежит симуляции (без Bevy): он сохраняется в сейв и в будущем
//! будет влиять на механики. Рендер только читает высоты.

use super::chunk::CHUNK_TILES;

/// Максимальное значение высоты (диапазон 0..=4095, 12 бит).
pub const HEIGHT_MAX: u16 = 4095;

/// Уровень моря в единицах высоты. Согласован с генерацией: море — 64
/// (2 тайла при `HEIGHT_WORLD_UNIT = 0.5`), камень выше 448, горы выше 1088
/// (см. `generation/height.rs`).
pub const SEA_LEVEL: u16 = 64;

/// Максимальная битовая ширина дельт (12 бит покрывают весь диапазон).
const MAX_BITS: u8 = 12;

/// Упакованные дельты неоднородного чанка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedHeights {
    /// Локальный минимум высоты в чанке.
    pub min: u16,
    /// Бит на одну дельту, 1..=12.
    pub bits: u8,
    /// Упакованные дельты по `bits` бит, little-endian по битовому потоку.
    pub words: Box<[u64]>,
}

impl PackedHeights {
    /// Маска одной дельты.
    #[inline]
    fn mask(&self) -> u64 {
        (1_u64 << self.bits) - 1
    }

    /// Читает дельту тайла `idx` (0..256).
    #[inline]
    fn delta(&self, idx: usize) -> u64 {
        let bit = idx * self.bits as usize;
        let word = bit / 64;
        let shift = bit % 64;
        let mut value = self.words[word] >> shift;
        // Значение пересекает границу слов — добираем старшие биты.
        if shift + self.bits as usize > 64 {
            value |= self.words[word + 1] << (64 - shift);
        }
        value & self.mask()
    }

    /// Записывает дельту тайла `idx`; вызывающий гарантирует, что она
    /// помещается в `bits`.
    #[inline]
    fn set_delta(&mut self, idx: usize, delta: u64) {
        write_bits(&mut self.words, idx * self.bits as usize, self.bits, delta);
    }

    /// Абсолютная высота тайла.
    #[inline]
    pub fn get(&self, idx: usize) -> u16 {
        self.min + self.delta(idx) as u16
    }

    /// Сколько байт занимает массив дельт (без заголовка и аллокатора).
    pub fn packed_bytes(&self) -> u64 {
        (self.words.len() * std::mem::size_of::<u64>()) as u64
    }
}

/// Записывает `bits` младших бит `value` в битовый поток, начиная с `bit`.
/// Значение может пересекать границу двух слов.
#[inline]
fn write_bits(words: &mut [u64], bit: usize, bits: u8, value: u64) {
    let word = bit / 64;
    let shift = bit % 64;
    let mask = (1_u64 << bits) - 1;
    let value = value & mask;
    // Низкая часть. Сдвиг маски за пределы u64 отбрасывает лишние биты —
    // они относятся к следующему слову и очищаются отдельно.
    words[word] = (words[word] & !(mask << shift)) | (value << shift);
    if shift + bits as usize > 64 {
        let high_bits = shift + bits as usize - 64;
        let high_mask = (1_u64 << high_bits) - 1;
        words[word + 1] = (words[word + 1] & !high_mask) | (value >> (64 - shift));
    }
}

/// Слой высот одного чанка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heightmap {
    /// Все тайлы чанка на одной высоте; массив не хранится.
    Uniform(u16),
    /// Неоднородный чанк: минимум + упакованные дельты.
    Packed(PackedHeights),
}

impl Heightmap {
    /// Однородный слой.
    pub fn uniform(height: u16) -> Self {
        Self::Uniform(height.min(HEIGHT_MAX))
    }

    /// Собирает слой из полного массива высот за один проход.
    /// Используется генерацией: 256 `set` на чанк не нужны.
    pub fn from_tiles(tiles: &[u16; CHUNK_TILES]) -> Self {
        let mut min = HEIGHT_MAX;
        let mut max = 0_u16;
        for &tile in tiles {
            let tile = tile.min(HEIGHT_MAX);
            min = min.min(tile);
            max = max.max(tile);
        }
        if min == max {
            return Self::Uniform(min);
        }
        let range = u32::from(max - min);
        // ceil(log2(range + 1)) = число бит, достаточное для range.
        let bits = (32 - range.leading_zeros()) as u8;
        debug_assert!((1..=MAX_BITS).contains(&bits));
        let word_count = bit_words(bits);
        let mut words = vec![0_u64; word_count];
        for (idx, &tile) in tiles.iter().enumerate() {
            let delta = u64::from(tile.min(HEIGHT_MAX) - min);
            write_bits(&mut words, idx * bits as usize, bits, delta);
        }
        Self::Packed(PackedHeights {
            min,
            bits,
            words: words.into_boxed_slice(),
        })
    }

    /// Высота тайла по линейному индексу 0..256.
    #[inline]
    pub fn get(&self, idx: usize) -> u16 {
        debug_assert!(idx < CHUNK_TILES);
        match self {
            Self::Uniform(height) => *height,
            Self::Packed(packed) => packed.get(idx),
        }
    }

    /// Записывает высоту тайла. Возвращает `true`, если значение изменилось.
    /// Внутри текущего диапазона запись идёт на месте; при расширении
    /// диапазона чанк переупаковывается (256 значений).
    pub fn set(&mut self, idx: usize, height: u16) -> bool {
        debug_assert!(idx < CHUNK_TILES);
        let height = height.min(HEIGHT_MAX);
        match self {
            Self::Uniform(current) => {
                if *current == height {
                    return false;
                }
                let mut tiles = [*current; CHUNK_TILES];
                tiles[idx] = height;
                *self = Self::from_tiles(&tiles);
                true
            }
            Self::Packed(packed) => {
                let fits = height >= packed.min
                    && u32::from(height - packed.min) < (1_u32 << packed.bits);
                if fits {
                    let old = packed.get(idx);
                    if old == height {
                        return false;
                    }
                    packed.set_delta(idx, u64::from(height - packed.min));
                    return true;
                }
                // Значение вышло за диапазон: переупаковываем чанк.
                let mut tiles = [0_u16; CHUNK_TILES];
                for (slot, tile) in tiles.iter_mut().enumerate() {
                    *tile = packed.get(slot);
                }
                tiles[idx] = height;
                *self = Self::from_tiles(&tiles);
                true
            }
        }
    }

    /// Однородное значение, если весь чанк на одной высоте.
    /// Публичный помощник для рендера и тестов.
    pub fn uniform_value(&self) -> Option<u16> {
        match self {
            Self::Uniform(height) => Some(*height),
            Self::Packed(_) => None,
        }
    }

    /// Схлопывает чанк в `Uniform`, если все тайлы сравнялись.
    /// Вызывается редко: после генерации и правок рельефа.
    pub fn try_compress(&mut self) {
        let Self::Packed(packed) = self else {
            return;
        };
        let first = packed.get(0);
        if (0..CHUNK_TILES).all(|idx| packed.get(idx) == first) {
            *self = Self::Uniform(first);
        }
    }

    /// Байты, занимаемые массивом дельт. Заголовок и место под enum уже
    /// входят в `size_of::<Chunk>()`; здесь только отдельная аллокация.
    pub fn memory_bytes(&self) -> u64 {
        match self {
            Self::Uniform(_) => 0,
            Self::Packed(packed) => packed.packed_bytes(),
        }
    }
}

/// Сколько `u64` нужно для 256 значений по `bits` бит.
#[inline]
fn bit_words(bits: u8) -> usize {
    (CHUNK_TILES * bits as usize).div_ceil(64)
}
