//! Тесты общей системы брони: защита предметов, рейтинг из надетых слотов,
//! линейное поглощение урона.

#[allow(unused_imports)]
use crate::simulation::armor::{protection_of, rating, reduce_damage};
use crate::simulation::items::{Inventory, ItemKind, ItemStack, SlotArea, SlotRef};

// Кольчуга даёт 3/4/3/2 поинта (шлем/кираса/поножи/ботинки), остальные
// предметы защиты не имеют.


/// Рейтинг собирается только из надетых слотов брони: предмет в основном
/// слоте или мусорке не считается надетым, пустой инвентарь даёт 0.0.
#[test]
fn armor_rating_counts_equipped_slots_only() {
    let mut inv = Inventory::default();
    assert_eq!(rating(&inv), 0.0, "без брони рейтинг нулевой");

    // Кольчуга в основных слотах не надета.
    inv.add(ItemKind::ChestplateChainmail, 1);
    assert_eq!(rating(&inv), 0.0);
    inv.trash = Some(ItemStack { kind: ItemKind::HelmetChainmail, count: 1 });
    assert_eq!(rating(&inv), 0.0);

    // Надеваем по частям: рейтинг растёт как сумма защиты.
    let armor = |index| SlotRef { area: SlotArea::Armor, index };
    inv.transfer(SlotRef { area: SlotArea::Misc, index: 0 }, armor(1));
    assert_eq!(rating(&inv), 4.0, "кираса");
    inv.put(armor(0), ItemStack { kind: ItemKind::HelmetChainmail, count: 1 });
    inv.put(armor(2), ItemStack { kind: ItemKind::LeggingsChainmail, count: 1 });
    inv.put(armor(3), ItemStack { kind: ItemKind::BootsChainmail, count: 1 });
    assert_eq!(rating(&inv), 12.0, "полный комплект");
}

/// Линейное поглощение: сколько поинтов рейтинга (из 100) — столько
/// процентов урона поглощается; нулевой и полный рейтинги — граничные.
#[test]
fn armor_reduces_damage_linearly() {
    assert_eq!(reduce_damage(0.0, 10.0), 10.0, "без брони урон проходит целиком");
    assert_eq!(reduce_damage(100.0, 10.0), 0.0, "полный рейтинг гасит всё");
    assert!((reduce_damage(25.0, 10.0) - 7.5).abs() < 1e-4, "25% поглощено");
    assert!((reduce_damage(12.0, 10.0) - 8.8).abs() < 1e-4, "полный комплект кольчуги");

    // Рейтинг вне диапазона зажимается.
    assert_eq!(reduce_damage(-5.0, 10.0), 10.0);
    assert_eq!(reduce_damage(150.0, 10.0), 0.0);
    // Отрицательный урон не превращается в лечение.
    assert!(reduce_damage(50.0, -3.0) <= 0.0);
}
