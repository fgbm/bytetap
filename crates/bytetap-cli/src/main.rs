//! Консольная версия bytetap.
//!
//! На M1 это работа с готовым журналом: `verify` проверяет журнал по схеме и печатает
//! статистику, `note` дописывает комментарий инженера. `capture` появляется на шаге 3 этапа.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bytetap_core::journal::{
    self, Autoresponder, Data, Dir, Event, Host, Journal, LineErrorKind, Mode, MonoClock, Note,
    Session, now_rfc3339,
};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

/// JSON Schema формата, вшита в бинарь: проверка не зависит от файлов рядом и от сети (FR-24).
const SCHEMA: &str = include_str!("../../../docs/public/schema/bytetap-session-1.schema.json");

/// Запись «сырого» обмена между лабораторным анализатором и ЛИС.
#[derive(Debug, Parser)]
#[command(name = "bytetap-cli", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Проверить журнал сессии по схеме и напечатать статистику
    Verify {
        /// Файл журнала `*.jsonl`
        file: PathBuf,
    },
    /// Дописать комментарий инженера в конец журнала
    Note {
        /// Файл журнала `*.jsonl`
        file: PathBuf,
        /// Текст комментария
        text: String,
    },
    /// Нагрузочная запись журнала для теста устойчивости (T-4)
    #[command(name = "_stress-journal", hide = true)]
    StressJournal {
        /// Файл журнала
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Verify { file } => verify(&file),
        Command::Note { file, text } => note(&file, &text),
        Command::StressJournal { out } => stress_journal(&out),
    }
}

/// События в порядке, в котором их печатает статистика.
const KINDS: [&str; 7] = [
    "session",
    "open",
    "data",
    "close",
    "error",
    "note",
    "session_end",
];

/// `bytetap-cli verify`: проверка каждой строки по схеме и краткая статистика (M1-AC1, M1-AC15).
fn verify(file: &Path) -> ExitCode {
    let schema: Value = match serde_json::from_str(SCHEMA) {
        Ok(schema) => schema,
        Err(err) => {
            eprintln!("ошибка: вшитая схема формата не разобралась: {err}");
            return ExitCode::FAILURE;
        }
    };
    let validator = match jsonschema::validator_for(&schema) {
        Ok(validator) => validator,
        Err(err) => {
            eprintln!("ошибка: вшитая схема формата не принята движком: {err}");
            return ExitCode::FAILURE;
        }
    };

    let reader = match journal::JournalReader::open(file) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("ошибка: не удалось открыть {}: {err}", file.display());
            return ExitCode::FAILURE;
        }
    };

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut bytes: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut connections: BTreeSet<u32> = BTreeSet::new();
    let mut lines = 0usize;
    let mut errors = 0usize;
    let mut first_kind: Option<&'static str> = None;

    for item in reader {
        match item {
            Ok(line) => {
                lines += 1;
                if lines == 1 {
                    first_kind = Some(line.event.kind());
                }
                *counts.entry(line.event.kind()).or_default() += 1;
                match &line.event {
                    Event::Data(data) => {
                        *bytes.entry(data.dir.as_str()).or_default() += data.len;
                        connections.insert(data.conn);
                    }
                    Event::Open(open) => {
                        connections.insert(open.conn);
                    }
                    Event::Close(close) => {
                        connections.insert(close.conn);
                    }
                    Event::Session(_) | Event::Error(_) | Event::Note(_) | Event::SessionEnd(_) => {
                    }
                }
                if let Err(error) = validator.validate(&line.value) {
                    errors += 1;
                    eprintln!("ошибка: строка {}: {error}", line.number);
                }
            }
            Err(error) if error.kind == LineErrorKind::Truncated => {
                eprintln!(
                    "предупреждение: строка {} обрезана — потеря при аварийном завершении (FR-20)",
                    error.line
                );
            }
            Err(error) => {
                errors += 1;
                eprintln!("ошибка: строка {}: {}", error.line, error.message);
            }
        }
    }

    if first_kind != Some("session") {
        errors += 1;
        eprintln!("ошибка: первая строка файла должна быть заголовком `session` (FR-21)");
    }

    println!("Журнал: {}", file.display());
    println!("Строк: {lines}");
    let kinds: Vec<String> = KINDS
        .iter()
        .map(|kind| format!("{kind} {}", counts.get(kind).copied().unwrap_or(0)))
        .collect();
    println!("События: {}", kinds.join(", "));
    println!(
        "Байты: {} {}, {} {}",
        Dir::AnalyzerToLis.as_str(),
        bytes.get(Dir::AnalyzerToLis.as_str()).copied().unwrap_or(0),
        Dir::LisToAnalyzer.as_str(),
        bytes.get(Dir::LisToAnalyzer.as_str()).copied().unwrap_or(0)
    );
    println!("Соединения: {}", connections.len());

    if errors > 0 {
        eprintln!("проверка не пройдена: ошибок {errors}");
        return ExitCode::FAILURE;
    }
    println!("Проверка пройдена.");
    ExitCode::SUCCESS
}

/// `bytetap-cli note`: дописывает событие `note` в конец файла, начало не переписывается (FR-23).
fn note(file: &Path, text: &str) -> ExitCode {
    let note = Note {
        t: now_rfc3339(),
        mono_us: None,
        text: text.to_owned(),
    };
    match journal::append_note(file, &note) {
        Ok(()) => {
            println!("Комментарий дописан: {}", file.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("ошибка: не удалось дописать комментарий: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `_stress-journal`: пишет журнал под нагрузкой, пока процесс не убьют (T-4).
///
/// Команда скрыта из справки: ею пользуется только интеграционный тест устойчивости.
fn stress_journal(out: &Path) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("ошибка: не удалось запустить runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    let result = runtime.block_on(async {
        let session = Session::new(
            Mode::TcpServer,
            Autoresponder::Hl7Mllp,
            json!({"listen": "127.0.0.1:5000", "stress": true}),
            Host::current(),
        );
        let journal = Journal::create(out, session)?;
        let clock = MonoClock::new();
        let mut index: u64 = 0;
        loop {
            index += 1;
            let message = format!(
                "\u{b}MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|{index}|P|2.3.1\rPID|1||PATIENT^TEST\rOBX|1|NM|GLU||5.4|mmol/L\r\u{c}\r"
            );
            let event = Event::Data(Data::new(
                now_rfc3339(),
                clock.now_us(),
                1,
                Dir::AnalyzerToLis,
                false,
                message.as_bytes(),
            ));
            if !journal.record(event) {
                return Err(std::io::Error::other("писатель журнала остановился"));
            }
            if index.is_multiple_of(64) {
                tokio::task::yield_now().await;
            }
        }
    });

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ошибка: нагрузочная запись прервана: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_matches_core() {
        let version = Cli::command().get_version().map(str::to_owned);
        assert_eq!(version.as_deref(), Some(bytetap_core::version()));
    }

    #[test]
    fn embedded_schema_is_valid() {
        let schema: Value = serde_json::from_str(SCHEMA).expect("схема — корректный JSON");
        assert!(jsonschema::validator_for(&schema).is_ok());
    }

    #[test]
    fn stress_command_is_hidden_from_help() {
        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("_stress-journal"), "{help}");
    }
}
