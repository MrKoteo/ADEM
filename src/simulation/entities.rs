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

use std::collections::{HashSet, VecDeque};

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng}; // Bevy 0.20 / rand 0.10: RngExt вместо Rng

use super::chunk::Chunk;
use super::player::WorldPos;
use super::tile::TileType;
use super::time::TICKS_PER_SEC;
use super::world::{World, split_tile, TilePos};

/// Радиус, в котором волк выбирает следующую точку брожения, тайлов.
/// Точка отсчитывается от текущей позиции, а не от «дома»: так животное
/// может постепенно мигрировать по карте, не будучи привязанным к месту.
const WANDER_RADIUS_TILES: f32 = 8.0;

/// Скорость ходьбы существ, тайлов за тик (GAME_DESIGN 3.4).
const WALK_SPEED: f32 = 0.2;

/// Длительности состояний, реальные секунды при x1.
const WALK_SECS: (u64, u64) = (3, 8);
const STAND_SECS: (u64, u64) = (1, 3);
const LIE_SECS: (u64, u64) = (5, 15);

/// Вероятность лечь при выборе из стояния. Ходьба — основное занятие,
/// лежание — редкое.
const LIE_CHANCE: f32 = 0.2;

/// Период смены кадров анимации ходьбы: 0.5 секунды.
const WALK_FRAME_TICKS: u64 = (TICKS_PER_SEC as u64) / 2;

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

/// Вид существа. Значения стабильны для сохранений.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum EntityKind {
    Wolf,
}

impl EntityKind {
    /// Все виды существ в стабильном порядке (генерация, UI редактора).
    pub const ALL: [EntityKind; 1] = [EntityKind::Wolf];

    /// Максимальный запас здоровья вида. HP пока только данные —
    /// механика урона появится позже.
    pub fn max_hp(self) -> f32 {
        match self {
            EntityKind::Wolf => 80.0,
        }
    }

    /// Скорость ходьбы вида, тайлов за тик.
    pub fn walk_speed(self) -> f32 {
        match self {
            EntityKind::Wolf => WALK_SPEED,
        }
    }

    /// Вид существа по стабильному значению u8 (сохранения).
    pub fn from_discriminant(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Wolf),
            _ => None,
        }
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
/// при взгляде влево рендер зеркалит спрайт по горизонтали.
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

/// Существо мира: непрерывная позиция, состояние поведения и данные,
/// которые понадобятся будущим механикам (HP, инвентарь и т. п.).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SimEntity {
    /// Стабильный идентификатор в пределах мира (для планировщика и команд).
    pub id: u64,
    pub kind: EntityKind,
    /// Позиция в тайлах (непрерывная).
    pub pos: WorldPos,
    /// Позиция на прошлом тике: рендер интерполирует между ней и `pos`.
    pub prev_pos: WorldPos,
    pub facing: Facing,
    pub state: EntityState,
    /// Текущее здоровье; максимум задаётся видом.
    pub hp: f32,
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
}

impl SimEntity {
    /// Новое существо в состоянии «стоит» с полным здоровьем.
    fn new(id: u64, kind: EntityKind, pos: WorldPos) -> Self {
        Self {
            id,
            kind,
            pos,
            prev_pos: pos,
            facing: Facing::Right,
            state: EntityState::Standing,
            hp: kind.max_hp(),
            water_ticks: 0,
            state_ticks: 0,
            state_duration: INITIAL_STAND_TICKS,
            target: pos,
            stuck_ticks: 0,
            land_search_failed_at: 0,
        }
    }

    /// Кадр анимации ходьбы: 0 или 1, смена раз в `WALK_FRAME_TICKS`.
    pub fn walk_frame(&self) -> u8 {
        ((self.state_ticks / WALK_FRAME_TICKS) % 2) as u8
    }
}

/// Хранилище существ мира. `Vec` вместо `HashMap`: порядок обхода
/// детерминирован, а поиск по id пока не горячий путь.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityStore {
    /// Существа в порядке создания (id возрастают).
    pub entities: Vec<SimEntity>,
    /// Следующий свободный идентификатор; 0 зарезервирован как «нет id».
    pub next_id: u64,
}

impl EntityStore {
    /// Создаёт существо вида `kind` в позиции `pos` и возвращает его id.
    pub fn spawn(&mut self, kind: EntityKind, pos: WorldPos) -> u64 {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entities.push(SimEntity::new(id, kind, pos));
        id
    }

    /// Убирает существо по id (инструмент редактора). `true` — существо
    /// было и удалено; id существ не переиспользуются, поэтому старые
    /// ссылки на удалённое существо просто перестают находиться.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entities.len();
        self.entities.retain(|entity| entity.id != id);
        self.entities.len() != before
    }

    /// Переносит существо в точку (перетаскивание колёсиком в редакторе).
    /// `prev_pos` выравнивается с `pos`: иначе рендер плавно догонял бы
    /// старое место и существо «уезжало» от курсора. Состояние сбрасывается
    /// в стояние, чтобы после освобождения существо не продолжило путь к
    /// прежней цели. `false` — существа с таким id нет.
    pub fn move_to(&mut self, id: u64, pos: WorldPos) -> bool {
        let Some(entity) = self.entities.iter_mut().find(|entity| entity.id == id) else {
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
    fn update(&mut self, seed: u64, tick: u64, terrain: &TerrainView<'_>) {
        let side = terrain.side as f32;
        for entity in &mut self.entities {
            entity.prev_pos = entity.pos;

            // Время в воде копится только подряд: на суше счётчик сброшен.
            if terrain.is_water(entity.pos) {
                entity.water_ticks = entity.water_ticks.saturating_add(1);
            } else {
                entity.water_ticks = 0;
            }

            // В воде нельзя стоять или лежать: сразу идём к ближайшей суше.
            // Так волк может переплыть реку, но не остаётся в ней отдыхать.
            // Если поиск суши недавно провалился (океан без берегов), BFS
            // не повторяется до конца паузы: существо живёт в прежнем
            // состоянии, не сжигая обход тайлов каждый тик.
            if !land_search_on_cooldown(entity, tick) {
                if entity.state != EntityState::Walking && terrain.is_water(entity.pos) {
                    enter_escape(entity, terrain, tick);
                } else if entity.water_ticks >= WATER_PANIC_TICKS
                    && !terrain.is_land(entity.target)
                {
                    // Плывём слишком долго — бросаем прежнюю цель и
                    // кратчайшим путём выходим на берег.
                    enter_escape(entity, terrain, tick);
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
                        choose_from_standing(entity, &mut rng, terrain, side, tick);
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
                                enter_escape(entity, terrain, tick);
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
    pub fn tick_entities(&mut self, tick: u64) {
        let World {
            entities,
            chunks,
            chunks_per_side,
            size,
            seed,
            ..
        } = self;
        let terrain = TerrainView {
            chunks,
            chunks_per_side: *chunks_per_side,
            side: size.tiles() as i32,
        };
        entities.update(*seed, tick, &terrain);
    }
}

/// Выбор следующего состояния из стояния: обычно идти, редко лежать.
fn choose_from_standing(
    entity: &mut SimEntity,
    rng: &mut StdRng,
    terrain: &TerrainView<'_>,
    side: f32,
    tick: u64,
) {
    // Защита от рассинхрона: стоять в воде нельзя даже один цикл.
    // На кулдауне поиска суши состояние сохраняем — BFS повторится позже.
    if terrain.is_water(entity.pos) && !land_search_on_cooldown(entity, tick) {
        enter_escape(entity, terrain, tick);
        return;
    }
    if rng.random::<f32>() < LIE_CHANCE {
        enter_lying(entity, rng);
    } else {
        enter_walking(entity, rng, terrain, side);
    }
}

/// Переводит существо в ходьбу и выбирает достижимую точку брожения.
/// Если подходящей точки нет (например, всё вокруг — глубокая вода),
/// существо остаётся стоять ещё один цикл.
fn enter_walking(
    entity: &mut SimEntity,
    rng: &mut StdRng,
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
fn enter_standing(entity: &mut SimEntity, rng: &mut StdRng) {
    entity.state = EntityState::Standing;
    entity.state_ticks = 0;
    entity.state_duration = random_duration(rng, STAND_SECS);
    entity.stuck_ticks = 0;
}

/// Переводит существо в лежание.
fn enter_lying(entity: &mut SimEntity, rng: &mut StdRng) {
    entity.state = EntityState::Lying;
    entity.state_ticks = 0;
    entity.state_duration = random_duration(rng, LIE_SECS);
    entity.stuck_ticks = 0;
}

/// Переводит существо в ходьбу к ближайшей суше. Длительность берётся
/// с запасом на весь путь плюс секунда, чтобы состояние не закончилось
/// раньше берега. Возвращает `false`, если суши в пределах поиска нет
/// (неудача запоминается: до `LAND_SEARCH_RETRY_TICKS` BFS не повторяется).
fn enter_escape(entity: &mut SimEntity, terrain: &TerrainView<'_>, tick: u64) -> bool {
    let Some(target) = terrain.nearest_land(entity.pos) else {
        entity.land_search_failed_at = tick;
        return false;
    };
    let dx = target.x - entity.pos.x;
    let dy = target.y - entity.pos.y;
    let distance = (dx * dx + dy * dy).sqrt();
    entity.state = EntityState::Walking;
    entity.state_ticks = 0;
    entity.state_duration =
        (distance / WALK_SPEED).ceil() as u64 + TICKS_PER_SEC as u64;
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
fn random_duration(rng: &mut StdRng, secs: (u64, u64)) -> u64 {
    let min = secs.0 * TICKS_PER_SEC as u64;
    let max = secs.1 * TICKS_PER_SEC as u64;
    min + rng.random_range(0..=(max - min))
}

/// Выбирает точку в радиусе брожения, лежащую на проходимом тайле.
/// Сначала ищет сушу; вода — только запасной вариант, чтобы волк мог
/// перейти реку, если вокруг неё всё остальное тоже вода или глубоко.
/// `None` — подходящей точки не нашли.
fn pick_target(
    rng: &mut StdRng,
    from: WorldPos,
    terrain: &TerrainView<'_>,
    side: f32,
) -> Option<WorldPos> {
    let mut water_fallback = None;
    for _ in 0..16 {
        let angle = rng.random::<f32>() * std::f32::consts::TAU;
        // sqrt делает выбор равномерным по площади круга, а не по радиусу.
        let radius = WANDER_RADIUS_TILES * rng.random::<f32>().sqrt();
        let pos = WorldPos {
            x: (from.x + angle.cos() * radius).clamp(0.0, side),
            y: (from.y + angle.sin() * radius).clamp(0.0, side),
        };
        if !terrain.passable(pos) {
            continue;
        }
        if !terrain.is_water(pos) {
            return Some(pos);
        }
        water_fallback.get_or_insert(pos);
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

    // Взгляд следует за горизонтальным направлением движения; при
    // движении строго по вертикали существо сохраняет прежний взгляд.
    if dx.abs() > f32::EPSILON {
        entity.facing = if dx > 0.0 { Facing::Right } else { Facing::Left };
    }

    let speed = entity.kind.walk_speed();
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

/// Новый RNG для одного решения существа.
fn decision_rng(world_seed: u64, entity_id: u64, tick: u64) -> StdRng {
    StdRng::seed_from_u64(decision_seed(world_seed, entity_id, tick))
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

    /// Суша ли под позицией (вне мира сушей не считается).
    fn is_land(&self, pos: WorldPos) -> bool {
        matches!(self.terrain_at(pos), Some(kind) if !kind.is_water())
    }

    /// Ближайшая суша от позиции: BFS по проходимой воде, первый берег
    /// в фиксированном порядке соседей. Возвращает центр тайла суши.
    /// `None` — в пределах `LAND_SEARCH_LIMIT` суши нет.
    fn nearest_land(&self, start: WorldPos) -> Option<WorldPos> {
        let sx = start.x.floor() as i32;
        let sy = start.y.floor() as i32;

        let mut visited: HashSet<(i32, i32)> = HashSet::new();
        let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
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
