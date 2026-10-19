//! События журнала сессии: типы, поля и их сериализация (FR-21, FR-22).
//!
//! Имена полей и значения перечислений совпадают с форматом `bytetap-session/1`
//! (`docs/reference/session-format.md`) и описаны схемой
//! `docs/public/schema/bytetap-session-1.schema.json`.

use std::fmt;

use base64::Engine as _;
use chrono::{DateTime, Local, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::journal::now_rfc3339;
use crate::text;

/// Версия формата журнала в поле `format` заголовка сессии.
pub const FORMAT: &str = "bytetap-session/1";

/// Строка журнала.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Заголовок сессии, первая строка файла (FR-21).
    Session(Session),
    /// Открытие соединения.
    Open(Open),
    /// Фрагмент обмена (FR-22).
    Data(Data),
    /// Закрытие соединения.
    Close(Close),
    /// Ошибка обработки соединения. Соединение не обязательно закрывается, сессия продолжается.
    Error(ErrorEvent),
    /// Замечание обработчика: обмен не прерван, но кое-что стоит записать.
    Info(InfoEvent),
    /// Комментарий инженера, дописанный в конец файла (FR-23).
    Note(Note),
    /// Итоги сессии, последняя строка при штатной остановке.
    SessionEnd(SessionEnd),
}

impl Event {
    /// Имя события в поле `type`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::Open(_) => "open",
            Self::Data(_) => "data",
            Self::Close(_) => "close",
            Self::Error(_) => "error",
            Self::Info(_) => "info",
            Self::Note(_) => "note",
            Self::SessionEnd(_) => "session_end",
        }
    }
}

/// Режим работы сессии (FR-1, FR-2, FR-5, FR-9, FR-10, FR-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Вместо ЛИС по TCP.
    TcpServer,
    /// К анализатору по TCP.
    TcpClient,
    /// Прокси TCP.
    Proxy,
    /// Вместо ЛИС через COM-порт.
    Serial,
    /// Прослушка через два COM-порта.
    ComMonitor,
    /// COM-порт анализатора ↔ ЛИС по TCP.
    ComTcp,
}

impl Mode {
    /// Значение режима в поле `mode` и в имени файла.
    ///
    /// COM-порт «вместо ЛИС» пишется как `serial`: имя файла `…-serial.jsonl` (M3-AC16).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TcpServer => "tcp_server",
            Self::TcpClient => "tcp_client",
            Self::Proxy => "proxy",
            Self::Serial => "serial",
            Self::ComMonitor => "com_monitor",
            Self::ComTcp => "com_tcp",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Профиль автоответчика (FR-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autoresponder {
    /// HL7/MLLP (FR-16).
    Hl7Mllp,
    /// ASTM E1381 (FR-17).
    AstmE1381,
    /// Без ответов (FR-18).
    None,
    /// Определение протокола по первому байту (FR-15).
    Auto,
}

impl Autoresponder {
    /// Значение профиля в поле `autoresponder`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hl7Mllp => "hl7_mllp",
            Self::AstmE1381 => "astm_e1381",
            Self::None => "none",
            Self::Auto => "auto",
        }
    }
}

impl fmt::Display for Autoresponder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Направление фрагмента — всегда относительно анализатора и ЛИС.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// От анализатора к ЛИС.
    AnalyzerToLis,
    /// От ЛИС к анализатору.
    LisToAnalyzer,
}

impl Dir {
    /// Значение направления в поле `dir`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnalyzerToLis => "analyzer_to_lis",
            Self::LisToAnalyzer => "lis_to_analyzer",
        }
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Кто закрыл соединение (поле `by` события `close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedBy {
    /// Анализатор.
    Analyzer,
    /// ЛИС.
    Lis,
    /// Само приложение: остановка сессии, ошибка, таймаут.
    Bytetap,
}

impl ClosedBy {
    /// Значение в поле `by`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Analyzer => "analyzer",
            Self::Lis => "lis",
            Self::Bytetap => "bytetap",
        }
    }
}

impl fmt::Display for ClosedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Сведения о машине, на которой шла сессия.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// Описание ОС в формате `os_info`, например `Windows 11 Professional 10.0.22631 [64-bit]`
    /// или `Ubuntu 24.04.1 (noble) [64-bit]`.
    pub os: String,
}

impl Host {
    /// Сведения о текущей машине.
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: os_info::get().to_string(),
        }
    }
}

/// Заголовок сессии (FR-21).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Версия формата журнала, всегда [`FORMAT`].
    pub format: String,
    /// Версия приложения из `Cargo.toml`.
    pub app_version: String,
    /// Начало сессии по системным часам: RFC 3339 с микросекундами и смещением.
    pub started_at: String,
    /// Режим работы.
    pub mode: Mode,
    /// Профиль автоответчика.
    pub autoresponder: Autoresponder,
    /// Настройки сессии: адрес, порт, параметры порта, профиль.
    pub settings: Value,
    /// Модель анализатора, если инженер её указал.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
    /// Комментарий инженера, если он указан при старте.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Машина, на которой шла сессия.
    pub host: Host,
}

impl Session {
    /// Заголовок без необязательных полей `analyzer` и `note`.
    ///
    /// `app_version` берётся из `Cargo.toml` (FR-21), `started_at` — из системных часов.
    #[must_use]
    pub fn new(mode: Mode, autoresponder: Autoresponder, settings: Value, host: Host) -> Self {
        Self {
            format: FORMAT.to_owned(),
            app_version: crate::version().to_owned(),
            started_at: now_rfc3339(),
            mode,
            autoresponder,
            settings,
            analyzer: None,
            note: None,
            host,
        }
    }

    /// Заголовок с началом сессии, заданным снаружи.
    ///
    /// Нужен, чтобы `started_at` заголовка и имя файла (`session_file_name`) считались от
    /// одного момента: [`Self::new`] берёт текущее время сам, а здесь момент передают явно.
    #[must_use]
    pub fn at(
        started: DateTime<Local>,
        mode: Mode,
        autoresponder: Autoresponder,
        settings: Value,
        host: Host,
    ) -> Self {
        let mut session = Self::new(mode, autoresponder, settings, host);
        session.started_at = started.to_rfc3339_opts(SecondsFormat::Micros, false);
        session
    }

    /// Заголовок с моделью анализатора.
    #[must_use]
    pub fn with_analyzer(mut self, analyzer: impl Into<String>) -> Self {
        self.analyzer = Some(analyzer.into());
        self
    }

    /// Заголовок с комментарием инженера.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Открытие соединения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Open {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Номер соединения внутри сессии, с 1.
    pub conn: u32,
    /// Адрес и порт анализатора.
    pub peer: String,
    /// Локальный адрес и порт.
    pub local: String,
}

/// Фрагмент обмена (FR-22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Data {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Номер соединения внутри сессии, с 1.
    pub conn: u32,
    /// Направление фрагмента.
    pub dir: Dir,
    /// `true` для ответов автоответчика (FR-4).
    pub synthetic: bool,
    /// Длина фрагмента в байтах, равна длине декодированного `b64`.
    pub len: u64,
    /// Исходные байты фрагмента без склейки — единственный источник правды.
    pub b64: String,
    /// Копия для чтения глазами по правилам [`crate::text::escape`].
    pub text: String,
}

impl Data {
    /// Событие `data` из исходных байтов фрагмента: `len`, `b64` и `text` выводятся из них.
    #[must_use]
    pub fn new(
        t: impl Into<String>,
        mono_us: u64,
        conn: u32,
        dir: Dir,
        synthetic: bool,
        payload: &[u8],
    ) -> Self {
        Self {
            t: t.into(),
            mono_us,
            conn,
            dir,
            synthetic,
            len: payload.len() as u64,
            b64: base64::engine::general_purpose::STANDARD.encode(payload),
            text: text::escape(payload),
        }
    }
}

/// Закрытие соединения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Close {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Номер соединения внутри сессии, с 1.
    pub conn: u32,
    /// Кто закрыл соединение.
    pub by: ClosedBy,
    /// Причина: `eof`, `reset`, `stop`, `error` и подобные.
    pub reason: String,
}

/// Ошибка обработки соединения. Соединение не обязательно закрывается: ошибка может быть
/// и уровнем ниже, когда обмен продолжается.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Номер соединения; у ошибки уровня сессии его нет.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn: Option<u32>,
    /// Код ошибки для интерфейса: `connect_failed`, `serial_open_failed` и подобные.
    ///
    /// Список кодов — в `docs/reference/session-format.md`. Текст `message` остаётся
    /// английским для журнала, а русскую строку интерфейс берёт по коду из i18n.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Описание ошибки для инженера.
    pub message: String,
}

/// Замечание обработчика, которое не мешает обмену.
///
/// Например: мусор вне рамки MLLP, поле, выброшенное из ACK, входящее подтверждение,
/// которое не подтверждается. Поля те же, что у `error`, но обмен продолжается.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoEvent {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Номер соединения; у замечания уровня сессии его нет.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn: Option<u32>,
    /// Код замечания для интерфейса; список — в `docs/reference/session-format.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Описание для инженера.
    pub message: String,
}

/// Комментарий инженера (FR-23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии; у комментария, дописанного после остановки, их нет.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_us: Option<u64>,
    /// Текст комментария.
    pub text: String,
}

/// Итоги сессии.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stats {
    /// Число соединений за сессию.
    pub connections: u32,
    /// Байты от анализатора к ЛИС.
    pub bytes_analyzer_to_lis: u64,
    /// Байты от ЛИС к анализатору.
    pub bytes_lis_to_analyzer: u64,
}

impl Stats {
    /// Учитывает фрагмент в счётчиках.
    pub fn add(&mut self, dir: Dir, len: u64) {
        match dir {
            Dir::AnalyzerToLis => self.bytes_analyzer_to_lis += len,
            Dir::LisToAnalyzer => self.bytes_lis_to_analyzer += len,
        }
    }

    /// Байты по направлению.
    #[must_use]
    pub const fn bytes(&self, dir: Dir) -> u64 {
        match dir {
            Dir::AnalyzerToLis => self.bytes_analyzer_to_lis,
            Dir::LisToAnalyzer => self.bytes_lis_to_analyzer,
        }
    }
}

/// Итоги сессии, последняя строка при штатной остановке.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnd {
    /// Время по системным часам.
    pub t: String,
    /// Микросекунды от начала сессии по монотонным часам.
    pub mono_us: u64,
    /// Счётчики сессии.
    pub stats: Stats,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_session() -> Session {
        let mut session = Session::new(
            Mode::TcpServer,
            Autoresponder::Hl7Mllp,
            json!({"listen": "0.0.0.0:5000"}),
            Host {
                os: "Windows 11 Professional 10.0.22631 [64-bit]".to_owned(),
            },
        );
        session.started_at = "2026-10-19T10:15:00.123456+03:00".to_owned();
        session
            .with_analyzer("ANALYZER")
            .with_note("комментарий инженера")
    }

    #[test]
    fn session_header_matches_fr21() {
        let event = Event::Session(sample_session());
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "session",
                "format": "bytetap-session/1",
                "app_version": "0.1.0",
                "started_at": "2026-10-19T10:15:00.123456+03:00",
                "mode": "tcp_server",
                "autoresponder": "hl7_mllp",
                "settings": {"listen": "0.0.0.0:5000"},
                "analyzer": "ANALYZER",
                "note": "комментарий инженера",
                "host": {"os": "Windows 11 Professional 10.0.22631 [64-bit]"}
            })
        );
    }

    #[test]
    fn session_omits_empty_optional_fields() {
        let event = Event::Session(Session::new(
            Mode::Proxy,
            Autoresponder::None,
            json!({}),
            Host {
                os: "Ubuntu 24.04.1 (noble) [64-bit]".to_owned(),
            },
        ));
        let value = serde_json::to_value(&event).unwrap();
        assert!(value.get("analyzer").is_none(), "{value}");
        assert!(value.get("note").is_none(), "{value}");
    }

    #[test]
    fn data_event_matches_fr22() {
        let payload = b"\x0bMSH|^~\\&\r\x1c\r";
        let event = Event::Data(Data::new(
            "2026-10-19T10:15:03.000512+03:00",
            3_000_389,
            1,
            Dir::AnalyzerToLis,
            false,
            payload,
        ));
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "data");
        assert_eq!(value["dir"], "analyzer_to_lis");
        assert_eq!(value["synthetic"], false);
        assert_eq!(value["conn"], 1);
        assert_eq!(value["mono_us"], 3_000_389);
        assert_eq!(value["len"], payload.len());
        assert_eq!(value["b64"], "C01TSHxeflwmDRwN");
        assert_eq!(value["text"], "<VT>MSH|^~\\&<CR><FS><CR>");
    }

    #[test]
    fn data_b64_and_text_are_derived_from_the_same_bytes() {
        let payload: Vec<u8> = (0x00..=0xFFu8).collect();
        let data = Data::new("t", 0, 1, Dir::LisToAnalyzer, true, &payload);
        assert_eq!(data.len, 256);
        assert_eq!(data.text, text::escape(&payload));
        assert_eq!(
            data.b64,
            base64::engine::general_purpose::STANDARD.encode(&payload)
        );
        assert_eq!(data.dir, Dir::LisToAnalyzer);
        assert!(data.synthetic);
    }

    #[test]
    fn close_open_error_and_session_end_shapes() {
        let open = Event::Open(Open {
            t: "2026-10-19T10:15:00.100000+03:00".to_owned(),
            mono_us: 1000,
            conn: 1,
            peer: "192.168.1.50:49152".to_owned(),
            local: "192.168.1.10:5000".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(&open).unwrap(),
            json!({
                "type": "open",
                "t": "2026-10-19T10:15:00.100000+03:00",
                "mono_us": 1000,
                "conn": 1,
                "peer": "192.168.1.50:49152",
                "local": "192.168.1.10:5000"
            })
        );

        let close = Event::Close(Close {
            t: "2026-10-19T10:15:09.000000+03:00".to_owned(),
            mono_us: 9_000_000,
            conn: 1,
            by: ClosedBy::Analyzer,
            reason: "eof".to_owned(),
        });
        assert_eq!(serde_json::to_value(&close).unwrap()["by"], "analyzer");

        let error = Event::Error(ErrorEvent {
            t: "2026-10-19T10:15:09.100000+03:00".to_owned(),
            mono_us: 9_100_000,
            conn: Some(1),
            code: Some("checksum_mismatch".to_owned()),
            message: "ASTM checksum mismatch: got 3A, expected 3B".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(&error).unwrap()["message"],
            "ASTM checksum mismatch: got 3A, expected 3B"
        );
        assert_eq!(
            serde_json::to_value(&error).unwrap()["code"],
            "checksum_mismatch"
        );

        let mut stats = Stats::default();
        stats.add(Dir::AnalyzerToLis, 10240);
        stats.add(Dir::LisToAnalyzer, 512);
        stats.connections = 1;
        let end = Event::SessionEnd(SessionEnd {
            t: "2026-10-19T10:15:12.000000+03:00".to_owned(),
            mono_us: 12_000_000,
            stats,
        });
        assert_eq!(
            serde_json::to_value(&end).unwrap()["stats"],
            json!({
                "connections": 1,
                "bytes_analyzer_to_lis": 10240,
                "bytes_lis_to_analyzer": 512
            })
        );
    }

    #[test]
    fn info_event_has_the_error_fields_and_no_connection_when_absent() {
        let info = Event::Info(InfoEvent {
            t: "2026-10-19T10:15:09.100000+03:00".to_owned(),
            mono_us: 9_100_000,
            conn: None,
            code: None,
            message: "4 bytes outside the MLLP frame ignored".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({
                "type": "info",
                "t": "2026-10-19T10:15:09.100000+03:00",
                "mono_us": 9_100_000,
                "message": "4 bytes outside the MLLP frame ignored"
            })
        );
    }

    #[test]
    fn note_without_monotonic_time_omits_the_field() {
        let note = Event::Note(Note {
            t: "2026-10-19T10:20:00.000000+03:00".to_owned(),
            mono_us: None,
            text: "прогнали три пробы".to_owned(),
        });
        let value = serde_json::to_value(&note).unwrap();
        assert!(value.get("mono_us").is_none(), "{value}");
        assert_eq!(value["type"], "note");
        assert_eq!(value["text"], "прогнали три пробы");
    }

    #[test]
    fn events_roundtrip_through_json() {
        let events = [
            Event::Session(sample_session()),
            Event::Open(Open {
                t: "t".to_owned(),
                mono_us: 1,
                conn: 1,
                peer: "p".to_owned(),
                local: "l".to_owned(),
            }),
            Event::Data(Data::new("t", 2, 1, Dir::AnalyzerToLis, false, b"x")),
            Event::Close(Close {
                t: "t".to_owned(),
                mono_us: 3,
                conn: 1,
                by: ClosedBy::Bytetap,
                reason: "stop".to_owned(),
            }),
            Event::Error(ErrorEvent {
                t: "t".to_owned(),
                mono_us: 4,
                conn: None,
                code: None,
                message: "boom".to_owned(),
            }),
            Event::Info(InfoEvent {
                t: "t".to_owned(),
                mono_us: 5,
                conn: Some(1),
                code: Some("protocol_undetected".to_owned()),
                message: "incoming ACK is not acknowledged".to_owned(),
            }),
            Event::Note(Note {
                t: "t".to_owned(),
                mono_us: Some(5),
                text: "n".to_owned(),
            }),
            Event::SessionEnd(SessionEnd {
                t: "t".to_owned(),
                mono_us: 6,
                stats: Stats::default(),
            }),
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
        }
    }

    #[test]
    fn kind_matches_serialized_type() {
        let event = Event::Data(Data::new("t", 0, 1, Dir::AnalyzerToLis, false, b""));
        assert_eq!(serde_json::to_value(&event).unwrap()["type"], event.kind());
    }

    #[test]
    fn mode_and_dir_display_the_wire_values() {
        assert_eq!(Mode::TcpServer.to_string(), "tcp_server");
        assert_eq!(Mode::TcpClient.to_string(), "tcp_client");
        assert_eq!(Mode::Serial.to_string(), "serial");
        assert_eq!(Mode::ComTcp.to_string(), "com_tcp");
        assert_eq!(Dir::LisToAnalyzer.to_string(), "lis_to_analyzer");
        assert_eq!(Autoresponder::AstmE1381.to_string(), "astm_e1381");
        assert_eq!(ClosedBy::Bytetap.to_string(), "bytetap");
    }
}
