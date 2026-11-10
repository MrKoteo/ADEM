//! Сохранение и загрузка игры в файлы слотов `saves/save_N.adem`.
//!
//! IO живёт здесь (app-слой); сериализация — в `simulation/save.rs`
//! без зависимости от Bevy. UI шлёт запросы-сообщения со слотом, системы
//! ниже выполняют их в фоновом потоке и обновляют экран слотов.
//!
//! Почему в фоне: сериализация и разбор большого мира — такая же тяжёлая
//! работа, как генерация, и на главном потоке она давала те же фризы.
//! Загрузка читает файл в фоне, пока старый мир продолжает жить. Сохранение
//! временно забирает мир и планировщик в поток (копировать их нельзя) и
//! возвращает их вместе с результатом; на это время `GameState::saving`
//! останавливает тики и блокирует второй запрос.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use bevy::prelude::*;

use crate::app::generation::WorldGeneration;
use crate::app::settings::Settings;
use crate::app::state::{ClockRes, GameState, SchedulerRes, TickAccumulator};
use crate::simulation::save::{self, SaveHeader, SavedGame};
use crate::simulation::{GameClock, Scheduler, World};
use crate::ui::pause_menu::{
    PauseMenuRoot, PauseMenuView, SaveStatusText, SlotLabel, close_menu, set_status_text,
    slot_label,
};

/// Число слотов сохранения (решение автора: save_1..save_4).
pub const SAVE_SLOTS: u8 = 4;

/// Запрос UI: записать текущую симуляцию в слот (нумерация с 1).
#[derive(Message)]
pub struct SaveGameRequest(pub u8);

/// Запрос UI: загрузить симуляцию из слота (нумерация с 1).
#[derive(Message)]
pub struct LoadGameRequest(pub u8);

/// Результат фонового сохранения: мир и планировщик возвращаются обратно.
type SaveOutcome = (World, Scheduler, u8, Result<(), String>);

/// Результат фоновой загрузки: слот и разобранное сохранение либо ошибка.
type LoadOutcome = (u8, Result<SavedGame, String>);

/// Активные фоновые операции сохранения/загрузки.
#[derive(Resource, Default)]
pub(crate) struct SaveLoadTasks {
    /// `Receiver` не `Sync`, а ресурс Bevy обязан быть `Send + Sync`:
    /// каналы заворачиваются в мьютекс (проверка раз в кадр).
    save: Option<Mutex<Receiver<SaveOutcome>>>,
    load: Option<Mutex<Receiver<LoadOutcome>>>,
}

/// Каталог сохранений: рабочий каталог при `cargo run` (там уже лежит
/// папка `saves`), иначе рядом с exe.
fn saves_dir() -> PathBuf {
    let cwd = PathBuf::from("saves");
    if cwd.is_dir() {
        return cwd;
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("saves")
}

/// Путь к файлу слота (слоты нумеруются с 1).
fn slot_path(slot: u8) -> PathBuf {
    saves_dir().join(format!("save_{slot}.adem"))
}

/// Заголовок слота без чтения тела файла — для подписей кнопок.
/// None — слота нет или файл не читается (в списке он выглядит пустым).
pub fn slot_info(slot: u8) -> Option<SaveHeader> {
    let mut file = fs::File::open(slot_path(slot)).ok()?;
    // Заголовок — несколько десятков байт; читаем префикс с запасом.
    let mut prefix = vec![0_u8; 256];
    let len = file.read(&mut prefix).ok()?;
    prefix.truncate(len);
    save::read_header(&prefix).ok()
}

pub struct SavesPlugin;

impl Plugin for SavesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SaveLoadTasks>()
            .add_message::<SaveGameRequest>()
            .add_message::<LoadGameRequest>()
            .add_systems(
                Update,
                (
                    handle_save_request,
                    handle_load_request,
                    poll_save_task,
                    poll_load_task,
                ),
            );
    }
}

/// Кладёт сохранение текущей симуляции в слот **в фоновом потоке**.
/// Мир и планировщик временно переезжают в поток и возвращаются вместе с
/// результатом: копировать большой мир дороже самой записи, а держать
/// `&World` на главном потоке нельзя (симуляция его мутирует).
fn handle_save_request(
    mut requests: MessageReader<SaveGameRequest>,
    mut state: ResMut<GameState>,
    clock: Res<ClockRes>,
    mut sched: ResMut<SchedulerRes>,
    mut tasks: ResMut<SaveLoadTasks>,
    status: Query<Entity, With<SaveStatusText>>,
    mut texts: Query<&mut Text, Without<SlotLabel>>,
) {
    for request in requests.read() {
        let slot = request.0.clamp(1, SAVE_SLOTS);
        if state.generating || state.loading {
            set_status_text(&status, &mut texts, "Дождитесь окончания операции с миром");
            continue;
        }
        if state.saving {
            set_status_text(&status, &mut texts, "Сохранение уже идёт");
            continue;
        }
        let Some(world) = state.world.take() else {
            set_status_text(&status, &mut texts, "Мир ещё не создан");
            continue;
        };
        let scheduler = std::mem::take(&mut sched.0);
        let clock = clock.0;
        state.saving = true;
        set_status_text(&status, &mut texts, "Сохранение…");

        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("save-game".to_string())
            .spawn(move || {
                let bytes = save::save_game(&world, &clock, &scheduler, crate::app::GAME_VERSION);
                let path = slot_path(slot);
                let result = fs::create_dir_all(saves_dir())
                    .and_then(|_| fs::write(&path, &bytes))
                    .map_err(|e| e.to_string());
                let _ = sender.send((world, scheduler, slot, result));
            })
            .expect("save thread");
        tasks.save = Some(Mutex::new(receiver));
    }
}

/// Загружает симуляцию из слота: чтение и разбор файла идут в фоне,
/// старый мир продолжает жить до готовности нового.
fn handle_load_request(
    mut requests: MessageReader<LoadGameRequest>,
    mut state: ResMut<GameState>,
    mut generation: ResMut<WorldGeneration>,
    mut tasks: ResMut<SaveLoadTasks>,
    status: Query<Entity, With<SaveStatusText>>,
    mut texts: Query<&mut Text, Without<SlotLabel>>,
) {
    for request in requests.read() {
        let slot = request.0.clamp(1, SAVE_SLOTS);
        if state.saving || state.loading {
            set_status_text(&status, &mut texts, "Дождитесь окончания операции с миром");
            continue;
        }
        // Загрузка отменяет фоновую генерацию: иначе её результат придёт
        // позже и подменит загруженный мир.
        generation.cancel();
        state.generating = false;
        state.loading = true;
        set_status_text(&status, &mut texts, "Загрузка…");

        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("load-game".to_string())
            .spawn(move || {
                let result = fs::read(slot_path(slot))
                    .map_err(|e| format!("слот {slot} недоступен: {e}"))
                    .and_then(|bytes| {
                        save::load_game(&bytes).map_err(|e| format!("слот {slot}: {e}"))
                    });
                let _ = sender.send((slot, result));
            })
            .expect("load thread");
        tasks.load = Some(Mutex::new(receiver));
    }
}

/// Применяет результат фонового сохранения: возвращает мир и планировщик,
/// обновляет статус и подпись слота.
fn poll_save_task(
    mut tasks: ResMut<SaveLoadTasks>,
    mut state: ResMut<GameState>,
    mut sched: ResMut<SchedulerRes>,
    mut generation: ResMut<WorldGeneration>,
    clock: Res<ClockRes>,
    status: Query<Entity, With<SaveStatusText>>,
    mut texts: Query<&mut Text, Without<SlotLabel>>,
    mut slot_labels: Query<(&SlotLabel, &mut Text)>,
) {
    let Some(receiver) = tasks.save.take() else {
        return;
    };
    let received = receiver
        .lock()
        .expect("save receiver mutex")
        .try_recv();
    match received {
        Ok((world, scheduler, slot, result)) => {
            state.world = Some(world);
            sched.0 = scheduler;
            state.saving = false;
            match result {
                Ok(()) => {
                    set_status_text(
                        &status,
                        &mut texts,
                        &format!("Игра сохранена — слот {slot}"),
                    );
                    let info = slot_info(slot);
                    for (label, mut text) in &mut slot_labels {
                        if label.0 == slot {
                            **text = slot_label(slot, info.as_ref());
                        }
                    }
                }
                Err(e) => {
                    set_status_text(&status, &mut texts, &format!("Ошибка сохранения: {e}"));
                }
            }
        }
        Err(TryRecvError::Empty) => {
            tasks.save = Some(receiver);
        }
        Err(TryRecvError::Disconnected) => {
            // Поток завершился без результата (паника): мир потерян.
            // Пересоздаём его по текущим параметрам, чтобы игра не осталась
            // без мира; сообщаем об ошибке.
            state.saving = false;
            warn!("Фоновое сохранение завершилось без результата");
            set_status_text(
                &status,
                &mut texts,
                "Сохранение не удалось; мир пересоздаётся",
            );
            let size = state.world_size;
            let seed = state.seed;
            crate::app::generation::request_world(
                &mut state,
                &mut generation,
                size,
                seed,
                clock.0.tick,
            );
        }
    }
}

/// Применяет результат фоновой загрузки: подменяет мир, часы и планировщик,
/// закрывает меню и снимает паузу.
fn poll_load_task(
    mut commands: Commands,
    mut tasks: ResMut<SaveLoadTasks>,
    mut state: ResMut<GameState>,
    mut clock: ResMut<ClockRes>,
    mut sched: ResMut<SchedulerRes>,
    mut acc: ResMut<TickAccumulator>,
    settings: Res<Settings>,
    status: Query<Entity, With<SaveStatusText>>,
    mut texts: Query<&mut Text, Without<SlotLabel>>,
    roots: Query<Entity, With<PauseMenuRoot>>,
    mut view: ResMut<PauseMenuView>,
) {
    let Some(receiver) = tasks.load.take() else {
        return;
    };
    let received = receiver
        .lock()
        .expect("load receiver mutex")
        .try_recv();
    match received {
        Ok((_slot, Ok(loaded))) => {
            let size = loaded.world.size;
            state.world = Some(loaded.world);
            state.world_size = size;
            state.seed = loaded.seed;
            clock.0 = GameClock { tick: loaded.tick };
            sched.0 = loaded.scheduler;
            // Аккумулятор старого мира не переносится: тики начинаются с файла.
            acc.seconds = 0.0;
            // Новая эпоха мира: камера один раз центрируется на загруженной
            // карте, рендер сбрасывает блоки и спрайты. Управление всегда
            // начинается с наблюдателя — существо можно захватить заново по F1.
            state.mark_world_replaced();
            close_menu(&mut commands, &roots, &mut state, &mut view, &settings);
        }
        Ok((_slot, Err(message))) => {
            state.loading = false;
            set_status_text(
                &status,
                &mut texts,
                &format!("Не удалось загрузить: {message}"),
            );
        }
        Err(TryRecvError::Empty) => {
            tasks.load = Some(receiver);
        }
        Err(TryRecvError::Disconnected) => {
            state.loading = false;
            set_status_text(&status, &mut texts, "Не удалось загрузить сохранение");
        }
    }
}
