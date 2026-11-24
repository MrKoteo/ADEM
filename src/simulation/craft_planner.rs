//! Планировщик ручного крафта.
//!
//! ИИ умеет не только нажать «создать», но и добыть/сделать недостающие
//! промежуточные предметы: палки, волокно, верёвку. Логика вынесена из
//! `ai.rs` в отдельный сервис, чтобы её могли переиспользовать другие
//! системы (например, будущие поселения или планировщик задач).
//!
//! Планировщик работает только с ручными рецептами: он не телепортирует
//! человека к верстаку и не трогает мир. Всё состояние — книга рецептов,
//! каталог знаний и инвентарь.

use super::crafts::{CraftBook, CraftRecipe, CrafterKind};
use super::items::{Inventory, ItemKind};
use super::knowledge::{KnowledgeCatalog, KnowledgeSet};

/// Максимальная глубина вложенности промежуточных крафтов.
pub const MAX_CRAFT_DEPTH: u32 = 6;

/// Первый ручной рецепт предмета, который человек умеет делать.
pub(crate) fn find_manual_recipe<'a>(
    book: &'a CraftBook,
    catalog: &KnowledgeCatalog,
    known: &KnowledgeSet,
    item: ItemKind,
) -> Option<&'a CraftRecipe> {
    book.recipes_for(CrafterKind::Manual).find(|recipe| {
        recipe.result.kind == item
            && recipe.tools.is_empty()
            && recipe.knowledge_satisfied(known, catalog)
    })
}

/// Рекурсивно крафтит предмет из инвентаря, сначала создавая недостающие
/// промежуточные предметы. Возвращает `true`, если рецепт выполнен.
pub(crate) fn plan_craft(
    book: &CraftBook,
    catalog: &KnowledgeCatalog,
    known: &KnowledgeSet,
    inventory: &mut Inventory,
    recipe: &CraftRecipe,
    depth: u32,
) -> bool {
    if depth > MAX_CRAFT_DEPTH || !recipe.knowledge_satisfied(known, catalog) {
        return false;
    }
    for &(kind, count) in recipe.cost() {
        let mut have = inventory.count_of(kind);
        while have < count {
            let Some(sub) = find_manual_recipe(book, catalog, known, kind) else {
                return false;
            };
            if !plan_craft(book, catalog, known, inventory, sub, depth + 1) {
                return false;
            }
            let now = inventory.count_of(kind);
            if now <= have {
                return false;
            }
            have = now;
        }
    }
    recipe.try_craft_from_inventory(inventory)
}

/// Первый базовый ресурс, которого не хватает для рецепта. Нужен, чтобы
/// отправить человека к кусту/камню/траве вместо бесконечных попыток.
pub(crate) fn missing_material(
    book: &CraftBook,
    catalog: &KnowledgeCatalog,
    known: &KnowledgeSet,
    inventory: &Inventory,
    recipe: &CraftRecipe,
    depth: u32,
) -> Option<ItemKind> {
    if depth > MAX_CRAFT_DEPTH {
        return None;
    }
    for &(kind, count) in recipe.cost() {
        if inventory.count_of(kind) >= count {
            continue;
        }
        if let Some(sub) = find_manual_recipe(book, catalog, known, kind)
            && let Some(base) = missing_material(book, catalog, known, inventory, sub, depth + 1)
        {
            return Some(base);
        }
        return Some(kind);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::crafts::CraftBook;
    use crate::simulation::items::Inventory;
    use crate::simulation::knowledge::KnowledgeCatalog;

    #[test]
    fn plans_missing_intermediate_items() {
        let loaded = CraftBook::load_dir("data/crafts");
        assert!(loaded.errors.is_empty());
        let catalog = KnowledgeCatalog::load_default();
        let mut known = KnowledgeSet::new();
        if let Some(index) = catalog.index_of("stone_tools") {
            known.insert(index);
        }
        let recipe = find_manual_recipe(
            &loaded.book,
            &catalog,
            &known,
            ItemKind::AxePrimal,
        )
        .expect("каменный топор должен быть ручным рецептом");
        let mut inventory = Inventory::default();
        // Ровно базовые материалы для топора: камень, палки, трава на
        // верёвку (3 волокна = 1 верёвка).
        assert!(inventory.add(ItemKind::Rock, 3) == 0);
        assert!(inventory.add(ItemKind::Stick, 2) == 0);
        assert!(inventory.add(ItemKind::PlantFiber, 3) == 0);
        assert!(plan_craft(
            &loaded.book,
            &catalog,
            &known,
            &mut inventory,
            recipe,
            0
        ));
        assert_eq!(inventory.count_of(ItemKind::AxePrimal), 1);
    }
}
