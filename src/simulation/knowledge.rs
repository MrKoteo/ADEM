//! Технологии и социальные институты (SI).
//!
//! Каталог загружается из `data/science` и `data/culture`. В симуляции
//! хранится только компактный битовый набор изученного; сами карточки
//! (названия, стоимость изучения) — это статичные данные, которые не
//! сохраняются в сейв.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::time::{GAME_SECS_PER_TICK, SECS_PER_HOUR};

/// Максимум карточек знаний. 256 бит — 32 байта на человека.
pub const MAX_KNOWLEDGE: usize = 256;
const WORDS: usize = MAX_KNOWLEDGE / 64;

/// Одна карточка технологии или социального института.
#[derive(Clone, Debug, PartialEq)]
pub struct KnowledgeInfo {
    pub id: String,
    pub name: String,
    /// Базовое расчётное время изучения в игровых часах.
    pub study_hours: f32,
    /// Индексы предпосылок в каталоге.
    pub requires: Vec<u16>,
    pub kind: KnowledgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnowledgeKind {
    Technology,
    Social,
}

impl KnowledgeKind {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            KnowledgeKind::Technology => "Технология",
            KnowledgeKind::Social => "Социальный институт",
        }
    }
}

/// Статичный каталог знаний. Индексы стабильны в пределах запуска и
/// отсортированы по id, чтобы сейв не зависел от порядка файлов.
#[derive(Clone, Debug, Default)]
pub struct KnowledgeCatalog {
    entries: Vec<KnowledgeInfo>,
    by_id: HashMap<String, u16>,
    /// Строковые зависимости до вызова `finish`.
    pending_requires: Vec<(u16, Vec<String>)>,
}

impl KnowledgeCatalog {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Загружает `data/science/*.json` и `data/culture/*.json`.
    pub fn load_default() -> Self {
        let mut catalog = Self::empty();
        let mut errors = Vec::new();
        catalog.load_dir(Path::new("data/science"), KnowledgeKind::Technology, &mut errors);
        catalog.load_dir(Path::new("data/culture"), KnowledgeKind::Social, &mut errors);
        for error in errors {
            eprintln!("knowledge: {error}");
        }
        catalog.finish();
        catalog
    }

    fn load_dir(&mut self, dir: &Path, kind: KnowledgeKind, errors: &mut Vec<String>) {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            errors.push(format!("не удалось прочитать {}", dir.display()));
            return;
        };
        let mut files: Vec<_> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        files.sort();

        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                errors.push(format!("не удалось прочитать {}", path.display()));
                continue;
            };
            let Ok(doc) = serde_json::from_str::<RawKnowledgeDoc>(&text) else {
                errors.push(format!("некорректный JSON {}", path.display()));
                continue;
            };
            for raw in doc.technologies.into_iter().chain(doc.institutions) {
                if raw.id.trim().is_empty() {
                    errors.push(format!("пустой id в {}", path.display()));
                    continue;
                }
                if self.by_id.contains_key(&raw.id) {
                    errors.push(format!("дубликат знания {}", raw.id));
                    continue;
                }
                let index = self.entries.len();
                if index >= MAX_KNOWLEDGE {
                    errors.push(format!("слишком много знаний, пропущено {}", raw.id));
                    continue;
                }
                self.by_id.insert(raw.id.clone(), index as u16);
                if !raw.requires.is_empty() {
                    self.pending_requires.push((index as u16, raw.requires));
                }
                self.entries.push(KnowledgeInfo {
                    id: raw.id,
                    name: raw.name,
                    study_hours: raw.study_hours.max(0.0),
                    requires: Vec::new(),
                    kind,
                });
            }
        }
    }

    /// Разрешает строковые предпосылки в индексы и сортирует по id.
    fn finish(&mut self) {
        let mut resolved: Vec<(u16, Vec<u16>)> = Vec::with_capacity(self.pending_requires.len());
        for (index, requires) in self.pending_requires.drain(..) {
            let mut indices = Vec::with_capacity(requires.len());
            for id in requires {
                if let Some(&req) = self.by_id.get(&id) {
                    indices.push(req);
                } else {
                    eprintln!("knowledge: {} ссылается на неизвестное {id}", self.entries[index as usize].id);
                }
            }
            resolved.push((index, indices));
        }

        let mut order: Vec<usize> = (0..self.entries.len()).collect();
        order.sort_by(|&a, &b| self.entries[a].id.cmp(&self.entries[b].id));
        let mut old_to_new = vec![u16::MAX; self.entries.len()];
        for (new_index, &old_index) in order.iter().enumerate() {
            old_to_new[old_index] = new_index as u16;
        }

        let mut sorted = Vec::with_capacity(self.entries.len());
        for &old_index in &order {
            let mut entry = self.entries[old_index].clone();
            entry.requires.clear();
            sorted.push(entry);
        }
        for (old_index, requires) in resolved {
            let new_index = old_to_new[old_index as usize] as usize;
            sorted[new_index].requires =
                requires.into_iter().map(|req| old_to_new[req as usize]).collect();
        }

        self.entries = sorted;
        self.by_id.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            self.by_id.insert(entry.id.clone(), index as u16);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[allow(dead_code)]
    pub fn entries(&self) -> &[KnowledgeInfo] {
        &self.entries
    }

    pub fn get(&self, index: u16) -> Option<&KnowledgeInfo> {
        self.entries.get(index as usize)
    }

    pub fn index_of(&self, id: &str) -> Option<u16> {
        self.by_id.get(id).copied()
    }

    /// Базовое время изучения в тиках.
    pub fn base_ticks(&self, index: u16) -> u64 {
        let Some(info) = self.get(index) else {
            return 0;
        };
        let secs = info.study_hours.max(0.0) as f64 * SECS_PER_HOUR as f64;
        (secs / GAME_SECS_PER_TICK as f64).round().max(1.0) as u64
    }

    /// Все предпосылки изучены?
    pub fn requirements_met(&self, index: u16, known: &KnowledgeSet) -> bool {
        self.get(index)
            .is_some_and(|info| info.requires.iter().all(|&req| known.has(req)))
    }
}

#[derive(Deserialize)]
struct RawKnowledgeDoc {
    #[serde(default)]
    technologies: Vec<RawKnowledge>,
    #[serde(default)]
    institutions: Vec<RawKnowledge>,
}

#[derive(Deserialize)]
struct RawKnowledge {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    study_hours: f32,
    #[serde(default)]
    requires: Vec<String>,
}

/// Компактный набор изученного. Копируется вместе с `HumanMind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KnowledgeSet {
    bits: [u64; WORDS],
}

impl KnowledgeSet {
    pub const fn new() -> Self {
        Self { bits: [0; WORDS] }
    }

    pub fn has(&self, index: u16) -> bool {
        let index = index as usize;
        index < MAX_KNOWLEDGE && (self.bits[index / 64] & (1 << (index % 64))) != 0
    }

    /// Возвращает true, если знание действительно добавлено.
    pub fn insert(&mut self, index: u16) -> bool {
        let index = index as usize;
        if index >= MAX_KNOWLEDGE || self.has(index as u16) {
            return false;
        }
        self.bits[index / 64] |= 1 << (index % 64);
        true
    }

    pub fn count(&self) -> u32 {
        self.bits.iter().map(|word| word.count_ones()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&word| word == 0)
    }

    /// Обходит изученные индексы по возрастанию.
    pub fn for_each(&self, mut f: impl FnMut(u16)) {
        for (word_index, &word) in self.bits.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                f((word_index * 64 + bit) as u16);
                bits &= bits - 1;
            }
        }
    }

    pub fn raw(&self) -> &[u64; WORDS] {
        &self.bits
    }

    pub fn from_raw(bits: [u64; WORDS]) -> Self {
        Self { bits }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.bits = [0; WORDS];
    }
}
