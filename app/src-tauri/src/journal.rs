//! Просмотр журнала: сводка, плотность и постраничное чтение (FR-29, M2-AC9).
//!
//! Журнал бывает в сотни мегабайт, поэтому в память интерфейса он не загружается. Первый
//! проход по файлу строит индекс: смещения строк событий ленты и гистограмму плотности
//! на [`DENSITY_BUCKETS`] корзин по `mono_us`. Индекс кешируется по пути и перестраивается,
//! только если файл изменился (дописан комментарий). Постраничное чтение идёт по индексу:
//! `seek` на смещение и чтение нужного числа строк. Строится всё в `spawn_blocking`, чтобы
//! не задерживать окно.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use bytetap_core::journal::{Dir, Event, Note, Stats, now_rfc3339};
use serde::Deserialize;

use crate::api::{Density, JournalSummary, SessionStats};
use crate::error::{CommandError, ErrorCode};
use crate::feed::FeedEvent;

/// Сколько корзин в гистограмме плотности: столько точек рисует трасса всей сессии.
pub const DENSITY_BUCKETS: usize = 600;

/// Индекс журнала: всё, что нужно для сводки и постраничного чтения.
#[derive(Debug)]
pub struct JournalIndex {
    header: Option<serde_json::Value>,
    /// Смещения в байтах у строк, которые становятся событиями ленты.
    offsets: Vec<u64>,
    duration_us: u64,
    stats: SessionStats,
    density: Density,
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

impl JournalIndex {
    /// Проход по файлу: заголовок, итоги, смещения строк и плотность.
    ///
    /// Битая строка не ломает просмотр: она пропускается, как и в `bytetap-cli verify`
    /// (ошибки строк там — отдельный отчёт). Файл без заголовка `session` — не журнал.
    fn build(path: &Path) -> Result<Self, CommandError> {
        let file = std::fs::File::open(path).map_err(|err| journal_error(&err, path))?;
        let metadata = file.metadata().map_err(|err| journal_error(&err, path))?;
        let mut reader = BufReader::new(file);

        let mut header = None;
        let mut offsets = Vec::new();
        let mut samples: Vec<Sample> = Vec::new();
        let mut duration_us = 0u64;
        let mut stats = Stats::default();
        let mut stats_from_end = false;
        let mut offset = 0u64;
        let mut buffer = Vec::new();

        loop {
            buffer.clear();
            let read = reader
                .read_until(b'\n', &mut buffer)
                .map_err(|err| journal_error(&err, path))?;
            if read == 0 {
                break;
            }
            let start = offset;
            offset += read as u64;

            let Ok(value) = serde_json::from_slice::<serde_json::Value>(trim_line(&buffer)) else {
                continue;
            };
            let Ok(event) = Event::deserialize(&value) else {
                continue;
            };
            match event {
                Event::Session(_) => {
                    if header.is_none() {
                        header = Some(value);
                    }
                }
                Event::SessionEnd(end) => {
                    duration_us = duration_us.max(end.mono_us);
                    stats = end.stats;
                    stats_from_end = true;
                }
                Event::Open(open) => {
                    offsets.push(start);
                    duration_us = duration_us.max(open.mono_us);
                    if !stats_from_end {
                        stats.connections += 1;
                    }
                }
                Event::Data(data) => {
                    offsets.push(start);
                    duration_us = duration_us.max(data.mono_us);
                    samples.push(Sample {
                        mono_us: data.mono_us,
                        dir: data.dir,
                        len: data.len,
                    });
                    if !stats_from_end {
                        stats.add(data.dir, data.len);
                    }
                }
                Event::Close(close) => {
                    offsets.push(start);
                    duration_us = duration_us.max(close.mono_us);
                }
                Event::Error(error) => {
                    offsets.push(start);
                    duration_us = duration_us.max(error.mono_us);
                }
                Event::Info(info) => {
                    offsets.push(start);
                    duration_us = duration_us.max(info.mono_us);
                }
                Event::Note(note) => {
                    offsets.push(start);
                    if let Some(mono_us) = note.mono_us {
                        duration_us = duration_us.max(mono_us);
                    }
                }
            }
        }

        let header = header.ok_or_else(|| {
            CommandError::new(
                ErrorCode::Journal,
                "это не журнал bytetap: в файле нет заголовка `session`",
            )
            .with_param("path", path.to_string_lossy())
        })?;
        Ok(Self {
            header: Some(header),
            offsets,
            duration_us,
            stats: stats.into(),
            density: density(&samples, duration_us),
            len: metadata.len(),
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
            stats: self.stats,
            density: self.density.clone(),
        }
    }

    /// Индекс собран по этой версии файла.
    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        self.len == metadata.len() && self.modified == metadata.modified().ok()
    }
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

/// Кеш индексов журналов по пути.
#[derive(Debug, Default)]
pub struct JournalCache {
    entries: Mutex<HashMap<PathBuf, Arc<JournalIndex>>>,
}

impl JournalCache {
    /// Индекс журнала: из кеша, а если файл изменился или ещё не открывался — заново.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub async fn index(&self, path: &Path) -> Result<Arc<JournalIndex>, CommandError> {
        let metadata = std::fs::metadata(path).map_err(|err| journal_error(&err, path))?;
        let cached = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .filter(|index| index.matches(&metadata))
            .cloned();
        if let Some(index) = cached {
            return Ok(index);
        }

        let building = path.to_path_buf();
        let index = tokio::task::spawn_blocking(move || JournalIndex::build(&building))
            .await
            .map_err(|err| {
                CommandError::new(
                    ErrorCode::Journal,
                    format!("индексация журнала упала: {err}"),
                )
            })??;
        let index = Arc::new(index);
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_path_buf(), Arc::clone(&index));
        Ok(index)
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

/// Постраничное чтение событий ленты: `from` — номер события, `count` — сколько отдать.
///
/// Нумерация — по событиям ленты: заголовок `session` и итоги `session_end` в неё не входят,
/// они приходят в сводке. Строки, которых нет (файл стал короче), просто не читаются.
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
            CommandError::new(ErrorCode::Journal, format!("чтение журнала упало: {err}"))
        })?
}

/// Читает строки по смещениям и превращает их в события ленты.
fn read_at(path: &Path, offsets: &[u64]) -> Result<Vec<FeedEvent>, CommandError> {
    let file = std::fs::File::open(path).map_err(|err| journal_error(&err, path))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::with_capacity(offsets.len());
    let mut buffer = Vec::new();
    for offset in offsets {
        reader
            .seek(io::SeekFrom::Start(*offset))
            .map_err(|err| journal_error(&err, path))?;
        buffer.clear();
        reader
            .read_until(b'\n', &mut buffer)
            .map_err(|err| journal_error(&err, path))?;
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

/// Ошибка журнала с путём в подстановках.
fn journal_error(err: &io::Error, path: &Path) -> CommandError {
    CommandError::from_io_with(ErrorCode::Journal, err).with_param("path", path.to_string_lossy())
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
        assert_eq!(error.code, ErrorCode::Journal);

        let missing = dir.path().join("missing.jsonl");
        assert_eq!(
            open(&missing, &JournalCache::default())
                .await
                .expect_err("нет файла")
                .code,
            ErrorCode::Journal
        );
    }

    #[test]
    fn journal_error_names_the_path() {
        let error = journal_error(
            &io::Error::from(io::ErrorKind::NotFound),
            Path::new("/tmp/x"),
        );
        assert_eq!(
            error.params.as_ref().and_then(|params| params.get("path")),
            Some(&"/tmp/x".to_owned())
        );
    }
}
