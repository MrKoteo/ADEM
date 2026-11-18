//! Чанк — блок 16×16 тайлов. Несколько независимых слоёв: рельеф, высоты,
//! ресурсы, объекты, предметы на земле, культуры, контейнеры станций.
//! Слои хранятся и сжимаются независимо (GAME_DESIGN 3.2, ARCHITECTURE 2.2).

use std::collections::HashMap;

use super::containers::Container;
use super::crops::Crop;
use super::heightmap::Heightmap;
use super::items::ItemStack;
use super::objects::WorldObject;
use super::tile::{ResourceKind, TileType};

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_TILES: usize = CHUNK_SIZE * CHUNK_SIZE;

/// Слой рельефа: одно значение на весь чанк или полный массив тайлов.
/// Uniform — основной случай для однородных областей (океан, пустыня):
/// 1 байт вместо 256, обход без чтения массива.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerrainTiles {
    Uniform(TileType),
    Full(Box<[TileType; CHUNK_TILES]>),
}

impl TerrainTiles {
    /// Однородный слой заданного вида.
    pub fn uniform(kind: TileType) -> Self {
        Self::Uniform(kind)
    }
    /// Вид тайла по линейному индексу (0..256).
    pub fn get(&self, idx: usize) -> TileType {
        match self {
            Self::Uniform(kind) => *kind,
            Self::Full(arr) => arr[idx],
        }
    }

    /// Записывает вид тайла; первый отличающийся тайл разворачивает Uniform в Full.
    pub fn set(&mut self, idx: usize, kind: TileType) {
        match self {
            Self::Uniform(current) if *current == kind => {}
            Self::Uniform(current) => {
                let mut arr = Box::new([*current; CHUNK_TILES]);
                arr[idx] = kind;
                *self = Self::Full(arr);
            }
            Self::Full(arr) => arr[idx] = kind,
        }
    }

    /// Схлопывает полный слой в Uniform, если все тайлы одинаковы.
    /// Вызывается редко: после генерации, при «засыпании» чанка, при сохранении.
    pub fn try_compress(&mut self) {
        if let Self::Full(arr) = self {
            let first = arr[0];
            if arr.iter().all(|kind| *kind == first) {
                *self = Self::Uniform(first);
            }
        }
    }
}

/// Залежь ресурса на тайле.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResourceDeposit {
    pub kind: ResourceKind,
    pub amount: u16,
}

/// Разреженный слой чанка: «линейный индекс тайла → значение».
///
/// Пять `HashMap` в каждом чанке занимают ~240 байт **даже пустыми**
/// (структура hashbrown). На 16384² это миллион чанков и сотни мегабайт
/// впустую, хотя объекты/предметы/культуры есть на доле процента тайлов.
/// Слой выделяет таблицу при первой записи и не занимает ничего, пока пуст.
/// API повторяет нужную часть `HashMap`, чтобы код-потребитель не менялся.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
// Box здесь намеренный: цель — убрать 48-байтный заголовок таблицы из
// каждого чанка, пока слой пуст (см. комментарий выше).
#[allow(clippy::box_collection)]
pub struct SparseLayer<T> {
    map: Option<Box<HashMap<u8, T>>>,
}

impl<T> SparseLayer<T> {
    /// Пустой слой (без таблицы).
    pub fn new() -> Self {
        Self { map: None }
    }
    /// Слой из уже собранной таблицы (загрузка сейва); пустая — без таблицы.
    pub fn from_map(map: HashMap<u8, T>) -> Self {
        Self {
            map: (!map.is_empty()).then(|| Box::new(map)),
        }
    }
    pub fn get(&self, key: &u8) -> Option<&T> {
        self.map.as_ref().and_then(|map| map.get(key))
    }
    pub fn get_mut(&mut self, key: &u8) -> Option<&mut T> {
        self.map.as_mut().and_then(|map| map.get_mut(key))
    }
    pub fn contains_key(&self, key: &u8) -> bool {
        self.get(key).is_some()
    }
    /// Вставляет значение, выделяя таблицу при первой записи.
    pub fn insert(&mut self, key: u8, value: T) -> Option<T> {
        self.map
            .get_or_insert_with(|| Box::new(HashMap::new()))
            .insert(key, value)
    }
    pub fn remove(&mut self, key: &u8) -> Option<T> {
        self.map.as_mut().and_then(|map| map.remove(key))
    }
    pub fn len(&self) -> usize {
        self.map.as_ref().map_or(0, |map| map.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = (&u8, &T)> {
        self.map.iter().flat_map(|map| map.iter())
    }
    pub fn keys(&self) -> impl Iterator<Item = &u8> {
        self.map.iter().flat_map(|map| map.keys())
    }
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.map.iter().flat_map(|map| map.values())
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&u8, &mut T) -> bool) {
        if let Some(map) = &mut self.map {
            map.retain(|key, value| keep(key, value));
        }
    }
    /// Освобождает таблицу целиком (память возвращается аллокатору).
    pub fn clear(&mut self) {
        self.map = None;
    }
}

impl<'a, T> IntoIterator for &'a SparseLayer<T> {
    type Item = (&'a u8, &'a T);
    type IntoIter = std::iter::Flatten<std::option::IntoIter<&'a HashMap<u8, T>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.as_deref().into_iter().flatten()
    }
}

/// Чанк 16×16. Рельеф — плотный слой со сжатием; остальные слои —
/// разреженные: они занимают малую долю тайлов, и плотный массив на каждый
/// слой раздувал бы память на порядки (ARCHITECTURE 2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub terrain: TerrainTiles,
    /// Высоты тайлов 0..255 (0 — океан/море по умолчанию).
    pub heights: Heightmap,
    /// Ресурсы по линейному индексу тайла 0..256.
    pub resources: SparseLayer<ResourceDeposit>,
    /// Объекты по линейному индексу тайла 0..256. Правило: один объект на тайл.
    pub objects: SparseLayer<WorldObject>,
    /// Предметы, лежащие на тайлах (дроп). Разреженный слой, как ресурсы.
    pub items: SparseLayer<ItemStack>,
    /// Культуры на пашне по линейному индексу тайла (одна культура на тайл).
    pub crops: SparseLayer<Crop>,
    /// Контейнеры станций и складов (печи, бочка, склад, костёр).
    /// Существуют только у тайлов со станцией-объектом.
    pub containers: SparseLayer<Container>,
}

impl Chunk {
    /// Пустой чанк: рельеф Empty, остальных слоёв нет.
    pub fn empty() -> Self {
        Self {
            terrain: TerrainTiles::uniform(TileType::Empty),
            heights: Heightmap::uniform(0),
            resources: SparseLayer::new(),
            objects: SparseLayer::new(),
            items: SparseLayer::new(),
            crops: SparseLayer::new(),
            containers: SparseLayer::new(),
        }
    }
}
