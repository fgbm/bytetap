//! Источник страниц журнала: постраничное чтение с LRU на 20 страниц (FR-29).
//!
//! Журнал бывает в сотни мегабайт, поэтому в память окна он не загружается. Лента
//! знает общее число событий из сводки, а строки приходят страницами по [`PAGE_SIZE`]
//! событий. Загруженные страницы лежат в LRU: прокрутка вперёд вытесняет старые, и
//! память не растёт с длиной сессии.
//!
//! Чтение блокирующее, поэтому его зовёт не поток окна, а фоновая задача: окно
//! получает готовые события и перерисовывается.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use bytetap_core::journal::Event;
use bytetap_core::journal::view::JournalCache;

/// Сколько событий в одной странице журнала.
pub const PAGE_SIZE: usize = 500;

/// Сколько страниц журнала держим в памяти.
pub const PAGES_KEPT: usize = 20;

/// Ошибка чтения страницы журнала.
#[derive(Debug, Clone)]
pub struct PageError {
    /// Что случилось: текст для журнала запуска.
    pub message: String,
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PageError {}

/// Кеш индексов журналов: общий на приложение, индексы переживают закрытие просмотра.
#[derive(Debug, Default)]
pub struct JournalCacheHandle {
    cache: JournalCache,
}

impl JournalCacheHandle {
    /// Открыть журнал: сводка, плотность и таблица корзин.
    ///
    /// Вызов блокирующий: чтение файла — работа для фоновой задачи.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn open(&self, path: &Path) -> Result<bytetap_core::journal::JournalSummary, PageError> {
        self.cache.open(path).map_err(|err| PageError {
            message: err.to_string(),
        })
    }

    /// Страница событий ленты: см. [`JournalIndex::read`].
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn read(&self, path: &Path, from: usize, count: usize) -> Result<Vec<Event>, PageError> {
        self.cache.read(path, from, count).map_err(|err| PageError {
            message: err.to_string(),
        })
    }

    /// Длины событий ленты в байтах: по ним считаются высоты HEX-строк.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn lengths(&self, path: &Path) -> Result<Vec<u64>, PageError> {
        let index = self.cache.index(path).map_err(|err| PageError {
            message: err.to_string(),
        })?;
        Ok(index.lengths().to_vec())
    }

    /// Таблица «первое событие корзины»: перевод доли времени в номер события.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn first_event(&self, path: &Path) -> Result<Vec<usize>, PageError> {
        let index = self.cache.index(path).map_err(|err| PageError {
            message: err.to_string(),
        })?;
        Ok(index.first_event().to_vec())
    }

    /// Дописать комментарий в остановленный журнал (FR-23).
    ///
    /// # Errors
    ///
    /// Журнал идёт или занят другой программой, файл оборван, это не журнал bytetap.
    pub fn append_note(&self, path: &Path, text: &str) -> Result<(), PageError> {
        bytetap_core::journal::view::append_note(path, text).map_err(|err| PageError {
            message: err.to_string(),
        })
    }
}

/// Источник страниц журнала: события по номерам, страницы в LRU.
///
/// Страницы загружаются по требованию: окно знает, какие номера событий видны, и
/// просит их. Пока страница едет, строки нет — лента дорисуется, когда страница
/// придёт.
#[derive(Debug)]
pub struct PageSource {
    path: PathBuf,
    total: usize,
    /// Длины событий из индекса: нужны для высот HEX-строк, не дожидаясь страницы.
    lengths: Vec<u64>,
    pages: HashMap<usize, Vec<Event>>,
    /// Порядок обращения к страницам: последняя — самая свежая.
    order: Vec<usize>,
    /// Страницы, которые уже запрошены: повторный запрос не нужен.
    pending: Vec<usize>,
    /// Страницы, которые не удалось прочитать: повтор не повторяется.
    failed: Vec<usize>,
    /// Комментарии каждой загруженной страницы: из них берутся комментарии журнала.
    page_notes: HashMap<usize, Vec<String>>,
    /// Версия: растёт, когда пришла новая страница или сменилось число событий.
    version: u64,
}

impl PageSource {
    /// Источник для журнала: путь, общее число событий и длины из индекса.
    pub fn new(path: PathBuf, total: usize, lengths: Vec<u64>) -> Self {
        Self {
            path,
            total,
            lengths,
            pages: HashMap::new(),
            order: Vec::new(),
            pending: Vec::new(),
            failed: Vec::new(),
            page_notes: HashMap::new(),
            version: 0,
        }
    }

    /// Общее число событий журнала: строки ленты считаются по нему.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Версия источника: меняется, когда пришла страница или изменилось число событий.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Путь к журналу.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Длина события в байтах из индекса; `None` — индекс не дал её.
    pub fn byte_length(&self, index: usize) -> Option<u64> {
        self.lengths.get(index).copied()
    }

    /// Число строк дампа у события: по длине из индекса, без чтения страницы.
    pub fn hex_lines(&self, index: usize) -> usize {
        let len = self.byte_length(index).unwrap_or(0) as usize;
        crate::feed::hex_line_count(len)
    }

    /// Событие по номеру; `None` — страница ещё едет или её не удалось прочитать.
    pub fn event(&self, index: usize) -> Option<&Event> {
        if index >= self.total {
            return None;
        }
        let page = index / PAGE_SIZE;
        let events = self.pages.get(&page)?;
        events.get(index % PAGE_SIZE)
    }

    /// Все загруженные события: по страницам в порядке обращения.
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.order
            .iter()
            .filter_map(|page| self.pages.get(page))
            .flat_map(|events| events.iter())
    }

    /// Страницы, покрывающие события `[from, to)`: их надо загрузить.
    pub fn missing_pages(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        if to <= from || self.total == 0 {
            return Vec::new();
        }
        let last_page = self.total.div_ceil(PAGE_SIZE) - 1;
        let first_page = (from / PAGE_SIZE).min(last_page);
        let end_page = ((to - 1) / PAGE_SIZE).min(last_page);
        let mut out = Vec::new();
        for page in first_page..=end_page {
            if self.pages.contains_key(&page)
                || self.pending.contains(&page)
                || self.failed.contains(&page)
            {
                continue;
            }
            out.push((page, page * PAGE_SIZE));
        }
        out
    }

    /// Отметить страницу запрошенной: повторный запрос не нужен.
    pub fn mark_pending(&mut self, page: usize) {
        if !self.pending.contains(&page) {
            self.pending.push(page);
        }
    }

    /// Положить загруженную страницу: события попадают в кеш, LRU обновляется.
    pub fn store_page(&mut self, page: usize, events: Vec<Event>) {
        self.pending.retain(|p| *p != page);
        if page * PAGE_SIZE >= self.total {
            // Журнал успел сократиться: страница больше не нужна.
            return;
        }
        let notes: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                Event::Note(note) => Some(note.text.clone()),
                _ => None,
            })
            .collect();
        self.page_notes.insert(page, notes);
        if self.pages.insert(page, events).is_none() {
            self.order.push(page);
        } else {
            self.order.retain(|p| *p != page);
            self.order.push(page);
        }
        while self.pages.len() > PAGES_KEPT
            && let Some(oldest) = self.order.first().copied()
        {
            self.order.remove(0);
            self.pages.remove(&oldest);
            self.page_notes.remove(&oldest);
        }
        self.version += 1;
    }

    /// Страница не прочиталась: повтор не повторяется, чтобы прокрутка не зациклилась.
    pub fn mark_failed(&mut self, page: usize) {
        self.pending.retain(|p| *p != page);
        if !self.failed.contains(&page) {
            self.failed.push(page);
        }
    }

    /// Комментарии журнала: новые сверху, не больше `limit`.
    ///
    /// Комментарии берутся из загруженных страниц: незагруженные страницы лента всё
    /// равно не показывает.
    pub fn notes(&self, limit: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for page in self.order.iter().rev() {
            let Some(notes) = self.page_notes.get(page) else {
                continue;
            };
            for note in notes.iter().rev() {
                out.push(note.clone());
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// В журнал дописан комментарий: событий стало больше.
    ///
    /// Последняя страница перечитывается: дописанный комментарий виден и в сводке, и
    /// в ленте без прокрутки.
    pub fn extend(&mut self, total: usize) {
        if total <= self.total {
            return;
        }
        self.total = total;
        while self.lengths.len() < total {
            self.lengths.push(0);
        }
        let last_page = self.total.div_ceil(PAGE_SIZE).saturating_sub(1);
        self.pages.remove(&last_page);
        self.page_notes.remove(&last_page);
        self.order.retain(|p| *p != last_page);
        self.pending.retain(|p| *p != last_page);
        self.failed.retain(|p| *p != last_page);
        self.version += 1;
    }

    /// Обновить длины событий из индекса: журнал мог подрос.
    pub fn set_lengths(&mut self, lengths: Vec<u64>) {
        self.lengths = lengths;
    }

    /// Забыть загруженное: журнал закрыт или открыт другой.
    pub fn clear(&mut self) {
        self.pages.clear();
        self.page_notes.clear();
        self.order.clear();
        self.pending.clear();
        self.failed.clear();
        self.total = 0;
        self.lengths.clear();
        self.version += 1;
    }
}

/// Кеш индексов журналов, общий на приложение: индекс переживает закрытие просмотра.
///
/// Кеш защищён замком: индексы строятся в фоновых задачах, а окно читает сводку.
#[derive(Debug, Default)]
pub struct SharedJournalCache {
    cache: Mutex<JournalCacheHandle>,
}

impl SharedJournalCache {
    /// Кеш индексов под замком: задача держит его, пока читает журнал.
    pub fn handle(&self) -> std::sync::MutexGuard<'_, JournalCacheHandle> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Индекс журнала для источника страниц: сводка, длины и таблица корзин.
#[derive(Debug, Clone)]
pub struct JournalOpen {
    /// Путь к журналу.
    pub path: PathBuf,
    /// Сводка журнала.
    pub summary: bytetap_core::journal::JournalSummary,
    /// Длины событий ленты в байтах.
    pub lengths: Vec<u64>,
    /// Таблица «первое событие корзины».
    pub first_event: Vec<usize>,
}

impl JournalOpen {
    /// Открыть журнал и собрать всё, что нужно ленте и карте.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn open(cache: &JournalCacheHandle, path: &Path) -> Result<Self, PageError> {
        let summary = cache.open(path)?;
        let lengths = cache.lengths(path)?;
        let first_event = cache.first_event(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            summary,
            lengths,
            first_event,
        })
    }
}

/// Источник страниц из уже открытого журнала.
pub fn source(opened: &JournalOpen) -> PageSource {
    PageSource::new(
        opened.path.clone(),
        opened.summary.events,
        opened.lengths.clone(),
    )
}

/// Кеш индексов журналов, общий на окно: [`SharedJournalCache`].
pub type SharedCache = Arc<SharedJournalCache>;

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Data, Dir, Event};

    use super::*;

    fn event(index: usize) -> Event {
        Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            b"A",
        ))
    }

    fn source_with(total: usize) -> PageSource {
        PageSource::new(PathBuf::from("journal.jsonl"), total, vec![0; total])
    }

    #[test]
    fn the_pages_are_asked_for_the_visible_window() {
        let mut source = source_with(2000);
        let missing = source.missing_pages(0, 10);
        assert_eq!(missing, vec![(0, 0)]);
        source.mark_pending(0);
        assert!(
            source.missing_pages(0, 10).is_empty(),
            "запрошенная страница повторно не запрашивается"
        );
        // Вторая страница — за событиями 500.
        let missing = source.missing_pages(500, 520);
        assert_eq!(missing, vec![(1, 500)]);
    }

    #[test]
    fn the_loaded_pages_are_evicted_by_lru() {
        let mut source = source_with(PAGE_SIZE * (PAGES_KEPT + 2));
        for page in 0..PAGES_KEPT + 2 {
            source.mark_pending(page);
            source.store_page(page, vec![event(page)]);
        }
        assert_eq!(
            source.pages.len(),
            PAGES_KEPT,
            "LRU вытеснил лишние страницы"
        );
        assert!(!source.pages.contains_key(&0), "самый давний вытеснен");
        assert!(source.pages.contains_key(&(PAGES_KEPT + 1)));
    }

    #[test]
    fn a_touched_page_stays_in_the_cache() {
        let mut source = source_with(PAGE_SIZE * (PAGES_KEPT + 1));
        for page in 0..=PAGES_KEPT {
            source.mark_pending(page);
            source.store_page(page, vec![event(page)]);
        }
        // Страница 0 снова становится свежей.
        source.mark_pending(0);
        source.store_page(0, vec![event(0)]);
        source.mark_pending(PAGES_KEPT + 1);
        source.store_page(PAGES_KEPT + 1, vec![event(PAGES_KEPT + 1)]);
        assert!(
            source.pages.contains_key(&0),
            "недавно тронутая страница не вытесняется"
        );
    }

    #[test]
    fn the_comment_extends_the_last_page() {
        let mut source = source_with(PAGE_SIZE);
        source.mark_pending(0);
        source.store_page(0, vec![event(0)]);
        assert_eq!(source.total(), PAGE_SIZE);

        source.extend(PAGE_SIZE + 1);
        assert_eq!(source.total(), PAGE_SIZE + 1);
        assert!(
            source.pages.contains_key(&0),
            "первая страница остаётся в кеше"
        );
        // Комментарий — событие 500: он в новой странице, её и надо прочитать.
        assert_eq!(
            source.missing_pages(PAGE_SIZE, PAGE_SIZE + 1),
            vec![(1, PAGE_SIZE)],
            "новая страница запрошена"
        );
    }

    #[test]
    fn the_grown_last_page_is_read_again() {
        let mut source = source_with(PAGE_SIZE + 10);
        source.mark_pending(1);
        source.store_page(1, vec![event(PAGE_SIZE)]);
        source.extend(PAGE_SIZE + 20);
        assert!(
            !source.pages.contains_key(&1),
            "последняя страница перечитывается после комментария"
        );
    }

    #[test]
    fn the_notes_of_the_loaded_pages_are_collected() {
        let mut source = source_with(10);
        source.mark_pending(0);
        source.store_page(
            0,
            vec![Event::Note(bytetap_core::journal::Note {
                t: "t".into(),
                mono_us: None,
                text: "проба".into(),
            })],
        );
        assert_eq!(source.notes(3), vec!["проба".to_owned()]);
    }

    #[test]
    fn a_failed_page_is_not_requested_again() {
        let mut source = source_with(1000);
        source.mark_pending(0);
        source.mark_failed(0);
        assert!(
            source.missing_pages(0, 10).is_empty(),
            "неудачная страница не запрашивается снова"
        );
    }

    #[test]
    fn the_event_comes_from_its_page() {
        let mut source = source_with(600);
        source.mark_pending(0);
        source.store_page(0, vec![event(0), event(1)]);
        source.mark_pending(1);
        source.store_page(1, vec![event(500)]);
        assert!(source.event(0).is_some());
        assert!(source.event(500).is_some());
        assert!(
            source.event(600).is_none(),
            "за последним событием ничего нет"
        );
    }
}
