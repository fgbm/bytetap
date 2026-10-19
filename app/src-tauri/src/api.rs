//! Типы контракта моста: то, что уходит в интерфейс и приходит из него.
//!
//! Имена в JSON — camelCase, как в `app/src/api/types.ts`. Здесь только данные: логика
//! запуска, чтения журнала и ошибок — в соседних модулях, которые тестируются без окна.

use serde::{Deserialize, Serialize};

/// Настройки старта из формы интерфейса (FR-25).
///
/// Поля чужого режима необязательны: интерфейс отправляет только поля выбранного
/// режима, а лишние мост отвергает до создания журнала (контракт M3).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    /// Режим работы: `tcp_server`, `tcp_client` или `serial`.
    pub mode: String,
    /// IPv4-адрес прослушивания; пустая строка — все интерфейсы.
    #[serde(default)]
    pub listen_host: Option<String>,
    /// Порт прослушивания; `0` — любой свободный.
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// Адрес анализатора-сервера: имя узла или IP (FR-2).
    #[serde(default)]
    pub connect_host: Option<String>,
    /// Порт анализатора-сервера.
    #[serde(default)]
    pub connect_port: Option<u16>,
    /// Пауза между попытками подключения, секунды (FR-2).
    #[serde(default)]
    pub reconnect_sec: Option<u64>,
    /// Параметры COM-порта (FR-11).
    #[serde(default)]
    pub serial: Option<SerialConfig>,
    /// Профиль автоответчика: `auto`, `hl7`, `astm` или `none` (FR-14).
    pub responder: String,
    /// Отвечать `NAK` на неверную контрольную сумму ASTM (FR-17).
    #[serde(default)]
    pub astm_nak_on_bad_checksum: bool,
    /// Модель анализатора для заголовка журнала; пусто — не указывать.
    pub analyzer: String,
    /// Комментарий инженера для заголовка журнала; пусто — не указывать.
    pub note: String,
    /// Папка журналов (FR-19).
    pub log_dir: String,
}

/// Настройки COM-порта из формы (FR-11).
///
/// Все поля имеют значения по умолчанию: форма показывает их сразу, поэтому
/// не заполненное поле — не ошибка разбора, а ошибка поля с понятным текстом.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialConfig {
    /// Имя порта: `COM5` или `/dev/ttyUSB0`.
    #[serde(default)]
    pub port: String,
    /// Скорость: 1…4 000 000.
    #[serde(default)]
    pub baud: u32,
    /// Биты данных: 7 или 8.
    #[serde(default = "eight_bits")]
    pub data_bits: u8,
    /// Чётность: `none`, `even`, `odd`, `mark`, `space`.
    #[serde(default = "none_word")]
    pub parity: String,
    /// Стоп-биты: 1 или 2.
    #[serde(default = "one_stop_bit")]
    pub stop_bits: u8,
    /// Управление потоком: `none`, `rtscts`, `xonxoff`.
    #[serde(default = "none_word")]
    pub flow: String,
    /// Начальное состояние DTR.
    #[serde(default = "line_up")]
    pub dtr: bool,
    /// Начальное состояние RTS.
    #[serde(default = "line_up")]
    pub rts: bool,
}

fn eight_bits() -> u8 {
    8
}

fn one_stop_bit() -> u8 {
    1
}

fn none_word() -> String {
    "none".to_owned()
}

fn line_up() -> bool {
    true
}

/// Порт из `list_serial_ports`: имя для открытия и понятное описание (FR-12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SerialPort {
    /// Имя порта: `COM5`, `/dev/ttyUSB0`.
    pub name: String,
    /// Описание для человека: `USB-SERIAL CH340 (COM5)` или имя порта.
    pub description: String,
}

/// Журнал в папке: заголовок и итоги, без чтения всего файла (M3, стартовый экран).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentJournal {
    /// Абсолютный путь к файлу.
    pub path: String,
    /// Начало сессии из заголовка, RFC 3339.
    pub started_at: String,
    /// Модель анализатора; пусто — не указана.
    pub analyzer: String,
    /// Длительность по `session_end`, микросекунды; `0` — журнал не закрыт.
    pub duration_us: u64,
    /// Режим из заголовка: `tcp_server`, `tcp_client` или `serial`.
    pub mode: String,
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
    /// Номер поколения сессии: по нему «Стоп» попадает только в свою сессию (NFR-5).
    pub generation: u64,
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
    /// Индекс первого события в каждой из 601 границы корзин плотности.
    ///
    /// По нему интерфейс переводит долю времени на трассе в номер события и обратно:
    /// рамка видимой области и переход к месту считаются по времени, а не по строкам (AC9).
    pub first_event: Vec<usize>,
}
