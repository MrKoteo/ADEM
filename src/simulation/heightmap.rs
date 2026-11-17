//! Слой высот чанка: высота 0..255 на каждый тайл.
//!
//! Хранение — по чанку 16×16: одно значение (`Uniform`) для однородного
//! чанка или плотный массив `u8` (`Full`). Пустой чанк и незаполненная
//! ячейка означают высоту по умолчанию — `SEA_LEVEL` (0, океан).
//!
//! Схема намеренно простая и быстрая: чтение — одно индексирование без
//! битовых операций (горячий путь меша и генерации). Однородные чанки
//! (океан, плато) массива не хранят; первый отличающийся тайл разворачивает
//! `Uniform` в `Full`, `try_compress` схлопывает обратно.
//!
//! Слой принадлежит симуляции (без Bevy): он сохраняется в сейв и в будущем
//! будет влиять на механики. Рендер только читает высоты.

use super::chunk::CHUNK_TILES;

/// Максимальное значение высоты (диапазон 0..=255).
pub const HEIGHT_MAX: u8 = 255;

/// Уровень моря: океан и море всегда на этой высоте (0). Меньше нуля
/// высот не бывает, поэтому вода — это ровно `SEA_LEVEL`.
pub const SEA_LEVEL: u8 = 0;

/// Слой высот одного чанка.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heightmap {
    /// Все тайлы чанка на одной высоте; массив не хранится.
    Uniform(u8),
    /// Неоднородный чанк: плотный массив высот.
    Full(Box<[u8; CHUNK_TILES]>),
}

impl Heightmap {
    /// Однородный слой.
    pub fn uniform(height: u8) -> Self {
        Self::Uniform(height)
    }

    /// Собирает слой из полного массива высот за один проход.
    /// Используется генерацией: 256 `set` на чанк не нужны.
    pub fn from_tiles(tiles: &[u8; CHUNK_TILES]) -> Self {
        let first = tiles[0];
        if tiles.iter().all(|&tile| tile == first) {
            return Self::Uniform(first);
        }
        Self::Full(Box::new(*tiles))
    }

    /// Высота тайла по линейному индексу 0..256. Незаполненная ячейка
    /// (однородный чанк) возвращает своё единственное значение.
    #[inline]
    pub fn get(&self, idx: usize) -> u8 {
        debug_assert!(idx < CHUNK_TILES);
        match self {
            Self::Uniform(height) => *height,
            Self::Full(tiles) => tiles[idx],
        }
    }

    /// Записывает высоту тайла. Возвращает `true`, если значение изменилось.
    /// Первая правка однородного чанка разворачивает массив; схлопывание —
    /// отдельный `try_compress`.
    pub fn set(&mut self, idx: usize, height: u8) -> bool {
        debug_assert!(idx < CHUNK_TILES);
        match self {
            Self::Uniform(current) => {
                if *current == height {
                    return false;
                }
                let mut tiles = Box::new([*current; CHUNK_TILES]);
                tiles[idx] = height;
                *self = Self::Full(tiles);
                true
            }
            Self::Full(tiles) => {
                if tiles[idx] == height {
                    return false;
                }
                tiles[idx] = height;
                true
            }
        }
    }

    /// Однородное значение, если весь чанк на одной высоте.
    /// Публичный помощник для рендера и тестов.
    pub fn uniform_value(&self) -> Option<u8> {
        match self {
            Self::Uniform(height) => Some(*height),
            Self::Full(_) => None,
        }
    }

    /// Схлопывает чанк в `Uniform`, если все тайлы сравнялись.
    /// Вызывается редко: после генерации и правок рельефа.
    pub fn try_compress(&mut self) {
        let Self::Full(tiles) = self else {
            return;
        };
        let first = tiles[0];
        if tiles.iter().all(|&tile| tile == first) {
            *self = Self::Uniform(first);
        }
    }

    /// Байты, занимаемые массивом высот. Заголовок и место под enum уже
    /// входят в `size_of::<Chunk>()`; здесь только отдельная аллокация.
    pub fn memory_bytes(&self) -> u64 {
        match self {
            Self::Uniform(_) => 0,
            Self::Full(_) => CHUNK_TILES as u64,
        }
    }
}
