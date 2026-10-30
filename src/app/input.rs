//! Ввод режима игрока и редактора: переключение режимов, движение WASD,
//! клики ЛКМ по карте (инструменты «бога» в наблюдателе, добыча — в игре).
//!
//! UI не меняет мир напрямую: все действия уходят командами в очередь
//! симуляции (`simulation/commands.rs`).

use bevy::camera::{Camera2d, Projection};
use bevy::input::mouse::{MouseButton, MouseMotion};
use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::ui_widgets::TextInput;

use crate::app::settings::{ControlAction, Settings};
use crate::app::state::{
    ClockRes, CommandQueueRes, GameMode, GameState, MenuKind, PlayerRes, Tool, ToolCategory,
    WindowFocusState,
};
use crate::simulation::commands::Command;
use crate::simulation::items::ItemKind;
use crate::simulation::objects::{ObjectKind, WorldObject};
use crate::simulation::player::{Player, WorldPos};
use crate::simulation::{TILE_SIZE, TilePos, World};
use crate::ui::bottom_panel::{PanelMenu, PanelMenuState};
use crate::ui::pause_menu::BindingCapture;
use crate::ui::{BlocksWorldInput, cursor_over_ui, text_input_focused};

/// Сколько секунд ЛКМ нужно удержать с копьём, чтобы отпускание бросило
/// его, а не укололо. Меньше удержания — удар в ближнем бою.
const SPEAR_THROW_HOLD_SECS: f32 = 0.5;

/// Вид визуального эффекта оружия. Бой и кулдауны считаются в симуляции,
/// сообщение — только сигнал о визуальном событии для рендера.
#[derive(Clone, Copy, PartialEq, Eq, Message)]
pub enum WeaponFxKind {
    /// Дуга взмаха лезвия (нож, в будущем меч).
    Blade,
    /// Выпад копья: текстура от центра игрока вперёд и назад.
    Thrust,
    /// Выстрел из лука: текстура лука в центре игрока, повёрнутая к выстрелу.
    Bow,
}

/// Событие «игрок применил оружие» для рендера эффектов.
#[derive(Message)]
pub struct WeaponFx {
    pub kind: WeaponFxKind,
    /// Точка, откуда эффект (позиция игрока в мировых пикселях).
    pub origin: Vec2,
    /// Точка, куда направлено (прицел в мировых пикселях).
    pub aim: Vec2,
}

/// Направление движения от биндов WASD (ось X вправо, ось Y вверх).
/// Ноль — стоит на месте; нормализация и скорость — в симуляции.
/// Читается тиковой системой движения, поэтому направление — состояние,
/// а не поток команд.
#[derive(Resource, Default, Clone, Copy)]
pub struct PlayerInput {
    pub dir: (f32, f32),
}

/// Читает нажатия биндов движения (по умолчанию WASD). Возвращает
/// направление (−1/0/1 по осям) без нормализации. Общая для движения
/// игрока (input.rs) и камеры редактора (camera.rs).
pub(crate) fn move_dir(keys: &ButtonInput<KeyCode>, settings: &Settings) -> (f32, f32) {
    let mut x = 0.0;
    let mut y = 0.0;
    if keys.pressed(settings.controls.key(ControlAction::MoveUp)) {
        y += 1.0;
    }
    if keys.pressed(settings.controls.key(ControlAction::MoveDown)) {
        y -= 1.0;
    }
    if keys.pressed(settings.controls.key(ControlAction::MoveRight)) {
        x += 1.0;
    }
    if keys.pressed(settings.controls.key(ControlAction::MoveLeft)) {
        x -= 1.0;
    }
    (x, y)
}

/// Минимальный интервал повторной добычи при зажатой ЛКМ, секунды реального
/// времени (не игрового: ускорение мира не должно превращать удержание
/// в очередь команд).
const INTERACT_REPEAT_SECS: f32 = 0.5;

/// Состояние удержания ЛКМ в режиме игрока: время до следующей добычи.
#[derive(Default)]
pub(crate) struct HarvestHold {
    cooldown: f32,
}

pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerInput>()
            .add_message::<WeaponFx>()
            .add_systems(Update, (toggle_player_mode, player_move_input, world_click_input, player_drop_input, player_pickup_input, editor_delete_input, editor_entity_drag));
    }
}

/// F1 — переключение режима наблюдатель ↔ игрок. Системная клавиша (как
/// F11), пока не перебиндивается.
///
/// Из наблюдателя игрок размещается на тайле под курсором; из игрока —
/// «паркуется»: данные и инвентарь остаются в `PlayerRes` и переживут
/// следующий вход в режим.
fn toggle_player_mode(
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    mut state: ResMut<GameState>,
    mut player: ResMut<PlayerRes>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
    capture: Res<BindingCapture>,
    focus: Res<InputFocus>,
    text_inputs: Query<(), With<TextInput>>,
) {
    if !keys.just_pressed(KeyCode::F1) {
        return;
    }
    // Пока фокус в текстовом поле, идёт захват бинда или открыт пауз-меню,
    // смена режима отключена.
    if capture.0.is_some() || text_input_focused(&focus, &text_inputs) {
        return;
    }
    if state.active_menu == Some(MenuKind::Pause) {
        return;
    }

    match state.mode {
        GameMode::Player => state.mode = GameMode::Observer,
        GameMode::Observer => {
            let Ok(window) = windows.single() else {
                return;
            };
            if cursor_over_ui(window, &blockers) {
                return;
            }
            let Some(tile) = cursor_world_tile(window, &cameras, state.world.as_ref()) else {
                return;
            };
            let pos = WorldPos::of_tile_center(tile);
            match player.0.as_mut() {
                // Существо уже было: переносим, инвентарь сохраняется.
                Some(p) => p.teleport_to(pos),
                None => player.0 = Some(Player::new(pos)),
            }
            state.mode = GameMode::Player;
        }
    }
}

/// Бинды движения — направление игрока. Пока фокус в текстовом поле
/// или открыто модальное меню (пауза, деревья, творческое), движение
/// замирает: набор цифр и навигация в меню не должны двигать существо.
fn player_move_input(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    state: Res<GameState>,
    mut input: ResMut<PlayerInput>,
    focus: Res<InputFocus>,
    text_inputs: Query<(), With<TextInput>>,
) {
    let dir = if text_input_focused(&focus, &text_inputs) || state.active_menu.is_some() {
        (0.0, 0.0)
    } else {
        move_dir(&keys, &settings)
    };
    input.dir = dir;
}

/// ЛКМ по карте.
///
/// Наблюдатель с выбранным инструментом — размещение: по нажатию и затем
/// при каждой смене тайла под курсором, пока ЛКМ зажата (спам по одному
/// месту без движения мыши исключён). Режим игрока — зависит от предмета
/// в руке: лук стреляет по нажатию (добычи нет), копьё на коротком
/// нажатии бьёт, а на отпускании после удержания бросается в тайл под
/// курсором; остальное — удар/добыча: сразу по нажатию, затем повтор
/// каждые `INTERACT_REPEAT_SECS`. Бой и кулдауны считаются в симуляции;
/// след взмаха — косметика, сообщение уходит и на промахе. Публичная для
/// порядка `apply_commands` в главном плагине.
pub(crate) fn world_click_input(
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window>,
    state: Res<GameState>,
    player: Res<PlayerRes>,
    clock: Res<ClockRes>,
    mut queue: ResMut<CommandQueueRes>,
    mut fx: MessageWriter<WeaponFx>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
    mut drag: Local<Option<TilePos>>,
    mut hold: Local<HarvestHold>,
    mut spear_hold: Local<Option<f32>>,
    // Тик, с которого снова можно показывать анимацию выпада (в игровых
    // тиках: на паузе лимитер замирает вместе с миром).
    mut thrust_fx_ready_tick: Local<u64>,
) {
    // Наблюдатель: инструменты «бога».
    if state.mode == GameMode::Observer {
        *hold = HarvestHold::default();
        if !mouse.pressed(MouseButton::Left) {
            *drag = None;
            return;
        }
        let Ok(window) = windows.single() else {
            return;
        };
        let Some(world) = state.world.as_ref() else {
            return;
        };
        if cursor_over_ui(window, &blockers) {
            *drag = None;
            return;
        }
        let Some(tool) = state.selected_tool else {
            *drag = None;
            return;
        };
        let Some(tile) = cursor_world_tile(window, &cameras, Some(world)) else {
            return;
        };
        // Ставим по нажатию и при смене тайла под курсором.
        if *drag == Some(tile) {
            return;
        }
        let command = match tool {
            Tool::Tile(kind) => Command::SetTerrain { pos: tile, kind },
            Tool::Object(kind) => Command::SetObject {
                pos: tile,
                object: Some(placed_object(kind)),
            },
            Tool::Entity(kind) => Command::SpawnEntity { kind, pos: tile },
        };
        queue.0.push(command);
        *drag = Some(tile);
        return;
    }

    // Режим игрока: на паузе мир не тикает — и действие невозможно, как и
    // всё, что является частью симуляции. Таймеры удержания сбрасываются:
    // после снятия паузы зажатая ЛКМ начинает цикл заново, а не даёт
    // внеочередную добычу по устаревшему кулдауну.
    if state.paused {
        *spear_hold = None;
        *hold = HarvestHold::default();
        return;
    }
    let Some(world) = state.world.as_ref() else {
        *hold = HarvestHold::default();
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    if cursor_over_ui(window, &blockers) {
        *spear_hold = None;
        *drag = None;
        return;
    }
    let Some(p) = player.0.as_ref() else {
        return;
    };
    let Some(tile) = cursor_world_tile(window, &cameras, Some(world)) else {
        return;
    };
    let aim = cursor_world_pos(window, &cameras).unwrap_or_default();
    let origin = Vec2::new(p.pos.x * TILE_SIZE, p.pos.y * TILE_SIZE);
    let held = p.selected_item();

    // Лук: стрельба по нажатию. Добычи и ударов нет — луком нельзя ломать.
    // Эффект показывается только при реальном выстреле: на кулдауне
    // симуляция команду отклонит, и текстура лука не должна появляться.
    if held == Some(ItemKind::Bow) {
        if mouse.just_pressed(MouseButton::Left) && clock.0.tick >= p.attack_ready_tick {
            queue.0.push(Command::PlayerShootBow { target: tile });
            fx.write(WeaponFx { kind: WeaponFxKind::Bow, origin, aim });
        }
        return;
    }

    // Копьё: короткое нажатие — укол (мимо существ — обычная добыча),
    // удержание дольше `SPEAR_THROW_HOLD_SECS` — бросок на отпускание.
    if held == Some(ItemKind::Spear) {
        if mouse.just_pressed(MouseButton::Left) {
            *spear_hold = Some(0.0);
        }
        if let Some(secs) = spear_hold.as_mut()
            && mouse.pressed(MouseButton::Left)
        {
            *secs += time.delta_secs();
        }
        if mouse.just_released(MouseButton::Left)
            && let Some(secs) = spear_hold.take()
        {
            if secs < SPEAR_THROW_HOLD_SECS {
                // Укол: анимация выпада только когда кулдаун прошлой атаки
                // вышел (иначе симуляция удар отклонит, а текстура была бы
                // видна). На промахе кулдаун не тратится — выпад виден и там.
                // Свой ограничитель анимации: симуляция списывает кулдаун
                // только при попадании, поэтому без него заклик мимо играл
                // бы выпад на каждое нажатие. Интервал — кулдаун копья.
                let spear_cooldown = ItemKind::Spear
                    .weapon_stats()
                    .map(|stats| stats.cooldown_ticks)
                    .unwrap_or(10);
                if clock.0.tick >= p.attack_ready_tick && clock.0.tick >= *thrust_fx_ready_tick {
                    fx.write(WeaponFx { kind: WeaponFxKind::Thrust, origin, aim });
                    *thrust_fx_ready_tick = clock.0.tick + spear_cooldown;
                }
                queue.0.push(Command::PlayerInteract { pos: tile });
            } else {
                queue.0.push(Command::PlayerThrowSpear { target: tile });
            }
        }
        if !mouse.pressed(MouseButton::Left) {
            *spear_hold = None;
        }
        return;
    }

    // Остальные предметы и рука: удар/добыча. Первое действие выполняется
    // сразу, последующие — строго с интервалом по реальному времени, даже
    // если курсор перемещается между тайлами.
    if !mouse.pressed(MouseButton::Left) {
        *hold = HarvestHold::default();
        return;
    }
    if mouse.just_pressed(MouseButton::Left) {
        // След взмаха — только у лезвий (нож; меч появится позже): рука
        // и инструменты без анимации, бой при этом считается всегда.
        // Анимация только когда кулдаун прошлой атаки вышел — как у лука
        // и копья, чтобы визуал не убегал вперёд реального урона.
        if held == Some(ItemKind::Knife) && clock.0.tick >= p.attack_ready_tick {
            fx.write(WeaponFx { kind: WeaponFxKind::Blade, origin, aim });
        }
        queue.0.push(Command::PlayerInteract { pos: tile });
        hold.cooldown = INTERACT_REPEAT_SECS;
        return;
    }

    hold.cooldown -= time.delta_secs();
    if hold.cooldown <= 0.0 {
        queue.0.push(Command::PlayerInteract { pos: tile });
        hold.cooldown =
            INTERACT_REPEAT_SECS - (-hold.cooldown).rem_euclid(INTERACT_REPEAT_SECS);
    }
}

/// Бинд «Положить предмет» (по умолчанию V) — положить предмет из руки на
/// тайл под курсором. Тайл должен быть в радиусе взаимодействия игрока
/// (как для добычи); финальную проверку радиуса всё равно делает симуляция
/// при применении команды.
fn player_drop_input(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    windows: Query<&Window>,
    state: Res<GameState>,
    mut queue: ResMut<CommandQueueRes>,
    player: Res<PlayerRes>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
    focus: Res<InputFocus>,
    text_inputs: Query<(), With<TextInput>>,
    capture: Res<BindingCapture>,
) {
    if state.mode != GameMode::Player || state.paused {
        return;
    }
    if capture.0.is_some() || text_input_focused(&focus, &text_inputs) {
        return;
    }
    if !keys.just_pressed(settings.controls.key(ControlAction::DropItem)) {
        return;
    }
    let Some(p) = player.0.as_ref() else {
        return;
    };
    let Some(world) = state.world.as_ref() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    if cursor_over_ui(window, &blockers) {
        return;
    }
    let Some(tile) = cursor_world_tile(window, &cameras, Some(world)) else {
        return;
    };
    if !p.reachable(tile, world) {
        return;
    }
    queue.0.push(Command::PlayerDropItem { pos: tile });
}

/// ПКМ по лежащей стопке предметов — подобрать её; по яблоне с плодами —
/// собрать плоды. Зажатие действует как ЛКМ-добыча: первое действие по
/// нажатию, затем повтор каждые `INTERACT_REPEAT_SECS` по тайлу под
/// курсором. Выбранная постройка в меню строительства перехватывает ПКМ:
/// клик строит, а не подбирает. Яблоко в выбранном слоте хотбара
/// перехватывает ПКМ раньше всего: клик ест его (побочный эффект — с
/// яблоком в руке нельзя подобрать предмет или собрать плоды; еда не
/// повторяется зажатием — у неё свой кулдаун в симуляции).
fn player_pickup_input(
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    windows: Query<&Window>,
    state: Res<GameState>,
    construction: Res<crate::ui::construction::ConstructionUi>,
    mut queue: ResMut<CommandQueueRes>,
    player: Res<PlayerRes>,
    mut hold: Local<HarvestHold>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
) {
    if state.mode != GameMode::Player || state.paused {
        *hold = HarvestHold::default();
        return;
    }
    if crate::ui::construction::ConstructionUi::building_selected(&construction) {
        *hold = HarvestHold::default();
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    if cursor_over_ui(window, &blockers) {
        *hold = HarvestHold::default();
        return;
    }
    // Еда в руке — ПКМ ест по нажатию (зажатие без повторов: кулдаун
    // поедания всё равно отбрасывал бы команды).
    if player.0.as_ref().is_some_and(|p| p.selected_item() == Some(ItemKind::Apple)) {
        if mouse.just_pressed(MouseButton::Right) {
            queue.0.push(Command::PlayerEat);
        }
        *hold = HarvestHold::default();
        return;
    }

    // Зажатие ПКМ — как ЛКМ-добыча: действие по нажатию и повтором.
    if !mouse.pressed(MouseButton::Right) {
        *hold = HarvestHold::default();
        return;
    }
    if mouse.just_pressed(MouseButton::Right) {
        rmb_action(&mut queue, &state, &windows, &cameras);
        hold.cooldown = INTERACT_REPEAT_SECS;
        return;
    }
    hold.cooldown -= time.delta_secs();
    if hold.cooldown <= 0.0 {
        rmb_action(&mut queue, &state, &windows, &cameras);
        hold.cooldown =
            INTERACT_REPEAT_SECS - (-hold.cooldown).rem_euclid(INTERACT_REPEAT_SECS);
    }
}

/// Одно действие ПКМ в режиме игрока: подобрать стопку предметов с тайла
/// под курсором или собрать плоды с яблони. Тайл читается заново на
/// каждый повтор — зажатие собирает и с соседних яблонь при проведении.
fn rmb_action(
    queue: &mut CommandQueueRes,
    state: &GameState,
    windows: &Query<&Window>,
    cameras: &Query<(&Transform, &Projection), With<Camera2d>>,
) {
    let Some(world) = state.world.as_ref() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    if let Some(tile) = cursor_world_tile(window, cameras, Some(world)) {
        if world.item_at(tile).is_some() {
            queue.0.push(Command::PlayerPickupItem { pos: tile });
        } else if world.object_at(tile).is_some_and(|obj| obj.apple_yield().is_some()) {
            // ПКМ по яблоне — собрать плоды (ЛКМ по ней ничего не делает).
            queue.0.push(Command::PlayerHarvestApple { pos: tile });
        }
    }
}

/// Объект, ставимый редактором по виду. У валуна — средний запас из
/// рабочего диапазона генерации (5–15 единиц, GAME_DESIGN приложение A).
fn placed_object(kind: ObjectKind) -> WorldObject {
    match kind {
        ObjectKind::Tree => WorldObject::tree(),
        ObjectKind::Bush => WorldObject::bush(),
        ObjectKind::Rock => WorldObject::rock(10),
        ObjectKind::AppleTree => WorldObject::apple_tree(),
        ObjectKind::Campfire => WorldObject::campfire(),
    }
}

/// Что удаляет ПКМ в редакторе. Класс цели задаётся открытым подменю
/// нижней панели: пока подменю категории не открыто, цель не выбрана.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteClass {
    Objects,
    Entities,
}

/// ПКМ в режиме наблюдателя — удалить объект из открытой категории.
///
/// Удаление срабатывает по отпусканию без движения (порог в несколько
/// пикселей): зажатая ПКМ с движением — это панорамирование камеры, и
/// удалять то, что протащили под курсором, нельзя.
pub(crate) fn editor_delete_input(
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    windows: Query<&Window>,
    state: Res<GameState>,
    menu: Res<PanelMenuState>,
    mut queue: ResMut<CommandQueueRes>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
    mut drag_distance: Local<Option<f32>>,
) {
    if state.mode != GameMode::Observer {
        *drag_distance = None;
        return;
    }

    // Класс цели — только из открытого подменю инструментов. Базовые кнопки,
    // скорость и категория тайлов цель для удаления не задают: ПКМ по карте
    // в этих состояниях ничего не трогает.
    let target_class = match menu.open {
        Some(PanelMenu::Tools(ToolCategory::Objects)) => Some(DeleteClass::Objects),
        Some(PanelMenu::Tools(ToolCategory::Buildings)) => Some(DeleteClass::Objects),
        Some(PanelMenu::Tools(ToolCategory::Entities)) => Some(DeleteClass::Entities),
        _ => None,
    };

    // Дрожание руки при клике не должно отменять удаление: порог в
    // несколько пикселей отделяет клик от осознанного панорамирования.
    const CLICK_DRAG_THRESHOLD: f32 = 4.0;
    let motion_delta: f32 = motion.read().map(|event| event.delta.length()).sum();
    if mouse.just_pressed(MouseButton::Right) {
        *drag_distance = Some(motion_delta);
    } else if let Some(distance) = drag_distance.as_mut() {
        *distance += motion_delta;
    }

    // Отпускание — единственный момент удаления: так быстрый клик
    // (нажатие и отпускание в одном кадре) тоже обрабатывается.
    if !mouse.just_released(MouseButton::Right) {
        return;
    }
    let Some(distance) = drag_distance.take() else {
        return;
    };
    if distance > CLICK_DRAG_THRESHOLD {
        return;
    }
    let Some(target_class) = target_class else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    if cursor_over_ui(window, &blockers) {
        return;
    }
    let Some(point) = cursor_world_pos(window, &cameras) else {
        return;
    };
    let Some(world) = state.world.as_ref() else {
        return;
    };
    let Some(tile) = world_tile_at(world, point) else {
        return;
    };
    match target_class {
        DeleteClass::Objects => {
            if world.object_at(tile).is_some() {
                queue.0.push(Command::SetObject { pos: tile, object: None });
            }
        }
        DeleteClass::Entities => {
            if let Some(id) = entity_at(world, point) {
                queue.0.push(Command::RemoveEntity { id });
            }
        }
    }
}

/// Перетаскивание существ колёсиком мыши в редакторе: нажатие на существо
/// «берёт» его, пока кнопка зажата — существо следует за курсором, отпускание
/// освобождает. Позиция непрерывная (между тайлами), но зажимается в границы
/// мира. Каждый кадр уходит отдельная команда `MoveEntity`: UI не меняет мир
/// напрямую, как и остальные действия редактора.
pub(crate) fn editor_entity_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    focus: Res<WindowFocusState>,
    state: Res<GameState>,
    mut queue: ResMut<CommandQueueRes>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    blockers: Query<(&ComputedNode, &UiGlobalTransform, &Visibility), With<BlocksWorldInput>>,
    mut dragged: Local<Option<u64>>,
) {
    // Потеря фокуса окна: отпускание колёсика могло не дойти до игры
    // (например, кнопку отпустили в другом окне), поэтому перетаскивание
    // завершаем сразу, не дожидаясь следующего нажатия.
    if state.mode != GameMode::Observer || !focus.focused {
        *dragged = None;
        return;
    }

    // Нажатие на существо начинает перетаскивание. Клик мимо — ничего.
    if mouse.just_pressed(MouseButton::Middle) {
        *dragged = None;
        let Ok(window) = windows.single() else {
            return;
        };
        if cursor_over_ui(window, &blockers) {
            return;
        }
        let Some(world) = state.world.as_ref() else {
            return;
        };
        let Some(point) = cursor_world_pos(window, &cameras) else {
            return;
        };
        *dragged = entity_at(world, point);
    }

    let Some(id) = *dragged else {
        return;
    };
    let releasing = mouse.just_released(MouseButton::Middle);
    if !mouse.pressed(MouseButton::Middle) && !releasing {
        *dragged = None;
        return;
    }

    let Ok(window) = windows.single() else {
        *dragged = None;
        return;
    };
    // Над панелями UI существо не двигаем, но отпускание кнопки всё равно
    // завершает перетаскивание.
    if !cursor_over_ui(window, &blockers)
        && let Some(world) = state.world.as_ref()
        && let Some(point) = cursor_world_pos(window, &cameras)
    {
        // Курсор в пикселях мира, позиция существа — в тайлах. Край мира
        // зажимаем чуть внутрь, чтобы существо не вышло за границу.
        let edge = (world.size.tiles() as f32 - 0.001).max(0.0);
        let pos = WorldPos {
            x: (point.x / TILE_SIZE).clamp(0.0, edge),
            y: (point.y / TILE_SIZE).clamp(0.0, edge),
        };
        queue.0.push(Command::MoveEntity { id, pos });
    }

    if releasing {
        *dragged = None;
    }
}

/// Существо, по которому кликнули. Спрайт существа вдвое больше тайла,
/// поэтому цель выбирается не по тайлу центра, а по расстоянию до точки
/// клика: ближайшее существо, чей спрайт накрывает эту точку (с допуском
/// на диагональ спрайта). На одном месте может оказаться несколько
/// существ — удаляется то, по которому кликнули точнее.
fn entity_at(world: &World, point: Vec2) -> Option<u64> {
    // Половина диагонали спрайта 2×2 тайла: (TILE_SIZE·√2) ≈ 1.41 тайла.
    const PICK_RADIUS: f32 = TILE_SIZE * 1.45;
    let radius2 = PICK_RADIUS * PICK_RADIUS;
    let distance2 = |pos: WorldPos| {
        let dx = pos.x * TILE_SIZE - point.x;
        let dy = pos.y * TILE_SIZE - point.y;
        dx * dx + dy * dy
    };
    world
        .entities
        .iter()
        .map(|entity| (entity.id, distance2(entity.pos)))
        .filter(|(_, distance)| *distance <= radius2)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// Мировая позиция курсора в пикселях. `None` — курсор вне окна или
/// 2D-камера ещё не готова. Публичная: используется рендером (голограмма
/// постройки) и меню строительства.
pub(crate) fn cursor_world_pos(
    window: &Window,
    cameras: &Query<(&Transform, &Projection), With<Camera2d>>,
) -> Option<Vec2> {
    let cursor = window.cursor_position()?;
    let Ok((transform, projection)) = cameras.single() else {
        return None;
    };
    let Projection::Orthographic(ortho) = projection else {
        return None;
    };

    // Курсор — в логических пикселях окна; смещение от центра экрана
    // умножается на масштаб ортокамеры (как в панорамировании camera.rs).
    // Ось Y курсора растёт вниз, мировая — вверх.
    let half = Vec2::new(window.width(), window.height()) * 0.5;
    let offset = Vec2::new(cursor.x - half.x, half.y - cursor.y) * ortho.scale;
    Some(transform.translation.xy() + offset)
}

/// Тайл мира по мировой позиции. `None` — за границами карты (камера
/// может смотреть и мимо мира).
fn world_tile_at(world: &World, point: Vec2) -> Option<TilePos> {
    let size = world.size.tiles() as i32;
    let tx = (point.x / TILE_SIZE).floor() as i32;
    let ty = (point.y / TILE_SIZE).floor() as i32;
    (tx >= 0 && ty >= 0 && tx < size && ty < size).then_some(TilePos { x: tx, y: ty })
}

/// Тайл мира под курсором окна. `None` — курсор вне окна или за границами
/// мира (камера может смотреть и мимо карты). Публичная: используется
/// рендером (голограмма постройки) и меню строительства.
pub(crate) fn cursor_world_tile(
    window: &Window,
    cameras: &Query<(&Transform, &Projection), With<Camera2d>>,
    world: Option<&World>,
) -> Option<TilePos> {
    world_tile_at(world?, cursor_world_pos(window, cameras)?)
}
