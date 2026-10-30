//! Звук: четыре шины (окружение, эффекты, существа, интерфейс) с
//! независимой громкостью плюс фоновый эмбиент и звуки UI.
//!
//! Особенность bevy: изменения в `PlaybackSettings` НЕ влияют на уже
//! играющий звук. Громкость по ходу воспроизведения меняется только
//! через `AudioSink`, который Bevy добавляет на сущность, когда звук
//! начинает играть. Поэтому ползунки применяются системой `apply_volumes`
//! ко всем играющим источникам, а свежие получают громкость при спавне.

use bevy::audio::{AudioPlayer, AudioSink, AudioSinkPlayback};
use bevy::prelude::*;

use crate::app::settings::{Settings, SoundBus, volume_for};

/// Какая шина у играющего звука.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bus(pub SoundBus);

/// Контроллер фонового эмбиента: зацикливает треки из
/// `assets/sounds/ambient` по очереди, с тишиной между ними.
#[derive(Resource)]
pub struct AmbientController {
    tracks: Vec<Handle<AudioSource>>,
    /// Таймер тишины между треками. Активен, когда ничего не играет.
    gap: Option<Timer>,
    /// Сущность текущего трека, чтобы не запустить второй поверх первого.
    current: Option<Entity>,
    /// Последний сыгранный индекс — не повторяем трек подряд.
    last_index: usize,
    /// Длительность тишины между треками, сек.
    gap_secs: f32,
}

/// Загружает треки эмбиента из каталога ассетов.
///
/// Каталог может отсутствовать (пустая установка) — тогда эмбиента
/// просто нет, это не ошибка.
pub fn setup_ambient(mut commands: Commands, asset_server: Res<AssetServer>) {
    let mut tracks = Vec::new();
    if let Ok(rd) = std::fs::read_dir("assets/sounds/ambient") {
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_audio = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|ext| matches!(ext, "ogg" | "wav" | "mp3"));
                let name = path.file_name()?.to_str()?.to_string();
                is_audio.then_some(name)
            })
            // read_dir отдаёт произвольный порядок — сортируем для
            // воспроизводимого результата.
            .collect();
        names.sort();
        for name in names {
            tracks.push(asset_server.load(format!("sounds/ambient/{name}")));
        }
    }

    commands.insert_resource(AmbientController {
        tracks,
        // Первый трек стартует сразу: таймер уже «истёк».
        gap: Some(zero_timer()),
        current: None,
        last_index: usize::MAX,
        gap_secs: 5.0,
    });
}

/// Таймер, «истёкший» в момент создания (нулевой duration не считается
/// finished до первого tick, поэтому ставим elapsed = duration).
fn zero_timer() -> Timer {
    let mut timer = Timer::from_seconds(0.0, TimerMode::Once);
    timer.set_elapsed(timer.duration());
    timer
}

/// Проигрывает треки эмбиента по очереди, с паузой тишины между ними.
///
/// Трек запускается только после окончания предыдущего: иначе короткий
/// трек успевает начаться поверх ещё не закончившегося.
pub fn ambient_system(
    time: Res<Time>,
    mut commands: Commands,
    settings: Res<Settings>,
    mut ambient: ResMut<AmbientController>,
    players: Query<(), With<AudioPlayer>>,
) {
    // Текущий трек ещё играет — не трогаем ничего.
    if let Some(entity) = ambient.current {
        if players.contains(entity) {
            return;
        }
        ambient.current = None;
        ambient.gap = Some(Timer::from_seconds(ambient.gap_secs, TimerMode::Once));
    }

    let Some(gap) = ambient.gap.as_mut() else {
        return;
    };
    gap.tick(time.delta());
    if !gap.is_finished() {
        return;
    }
    ambient.gap = None;

    if ambient.tracks.is_empty() {
        return;
    }

    // Не играем один и тот же трек дважды подряд: детерминированный
    // сдвиг по номеру запуска. Для одного трека даёт сам себя.
    // wrapping_add: стартовый last_index = usize::MAX переполнился бы
    // обычным сложением на первом же треке.
    let index = (ambient.last_index.wrapping_add(1)) % ambient.tracks.len();
    ambient.last_index = index;

    let handle = ambient.tracks[index].clone();
    let entity = commands
        .spawn((
            AudioPlayer::new(handle),
            PlaybackSettings {
                volume: volume_for(&settings, SoundBus::Ambient),
                ..PlaybackSettings::DESPAWN
            },
            Bus(SoundBus::Ambient),
        ))
        .id();
    ambient.current = Some(entity);
}

/// Применяет громкость к уже играющим звукам через `AudioSink`.
///
/// `PlaybackSettings` после старта воспроизведения не действует, поэтому
/// единственный способ изменить громкость «на лету» — AudioSink.
/// Обход всех плееров нужен только при изменении настроек: пока ползунок
/// не трогали, система выходит сразу.
pub fn apply_volumes(
    settings: Res<Settings>,
    mut sinks: Query<(&mut AudioSink, &Bus), With<AudioPlayer>>,
) {
    if !settings.is_changed() {
        return;
    }
    for (mut sink, bus) in &mut sinks {
        sink.set_volume(volume_for(&settings, bus.0));
    }
}

/// Ставит громкость при старте звука — на случай, если звук начнёт
/// играть между тиками `apply_volumes`.
pub fn set_initial_volume(
    mut players: Query<(&mut PlaybackSettings, &Bus), Added<AudioPlayer>>,
    settings: Res<Settings>,
) {
    for (mut playback, bus) in &mut players {
        playback.volume = volume_for(&settings, bus.0);
    }
}

/// Хендлы коротких звуков интерфейса. Грузятся на старте, чтобы клик
/// звучал сразу.
#[derive(Resource, Debug, Clone)]
pub struct UiSounds {
    pub click: Handle<AudioSource>,
    pub select: Handle<AudioSource>,
    pub slider: Handle<AudioSource>,
}

/// Загружает звуки UI из assets/sounds/menu.
pub fn load_ui_sounds(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(UiSounds {
        click: asset_server.load("sounds/menu/click_btn.wav"),
        select: asset_server.load("sounds/menu/select_btn.wav"),
        slider: asset_server.load("sounds/menu/slider_change.wav"),
    });
}

/// Гейт от звуковой каши: метки времени (elapsed, секунды), до которых
/// новая порция звуков категории блокируется. Пока играет звук меню,
/// следующий звук меню не запускается; честный трекинг сущностей дороже,
/// кулдаун по длине типового сэмпла даёт тот же эффект.
#[derive(Resource, Debug, Clone, Default)]
pub struct SoundGate {
    menu_ready_at: f32,
}

/// Кулдаун меню: частые события (тяга слайдера, ховер) режутся,
/// редкие клики проходят всегда (кулдаун короче интервала кликов).
pub const MENU_SOUND_COOLDOWN: f32 = 0.075;

/// Короткий звук интерфейса без гейта (клик по кнопке).
pub fn play_ui_sound(
    commands: &mut Commands,
    handle: Handle<AudioSource>,
    settings: &Settings,
) {
    commands.spawn((
        AudioPlayer::new(handle),
        PlaybackSettings {
            volume: volume_for(settings, SoundBus::Ui),
            ..PlaybackSettings::DESPAWN
        },
        Bus(SoundBus::Ui),
    ));
}

/// Звук меню с гейтом: вернул true — сыграло, false — заблокировано.
pub fn play_menu_sound(
    commands: &mut Commands,
    handle: Handle<AudioSource>,
    gate: &mut SoundGate,
    time: &Time,
    settings: &Settings,
) -> bool {
    let now = time.elapsed_secs();
    if now < gate.menu_ready_at {
        return false;
    }
    gate.menu_ready_at = now + MENU_SOUND_COOLDOWN;
    play_ui_sound(commands, handle, settings);
    true
}

/// Вход курсора на кнопки для звука выделения.
///
/// Нюанс движка: `Hovered` — bool-обёртка, которую пикинг INSERT'ит
/// один раз и дальше обновляет на месте, поэтому фронт `Added<Hovered>`
/// срабатывал бы ровно раз за жизнь кнопки. Переход ловим сравнением
/// с прошлым кадром: true, если хотя бы одна сущность из `hovered_now`
/// не была под курсором раньше. Множество сдвигается на текущий кадр
/// всегда (звать каждый кадр, даже если результат не нужен — иначе
/// прошлое «протухнет» и следующий ховер даст ложный фронт).
/// `clear` сохраняет capacity — аллокаций в установившемся режиме нет.
pub fn hover_just_entered(
    hovered_now: &[Entity],
    prev_hovered: &mut std::collections::HashSet<Entity>,
) -> bool {
    let mut entered = false;
    for e in hovered_now {
        if !prev_hovered.contains(e) {
            entered = true;
            break;
        }
    }
    prev_hovered.clear();
    prev_hovered.extend(hovered_now.iter().copied());
    entered
}

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundGate>()
            .add_systems(Startup, (load_ui_sounds, setup_ambient))
            .add_systems(Update, (ambient_system, apply_volumes, set_initial_volume));
    }
}
