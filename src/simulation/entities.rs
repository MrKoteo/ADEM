//! Существа симуляции: данные, состояния и простое поведение.
//!
//! Существо — самостоятельная сущность мира, а не свойство тайла
//! (GAME_DESIGN 3.2). Здесь хранятся лёгкие записи в `Vec`; Bevy-сущности
//! для рендера создаёт слой `rendering` только для видимых существ.
//!
//! Поведение волка — конечный автомат «стоять / идти / лежать». Между
//! ходьбой и лежанием всегда есть стояние. Решения принимаются в момент
//! смены состояния; случайность детерминирована seed'ом мира, id существа
//! и тиком, поэтому состояние ИИ не нужно сохранять отдельным RNG.

use std::collections::{HashMap, HashSet, VecDeque};

use super::chunk::Chunk;
use super::hunger::{Activity, Hunger};
use super::items::Inventory;
use super::movement;
use super::rng::{SplitMix64, mix_seed};
use super::spatial::SpatialGrid;
use super::tile::TileType;
use super::time::{TICKS_PER_SEC, TICKS_PER_YEAR};
use super::world::{World, WorldPos, split_tile, TilePos};

/// Радиус, в котором волк выбирает следующую точку брожения, тайлов.
/// Точка отсчитывается от текущей позиции, а не от «дома»: так животное
/// может постепенно мигрировать по карте, не будучи привязанным к месту.
const WANDER_RADIUS_TILES: f32 = 8.0;

/// Базовая скорость ходьбы волка, тайлов за тик (GAME_DESIGN 3.4).
/// Скорость задаётся отдельно для каждого вида, а модификаторы рельефа —
/// общие правила мира (`simulation/movement.rs`).
const WOLF_WALK_SPEED: f32 = 0.2;

/// Базовая скорость ходьбы кролика, тайлов за тик. Чуть медленнее волка:
/// хищник должен догонять убегающую добычу.
const RABBIT_WALK_SPEED: f32 = 0.18;

/// Максимальный запас здоровья кролика.
const RABBIT_MAX_HP: f32 = 40.0;

/// Радиус, в котором кролик замечает волка и убегает, тайлов.
pub(crate) const RABBIT_FLEE_RADIUS_TILES: f32 = 6.0;

/// Дальность убегания кролика от волка за одну цель, тайлов.
const RABBIT_FLEE_DISTANCE_TILES: f32 = 8.0;

/// Порог сытости, ниже которого кролик ищет траву и ест.
pub(crate) const RABBIT_GRAZE_BELOW: f32 = 80.0;

/// Сытость, восстанавливаемая одним поеданием травы.
pub(crate) const RABBIT_GRAZE_AMOUNT: f32 = 15.0;

/// Кулдаун поедания травы, тиков.
pub(crate) const RABBIT_GRAZE_COOLDOWN_TICKS: u64 = 50;

/// Радиус поиска травяного тайла голодным кроликом, тайлов.
const RABBIT_GRASS_SEARCH_TILES: i32 = 8;

/// Пауза перед повторным поиском травы, если рядом её не оказалось.
const RABBIT_GRASS_RETRY_TICKS: u64 = 2 * TICKS_PER_SEC as u64;

/// Длительности состояний, реальные секунды при x1.
const WALK_SECS: (u64, u64) = (3, 8);
const STAND_SECS: (u64, u64) = (1, 3);
const LIE_SECS: (u64, u64) = (5, 15);

/// Вероятность лечь при выборе из стояния. Ходьба — основное занятие,
/// лежание — редкое.
const LIE_CHANCE: f32 = 0.2;

/// Период смены кадров анимации ходьбы: 0.5 секунды.
const WALK_FRAME_TICKS: u64 = (TICKS_PER_SEC as u64) / 2;

/// Период смены кадров анимации ходьбы кролика: 0.25 секунды.
/// Прыжковые кадры короткие, поэтому листаются быстрее волчьих.
const RABBIT_WALK_FRAME_TICKS: u64 = (TICKS_PER_SEC as u64) / 4;

/// Число кадров ходьбы кролика (присед → толчок → полёт).
const RABBIT_WALK_FRAMES: u8 = 3;

/// Сколько тиков волк может «толкаться» в препятствие, прежде чем
/// прекратить ходьбу и перейти в стояние.
const STUCK_LIMIT_TICKS: u16 = 10;

/// Сколько тиков волк стоит сразу после появления.
const INITIAL_STAND_TICKS: u64 = TICKS_PER_SEC as u64;

/// Сколько тиков подряд волк может плыть, прежде чем бросить текущую цель
/// и кратчайшим путём пойти к ближайшей суше. Волк реки переходить умеет,
/// но не должен «жить» в воде.
const WATER_PANIC_TICKS: u16 = 3 * TICKS_PER_SEC as u16;

/// Предел обхода тайлов при поиске ближайшей суши: защита от бесконечного
/// BFS на огромной карте, целиком залитой водой.
const LAND_SEARCH_LIMIT: usize = 40_000;

/// Пауза между попытками поиска суши после неудачи (BFS упёрся в лимит
/// и не нашёл ни тайла земли): 5 секунд игрового времени. Без неё существо
/// в безбрежном океане повторяло бы полный обход 40 000 тайлов каждый тик.
const LAND_SEARCH_RETRY_TICKS: u64 = 5 * TICKS_PER_SEC as u64;

/// Соседи по 4 направлениям для поиска суши. Порядок фиксирован, поэтому
/// при равном расстоянии выбирается один и тот же берег (детерминизм).
const LAND_NEIGHBORS: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Урон укуса волка (решение автора: 8 здоровья) — и ответного, и
/// захваченного под контролем: это один и тот же прикус существа.
pub const WOLF_BITE_DAMAGE: f32 = 8.0;
/// Кулдаун укуса волка, тиков (общий для ИИ-куса и контроля).
pub(crate) const WOLF_BITE_COOLDOWN_TICKS: u64 = TICKS_PER_SEC as u64;
/// Дальность укуса волка, тайлов.
pub(crate) const WOLF_BITE_REACH_TILES: f32 = 1.4;
/// Сколько тиков волк помнит обиду: кусается и убегает, пока не забыто.
pub const WOLF_PROVOKE_TICKS: u64 = 5 * TICKS_PER_SEC as u64;
/// Дальность убегания от обидчика, тайлов.
const WOLF_FLEE_DISTANCE_TILES: f32 = 10.0;

/// Базовая скорость ходьбы человека, тайлов за тик (GAME_DESIGN 4.4:
/// 0.2 тайла/тик — как у волка; модификаторы рельефа — общие правила мира).
pub const HUMAN_WALK_SPEED: f32 = 0.2;

/// Максимальный запас здоровья человека.
pub const HUMAN_MAX_HP: f32 = 100.0;

/// Вид существа. Значения стабильны для сохранений; новые — только в конец.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EntityKind {
    Wolf,
    Human,
    Rabbit,
}

impl EntityKind {
    /// Все виды существ в стабильном порядке (генерация, UI редактора).
    pub const ALL: [EntityKind; 3] = [EntityKind::Wolf, EntityKind::Human, EntityKind::Rabbit];

    /// Номинальная продолжительность жизни вида в игровых годах.
    /// Фактическая при спавне отклоняется на ±10% (см. `SimEntity::new`).
    /// Значения сбалансированы под текущий календарь; правятся здесь.
    pub fn nominal_lifespan_years(self) -> u64 {
        match self {
            EntityKind::Wolf => 12,
            EntityKind::Human => 70,
            EntityKind::Rabbit => 6,
        }
    }

    /// Номинальная продолжительность жизни вида в тиках.
    pub fn nominal_lifespan_ticks(self) -> u64 {
        self.nominal_lifespan_years().saturating_mul(TICKS_PER_YEAR)
    }

    /// Максимальный запас здоровья вида.
    pub fn max_hp(self) -> f32 {
        match self {
            EntityKind::Wolf => 80.0,
            EntityKind::Human => HUMAN_MAX_HP,
            EntityKind::Rabbit => RABBIT_MAX_HP,
        }
    }

    /// Базовая скорость ходьбы вида, тайлов за тик. Замедление рельефом
    /// сюда не входит — его применяет `movement::effective_walk_speed`.
    pub fn walk_speed(self) -> f32 {
        match self {
            EntityKind::Wolf => WOLF_WALK_SPEED,
            EntityKind::Human => HUMAN_WALK_SPEED,
            EntityKind::Rabbit => RABBIT_WALK_SPEED,
        }
    }

    /// Разумен ли вид (есть ли «ум»: инвентарь, голод, автономное поведение).
    /// Разумные — только люди (GAME_DESIGN 4.4).
    pub fn is_sentient(self) -> bool {
        matches!(self, EntityKind::Human)
    }

    /// Число кадров анимации ходьбы. Рендер строит по этому значению
    /// текстуры кадров в атласе; `walk_frame` обязан возвращать индекс
    /// в этих пределах.
    pub fn walk_frame_count(self) -> u8 {
        match self {
            EntityKind::Wolf => 2,
            EntityKind::Rabbit => RABBIT_WALK_FRAMES,
            EntityKind::Human => 1,
        }
    }

    /// Вид существа по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Wolf),
            1 => Some(Self::Human),
            2 => Some(Self::Rabbit),
            _ => None,
        }
    }
}

/// Пол существа. Значения стабильны для сохранений; новые — только в конец.
/// Влияет пока только на будущую репродукцию (GAME_DESIGN 4.1), но уже
/// хранится и отображается в инспекторе.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Gender {
    Male,
    Female,
}

impl Gender {
    /// Пол по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Male),
            1 => Some(Self::Female),
            _ => None,
        }
    }

    /// Подпись для UI.
    pub fn label(self) -> &'static str {
        match self {
            Gender::Male => "мужской",
            Gender::Female => "женский",
        }
    }
}

/// «Ум» разумного существа: то, что делает человека человеком —
/// инвентарь и кулдауны действий. Живёт в `SimEntity.mind` только у
/// разумных видов (волк — `None`), поэтому неразумные существа не платят
/// памятью за механики, которых у них нет. Голод — общая потребность
/// существа, живёт на `SimEntity` (волк тоже голодает).
///
/// По целевому формату (GAME_DESIGN 4.1) игрок — это захваченная сущность:
/// UI отдаёт команды управляемому существу, а это хранилище даёт ему те же
/// возможности, что и раньше были у `Player` (ARCHITECTURE 2.7).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HumanMind {
    /// Инвентарь разумного существа: 8 предметов + 4 брони + 4 одежды
    /// + 4 косметики + слот-мусорка UI.
    pub inventory: Inventory,
    /// Выбранный слот хотбара (индекс в секции misc), 0..MISC_SLOTS.
    pub selected_slot: usize,
    /// Тик, с которого снова можно есть (кулдаун поедания). Transient:
    /// в сейв не пишется.
    pub eat_ready_tick: u64,
    /// Тик, с которого снова доступна атака в ближнем бою (кулдаун оружия).
    /// Атака без попадания кулдаун не списывает. Transient: в сейв не пишется.
    pub attack_ready_tick: u64,
    /// Текущая цель автономного поведения (ИИ выживания). У захваченного
    /// человека цель не используется — им управляет контроллер. Transient:
    /// в сейв не пишется (после загрузки человек сам выберет цель заново).
    pub goal: Option<HumanGoal>,
    /// Запомненный костёр племени (ИИ ходит и ищет, найдя — помнит).
    /// Transient: в сейв не пишется.
    pub known_campfire: Option<TilePos>,
    /// Запомненный тайл с едой (яблоня с плодами, зрелая культура).
    /// Transient: в сейв не пишется.
    pub known_food: Option<TilePos>,
    /// Тик следующего «обдумывания» ИИ (переоценка цели, скан окрестностей).
    /// Разрежает дорогие сканы объектов: между обдумываниями ИИ только
    /// исполняет текущую цель. Transient: в сейв не пишется.
    pub next_think_tick: u64,
}

/// Цель автономного поведения человека (ИИ выживания, simulation/ai.rs).
/// Данные цели — только то, что нужно для проверки её исполнения.
/// Transient: в сейв не пишется, после загрузки человек выберет цель заново.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HumanGoal {
    /// Ночь: дойти до костра племени и стоять рядом до утра.
    Campfire(TilePos),
    /// Дойти до тайла с едой (яблоня с плодами или зрелая культура)
    /// и собрать урожай в инвентарь.
    Food(TilePos),
    /// Охота: догнать животное и убить (мясо подберётся с земли).
    Hunt(u64),
    /// Сбор ресурсов с куста (палки): дойти и облупить куст.
    Gather(TilePos),
}

impl HumanMind {
    /// Новый ум с пустым инвентарём.
    pub fn new() -> Self {
        Self {
            inventory: Inventory::default(),
            selected_slot: 0,
            eat_ready_tick: 0,
            attack_ready_tick: 0,
            goal: None,
            known_campfire: None,
            known_food: None,
            next_think_tick: 0,
        }
    }

    /// Предмет в выбранном слоте хотбара, если там что-то лежит.
    pub fn selected_item(&self) -> Option<super::items::ItemKind> {
        let slot = self.selected_slot.min(super::items::MISC_SLOTS - 1);
        self.inventory.items[slot].map(|stack| stack.kind)
    }
}

/// Состояние поведения существа. Значения стабильны для сохранений.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EntityState {
    /// Стоит на месте. Промежуточное состояние между ходьбой и лежанием.
    Standing,
    /// Идёт к выбранной точке.
    Walking,
    /// Лежит на месте.
    Lying,
}

impl EntityState {
    /// Состояние по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Standing),
            1 => Some(Self::Walking),
            2 => Some(Self::Lying),
            _ => None,
        }
    }
}

/// Направление взгляда. Текстуры нарисованы смотрящими вправо, поэтому
/// при взгляде влево рендер зеркалит спрайт по горизонтали. Взгляд
/// меняется только при экранно-горизонтальном движении (см. `screen_facing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Facing {
    Right,
    Left,
}

impl Facing {
    /// Направление по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Right),
            1 => Some(Self::Left),
            _ => None,
        }
    }
}

/// Взгляд по экранной горизонтали движения/цели в изометрии:
/// экранное «вправо» — знак (dx − dy). `None` — движение экранно
/// вертикально (вверх/вниз): взгляд не меняется.
pub fn screen_facing(dx: f32, dy: f32) -> Option<Facing> {
    let sx = dx - dy;
    if sx.abs() > f32::EPSILON {
        Some(if sx > 0.0 { Facing::Right } else { Facing::Left })
    } else {
        None
    }
}

/// Существо мира: непрерывная позиция, состояние поведения и данные,
/// которые понадобятся будущим механикам (HP, инвентарь и т. п.).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SimEntity {
    /// Стабильный идентификатор в пределах мира (для планировщика и команд).
    pub id: u64,
    pub kind: EntityKind,
    /// Пол особи: случайный при спавне (детерминированно от id).
    pub gender: Gender,
    /// Прожитые тики. При спавне — случайная доля 20–80% от `lifetime_ticks`.
    pub age_ticks: u64,
    /// Индивидуальная продолжительность жизни в тиках: номинал вида ±10%.
    /// Достигнув её, существо умирает обычной смертью (с дропом).
    pub lifetime_ticks: u64,
    /// Позиция в тайлах (непрерывная).
    pub pos: WorldPos,
    /// Позиция на прошлом тике: рендер интерполирует между ней и `pos`.
    pub prev_pos: WorldPos,
    pub facing: Facing,
    pub state: EntityState,
    /// Текущее здоровье; максимум задаётся видом.
    pub hp: f32,
    /// Потребность голода — общая для всех видов (simulation/hunger.rs):
    /// волк тоже голодает и получает урон от голода. Еда для волка —
    /// будущая механика; пока его сытость только расходуется.
    pub hunger: Hunger,
    /// Тиков подряд, проведённых в воде. Сбрасывается на суше; после
    /// `WATER_PANIC_TICKS` волк идёт к ближайшему берегу.
    pub water_ticks: u16,
    /// Тиков, проведённых в текущем состоянии.
    pub state_ticks: u64,
    /// Полная длительность текущего состояния в тиках.
    pub state_duration: u64,
    /// Точка, к которой идёт существо (имеет смысл только в `Walking`).
    pub target: WorldPos,
    /// Тиков ходьбы без фактического смещения (упёрлось в препятствие).
    pub stuck_ticks: u16,
    /// Тик последней неудачи поиска суши (0 — неудач не было). До
    /// `LAND_SEARCH_RETRY_TICKS` после неё BFS не повторяется.
    pub land_search_failed_at: u64,
    /// «Ум» разумного существа: инвентарь, кулдауны. `None` —
    /// у неразумных видов (волк). Разумные — только люди (GAME_DESIGN 4.4).
    pub mind: Option<HumanMind>,
    /// Кулдаун ответного укуса волка. Transient: в сейв не пишется.
    pub next_bite_tick: u64,
    /// Кто последний раз ударил волка (ответный кусь и убегание).
    /// Transient: в сейв не пишется.
    pub provoked_by: Option<u64>,
    /// Тик, до которого волк помнит обиду. Transient: в сейв не пишется.
    pub provoked_until_tick: u64,
    /// Тик, с которого кролик снова может есть траву (кулдаун поедания).
    /// Transient: в сейв не пишется.
    pub next_graze_tick: u64,
}

impl SimEntity {
    /// Новое существо в состоянии «стоит» с полным здоровьем. Пол, возраст
    /// и продолжительность жизни разыгрываются детерминированно от id:
    /// поток не хранится и воспроизводится после загрузки/перегенерации.
    fn new(id: u64, kind: EntityKind, pos: WorldPos) -> Self {
        let mut rng = SplitMix64::new(mix_seed(id, 0xA6E0_0000_0000_0001));
        let gender = if rng.next_below(2) == 0 {
            Gender::Male
        } else {
            Gender::Female
        };
        // Номинал ±10%: особи одного вида стареют не синхронно.
        let nominal = kind.nominal_lifespan_ticks();
        let lifetime_ticks =
            ((nominal as f64 * (0.9 + 0.2 * rng.next_f32() as f64)) as u64).max(1);
        // Стартовый возраст — от 20% до 80% жизни (не новорождённые).
        let age_ticks = ((lifetime_ticks as f64 * (0.2 + 0.6 * rng.next_f32() as f64)) as u64)
            .min(lifetime_ticks.saturating_sub(1));
        Self {
            id,
            kind,
            gender,
            age_ticks,
            lifetime_ticks,
            pos,
            prev_pos: pos,
            facing: Facing::Right,
            state: EntityState::Standing,
            hp: kind.max_hp(),
            hunger: Hunger::new(),
            water_ticks: 0,
            state_ticks: 0,
            state_duration: INITIAL_STAND_TICKS,
            target: pos,
            stuck_ticks: 0,
            land_search_failed_at: 0,
            mind: kind.is_sentient().then(HumanMind::new),
            next_bite_tick: 0,
            provoked_by: None,
            provoked_until_tick: 0,
            next_graze_tick: 0,
        }
    }

    /// Базовая скорость ходьбы существа, тайлов за тик. Единая точка, через
    /// которую движение берёт скорость конкретной особи: позже сюда
    /// добавятся травмы, груз и прочие индивидуальные модификаторы.
    pub fn walk_speed(&self) -> f32 {
        self.kind.walk_speed()
    }

    /// Возраст в игровых годах (для UI).
    pub fn age_years(&self) -> u64 {
        self.age_ticks / TICKS_PER_YEAR
    }

    /// Индивидуальная продолжительность жизни в игровых годах (для UI).
    pub fn lifespan_years(&self) -> u64 {
        self.lifetime_ticks / TICKS_PER_YEAR
    }

    /// Кадр анимации ходьбы. У волка два кадра раз в `WALK_FRAME_TICKS`,
    /// у кролика три прыжковых кадра раз в `RABBIT_WALK_FRAME_TICKS`.
    pub fn walk_frame(&self) -> u8 {
        match self.kind {
            EntityKind::Wolf => ((self.state_ticks / WALK_FRAME_TICKS) % 2) as u8,
            EntityKind::Rabbit => {
                ((self.state_ticks / RABBIT_WALK_FRAME_TICKS) % RABBIT_WALK_FRAMES as u64) as u8
            }
            EntityKind::Human => 0,
        }
    }
}

/// Хранилище существ мира. `Vec` вместо `HashMap`: порядок обхода
/// детерминирован, а id→индекс кэшируется рядом (`index`) — `get/get_mut`
/// O(1), а не линейный поиск по тысячам существ. Индекс не входит в
/// сохранения и пересобирается при редких удалениях.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityStore {
    /// Существа в порядке создания (id возрастают).
    pub entities: Vec<SimEntity>,
    /// Следующий свободный идентификатор; 0 зарезервирован как «нет id».
    pub next_id: u64,
    /// id → позиция в `entities`. Поддерживается `spawn/remove`.
    index: HashMap<u64, usize>,
}

/// Переиспользуемые буферы поведения фауны: снимки позиций и
/// пространственные сетки. Лежат в `World` (не в `EntityStore`), чтобы не
/// попадать в сохранения и сравнения хранилища.
#[derive(Default)]
pub(crate) struct FaunaScratch {
    /// Кролики текущего тика (id, позиция) — для укусов и сетки.
    pub(crate) rabbits: Vec<(u64, WorldPos)>,
    /// Голодные волки, которые ищут добычу.
    pub(crate) hungry: Vec<u64>,
    /// Сетка кроликов: волк ищет ближайшего за константное число ячеек.
    pub(crate) rabbit_grid: SpatialGrid,
    /// Сетка волков: кролик ищет ближайшую угрозу.
    pub(crate) wolf_grid: SpatialGrid,
    /// Сетка мяса на земле: цель голодного волка.
    pub(crate) meat_grid: SpatialGrid,
}

/// Переиспользуемые буферы BFS поиска суши: `HashSet` и очередь с
/// сохранённой ёмкостью. Раньше каждый вызов `nearest_land` аллоцировал
/// их заново — на существо в воде и каждом повторе поиска.
#[derive(Default)]
pub(crate) struct LandSearchScratch {
    visited: HashSet<(i32, i32)>,
    queue: VecDeque<(i32, i32)>,
}

impl EntityStore {
    /// Создаёт хранилище из готового списка (загрузка сохранения):
    /// индекс id→позиция строится здесь, а не читается из файла.
    pub(crate) fn from_entities(entities: Vec<SimEntity>, next_id: u64) -> Self {
        let mut store = Self {
            entities,
            next_id,
            index: HashMap::new(),
        };
        store.rebuild_index();
        store
    }

    /// Перестраивает кэш id→индекс после удаления (индексы сдвигаются).
    fn rebuild_index(&mut self) {
        self.index.clear();
        self.index.reserve(self.entities.len());
        for (position, entity) in self.entities.iter().enumerate() {
            self.index.insert(entity.id, position);
        }
    }

    /// Создаёт существо вида `kind` в позиции `pos` и возвращает его id.
    pub fn spawn(&mut self, kind: EntityKind, pos: WorldPos) -> u64 {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.index.insert(id, self.entities.len());
        self.entities.push(SimEntity::new(id, kind, pos));
        id
    }

    /// Убирает существо по id (инструмент редактора). `true` — существо
    /// было и удалено; id существ не переиспользуются, поэтому старые
    /// ссылки на удалённое существо просто перестают находиться.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(position) = self.index.get(&id).copied() else {
            return false;
        };
        self.entities.remove(position);
        // Удаления редки (смерть/редактор): пересборка индекса O(n) дешевле,
        // чем сдвиг всех последующих записей на каждом удалении.
        self.rebuild_index();
        true
    }

    /// Существо по id — O(1) по кэшу индекса.
    pub fn get(&self, id: u64) -> Option<&SimEntity> {
        let position = *self.index.get(&id)?;
        self.entities.get(position)
    }

    /// Существо по id (мутабельно) — O(1) по кэшу индекса.
    pub fn get_mut(&mut self, id: u64) -> Option<&mut SimEntity> {
        let position = *self.index.get(&id)?;
        self.entities.get_mut(position)
    }

    /// Переносит существо в точку (перетаскивание колёсиком в редакторе).
    /// `prev_pos` выравнивается с `pos`: иначе рендер плавно догонял бы
    /// старое место и существо «уезжало» от курсора. Состояние сбрасывается
    /// в стояние, чтобы после освобождения существо не продолжило путь к
    /// прежней цели. `false` — существа с таким id нет.
    pub fn move_to(&mut self, id: u64, pos: WorldPos) -> bool {
        let Some(entity) = self.get_mut(id) else {
            return false;
        };
        entity.pos = pos;
        entity.prev_pos = pos;
        entity.target = pos;
        entity.state = EntityState::Standing;
        entity.state_ticks = 0;
        entity.state_duration = INITIAL_STAND_TICKS;
        entity.stuck_ticks = 0;
        true
    }

    /// Итератор по существам в стабильном порядке.
    pub fn iter(&self) -> impl Iterator<Item = &SimEntity> {
        self.entities.iter()
    }

    /// Количество существ.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Продвигает поведение всех существ на один тик.
    ///
    /// Обход существ, а не тайлов мира: спящие/стоящие существа почти
    /// ничего не делают, а ходящие обязаны двигаться каждый тик. Решения
    /// принимаются только в момент смены состояния.
    ///
    /// `skip` — захваченное существо контроллера: весь FSM его не ведёт
    /// (куда идти и когда стоять — решает игрок через `step_direction`,
    /// который сам ставит `prev_pos` для интерполяции). ИИ людей его
    /// пропускает по тому же правилу. Голод захваченного тикает на
    /// app-слое (`actions::tick_hunger`), поэтому здесь он тоже пропущен.
    ///
    /// Умершие от голода собираются в `dead` — вызывающая сторона
    /// (`World::tick_entities`) исполняет смерть с дропом лута.
    fn update(
        &mut self,
        seed: u64,
        tick: u64,
        terrain: &TerrainView<'_>,
        skip: Option<u64>,
        land_scratch: &mut LandSearchScratch,
        dead: &mut Vec<u64>,
    ) {
        let side = terrain.side as f32;
        for entity in &mut self.entities {
            // Возраст тикает у всех, включая захваченное существо. Смерть
            // от старости — та же смерть, что от голода: дроп и удаление
            // исполняет вызывающая сторона после обхода.
            entity.age_ticks = entity.age_ticks.saturating_add(1);
            if entity.age_ticks >= entity.lifetime_ticks {
                dead.push(entity.id);
                continue;
            }
            // Захваченное существо пропускается до всяких мутаций: его
            // `prev_pos` уже выставил `step_direction` на этом тике.
            if Some(entity.id) == skip {
                continue;
            }
            entity.prev_pos = entity.pos;

            // Время в воде копится только подряд: на суше счётчик сброшен.
            if terrain.is_water(entity.pos) {
                entity.water_ticks = entity.water_ticks.saturating_add(1);
            } else {
                entity.water_ticks = 0;
            }

            // Голод — общая потребность всех видов: расход по активности,
            // урон при пустой сытости. Смерть от голода исполняет
            // вызывающая сторона (нужен мир для дропа лута).
            let activity = if terrain.is_water(entity.pos) {
                Activity::Swimming
            } else if entity.state == EntityState::Walking {
                Activity::Walking
            } else {
                Activity::Idle
            };
            let damage = entity.hunger.tick(activity);
            if damage > 0.0 {
                entity.hp -= damage;
            }
            // Сытый организм подлечивается (полное HP — скип внутри).
            entity.hunger.regen(&mut entity.hp, entity.kind.max_hp());
            if entity.hp <= 0.0 {
                dead.push(entity.id);
                continue;
            }

            // В воде нельзя стоять или лежать: сразу идём к ближайшей суше.
            // Так волк может переплыть реку, но не остаётся в ней отдыхать.
            // Если поиск суши недавно провалился (океан без берегов), BFS
            // не повторяется до конца паузы: существо живёт в прежнем
            // состоянии, не сжигая обход тайлов каждый тик.
            if !land_search_on_cooldown(entity, tick) {
                if entity.state != EntityState::Walking && terrain.is_water(entity.pos) {
                    enter_escape(entity, terrain, tick, land_scratch);
                } else if entity.water_ticks >= WATER_PANIC_TICKS
                    && !terrain.is_land(entity.target)
                {
                    // Плывём слишком долго — бросаем прежнюю цель и
                    // кратчайшим путём выходим на берег.
                    enter_escape(entity, terrain, tick, land_scratch);
                }
            }

            match entity.state {
                EntityState::Standing | EntityState::Lying => {
                    entity.state_ticks += 1;
                    if entity.state_ticks < entity.state_duration {
                        continue;
                    }
                    // Стояние — единственный «узловой» узел: из него можно
                    // пойти или лечь, но не наоборот.
                    let mut rng = decision_rng(seed, entity.id, tick);
                    if entity.state == EntityState::Standing {
                        choose_from_standing(entity, &mut rng, terrain, side, tick, land_scratch);
                    } else {
                        enter_standing(entity, &mut rng);
                    }
                }
                EntityState::Walking => {
                    entity.state_ticks += 1;
                    let moved = step_towards(entity, terrain, side);
                    if moved {
                        entity.stuck_ticks = 0;
                    } else {
                        entity.stuck_ticks = entity.stuck_ticks.saturating_add(1);
                    }

                    let dx = entity.target.x - entity.pos.x;
                    let dy = entity.target.y - entity.pos.y;
                    let reached = dx.abs() + dy.abs() <= f32::EPSILON;
                    if reached
                        || entity.stuck_ticks >= STUCK_LIMIT_TICKS
                        || entity.state_ticks >= entity.state_duration
                    {
                        let mut rng = decision_rng(seed, entity.id, tick);
                        // Дошли до цели в воде — не встаём там, а идём на сушу.
                        // На кулдауне поиска состояние не меняем: существо
                        // плывёт к прежней цели, пока BFS можно повторить.
                        if terrain.is_water(entity.pos) {
                            if !land_search_on_cooldown(entity, tick) {
                                enter_escape(entity, terrain, tick, land_scratch);
                            }
                        } else {
                            enter_standing(entity, &mut rng);
                        }
                    }
                }
            }
        }
    }
}

impl World {
    /// Продвигает поведение существ мира на один тик.
    ///
    /// Метод на `World` нужен, чтобы одновременно взять `entities` мутабельно,
    /// а чанки — только для чтения: существа не меняют рельеф.
    /// `player_controlled` — существо, которым управляет контроллер (режим
    /// игрока; при включённом ИИ захваченного — `None`): FSM-автоматика
    /// (брожение, лежание) его не ведёт, ИИ тоже — движение и голод
    /// обеспечивает контроллер.
    pub fn tick_entities(&mut self, tick: u64, player_controlled: Option<u64>) {
        // Буфер умерших переиспользуется между тиками: пустой Vec на каждом
        // тике — лишняя аллокация при тысячах существ.
        let mut dead = std::mem::take(&mut self.entity_scratch.dead);
        dead.clear();
        {
            let World {
                entities,
                chunks,
                chunks_per_side,
                size,
                seed,
                land_scratch,
                ..
            } = self;
            let terrain = TerrainView {
                chunks,
                chunks_per_side: *chunks_per_side,
                side: size.tiles() as i32,
            };
            entities.update(
                *seed,
                tick,
                &terrain,
                player_controlled,
                land_scratch,
                &mut dead,
            );
        }
        // Смерти от голода: лут на землю, существо из мира (нужен мир,
        // поэтому не внутри `update`).
        for &id in &dead {
            super::combat::kill_entity(self, id, tick);
        }
        dead.clear();
        self.entity_scratch.dead = dead;
        // Ответный кусь и убегание волка — после FSM: провокация
        // переопределяет цель обычного поведения.
        self.tick_wolf_provocations(tick);
        // Фауна: охота волков на кроликов, побег и выпас кроликов.
        self.tick_fauna(tick, player_controlled);
        // Автономное поведение людей — после FSM: ИИ выставляет цели,
        // движение исполняется общим кодом на следующем тике.
        self.tick_human_ai(player_controlled, tick);
    }

    /// Поведение животных после общего FSM.
    ///
    /// Снимки позиций волков и кроликов берутся один раз за тик: решения
    /// принимаются по одному состоянию мира, без повторных сканов существ.
    /// Стоимость — O(волки × кролики) в худшем случае; при текущих лимитах
    /// спавна это дешевле, чем отдельные проходы на каждую механику.
    ///
    /// Волк: голодный (ниже `WOLF_SEEK_BELOW`), не спровоцированный и не
    /// лежащий кусает кролика в досягаемости, иначе идёт к мясу (мясо
    /// важнее), иначе — к ближайшему кролику в радиусе поиска.
    /// Кролик: убегает от волка в радиусе `RABBIT_FLEE_RADIUS_TILES`,
    /// иначе при голоде ест траву под собой или идёт к ближайшей траве.
    fn tick_fauna(&mut self, tick: u64, player_controlled: Option<u64>) {
        use super::item_entity::{WOLF_MEAT_SEEK_RADIUS, WOLF_SEEK_BELOW};

        let mut fauna = std::mem::take(&mut self.fauna_scratch);
        fauna.rabbits.clear();
        fauna.hungry.clear();
        fauna.rabbit_grid.clear();
        fauna.wolf_grid.clear();
        fauna.meat_grid.clear();
        for entity in self.entities.iter() {
            match entity.kind {
                EntityKind::Rabbit => {
                    fauna.rabbits.push((entity.id, entity.pos));
                    fauna.rabbit_grid.insert(entity.id, entity.pos);
                }
                EntityKind::Wolf => {
                    fauna.wolf_grid.insert(entity.id, entity.pos);
                    // Голодный неспровоцированный волк ищет добычу.
                    if entity.state != EntityState::Lying
                        && entity.provoked_by.is_none()
                        && Some(entity.id) != player_controlled
                        && entity.hunger.value() < WOLF_SEEK_BELOW
                    {
                        fauna.hungry.push(entity.id);
                    }
                }
                EntityKind::Human => {}
            }
        }

        // --- Волки: охота на кроликов и поиск мяса. ---
        if !fauna.hungry.is_empty() {
            for item in self.item_entities.items.iter() {
                if item.stack.kind.is_wolf_food() && item.fly.is_none() {
                    fauna.meat_grid.insert(item.id, item.pos);
                }
            }
            for index in 0..fauna.hungry.len() {
                let wolf_id = fauna.hungry[index];
                let Some(wolf) = self.entities.get(wolf_id) else {
                    continue;
                };
                let pos = wolf.pos;
                let bite_ready = tick >= wolf.next_bite_tick;

                // Ближайший кролик в радиусе охоты (детерминированный выбор).
                let nearest_rabbit = fauna.rabbit_grid.nearest(pos, WOLF_MEAT_SEEK_RADIUS);

                // Добыча в досягаемости — кусь по кулдауну.
                if let Some((rabbit_id, rabbit_pos)) = nearest_rabbit
                    && bite_ready
                    && pos.dist(rabbit_pos) <= WOLF_BITE_REACH_TILES
                {
                    let bitten = super::combat::wolf_bite_entity(self, wolf_id, rabbit_id, tick);
                    if bitten {
                        // Убитый кролик выбывает из сетки: остальные волки
                        // не должны гнаться за трупом.
                        if self.entities.get(rabbit_id).is_none() {
                            fauna.rabbit_grid.remove(rabbit_id);
                            fauna.rabbits.retain(|(id, _)| *id != rabbit_id);
                        }
                        continue;
                    }
                }

                // Цель: мясо важнее живой добычи, иначе ближайший кролик.
                let target = fauna
                    .meat_grid
                    .nearest(pos, WOLF_MEAT_SEEK_RADIUS)
                    .map(|(_, meat)| meat)
                    .or_else(|| nearest_rabbit.map(|(_, rabbit_pos)| rabbit_pos));
                let Some(target) = target else {
                    continue;
                };
                let Some(wolf) = self.entities.get_mut(wolf_id) else {
                    continue;
                };
                if wolf.state == EntityState::Lying {
                    continue;
                }
                // Цель обновляется каждый тик, но анимация ходьбы не
                // сбрасывается: `state_ticks` не трогаем.
                set_walk_target(wolf, target, 10 * TICKS_PER_SEC as u64);
            }
        }

        // --- Кролики: побег от волков, затем выпас. ---
        let side = self.size.tiles() as f32;
        for index in 0..fauna.rabbits.len() {
            let (rabbit_id, pos) = fauna.rabbits[index];
            if Some(rabbit_id) == player_controlled {
                continue;
            }
            let Some(rabbit) = self.entities.get(rabbit_id) else {
                continue;
            };
            let state = rabbit.state;
            let hunger = rabbit.hunger.value();
            let next_graze_tick = rabbit.next_graze_tick;
            let current_target = rabbit.target;

            // Ближайший волк в радиусе побега.
            let threat = fauna
                .wolf_grid
                .nearest(pos, RABBIT_FLEE_RADIUS_TILES)
                .map(|(_, wolf_pos)| wolf_pos);
            if let Some(wolf_pos) = threat {
                let dx = pos.x - wolf_pos.x;
                let dy = pos.y - wolf_pos.y;
                let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
                let flee = WorldPos {
                    x: (pos.x + dx / len * RABBIT_FLEE_DISTANCE_TILES).clamp(0.0, side),
                    y: (pos.y + dy / len * RABBIT_FLEE_DISTANCE_TILES).clamp(0.0, side),
                };
                let Some(rabbit) = self.entities.get_mut(rabbit_id) else {
                    continue;
                };
                // Лежание → стояние → ходьба: прямой переход запрещён.
                if rabbit.state == EntityState::Lying {
                    stand_up_for_action(rabbit);
                    continue;
                }
                set_walk_target(rabbit, flee, 10 * TICKS_PER_SEC as u64);
                continue;
            }

            if hunger >= RABBIT_GRAZE_BELOW || tick < next_graze_tick {
                continue;
            }

            // На траве — едим, не меняя рельеф. Кулдаун не даёт
            // восстанавливать сытость каждый тик.
            if is_grass(super::actions::terrain_at(self, pos)) {
                let Some(rabbit) = self.entities.get_mut(rabbit_id) else {
                    continue;
                };
                rabbit.hunger.eat(RABBIT_GRAZE_AMOUNT);
                rabbit.next_graze_tick = tick + RABBIT_GRAZE_COOLDOWN_TICKS;
                if rabbit.state == EntityState::Walking {
                    rabbit.state = EntityState::Standing;
                    rabbit.state_ticks = 0;
                    rabbit.state_duration = TICKS_PER_SEC as u64;
                    rabbit.target = rabbit.pos;
                    rabbit.stuck_ticks = 0;
                }
                continue;
            }

            // Уже идёт к травяному тайлу — не пересканируем окрестности
            // каждый тик.
            if state == EntityState::Walking
                && is_grass(super::actions::terrain_at(self, current_target))
            {
                continue;
            }

            match self.nearest_grass(pos) {
                Some(target) => {
                    let Some(rabbit) = self.entities.get_mut(rabbit_id) else {
                        continue;
                    };
                    if rabbit.state == EntityState::Lying {
                        stand_up_for_action(rabbit);
                        continue;
                    }
                    set_walk_target(rabbit, target, 10 * TICKS_PER_SEC as u64);
                }
                None => {
                    // Травы рядом нет: не сканируем окрестности каждый тик.
                    let Some(rabbit) = self.entities.get_mut(rabbit_id) else {
                        continue;
                    };
                    rabbit.next_graze_tick = tick + RABBIT_GRASS_RETRY_TICKS;
                }
            }
        }
        // Буферы (вместе с ёмкостью сеток) возвращаются в мир.
        self.fauna_scratch = fauna;
    }

    /// Ближайший травяной тайл в радиусе `RABBIT_GRASS_SEARCH_TILES`.
    /// Обход квадрата вокруг кролика; при равном расстоянии выбирается
    /// меньший `(y, x)` — выбор детерминирован.
    fn nearest_grass(&self, from: WorldPos) -> Option<WorldPos> {
        let side = self.size.tiles() as i32;
        let cx = (from.x.floor() as i32).clamp(0, side - 1);
        let cy = (from.y.floor() as i32).clamp(0, side - 1);
        let radius = RABBIT_GRASS_SEARCH_TILES;
        let mut best: Option<(f32, i32, i32)> = None;
        for y in (cy - radius).max(0)..=(cy + radius).min(side - 1) {
            for x in (cx - radius).max(0)..=(cx + radius).min(side - 1) {
                let tile = TilePos { x, y };
                if !is_grass(Some(self.terrain_kind(tile))) {
                    continue;
                }
                let center = WorldPos::of_tile_center(tile);
                let distance = from.dist(center);
                let better = best.is_none_or(|(best_distance, best_y, best_x)| {
                    distance < best_distance
                        || (distance == best_distance && (y, x) < (best_y, best_x))
                });
                if better {
                    best = Some((distance, y, x));
                }
            }
        }
        best.map(|(_, y, x)| WorldPos::of_tile_center(TilePos { x, y }))
    }

    /// Поведение провоцированного волка: кусает обидчика в досягаемости
    /// (по кулдауну), иначе убегает на `WOLF_FLEE_DISTANCE_TILES` прочь.
    /// Вызывается после FSM — цель и состояние провокации сильнее брожения.
    /// Обидчик мёртв или обида забылась — волк спокоен.
    fn tick_wolf_provocations(&mut self, tick: u64) {
        // Снимки позиций до мутаций: заём сущности не должен мешать
        // менять состояние волка. Буферы переиспользуются между тиками.
        let mut positions = std::mem::take(&mut self.ai_scratch.positions);
        positions.clear();
        positions.extend(self.entities.iter().map(|e| (e.id, e.pos)));
        let mut provoked = std::mem::take(&mut self.ai_scratch.provoked);
        provoked.clear();
        provoked.extend(
            self.entities
                .iter()
                .filter(|e| e.kind == EntityKind::Wolf && e.provoked_by.is_some())
                .map(|e| e.id),
        );
        let mut killed = std::mem::take(&mut self.ai_scratch.killed);
        killed.clear();
        for &id in &provoked {
            let Some(entity) = self.entities.get_mut(id) else {
                continue;
            };
            if tick >= entity.provoked_until_tick {
                // Обида забылась.
                entity.provoked_by = None;
                continue;
            }
            let Some(attacker) = entity.provoked_by else {
                continue;
            };
            let Some(attacker_pos) = positions
                .iter()
                .find(|(aid, _)| *aid == attacker)
                .map(|(_, pos)| *pos)
            else {
                // Обидчика нет в мире (убит/убран) — обида бесполезна.
                entity.provoked_by = None;
                continue;
            };
            let (pos, attacker_pos) = (entity.pos, attacker_pos);
            let distance = pos.dist(attacker_pos);
            if distance <= WOLF_BITE_REACH_TILES {
                // В досягаемости: стоим лицом к обидчику и кусаем по кулдауну.
                if let Some(facing) =
                    screen_facing(attacker_pos.x - pos.x, attacker_pos.y - pos.y)
                {
                    entity.facing = facing;
                }
                if entity.state != EntityState::Standing {
                    entity.state = EntityState::Standing;
                    entity.state_ticks = 0;
                    entity.state_duration = u64::MAX;
                    entity.target = entity.pos;
                    entity.stuck_ticks = 0;
                }
                if tick >= entity.next_bite_tick {
                    entity.next_bite_tick = tick + WOLF_BITE_COOLDOWN_TICKS;
                    // Обида продлевается, пока волк дерётся.
                    entity.provoked_until_tick = tick + WOLF_PROVOKE_TICKS;
                    let Some(victim) = self.entities.get_mut(attacker) else {
                        continue;
                    };
                    victim.hp = (victim.hp - WOLF_BITE_DAMAGE).max(0.0);
                    if victim.hp <= 0.0 {
                        killed.push(attacker);
                    }
                }
            } else {
                // Убегание: цель — прочь от обидчика на заданное расстояние.
                let dx = pos.x - attacker_pos.x;
                let dy = pos.y - attacker_pos.y;
                let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
                let side = self.size.tiles() as f32;
                let flee = WorldPos {
                    x: (pos.x + dx / len * WOLF_FLEE_DISTANCE_TILES).clamp(0.0, side),
                    y: (pos.y + dy / len * WOLF_FLEE_DISTANCE_TILES).clamp(0.0, side),
                };
                entity.state = EntityState::Walking;
                entity.state_ticks = 0;
                entity.state_duration = 10 * TICKS_PER_SEC as u64;
                entity.target = flee;
                entity.stuck_ticks = 0;
                if let Some(facing) = screen_facing(flee.x - pos.x, flee.y - pos.y) {
                    entity.facing = facing;
                }
            }
        }
        // Обидчик, забитый насмерть: лут и удаление — как в бою.
        for &id in &killed {
            super::combat::kill_entity(self, id, tick);
        }
        // Буферы возвращаются в мир с сохранённой ёмкостью.
        positions.clear();
        provoked.clear();
        killed.clear();
        self.ai_scratch.positions = positions;
        self.ai_scratch.provoked = provoked;
        self.ai_scratch.killed = killed;
    }
}

/// Выбор следующего состояния из стояния: обычно идти, редко лежать.
fn choose_from_standing(
    entity: &mut SimEntity,
    rng: &mut SplitMix64,
    terrain: &TerrainView<'_>,
    side: f32,
    tick: u64,
    land_scratch: &mut LandSearchScratch,
) {
    // Защита от рассинхрона: стоять в воде нельзя даже один цикл.
    // На кулдауне поиска суши состояние сохраняем — BFS повторится позже.
    if terrain.is_water(entity.pos) && !land_search_on_cooldown(entity, tick) {
        enter_escape(entity, terrain, tick, land_scratch);
        return;
    }
    // Поведением человека ведает ИИ (simulation/ai.rs): из стояния FSM
    // ничего не решает. Стояние не сбрасывается: пока `state_ticks`
    // растёт past длительности, условие брожения ИИ (`state_ticks >=
    // state_duration`) становится достижимым — раньше вечный сброс
    // стояния делал брожение недостижимым, и люди стояли на месте.
    if entity.kind.is_sentient() {
        return;
    }
    if rng.next_f32() < LIE_CHANCE {
        enter_lying(entity, rng);
    } else {
        enter_walking(entity, rng, terrain, side);
    }
}

/// Направляет существо к точке, не сбрасывая анимацию, если оно уже идёт.
/// Нужно для погони/побега: цель обновляется каждый тик, а `state_ticks`
/// продолжает листать кадры ходьбы.
fn set_walk_target(entity: &mut SimEntity, target: WorldPos, duration: u64) {
    if entity.state != EntityState::Walking {
        entity.state = EntityState::Walking;
        entity.state_ticks = 0;
    }
    entity.state_duration = duration;
    entity.target = target;
    entity.stuck_ticks = 0;
    if let Some(facing) = screen_facing(target.x - entity.pos.x, target.y - entity.pos.y) {
        entity.facing = facing;
    }
}

/// Поднимает лежащее существо в стояние перед действием. Небольшая
/// длительность не даёт FSM на следующем тике случайно лечь снова, но и
/// не задерживает действие: `tick_fauna` сразу задаст цель. Прямой переход
/// «лежание → ходьба» запрещён.
fn stand_up_for_action(entity: &mut SimEntity) {
    entity.state = EntityState::Standing;
    entity.state_ticks = 0;
    entity.state_duration = TICKS_PER_SEC as u64;
    entity.target = entity.pos;
    entity.stuck_ticks = 0;
}

/// Трава ли тайл (обычная или лесная) — корм кролика.
fn is_grass(kind: Option<TileType>) -> bool {
    matches!(kind, Some(TileType::Grass | TileType::ForestGrass))
}

/// Переводит существо в ходьбу и выбирает достижимую точку брожения.
/// Если подходящей точки нет (например, всё вокруг — глубокая вода),
/// существо остаётся стоять ещё один цикл.
fn enter_walking(
    entity: &mut SimEntity,
    rng: &mut SplitMix64,
    terrain: &TerrainView<'_>,
    side: f32,
) {
    let Some(target) = pick_target(rng, entity.pos, terrain, side) else {
        enter_standing(entity, rng);
        return;
    };
    entity.state = EntityState::Walking;
    entity.state_ticks = 0;
    entity.state_duration = random_duration(rng, WALK_SECS);
    entity.target = target;
    entity.stuck_ticks = 0;
}

/// Переводит существо в стояние.
fn enter_standing(entity: &mut SimEntity, rng: &mut SplitMix64) {
    entity.state = EntityState::Standing;
    entity.state_ticks = 0;
    entity.state_duration = random_duration(rng, STAND_SECS);
    entity.stuck_ticks = 0;
}

/// Переводит существо в лежание.
fn enter_lying(entity: &mut SimEntity, rng: &mut SplitMix64) {
    entity.state = EntityState::Lying;
    entity.state_ticks = 0;
    entity.state_duration = random_duration(rng, LIE_SECS);
    entity.stuck_ticks = 0;
}

/// Переводит существо в ходьбу к ближайшей суше. Длительность берётся
/// с запасом на весь путь плюс секунда, чтобы состояние не закончилось
/// раньше берега. Возвращает `false`, если суши в пределах поиска нет
/// (неудача запоминается: до `LAND_SEARCH_RETRY_TICKS` BFS не повторяется).
fn enter_escape(
    entity: &mut SimEntity,
    terrain: &TerrainView<'_>,
    tick: u64,
    land_scratch: &mut LandSearchScratch,
) -> bool {
    let Some(target) = terrain.nearest_land(entity.pos, land_scratch) else {
        entity.land_search_failed_at = tick;
        return false;
    };
    let dx = target.x - entity.pos.x;
    let dy = target.y - entity.pos.y;
    let distance = (dx * dx + dy * dy).sqrt();
    // Оценка пути — по собственной скорости существа с учётом рельефа
    // под ним, чтобы длительность не закончилась раньше берега.
    let speed = movement::effective_walk_speed(
        entity.walk_speed(),
        terrain.terrain_at(entity.pos),
    );
    entity.state = EntityState::Walking;
    entity.state_ticks = 0;
    entity.state_duration =
        (distance / speed).ceil() as u64 + TICKS_PER_SEC as u64;
    entity.target = target;
    entity.stuck_ticks = 0;
    true
}

/// Идёт ли отсрочка повторного поиска суши после недавней неудачи.
fn land_search_on_cooldown(entity: &SimEntity, tick: u64) -> bool {
    entity.land_search_failed_at != 0
        && tick.wrapping_sub(entity.land_search_failed_at) < LAND_SEARCH_RETRY_TICKS
}

/// Случайная длительность состояния в тиках из диапазона секунд.
fn random_duration(rng: &mut SplitMix64, secs: (u64, u64)) -> u64 {
    let min = secs.0 * TICKS_PER_SEC as u64;
    let max = secs.1 * TICKS_PER_SEC as u64;
    rng.next_range_inclusive(min, max)
}

/// Выбирает точку в радиусе брожения, лежащую на проходимом тайле, до
/// которой по прямой не лежит глубокая вода (лёгкая проверка пути: рёбра
/// и мелководье не мешают, глубокие озёра и океан отбрасывают цель).
/// Сначала ищет сушу; вода — только запасной вариант, чтобы волк мог
/// перейти реку, если вокруг неё всё остальное тоже вода или глубоко.
/// `None` — подходящей точки не нашли.
fn pick_target(
    rng: &mut SplitMix64,
    from: WorldPos,
    terrain: &TerrainView<'_>,
    side: f32,
) -> Option<WorldPos> {
    let mut water_fallback = None;
    for _ in 0..16 {
        let angle = rng.next_f32() * std::f32::consts::TAU;
        // sqrt делает выбор равномерным по площади круга, а не по радиусу.
        let radius = WANDER_RADIUS_TILES * rng.next_f32().sqrt();
        let pos = WorldPos {
            x: (from.x + angle.cos() * radius).clamp(0.0, side),
            y: (from.y + angle.sin() * radius).clamp(0.0, side),
        };
        if !terrain.passable(pos) {
            continue;
        }
        if terrain.line_avoids_deep_water(from, pos) {
            if !terrain.is_water(pos) {
                return Some(pos);
            }
            water_fallback.get_or_insert(pos);
        }
    }
    water_fallback
}

/// Шаг к цели с проверкой проходимости по каждой оси отдельно:
/// упёршись в воду, существо скользит вдоль берега. Возвращает, было ли
/// фактическое смещение.
fn step_towards(entity: &mut SimEntity, terrain: &TerrainView<'_>, side: f32) -> bool {
    let dx = entity.target.x - entity.pos.x;
    let dy = entity.target.y - entity.pos.y;
    let distance = (dx * dx + dy * dy).sqrt();
    if distance <= f32::EPSILON {
        return false;
    }

    // Взгляд следует за экранно-горизонтальным направлением движения; при
    // движении по экранной вертикали существо сохраняет прежний взгляд.
    if let Some(facing) = screen_facing(dx, dy) {
        entity.facing = facing;
    }

    // Скорость — собственная скорость существа; замедление рельефом —
    // общее правило мира, как у игрока.
    let speed = movement::effective_walk_speed(
        entity.walk_speed(),
        terrain.terrain_at(entity.pos),
    );
    let step = speed.min(distance);
    let next_x = entity.pos.x + dx / distance * step;
    let next_y = entity.pos.y + dy / distance * step;
    let before = entity.pos;

    let try_x = WorldPos { x: next_x.clamp(0.0, side), y: entity.pos.y };
    if terrain.passable(try_x) {
        entity.pos = try_x;
    }
    let try_y = WorldPos { x: entity.pos.x, y: next_y.clamp(0.0, side) };
    if terrain.passable(try_y) {
        entity.pos = try_y;
    }

    (entity.pos.x - before.x).abs() + (entity.pos.y - before.y).abs() > 0.0001
}

/// Детерминированный seed решения: seed мира + id существа + тик.
/// Одно решение на существо за тик, поэтому пары (id, tick) достаточно,
/// и состояние RNG не нужно хранить в сохранении.
fn decision_seed(world_seed: u64, entity_id: u64, tick: u64) -> u64 {
    let mut x = world_seed
        ^ entity_id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ tick.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Новый RNG для одного решения существа: splitmix64 вместо `StdRng` —
/// решений много (тысячи существ × тик), инициализация ChaCha на каждое
/// была чистой накладной стоимостью (ARCHITECTURE 2.13).
fn decision_rng(world_seed: u64, entity_id: u64, tick: u64) -> SplitMix64 {
    SplitMix64::new(mix_seed(
        decision_seed(world_seed, entity_id, tick),
        0xD1EC_1510_0000_0001,
    ))
}

/// Только читаемый вид рельефа для ИИ существ. Отдельный тип позволяет
/// одновременно менять список существ и читать чанки мира.
struct TerrainView<'a> {
    chunks: &'a [Chunk],
    chunks_per_side: u32,
    side: i32,
}

impl TerrainView<'_> {
    /// Вид рельефа под непрерывной позицией; `None` — вне мира.
    fn terrain_at(&self, pos: WorldPos) -> Option<TileType> {
        self.terrain_tile(pos.x.floor() as i32, pos.y.floor() as i32)
    }

    /// Вид рельефа в тайле; `None` — вне мира.
    fn terrain_tile(&self, tx: i32, ty: i32) -> Option<TileType> {
        if tx < 0 || ty < 0 || tx >= self.side || ty >= self.side {
            return None;
        }
        let (chunk, index) = split_tile(TilePos { x: tx, y: ty });
        Some(
            self.chunks[(chunk.y * self.chunks_per_side + chunk.x) as usize]
                .terrain
                .get(index as usize),
        )
    }

    /// Проходим ли тайл: глубокая вода и океан — нет. Мелководье
    /// проходимо (как у игрока вброд).
    fn passable(&self, pos: WorldPos) -> bool {
        !matches!(
            self.terrain_at(pos),
            Some(TileType::DeepWater | TileType::Ocean)
        )
    }

    /// Вода ли под позицией (любая глубина).
    fn is_water(&self, pos: WorldPos) -> bool {
        matches!(self.terrain_at(pos), Some(kind) if kind.is_water())
    }

    /// Лёгкая проверка прямой «от—до»: нет ли на пути глубокой воды
    /// (DeepWater/Ocean). 8 проб по отрезку; мелководье и реки 1–2 тайла
    /// не мешают — через них существо переходит вброд. Для BFS-точности
    /// не претендует, только отсеивает заведомо «пловцовые» цели брожения.
    fn line_avoids_deep_water(&self, from: WorldPos, to: WorldPos) -> bool {
        const SAMPLES: usize = 8;
        for step in 1..=SAMPLES {
            let t = step as f32 / SAMPLES as f32;
            let point = WorldPos {
                x: from.x + (to.x - from.x) * t,
                y: from.y + (to.y - from.y) * t,
            };
            if matches!(
                self.terrain_at(point),
                Some(TileType::DeepWater | TileType::Ocean)
            ) {
                return false;
            }
        }
        true
    }

    /// Суша ли под позицией (вне мира сушей не считается).
    fn is_land(&self, pos: WorldPos) -> bool {
        matches!(self.terrain_at(pos), Some(kind) if !kind.is_water())
    }

    /// Ближайшая суша от позиции: BFS по проходимой воде, первый берег
    /// в фиксированном порядке соседей. Возвращает центр тайла суши.
    /// `None` — в пределах `LAND_SEARCH_LIMIT` суши нет.
    fn nearest_land(
        &self,
        start: WorldPos,
        scratch: &mut LandSearchScratch,
    ) -> Option<WorldPos> {
        let sx = start.x.floor() as i32;
        let sy = start.y.floor() as i32;

        // Буферы переиспользуются между вызовами: BFS в воде — не редкость.
        let visited = &mut scratch.visited;
        let queue = &mut scratch.queue;
        visited.clear();
        queue.clear();
        visited.insert((sx, sy));
        queue.push_back((sx, sy));

        while let Some((x, y)) = queue.pop_front() {
            for (dx, dy) in LAND_NEIGHBORS {
                let nx = x + dx;
                let ny = y + dy;
                if !visited.insert((nx, ny)) {
                    continue;
                }
                let Some(kind) = self.terrain_tile(nx, ny) else {
                    continue;
                };
                if !kind.is_water() {
                    return Some(WorldPos::of_tile_center(TilePos { x: nx, y: ny }));
                }
                // Уходим от берега только по мелководью: глубина и океан
                // для волка непроходимы, и путь через них не существует.
                if !matches!(kind, TileType::DeepWater | TileType::Ocean) {
                    queue.push_back((nx, ny));
                }
                if visited.len() >= LAND_SEARCH_LIMIT {
                    return None;
                }
            }
        }
        None
    }
}
