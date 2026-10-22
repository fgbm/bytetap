//! `bytetap-cli verify` и `bytetap-cli note` на синтетических журналах (M1-AC1, M1-AC3, M1-AC16).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bytetap_testkit::fixtures_dir;
use serde_json::{Value, json};

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

fn fixture_lines() -> Vec<Value> {
    std::fs::read_to_string(fixture())
        .expect("фикстура")
        .lines()
        .map(|line| serde_json::from_str(line).expect("строка фикстуры — JSON"))
        .collect()
}

/// Записывает строки журнала в новый файл и возвращает путь к нему.
fn write_lines(dir: &Path, lines: &[Value]) -> PathBuf {
    let path = dir.join("case.jsonl");
    let text: String = lines
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<Vec<_>>()
        .join("");
    std::fs::write(&path, text).expect("журнал записан");
    path
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
    assert!(
        text.contains("Формат: bytetap-session/1"),
        "версия формата печатается: {text}"
    );
}

/// Журнал прокси в формате `/2`: пара в `open` и проверка схемой новой версии (M4-AC9).
#[test]
fn verify_checks_a_proxy_journal_of_the_second_format() {
    use bytetap_core::journal::{
        Autoresponder, Close, ClosedBy, Data, Dir, Event, Host, Link, LinkRole, Mode, Open,
        Session, SessionEnd, Stats, now_rfc3339,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proxy.jsonl");
    let message = b"\x0bMSH|^~\\&|ANALYZER\r\x1c\r";
    let events = [
        Event::Session(Session::new(
            Mode::Proxy,
            Autoresponder::None,
            json!({"listen": "127.0.0.1:50025", "target": "127.0.0.1:5000", "role": "analyzer_client"}),
            Host::current(),
        )),
        Event::Open(Open {
            t: now_rfc3339(),
            mono_us: 1000,
            conn: 1,
            peer: "127.0.0.1:49152".to_owned(),
            local: "127.0.0.1:50025".to_owned(),
            link: Some(Link {
                role: LinkRole::Lis,
                peer: "127.0.0.1:5000".to_owned(),
                local: "127.0.0.1:50026".to_owned(),
            }),
        }),
        Event::Data(Data::new(
            now_rfc3339(),
            2000,
            1,
            Dir::AnalyzerToLis,
            false,
            message,
        )),
        Event::Data(Data::new(
            now_rfc3339(),
            3000,
            1,
            Dir::LisToAnalyzer,
            false,
            b"\x06",
        )),
        Event::Close(Close {
            t: now_rfc3339(),
            mono_us: 4000,
            conn: 1,
            by: ClosedBy::Analyzer,
            reason: "eof".to_owned(),
        }),
        Event::SessionEnd(SessionEnd {
            t: now_rfc3339(),
            mono_us: 5000,
            stats: Stats {
                connections: 1,
                bytes_analyzer_to_lis: message.len() as u64,
                bytes_lis_to_analyzer: 1,
            },
        }),
    ];
    let text: String = events
        .iter()
        .map(|event| format!("{}\n", serde_json::to_string(event).expect("сериализация")))
        .collect();
    std::fs::write(&path, text).unwrap();

    let output = verify(&path);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Формат: bytetap-session/2"), "{text}");
    assert!(text.contains("Соединения: 1"), "{text}");
    assert!(text.contains("Проверка пройдена"), "{text}");
}

/// Смена ошибки источника без единого повтора: в журнале два разных `error` и нет `info`
/// с `source_retry_repeats`, потому что поле `repeats` схемы `/2` начинается с единицы.
/// `verify` такой журнал принимает, а замечание с нулём отверг бы (M4-AC6).
#[test]
fn verify_accepts_an_error_change_without_repeats() {
    use bytetap_core::journal::{
        Autoresponder, Event, Host, Mode, Session, SessionEnd, Stats, now_rfc3339,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proxy-change.jsonl");
    let error = |code: &str, message: &str, mono_us: u64| {
        json!({
            "type": "error",
            "t": now_rfc3339(),
            "mono_us": mono_us,
            "code": code,
            "message": message,
        })
    };
    // Заголовок и итоги — из ядра, ошибки — строками: так видно ровно то, что проверяет
    // схема, а `repeats` в них нет вовсе.
    let header = Event::Session(Session::new(
        Mode::Proxy,
        Autoresponder::None,
        json!({"listen": "127.0.0.1:50025", "target": "127.0.0.1:5000", "role": "analyzer_client"}),
        Host::current(),
    ));
    let end = Event::SessionEnd(SessionEnd {
        t: now_rfc3339(),
        mono_us: 3000,
        stats: Stats::default(),
    });
    let mut text = format!("{}\n", serde_json::to_string(&header).unwrap());
    for line in [
        error(
            "proxy_connect_failed",
            "connect failed: connection refused",
            1000,
        ),
        error("accept_failed", "accept failed: too many files", 2000),
    ] {
        text.push_str(&format!("{}\n", serde_json::to_string(&line).unwrap()));
    }
    text.push_str(&format!("{}\n", serde_json::to_string(&end).unwrap()));
    std::fs::write(&path, text).unwrap();

    let output = verify(&path);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Проверка пройдена"),
        "{}",
        stdout(&output)
    );

    // Замечание о нуле повторов схема не принимает: поэтому сессия его и не пишет.
    let broken = dir.path().join("repeats-zero.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.insert_str(
        text.find("\n{\"code\":").expect("строка ошибки") + 1,
        "{\"type\":\"info\",\"t\":\"2026-10-22T10:15:01.000000+03:00\",\"mono_us\":1500,\"code\":\"source_retry_repeats\",\"repeats\":0,\"message\":\"changed\"}\n",
    );
    std::fs::write(&broken, text).unwrap();
    let output = verify(&broken);
    assert!(!output.status.success(), "repeats: 0 схема отвергает");
    assert!(stderr(&output).contains("repeats"), "{}", stderr(&output));
}

#[test]
fn verify_rejects_a_journal_of_an_unknown_format() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    lines[0]["format"] = json!("bytetap-session/9");
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("неизвестная версия формата"),
        "{}",
        stderr(&output)
    );
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
fn verify_checks_that_len_matches_the_decoded_b64() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    let actual = lines[2]["len"].as_u64().unwrap();
    lines[2]["len"] = json!(actual + 1);
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("len"), "{}", stderr(&output));
}

#[test]
fn verify_checks_that_text_is_the_escaped_b64() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    lines[2]["text"] = json!("<VT>MSH");
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("text"), "{}", stderr(&output));
}

#[test]
fn verify_rejects_an_undecodable_b64() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    lines[2]["b64"] = json!("не base64");
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("b64"), "{}", stderr(&output));
}

#[test]
fn verify_rejects_two_session_headers() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    let header = lines[0].clone();
    lines.push(header);
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("ровно один"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn verify_rejects_events_after_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    let data = lines[2].clone();
    lines.push(data);
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("после session_end"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn verify_accepts_a_note_after_session_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = fixture_lines();
    lines.push(
        json!({"type":"note","t":"2026-10-19T10:30:00.000000+03:00","text":"прогнали три пробы"}),
    );
    let path = write_lines(dir.path(), &lines);

    let output = verify(&path);
    assert!(output.status.success(), "{}", stderr(&output));
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
