//! Работа вне потока окна: сеть, порты, папка журналов, диалоги выбора.
//!
//! Поток окна никогда не ждёт ввод-вывод (этап U1): задача идёт в своём потоке, результат
//! приходит через канал, а окно перерисовывается по `request_repaint`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui;

/// Результат фоновой задачи, который окно забирает в своём кадре.
pub struct Task<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T: Send + 'static> Task<T> {
    /// Запускает `job` в отдельном потоке; по готовности окно перерисовывается.
    pub fn spawn(ctx: &egui::Context, job: impl FnOnce() -> T + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(job());
            ctx.request_repaint();
        });
        Self { receiver }
    }

    /// Готовое значение; `None` — задача ещё идёт или уже отдала результат.
    pub fn poll(&mut self) -> Option<T> {
        self.receiver.try_recv().ok()
    }
}

/// Значение, которое грузится в фоне.
pub enum Loaded<T> {
    /// Ещё не запрашивали.
    Idle,
    Loading(Task<T>),
    Ready(T),
}

impl<T: Send + 'static> Loaded<T> {
    /// Забирает результат, если он пришёл.
    pub fn poll(&mut self) {
        if let Self::Loading(task) = self
            && let Some(value) = task.poll()
        {
            *self = Self::Ready(value);
        }
    }

    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// Адрес ноутбука для подсказки анализатору: IPv4 без loopback, с именем интерфейса.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub addr: String,
}

/// IPv4-адреса интерфейсов без loopback: анализаторы в режиме «вместо ЛИС» работают по
/// IPv4. Только локальный перебор адресов, запросов в сеть нет (FR-33). Провал перебора —
/// пустой список: форма работает и без подсказки.
pub fn list_interfaces() -> Vec<Interface> {
    let mut interfaces: Vec<Interface> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.addr.is_loopback())
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) => Some(Interface {
                name: interface.name,
                addr: v4.ip.to_string(),
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect();
    interfaces.sort_by(|left, right| {
        (left.name.as_str(), left.addr.as_str()).cmp(&(right.name.as_str(), right.addr.as_str()))
    });
    interfaces.dedup();
    interfaces
}

/// Выбор папки журналов; `None` — диалог закрыт.
pub fn pick_folder(start: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    if !start.trim().is_empty() && Path::new(start).is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

/// Выбор файла журнала `*.jsonl`; `None` — диалог закрыт.
pub fn pick_journal(start: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("bytetap", &["jsonl"]);
    if !start.trim().is_empty() && Path::new(start).is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_file()
}

/// Путь, который можно открыть в проводнике: абсолютный и существующий каталог. URL и
/// файлы не принимаются, чтобы кнопка не стала способом запустить что угодно (FR-33).
pub fn folder_to_open(dir: &str) -> Option<PathBuf> {
    let path = Path::new(dir.trim());
    (path.is_absolute() && path.is_dir()).then(|| path.to_path_buf())
}

/// Открывает папку в проводнике ОС; ошибка — текст для журнала запуска.
pub fn open_folder(dir: &Path) -> Result<(), String> {
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(drop)
        .map_err(|err| format!("{program} {}: {err}", dir.display()))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[test]
    fn only_ipv4_addresses_without_loopback_are_listed() {
        for interface in list_interfaces() {
            let addr: Ipv4Addr = interface
                .addr
                .parse()
                .unwrap_or_else(|_| panic!("`{}` — не IPv4", interface.addr));
            assert!(!addr.is_loopback(), "loopback не показывается: {addr}");
            assert!(!interface.name.is_empty(), "у адреса есть имя интерфейса");
        }
    }

    #[test]
    fn only_an_existing_absolute_folder_is_opened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.jsonl");
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(
            folder_to_open(&dir.path().to_string_lossy()),
            Some(dir.path().to_path_buf())
        );
        assert_eq!(folder_to_open(&file.to_string_lossy()), None, "файл");
        assert_eq!(folder_to_open("relative/dir"), None);
        assert_eq!(folder_to_open("https://example.com"), None);
        assert_eq!(
            folder_to_open(&dir.path().join("missing").to_string_lossy()),
            None
        );
    }

    #[test]
    fn a_task_hands_its_result_to_the_window() {
        let ctx = egui::Context::default();
        let mut loaded = Loaded::Loading(Task::spawn(&ctx, || 42));
        for _ in 0..200 {
            loaded.poll();
            if loaded.ready().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(loaded.ready(), Some(&42));
    }
}
