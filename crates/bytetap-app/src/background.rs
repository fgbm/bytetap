//! Работа вне потока окна: сеть, порты, папка журналов, диалоги выбора.
//!
//! Поток окна никогда не ждёт ввод-вывод (этап U1): задача идёт в своём потоке, результат
//! приходит через канал, а окно перерисовывается по `request_repaint`.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui;

/// Результат фоновой задачи, который окно забирает в своём кадре.
pub struct Task<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T: Send + 'static> Task<T> {
    /// Запускает `job` в отдельном потоке; по готовности окно перерисовывается.
    ///
    /// Кадр просится только у живого приёмника: задачу могли выгрузить раньше — например,
    /// экран получил фикстуры вместо опроса системы, — и запрос от выброшенной задачи
    /// разбудил бы окно в простое (NFR-5).
    pub fn spawn(ctx: &egui::Context, job: impl FnOnce() -> T + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if sender.send(job()).is_ok() {
                ctx.request_repaint();
            }
        });
        Self { receiver }
    }

    /// Запускает `future` в runtime приложения; по готовности окно перерисовывается.
    pub fn spawn_async(
        runtime: &tokio::runtime::Handle,
        ctx: &egui::Context,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        runtime.spawn(async move {
            if sender.send(future.await).is_ok() {
                ctx.request_repaint();
            }
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

/// Адрес источника исходящего соединения к `host:port` — тот, с которого вторая сторона
/// увидит ноутбук (FR-8).
///
/// Соединение UDP ничего не отправляет: `connect` только выбирает локальный адрес по
/// таблице маршрутов, и `local_addr` отдаёт адрес интерфейса, через который пошёл бы
/// обмен. Имя цели разрешается здесь же, поэтому вызов идёт в фоновом потоке. `None` —
/// цель не разрешилась или соединение невозможно: тогда форма показывает адрес
/// прослушивания.
pub fn source_addr(host: &str, port: u16) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || port == 0 {
        return None;
    }
    let target = (host, port).to_socket_addrs().ok()?.next()?;
    let bind = match target {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let socket = std::net::UdpSocket::bind(bind).ok()?;
    socket.connect(target).ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
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
    fn the_source_address_of_a_loopback_target_is_loopback() {
        assert_eq!(source_addr("127.0.0.1", 9).as_deref(), Some("127.0.0.1"));
        assert_eq!(source_addr("", 5000), None, "цель не набрана");
        assert_eq!(source_addr("127.0.0.1", 0), None, "порт не набран");
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

    /// Считает запросы перерисовки: фоновая задача или само окно — неважно, важно число.
    fn count_repaints(ctx: &egui::Context) -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        ctx.set_repaint_observer(move |_| {
            let _ = sender.send(());
        });
        receiver
    }

    /// Причины запросов последнего прохода: файл, строка и текст.
    ///
    /// Причины копятся между проходами, поэтому перед чтением нужен проход
    /// (`Context::run`): он переносит их в то, что видит [`egui::Context::repaint_causes`].
    fn repaint_causes(ctx: &egui::Context) -> Vec<String> {
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        // Текстуры шрифтов некому применить: без этого `TexturesDelta` ругается на выходе.
        output.textures_delta.clear();
        ctx.repaint_causes()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Готовая задача будит окно: без кадра результат некому показать.
    #[test]
    fn a_ready_task_wakes_the_window() {
        let ctx = egui::Context::default();
        let requests = count_repaints(&ctx);
        let task = Task::spawn(&ctx, || 7);
        requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("готовый результат просит кадр");
        let causes = repaint_causes(&ctx);
        assert!(
            causes.iter().any(|cause| cause.contains("background.rs")),
            "кадр просит фоновая задача: {causes:?}"
        );
        drop(task);
    }

    /// Выгруженная задача окно не будит: приёмника нет, и результат некому показать.
    ///
    /// Так бывает на старте: экран получает фикстуры (снимки, тесты) вместо опроса системы
    /// и выбрасывает задачу. Запрос от неё разбудил бы окно в простое, а простой без кадров
    /// и есть требование NFR-5.
    #[test]
    fn a_dropped_task_does_not_wake_the_window() {
        let ctx = egui::Context::default();
        let requests = count_repaints(&ctx);
        // Задача ждёт, пока её приёмник выбросят: иначе она успела бы отдать результат.
        let (gate, gate_rx) = mpsc::channel::<()>();
        let (done, finished) = mpsc::channel::<()>();
        let task = Task::spawn(&ctx, move || {
            let _ = gate_rx.recv();
            let _ = done.send(());
        });
        drop(task);
        let _ = gate.send(());
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("задача отработала");
        // Запрос идёт сразу после отправки результата: даём потоку его сделать.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            requests.try_recv().is_err(),
            "выгруженная задача не просит кадр: {:?}",
            repaint_causes(&ctx)
        );
    }
}
