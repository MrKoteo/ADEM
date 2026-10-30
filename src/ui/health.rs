//! Полосы потребностей игрока в левом нижнем углу, снизу вверх: здоровье
//! (красная), голод (оранжевая), броня (серая). Каждая — заливка на
//! полупрозрачном фоне панели и число справа от полосы.
//!
//! Видны только в режиме игрока: в наблюдателе существа нет, и полосы
//! не должны занимать экран. Значения читаются из `Player` каждый
//! кадр — источник правды остаётся в симуляции.

use bevy::prelude::*;

use crate::app::state::{GameMode, GameState, PlayerRes};
use crate::simulation::armor::{ARMOR_RATING_MAX, rating as armor_rating};
use crate::simulation::hunger::HUNGER_MAX;
use crate::simulation::player::PLAYER_MAX_HP;
use crate::ui::UiFont;
use crate::ui::style;

pub struct HealthPlugin;

impl Plugin for HealthPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_health_bar)
            .add_systems(Update, (toggle_health_visibility, refresh_health_bar));
    }
}

/// Ширина и высота полосы (без рамки).
const BAR_WIDTH: f32 = 220.0;
const BAR_HEIGHT: f32 = 18.0;

/// Отступ от края экрана.
const MARGIN: f32 = 16.0;

/// Зазор между шкалами в колонке.
const BAR_GAP: f32 = 4.0;

/// Цвет заливки шкалы брони.
const ARMOR_COLOR: Color = Color::srgb_u8(0xCC, 0xCC, 0xCC);

/// Цвет заливки шкалы голода.
const HUNGER_COLOR: Color = Color::srgb_u8(0xFF, 0x88, 0x44);

/// Цвет заливки шкалы здоровья.
const HEALTH_COLOR: Color = Color::srgb(0.78, 0.16, 0.14);

/// Какая шкала: по маркеру заливка и текст находят своё значение игрока.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
enum BarKind {
    Armor,
    Hunger,
    Health,
}

/// Маркер корневой колонки (виден переключателю видимости).
#[derive(Component)]
struct HealthRoot;

/// Заливка шкалы: тип шкалы (цвет заливки уже стоит в `BackgroundColor`).
#[derive(Component)]
struct BarFill {
    kind: BarKind,
}

/// Число значения справа от полосы.
#[derive(Component)]
struct BarText {
    kind: BarKind,
}

fn spawn_health_bar(mut commands: Commands, font: Res<UiFont>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(MARGIN),
                bottom: Val::Px(MARGIN),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(BAR_GAP),
                ..default()
            },
            HealthRoot,
        ))
        .with_children(|root| {
            // Снизу вверх: здоровье страдает от голода, голод — от брони
            // (броня поглощает урон, но не сытость). Броня сверху.
            spawn_bar(root, &font, BarKind::Health, HEALTH_COLOR);
            spawn_bar(root, &font, BarKind::Hunger, HUNGER_COLOR);
            spawn_bar(root, &font, BarKind::Armor, ARMOR_COLOR);
        });
}

/// Одна полоса: ряд из фона с рамкой, заливки на всю ширину и числа
/// справа. Значение заливку и текст обновляет `refresh_health_bar`.
fn spawn_bar(
    root: &mut ChildSpawnerCommands,
    font: &UiFont,
    kind: BarKind,
    fill_color: Color,
) {
    root.spawn(Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(8.0),
        ..default()
    })
    .with_children(|row| {
        // Пустая часть полосы — фон панели (полупрозрачный тёмный).
        row.spawn((
            Node {
                width: Val::Px(BAR_WIDTH),
                height: Val::Px(BAR_HEIGHT),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(style::background()),
            BorderColor::all(style::border()),
        ))
        .with_children(|bar| {
            bar.spawn((
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                BackgroundColor(fill_color),
                BarFill { kind },
            ));
        });

        row.spawn((
            Text::new(String::new()),
            TextFont {
                font: font.0.clone().into(),
                font_size: 14.0.into(),
                ..default()
            },
            TextColor(style::text()),
            BarText { kind },
        ));
    });
}

fn toggle_health_visibility(
    state: Res<GameState>,
    mut roots: Query<&mut Node, With<HealthRoot>>,
) {
    let visible = state.mode == GameMode::Player;
    for mut node in &mut roots {
        let want = if visible { Display::Flex } else { Display::None };
        if node.display != want {
            node.display = want;
        }
    }
}

/// Значение шкалы и её максимум по типу. Броня вычисляется из надетых
/// предметов (без брони — 0.0 и полоса пуста).
fn bar_value(kind: BarKind, player: &crate::simulation::player::Player) -> (f32, f32) {
    match kind {
        BarKind::Armor => (armor_rating(&player.inventory), ARMOR_RATING_MAX),
        BarKind::Hunger => (player.hunger.value(), HUNGER_MAX),
        BarKind::Health => (player.hp, PLAYER_MAX_HP),
    }
}

/// Обновляет все полосы: ширину заливки и число справа. Пишет только
/// реальное изменение, чтобы не дёргать переразметку каждый кадр.
fn refresh_health_bar(
    state: Res<GameState>,
    player: Res<PlayerRes>,
    mut fills: Query<(&mut Node, &BarFill)>,
    mut texts: Query<(&BarText, &mut Text)>,
) {
    if state.mode != GameMode::Player {
        return;
    }
    let Some(p) = player.0.as_ref() else {
        return;
    };

    for (mut node, fill) in &mut fills {
        let (value, max) = bar_value(fill.kind, p);
        let ratio = (value / max).clamp(0.0, 1.0);
        let want = Val::Percent(ratio * 100.0);
        if node.width != want {
            node.width = want;
        }
    }
    for (bar, mut text) in &mut texts {
        let (value, _) = bar_value(bar.kind, p);
        let want = format!("{value:.1}");
        if text.as_str() != want {
            **text = want.clone();
        }
    }
}
