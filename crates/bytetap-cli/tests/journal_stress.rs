//! T-4: журнал остаётся пригодным после аварийного завершения процесса
//! (`Child::kill()` — SIGKILL на Unix, TerminateProcess на Windows).
//!
//! Тест гоняет настоящий бинарь под нагрузкой, ждёт накопления строк, убивает процесс
//! и проверяет, что каждая строка, кроме, возможно, последней, — валидное событие формата.

use std::io::BufRead;
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

fn count_lines(path: &Path) -> usize {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .count()
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

#[test]
fn journal_survives_a_killed_process() {
    let dir = tempfile::tempdir().expect("временная папка");
    let path = dir.path().join("stress.jsonl");

    let child = Command::new(env!("CARGO_BIN_EXE_bytetap-cli"))
        .arg("_stress-journal")
        .arg("--out")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("запуск bytetap-cli _stress-journal");
    let mut child = KillOnDrop(Some(child));

    let deadline = Instant::now() + TIMEOUT;
    while count_lines(&path) < MIN_LINES {
        assert!(
            Instant::now() < deadline,
            "журнал не наполнился за {TIMEOUT:?}: {} строк",
            count_lines(&path)
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut child = child.0.take().expect("процесс ещё жив");
    child.kill().expect("SIGKILL / TerminateProcess");
    let status = child.wait().expect("процесс завершился");
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
