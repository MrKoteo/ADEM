//! Фоновая генерация мира: тяжёлые проходы (рельеф, реки, посев цепочек
//! растительности) идут в отдельном потоке, UI и редактор не блокируются.
//!
//! Один активный job: новый запрос отменяет предыдущий (атомарный флаг,
//! который генерация проверяет на каждом проходе) и запускает поток с
//! новыми параметрами. Готовый мир приходит через канал и устанавливается
//! в `GameState` в Update-системе.
//!
//! Старый мир при старте генерации **выгружается**: две карты в ОЗУ
//! (4096² — это десятки МБ каждая) не нужны, а прогресс-бар в центре экрана
//! показывает, что происходит. Замеры времени пишутся в
//! `GenerationTimes` (на диск — только при выходе) и дают оценку остатка.
//!
//! Планировщик растительности строится в том же потоке: посев цепочек на
//! большой карте — второй по стоимости проход после рельефа, и он тоже
//! должен быть отменяемым. Календарные события засеваются здесь же, чтобы
//! установка мира заменяла планировщик целиком и не теряла их.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use bevy::prelude::*;

use crate::app::generation_times::GenerationTimes;
use crate::app::state::{ClockRes, GameState, SchedulerRes, TickAccumulator};
use crate::simulation::climate;
use crate::simulation::generation::generate_world_cancellable;
use crate::simulation::scheduler::{Scheduler, TaskKind};
use crate::simulation::time::{next_hour_tick, next_season_tick};
use crate::simulation::vegetation;
use crate::simulation::{World, WorldSize};

/// Доля генерации (рельеф, гидрология, объекты) в общем прогрессе: посев
/// цепочек растительности — остальные 15%.
const WORLD_SHARE: f32 = 0.85;

/// Параметры запрошенной генерации: сравнение по ним отсекает повторный
/// Enter с теми же значениями и определяет, что именно строит поток.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct GenerationParams {
    pub size: WorldSize,
    pub seed: u64,
}

/// Результат фоновой генерации: мир + уже засеянный планировщик.
pub(crate) struct GeneratedWorld {
    pub world: World,
    pub scheduler: Scheduler,
    pub params: GenerationParams,
}

/// Активная фоновая генерация и параметры последнего запроса.
#[derive(Resource, Default)]
pub(crate) struct WorldGeneration {
    job: Option<GenerationJob>,
    /// Запрошенные параметры (включая уже идущую генерацию): UI сравнивает
    /// с ними, чтобы не перезапускать поток на каждый Enter.
    pub(crate) requested: Option<GenerationParams>,
    /// Прогресс активной генерации 0..=1 (для прогресс-бара).
    pub(crate) progress: f32,
    /// Момент запуска активной генерации: ETA и замер времени.
    pub(crate) started: Option<Instant>,
}

struct GenerationJob {
    /// `std::sync::mpsc::Receiver` не `Sync`, а ресурс Bevy обязан быть
    /// `Send + Sync`: канал заворачивается в мьютекс (проверка раз в кадр).
    receiver: Mutex<Receiver<GeneratedWorld>>,
    handle: JoinHandle<()>,
    cancel: Arc<AtomicBool>,
    /// Прогресс потока в промилле (0..=1000): `AtomicU32` читается без
    /// блокировок, писать в него может только поток генерации.
    progress: Arc<AtomicU32>,
}

impl WorldGeneration {
    /// Параметры, которые сейчас «в работе»: pending-запрос, иначе
    /// применённый мир. Ввод второго поля (сид/размер) должен комбинироваться
    /// с pending-значением первого, а не с устаревшим применённым.
    pub(crate) fn pending_params(&self, state: &GameState) -> GenerationParams {
        self.requested.unwrap_or(GenerationParams {
            size: state.world_size,
            seed: state.seed,
        })
    }

    /// Сколько секунд уже идёт активная генерация.
    pub(crate) fn elapsed_secs(&self) -> f32 {
        self.started.map_or(0.0, |started| started.elapsed().as_secs_f32())
    }

    /// Отменяет активную генерацию (поток завершится на ближайшей проверке).
    /// Возвращает `true`, если было что отменять.
    pub(crate) fn cancel(&mut self) -> bool {
        let had_job = match self.job.take() {
            Some(job) => {
                job.cancel.store(true, Ordering::Relaxed);
                // JoinHandle отпускается: поток не нужен, а ждать его в
                // UI-потоке нельзя (это и был бы фриз, от которого уходим).
                true
            }
            None => false,
        };
        self.requested = None;
        self.progress = 0.0;
        self.started = None;
        had_job
    }
}

/// Запускает генерацию мира в отдельном потоке. Повторный запрос с теми же
/// параметрами игнорируется; параметры, равные уже установленному миру и
/// без активной генерации, тоже не перезапускают карту.
///
/// Старый мир перед стартом выгружается: генерация занимает секунды, и
/// держать две карты в ОЗУ (десятки МБ на больших размерах) незачем.
pub(crate) fn request_world(
    state: &mut GameState,
    generation: &mut WorldGeneration,
    size: WorldSize,
    seed: u64,
    tick: u64,
) {
    // Пока мир сохранён/загружается в фоне, новый запрос ждёт: иначе
    // результат операции подменит только что запущенную генерацию.
    if state.saving || state.loading {
        return;
    }
    let params = GenerationParams { size, seed };
    // Возврат к параметрам уже установленного мира отменяет идущую
    // генерацию: пересобирать ту же карту незачем.
    let applied = state.world.is_some() && state.world_size == size && state.seed == seed;
    if applied {
        generation.cancel();
        state.generating = false;
        return;
    }
    if generation.requested == Some(params) {
        return;
    }
    generation.cancel();

    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU32::new(0));
    let thread_cancel = Arc::clone(&cancel);
    let thread_progress = Arc::clone(&progress);
    let (sender, receiver) = channel();
    let handle = std::thread::Builder::new()
        .name(format!("world-gen-{}x{}", size.tiles(), size.tiles()))
        .spawn(move || {
            let cancelled = || thread_cancel.load(Ordering::Relaxed);
            let report = |fraction: f32| {
                let permille = (fraction.clamp(0.0, 1.0) * 1000.0) as u32;
                thread_progress.store(permille, Ordering::Relaxed);
            };
            let Some(world) = generate_world_cancellable(size, seed, &cancelled, &|fraction| {
                report(fraction * WORLD_SHARE)
            }) else {
                return;
            };
            let mut scheduler = Scheduler::starting_at(tick);
            if !vegetation::seed_all_cancellable(&world, &mut scheduler, tick, &cancelled, &|f| {
                report(WORLD_SHARE + (1.0 - WORLD_SHARE) * f)
            }) {
                return;
            }
            // Календарь как в `seed_calendar`: следующий час/сезон от тика,
            // на котором стартовала генерация (симуляция на это время стоит).
            scheduler.schedule_at(next_hour_tick(tick), TaskKind::TimeOfDayChanged);
            scheduler.schedule_at(next_season_tick(tick), TaskKind::SeasonChanged);
            report(1.0);
            // Приёмник мог быть выброшен (запрос отменили) — отправка тогда
            // просто не удалась, поток завершается.
            let _ = sender.send(GeneratedWorld {
                world,
                scheduler,
                params,
            });
        })
        .expect("world generation thread");

    generation.job = Some(GenerationJob {
        receiver: Mutex::new(receiver),
        handle,
        cancel,
        progress,
    });
    generation.requested = Some(params);
    generation.progress = 0.0;
    generation.started = Some(Instant::now());
    state.generating = true;

    // Старый мир выгружается сразу: дальше на экране только прогресс-бар,
    // а рендер сцену снимет сам (см. `render_world`). Захват существа и
    // выбор редактора ссылаются на id прежнего мира — сбрасываются.
    state.world = None;
    state.set_controlled(None);
    state.dirty_tiles.clear();
}

/// Забирает готовый мир из фонового потока и устанавливает его в игру.
/// Пока job активен, тики симуляции стоят; после установки мир оживает.
pub(crate) fn poll_world_generation(
    mut generation: ResMut<WorldGeneration>,
    mut state: ResMut<GameState>,
    mut scheduler: ResMut<SchedulerRes>,
    mut acc: ResMut<TickAccumulator>,
    clock: Res<ClockRes>,
    mut times: ResMut<GenerationTimes>,
) {
    let Some(job) = generation.job.take() else {
        return;
    };
    generation.progress = job.progress.load(Ordering::Relaxed) as f32 / 1000.0;
    let received = job
        .receiver
        .lock()
        .expect("generation receiver mutex")
        .try_recv();
    match received {
        Ok(generated) => {
            let mut world = generated.world;
            world.ambient_temperature_c = climate::ambient_temperature_c(clock.0.tick);
            state.world = Some(world);
            state.world_size = generated.params.size;
            state.seed = generated.params.seed;
            scheduler.0 = generated.scheduler;
            // Аккумулятор старого мира не переносится: тики нового мира
            // начинаются с текущего тика часов.
            acc.seconds = 0.0;
            state.mark_world_replaced();
            generation.requested = None;
            generation.progress = 1.0;
            // Замер полного времени (рельеф + посев) — для оценки ETA
            // следующих карт.
            if let Some(started) = generation.started.take() {
                times.record(
                    generated.params.size.tiles(),
                    started.elapsed().as_secs_f32(),
                );
            }
        }
        Err(TryRecvError::Empty) => {
            if job.handle.is_finished() {
                // Поток завершился без результата: отмена снаружи уже
                // сбросила бы job, значит это ошибка/паника генерации.
                warn!("Фоновая генерация мира завершилась без результата");
                generation.requested = None;
                generation.progress = 0.0;
                generation.started = None;
                state.generating = false;
            } else {
                generation.job = Some(job);
            }
        }
        Err(TryRecvError::Disconnected) => {
            warn!("Фоновая генерация мира потеряла канал результата");
            generation.requested = None;
            generation.progress = 0.0;
            generation.started = None;
            state.generating = false;
        }
    }
}

/// Пока идёт генерация, старый мир уже выгружен: его планировщик (цепочки
/// растительности, культуры, станции) тоже освобождается — иначе десятки
/// тысяч задач висят в памяти до установки нового мира.
pub(crate) fn release_old_world_scheduler(
    state: Res<GameState>,
    mut scheduler: ResMut<SchedulerRes>,
    mut released: Local<bool>,
) {
    let generating = state.generating && !state.saving;
    if !generating {
        *released = false;
        return;
    }
    if !*released {
        *released = true;
        scheduler.0 = Scheduler::default();
    }
}

/// Стартовая генерация: вместо блокирующего PostStartup — фоновая задача.
pub(crate) fn start_initial_generation(
    mut state: ResMut<GameState>,
    mut generation: ResMut<WorldGeneration>,
    clock: Res<ClockRes>,
) {
    let size = state.world_size;
    let seed = state.seed;
    request_world(&mut state, &mut generation, size, seed, clock.0.tick);
}

#[cfg(test)]
#[path = "../tests/generation_app.rs"]
mod tests;
