//! Иконка окна (тайтлбар и панель задач).
//!
//! В bevy 0.20 поле `Window::icon` удалено, поэтому иконка ставится
//! напрямую в winit через `WINIT_WINDOWS`. Окно к моменту `Startup` уже
//! создано: `create_windows` вызывается в `resumed`, до первого прогона
//! расписания.
//!
//! `NonSendMarker` обязателен: `WINIT_WINDOWS` — thread-local, а без него
//! планировщик может запустить систему на worker-потоке с пустой копией.
//! Так же фиксируют поток сами системы bevy_winit.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

/// PNG встраивается в бинарник: иконка не зависит от рабочей директории
/// и от того, скопированы ли `assets` рядом с exe.
const ICON_PNG: &[u8] = include_bytes!("../../icon.png");

/// Ставит иконку окна (тайтлбар + панель задач).
pub fn set_window_icon(
    windows: Query<Entity, With<PrimaryWindow>>,
    // NonSend-параметр держит систему на главном потоке.
    _main_thread: bevy::ecs::system::NonSendMarker,
) {
    let Ok(entity) = windows.single() else {
        warn!("Основное окно не найдено, иконка не установлена");
        return;
    };
    let img = match image::load_from_memory(ICON_PNG) {
        Ok(img) => img,
        Err(e) => {
            warn!("Иконка окна не прочитана: {e}");
            return;
        }
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    let icon = match winit::window::Icon::from_rgba(rgba.into_raw(), width, height) {
        Ok(icon) => icon,
        Err(e) => {
            warn!("Иконка окна испорчена ({width}x{height}): {e}");
            return;
        }
    };
    bevy::winit::WINIT_WINDOWS.with_borrow(|winit_windows| {
        match winit_windows.get_window(entity) {
            Some(winit_window) => winit_window.set_window_icon(Some(icon)),
            None => warn!("winit-окно для {entity:?} не найдено, иконка не установлена"),
        }
    });
}
