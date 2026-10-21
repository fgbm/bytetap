//! Настройки и профили (FR-30, U1-AC7): `settings.json` в `%APPDATA%\bytetap`.
//!
//! Модель — та же, что в Vue-версии (`app/src/settings/settings.ts`): полная форма всех
//! трёх режимов, тема, профили, вид и кодировка ленты, последний старт; плюс размер и
//! положение окна. Чтение терпит старые и испорченные значения: каждое поле, которого
//! нет или которое не того типа, заменяется значением по умолчанию — так переносятся
//! файлы прошлых версий. Файл, который не разбирается как JSON, копируется рядом в
//! `settings.corrupt-<дата>.json`, приложение стартует с умолчаниями и показывает
//! предупреждение. Файл, который не прочитать (права, блокировка), не перезаписывается:
//! хранилище до конца сессии только читает. Настройки v0.1.0 (Tauri, plugin-store в
//! `%APPDATA%\io.github.fgbm.bytetap\settings.json`) переносятся при первом запуске.
//! Запись атомарна: временный файл рядом и переименование.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Папка настроек внутри каталога конфигурации пользователя (`%APPDATA%` на Windows).
pub const DIR: &str = "bytetap";
pub const FILE: &str = "settings.json";

/// Пустой адрес — слушать на всех интерфейсах; так его понимает ядро (`listen_host`).
pub const ALL_INTERFACES: &str = "";
pub const DEFAULT_PORT: u16 = 5000;
pub const DEFAULT_CONNECT_HOST: &str = "192.168.1.50";
pub const DEFAULT_BAUD: u32 = 9600;
pub const DEFAULT_RECONNECT_SEC: u32 = 5;

/// Типовые скорости COM-порта FR-11; остальные значения — «другая…».
pub const BAUD_RATES: [u32; 11] = [
    300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200,
];
pub const BAUD_MIN: u32 = 1;
pub const BAUD_MAX: u32 = 4_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    TcpServer,
    TcpClient,
    Serial,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Responder {
    #[default]
    Auto,
    Hl7,
    Astm,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Even,
    Odd,
    Mark,
    Space,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    #[default]
    None,
    Rtscts,
    Xonxoff,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Вид ленты: текст с управляющими символами или HEX-дамп (FR-26).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedView {
    #[default]
    Text,
    Hex,
}

/// Кодировка текста ленты (FR-27).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Ascii,
    #[default]
    Cp1251,
    Utf8,
    Gbk,
    Latin1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SerialSettings {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: Flow,
    pub dtr: bool,
    pub rts: bool,
}

impl Default for SerialSettings {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud: DEFAULT_BAUD,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow: Flow::None,
            dtr: true,
            rts: true,
        }
    }
}

/// Полная конфигурация всех режимов: поля чужого режима не теряются при переключении.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub mode: SessionMode,
    pub listen_host: String,
    pub listen_port: u16,
    pub connect_host: String,
    pub connect_port: u16,
    pub reconnect_sec: u32,
    pub serial: SerialSettings,
    pub responder: Responder,
    pub astm_nak_on_bad_checksum: bool,
    pub analyzer: String,
    pub note: String,
    pub log_dir: String,
}

impl AppConfig {
    pub fn with_log_dir(log_dir: &str) -> Self {
        Self {
            mode: SessionMode::TcpServer,
            listen_host: ALL_INTERFACES.to_owned(),
            listen_port: DEFAULT_PORT,
            connect_host: DEFAULT_CONNECT_HOST.to_owned(),
            connect_port: DEFAULT_PORT,
            reconnect_sec: DEFAULT_RECONNECT_SEC,
            serial: SerialSettings::default(),
            responder: Responder::Auto,
            astm_nak_on_bad_checksum: false,
            analyzer: String::new(),
            note: String::new(),
            log_dir: log_dir.to_owned(),
        }
    }
}

/// Последний старт: карточка сценария показывает его порт (UI v2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastRun {
    pub mode: SessionMode,
    /// Порт режима: TCP-порт сервера и клиента, имя порта для COM.
    pub port: String,
    /// Адрес анализатора для TCP-клиента; в остальных режимах пуст.
    pub host: String,
    pub baud: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub name: String,
    pub config: AppConfig,
}

/// Выбор вида и кодировки ленты — запоминается вместе с настройками.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FeedSettings {
    pub view: FeedView,
    pub encoding: Encoding,
}

/// Размер окна в точках интерфейса и положение в физических пикселях экрана.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct WindowState {
    /// Левый верхний угол окна в физических пикселях: так положение не зависит от
    /// масштаба монитора, на котором окно создаётся. `None` — окно по центру экрана.
    pub position: Option<[i32; 2]>,
    pub size: [f32; 2],
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub config: AppConfig,
    pub theme: ThemeMode,
    pub profiles: Vec<Profile>,
    pub feed: FeedSettings,
    pub last_run: Option<LastRun>,
    pub window: Option<WindowState>,
}

impl Settings {
    pub fn with_log_dir(log_dir: &str) -> Self {
        Self {
            config: AppConfig::with_log_dir(log_dir),
            theme: ThemeMode::System,
            profiles: Vec::new(),
            feed: FeedSettings::default(),
            last_run: None,
            window: None,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::with_log_dir("")
    }
}

fn text(value: Option<&Value>, fallback: &str) -> String {
    value.and_then(Value::as_str).unwrap_or(fallback).to_owned()
}

fn flag(value: Option<&Value>, fallback: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(fallback)
}

/// Целое в границах, как `Number()` в Vue-версии: число или строка с целым числом;
/// дробное, отрицательное или вне границ — значение по умолчанию.
fn int<T: TryFrom<u64> + Into<u64> + Copy>(
    value: Option<&Value>,
    fallback: T,
    min: T,
    max: T,
) -> T {
    value
        .and_then(|value| match value {
            Value::Number(n) => n.as_u64().or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f <= u64::MAX as f64)
                    .map(|f| f as u64)
            }),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        })
        .filter(|n| (min.into()..=max.into()).contains(n))
        .and_then(|n| T::try_from(n).ok())
        .unwrap_or(fallback)
}

/// Значение перечисления по его имени в JSON; неизвестное — значение по умолчанию.
fn choice<T: for<'de> Deserialize<'de>>(value: Option<&Value>, fallback: T) -> T {
    value
        .filter(|v| v.is_string())
        .and_then(|v| T::deserialize(v).ok())
        .unwrap_or(fallback)
}

fn number(value: Option<&Value>) -> Option<f32> {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .map(|n| n as f32)
}

fn serial(raw: Option<&Value>, fallback: &SerialSettings) -> SerialSettings {
    let Some(raw) = raw.filter(|v| v.is_object()) else {
        return fallback.clone();
    };
    SerialSettings {
        port: text(raw.get("port"), &fallback.port),
        baud: int(raw.get("baud"), fallback.baud, BAUD_MIN, BAUD_MAX),
        data_bits: if raw.get("dataBits").and_then(Value::as_u64) == Some(7) {
            7
        } else {
            8
        },
        parity: choice(raw.get("parity"), fallback.parity),
        stop_bits: if raw.get("stopBits").and_then(Value::as_u64) == Some(2) {
            2
        } else {
            1
        },
        flow: choice(raw.get("flow"), fallback.flow),
        dtr: flag(raw.get("dtr"), fallback.dtr),
        rts: flag(raw.get("rts"), fallback.rts),
    }
}

pub fn normalize_config(raw: Option<&Value>, fallback: &AppConfig) -> AppConfig {
    let Some(raw) = raw.filter(|v| v.is_object()) else {
        return fallback.clone();
    };
    AppConfig {
        mode: choice(raw.get("mode"), fallback.mode),
        listen_host: text(raw.get("listenHost"), &fallback.listen_host),
        listen_port: int(raw.get("listenPort"), fallback.listen_port, 1, u16::MAX),
        connect_host: text(raw.get("connectHost"), &fallback.connect_host),
        connect_port: int(raw.get("connectPort"), fallback.connect_port, 1, u16::MAX),
        reconnect_sec: int(raw.get("reconnectSec"), fallback.reconnect_sec, 0, 3600),
        serial: serial(raw.get("serial"), &fallback.serial),
        responder: choice(raw.get("responder"), fallback.responder),
        astm_nak_on_bad_checksum: flag(
            raw.get("astmNakOnBadChecksum"),
            fallback.astm_nak_on_bad_checksum,
        ),
        analyzer: text(raw.get("analyzer"), &fallback.analyzer),
        note: text(raw.get("note"), &fallback.note),
        log_dir: text(raw.get("logDir"), &fallback.log_dir),
    }
}

fn normalize_profile(raw: &Value, fallback: &AppConfig) -> Option<Profile> {
    let name = raw.get("name")?.as_str()?.trim();
    (!name.is_empty()).then(|| Profile {
        name: name.to_owned(),
        config: normalize_config(raw.get("config"), fallback),
    })
}

/// Последний старт: неизвестный режим забывается, порт остаётся строкой.
fn normalize_last_run(raw: Option<&Value>) -> Option<LastRun> {
    let raw = raw?;
    let mode = raw
        .get("mode")
        .filter(|v| v.is_string())
        .and_then(|v| SessionMode::deserialize(v).ok())?;
    Some(LastRun {
        mode,
        port: text(raw.get("port"), ""),
        host: text(raw.get("host"), ""),
        baud: int(raw.get("baud"), 0, 0, BAUD_MAX),
    })
}

fn normalize_feed(raw: Option<&Value>) -> FeedSettings {
    let base = FeedSettings::default();
    let Some(raw) = raw.filter(|v| v.is_object()) else {
        return base;
    };
    FeedSettings {
        view: choice(raw.get("view"), base.view),
        encoding: choice(raw.get("encoding"), base.encoding),
    }
}

fn normalize_window(raw: Option<&Value>) -> Option<WindowState> {
    let raw = raw?;
    let pair = |value: Option<&Value>| -> Option<[f32; 2]> {
        let items = value?.as_array()?;
        match items.as_slice() {
            [a, b] => Some([number(Some(a))?, number(Some(b))?]),
            _ => None,
        }
    };
    let size = pair(raw.get("size")).filter(|[w, h]| *w > 0.0 && *h > 0.0)?;
    let pixel = |n: f32| (n.abs() < 1e6).then(|| n.round() as i32);
    Some(WindowState {
        position: pair(raw.get("position")).and_then(|[x, y]| Some([pixel(x)?, pixel(y)?])),
        size,
        maximized: flag(raw.get("maximized"), false),
    })
}

/// Настройки из разобранного JSON: всё, чего нет, — значения по умолчанию.
pub fn normalize(raw: &Value, log_dir: &str) -> Settings {
    let base = Settings::with_log_dir(log_dir);
    if !raw.is_object() {
        return base;
    }
    let config = normalize_config(raw.get("config"), &base.config);
    let profiles = raw
        .get("profiles")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| normalize_profile(item, &config))
                .collect()
        })
        .unwrap_or_default();
    Settings {
        theme: choice(raw.get("theme"), ThemeMode::System),
        profiles,
        feed: normalize_feed(raw.get("feed")),
        last_run: normalize_last_run(raw.get("lastRun")),
        window: normalize_window(raw.get("window")),
        config,
    }
}

/// Сохраняет профиль под именем: одноимённый заменяется, остальные остаются на месте.
pub fn upsert_profile(profiles: &mut Vec<Profile>, name: &str, config: &AppConfig) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let next = Profile {
        name: name.to_owned(),
        config: config.clone(),
    };
    match profiles.iter_mut().find(|profile| profile.name == name) {
        Some(existing) => *existing = next,
        None => profiles.push(next),
    }
}

pub fn remove_profile(profiles: &mut Vec<Profile>, name: &str) {
    profiles.retain(|profile| profile.name != name);
}

/// Что случилось с файлом при чтении: показывается строкой под шапкой.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadWarning {
    /// Файл не прочитать (права, блокировка): работают значения по умолчанию, а файл
    /// до конца сессии не перезаписывается — в нём могут быть настройки пользователя.
    Reset { reason: String },
    /// Файл не JSON: копия сохранена по пути (`None` — скопировать не удалось, и тогда
    /// файл не перезаписывается).
    Corrupt { copy: Option<PathBuf> },
}

impl LoadWarning {
    /// Хранилище после этого предупреждения только читает: записать умолчания поверх
    /// файла — потерять то, что в нём было.
    pub fn read_only(&self) -> bool {
        matches!(self, Self::Reset { .. } | Self::Corrupt { copy: None })
    }
}

/// Имя файла настроек v0.1.0 внутри `%APPDATA%\io.github.fgbm.bytetap`.
pub const LEGACY_FILE: &str = "settings.json";

/// Ключ plugin-store Tauri, под которым v0.1.0 хранила настройки.
const LEGACY_KEY: &str = "settings";

/// Пауза между попытками переименования: файл может держать антивирус или индексатор.
const RENAME_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// Файл настроек на диске.
#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
    /// Файл настроек v0.1.0: читается, если своего файла ещё нет.
    legacy: Option<PathBuf>,
    /// Запись выключена до конца сессии: файл не прочитан или испорчен без копии.
    read_only: bool,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            legacy: None,
            read_only: false,
        }
    }

    /// Хранилище, которое переносит настройки v0.1.0 из `legacy`.
    #[must_use]
    pub fn with_legacy(mut self, legacy: PathBuf) -> Self {
        self.legacy = Some(legacy);
        self
    }

    /// Хранилище, которое ничего не пишет.
    #[must_use]
    pub fn into_read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// `settings.json` в каталоге конфигурации пользователя; `None` — каталог не определить.
    /// Настройки v0.1.0 — в папке данных Tauri (`app_data_dir`, тот же `%APPDATA%`).
    pub fn default_location() -> Option<Self> {
        let store = Self::new(dirs::config_dir()?.join(DIR).join(FILE));
        Some(match dirs::data_dir() {
            Some(data) => store.with_legacy(data.join(crate::diag::IDENTIFIER).join(LEGACY_FILE)),
            None => store,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Читает настройки и никогда не падает: ошибка — предупреждение и значения по
    /// умолчанию. Испорченный файл перед перезаписью копируется рядом, а умолчания сразу
    /// записываются, чтобы предупреждение не повторялось при каждом запуске; без копии
    /// файл остаётся как есть.
    pub fn load(&self, log_dir: &str) -> (Settings, Option<LoadWarning>) {
        self.load_at(log_dir, &chrono::Local::now())
    }

    fn load_at<Tz: chrono::TimeZone>(
        &self,
        log_dir: &str,
        now: &chrono::DateTime<Tz>,
    ) -> (Settings, Option<LoadWarning>)
    where
        Tz::Offset: std::fmt::Display,
    {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return (self.migrate(log_dir), None);
            }
            Err(err) => {
                let reason = err.to_string();
                return (
                    Settings::with_log_dir(log_dir),
                    Some(LoadWarning::Reset { reason }),
                );
            }
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(raw) => (normalize(&raw, log_dir), None),
            Err(_) => {
                let copy = self.preserve_corrupt(now);
                let settings = Settings::with_log_dir(log_dir);
                if copy.is_some() && !self.read_only {
                    let _ = self.save(&settings);
                }
                (settings, Some(LoadWarning::Corrupt { copy }))
            }
        }
    }

    /// Своего файла нет: настройки v0.1.0, если они есть, переносятся в новое место.
    fn migrate(&self, log_dir: &str) -> Settings {
        let Some(legacy) = &self.legacy else {
            return Settings::with_log_dir(log_dir);
        };
        let raw = match fs::read(legacy) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Settings::with_log_dir(log_dir);
            }
            Err(err) => {
                crate::diag::error(format!(
                    "настройки v0.1.0 не прочитаны из {}: {err}",
                    legacy.display()
                ));
                return Settings::with_log_dir(log_dir);
            }
        };
        let Some(raw) = raw
            .ok()
            .and_then(|mut raw| raw.get_mut(LEGACY_KEY).map(Value::take))
        else {
            crate::diag::error(format!(
                "настройки v0.1.0 в {} не разобраны: остаются значения по умолчанию",
                legacy.display()
            ));
            return Settings::with_log_dir(log_dir);
        };
        let settings = normalize(&raw, log_dir);
        match self.save(&settings) {
            Ok(()) => crate::diag::info(format!(
                "настройки v0.1.0 перенесены из {} в {}",
                legacy.display(),
                self.path.display()
            )),
            Err(err) => crate::diag::error(format!(
                "настройки v0.1.0 прочитаны из {}, но не сохранены в {}: {err}",
                legacy.display(),
                self.path.display()
            )),
        }
        settings
    }

    /// Копия испорченного файла `settings.corrupt-<дата>.json` рядом с оригиналом.
    fn preserve_corrupt<Tz: chrono::TimeZone>(&self, now: &chrono::DateTime<Tz>) -> Option<PathBuf>
    where
        Tz::Offset: std::fmt::Display,
    {
        let dir = self.path.parent()?;
        let stamp = now.format("%Y%m%d-%H%M%S");
        let copy = dir.join(format!("settings.corrupt-{stamp}.json"));
        fs::copy(&self.path, &copy).ok()?;
        Some(copy)
    }

    /// Атомарная запись: временный файл `settings.json.<pid>.tmp` в той же папке (два
    /// окна не пишут в один временный файл), затем переименование поверх — до трёх
    /// попыток: на Windows файл ненадолго держат антивирус и индексатор.
    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "settings store is read-only for this session",
            ));
        }
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let mut json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
        json.push(b'\n');
        let tmp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }
        let mut attempt = 1;
        loop {
            match fs::rename(&tmp, &self.path) {
                Ok(()) => return Ok(()),
                Err(_) if attempt < 3 => {
                    attempt += 1;
                    std::thread::sleep(RENAME_RETRY);
                }
                Err(err) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(err);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::new(dir.path().join(DIR).join(FILE));
        (dir, store)
    }

    fn sample() -> Settings {
        let mut settings = Settings::with_log_dir("C:/logs");
        settings.config.mode = SessionMode::Serial;
        settings.config.serial.port = "COM5".into();
        settings.config.serial.parity = Parity::Even;
        settings.config.analyzer = "Анализатор 1".into();
        settings.theme = ThemeMode::Dark;
        settings.feed = FeedSettings {
            view: FeedView::Hex,
            encoding: Encoding::Gbk,
        };
        settings.last_run = Some(LastRun {
            mode: SessionMode::Serial,
            port: "COM5".into(),
            host: String::new(),
            baud: 9600,
        });
        settings.window = Some(WindowState {
            position: Some([-1880, 30]),
            size: [1000.0, 700.0],
            maximized: false,
        });
        let config = settings.config.clone();
        upsert_profile(&mut settings.profiles, "  Прибор  ", &config);
        settings
    }

    #[test]
    fn a_missing_file_gives_defaults_without_a_warning() {
        let (_dir, store) = store();
        let (settings, warning) = store.load("D:/logs");
        assert_eq!(settings, Settings::with_log_dir("D:/logs"));
        assert_eq!(warning, None);
    }

    #[test]
    fn saved_settings_read_back_the_same() {
        let (_dir, store) = store();
        let settings = sample();
        store.save(&settings).expect("save");
        let (loaded, warning) = store.load("");
        assert_eq!(warning, None);
        assert_eq!(loaded, settings);
        assert_eq!(loaded.profiles[0].name, "Прибор");
    }

    #[test]
    fn the_file_uses_the_vue_field_names() {
        let value = serde_json::to_value(sample()).expect("json");
        assert_eq!(value["config"]["mode"], "serial");
        assert_eq!(value["config"]["listenPort"], 5000);
        assert_eq!(value["config"]["serial"]["dataBits"], 8);
        assert_eq!(value["config"]["astmNakOnBadChecksum"], false);
        assert_eq!(value["lastRun"]["port"], "COM5");
        assert_eq!(value["feed"]["encoding"], "gbk");
        assert_eq!(value["theme"], "dark");
    }

    #[test]
    fn a_save_leaves_no_temporary_file() {
        let (_dir, store) = store();
        store.save(&sample()).expect("save");
        store.save(&Settings::default()).expect("save again");
        let names: Vec<_> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [FILE]);
    }

    #[test]
    fn a_corrupt_file_is_copied_and_replaced_with_defaults() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"{\"config\": {").unwrap();
        let (settings, warning) = store.load("");
        assert_eq!(settings, Settings::default());
        let Some(LoadWarning::Corrupt { copy: Some(copy) }) = warning else {
            panic!("ожидалось предупреждение с копией, а не {warning:?}");
        };
        let name = copy.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("settings.corrupt-") && name.ends_with(".json"),
            "{name}"
        );
        assert_eq!(fs::read(&copy).unwrap(), b"{\"config\": {");
        // Умолчания уже записаны: следующий запуск без предупреждения.
        assert_eq!(store.load(""), (Settings::default(), None));
    }

    #[test]
    fn a_corrupt_file_without_a_copy_is_left_alone() {
        let (_dir, store) = store();
        let dir = store.path().parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        fs::write(store.path(), b"{\"config\": {").unwrap();
        // Каталог на месте копии: скопировать не выйдет, а писать в папку можно.
        fs::create_dir(dir.join("settings.corrupt-20261021-090507.json")).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-21T09:05:07+03:00").unwrap();
        let (settings, warning) = store.load_at("", &now);
        assert_eq!(settings, Settings::default());
        let warning = warning.expect("предупреждение");
        assert_eq!(warning, LoadWarning::Corrupt { copy: None });
        assert!(warning.read_only());
        assert_eq!(fs::read(store.path()).unwrap(), b"{\"config\": {");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_makes_the_store_read_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, store) = store();
        store.save(&sample()).expect("save");
        let before = fs::read(store.path()).unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(store.path()).is_ok() {
            // root читает и без прав: проверять нечего.
            return;
        }
        let (settings, warning) = store.load("");
        assert_eq!(settings, Settings::default());
        let warning = warning.expect("предупреждение");
        assert!(matches!(&warning, LoadWarning::Reset { reason } if !reason.is_empty()));
        assert!(warning.read_only());
        let store = store.into_read_only();
        assert!(store.save(&Settings::default()).is_err());
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(store.path()).unwrap(), before);
        let names: Vec<_> = fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [FILE], "временных файлов нет");
    }

    /// Файл plugin-store Tauri v0.1.0: объект «ключ → значение», настройки под `settings`.
    const LEGACY_SAMPLE: &str = r#"{
  "settings": {
    "config": {
      "mode": "serial",
      "listenHost": "",
      "listenPort": 5000,
      "connectHost": "192.168.1.50",
      "connectPort": 5000,
      "reconnectSec": 5,
      "serial": { "port": "COM4", "baud": 19200, "dataBits": 8, "parity": "none", "stopBits": 1, "flow": "none", "dtr": true, "rts": true },
      "responder": "astm",
      "astmNakOnBadChecksum": false,
      "analyzer": "Прибор",
      "note": "",
      "logDir": "D:\\journals"
    },
    "theme": "dark",
    "profiles": [{ "name": "Прибор", "config": { "mode": "serial", "serial": { "port": "COM4" } } }],
    "feed": { "view": "hex", "encoding": "utf8" },
    "lastRun": { "mode": "serial", "port": "COM4", "host": "", "baud": 19200 }
  }
}"#;

    #[test]
    fn v0_1_0_settings_are_migrated_once() {
        let (dir, store) = store();
        let legacy = dir.path().join("io.github.fgbm.bytetap").join(LEGACY_FILE);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, LEGACY_SAMPLE).unwrap();
        let store = store.with_legacy(legacy.clone());
        let (settings, warning) = store.load("C:/default");
        assert_eq!(warning, None);
        assert_eq!(settings.config.mode, SessionMode::Serial);
        assert_eq!(settings.config.serial.port, "COM4");
        assert_eq!(settings.config.serial.baud, 19200);
        assert_eq!(settings.config.responder, Responder::Astm);
        assert_eq!(settings.config.log_dir, r"D:\journals");
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.feed.encoding, Encoding::Utf8);
        assert_eq!(settings.last_run.as_ref().map(|r| r.baud), Some(19200));
        // Перенесённое лежит в новом месте; старый файл не тронут.
        assert_eq!(
            Store::new(store.path().to_owned()).load("C:/default").0,
            settings
        );
        assert_eq!(fs::read_to_string(&legacy).unwrap(), LEGACY_SAMPLE);
    }

    #[test]
    fn a_broken_v0_1_0_file_gives_defaults() {
        let (dir, store) = store();
        let legacy = dir.path().join("legacy.json");
        for content in ["{", r#"{"other": 1}"#] {
            fs::write(&legacy, content).unwrap();
            let store = store.clone().with_legacy(legacy.clone());
            assert_eq!(store.load(""), (Settings::default(), None));
            assert!(!store.path().exists());
        }
    }

    #[test]
    fn the_corrupt_copy_is_named_by_the_date() {
        let (_dir, store) = store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), b"not json").unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-21T09:05:07+03:00").unwrap();
        let copy = store.preserve_corrupt(&now).expect("copy");
        assert!(copy.ends_with("settings.corrupt-20261021-090507.json"));
    }

    #[test]
    fn missing_fields_are_filled_with_defaults() {
        let raw = json!({
            "config": { "mode": "tcp_client", "connectPort": 6000, "serial": { "baud": 19200 } },
            "profiles": [{ "name": "Старый", "config": { "listenPort": 7000 } }]
        });
        let settings = normalize(&raw, "E:/logs");
        let mut expected = AppConfig::with_log_dir("E:/logs");
        expected.mode = SessionMode::TcpClient;
        expected.connect_port = 6000;
        expected.serial.baud = 19200;
        assert_eq!(settings.config, expected);
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.feed, FeedSettings::default());
        assert_eq!(settings.last_run, None);
        assert_eq!(settings.window, None);
        // Поля профиля без значения берутся из текущей конфигурации, как в Vue-версии.
        assert_eq!(settings.profiles[0].config.listen_port, 7000);
        assert_eq!(settings.profiles[0].config.connect_port, 6000);
    }

    #[test]
    fn wrong_values_fall_back_one_by_one() {
        let raw = json!({
            "config": {
                "mode": "proxy",
                "listenPort": 70000,
                "connectPort": "5001",
                "reconnectSec": 1.5,
                "responder": "hl7",
                "serial": { "dataBits": 7, "stopBits": 3, "parity": "odd", "flow": "magic", "dtr": "yes" },
                "analyzer": 42
            },
            "theme": "sepia",
            "profiles": [{ "name": "  " }, 5, { "name": "Ок" }],
            "feed": { "view": "hex", "encoding": "koi8" },
            "lastRun": { "mode": "proxy", "port": "5000" },
            "window": { "size": [-1, 600] }
        });
        let settings = normalize(&raw, "");
        let config = &settings.config;
        assert_eq!(config.mode, SessionMode::TcpServer);
        assert_eq!(config.listen_port, DEFAULT_PORT);
        assert_eq!(config.connect_port, 5001);
        assert_eq!(config.reconnect_sec, DEFAULT_RECONNECT_SEC);
        assert_eq!(config.responder, Responder::Hl7);
        assert_eq!(config.serial.data_bits, 7);
        assert_eq!(config.serial.stop_bits, 1);
        assert_eq!(config.serial.parity, Parity::Odd);
        assert_eq!(config.serial.flow, Flow::None);
        assert!(config.serial.dtr);
        assert_eq!(config.analyzer, "");
        assert_eq!(settings.theme, ThemeMode::System);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.feed.view, FeedView::Hex);
        assert_eq!(settings.feed.encoding, Encoding::Cp1251);
        assert_eq!(settings.last_run, None);
        assert_eq!(settings.window, None);
    }

    #[test]
    fn json_that_is_not_an_object_gives_defaults() {
        assert_eq!(normalize(&json!([1, 2]), ""), Settings::default());
        assert_eq!(normalize(&json!(null), ""), Settings::default());
    }

    #[test]
    fn profiles_are_replaced_by_name_and_removed() {
        let mut profiles = Vec::new();
        let mut config = AppConfig::with_log_dir("");
        upsert_profile(&mut profiles, "A", &config);
        upsert_profile(&mut profiles, "B", &config);
        upsert_profile(&mut profiles, "   ", &config);
        config.listen_port = 6000;
        upsert_profile(&mut profiles, "A", &config);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "A");
        assert_eq!(profiles[0].config.listen_port, 6000);
        remove_profile(&mut profiles, "A");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "B");
    }
}
