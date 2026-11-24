//! Тесты поселений как отдельных сущностей и индекса объектов.

use crate::simulation::actions;
use crate::simulation::entities::EntityKind;
use crate::simulation::items::ItemKind;
use crate::simulation::objects::{BuildingKind, ObjectKind};
use crate::simulation::tile::TileType;
use crate::simulation::world::{TilePos, World, WorldPos, WorldSize};

fn campfire_setup() -> (World, u64, TilePos) {
    let mut world = World::new(WorldSize::new(128), 1);
    let pos = TilePos { x: 64, y: 64 };
    world.set_terrain(pos, TileType::Grass);
    let actor = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(pos));
    let mind = world
        .entities
        .get_mut(actor)
        .and_then(|e| e.mind.as_mut())
        .unwrap();
    assert_eq!(mind.inventory.add(ItemKind::Stick, 20), 0);
    (world, actor, pos)
}

/// Костёр — центр нового поселения; строитель сразу становится жителем.
#[test]
fn campfire_build_founds_settlement() {
    let (mut world, actor, pos) = campfire_setup();
    let fire = TilePos {
        x: pos.x + 1,
        y: pos.y,
    };
    assert!(actions::build(
        &mut world,
        actor,
        fire,
        BuildingKind::Campfire
    ));

    assert_eq!(world.settlements.len(), 1);
    let settlement = world.settlements.iter().next().unwrap();
    assert_eq!(settlement.center, fire);
    assert_eq!(settlement.members, vec![actor]);
    assert_eq!(settlement.storage, None);
    assert!(settlement.shared_knowledge.is_empty());
    // Постройка обязана попасть в очередь рендера: иначе ИИ-костёр не
    // появится на экране и не даст света.
    assert!(world.render_dirty.contains(&fire));

    // Индекс объектов видит постройку без скана чанков.
    assert_eq!(
        world.nearest_object(fire, 3, ObjectKind::Building(BuildingKind::Campfire), |_| true),
        Some(fire)
    );
}

/// Индекс обновляется при сносе/замене объекта.
#[test]
fn object_index_tracks_changes() {
    let mut world = World::new(WorldSize::new(128), 2);
    let pos = TilePos { x: 30, y: 30 };
    world.set_object(pos, Some(crate::simulation::WorldObject::bush()));
    assert_eq!(world.nearest_object(pos, 4, ObjectKind::Bush, |_| true), Some(pos));

    world.set_object(pos, None);
    assert_eq!(world.nearest_object(pos, 4, ObjectKind::Bush, |_| true), None);
    assert!(
        world.render_dirty.contains(&pos),
        "удаление куста ИИ должно обновить декор"
    );
}
