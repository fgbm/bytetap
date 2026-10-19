//! T-4: журнал остаётся пригодным после аварийного завершения процесса
//! (`Child::kill()` — SIGKILL на Unix, TerminateProcess на Windows).
//!
//! Тест гоняет настоящий бинарь под нагрузкой, ждёт накопления строк, убивает процесс
//! и проверяет, что каждая строка, кроме, возможно, последней, — валидное событие формата.
//!
//! Журнал читается, пока процесс ещё пишет, и на Windows: писатель открывает файл без
//! `FILE_SHARE_WRITE`, но с `FILE_SHARE_READ` (ADR 0001, п. 7), поэтому чтение из другого
//! процесса проходит, а запись — нет.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytetap_core::journal::{LineErrorKind, read_all};
use serde_json::Value;

/// Сколько строк должно оказаться в файле до убийства процесса.
const MIN_LINES: usize = 2_000;
/// Сколько ждём наполнения журнала — с запасом на медленную CI-машину.
const TIMEOUT: Duration = Duration::from_secs(60);

fn schema() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/public/schema/bytetap-session-1.schema.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("схема формата")).expect("JSON")
}

/// Число строк в файле: строки считаются по `\n`, как их пишет и читает журнал.
fn count_lines(path: &Path) -> usize {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    bytes.iter().filter(|byte| **byte == b'\n').count()
}

/// Убивает процесс, даже если тест паникует раньше.
struct KillOnDrop(Option<Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Останавливает тест с ошибкой, если дочерний процесс вышел раньше срока, и печатает его stderr.
fn panic_on_early_exit(child: &mut Child, lines: usize) {
    let Some(status) = child.try_wait().expect("try_wait") else {
        return;
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    panic!("_stress-journal завершился раньше срока ({status}), в журнале {lines} строк: {stderr}");
}

#[test]
fn journal_survives_a_killed_process() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("stress.jsonl");

    let child = Command::new(env!("CARGO_BIN_EXE_bytetap-cli"))
        .arg("_stress-journal")
        .arg("--out")
        .arg(&path)
        .arg("--max-seconds")
        .arg("120")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("запуск bytetap-cli _stress-journal");
    let mut child = KillOnDrop(Some(child));

    let deadline = Instant::now() + TIMEOUT;
    loop {
        let lines = count_lines(&path);
        if lines >= MIN_LINES {
            break;
        }
        let process = child.0.as_mut().expect("процесс ещё не забран");
        panic_on_early_exit(process, lines);
        assert!(
            Instant::now() < deadline,
            "журнал не наполнился за {TIMEOUT:?}: {lines} строк"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut process = child.0.take().expect("процесс ещё жив");
    process.kill().expect("SIGKILL / TerminateProcess");
    let status = process.wait().expect("процесс завершился");
    assert!(!status.success(), "процесс убит, а не завершился штатно");

    let outcome = read_all(&path).expect("чтение журнала");
    assert!(
        outcome.lines.len() >= MIN_LINES,
        "строк меньше ожидаемого: {}",
        outcome.lines.len()
    );
    assert_eq!(
        outcome.lines[0].event.kind(),
        "session",
        "первая строка — заголовок сессии (FR-21)"
    );

    let last_line = outcome.lines.last().map(|line| line.number).unwrap_or(0);
    for problem in &outcome.problems {
        assert_eq!(
            problem.kind,
            LineErrorKind::Truncated,
            "битая строка в середине файла: {problem}"
        );
        assert_eq!(
            problem.line,
            last_line + 1,
            "обрезанной может быть только последняя строка: {problem}"
        );
    }

    let validator = jsonschema::validator_for(&schema()).expect("схема проверяется движком");
    for line in &outcome.lines {
        if let Err(error) = validator.validate(&line.value) {
            panic!("строка {} не прошла схему: {error}", line.number);
        }
    }
}
