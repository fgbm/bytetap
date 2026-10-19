//! Типы контракта моста: то, что уходит в интерфейс и приходит из него.
//!
//! Имена в JSON — camelCase, как в `app/src/api/types.ts`. Здесь только данные: логика
//! запуска, чтения журнала и ошибок — в соседних модулях, которые тестируются без окна.

use serde::{Deserialize, Serialize};

/// Настройки старта из формы интерфейса (FR-25).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    /// Режим работы; в M2 доступен только `tcp_server`.
    pub mode: String,
    /// IPv4-адрес прослушивания; пустая строка — все интерфейсы.
    pub listen_host: String,
    /// Порт прослушивания.
    pub listen_port: u16,
    /// Профиль автоответчика: `hl7` (HL7/MLLP) или `none`.
    pub responder: String,
    /// Модель анализатора для заголовка журнала; пусто — не указывать.
    pub analyzer: String,
    /// Комментарий инженера для заголовка журнала; пусто — не указывать.
    pub note: String,
    /// Папка журналов (FR-19).
    pub log_dir: String,
}

/// Адрес ноутбука для подсказки анализатору (FR-25, M2-AC2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Interface {
    /// Имя интерфейса, например `Ethernet` или `Wi-Fi`.
    pub name: String,
    /// IPv4-адрес интерфейса.
    pub addr: String,
}

/// Что вернул успешный старт сессии.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartInfo {
    /// Путь к созданному файлу журнала.
    pub journal_path: String,
    /// Фактический адрес и порт слушателя: при порте 0 его выбрала система.
    pub local_addr: String,
    /// Начало сессии по системным часам, RFC 3339.
    pub started_at: String,
}

/// Счётчики сессии для строки состояния (FR-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    /// Число соединений.
    pub connections: u32,
    /// Байты от анализатора к ЛИС.
    pub bytes_analyzer_to_lis: u64,
    /// Байты от ЛИС к анализатору.
    pub bytes_lis_to_analyzer: u64,
}

/// Итоги остановленной сессии (FR-25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// Счётчики сессии.
    pub stats: SessionStats,
    /// Путь к журналу, который остался на диске.
    pub journal_path: String,
}

/// Плотность байтов по направлениям: 600 корзин на всю сессию (M2-AC9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Density {
    /// Байты от анализатора к ЛИС по корзинам.
    pub a2l: Vec<u64>,
    /// Байты от ЛИС к анализатору по корзинам.
    pub l2a: Vec<u64>,
}

/// Предупреждение о папке журналов: код для перевода и путь для подстановки (FR-19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogDirWarning {
    /// Код из контракта: `onedrive` | `network`.
    pub code: String,
    /// Подстановки текста интерфейса: `dir` — проверенная папка.
    pub params: std::collections::BTreeMap<String, String>,
}

/// Нужна ли перезагрузка странице: идёт ли запись и что вернул её старт (M2-AC1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    /// Сессия идёт или останавливается.
    pub running: bool,
    /// Параметры идущей сессии; `None`, если мост занят запуском или остановкой.
    pub start_info: Option<StartInfo>,
}

/// Сводка открытого журнала (FR-29).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSummary {
    /// Путь к файлу.
    pub path: String,
    /// Заголовок `session` как объект журнала: режим, начало, анализатор, комментарий.
    pub header: serde_json::Value,
    /// Сколько событий ленты можно прочитать постранично.
    pub events: usize,
    /// Длительность сессии по монотонным часам, микросекунды.
    pub duration_us: u64,
    /// Итоги сессии: из `session_end`, а если его нет — посчитанные по событиям.
    pub stats: SessionStats,
    /// Плотность байтов для трассы.
    pub density: Density,
}
