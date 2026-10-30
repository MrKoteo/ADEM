//! Объекты над тайлами: дерево, куст, валун, яблоня и постройки (костёр).
//! Один объект на тайл; хранятся разреженно в слое чанка, а не как
//! Bevy-сущности — сущности рендер создаёт только для видимых объектов.

use super::items::ItemKind;
use super::tile::ResourceKind;

/// Вид объекта.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ObjectKind {
    Tree,
    Bush,
    Rock,
    AppleTree,
    /// Постройка: костёр (строится игроком, пока горит вечно).
    Campfire,
}

impl ObjectKind {
    /// Все виды объектов в стабильном порядке (генерация, UI редактора).
    /// Новые виды добавляются в конец — стабильные значения не сдвигаются.
    pub const ALL: [ObjectKind; 5] = [
        ObjectKind::Tree,
        ObjectKind::Bush,
        ObjectKind::Rock,
        ObjectKind::AppleTree,
        ObjectKind::Campfire,
    ];

    /// Виды-постройки: не генерируются, строятся через меню строительства
    /// (в редакторе — бесплатной установкой из категории Buildings).
    pub const BUILDINGS: [ObjectKind; 1] = [ObjectKind::Campfire];

    /// Является ли вид постройкой (у кнопок редактора, меню строительства).
    pub fn is_building(self) -> bool {
        Self::BUILDINGS.contains(&self)
    }

    /// Вид объекта по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Tree),
            1 => Some(Self::Bush),
            2 => Some(Self::Rock),
            3 => Some(Self::AppleTree),
            4 => Some(Self::Campfire),
            _ => None,
        }
    }
}

/// Стадия роста (у деревьев и кустов; валуны рождаются зрелыми).
/// Варианты Sapling/Young включит механика роста (ROADMAP, этап «Мир и активность»).
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ObjectStage {
    Sapling,
    Young,
    Mature,
}

impl ObjectStage {
    /// Стадия роста по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Sapling),
            1 => Some(Self::Young),
            2 => Some(Self::Mature),
            _ => None,
        }
    }
}

/// Объект на тайле: лёгкая копируемая запись без аллокаций.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorldObject {
    pub kind: ObjectKind,
    /// Стадия включится механикой роста объектов (ROADMAP, этап «Мир и активность»).
    #[allow(dead_code)]
    pub stage: ObjectStage,
    /// Семантика зависит от вида: запас камня у валуна, счётчик плодов
    /// у яблони, 0 у остальных.
    pub charge: u16,
}

impl WorldObject {
    /// Взрослое дерево — постоянный объект лесной травы.
    pub fn tree() -> Self {
        Self { kind: ObjectKind::Tree, stage: ObjectStage::Mature, charge: 0 }
    }

    /// Взрослый куст — источник палок, ломается при сборе.
    pub fn bush() -> Self {
        Self { kind: ObjectKind::Bush, stage: ObjectStage::Mature, charge: 0 }
    }

    /// Валун с запасом камня (GAME_DESIGN, приложение A: 5–15 единиц).
    pub fn rock(amount: u16) -> Self {
        Self { kind: ObjectKind::Rock, stage: ObjectStage::Mature, charge: amount }
    }

    /// Взрослая яблоня со стандартным запасом плодов.
    pub fn apple_tree() -> Self {
        Self::apple_tree_with_fruit(6)
    }

    /// Костёр — первая постройка. Пока горит вечно (без топлива).
    pub fn campfire() -> Self {
        Self { kind: ObjectKind::Campfire, stage: ObjectStage::Mature, charge: 0 }
    }

    /// Объект для постройки по виду. `None` — вид не является постройкой.
    pub fn building(kind: ObjectKind) -> Option<Self> {
        match kind {
            ObjectKind::Campfire => Some(Self::campfire()),
            _ => None,
        }
    }

    /// Взрослая яблоня с заданным числом плодов (при генерации — 4–10).
    pub fn apple_tree_with_fruit(fruit: u16) -> Self {
        Self {
            kind: ObjectKind::AppleTree,
            stage: ObjectStage::Mature,
            charge: fruit,
        }
    }

    /// Что даёт объект при добыче/сборе. None — руками не добывается.
    pub fn harvest_yield(&self) -> Option<(ResourceKind, u16)> {
        match self.kind {
            // Куст при сборе ломается и даёт две палки (решение автора).
            ObjectKind::Bush => Some((ResourceKind::Stick, 2)),
            // От валуна откалывают камень, пока есть запас.
            ObjectKind::Rock if self.charge > 0 => Some((ResourceKind::Stone, 1)),
            _ => None,
        }
    }

    /// Сколько яблок можно собрать с дерева. None — это не плодоносящая
    /// яблоня (пустая яблоня остаётся объектом, но больше не даёт плодов).
    pub fn apple_yield(&self) -> Option<u16> {
        (self.kind == ObjectKind::AppleTree && self.charge > 0).then_some(self.charge)
    }
}

/// Стоимость строительства постройки: предмет и количество. `None` — вид
/// не является постройкой. Цены построек собраны здесь, чтобы симуляция
/// (команда строительства) и UI (голограмма, меню) читали одни и те же
/// данные.
pub fn build_cost(kind: ObjectKind) -> Option<(ItemKind, u32)> {
    match kind {
        ObjectKind::Campfire => Some((ItemKind::Stick, 16)),
        _ => None,
    }
}
