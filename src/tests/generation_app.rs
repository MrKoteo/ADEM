//! Тесты фоновой генерации мира на app-слое (без графики).

use crate::app::generation::{WorldGeneration, request_world};
use crate::app::state::GameState;
use crate::simulation::{World, WorldSize};

/// Запрос нового мира сразу выгружает предыдущий: две карты в ОЗУ не
/// держим, а захват существа ссылается на id прежнего мира.
#[test]
fn new_world_request_unloads_previous_world() {
    let mut state = GameState::default();
    state.world_size = WorldSize::new(256);
    state.seed = 1;
    state.world = Some(World::new(WorldSize::new(256), 1));
    state.set_controlled(Some(7));
    let mut generation = WorldGeneration::default();

    request_world(&mut state, &mut generation, WorldSize::new(512), 2, 0);

    assert!(state.world.is_none(), "старый мир должен быть выгружен");
    assert!(state.generating, "идёт генерация нового мира");
    assert_eq!(state.controlled, None, "захват прежнего мира снят");
    assert_eq!(generation.progress, 0.0);
    assert!(generation.requested.is_some());

    // Отменяем поток, чтобы тест не ждал полную генерацию карты.
    generation.cancel();
    assert!(!state.generating || state.world.is_none());
}

/// Повторный запрос с параметрами уже применённого мира не перезапускает
/// генерацию и не трогает мир.
#[test]
fn same_params_do_not_restart_generation() {
    let mut state = GameState::default();
    state.world_size = WorldSize::new(256);
    state.seed = 5;
    state.world = Some(World::new(WorldSize::new(256), 5));
    let mut generation = WorldGeneration::default();

    request_world(&mut state, &mut generation, WorldSize::new(256), 5, 0);

    assert!(state.world.is_some(), "мир остался на месте");
    assert!(!state.generating);
    assert!(generation.requested.is_none());
}
