//! Последние журналы папки для стартового экрана (M3, U1-AC9).
//!
//! Журнал бывает в сотни мегабайт, а списку нужны только две строки: заголовок сверху и
//! итоги `session_end` в хвосте. Целиком файл не читается даже на список из десятков
//! журналов.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Read as _, Seek as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};

/// Сколько журналов отдаёт список папки не больше: стартовый экран показывает не всё.
pub const MAX_RECENT_JOURNALS: usize = 20;

/// Сколько байт файла читается ради заголовка и ради `session_end`.
pub const RECENT_CHUNK_BYTES: u64 = 64 * 1024;

/// Журнал в списке последних записей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentJournal {
    /// Путь к файлу.
    pub path: PathBuf,
    /// Начало сессии из заголовка, RFC 3339.
    pub started_at: String,
    /// Модель анализатора; пусто — не указана.
    pub analyzer: String,
    /// Длительность по `session_end`, микросекунды; `0` — журнал не закрыт.
    pub duration_us: u64,
    /// Режим из заголовка: `tcp_server`, `tcp_client` или `serial`.
    pub mode: String,
}

/// Журналы в папке: начало, анализатор, режим и длительность.
///
/// Сканируется только верхний уровень каталога. Битые файлы и файлы без заголовка
/// `session` пропускаются молча: в папку журналов могут попасть чужие `.jsonl`.
/// Сортировка — по началу сессии, новые сверху; `limit` ограничен [`MAX_RECENT_JOURNALS`].
///
/// # Errors
///
/// Папка не читается.
pub fn recent(dir: &Path, limit: usize) -> io::Result<Vec<RecentJournal>> {
    let limit = limit.min(MAX_RECENT_JOURNALS);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut journals: Vec<(DateTime<FixedOffset>, RecentJournal)> = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !is_journal_file(&path) {
            continue;
        }
        if let Some(found) = recent_one(&path) {
            journals.push(found);
        }
    }
    // Новые сверху; при равном начале порядок задаёт путь, чтобы список не «дрожал».
    journals.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
    });
    journals.truncate(limit);
    Ok(journals.into_iter().map(|(_, journal)| journal).collect())
}

/// Файл журнала по имени: расширение `.jsonl` без учёта регистра и именно файл.
fn is_journal_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Одна строка списка: заголовок сверху и `session_end` из хвоста, если он есть.
fn recent_one(path: &Path) -> Option<(DateTime<FixedOffset>, RecentJournal)> {
    let header = read_header_line(path)?;
    if header.get("type")?.as_str()? != "session" {
        return None;
    }
    let started_at = header.get("started_at")?.as_str()?.to_owned();
    let started = DateTime::parse_from_rfc3339(&started_at).ok()?;
    let mode = header.get("mode")?.as_str()?.to_owned();
    let analyzer = header
        .get("analyzer")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    Some((
        started,
        RecentJournal {
            path: path.to_path_buf(),
            started_at,
            analyzer,
            duration_us: read_session_end(path).unwrap_or(0),
            mode,
        },
    ))
}

/// Первая строка файла как JSON: заголовок сессии (FR-21).
fn read_header_line(path: &Path) -> Option<serde_json::Value> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(RECENT_CHUNK_BYTES));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    serde_json::from_slice(&line[..end]).ok()
}

/// Длительность из `session_end` по монотонным часам, микросекунды.
///
/// Читается только хвост файла; комментарий инженера может стоять после `session_end`,
/// поэтому строки просматриваются с конца. Журнал без итогов (оборван) даёт `None`.
fn read_session_end(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(RECENT_CHUNK_BYTES);
    file.seek(io::SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // Срез с середины файла: первая строка обрезана и JSON из неё не выйдет.
        lines.remove(0);
    }
    lines.iter().rev().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        if value.get("type")?.as_str()? != "session_end" {
            return None;
        }
        value.get("mono_us")?.as_u64()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::journal::{
        Autoresponder, Data, Dir, Event, Host, Mode, Note, Session, SessionEnd, Stats,
    };

    /// Журнал для списка папки: заголовок с началом, режимом и анализатором, `session_end`.
    fn recent_journal(
        dir: &tempfile::TempDir,
        name: &str,
        mode: Mode,
        started_at: &str,
        analyzer: Option<&str>,
        end: Option<u64>,
    ) -> PathBuf {
        let mut header = Session::new(
            mode,
            Autoresponder::Auto,
            json!({"port": "COM5"}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        header.started_at = started_at.to_owned();
        header.analyzer = analyzer.map(str::to_owned);
        let mut text = format!(
            "{}\n",
            serde_json::to_string(&Event::Session(header)).unwrap()
        );
        let data = Event::Data(Data::new(
            "2026-10-19T09:00:01.000000+03:00",
            1_000,
            1,
            Dir::AnalyzerToLis,
            false,
            b"\x05",
        ));
        text.push_str(&serde_json::to_string(&data).unwrap());
        text.push('\n');
        if let Some(duration) = end {
            let end = Event::SessionEnd(SessionEnd {
                t: "2026-10-19T09:00:02.000000+03:00".to_owned(),
                mono_us: duration,
                stats: Stats::default(),
            });
            text.push_str(&serde_json::to_string(&end).unwrap());
            text.push('\n');
        }
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("журнал");
        path
    }

    /// Список папки: заголовок и хвост, новые сверху, битые файлы пропускаются.
    #[test]
    fn the_recent_list_reads_the_header_and_the_end_of_each_journal() {
        let dir = tempfile::tempdir().expect("временная папка");
        let older = recent_journal(
            &dir,
            "bytetap-20261019-090000-serial.jsonl",
            Mode::Serial,
            "2026-10-19T09:00:00.123456+03:00",
            Some("A"),
            Some(1_500_000),
        );
        let newer = recent_journal(
            &dir,
            "bytetap-20261019-100000-tcp_client.JSONL",
            Mode::TcpClient,
            "2026-10-19T10:00:00.000000+03:00",
            None,
            None,
        );
        std::fs::write(dir.path().join("broken.jsonl"), "not a journal\n").expect("битый");
        std::fs::write(dir.path().join("notes.txt"), "{}\n").expect("не журнал");
        std::fs::create_dir(dir.path().join("folder.jsonl")).expect("папка");

        let list = recent(dir.path(), 10).expect("список папки");
        assert_eq!(list.len(), 2, "битый файл и папка пропущены: {list:?}");
        assert_eq!(list[0].path, newer);
        assert_eq!(list[0].mode, "tcp_client");
        assert_eq!(list[0].analyzer, "");
        assert_eq!(
            list[0].duration_us, 0,
            "журнал без session_end не знает длительности"
        );
        assert_eq!(list[1].path, older);
        assert_eq!(list[1].started_at, "2026-10-19T09:00:00.123456+03:00");
        assert_eq!(list[1].mode, "serial");
        assert_eq!(list[1].analyzer, "A");
        assert_eq!(list[1].duration_us, 1_500_000);

        assert_eq!(recent(dir.path(), 1).expect("предел").len(), 1);
        assert!(recent(dir.path(), 0).expect("ничего не просили").is_empty());
        assert!(
            recent(&dir.path().join("missing"), 10).is_err(),
            "папки нет"
        );
    }

    /// Предел списка — не больше [`MAX_RECENT_JOURNALS`], сколько бы ни просили.
    #[test]
    fn the_list_is_capped() {
        let dir = tempfile::tempdir().expect("временная папка");
        for index in 0..MAX_RECENT_JOURNALS + 2 {
            recent_journal(
                &dir,
                &format!("j{index:02}.jsonl"),
                Mode::TcpServer,
                &format!("2026-10-19T09:{index:02}:00.000000+03:00"),
                None,
                Some(1),
            );
        }
        let list = recent(dir.path(), 1000).expect("список папки");
        assert_eq!(list.len(), MAX_RECENT_JOURNALS);
        assert!(list[0].path.ends_with("j21.jsonl"), "новые сверху");
    }

    /// В большом журнале читается только хвост: итоги находятся, журнал целиком не читается.
    #[test]
    fn the_end_is_found_in_the_tail_of_a_long_journal() {
        let dir = tempfile::tempdir().expect("временная папка");
        let mut header = Session::new(
            Mode::Serial,
            Autoresponder::Auto,
            json!({"port": "COM5"}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        header.started_at = "2026-10-19T09:00:00.000000+03:00".to_owned();
        let mut text = format!(
            "{}\n",
            serde_json::to_string(&Event::Session(header)).unwrap()
        );
        // Двести килобайт в одной строке: хвост начинается с её середины.
        let big = Event::Data(Data::new(
            "2026-10-19T09:00:01.500000+03:00",
            1_500_000,
            1,
            Dir::AnalyzerToLis,
            false,
            &vec![b'A'; 200 * 1024],
        ));
        text.push_str(&serde_json::to_string(&big).unwrap());
        text.push('\n');
        let end = Event::SessionEnd(SessionEnd {
            t: "2026-10-19T09:00:02.000000+03:00".to_owned(),
            mono_us: 2_000_000,
            stats: Stats::default(),
        });
        text.push_str(&serde_json::to_string(&end).unwrap());
        text.push('\n');
        // Комментарий инженера дописан после остановки: итоги всё равно находятся.
        let note = Event::Note(Note {
            t: "2026-10-19T09:05:00.000000+03:00".to_owned(),
            mono_us: None,
            text: "проба переснята".to_owned(),
        });
        text.push_str(&serde_json::to_string(&note).unwrap());
        text.push('\n');
        let path = dir.path().join("long.jsonl");
        std::fs::write(&path, text).expect("журнал");

        let list = recent(dir.path(), 10).expect("список папки");
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].duration_us, 2_000_000,
            "итоги взяты из хвоста длинного журнала"
        );
        assert_eq!(list[0].mode, "serial");
    }
}
