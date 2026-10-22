//! Просмотр журнала: сводка, плотность и постраничное чтение (FR-29).
//!
//! Журнал бывает в сотни мегабайт, поэтому в память окна он не загружается. Первый проход
//! по файлу строит индекс: смещения строк событий ленты и гистограмму плотности на
//! [`DENSITY_BUCKETS`] корзин по `mono_us`. Индекс кешируется по пути и перестраивается,
//! только если файл изменился (дописан комментарий). Постраничное чтение идёт по индексу:
//! `seek` на смещение и чтение нужного числа строк.
//!
//! Всё здесь блокирующее: вызывающий сам уводит сборку индекса с потока окна.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, Metadata};
use std::hash::{Hash as _, Hasher as _};
use std::io::{self, BufRead as _, BufReader, Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use super::{Dir, Event, Note, Session, Stats, io_message, now_rfc3339};

/// Сколько корзин в гистограмме плотности: столько точек рисует трасса всей сессии.
pub const DENSITY_BUCKETS: usize = 600;

/// Сколько точек в таблице «первое событие корзины»: корзины и правый край сессии.
pub const FIRST_EVENT_POINTS: usize = DENSITY_BUCKETS + 1;

/// Сколько байт начала файла сверяет доиндексация: файл на этом пути могли заменить.
///
/// Хеш первых килобайт и перевод строки на прежнем конце файла показывают, что доиндексируется
/// тот же журнал, а не другой файл, у которого случайно совпало начало.
pub const HEADER_CHECK_BYTES: u64 = 4 * 1024;

/// Сколько строк максимум отдаёт одна страница журнала.
///
/// Интерфейс просит по 500 строк; больший `count` обрезается: страница не должна
/// превращаться в чтение всего файла.
pub const MAX_PAGE: usize = 1000;

/// Предел длины строки журнала: файл с такой строкой считается битым.
///
/// Строка события — один `data` с base64: даже фрагмент в мегабайт занимает единицы
/// мегабайт JSON. Шестьдесят четыре мегабайта — заведомо испорченный файл, и читать его
/// целиком в память нельзя.
pub const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;

/// Сколько индексов журналов держит кеш (LRU).
pub const MAX_INDEXED: usize = 8;

/// Гистограмма плотности: байты по направлениям на [`DENSITY_BUCKETS`] корзин.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Density {
    /// Анализатор → ЛИС.
    pub a2l: Vec<u64>,
    /// ЛИС → анализатор.
    pub l2a: Vec<u64>,
}

/// Сводка открытого журнала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalSummary {
    /// Путь к файлу.
    pub path: PathBuf,
    /// Заголовок `session`.
    pub header: Session,
    /// Число событий ленты: всё, кроме `session` и `session_end`.
    pub events: usize,
    /// Длительность по монотонным часам, микросекунды.
    pub duration_us: u64,
    /// Итоги из `session_end`, а у оборванного журнала — подсчёт по событиям.
    pub stats: Stats,
    /// Гистограмма плотности.
    pub density: Density,
    /// Номер первого события в каждой из [`FIRST_EVENT_POINTS`] точек трассы.
    pub first_event: Vec<usize>,
}

/// Почему журнал не открылся.
#[derive(Debug)]
pub enum ViewErrorKind {
    /// Файл не открылся или не прочитался.
    Io(io::Error),
    /// В файле нет заголовка `session`: это не журнал bytetap.
    NotAJournal,
}

/// Ошибка просмотра журнала с путём к файлу.
#[derive(Debug)]
pub struct ViewError {
    /// Путь к файлу.
    pub path: PathBuf,
    /// Что случилось.
    pub kind: ViewErrorKind,
}

impl ViewError {
    fn io(err: io::Error, path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: ViewErrorKind::Io(err),
        }
    }
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ViewErrorKind::Io(err) => write!(
                f,
                "cannot read the journal {}: {}",
                self.path.display(),
                io_message(err)
            ),
            ViewErrorKind::NotAJournal => write!(
                f,
                "not a bytetap journal: {} has no `session` header",
                self.path.display()
            ),
        }
    }
}

impl std::error::Error for ViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ViewErrorKind::Io(err) => Some(err),
            ViewErrorKind::NotAJournal => None,
        }
    }
}

/// Индекс журнала: всё, что нужно для сводки и постраничного чтения.
#[derive(Debug, Clone)]
pub struct JournalIndex {
    header: Session,
    /// Смещения в байтах у строк, которые становятся событиями ленты.
    offsets: Vec<u64>,
    /// Длина каждого события ленты в байтах: у `data` — фрагмент, у служебных — 0.
    /// По ней лента считает высоты HEX-строк, не читая файл.
    lens: Vec<u64>,
    /// Время каждого события ленты по монотонным часам: из него строится таблица корзин.
    times: Vec<u64>,
    /// Индекс первого события в каждой точке таблицы корзин.
    first_event: Vec<usize>,
    /// Хеш начала файла: по нему видно, что файл тот же.
    header_hash: u64,
    /// Сколько байт начала покрывает хеш: у файла короче [`HEADER_CHECK_BYTES`] — весь файл.
    header_hashed: u64,
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
    header: Option<Session>,
    offsets: Vec<u64>,
    lens: Vec<u64>,
    times: Vec<u64>,
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
    /// (ошибки строк там — отдельный отчёт). Недописанный хвост без перевода строки ждёт
    /// следующего прохода.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или в нём нет заголовка `session`.
    pub fn build(path: &Path) -> Result<Self, ViewError> {
        Self::build_with_limit(path, MAX_LINE_BYTES)
    }

    /// То же, что [`JournalIndex::build`], но с заданным пределом длины строки: для тестов.
    fn build_with_limit(path: &Path, limit: u64) -> Result<Self, ViewError> {
        let metadata = std::fs::metadata(path).map_err(|err| ViewError::io(err, path))?;
        let mut scan = Scan::default();
        scan_into(path, 0, limit, &mut scan)?;
        Self::assemble(path, scan, &metadata)
    }

    /// Доиндексирует выросший файл с последнего смещения, а не с начала.
    ///
    /// Дописанный комментарий (FR-23) — обычный случай: перечитывать сотни мегабайт
    /// ради одной строки незачем.
    fn extend(&self, path: &Path) -> Result<Self, ViewError> {
        let metadata = std::fs::metadata(path).map_err(|err| ViewError::io(err, path))?;
        let mut scan = Scan {
            header: Some(self.header.clone()),
            offsets: self.offsets.clone(),
            lens: self.lens.clone(),
            times: self.times.clone(),
            samples: self.samples.clone(),
            duration_us: self.duration_us,
            stats: self.stats.clone(),
            stats_from_end: self.stats_from_end,
            len: self.len,
        };
        scan_into(path, self.len, MAX_LINE_BYTES, &mut scan)?;
        Self::assemble(path, scan, &metadata)
    }

    /// Собирает индекс из прохода и метаданных файла.
    fn assemble(path: &Path, scan: Scan, metadata: &Metadata) -> Result<Self, ViewError> {
        let header = scan.header.ok_or_else(|| ViewError {
            path: path.to_path_buf(),
            kind: ViewErrorKind::NotAJournal,
        })?;
        let (header_hash, header_hashed) =
            header_hash(path, HEADER_CHECK_BYTES).map_err(|err| ViewError::io(err, path))?;
        let first_event = first_events(&scan.times, scan.duration_us);
        Ok(Self {
            header,
            offsets: scan.offsets,
            lens: scan.lens,
            times: scan.times,
            first_event,
            header_hash,
            header_hashed,
            duration_us: scan.duration_us,
            stats: scan.stats,
            stats_from_end: scan.stats_from_end,
            density: density(&scan.samples, scan.duration_us),
            samples: scan.samples,
            len: scan.len,
            modified: metadata.modified().ok(),
        })
    }

    /// Заголовок `session`.
    #[must_use]
    pub const fn header(&self) -> &Session {
        &self.header
    }

    /// Число событий ленты.
    #[must_use]
    pub fn events(&self) -> usize {
        self.offsets.len()
    }

    /// Длительность по монотонным часам, микросекунды.
    #[must_use]
    pub const fn duration_us(&self) -> u64 {
        self.duration_us
    }

    /// Итоги сессии.
    #[must_use]
    pub const fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Гистограмма плотности.
    #[must_use]
    pub const fn density(&self) -> &Density {
        &self.density
    }

    /// Номер первого события в каждой точке трассы.
    ///
    /// `first_event[b]` — номер первого события, попавшего в корзину `b` или правее;
    /// последняя точка равна числу событий. По таблице трасса переводит долю времени
    /// в номер события и обратно, не читая страницы журнала.
    #[must_use]
    pub fn first_event(&self) -> &[usize] {
        &self.first_event
    }

    /// Длины событий ленты в байтах: `data` — фрагмент, служебные строки — 0.
    #[must_use]
    pub fn lengths(&self) -> &[u64] {
        &self.lens
    }

    /// Сводка для окна.
    #[must_use]
    pub fn summary(&self, path: &Path) -> JournalSummary {
        JournalSummary {
            path: path.to_path_buf(),
            header: self.header.clone(),
            events: self.offsets.len(),
            duration_us: self.duration_us,
            stats: self.stats.clone(),
            density: self.density.clone(),
            first_event: self.first_event.clone(),
        }
    }

    /// Постраничное чтение событий ленты: `from` — номер события, `count` — сколько отдать.
    ///
    /// Нумерация — по событиям ленты: заголовок `session` и итоги `session_end` в неё
    /// не входят, они есть в сводке. Строки, которых нет (файл стал короче) или которые
    /// больше не разбираются, пропускаются. `count` ограничен [`MAX_PAGE`].
    ///
    /// # Errors
    ///
    /// Файла нет или он не читается.
    pub fn read(&self, path: &Path, from: usize, count: usize) -> Result<Vec<Event>, ViewError> {
        let count = count.min(MAX_PAGE);
        let total = self.offsets.len();
        if from >= total || count == 0 {
            return Ok(Vec::new());
        }
        let end = from.saturating_add(count).min(total);
        read_at(path, &self.offsets[from..end])
    }

    /// Индекс собран по этой версии файла.
    fn matches(&self, metadata: &Metadata) -> bool {
        self.len == metadata.len() && self.modified == metadata.modified().ok()
    }
}

/// Читает строку не длиннее `limit` байт; `None` — конец файла.
///
/// Возвращает число прочитанных байт вместе с переводом строки и говорит, уложилась ли
/// строка в предел.
fn read_limited(
    reader: &mut BufReader<File>,
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
fn discard_line(reader: &mut BufReader<File>) -> io::Result<u64> {
    let mut dropped = 0u64;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(dropped);
        }
        if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            reader.consume(position + 1);
            return Ok(dropped + position as u64 + 1);
        }
        let len = chunk.len();
        reader.consume(len);
        dropped += len as u64;
    }
}

/// Проход по файлу с `start`: заголовок, события, плотность и смещение после целой строки.
fn scan_into(path: &Path, start: u64, limit: u64, scan: &mut Scan) -> Result<(), ViewError> {
    let file = File::open(path).map_err(|err| ViewError::io(err, path))?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        reader
            .seek(io::SeekFrom::Start(start))
            .map_err(|err| ViewError::io(err, path))?;
    }
    let mut offset = start;
    let mut buffer = Vec::new();
    // Последнее известное время: доиндексация продолжает ряд, а не начинает его заново.
    let mut last = scan.times.last().copied().unwrap_or(0);
    loop {
        buffer.clear();
        let Some((read, fits)) = read_limited(&mut reader, &mut buffer, limit)
            .map_err(|err| ViewError::io(err, path))?
        else {
            break;
        };
        let line_start = offset;
        offset += read;
        if !fits {
            if !buffer.ends_with(b"\n") {
                // Строка длиннее предела — битая: остаток отбрасываем, чтобы сбойный файл
                // не съел память.
                offset += discard_line(&mut reader).map_err(|err| ViewError::io(err, path))?;
            }
            continue;
        }
        if !buffer.ends_with(b"\n") {
            // Хвост без перевода строки ещё пишется: индексируем его в следующий раз.
            offset = line_start;
            break;
        }
        let Ok(event) = serde_json::from_slice::<Event>(trim_line(&buffer)) else {
            continue;
        };
        let mono_us = event_time(&event);
        let mut len = 0u64;
        match event {
            Event::Session(session) => {
                if scan.header.is_none() {
                    scan.header = Some(session);
                }
                continue;
            }
            Event::SessionEnd(end) => {
                scan.duration_us = scan.duration_us.max(end.mono_us);
                scan.stats = end.stats;
                scan.stats_from_end = true;
                continue;
            }
            Event::Open(_) => {
                if !scan.stats_from_end {
                    scan.stats.connections += 1;
                }
            }
            Event::Data(data) => {
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
            Event::Close(_) | Event::Error(_) | Event::Info(_) | Event::Note(_) => {}
        }
        if let Some(mono_us) = mono_us {
            scan.duration_us = scan.duration_us.max(mono_us);
        }
        // Время растёт вместе с событиями; у комментария без `mono_us` оно прежнее.
        let time = mono_us.unwrap_or(last).max(last);
        last = time;
        scan.times.push(time);
        scan.offsets.push(line_start);
        scan.lens.push(len);
    }
    scan.len = offset;
    Ok(())
}

/// Корзина плотности для момента `mono_us` на сессии длиной `duration_us`.
fn bucket_of(mono_us: u64, duration_us: u64) -> usize {
    // Последний фрагмент попадает в последнюю корзину, а не за её край.
    // Сессия без длительности (`duration_us == 0`) — всё в первую корзину.
    mono_us
        .min(duration_us)
        .saturating_mul(DENSITY_BUCKETS as u64)
        .checked_div(duration_us)
        .and_then(|bucket| usize::try_from(bucket).ok())
        .unwrap_or(0)
        .min(DENSITY_BUCKETS - 1)
}

/// Гистограмма плотности: байты по направлениям на [`DENSITY_BUCKETS`] корзин.
fn density(samples: &[Sample], duration_us: u64) -> Density {
    let mut density = Density {
        a2l: vec![0; DENSITY_BUCKETS],
        l2a: vec![0; DENSITY_BUCKETS],
    };
    for sample in samples {
        let bucket = bucket_of(sample.mono_us, duration_us);
        let track = match sample.dir {
            Dir::AnalyzerToLis => &mut density.a2l,
            Dir::LisToAnalyzer => &mut density.l2a,
        };
        track[bucket] += sample.len;
    }
    density
}

/// Индекс первого события в каждой точке таблицы корзин.
fn first_events(times: &[u64], duration_us: u64) -> Vec<usize> {
    let mut starts = vec![times.len(); FIRST_EVENT_POINTS];
    let mut point = 0;
    for (event, &time) in times.iter().enumerate() {
        let bucket = bucket_of(time, duration_us);
        while point <= bucket {
            starts[point] = event;
            point += 1;
        }
    }
    starts
}

/// Время события по монотонным часам; `None` — у события времени нет.
const fn event_time(event: &Event) -> Option<u64> {
    match event {
        Event::Data(data) => Some(data.mono_us),
        Event::Open(open) => Some(open.mono_us),
        Event::Close(close) => Some(close.mono_us),
        Event::Error(error) => Some(error.mono_us),
        Event::Info(info) => Some(info.mono_us),
        Event::Note(note) => note.mono_us,
        Event::Session(_) | Event::SessionEnd(_) => None,
    }
}

/// Хеш первых `limit` байт файла и сколько байт в него вошло.
fn header_hash(path: &Path, limit: u64) -> io::Result<(u64, u64)> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Ok((hasher.finish(), bytes.len() as u64))
}

/// Доиндексируется ли тот же журнал: начало файла совпало, а прежний конец — на границе строк.
///
/// Начало сверяется на той же длине, что и при сборке: у короткого журнала хеш покрывал
/// весь файл, и дописанная строка не должна выглядеть подменой.
fn continuation(path: &Path, index: &JournalIndex) -> bool {
    if index.len == 0 {
        return false;
    }
    match header_hash(path, index.header_hashed) {
        Ok((hash, hashed)) if hash == index.header_hash && hashed == index.header_hashed => {}
        _ => return false,
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    if file.seek(io::SeekFrom::Start(index.len - 1)).is_err() {
        return false;
    }
    let mut byte = [0u8; 1];
    matches!(file.read_exact(&mut byte), Ok(()) if byte[0] == b'\n')
}

/// Собирает индекс: выросший тот же файл доиндексируется, остальное собирается заново.
///
/// Файл на прежнем пути мог быть заменён другим журналом: тогда прежние смещения указывают
/// в чужие строки, и доиндексация склеила бы два разных журнала. Проверка — по началу файла
/// и по переводу строки на прежнем конце ([`continuation`]).
fn index_now(path: &Path, previous: Option<&JournalIndex>) -> Result<JournalIndex, ViewError> {
    let metadata = std::fs::metadata(path).map_err(|err| ViewError::io(err, path))?;
    match previous {
        Some(previous) if metadata.len() >= previous.len && continuation(path, previous) => {
            previous.extend(path)
        }
        // Файл стал короче, заменён или обрезан посреди строки — индекс с нуля.
        _ => JournalIndex::build(path),
    }
}

/// Читает строки по смещениям и превращает их в события ленты.
fn read_at(path: &Path, offsets: &[u64]) -> Result<Vec<Event>, ViewError> {
    let file = File::open(path).map_err(|err| ViewError::io(err, path))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::with_capacity(offsets.len());
    let mut buffer = Vec::new();
    for offset in offsets {
        reader
            .seek(io::SeekFrom::Start(*offset))
            .map_err(|err| ViewError::io(err, path))?;
        buffer.clear();
        let Some((_, fits)) = read_limited(&mut reader, &mut buffer, MAX_LINE_BYTES)
            .map_err(|err| ViewError::io(err, path))?
        else {
            continue;
        };
        if !fits {
            continue;
        }
        match serde_json::from_slice::<Event>(trim_line(&buffer)) {
            Ok(Event::Session(_) | Event::SessionEnd(_)) | Err(_) => {}
            Ok(event) => events.push(event),
        }
    }
    Ok(events)
}

/// Строка без завершающего перевода строки.
fn trim_line(buffer: &[u8]) -> &[u8] {
    let mut end = buffer.len();
    while end > 0 && matches!(buffer[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &buffer[..end]
}

/// Дописывает комментарий инженера в остановленный журнал (FR-23).
///
/// # Errors
///
/// Журнал идёт или занят другой программой, файл оборван, это не журнал bytetap.
pub fn append_note(path: &Path, text: &str) -> io::Result<()> {
    let note = Note {
        t: now_rfc3339(),
        mono_us: None,
        text: text.to_owned(),
    };
    super::append_note(path, &note)
}

/// Кеш индексов журналов: LRU на [`MAX_INDEXED`] путей.
#[derive(Debug, Default)]
pub struct JournalCache {
    state: Mutex<CacheState>,
    /// Замок сборки на путь: два окна не собирают один индекс параллельно.
    builds: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
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
    /// Вызов блокирующий: сборка индекса большого журнала читает весь файл.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn index(&self, path: &Path) -> Result<Arc<JournalIndex>, ViewError> {
        let metadata = std::fs::metadata(path).map_err(|err| ViewError::io(err, path))?;
        if let Some(index) = self.cached(path, &metadata) {
            return Ok(index);
        }

        // Один путь собирается одним вызовом: второй ждёт и берёт готовый индекс.
        let lock = self.build_lock(path);
        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let metadata = std::fs::metadata(path).map_err(|err| ViewError::io(err, path))?;
        if let Some(index) = self.cached(path, &metadata) {
            return Ok(index);
        }
        let previous = self.previous(path);
        let index = Arc::new(index_now(path, previous.as_deref())?);
        self.store(path, Arc::clone(&index));
        Ok(index)
    }

    /// Сводка открытого журнала.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn open(&self, path: &Path) -> Result<JournalSummary, ViewError> {
        Ok(self.index(path)?.summary(path))
    }

    /// Страница событий ленты: см. [`JournalIndex::read`].
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn read(&self, path: &Path, from: usize, count: usize) -> Result<Vec<Event>, ViewError> {
        self.index(path)?.read(path, from, count)
    }

    /// Индекс из кеша, если он собран по той же версии файла.
    fn cached(&self, path: &Path, metadata: &Metadata) -> Option<Arc<JournalIndex>> {
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
    fn build_lock(&self, path: &Path) -> Arc<Mutex<()>> {
        self.builds
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.to_path_buf())
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;
    use crate::journal::{Autoresponder, ClosedBy, Data, Host, Mode, Open, SessionEnd};

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

    /// Строка JSON с переводом строки.
    fn line(event: &Event) -> String {
        let mut text = serde_json::to_string(event).unwrap();
        text.push('\n');
        text
    }

    /// Заголовок синтетической сессии с моделью анализатора.
    fn header(analyzer: &str) -> Event {
        let mut session = Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            json!({"listen": "127.0.0.1:5000"}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        session.analyzer = Some(analyzer.to_owned());
        Event::Session(session)
    }

    /// Фрагмент `data` соединения `conn` в момент `mono_us`.
    fn data(mono_us: u64, conn: u32, dir: Dir, len: usize) -> Event {
        Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            mono_us,
            conn,
            dir,
            false,
            &vec![b'A'; len],
        ))
    }

    /// Текст журнала: заголовок и фрагменты по моментам.
    fn journal_text(analyzer: &str, samples: &[(u64, Dir, usize)]) -> String {
        let mut text = line(&header(analyzer));
        for (index, (mono_us, dir, len)) in samples.iter().enumerate() {
            text.push_str(&line(&data(
                *mono_us,
                u32::try_from(index).unwrap() + 1,
                *dir,
                *len,
            )));
        }
        text
    }

    /// Журнал по указанному пути: заголовок с моделью анализатора и фрагменты по моментам.
    fn write_journal(path: &Path, analyzer: &str, samples: &[(u64, Dir, usize)]) {
        std::fs::write(path, journal_text(analyzer, samples)).expect("журнал");
    }

    /// Синтетическая сессия с фрагментами в заданные моменты монотонных часов.
    fn synthetic(dir: &tempfile::TempDir, samples: &[(u64, Dir, usize)]) -> PathBuf {
        let path = dir.path().join("synthetic.jsonl");
        write_journal(&path, "ANALYZER", samples);
        path
    }

    /// Дописывает байты в конец файла.
    fn append(path: &Path, bytes: &[u8]) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("дописывание");
        file.write_all(bytes).expect("запись");
    }

    #[test]
    fn the_fixture_gives_a_summary_with_density_and_stats() {
        let cache = JournalCache::default();
        let summary = cache.open(&fixture()).expect("сводка");

        assert_eq!(summary.events, 5, "open, три data и close");
        assert_eq!(summary.duration_us, 1_100_000, "по session_end");
        assert_eq!(
            summary.stats,
            Stats {
                connections: 1,
                bytes_analyzer_to_lis: 136,
                bytes_lis_to_analyzer: 76,
            }
        );
        assert_eq!(summary.header.mode, Mode::TcpServer);
        assert_eq!(summary.header.analyzer.as_deref(), Some("ANALYZER"));

        assert_eq!(summary.density.a2l.len(), DENSITY_BUCKETS);
        assert_eq!(summary.density.l2a.len(), DENSITY_BUCKETS);
        assert_eq!(summary.density.a2l.iter().sum::<u64>(), 136);
        assert_eq!(summary.density.l2a.iter().sum::<u64>(), 76);
        assert_eq!(summary.density.a2l[27], 136, "50 мс от 1,1 с");
        assert_eq!(summary.density.l2a[38], 76, "70 мс от 1,1 с");
    }

    #[test]
    fn the_fixture_is_read_in_pages_by_event_index() {
        let cache = JournalCache::default();
        let path = fixture();

        let page = cache.read(&path, 0, 100).expect("страница");
        assert_eq!(page.len(), 5);
        assert!(matches!(&page[0], Event::Open(open) if open.conn == 1));
        assert!(matches!(&page[4], Event::Close(close) if close.conn == 1));

        let middle = cache.read(&path, 2, 2).expect("страница");
        assert_eq!(middle.len(), 2);
        assert!(matches!(&middle[0], Event::Data(data) if data.conn == 1));
        assert!(matches!(&middle[1], Event::Data(data) if data.conn == 1));

        assert!(
            cache.read(&path, 5, 10).expect("за краем").is_empty(),
            "за последним событием ничего нет"
        );
        assert!(
            cache.read(&path, 0, 0).expect("ноль").is_empty(),
            "count 0 — пусто"
        );
    }

    #[test]
    fn page_bounds_are_clamped_to_the_events_and_the_page_limit() {
        let dir = tempfile::tempdir().expect("временная папка");
        let samples: Vec<_> = (0..MAX_PAGE as u64 + 200)
            .map(|index| (index, Dir::AnalyzerToLis, 1))
            .collect();
        let path = synthetic(&dir, &samples);
        let cache = JournalCache::default();
        let total = samples.len();

        assert_eq!(
            cache.read(&path, 0, usize::MAX).expect("страница").len(),
            MAX_PAGE,
            "больше MAX_PAGE за раз не отдаётся"
        );
        let tail = cache.read(&path, total - 3, 10).expect("хвост");
        assert_eq!(tail.len(), 3, "страница обрезана по последнему событию");
        assert!(
            matches!(&tail[2], Event::Data(data) if data.conn == u32::try_from(total).unwrap()),
            "последнее событие — последний фрагмент"
        );
        assert_eq!(cache.read(&path, total - 1, 1).expect("одно").len(), 1);
        assert!(cache.read(&path, total, 1).expect("край").is_empty());
        assert!(
            cache
                .read(&path, usize::MAX, usize::MAX)
                .expect("далеко")
                .is_empty(),
            "from + count не переполняется"
        );
    }

    #[test]
    fn lengths_of_the_fixture_are_ready_for_hex_rows() {
        let cache = JournalCache::default();
        let index = cache.index(&fixture()).expect("индекс");
        assert_eq!(
            index.lengths(),
            [0, 20, 116, 76, 0],
            "data-события и служебные строки"
        );
    }

    #[test]
    fn a_growing_journal_is_indexed_from_the_last_offset() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(&dir, &[(0, Dir::AnalyzerToLis, 10)]);
        let cache = JournalCache::default();

        assert_eq!(cache.open(&path).expect("сводка").events, 1);
        let first = cache.index(&path).expect("индекс");
        append_note(&path, "дописано после остановки").expect("комментарий");

        let summary = cache.open(&path).expect("сводка после заметки");
        assert_eq!(summary.events, 2, "доиндексирован только хвост");
        let second = cache.index(&path).expect("индекс после заметки");
        assert_eq!(
            second.offsets[..1],
            first.offsets[..],
            "смещения начала файла не пересобирались"
        );
        assert_eq!(
            second.lengths(),
            [10, 0],
            "длина data сохранена, у note — 0"
        );
    }

    /// Доиндексация не перечитывает начало: строка, испорченная после первой сборки,
    /// остаётся в индексе, а пересборка с нуля её бы пропустила.
    #[test]
    fn a_long_growing_journal_is_extended_without_rereading_the_start() {
        let dir = tempfile::tempdir().expect("временная папка");
        let samples: Vec<_> = (0..40)
            .map(|index| (index * 1_000, Dir::AnalyzerToLis, 64))
            .collect();
        let path = synthetic(&dir, &samples);
        let cache = JournalCache::default();
        let first = cache.index(&path).expect("индекс");
        assert_eq!(first.events(), 40);

        // Строка за пределом сверяемого начала заменена пробелами той же длины.
        let mut bytes = std::fs::read(&path).expect("журнал");
        let victim = first
            .offsets
            .iter()
            .position(|offset| *offset > HEADER_CHECK_BYTES)
            .expect("строка за пределом начала");
        let start = usize::try_from(first.offsets[victim]).unwrap();
        let end = usize::try_from(first.offsets[victim + 1]).unwrap() - 1;
        bytes[start..end].fill(b' ');
        std::fs::write(&path, &bytes).expect("порча строки");
        append(
            &path,
            line(&data(50_000, 41, Dir::LisToAnalyzer, 7)).as_bytes(),
        );

        let second = cache.index(&path).expect("доиндексация");
        assert_eq!(second.events(), 41, "начало не перечитывалось");
        assert_eq!(second.offsets[..40], first.offsets[..]);
        assert_eq!(second.lengths()[40], 7);
        assert_eq!(second.duration_us(), 50_000);
        assert_eq!(second.stats().bytes_lis_to_analyzer, 7);
        assert_eq!(
            second.first_event().len(),
            FIRST_EVENT_POINTS,
            "таблица корзин пересобрана"
        );

        // Новый кеш собирает с нуля: испорченная строка пропущена.
        assert_eq!(
            JournalCache::default().open(&path).expect("сводка").events,
            40
        );
    }

    #[test]
    fn a_partially_written_line_is_indexed_when_it_is_finished() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("growing.jsonl");
        let text = format!(
            "{}{}",
            line(&header("ANALYZER")),
            serde_json::to_string(&data(1000, 1, Dir::AnalyzerToLis, 3)).unwrap()
        );
        std::fs::write(&path, &text).expect("файл без перевода строки в конце");

        let cache = JournalCache::default();
        assert_eq!(
            cache.open(&path).expect("сводка").events,
            0,
            "недописанная строка ещё не событие"
        );

        append(&path, b"\n");
        assert_eq!(
            cache.open(&path).expect("сводка").events,
            1,
            "строка доиндексирована после завершения"
        );
    }

    /// Отказ журнала обрывает файл без `session_end`, а иногда и посреди строки:
    /// просмотр открывает то, что успело записаться (FR-29, NFR-5).
    /// Журнал прокси формата `/2` открывается просмотром так же, как `/1` (M4-AC9).
    #[test]
    fn a_proxy_journal_of_the_second_format_opens() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("proxy.jsonl");
        let session = Session::new(
            Mode::Proxy,
            Autoresponder::None,
            json!({"listen": "127.0.0.1:50025", "target": "127.0.0.1:5000", "role": "analyzer_client"}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        let mut text = line(&Event::Session(session));
        text.push_str(&line(&Event::Open(Open {
            t: "2026-10-22T10:15:01.000000+03:00".to_owned(),
            mono_us: 1000,
            conn: 1,
            peer: "127.0.0.1:49152".to_owned(),
            local: "127.0.0.1:50025".to_owned(),
            link: Some(crate::journal::Link {
                role: crate::journal::LinkRole::Lis,
                peer: "127.0.0.1:5000".to_owned(),
                local: "127.0.0.1:50026".to_owned(),
            }),
        })));
        text.push_str(&line(&data(2000, 1, Dir::AnalyzerToLis, 16)));
        text.push_str(&line(&Event::Close(crate::journal::Close {
            t: "2026-10-22T10:15:02.000000+03:00".to_owned(),
            mono_us: 3000,
            conn: 1,
            by: ClosedBy::Analyzer,
            reason: "eof".to_owned(),
        })));
        std::fs::write(&path, text).expect("журнал прокси");

        let summary = JournalCache::default().open(&path).expect("сводка");
        assert_eq!(summary.header.mode, Mode::Proxy);
        assert_eq!(summary.header.format, crate::journal::FORMAT);
        assert_eq!(summary.events, 3, "open, data и close");
        assert_eq!(summary.stats.connections, 1);
        assert_eq!(summary.duration_us, 3000);
        let page = JournalCache::default()
            .read(&path, 0, 10)
            .expect("страница журнала");
        assert!(matches!(&page[0], Event::Open(open) if open.link.is_some()));
    }

    #[test]
    fn a_journal_interrupted_by_a_failure_still_opens() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("interrupted.jsonl");
        let mut text = line(&header("ANALYZER"));
        text.push_str(&line(&Event::Open(Open {
            t: "2026-10-19T10:15:01.000000+03:00".to_owned(),
            mono_us: 500,
            conn: 1,
            peer: "192.168.1.50:49152".to_owned(),
            local: "127.0.0.1:5000".to_owned(),
            link: None,
        })));
        text.push_str(&line(&data(0, 1, Dir::AnalyzerToLis, 10)));
        text.push_str(&line(&data(1_000, 2, Dir::LisToAnalyzer, 20)));
        text.push_str(r#"{"type":"data","#);
        std::fs::write(&path, text).expect("оборванный журнал");

        let cache = JournalCache::default();
        let summary = cache.open(&path).expect("оборванный журнал открывается");
        assert_eq!(summary.events, 3, "целые строки до обрыва видны");
        assert_eq!(
            summary.duration_us, 1_000,
            "итогов нет — длительность по последнему фрагменту"
        );
        assert_eq!(
            summary.stats,
            Stats {
                connections: 1,
                bytes_analyzer_to_lis: 10,
                bytes_lis_to_analyzer: 20,
            },
            "итогов нет — счётчики по событиям"
        );
        assert_eq!(summary.density.a2l[0], 10);
        assert_eq!(summary.density.l2a[DENSITY_BUCKETS - 1], 20);
        let page = cache.read(&path, 0, 10).expect("страница");
        assert_eq!(page.len(), 3, "обрывок строки не читается");
    }

    #[test]
    fn a_line_longer_than_the_limit_is_broken() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("broken.jsonl");
        let mut text = line(&header("A"));
        text.push_str(&"x".repeat(300));
        text.push('\n');
        text.push_str(&line(&data(1000, 1, Dir::AnalyzerToLis, 3)));
        std::fs::write(&path, text).expect("файл");

        // Заголовок короче предела, а мусорная строка — длиннее.
        let index = JournalIndex::build_with_limit(&path, 250).expect("индекс");
        assert_eq!(
            index.lengths(),
            [3],
            "длинная строка пропущена, следующая прочитана"
        );
    }

    /// Строка ровно на байт длиннее предела вместе с переводом строки: следующая строка
    /// не должна пропасть вместе с ней.
    #[test]
    fn a_line_just_over_the_limit_does_not_swallow_the_next_one() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("edge.jsonl");
        let limit = 250u64;
        let mut text = line(&header("A"));
        text.push_str(&"x".repeat(usize::try_from(limit).unwrap()));
        text.push('\n');
        text.push_str(&line(&data(1000, 1, Dir::AnalyzerToLis, 3)));
        std::fs::write(&path, text).expect("файл");

        let index = JournalIndex::build_with_limit(&path, limit).expect("индекс");
        assert_eq!(index.lengths(), [3]);
        assert_eq!(index.len, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn a_long_header_line_is_not_a_journal() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("long-header.jsonl");
        std::fs::write(&path, journal_text("A", &[(0, Dir::AnalyzerToLis, 3)])).expect("файл");

        let error = JournalIndex::build_with_limit(&path, 50).expect_err("заголовок за пределом");
        assert!(matches!(error.kind, ViewErrorKind::NotAJournal), "{error}");
    }

    #[test]
    fn the_cache_keeps_at_most_the_allowed_number_of_indexes() {
        let dir = tempfile::tempdir().expect("временная папка");
        let cache = JournalCache::default();
        for index in 0..MAX_INDEXED + 2 {
            let path = dir.path().join(format!("j{index}.jsonl"));
            std::fs::copy(fixture(), &path).expect("копия");
            cache.open(&path).expect("сводка");
        }
        let state = cache.state.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            state.entries.len(),
            MAX_INDEXED,
            "LRU вытеснил лишние индексы"
        );
        assert!(
            !state.entries.contains_key(&dir.path().join("j0.jsonl")),
            "вытеснен самый давний"
        );
    }

    #[test]
    fn one_index_is_built_once_for_parallel_calls() {
        let cache = JournalCache::default();
        let path = fixture();
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| cache.index(&path));
            let second = scope.spawn(|| cache.index(&path));
            (first.join().unwrap(), second.join().unwrap())
        });
        let first = first.expect("первый индекс");
        let second = second.expect("второй индекс");
        assert!(
            Arc::ptr_eq(&first, &second),
            "второй вызов взял тот же индекс"
        );
    }

    #[test]
    fn density_covers_the_whole_session() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(
            &dir,
            &[
                (0, Dir::AnalyzerToLis, 10),
                (50_000_000, Dir::AnalyzerToLis, 30),
                (100_000_000, Dir::LisToAnalyzer, 20),
            ],
        );
        let summary = JournalCache::default().open(&path).expect("сводка");

        assert_eq!(summary.events, 3);
        assert_eq!(
            summary.duration_us, 100_000_000,
            "итогов нет — по последнему"
        );
        assert_eq!(
            summary.stats,
            Stats {
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

    #[test]
    fn stats_of_session_end_win_over_counted_ones() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(&dir, &[(0, Dir::AnalyzerToLis, 10)]);
        let stats = Stats {
            connections: 3,
            bytes_analyzer_to_lis: 999,
            bytes_lis_to_analyzer: 5,
        };
        append(
            &path,
            line(&Event::SessionEnd(SessionEnd {
                t: "2026-10-19T10:15:04.000000+03:00".to_owned(),
                mono_us: 2_000,
                stats: stats.clone(),
            }))
            .as_bytes(),
        );
        append(
            &path,
            line(&data(3_000, 2, Dir::AnalyzerToLis, 50)).as_bytes(),
        );

        let summary = JournalCache::default().open(&path).expect("сводка");
        assert_eq!(summary.stats, stats, "итоги из session_end");
        assert_eq!(summary.events, 2, "session_end не событие ленты");
        assert_eq!(summary.duration_us, 3_000);
    }

    #[test]
    fn the_bucket_table_maps_a_time_fraction_to_the_first_event_of_it() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("batches.jsonl");
        // Две пачки: первая секунда сессии и последняя; между ними событий нет.
        write_journal(
            &path,
            "ANALYZER",
            &[
                (0, Dir::AnalyzerToLis, 10),
                (500_000, Dir::AnalyzerToLis, 10),
                (99_000_000, Dir::LisToAnalyzer, 20),
                (99_500_000, Dir::LisToAnalyzer, 20),
            ],
        );
        let summary = JournalCache::default().open(&path).expect("сводка");

        assert_eq!(summary.events, 4);
        assert_eq!(
            summary.first_event.len(),
            FIRST_EVENT_POINTS,
            "601 точка: корзины и правый край сессии"
        );
        assert_eq!(summary.first_event[0], 0, "начало — первая пачка");
        assert_eq!(
            summary.first_event[FIRST_EVENT_POINTS - 1],
            4,
            "за правым краем событий нет"
        );
        // Щелчок по трассе на 0.99 времени: первое событие корзины — вторая пачка.
        let point = DENSITY_BUCKETS * 99 / 100;
        assert_eq!(
            summary.first_event[point], 2,
            "доля 0.99 попадает во вторую пачку, а не в конец первой"
        );
    }

    #[test]
    fn a_file_replaced_on_the_same_path_is_indexed_from_the_start() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("swapped.jsonl");
        let cache = JournalCache::default();
        write_journal(&path, "ANALYZER A", &[(0, Dir::AnalyzerToLis, 10)]);
        assert_eq!(cache.open(&path).expect("первый журнал").events, 1);
        let first = cache.index(&path).expect("индекс первого журнала");

        // На том же пути другой, более длинный журнал: прежние смещения ему не подходят.
        write_journal(
            &path,
            "ANALYZER B",
            &[
                (0, Dir::AnalyzerToLis, 20),
                (10, Dir::AnalyzerToLis, 20),
                (20, Dir::LisToAnalyzer, 20),
            ],
        );
        let swapped = cache.open(&path).expect("второй журнал");

        assert_eq!(
            swapped.header.analyzer.as_deref(),
            Some("ANALYZER B"),
            "заголовок взят из нового файла, а не из прежнего индекса"
        );
        assert_eq!(swapped.events, 3, "индекс собран с начала файла");
        assert_eq!(
            swapped.stats,
            Stats {
                connections: 0,
                bytes_analyzer_to_lis: 40,
                bytes_lis_to_analyzer: 20,
            },
            "счётчики нового файла, без остатка прежнего"
        );
        let second = cache.index(&path).expect("индекс второго журнала");
        assert_ne!(
            second.header_hash, first.header_hash,
            "хеш заголовка отличает подменённый файл"
        );
    }

    /// Начало файла то же, но на прежнем конце уже не перевод строки: это другой файл,
    /// и доиндексация с прежнего смещения склеила бы чужие строки.
    #[test]
    fn a_file_with_the_same_start_but_no_line_break_at_the_old_end_is_rebuilt() {
        let dir = tempfile::tempdir().expect("временная папка");
        let samples: Vec<_> = (0..40)
            .map(|index| (index * 1_000, Dir::AnalyzerToLis, 64))
            .collect();
        let path = synthetic(&dir, &samples);
        let cache = JournalCache::default();
        assert_eq!(cache.open(&path).expect("сводка").events, 40);

        let mut bytes = std::fs::read(&path).expect("журнал");
        let old_len = bytes.len();
        bytes[old_len - 1] = b'x';
        bytes.push(b'\n');
        bytes.extend_from_slice(line(&data(90_000, 41, Dir::LisToAnalyzer, 5)).as_bytes());
        std::fs::write(&path, &bytes).expect("подмена");

        let summary = cache.open(&path).expect("сводка после подмены");
        assert_eq!(
            summary.events, 40,
            "последняя прежняя строка испорчена и пропущена, новая прочитана"
        );
        assert_eq!(summary.stats.bytes_analyzer_to_lis, 39 * 64);
        assert_eq!(summary.stats.bytes_lis_to_analyzer, 5);
    }

    #[test]
    fn a_shortened_file_is_indexed_from_the_start() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = synthetic(
            &dir,
            &[
                (0, Dir::AnalyzerToLis, 10),
                (1_000, Dir::AnalyzerToLis, 10),
                (2_000, Dir::AnalyzerToLis, 10),
            ],
        );
        let cache = JournalCache::default();
        assert_eq!(cache.open(&path).expect("сводка").events, 3);

        write_journal(&path, "ANALYZER", &[(0, Dir::AnalyzerToLis, 10)]);
        let summary = cache.open(&path).expect("сводка короткого файла");
        assert_eq!(summary.events, 1);
        assert_eq!(cache.read(&path, 0, 10).expect("страница").len(), 1);
    }

    #[test]
    fn a_note_appended_after_the_session_rebuilds_the_index() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = fixture_copy(&dir);
        let cache = JournalCache::default();

        assert_eq!(cache.open(&path).expect("сводка").events, 5);
        append_note(&path, "дописано после остановки").expect("комментарий");

        let summary = cache.open(&path).expect("сводка после заметки");
        assert_eq!(
            summary.events, 6,
            "индекс пересобран по изменившемуся файлу"
        );
        let last = cache.read(&path, 5, 1).expect("последняя");
        assert_eq!(last.len(), 1);
        match &last[0] {
            Event::Note(note) => assert_eq!(note.text, "дописано после остановки"),
            other => panic!("ожидался note, получено {other:?}"),
        }
    }

    #[test]
    fn a_file_that_is_not_a_journal_is_refused() {
        let dir = tempfile::tempdir().expect("временная папка");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "привет\n").expect("файл");
        let error = JournalCache::default().open(&path).expect_err("не журнал");
        assert!(matches!(error.kind, ViewErrorKind::NotAJournal));
        assert_eq!(error.path, path);

        let missing = dir.path().join("missing.jsonl");
        let error = JournalCache::default()
            .open(&missing)
            .expect_err("нет файла");
        assert!(
            matches!(&error.kind, ViewErrorKind::Io(err) if err.kind() == io::ErrorKind::NotFound)
        );
        assert!(error.to_string().contains("missing.jsonl"), "{error}");
    }
}
