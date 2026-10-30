//! 2D-камера: спавн, зум колёсиком, панорамирование ПКМ, движение
//! клавишами в редакторе, следование за игроком.

use bevy::camera::{Camera2d, OrthographicProjection, Projection};
use bevy::input::mouse::{MouseButton, MouseMotion, MouseWheel};
use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::ui_widgets::TextInput;

use crate::app::input::move_dir;
use crate::app::settings::Settings;
use crate::app::state::{GameMode, GameState, PlayerRes, SimTick, TickAccumulator, player_render_pos};
use crate::simulation::TILE_SIZE;
use crate::ui::pause_menu::BindingCapture;
use crate::ui::text_input_focused;

const ZOOM_MIN: f32 = 0.08;
const ZOOM_MAX: f32 = 150.0;
const ZOOM_STEP: f32 = 0.12;
const DEFAULT_SCALE: f32 = 2.0;

/// Скорость движения камеры клавишами: доли экрана в секунду. Ширина и
/// высота считаются отдельно — горизонталь идёт со скоростью пол-ширины
/// экрана, вертикаль пол-высоты.
const CAMERA_SPEED_SCREEN_FRACTION: f32 = 0.5;

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            .add_systems(
                Update,
                (zoom_system, editor_move_system, pan_system, follow_player)
                    .chain()
                    .after(SimTick),
            );
    }
}

/// Камера игнорирует ввод, пока открыто любое модальное меню (пауза,
/// деревья технологий/институтов, творческое): полноэкранные деревья
/// зумятся тем же колесом и панорамируются той же ПКМ — мир позади
/// не должен реагировать на тот же ввод.
fn modal_menu_open(state: &GameState) -> bool {
    state.active_menu.is_some()
}

/// Спавнит камеру в центре будущего мира.
fn spawn_camera(mut commands: Commands, state: Res<GameState>) {
    let center = state.world_size.tiles() as f32 * TILE_SIZE * 0.5;
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scale: DEFAULT_SCALE,
            ..OrthographicProjection::default_2d()
        }),
        Transform::from_xyz(center, center, 0.0),
    ));
}

/// Зум колёсиком. Скорость зума экспоненциальная, диапазон — [ZOOM_MIN, ZOOM_MAX].
fn zoom_system(
    mut query: Query<&mut Projection, With<Camera2d>>,
    mut wheel: MessageReader<MouseWheel>,
    state: Res<GameState>,
) {
    if modal_menu_open(&state) {
        return;
    }
    let mut delta = 0.0_f32;
    for ev in wheel.read() {
        delta += ev.y;
    }
    if delta.abs() < f32::EPSILON {
        return;
    }
    for mut proj in query.iter_mut() {
        if let Projection::Orthographic(ortho) = &mut *proj {
            let factor = 1.0 - delta * ZOOM_STEP;
            ortho.scale = (ortho.scale * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        }
    }
}

/// Движение камеры клавишами в режиме редактора (бинды WASD, те же, что
/// двигают существо в режиме игрока). Скорость — 0.5 экрана в секунду:
/// ширина и высота области просмотра считаются отдельно, в мировых
/// единицах, поэтому при любом зуме экран проходится за 2 секунды.
/// Как и панорамирование, отключено в режиме игрока — камеру ведёт
/// существо, — а также в пауз-меню, при захвате бинда и наборе текста.
fn editor_move_system(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    state: Res<GameState>,
    capture: Res<BindingCapture>,
    focus: Res<InputFocus>,
    text_inputs: Query<(), With<TextInput>>,
    mut query: Query<(&mut Transform, &Projection), With<Camera2d>>,
) {
    if state.mode != GameMode::Observer || modal_menu_open(&state) {
        return;
    }
    if capture.0.is_some() || text_input_focused(&focus, &text_inputs) {
        return;
    }
    let (dx, dy) = move_dir(&keys, &settings);
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let Ok((mut tf, proj)) = query.single_mut() else {
        return;
    };
    let Projection::Orthographic(ortho) = proj else {
        return;
    };
    let step = Vec2::new(
        dx * CAMERA_SPEED_SCREEN_FRACTION * ortho.area.width(),
        dy * CAMERA_SPEED_SCREEN_FRACTION * ortho.area.height(),
    ) * time.delta_secs();
    tf.translation.x += step.x;
    tf.translation.y += step.y;
}

/// Панорамирование зажатой ПКМ. Величина сдвига умножается на текущий масштаб,
/// чтобы скорость движения мира под курсором была постоянной.
/// В режиме игрока панорамирование отключено: камеру ведёт существо.
fn pan_system(
    mut query: Query<(&mut Transform, &Projection), With<Camera2d>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut motion: MessageReader<MouseMotion>,
    state: Res<GameState>,
) {
    if modal_menu_open(&state) || state.mode == GameMode::Player {
        for _ in motion.read() {}
        return;
    }
    if !buttons.pressed(MouseButton::Right) {
        for _ in motion.read() {}
        return;
    }
    let mut delta = Vec2::ZERO;
    for ev in motion.read() {
        delta += ev.delta;
    }
    if delta == Vec2::ZERO {
        return;
    }
    for (mut tf, proj) in query.iter_mut() {
        let scale = match proj {
            Projection::Orthographic(ortho) => ortho.scale,
            _ => 1.0,
        };
        tf.translation.x -= delta.x * scale;
        tf.translation.y += delta.y * scale;
    }
}

/// Следование камеры за игроком в режиме игрока (жёсткое: существо всегда
/// в центре экрана). Запускается после панорамирования и перекрывает его.
/// Позиция интерполируется между тиками — иначе камера двигалась бы рывком
/// вместе со спрайтом существа.
fn follow_player(
    state: Res<GameState>,
    player: Res<PlayerRes>,
    acc: Res<TickAccumulator>,
    mut query: Query<&mut Transform, With<Camera2d>>,
) {
    if state.mode != GameMode::Player {
        return;
    }
    let Some(p) = player.0.as_ref() else {
        return;
    };
    // Позиция игрока хранится в тайлах; рендер работает в пикселях мира.
    let pos = player_render_pos(p, &acc);
    for mut tf in &mut query {
        tf.translation.x = pos.x * TILE_SIZE;
        tf.translation.y = pos.y * TILE_SIZE;
    }
}
