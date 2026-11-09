//! Описание тайла и его ресурсов.

/// Вид тайла. Значения — стабильные идентификаторы (repr(u8)),
/// чтобы позже использовать в сохранениях и при обмене с рендером.
/// Новые варианты добавляются только в конец, чтобы не сдвигать
/// значения существующих (правило то же, что у `ResourceKind`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum TileType {
    Empty = 0,
    Water,
    Sand,
    Dirt,
    Grass,
    ForestGrass,
    Stone,
    Clay,
    Farmland,
    /// Глубокая вода: середина морей и глубокие озёра.
    DeepWater,
    /// Океан: самые глубокие воды у границ карты; в озёрах не встречается.
    Ocean,
    /// Гора: высокий рельеф над камнем.
    Mountain,
    /// Мох: пятнами в лесу на уровне лесной травы.
    Moss,
    /// Грязь: задел под будущие биомы, пока не генерируется.
    Mud,
    /// Подзол: задел под будущие биомы, пока не генерируется.
    Podzol,
}

impl TileType {
    /// Вид тайла по стабильному значению u8 (сохранения, обмен с рендером).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Empty),
            1 => Some(Self::Water),
            2 => Some(Self::Sand),
            3 => Some(Self::Dirt),
            4 => Some(Self::Grass),
            5 => Some(Self::ForestGrass),
            6 => Some(Self::Stone),
            7 => Some(Self::Clay),
            8 => Some(Self::Farmland),
            9 => Some(Self::DeepWater),
            10 => Some(Self::Ocean),
            11 => Some(Self::Mountain),
            12 => Some(Self::Moss),
            13 => Some(Self::Mud),
            14 => Some(Self::Podzol),
            _ => None,
        }
    }

    /// Водный ли тайл: любая из трёх глубин воды непроходима с суши.
    pub fn is_water(self) -> bool {
        matches!(self, TileType::Water | TileType::DeepWater | TileType::Ocean)
    }

    /// Число непустых видов тайлов (без `Empty`). Выводится из
    /// `from_discriminant`, поэтому новый вид не требует правки счётчика
    /// в рендере — только своего пути в `texture_path`.
    pub fn non_empty_count() -> usize {
        let mut count = 0;
        // Дискриминант непустых видов начинается с 1 и не выходит за u8.
        while count < u8::MAX as usize {
            if Self::from_discriminant((count + 1) as u8).is_none() {
                break;
            }
            count += 1;
        }
        count
    }
}

/// Вид ресурса, лежащего в тайле.
/// Варианты — стабильные идентификаторы (repr(u8)); новые добавляются
/// только в конец, чтобы не сдвигать значения существующих.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ResourceKind {
    None = 0,
    Copper,
    Clay,
    /// Дерево из тайла лесной травы; появится с добычей дерева.
    #[allow(dead_code)]
    Wood,
    /// Камень, откалываемый от валунов.
    Stone,
    /// Палка (из кустов, побочно из деревьев).
    Stick,
}

impl ResourceKind {
    /// Вид ресурса по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Copper),
            2 => Some(Self::Clay),
            3 => Some(Self::Wood),
            4 => Some(Self::Stone),
            5 => Some(Self::Stick),
            _ => None,
        }
    }
}

/// Тайл: вид + ресурс (вид + количество).
/// Плоский view для UI и тестов (ARCHITECTURE 2.3); горячие пути симуляции
/// работают со слоями чанка напрямую, поэтому в сборке без тестов view
/// временно без читателей.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub kind: TileType,
    pub resource: ResourceKind,
    pub amount: u16,
}

#[allow(dead_code)]
impl Tile {
    pub const EMPTY: Self = Self {
        kind: TileType::Empty,
        resource: ResourceKind::None,
        amount: 0,
    };

    /// Пустой тайл заданного вида.
    pub fn new(kind: TileType) -> Self {
        Self {
            kind,
            resource: ResourceKind::None,
            amount: 0,
        }
    }

    /// Тайл вида `kind` с ресурсом.
    pub fn with_resource(kind: TileType, resource: ResourceKind, amount: u16) -> Self {
        Self { kind, resource, amount }
    }
}
