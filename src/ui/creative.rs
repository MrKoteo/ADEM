//! Творческое меню (клавиша I): плитки всех существующих предметов.
//! Клик по плитке — полный стак предмета в инвентарь игрока. Меню доступно
//! в обоих режимах, пока существо игрока существует («припаркованное»
//! в наблюдателе тоже подходит — инвентарь у него общий).
//!
//! Окно модальное (через `MenuKind::Creative`): ESC закрывает его той же
//! цепочкой, что и остальные меню; T/Y при открытом творческом переключают
//! на свои экраны. Переназначаемая клавиша — бинд «Творческое меню».

use bevy::input_focus::InputFocus;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui_widgets::{Activate, Button, TextInput};

use crate::app::settings::{ControlAction, Settings};
use crate::app::state::{GameState, MenuKind, PlayerRes};
use crate::simulation::items::ItemKind;
use crate::ui::UiFont;
use crate::ui::bottom_panel::FitIcon;
use crate::ui::icons::UiIcons;
use crate::ui::inventory::InventoryUi;
use crate::ui::menus::MenuRoot;
use crate::ui::pause_menu::BindingCapture;
use crate::ui::style;
use crate::ui::text_input_focused;
use crate::ui::BlocksWorldInput;

pub struct CreativePlugin;

impl Plugin for CreativePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, toggle_creative)
            .add_observer(on_tile_activated);
    }
}

// --- Раскладка: как в инвентаре ---
/// Сторона плитки.
const TILE_SIZE_PX: f32 = 44.0;
/// Квадрат иконки внутри плитки.
const ICON_BOX: f32 = 30.0;
/// Зазор между плитками и рядами.
const TILE_GAP: f32 = 6.0;
/// Плиток в ряду (как основных слотов инвентаря).
const TILES_PER_ROW: usize = 8;

/// Маркер плитки предмета: клик выдаёт полный стак этого вида.
#[derive(Component)]
pub(crate) struct CreativeTile {
    pub(crate) kind: ItemKind,
}

/// Тоггл окна по бинду «Творческое меню» (по умолчанию I). Пока фокус
/// в текстовом поле, идёт захват бинда или открыт пауз-меню — не срабатывает.
/// Требует существующего игрока: класть предметы некуда, если существа нет.
fn toggle_creative(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<GameState>,
    player: Res<PlayerRes>,
    inventory: Res<InventoryUi>,
    settings: Res<Settings>,
    capture: Res<BindingCapture>,
    focus: Res<InputFocus>,
    text_inputs: Query<(), With<TextInput>>,
    font: Res<UiFont>,
    icons: Res<UiIcons>,
    roots: Query<Entity, With<MenuRoot>>,
) {
    if text_input_focused(&focus, &text_inputs) || capture.0.is_some() {
        return;
    }
    if state.active_menu == Some(MenuKind::Pause) {
        return;
    }
    // Окно инвентаря — такой же центральный оверлей: открывать творческое
    // поверх него нельзя (инвентарь, в свою очередь, не открывается при
    // открытой модалке — правило симметричное).
    if inventory.open {
        return;
    }
    if !keys.just_pressed(settings.controls.key(ControlAction::CreativeMenu)) {
        return;
    }

    // Повторное нажатие закрывает окно.
    if state.active_menu == Some(MenuKind::Creative) {
        for e in &roots {
            commands.entity(e).despawn();
        }
        state.active_menu = None;
        return;
    }
    // Без существа игрока меню бесполезно: выдавать предметы некому.
    if player.0.is_none() {
        return;
    }

    // Переключение с других модалок (технологии, институты): их корни сносятся.
    for e in &roots {
        commands.entity(e).despawn();
    }
    state.active_menu = Some(MenuKind::Creative);
    spawn_creative(&mut commands, &font, &icons);
}

/// Строит окно: заголовок, подсказка и плитки всех предметов рядами
/// по `TILES_PER_ROW`. Полноэкранный wrapper нужен для центрирования;
/// маркер `MenuRoot` стоит на нём — закрытие сносит весь overlay.
fn spawn_creative(commands: &mut Commands, font: &UiFont, icons: &UiIcons) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            MenuRoot,
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(10.0),
                        padding: UiRect::all(Val::Px(16.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(style::background()),
                    BorderColor::from(style::border()),
                    BlocksWorldInput,
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("Творческое меню"),
                        TextFont {
                            font: font.0.clone().into(),
                            font_size: 20.0.into(),
                            ..default()
                        },
                        TextColor(style::text()),
                    ));
                    panel.spawn((
                        Text::new("Клик по плитке — полный стак в инвентарь. Esc — закрыть"),
                        TextFont {
                            font: font.0.clone().into(),
                            font_size: 12.0.into(),
                            ..default()
                        },
                        TextColor(style::text_dim()),
                    ));
                    panel
                        .spawn(Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(TILE_GAP),
                            margin: UiRect::top(Val::Px(4.0)),
                            ..default()
                        })
                        .with_children(|rows| {
                            let items = ItemKind::ALL;
                            for row_start in (0..items.len()).step_by(TILES_PER_ROW) {
                                rows.spawn(Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(TILE_GAP),
                                    ..default()
                                })
                                .with_children(|row| {
                                    for &kind in &items[row_start..(row_start + TILES_PER_ROW)
                                        .min(items.len())]
                                    {
                                        spawn_tile(row, kind, font, icons);
                                    }
                                });
                            }
                        });
                });
        });
}

/// Плитка предмета: как слот инвентаря, счётчик — размер максимального стака.
fn spawn_tile(
    row: &mut ChildSpawnerCommands,
    kind: ItemKind,
    font: &UiFont,
    icons: &UiIcons,
) {
    row.spawn((
        Button,
        Node {
            width: Val::Px(TILE_SIZE_PX),
            height: Val::Px(TILE_SIZE_PX),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        },
        BackgroundColor(style::surface()),
        BorderColor::all(style::border()),
        Hovered::default(),
        CreativeTile { kind },
    ))
    .with_children(|tile| {
        tile.spawn((
            ImageNode::new(icons.item(kind).clone()),
            Node::default(),
        ))
        .insert(FitIcon { box_px: ICON_BOX });
        // Счётчик — предел стака предмета: у инструментов он 1, цифра
        // не показывается (как в инвентаре — счётчик только для >1).
        let max_stack = kind.max_stack();
        tile.spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(3.0),
                bottom: Val::Px(2.0),
                ..default()
            },
            Text::new(if max_stack > 1 {
                max_stack.to_string()
            } else {
                String::new()
            }),
            TextFont {
                font: font.0.clone().into(),
                font_size: 12.0.into(),
                ..default()
            },
            TextColor(style::text()),
        ));
    });
}

/// Клик по плитке: полный стак предмета (его `max_stack`: 1 у
/// инструментов, 64 у расходников) в инвентарь игрока. Инвентарь может
/// переполниться — остаток игнорируется (творческое меню не ограничено).
/// Наблюдатель реагирует только на плитки творческого меню: `Activate` —
/// глобальное событие, его шлют и все остальные кнопки UI.
fn on_tile_activated(
    activate: On<Activate>,
    state: Res<GameState>,
    tiles: Query<&CreativeTile>,
    mut player: ResMut<PlayerRes>,
) {
    if state.active_menu != Some(MenuKind::Creative) {
        return;
    }
    let Ok(tile) = tiles.get(activate.entity) else {
        return;
    };
    let Some(p) = player.0.as_mut() else {
        return;
    };
    p.inventory.add(tile.kind, tile.kind.max_stack());
}
