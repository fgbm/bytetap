//! Просмотр журнала: сводка, плотность и постраничное чтение (FR-29, M2-AC9).
//!
//! Журнал бывает в сотни мегабайт, поэтому в память интерфейса он не загружается. Первый
//! проход по файлу строит индекс: смещения строк событий ленты и гистограмму плотности
//! на [`DENSITY_BUCKETS`] корзин по `mono_us`. Индекс кешируется по пути и перестраивается,
//! только если файл изменился (дописан комментарий). Постраничное чтение идёт по индексу:
//! `seek` на смещение и чтение нужного числа строк. Строится всё в `spawn_blocking`, чтобы
//! не задерживать окно.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read as _, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use bytetap_core::journal::{Dir, Event, Note, Stats, now_rfc3339};
use serde::Deserialize;

use crate::api::{Density, JournalSummary};
use crate::error::{CommandError, ErrorCode};
use crate::feed::FeedEvent;

/// Сколько корзин в гистограмме плотности: столько точек рисует трасса всей сессии.
pub const DENSITY_BUCKETS: usize = 600;

/// Сколько строк максимум отдаёт одна страница журнала.
///
/// Интерфейс просит по 500 строк; больший `count` из команды обрезается: страница
/// не должна превращаться в чтение всего файла.
pub const MAX_PAGE: usize = 1000;

/// Предел длины строки журнала: файл с такой строкой считается битым.
///
/// Строка события — один `data` с base64: даже фрагмент в мегабайт занимает единицы
/// мегабайт JSON. Шестьдесят четыре мегабайта — заведомо испорченный файл, и читать его
/// целиком в память окна нельзя.
pub const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;

/// Сколько индексов журналов держим в памяти (LRU).
pub const MAX_INDEXED: usize = 8;

/// Индекс журнала: всё, что нужно для сводки и постраничного чтения.
#[derive(Debug, Clone)]
pub struct JournalIndex {
    header: Option<serde_json::Value>,
    /// Смещения в байтах у строк, которые становятся событиями ленты.
    offsets: Vec<u64>,
    /// Длина каждого события ленты в байтах: у `data` — фрагмент, у служебных — 0.
    /// По ней лента считает высоты HEX-строк, не читая файл (AC5).
    lens: Vec<u64>,
    duration_us: u64,
    stats: Stats,
    /// Итоги пришли из `session_end`: счётчики по событиям больше не накапливаются.
    stats_from_end: bool,
    density: Density,
    /// Выборки плотности: из них пересобирается гистограмма после доиндексации.
    samples: Vec<Sample>,
    /// Смещение после последней целой строки: с него продолжается доиндексация.
    len: u64,
    modified: Option<SystemTime>,
}

/// Один фрагмент обмена для гистограммы: где он по монотонным часам и сколько в нём байт.
#[derive(Debug, Clone, Copy)]
struct Sample {
    mono_us: u64,
    dir: Dir,
    len: u64,
}

/// Что накопил проход по файлу.
#[derive(Debug, Default)]
struct Scan {
    header: Option<serde_json::Value>,
    offsets: Vec<u64>,
    lens: Vec<u64>,
    samples: Vec<Sample>,
    duration_us: u64,
    stats: Stats,
    stats_from_end: bool,
    /// Смещение после последней целой строки.
    len: u64,
}

impl JournalIndex {
    /// Проход по файлу: заголовок, итоги, смещения строк и плотность.
    ///
    /// Битая строка не ломает просмотр: она пропускается, как и в `bytetap-cli verify`
    /// (ошибки строк там — отдельный отчёт). Файл без заголовка `session` — не журнал.
    fn build(path: &Path) -> Result<Self, CommandError> {
        Self::build_with_limit(path, MAX_LINE_BYTES)
    }

    /// То же, что [`JournalIndex::build`], но с заданным пределом длины строки: для тестов.
    fn build_with_limit(path: &Path, limit: u64) -> Result<Self, CommandError> {
        let metadata = std::fs::metadata(path).map_err(|err| open_error(&err, path))?;
        let mut scan = Scan::default();
        scan_into(path, 0, limit, &mut scan)?;
        Self::assemble(path, scan, &metadata)
    }

    /// Доиндексирует выросший файл с последнего смещения, а не с начала.
    ///
    /// Дописанный комментарий (FR-23) — обычный случай: перечитывать сотни мегабайт
    /// ради одной строки незачем.
    fn extend(path: &Path, previous: &Self) -> Result<Self, CommandError> {
        let metadata = std::fs::metadata(path).map_err(|err| open_error(&err, path))?;
        let mut scan = Scan {
            header: previous.header.clone(),
            offsets: previous.offsets.clone(),
            lens: previous.lens.clone(),
            samples: previous.samples.clone(),
            duration_us: previous.duration_us,
            stats: previous.stats.clone(),
            stats_from_end: previous.stats_from_end,
            len: previous.len,
        };
        scan_into(path, previous.len, MAX_LINE_BYTES, &mut scan)?;
        Self::assemble(path, scan, &metadata)
    }

    /// Собирает индекс из прохода и метаданных файла.
    fn assemble(
        path: &Path,
        scan: Scan,
        metadata: &std::fs::Metadata,
    ) -> Result<Self, CommandError> {
        let header = scan.header.ok_or_else(|| {
            CommandError::new(
                ErrorCode::JournalOpen,
                "это не журнал bytetap: в файле нет заголовка `session`",
            )
            .with_param("path", path.to_string_lossy())
        })?;
        Ok(Self {
            header: Some(header),
            offsets: scan.offsets,
            lens: scan.lens,
            duration_us: scan.duration_us,
            stats: scan.stats,
            stats_from_end: scan.stats_from_end,
            density: density(&scan.samples, scan.duration_us),
            samples: scan.samples,
            len: scan.len,
            modified: metadata.modified().ok(),
        })
    }

    /// Сводка для интерфейса.
    fn summary(&self, path: &str) -> JournalSummary {
        JournalSummary {
            path: path.to_owned(),
            header: self.header.clone().unwrap_or(serde_json::Value::Null),
            events: self.offsets.len(),
            duration_us: self.duration_us,
            stats: self.stats.clone().into(),
            density: self.density.clone(),
        }
    }

    /// Длины событий ленты в байтах: `data` — фрагмент, служебные строки — 0.
    fn lengths(&self) -> Vec<u64> {
        self.lens.clone()
    }

    /// Индекс собран по этой версии файла.
    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        self.len == metadata.len() && self.modified == metadata.modified().ok()
    }
}

/// Читает строку не длиннее `limit` байт; `None` — конец файла.
///
/// Возвращает байты вместе с переводом строки и говорит, уложилась ли строка в предел.
fn read_limited(
    reader: &mut BufReader<std::fs::File>,
    buffer: &mut Vec<u8>,
    limit: u64,
) -> io::Result<Option<(u64, bool)>> {
    let read = {
        let mut limited = reader.by_ref().take(limit.saturating_add(1));
        limited.read_until(b'\n', buffer)?
    };
    if read == 0 {
        return Ok(None);
    }
    Ok(Some((read as u64, read as u64 <= limit)))
}

/// Отбрасывает остаток слишком длинной строки до перевода строки; возвращает число байт.
fn discard_line(reader: &mut BufReader<std::fs::File>) -> io::Result<u64> {
    let mut dropped = 0u64;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(dropped);
        }
        match chunk.iter().position(|byte| *byte == b'\n') {
            Some(position) => {
                reader.consume(position + 1);
                return Ok(dropped + position as u64 + 1);
            }
            None => {
                let len = chunk.len();
                reader.consume(len);
                dropped += len as u64;
            }
        }
    }
}

/// Проход по файлу с `start`: заголовок, события, плотность и смещение после целой строки.
fn scan_into(path: &Path, start: u64, limit: u64, scan: &mut Scan) -> Result<(), CommandError> {
    let file = std::fs::File::open(path).map_err(|err| open_error(&err, path))?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        reader
            .seek(io::SeekFrom::Start(start))
            .map_err(|err| open_error(&err, path))?;
    }
    let mut offset = start;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let Some((read, fits)) =
            read_limited(&mut reader, &mut buffer, limit).map_err(|err| open_error(&err, path))?
        else {
            break;
        };
        let line_start = offset;
        offset += read;
        if !fits {
            // Строка длиннее предела — битая: остаток отбрасываем, чтобы сбойный файл
            // не съел память окна.
            offset += discard_line(&mut reader).map_err(|err| open_error(&err, path))?;
            continue;
        }
        if !buffer.ends_with(b"\n") {
            // Хвост без перевода строки ещё пишется: индексируем его в следующий раз.
            offset = line_start;
            break;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(trim_line(&buffer)) else {
            continue;
        };
        let Ok(event) = Event::deserialize(&value) else {
            continue;
        };
        let mut len = 0u64;
        match event {
            Event::Session(_) => {
                if scan.header.is_none() {
                    scan.header = Some(value);
                }
                continue;
            }
            Event::SessionEnd(end) => {
                scan.duration_us = scan.duration_us.max(end.mono_us);
                scan.stats = end.stats;
                scan.stats_from_end = true;
                continue;
            }
            Event::Open(open) => {
                scan.duration_us = scan.duration_us.max(open.mono_us);
                if !scan.stats_from_end {
                    scan.stats.connections += 1;
                }
            }
            Event::Data(data) => {
                scan.duration_us = scan.duration_us.max(data.mono_us);
                len = data.len;
                scan.samples.push(Sample {
                    mono_us: data.mono_us,
                    dir: data.dir,
                    len: data.len,
                });
                if !scan.stats_from_end {
                    scan.stats.add(data.dir, data.len);
                }
            }
            Event::Close(close) => {
                scan.duration_us = scan.duration_us.max(close.mono_us);
            }
            Event::Error(error) => {
                scan.duration_us = scan.duration_us.max(error.mono_us);
            }
            Event::Info(info) => {
                scan.duration_us = scan.duration_us.max(info.mono_us);
            }
            Event::Note(note) => {
                if let Some(mono_us) = note.mono_us {
                    scan.duration_us = scan.duration_us.max(mono_us);
                }
            }
        }
        scan.offsets.push(line_start);
        scan.lens.push(len);
    }
    scan.len = offset;
    Ok(())
}

/// Гистограмма плотности: байты по направлениям на [`DENSITY_BUCKETS`] корзин.
fn density(samples: &[Sample], duration_us: u64) -> Density {
    let mut density = Density {
        a2l: vec![0; DENSITY_BUCKETS],
        l2a: vec![0; DENSITY_BUCKETS],
    };
    let last = DENSITY_BUCKETS - 1;
    for sample in samples {
        // Последний фрагмент попадает в последнюю корзину, а не за её край.
        // Сессия без длительности (`duration_us == 0`) — всё в первую корзину.
        let bucket = sample
            .mono_us
            .min(duration_us)
            .saturating_mul(DENSITY_BUCKETS as u64)
            .checked_div(duration_us)
            .and_then(|bucket| usize::try_from(bucket).ok())
            .unwrap_or(0)
            .min(last);
        let track = match sample.dir {
            Dir::AnalyzerToLis => &mut density.a2l,
            Dir::LisToAnalyzer => &mut density.l2a,
        };
        track[bucket] += sample.len;
    }
    density
}

/// Кеш индексов журналов: LRU на [`MAX_INDEXED`] путей.
#[derive(Debug, Default)]
pub struct JournalCache {
    state: Mutex<CacheState>,
    /// Замок сборки на путь: два окна не собирают один индекс параллельно.
    builds: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
}

/// Состояние кеша: индексы и счётчик обращений для LRU.
#[derive(Debug, Default)]
struct CacheState {
    entries: HashMap<PathBuf, Entry>,
    tick: u64,
}

/// Индекс вместе с отметкой последнего обращения.
#[derive(Debug)]
struct Entry {
    index: Arc<JournalIndex>,
    used: u64,
}

impl JournalCache {
    /// Индекс журнала: из кеша, а если файл изменился или ещё не открывался — заново.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub async fn index(&self, path: &Path) -> Result<Arc<JournalIndex>, CommandError> {
        let metadata = std::fs::metadata(path).map_err(|err| open_error(&err, path))?;
        if let Some(index) = self.cached(path, &metadata) {
            return Ok(index);
        }

        // Один путь собирается одним вызовом: второй ждёт и берёт готовый индекс.
        let lock = self.build_lock(path);
        let _guard = lock.lock().await;
        let metadata = std::fs::metadata(path).map_err(|err| open_error(&err, path))?;
        if let Some(index) = self.cached(path, &metadata) {
            return Ok(index);
        }
        let previous = self.previous(path);
        let target = path.to_path_buf();
        let index = tokio::task::spawn_blocking(move || index_now(&target, previous.as_deref()))
            .await
            .map_err(|err| {
                CommandError::new(
                    ErrorCode::JournalOpen,
                    format!("индексация журнала упала: {err}"),
                )
            })??;
        let index = Arc::new(index);
        self.store(path, Arc::clone(&index));
        Ok(index)
    }

    /// Индекс из кеша, если он собран по той же версии файла.
    fn cached(&self, path: &Path, metadata: &std::fs::Metadata) -> Option<Arc<JournalIndex>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.tick += 1;
        let tick = state.tick;
        let entry = state.entries.get_mut(path)?;
        if !entry.index.matches(metadata) {
            return None;
        }
        entry.used = tick;
        Some(Arc::clone(&entry.index))
    }

    /// Индекс из кеша, даже если файл вырос: от него доиндексируется остаток.
    fn previous(&self, path: &Path) -> Option<Arc<JournalIndex>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .entries
            .get(path)
            .map(|entry| Arc::clone(&entry.index))
    }

    /// Кладёт индекс в кеш; самый давно использованный путь вытесняется.
    fn store(&self, path: &Path, index: Arc<JournalIndex>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.tick += 1;
        let tick = state.tick;
        state
            .entries
            .insert(path.to_path_buf(), Entry { index, used: tick });
        while state.entries.len() > MAX_INDEXED {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            state.entries.remove(&oldest);
        }
    }

    /// Замок сборки одного пути: карта живёт всё время работы окна, путей немного.
    fn build_lock(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        self.builds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.to_path_buf())
            .or_default()
            .clone()
    }
}

/// Собирает индекс: выросший файл доиндексируется, остальное собирается заново.
fn index_now(path: &Path, previous: Option<&JournalIndex>) -> Result<JournalIndex, CommandError> {
    let metadata = std::fs::metadata(path).map_err(|err| open_error(&err, path))?;
    match previous {
        // Файл стал короче — его переписали: индекс с нуля.
        Some(previous) if metadata.len() >= previous.len => JournalIndex::extend(path, previous),
        _ => JournalIndex::build(path),
    }
}

/// Сводка открытого журнала (FR-29).
///
/// # Errors
///
/// Файла нет, он не читается или это не журнал bytetap.
pub async fn open(path: &Path, cache: &JournalCache) -> Result<JournalSummary, CommandError> {
    let index = cache.index(path).await?;
    Ok(index.summary(&path.to_string_lossy()))
}

/// Длины событий ленты журнала в байтах: по ним лента считает высоты HEX-строк (AC5).
///
/// # Errors
///
/// Файла нет, он не читается или это не журнал bytetap.
pub async fn lengths(path: &Path, cache: &JournalCache) -> Result<Vec<u64>, CommandError> {
    let index = cache.index(path).await?;
    Ok(index.lengths())
}

/// Постраничное чтение событий ленты: `from` — номер события, `count` — сколько отдать.
///
/// Нумерация — по событиям ленты: заголовок `session` и итоги `session_end` в неё не входят,
/// они приходят в сводке. Строки, которых нет (файл стал короче), просто не читаются.
/// `count` ограничен [`MAX_PAGE`]: страница не превращается в чтение всего файла.
///
/// # Errors
///
/// Файла нет или он не читается.
pub async fn read_page(
    path: &Path,
    from: usize,
    count: usize,
    cache: &JournalCache,
) -> Result<Vec<FeedEvent>, CommandError> {
    let count = count.min(MAX_PAGE);
    let index = cache.index(path).await?;
    let total = index.offsets.len();
    if from >= total || count == 0 {
        return Ok(Vec::new());
    }
    let end = from.saturating_add(count).min(total);
    let offsets = index.offsets[from..end].to_vec();
    let reading = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_at(&reading, &offsets))
        .await
        .map_err(|err| {
            CommandError::new(
                ErrorCode::JournalOpen,
                format!("чтение журнала упало: {err}"),
            )
        })?
}

/// Читает строки по смещениям и превращает их в события ленты.
fn read_at(path: &Path, offsets: &[u64]) -> Result<Vec<FeedEvent>, CommandError> {
    let file = std::fs::File::open(path).map_err(|err| open_error(&err, path))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::with_capacity(offsets.len());
    let mut buffer = Vec::new();
    for offset in offsets {
        reader
            .seek(io::SeekFrom::Start(*offset))
            .map_err(|err| open_error(&err, path))?;
        buffer.clear();
        reader
            .read_until(b'\n', &mut buffer)
            .map_err(|err| open_error(&err, path))?;
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(trim_line(&buffer)) else {
            continue;
        };
        let Ok(event) = Event::deserialize(&value) else {
            continue;
        };
        if let Some(event) = FeedEvent::from_session(&event) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Дописывает комментарий в остановленный журнал (FR-23).
///
/// # Errors
///
/// Журнал идёт или занят другой программой, файл оборван, это не журнал bytetap.
pub fn append_note(path: &Path, text: &str) -> Result<(), CommandError> {
    let note = Note {
        t: now_rfc3339(),
        mono_us: None,
        text: text.to_owned(),
    };
    bytetap_core::journal::append_note(path, &note).map_err(|err| journal_error(&err, path))
}

/// Ошибка записи в журнал с путём в подстановках.
fn journal_error(err: &io::Error, path: &Path) -> CommandError {
    CommandError::from_io_with(ErrorCode::Journal, err).with_param("path", path.to_string_lossy())
}

/// Ошибка открытия или чтения журнала: отдельный код, чтобы интерфейс не писал
/// «не удалось записать журнал» там, где файл просто не открылся (AC10).
fn open_error(err: &io::Error, path: &Path) -> CommandError {
    CommandError::from_io_with(ErrorCode::JournalOpen, err)
        .with_param("path", path.to_string_lossy())
}

/// Строка без завершающего перевода строки.
fn trim_line(buffer: &[u8]) -> &[u8] {
    let mut end = buffer.len();
    while end > 0 && matches!(buffer[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &buffer[..end]
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Autoresponder, Data, Host, Mode, Session};
    use serde_json::json;

    use super::*;
    use crate::api::SessionStats;

    /// Синтетический журнал из `tests/fixtures/journal`.
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/journal/minimal.jsonl")
            .canonicalize()
            .expect("фикстура журнала")
    }

    /// Копия фикстуры во временной папке: тесты не меняют файл в репозитории.
    fn fixture_copy(dir: &tempfile::TempDir) -> PathBuf {
        let copy = dir.path().join("copy.jsonl");
        std::fs::copy(fixture(), &copy).expect("копия фикстуры");
        copy
    }

    /// Синтетическая сессия с фрагментами в заданные моменты монотонных часов.
    fn synthetic(dir: &tempfile::TempDir, samples: &[(u64, Dir, usize)]) -> PathBuf {
        let path = dir.path().join("synthetic.jsonl");
        let mut text = String::new();
        let header = Event::Session(Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            json!({"listen": "127.0.0.1:5000"}),
            Host {
                os: "Linux".to_owned(),
            },
        ));
        text.push_str(&serde_json::to_string(&header).unwrap());
        text.push('\n');
        for (index, (mono_us, dir, len)) in samples.iter().enumerate() {
            let payload = vec![b'A'; *len];
            let event = Event::Data(Data::new(
                "2026-10-19T10:15:03.000000+03:00",
                *mono_us,
                u32::try_from(index).unwrap() + 1,
                *dir,
                false,
                &payload,
            ));
            text.push_str(&serde_json::to_string(&event).unwrap());
            text.push('\n');
        }
        std::fs::write(&path, text).expect("синтетический журнал");
        path
    }

    #[tokio::test]
    async fn the_fixture_gives_a_summary_with_density_and_stats() {
        let cache = JournalCache::default();
        let summary = open(&fixture(), &cache).await.expect("сводка");

        assert_eq!(summary.events, 5, "open, три data и close");
        assert_eq!(summary.duration_us, 1_100_000, "по session_end");
        assert_eq!(
            summary.stats,
            SessionStats {
                connections: 1,
                bytes_analyzer_to_lis: 136,
                bytes_lis_to_analyzer: 76,
            }
        );
        assert_eq!(summary.header["mode"], "tcp_server");
        assert_eq!(summary.header["analyzer"], "ANALYZER");

        assert_eq!(summary.density.a2l.len(), DENSITY_BUCKETS);
        assert_eq!(summary.density.l2a.len(), DENSITY_BUCKETS);
        assert_eq!(summary.density.a2l.iter().sum::<u64>(), 136);
        assert_eq!(summary.density.l2a.iter().sum::<u64>(), 76);
        assert_eq!(summary.density.a2l[27], 136, "50 мс от 1,1 с");
        assert_eq!(summary.density.l2a[38], 76, "70 мс от 1,1 с");
    }

    #[tokio::test]
    async fn the_fixture_is_read_in_pages_by_event_index() {
        let cache = JournalCache::default();
        let path = fixture();

        let page = read_page(&path, 0, 100, &cache).await.expect("страница");
        assert_eq!(page.len(), 5);
        assert!(matches!(page[0], FeedEvent::Open { conn: 1, .. }));
        assert!(matches!(page[4], FeedEvent::Close { conn: 1, .. }));

        let middle = read_page(&path, 2, 2, &cache).await.expect("страница");
        assert_eq!(middle.len(), 2);
        assert!(matches!(middle[0], FeedEvent::Data { conn: 1, .. }));
        assert!(matches!(middle[1], FeedEvent::Data { conn: 1, .. }));

        assert!(
            read_page(&path, 5, 10, &cache)
                .await
                .expect("за краем")
                .is_empty(),
            "за последним событием ничего нет"
        );
        assert!(
            read_page(&path, 0, 0, &cache)
                .await
                .expect("ноль")
                .is_empty(),
            "count 0 — пусто"
        );
    }

    #[tokio::test]
    async fn lengths_of_the_fixture_are_ready_for_hex_rows() {
        let cache = JournalCache::default();
        let lens = lengths(&fixture(), &cache).await.expect("длины");
        assert_eq!(
            lens,
            vec![0, 20, 116, 76, 0],
            "data-события и служебные строки"
        );
    }

    #[tokio::test]
    async fn a_growing_journal_is_indexed_from_the_last_offset() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(&dir, &[(0, Dir::AnalyzerToLis, 10)]);
        let cache = JournalCache::default();

        assert_eq!(open(&path, &cache).await.expect("сводка").events, 1);
        let first = cache.index(&path).await.expect("индекс");
        append_note(&path, "дописано после остановки").expect("комментарий");

        let summary = open(&path, &cache).await.expect("сводка после заметки");
        assert_eq!(summary.events, 2, "доиндексирован только хвост");
        let second = cache.index(&path).await.expect("индекс после заметки");
        assert_eq!(
            second.offsets[..1],
            first.offsets[..],
            "смещения начала файла не пересобирались"
        );
        assert_eq!(second.lens, vec![10, 0], "длина data сохранена, у note — 0");
    }

    #[tokio::test]
    async fn a_partially_written_line_is_indexed_when_it_is_finished() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("growing.jsonl");
        let header = Event::Session(Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            json!({"listen": "127.0.0.1:5000"}),
            Host {
                os: "Linux".to_owned(),
            },
        ));
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            1000,
            1,
            Dir::AnalyzerToLis,
            false,
            b"MSH",
        ));
        let line = serde_json::to_string(&event).unwrap();
        let text = format!("{}\n{}", serde_json::to_string(&header).unwrap(), line);
        std::fs::write(&path, &text).expect("файл без перевода строки в конце");

        let cache = JournalCache::default();
        assert_eq!(
            open(&path, &cache).await.expect("сводка").events,
            0,
            "недописанная строка ещё не событие"
        );

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("дописывание");
        std::io::Write::write_all(&mut file, b"\n").expect("перевод строки");
        assert_eq!(
            open(&path, &cache).await.expect("сводка").events,
            1,
            "строка доиндексирована после завершения"
        );
    }

    #[test]
    fn a_line_longer_than_the_limit_is_broken() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("broken.jsonl");
        let header = Event::Session(Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            json!({}),
            Host {
                os: "Linux".to_owned(),
            },
        ));
        let mut text = serde_json::to_string(&header).unwrap();
        text.push('\n');
        text.push_str(&"x".repeat(300));
        text.push('\n');
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            1000,
            1,
            Dir::AnalyzerToLis,
            false,
            b"MSH",
        ));
        text.push_str(&serde_json::to_string(&event).unwrap());
        text.push('\n');
        std::fs::write(&path, text).expect("файл");

        // Заголовок короче предела, а мусорная строка — длиннее.
        let index = JournalIndex::build_with_limit(&path, 200).expect("индекс");
        assert_eq!(
            index.lengths(),
            vec![3],
            "длинная строка пропущена, следующая прочитана"
        );
    }

    #[tokio::test]
    async fn the_cache_keeps_at_most_the_allowed_number_of_indexes() {
        let dir = tempfile::tempdir().expect("временная папка");
        let cache = JournalCache::default();
        for index in 0..MAX_INDEXED + 2 {
            let path = dir.path().join(format!("j{index}.jsonl"));
            std::fs::copy(fixture(), &path).expect("копия");
            open(&path, &cache).await.expect("сводка");
        }
        let entries = cache
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .len();
        assert_eq!(entries, MAX_INDEXED, "LRU вытеснил лишние индексы");
    }

    #[tokio::test]
    async fn one_index_is_built_once_for_parallel_calls() {
        let cache = JournalCache::default();
        let path = fixture();
        let (first, second) = tokio::join!(cache.index(&path), cache.index(&path));
        let first = first.expect("первый индекс");
        let second = second.expect("второй индекс");
        assert!(
            Arc::ptr_eq(&first, &second),
            "второй вызов взял тот же индекс"
        );
    }

    #[tokio::test]
    async fn density_covers_the_whole_session() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(
            &dir,
            &[
                (0, Dir::AnalyzerToLis, 10),
                (50_000_000, Dir::AnalyzerToLis, 30),
                (100_000_000, Dir::LisToAnalyzer, 20),
            ],
        );
        let cache = JournalCache::default();
        let summary = open(&path, &cache).await.expect("сводка");

        assert_eq!(summary.events, 3);
        assert_eq!(
            summary.duration_us, 100_000_000,
            "итогов нет — по последнему"
        );
        assert_eq!(
            summary.stats,
            SessionStats {
                connections: 0,
                bytes_analyzer_to_lis: 40,
                bytes_lis_to_analyzer: 20,
            },
            "без session_end счётчики посчитаны по фрагментам"
        );
        assert_eq!(summary.density.a2l[0], 10);
        assert_eq!(summary.density.a2l[300], 30);
        assert_eq!(
            summary.density.l2a[599], 20,
            "последняя корзина, не за краем"
        );
    }

    #[tokio::test]
    async fn a_note_appended_after_the_session_rebuilds_the_index() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = fixture_copy(&dir);
        let cache = JournalCache::default();

        assert_eq!(open(&path, &cache).await.expect("сводка").events, 5);
        append_note(&path, "дописано после остановки").expect("комментарий");

        let summary = open(&path, &cache).await.expect("сводка после заметки");
        assert_eq!(
            summary.events, 6,
            "индекс пересобран по изменившемуся файлу"
        );
        let last = read_page(&path, 5, 1, &cache).await.expect("последняя");
        assert_eq!(last.len(), 1);
        match &last[0] {
            FeedEvent::Note { text, .. } => assert_eq!(text, "дописано после остановки"),
            other => panic!("ожидался note, получено {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_journal_is_refused() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "привет\n").expect("файл");
        let error = open(&path, &JournalCache::default())
            .await
            .expect_err("не журнал");
        assert_eq!(error.code, ErrorCode::JournalOpen);

        let missing = dir.path().join("missing.jsonl");
        assert_eq!(
            open(&missing, &JournalCache::default())
                .await
                .expect_err("нет файла")
                .code,
            ErrorCode::JournalOpen
        );
    }

    #[test]
    fn journal_errors_name_the_path_and_tell_reading_from_writing() {
        let write = journal_error(
            &io::Error::from(io::ErrorKind::NotFound),
            Path::new("/tmp/x"),
        );
        assert_eq!(write.code, ErrorCode::Journal);
        assert_eq!(
            write.params.as_ref().and_then(|params| params.get("path")),
            Some(&"/tmp/x".to_owned())
        );
        let read = open_error(
            &io::Error::from(io::ErrorKind::NotFound),
            Path::new("/tmp/x"),
        );
        assert_eq!(read.code, ErrorCode::JournalOpen);
    }
}
