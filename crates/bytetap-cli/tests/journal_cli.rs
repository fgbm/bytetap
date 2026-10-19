//! `bytetap-cli verify` и `bytetap-cli note` на синтетических журналах (M1-AC1, M1-AC3, M1-AC16).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bytetap_testkit::fixtures_dir;

fn verify(file: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bytetap-cli"))
        .arg("verify")
        .arg(file)
        .output()
        .expect("запуск bytetap-cli verify")
}

fn note(file: &Path, text: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bytetap-cli"))
        .arg("note")
        .arg(file)
        .arg(text)
        .output()
        .expect("запуск bytetap-cli note")
}

fn fixture() -> PathBuf {
    fixtures_dir().join("journal/minimal.jsonl")
}

fn copy_fixture(dir: &Path) -> PathBuf {
    let copy = dir.join("minimal.jsonl");
    std::fs::copy(fixture(), &copy).expect("копия фикстуры");
    copy
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn verify_accepts_the_synthetic_fixture() {
    let output = verify(&fixture());
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Строк: 7"), "{text}");
    assert!(text.contains("session 1"), "{text}");
    assert!(text.contains("data 3"), "{text}");
    assert!(text.contains("analyzer_to_lis 136"), "{text}");
    assert!(text.contains("lis_to_analyzer 76"), "{text}");
    assert!(text.contains("Соединения: 1"), "{text}");
    assert!(text.contains("Проверка пройдена"), "{text}");
}

#[test]
fn verify_rejects_a_broken_line_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path());
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.insert_str(
        0,
        "{\"type\":\"session\",\"format\":\"bytetap-session/1\"}\n",
    );
    std::fs::write(&path, text).unwrap();

    let output = verify(&path);
    assert!(!output.status.success(), "битая строка ломает проверку");
    assert!(stderr(&output).contains("строка 1"), "{}", stderr(&output));
}

#[test]
fn verify_warns_about_a_truncated_last_line_but_accepts_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path());
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{\"type\":\"note\",\"t\":\"2026-10-1");
    std::fs::write(&path, text).unwrap();

    let output = verify(&path);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("обрезана"), "{}", stderr(&output));
}

#[test]
fn verify_fails_when_the_first_line_is_not_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-header.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"note\",\"t\":\"2026-10-19T10:20:00.000000+03:00\",\"text\":\"x\"}\n",
    )
    .unwrap();

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("session"), "{}", stderr(&output));
}

#[test]
fn verify_reports_a_missing_file() {
    let output = verify(Path::new("нет-такого-файла.jsonl"));
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("не удалось открыть"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn note_appends_to_the_end_and_keeps_verify_green() {
    let dir = tempfile::tempdir().unwrap();
    let path = copy_fixture(dir.path());
    let before = std::fs::read(&path).unwrap();

    let output = note(&path, "прогнали три пробы");
    assert!(output.status.success(), "{}", stderr(&output));

    let after = std::fs::read(&path).unwrap();
    assert!(
        after.starts_with(&before),
        "начало файла не переписано (FR-23)"
    );
    let appended = std::str::from_utf8(&after[before.len()..]).unwrap();
    assert!(appended.contains("\"type\":\"note\""), "{appended}");
    assert!(appended.contains("прогнали три пробы"), "{appended}");

    let output = verify(&path);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("note 1"), "{}", stdout(&output));
}

#[test]
fn note_refuses_a_missing_file() {
    let output = note(Path::new("нет-такого-файла.jsonl"), "текст");
    assert!(!output.status.success());
}
