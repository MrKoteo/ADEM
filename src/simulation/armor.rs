//! Броня как общая система существа (GAME_DESIGN 3.5, 3.10).
//!
//! Рейтинг брони — сумма защиты надетых предметов (только слоты брони:
//! предмет в основном слоте или мусорке не считается надетым). Поглощение
//! урона пока линейное: сколько поинтов рейтинга — столько процентов урона
//! поглощается. Ни на что не влияет, пока урон игроку наносит только
//! голод, — основа под боевую систему и поведение существ.

use super::items::{Inventory, ItemKind};

/// Максимум шкалы брони (индикатор UI), поинтов.
pub const ARMOR_RATING_MAX: f32 = 100.0;

/// Защита предмета, поинтов рейтинга. Не-броня и будущие виды предметов
/// без защиты дают 0.0 — новые предметы не должны требовать правки здесь.
pub fn protection_of(kind: ItemKind) -> f32 {
    match kind {
        ItemKind::HelmetChainmail => 3.0,
        ItemKind::ChestplateChainmail => 4.0,
        ItemKind::LeggingsChainmail => 3.0,
        ItemKind::BootsChainmail => 2.0,
        _ => 0.0,
    }
}

/// Суммарный рейтинг брони сущности по надетым предметам, поинтов
/// (0.0–`ARMOR_RATING_MAX`). Без брони — 0.0.
pub fn rating(inventory: &Inventory) -> f32 {
    let mut total = 0.0;
    for stack in inventory.armor.iter().flatten() {
        total += protection_of(stack.kind);
    }
    total.clamp(0.0, ARMOR_RATING_MAX)
}

/// Линейное поглощение урона: из `damage` вычитается доля, равная доле
/// рейтинга от максимума. Рейтинг зажимается в допустимый диапазон.
#[allow(unused)]
pub fn reduce_damage(rating: f32, damage: f32) -> f32 {
    damage * (1.0 - rating.clamp(0.0, ARMOR_RATING_MAX) / ARMOR_RATING_MAX)
}
