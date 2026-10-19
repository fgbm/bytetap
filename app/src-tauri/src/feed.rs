//! События живой ленты для интерфейса: форма из контракта моста (`app/src/api/types.ts`).
//!
//! Лента строится из тех же событий, что попадают в журнал (FR-22, FR-26): поля берутся
//! из [`bytetap_core::journal::Event`], имена в JSON — camelCase, направление — короткое
//! `a2l` / `l2a`, байты — base64. Строки журнала `session` и `session_end` в ленту не идут:
//! заголовок интерфейс показывает отдельно, а итоги — из ответа `stop_session`.

use std::io;

use bytetap_core::journal::{Dir, Event, JournalFailureReason, OsFamily};
use serde::Serialize;

/// Направление фрагмента: от анализатора к ЛИС (`a2l`) или от ЛИС к анализатору (`l2a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedDir {
    /// От анализатора к ЛИС.
    A2l,
    /// От ЛИС к анализатору.
    L2a,
}

impl From<Dir> for FeedDir {
    fn from(dir: Dir) -> Self {
        match dir {
            Dir::AnalyzerToLis => Self::A2l,
            Dir::LisToAnalyzer => Self::L2a,
        }
    }
}

/// Причина остановки записи для интерфейса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Инженер нажал «Стоп».
    User,
    /// Писатель журнала отказал: запись прервана, записанное сохранено (NFR-5).
    JournalFailure,
}

/// Код причины отказа журнала: интерфейс переводит его, а `message` оставляет в подробностях.
///
/// Вид отказа берётся из структуры причины ([`JournalFailureReason`]): вид ошибки и код ОС
/// приходят из ядра числами, а не текстом, поэтому таблица не зависит от языка системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalFailureCode {
    /// На диске нет места.
    DiskFull,
    /// Нет прав на файл или папку.
    PermissionDenied,
    /// Файл или папка исчезли.
    NotFound,
    /// Сетевой диск недоступен: обрыв связи или удалённый узел пропал.
    Network,
    /// Файл занят другой программой.
    Busy,
    /// Фиксация не завершилась за отведённое время.
    Timeout,
    /// Всё остальное: сбой устройства, сброс, ошибка записи.
    Io,
}

impl JournalFailureCode {
    /// Код из контракта моста: `disk_full` | `permission_denied` | `not_found` |
    /// `network` | `busy` | `timeout` | `io`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskFull => "disk_full",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::Network => "network",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
            Self::Io => "io",
        }
    }

    /// Вид отказа по причине от ядра: сначала код ОС, затем вид ошибки.
    ///
    /// Один и тот же номер в Windows и Unix значит разное (5 — «нет доступа» в Windows
    /// и `EIO` в Linux), поэтому таблица выбирается по семейству ОС из причины.
    #[must_use]
    pub fn from_failure(reason: &JournalFailureReason) -> Self {
        match reason.raw_os_error() {
            Some(code) => Self::from_os_code(reason, code),
            None => Self::from_kind(reason.kind()),
        }
    }

    /// Вид по виду ошибки: работает, когда кода ОС нет — у своих сообщений программы.
    fn from_kind(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::StorageFull => Self::DiskFull,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::TimedOut => Self::Timeout,
            _ => Self::Io,
        }
    }

    /// Вид по номеру ошибки ОС: номер чужого семейства значит не то, что в этом.
    ///
    /// Номера нет в таблице — вид ошибки из причины ещё подсказка: `TimedOut` с кодом
    /// 121 (`ERROR_SEM_TIMEOUT`) на Windows — таймаут, а не сбой ввода-вывода.
    fn from_os_code(reason: &JournalFailureReason, code: i32) -> Self {
        match reason.os() {
            OsFamily::Windows => match code {
                // ERROR_DISK_FULL и ERROR_HANDLE_DISK_FULL.
                112 | 39 => Self::DiskFull,
                // ERROR_ACCESS_DENIED.
                5 => Self::PermissionDenied,
                // ERROR_FILE_NOT_FOUND и ERROR_PATH_NOT_FOUND.
                2 | 3 => Self::NotFound,
                // ERROR_BAD_NETPATH, ERROR_DEV_NOT_EXIST, ERROR_UNEXP_NET_ERR, ERROR_NETNAME_DELETED.
                53 | 55 | 59 | 64 => Self::Network,
                // ERROR_SHARING_VIOLATION и ERROR_LOCK_VIOLATION.
                32 | 33 => Self::Busy,
                _ => Self::from_kind(reason.kind()),
            },
            OsFamily::Unix => match code {
                // ENOSPC.
                28 => Self::DiskFull,
                // EACCES и EPERM.
                13 | 1 => Self::PermissionDenied,
                // ENOENT.
                2 => Self::NotFound,
                _ => Self::from_kind(reason.kind()),
            },
        }
    }
}

/// Событие ленты — дискриминированный union по `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum FeedEvent {
    /// Фрагмент обмена; `b64` — исходные байты, `text` интерфейс строит сам.
    Data {
        /// Время по системным часам.
        t: String,
        /// Микросекунды от начала сессии.
        mono_us: u64,
        /// Номер соединения.
        conn: u32,
        /// Направление фрагмента.
        dir: FeedDir,
        /// `true` для ответа автоответчика.
        synthetic: bool,
        /// Исходные байты фрагмента в base64.
        b64: String,
    },
    /// Открытие соединения.
    Open {
        /// Время по системным часам.
        t: String,
        /// Микросекунды от начала сессии.
        mono_us: u64,
        /// Номер соединения.
        conn: u32,
        /// Адрес и порт анализатора.
        peer: String,
        /// Локальный адрес и порт.
        local: String,
    },
    /// Закрытие соединения.
    Close {
        /// Время по системным часам.
        t: String,
        /// Микросекунды от начала сессии.
        mono_us: u64,
        /// Номер соединения.
        conn: u32,
        /// Кто закрыл соединение.
        by: String,
        /// Причина закрытия.
        reason: String,
    },
    /// Ошибка обработки; обмен при этом может продолжаться.
    Error {
        /// Время по системным часам.
        t: String,
        /// Микросекунды от начала сессии.
        mono_us: u64,
        /// Номер соединения; у ошибки уровня сессии его нет.
        #[serde(skip_serializing_if = "Option::is_none")]
        conn: Option<u32>,
        /// Код ошибки для интерфейса: по нему берётся русская строка из i18n (M3).
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Описание ошибки.
        message: String,
    },
    /// Замечание обработчика: обмен не прерван.
    Info {
        /// Время по системным часам.
        t: String,
        /// Микросекунды от начала сессии.
        mono_us: u64,
        /// Номер соединения; у замечания уровня сессии его нет.
        #[serde(skip_serializing_if = "Option::is_none")]
        conn: Option<u32>,
        /// Код замечания для интерфейса: по нему берётся русская строка из i18n (M3).
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Описание для инженера.
        message: String,
    },
    /// Комментарий инженера.
    Note {
        /// Время по системным часам.
        t: String,
        /// Текст комментария.
        text: String,
    },
    /// Сессия остановлена: запись больше не идёт.
    Stopped {
        /// Почему остановлена.
        reason: StopReason,
        /// Вид отказа журнала для перевода в интерфейсе.
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<JournalFailureCode>,
        /// Подробности отказа журнала: для журнала и отладки.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl FeedEvent {
    /// Событие ленты из строки журнала.
    ///
    /// `None` — строки, которых в ленте нет: заголовок `session` и итоги `session_end`.
    #[must_use]
    pub fn from_session(event: &Event) -> Option<Self> {
        match event {
            Event::Data(data) => Some(Self::Data {
                t: data.t.clone(),
                mono_us: data.mono_us,
                conn: data.conn,
                dir: data.dir.into(),
                synthetic: data.synthetic,
                b64: data.b64.clone(),
            }),
            Event::Open(open) => Some(Self::Open {
                t: open.t.clone(),
                mono_us: open.mono_us,
                conn: open.conn,
                peer: open.peer.clone(),
                local: open.local.clone(),
            }),
            Event::Close(close) => Some(Self::Close {
                t: close.t.clone(),
                mono_us: close.mono_us,
                conn: close.conn,
                by: close.by.as_str().to_owned(),
                reason: close.reason.clone(),
            }),
            Event::Error(error) => Some(Self::Error {
                t: error.t.clone(),
                mono_us: error.mono_us,
                conn: error.conn,
                code: error.code.clone(),
                message: error.message.clone(),
            }),
            Event::Info(info) => Some(Self::Info {
                t: info.t.clone(),
                mono_us: info.mono_us,
                conn: info.conn,
                code: info.code.clone(),
                message: info.message.clone(),
            }),
            Event::Note(note) => Some(Self::Note {
                t: note.t.clone(),
                text: note.text.clone(),
            }),
            Event::Session(_) | Event::SessionEnd(_) => None,
        }
    }
}

/// Пачка событий ленты: всё, что накопилось с прошлой отправки (M2-AC16).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FeedBatch {
    /// События в порядке возникновения.
    pub events: Vec<FeedEvent>,
    /// Сколько событий ленты отброшено из-за переполнения буфера или отставания подписчика.
    pub dropped: u64,
    /// Счётчики сессии на момент пачки: строка состояния берёт их, а не считает по ленте.
    pub stats: crate::api::SessionStats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytetap_core::journal::{Close, ClosedBy, Data, ErrorEvent, InfoEvent, Note, Open, Stats};

    fn data() -> Event {
        Event::Data(Data::new(
            "2026-10-19T10:15:03.000512+03:00",
            3_000_389,
            1,
            Dir::AnalyzerToLis,
            false,
            b"\x0bMSH|^~\\&\r\x1c\r",
        ))
    }

    #[test]
    fn data_event_has_the_contract_shape() {
        let event = FeedEvent::from_session(&data()).expect("data в ленте");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "type": "data",
                "t": "2026-10-19T10:15:03.000512+03:00",
                "monoUs": 3_000_389,
                "conn": 1,
                "dir": "a2l",
                "synthetic": false,
                "b64": "C01TSHxeflwmDRwN"
            })
        );
    }

    #[test]
    fn both_directions_are_short_names() {
        assert_eq!(FeedDir::from(Dir::AnalyzerToLis), FeedDir::A2l);
        assert_eq!(FeedDir::from(Dir::LisToAnalyzer), FeedDir::L2a);
        assert_eq!(serde_json::to_value(FeedDir::A2l).unwrap(), "a2l");
        assert_eq!(serde_json::to_value(FeedDir::L2a).unwrap(), "l2a");
    }

    #[test]
    fn open_close_error_info_and_note_are_mapped() {
        let open = Event::Open(Open {
            t: "t".to_owned(),
            mono_us: 10,
            conn: 2,
            peer: "192.168.1.50:49152".to_owned(),
            local: "127.0.0.1:5000".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(FeedEvent::from_session(&open).unwrap()).unwrap(),
            serde_json::json!({
                "type": "open",
                "t": "t",
                "monoUs": 10,
                "conn": 2,
                "peer": "192.168.1.50:49152",
                "local": "127.0.0.1:5000"
            })
        );

        let close = Event::Close(Close {
            t: "t".to_owned(),
            mono_us: 11,
            conn: 2,
            by: ClosedBy::Analyzer,
            reason: "eof".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(FeedEvent::from_session(&close).unwrap()).unwrap()["by"],
            "analyzer"
        );

        let error = Event::Error(ErrorEvent {
            t: "t".to_owned(),
            mono_us: 12,
            conn: None,
            code: Some("connect_failed".to_owned()),
            message: "boom".to_owned(),
        });
        let value = serde_json::to_value(FeedEvent::from_session(&error).unwrap()).unwrap();
        assert_eq!(value["type"], "error");
        assert_eq!(
            value["code"], "connect_failed",
            "код ошибки доходит до интерфейса"
        );
        assert!(
            value.get("conn").is_none(),
            "у ошибки уровня сессии нет conn"
        );

        let info = Event::Info(InfoEvent {
            t: "t".to_owned(),
            mono_us: 13,
            conn: Some(2),
            code: None,
            message: "note for the engineer".to_owned(),
        });
        let value = serde_json::to_value(FeedEvent::from_session(&info).unwrap()).unwrap();
        assert_eq!(value["conn"], 2);
        assert!(
            value.get("code").is_none(),
            "без кода поле не пишется: интерфейс берёт message"
        );

        let note = Event::Note(Note {
            t: "t".to_owned(),
            mono_us: Some(14),
            text: "три пробы".to_owned(),
        });
        assert_eq!(
            serde_json::to_value(FeedEvent::from_session(&note).unwrap()).unwrap(),
            serde_json::json!({"type": "note", "t": "t", "text": "три пробы"})
        );
    }

    #[test]
    fn journal_only_lines_do_not_reach_the_feed() {
        let header = Event::Session(bytetap_core::journal::Session::new(
            bytetap_core::journal::Mode::TcpServer,
            bytetap_core::journal::Autoresponder::None,
            serde_json::json!({}),
            bytetap_core::journal::Host {
                os: "Linux".to_owned(),
            },
        ));
        assert!(FeedEvent::from_session(&header).is_none());
        let end = Event::SessionEnd(bytetap_core::journal::SessionEnd {
            t: "t".to_owned(),
            mono_us: 15,
            stats: Stats::default(),
        });
        assert!(FeedEvent::from_session(&end).is_none());
    }

    #[test]
    fn stopped_has_reason_and_optional_message() {
        let user = FeedEvent::Stopped {
            reason: StopReason::User,
            code: None,
            message: None,
        };
        assert_eq!(
            serde_json::to_value(&user).unwrap(),
            serde_json::json!({"type": "stopped", "reason": "user"})
        );
        let failure = FeedEvent::Stopped {
            reason: StopReason::JournalFailure,
            code: Some(JournalFailureCode::DiskFull),
            message: Some("storage full (os error 112)".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(&failure).unwrap(),
            serde_json::json!({
                "type": "stopped",
                "reason": "journal_failure",
                "code": "disk_full",
                "message": "storage full (os error 112)"
            })
        );
    }

    /// Причина отказа по коду ОС: тест собирает её для любой системы.
    fn reason(os: OsFamily, code: i32) -> JournalFailureReason {
        JournalFailureReason::from_code(io::ErrorKind::Other, Some(code), os)
    }

    #[test]
    fn a_journal_failure_reason_becomes_a_code_for_the_interface() {
        let windows = |code| JournalFailureCode::from_failure(&reason(OsFamily::Windows, code));
        // Windows: диск полон, доступ запрещён, файла нет.
        assert_eq!(windows(112), JournalFailureCode::DiskFull);
        assert_eq!(windows(39), JournalFailureCode::DiskFull);
        assert_eq!(windows(5), JournalFailureCode::PermissionDenied);
        assert_eq!(windows(2), JournalFailureCode::NotFound);
        assert_eq!(windows(3), JournalFailureCode::NotFound);
        // Сеть и занятый файл — отдельные причины: подсказка в баннере разная.
        assert_eq!(windows(53), JournalFailureCode::Network);
        assert_eq!(windows(55), JournalFailureCode::Network);
        assert_eq!(windows(59), JournalFailureCode::Network);
        assert_eq!(windows(64), JournalFailureCode::Network);
        assert_eq!(windows(32), JournalFailureCode::Busy);
        assert_eq!(windows(33), JournalFailureCode::Busy);

        let unix = |code| JournalFailureCode::from_failure(&reason(OsFamily::Unix, code));
        assert_eq!(unix(28), JournalFailureCode::DiskFull, "ENOSPC");
        assert_eq!(unix(13), JournalFailureCode::PermissionDenied, "EACCES");
        assert_eq!(unix(1), JournalFailureCode::PermissionDenied, "EPERM");
        assert_eq!(unix(2), JournalFailureCode::NotFound, "ENOENT");
        // 5 в Linux — EIO, а не «нет доступа»: номер без семейства ОС ничего не значит.
        assert_eq!(unix(5), JournalFailureCode::Io, "EIO");
        // Номер Windows в Linux — неизвестный: сеть и занятость там другие.
        assert_eq!(unix(53), JournalFailureCode::Io);

        // Номер своей ОС, которого нет в таблице: вид ошибки ещё подсказка.
        // 121 на Windows — ERROR_SEM_TIMEOUT, и баннер должен сказать «таймаут», не «сбой».
        let sem_timeout =
            JournalFailureReason::from_code(io::ErrorKind::TimedOut, Some(121), OsFamily::Windows);
        assert_eq!(
            JournalFailureCode::from_failure(&sem_timeout),
            JournalFailureCode::Timeout
        );

        // Ошибка без кода ОС: вид берётся из вида ошибки.
        let timeout = JournalFailureReason::from_error(&io::Error::new(
            io::ErrorKind::TimedOut,
            "journal not flushed: the writer did not stop within 10s",
        ));
        assert_eq!(
            JournalFailureCode::from_failure(&timeout),
            JournalFailureCode::Timeout
        );
        let other =
            JournalFailureReason::from_message("test hook: the journal device refused the write");
        assert_eq!(
            JournalFailureCode::from_failure(&other),
            JournalFailureCode::Io
        );

        for (code, name) in [
            (JournalFailureCode::DiskFull, "disk_full"),
            (JournalFailureCode::PermissionDenied, "permission_denied"),
            (JournalFailureCode::NotFound, "not_found"),
            (JournalFailureCode::Network, "network"),
            (JournalFailureCode::Busy, "busy"),
            (JournalFailureCode::Timeout, "timeout"),
            (JournalFailureCode::Io, "io"),
        ] {
            assert_eq!(code.as_str(), name);
            assert_eq!(serde_json::to_value(code).unwrap(), name);
        }
    }
}
