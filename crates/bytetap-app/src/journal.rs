//! Источник страниц журнала: постраничное чтение с LRU на 20 страниц (FR-29).
//!
//! Журнал бывает в сотни мегабайт, поэтому в память окна он не загружается. Лента
//! знает общее число событий из сводки, а строки приходят страницами по [`PAGE_SIZE`]
//! событий. Загруженные страницы лежат в LRU: прокрутка вперёд вытесняет старые, и
//! память не растёт с длиной сессии.
//!
//! Чтение блокирующее, поэтому его зовёт не поток окна, а [`PageReader`] — пул из
//! [`PAGE_WORKERS`] потоков с очередью. Каждая страница запрашивается со своим
//! номером поколения: ответ, пришедший после смены окна или после дописанного
//! комментария, отбрасывается и не затирает свежие данные.
//!
//! Индекс журнала живёт в [`JournalCache`] ядра — он потокобезопасен сам, внешний
//! замок не нужен, и поток окна его не держит.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use bytetap_core::journal::Event;
pub use bytetap_core::journal::view::JournalCache;

/// Сколько событий в одной странице журнала.
pub const PAGE_SIZE: usize = 500;

/// Сколько страниц журнала держим в памяти.
pub const PAGES_KEPT: usize = 20;

/// Сколько байт страниц держим в памяти: большой фрагмент не вытеснит всё остальное.
pub const PAGES_BYTES: usize = 64 * 1024 * 1024;

/// Сколько раз пробуем прочитать страницу, прежде чем показать строку ошибки.
pub const PAGE_TRIES: u8 = 3;

/// Пауза перед повтором чтения страницы; с каждой попыткой длиннее.
pub const PAGE_RETRY: Duration = Duration::from_millis(300);

/// Сколько потоков читают страницы: пул с очередью, а не поток на страницу.
pub const PAGE_WORKERS: usize = 2;

/// Насколько событие в памяти больше своего представления в журнале.
const EVENT_OVERHEAD: usize = 128;

/// Ошибка чтения страницы журнала.
#[derive(Debug, Clone)]
pub struct PageError {
    /// Что случилось: текст для журнала запуска.
    pub message: String,
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PageError {}

/// Что стало со страницей после ответа на её чтение.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLoad {
    /// Страница легла в кеш.
    Stored,
    /// Страница короче, чем обещал индекс: индексы не двигаем, страницу перечитываем.
    Short,
    /// Ответ на устаревший запрос: данные отброшены.
    Stale,
}

/// Незагруженная страница: читается или ждёт повтора.
#[derive(Clone, Copy, Debug)]
enum PageState {
    /// Читается: номер поколения запроса и номер попытки.
    Loading { generation: u64, tries: u8 },
    /// Чтение не удалось: сколько попыток было и когда пробовать снова.
    Failed { tries: u8, at: Instant },
}

/// Источник страниц журнала: события по номерам, страницы в LRU.
///
/// Страницы загружаются по требованию: окно знает, какие номера событий видны, и
/// просит их. Пока страница едет, лента рисует на её месте заполнитель той же высоты:
/// строки не сдвигаются, когда страница придёт.
#[derive(Debug)]
pub struct PageSource {
    path: PathBuf,
    total: usize,
    /// Длины событий из индекса: нужны для высот HEX-строк, не дожидаясь страницы.
    lengths: Vec<u64>,
    pages: HashMap<usize, Vec<Event>>,
    /// Порядок обращения к страницам: последняя — самая свежая.
    order: VecDeque<usize>,
    /// Сколько байт занято загруженными страницами.
    bytes: usize,
    /// Незагруженные страницы: поколение запроса и попытки.
    states: HashMap<usize, PageState>,
    /// Комментарии каждой загруженной страницы: из них берутся комментарии журнала.
    page_notes: HashMap<usize, Vec<String>>,
    /// Счётчик поколений запросов: ответ старого чтения отбрасывается.
    generation: u64,
    /// Версия: растёт, когда пришла новая страница или сменилось число событий.
    version: u64,
}

impl PageSource {
    /// Источник для журнала: путь, общее число событий и длины из индекса.
    pub fn new(path: PathBuf, total: usize, lengths: Vec<u64>) -> Self {
        Self {
            path,
            total,
            lengths,
            pages: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            states: HashMap::new(),
            page_notes: HashMap::new(),
            generation: 0,
            version: 0,
        }
    }

    /// Общее число событий журнала: строки ленты считаются по нему.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Версия источника: меняется, когда пришла страница или изменилось число событий.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Путь к журналу.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Длина события в байтах из индекса; `None` — индекс не дал её.
    pub fn byte_length(&self, index: usize) -> Option<u64> {
        self.lengths.get(index).copied()
    }

    /// Число строк дампа у события: по длине из индекса, без чтения страницы.
    pub fn hex_lines(&self, index: usize) -> usize {
        let len = self.byte_length(index).unwrap_or(0) as usize;
        crate::feed::hex_line_count(len)
    }

    /// Событие по номеру; `None` — страница ещё едет или её не удалось прочитать.
    ///
    /// Обращение двигает страницу в LRU: прокрутка назад не вытесняет то, что видно.
    pub fn event(&mut self, index: usize) -> Option<&Event> {
        if index >= self.total {
            return None;
        }
        let page = index / PAGE_SIZE;
        if !self.pages.contains_key(&page) {
            return None;
        }
        self.touch(page);
        self.pages.get(&page)?.get(index % PAGE_SIZE)
    }

    /// Сколько страниц сейчас в памяти: для проверки LRU и диагностики.
    pub fn pages_in_memory(&self) -> usize {
        self.pages.len()
    }

    /// Сколько байт занято страницами: LRU ограничен и по памяти.
    pub fn bytes_in_memory(&self) -> usize {
        self.bytes
    }

    /// Страницы, покрывающие события `[from, to)`: их надо загрузить.
    ///
    /// Страница в работе и страница, у которой ещё не вышла пауза перед повтором, в
    /// список не попадают. Исчерпавшая попытки страница ждёт строку ошибки.
    pub fn missing_pages(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        if to <= from || self.total == 0 {
            return Vec::new();
        }
        let last_page = self.total.div_ceil(PAGE_SIZE) - 1;
        let first_page = (from / PAGE_SIZE).min(last_page);
        let end_page = ((to - 1) / PAGE_SIZE).min(last_page);
        let now = Instant::now();
        let mut out = Vec::new();
        for page in first_page..=end_page {
            if self.pages.contains_key(&page) {
                continue;
            }
            let requestable = match self.states.get(&page) {
                None => true,
                Some(PageState::Loading { .. }) => false,
                Some(PageState::Failed { tries, at }) => {
                    *tries < PAGE_TRIES && now.duration_since(*at) >= retry_pause(*tries)
                }
            };
            if requestable {
                out.push((page, page * PAGE_SIZE));
            }
        }
        out
    }

    /// Отметить страницу читаемой и выдать номер поколения этого запроса.
    ///
    /// Ответ с другим поколением отбрасывается: пока страница ехала, журнал мог
    /// вырасти после комментария или лента могла уйти в другое место.
    pub fn request_page(&mut self, page: usize) -> u64 {
        let tries = match self.states.get(&page) {
            Some(PageState::Loading { tries, .. } | PageState::Failed { tries, .. }) => *tries,
            None => 0,
        };
        self.generation += 1;
        self.states.insert(
            page,
            PageState::Loading {
                generation: self.generation,
                tries,
            },
        );
        self.generation
    }

    /// Положить прочитанную страницу: события попадают в кеш, LRU обновляется.
    pub fn store_page(&mut self, page: usize, generation: u64, events: Vec<Event>) -> PageLoad {
        let Some(PageState::Loading {
            generation: current,
            tries,
        }) = self.states.get(&page).copied()
        else {
            return PageLoad::Stale;
        };
        if current != generation {
            return PageLoad::Stale;
        }
        self.states.remove(&page);
        let expected = self.expected_events(page);
        if events.len() < expected {
            // Короткая страница: ядро отдало меньше событий, чем обещал индекс. Индексы
            // не сдвигаем — страницу перечитываем, а затем показываем строку ошибки.
            self.states.insert(
                page,
                PageState::Failed {
                    tries: tries.saturating_add(1),
                    at: Instant::now(),
                },
            );
            self.version += 1;
            return PageLoad::Short;
        }
        let notes: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                Event::Note(note) => Some(note.text.clone()),
                _ => None,
            })
            .collect();
        self.bytes += self.page_bytes(page, &events);
        self.page_notes.insert(page, notes);
        self.pages.insert(page, events);
        self.touch(page);
        self.evict();
        self.version += 1;
        PageLoad::Stored
    }

    /// Страница не прочиталась: повтор с паузой, после [`PAGE_TRIES`] — строка ошибки.
    ///
    /// Возвращает `true`, если попытки исчерпаны.
    pub fn page_failed(&mut self, page: usize, generation: u64) -> bool {
        let Some(PageState::Loading {
            generation: current,
            tries,
        }) = self.states.get(&page).copied()
        else {
            return false;
        };
        if current != generation {
            return false;
        }
        let tries = tries.saturating_add(1);
        self.states.insert(
            page,
            PageState::Failed {
                tries,
                at: Instant::now(),
            },
        );
        self.version += 1;
        tries >= PAGE_TRIES
    }

    /// Страница не прочитается никогда: очередь пула закрыта (потоки кончились).
    ///
    /// Страница помечается неудачной сразу, без повторов: иначе лента вечно рисовала бы
    /// на её месте «…» и ждала ответа, которого не будет.
    pub fn page_lost(&mut self, page: usize, generation: u64) {
        let Some(PageState::Loading {
            generation: current,
            ..
        }) = self.states.get(&page).copied()
        else {
            return;
        };
        if current != generation {
            return;
        }
        self.states.insert(
            page,
            PageState::Failed {
                tries: PAGE_TRIES,
                at: Instant::now(),
            },
        );
        self.version += 1;
    }

    /// Через сколько окну повторить запрос страниц, которые ждут повтора.
    pub fn next_retry(&self) -> Option<Duration> {
        let now = Instant::now();
        self.states
            .values()
            .filter_map(|state| match state {
                PageState::Failed { tries, at } if *tries < PAGE_TRIES => {
                    Some(retry_pause(*tries).saturating_sub(now.duration_since(*at)))
                }
                _ => None,
            })
            .min()
    }

    /// Строка события — страница, которую так и не удалось прочитать: рисуем ошибку.
    pub fn page_has_error(&self, index: usize) -> bool {
        if index >= self.total {
            return false;
        }
        matches!(
            self.states.get(&(index / PAGE_SIZE)),
            Some(PageState::Failed { tries, .. }) if *tries >= PAGE_TRIES
        )
    }

    /// Комментарии журнала: новые сверху, не больше `limit`.
    ///
    /// Комментарии берутся из загруженных страниц: незагруженные страницы лента всё
    /// равно не показывает. Порядок — по номеру страницы по убыванию, как в UI v2.
    pub fn notes(&self, limit: usize) -> Vec<String> {
        let mut pages: Vec<usize> = self.pages.keys().copied().collect();
        pages.sort_unstable_by(|left, right| right.cmp(left));
        let mut out: Vec<String> = Vec::new();
        for page in pages {
            let Some(notes) = self.page_notes.get(&page) else {
                continue;
            };
            for note in notes.iter().rev() {
                out.push(note.clone());
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// В журнал дописан комментарий: событий стало больше.
    ///
    /// Последняя страница перечитывается: дописанный комментарий виден и в сводке, и
    /// в ленте без прокрутки. Поколение растёт, поэтому ответ старого чтения её не
    /// затрёт.
    pub fn extend(&mut self, total: usize) {
        if total <= self.total {
            return;
        }
        self.total = total;
        while self.lengths.len() < total {
            self.lengths.push(0);
        }
        let last_page = self.total.div_ceil(PAGE_SIZE).saturating_sub(1);
        self.drop_page(last_page);
        self.states.remove(&last_page);
        self.version += 1;
    }

    /// Обновить длины событий из индекса: журнал мог подрос.
    pub fn set_lengths(&mut self, lengths: Vec<u64>) {
        self.lengths = lengths;
    }

    /// Забыть загруженное: журнал закрыт или открыт другой.
    pub fn clear(&mut self) {
        self.pages.clear();
        self.page_notes.clear();
        self.order.clear();
        self.states.clear();
        self.bytes = 0;
        self.total = 0;
        self.lengths.clear();
        self.version += 1;
    }

    /// Сколько событий обещает индекс для страницы.
    fn expected_events(&self, page: usize) -> usize {
        let start = page * PAGE_SIZE;
        self.total.saturating_sub(start).min(PAGE_SIZE)
    }

    /// Байты страницы в памяти: представление события в журнале плюс накладные.
    fn page_bytes(&self, page: usize, events: &[Event]) -> usize {
        let start = page * PAGE_SIZE;
        events
            .iter()
            .enumerate()
            .map(|(offset, _)| {
                usize::try_from(self.lengths.get(start + offset).copied().unwrap_or(0))
                    .unwrap_or(usize::MAX)
                    .saturating_add(EVENT_OVERHEAD)
            })
            .fold(0usize, usize::saturating_add)
    }

    /// Страница снова самая свежая: прокрутка назад не вытесняет видимое.
    fn touch(&mut self, page: usize) {
        if let Some(at) = self.order.iter().position(|p| *p == page) {
            self.order.remove(at);
        }
        self.order.push_back(page);
    }

    /// Вытеснить давние страницы: предел и по числу, и по памяти.
    fn evict(&mut self) {
        while self.order.len() > 1
            && (self.pages.len() > PAGES_KEPT || self.bytes > PAGES_BYTES)
            && let Some(oldest) = self.order.pop_front()
        {
            self.drop_page(oldest);
        }
    }

    /// Убрать страницу из кеша: её байты уходят из счёта.
    fn drop_page(&mut self, page: usize) {
        if let Some(events) = self.pages.remove(&page) {
            let bytes = self.page_bytes(page, &events);
            self.bytes = self.bytes.saturating_sub(bytes);
        }
        if let Some(at) = self.order.iter().position(|p| *p == page) {
            self.order.remove(at);
        }
        self.page_notes.remove(&page);
    }
}

/// Пауза перед повтором чтения: каждая следующая попытка ждёт дольше.
fn retry_pause(tries: u8) -> Duration {
    PAGE_RETRY.saturating_mul(u32::from(tries.max(1)))
}

/// Задание на чтение страницы.
#[derive(Clone, Copy, Debug)]
pub struct PageJob {
    /// Номер страницы: по нему окно кладёт ответ на место.
    pub page: usize,
    /// Номер первого события страницы.
    pub start: usize,
    /// Сколько событий просить.
    pub count: usize,
    /// Поколение запроса: ответ старого чтения окно отбрасывает.
    pub generation: u64,
}

/// Ответ на чтение страницы.
#[derive(Debug)]
pub struct PageDone {
    /// Номер страницы.
    pub page: usize,
    /// Поколение запроса, с которым страницу просили.
    pub generation: u64,
    /// События страницы или ошибка чтения.
    pub result: Result<Vec<Event>, PageError>,
}

/// Прочитать страницу, превратив панику в ошибку страницы.
///
/// Паника в чтении — это битый индекс или ошибка в кеше ядра. Пока она уносила поток
/// пула, страница не приходила никогда: окно ждало ответа, а очередь заданий оставалась
/// открытой, и [`PageReader::request`] отвечал «поставлено». Теперь паника становится
/// ответом `PageDone` с ошибкой, а поток пула читает следующую страницу.
fn catch_read(
    read: impl FnOnce() -> Result<Vec<Event>, bytetap_core::journal::view::ViewError>,
) -> Result<Vec<Event>, PageError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)) {
        Ok(result) => result.map_err(|err| PageError {
            message: err.to_string(),
        }),
        Err(payload) => Err(PageError {
            message: format!("page read panicked: {}", panic_text(payload.as_ref())),
        }),
    }
}

/// Текст паники из `catch_unwind`: `panic!` с текстом, `String` или неизвестный тип.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no panic message")
}

/// Пул чтения страниц: [`PAGE_WORKERS`] потока и очередь заданий.
///
/// Потоков ровно столько, сколько задано: прокрутка не плодит поток на страницу.
/// Задания идут через канал, ответы приходят в окно через свой канал; окно будит
/// `request_repaint`.
#[derive(Debug)]
pub struct PageReader {
    path: PathBuf,
    jobs: Vec<mpsc::Sender<PageJob>>,
    next: usize,
    done: mpsc::Receiver<PageDone>,
}

impl PageReader {
    /// Запустить пул: потоки живут, пока жива очередь, и сами заканчиваются с ней.
    pub fn new(ctx: &eframe::egui::Context, cache: Arc<JournalCache>, path: PathBuf) -> Self {
        let (done_sender, done) = mpsc::channel();
        let mut jobs = Vec::with_capacity(PAGE_WORKERS);
        for _ in 0..PAGE_WORKERS {
            let (sender, receiver) = mpsc::channel::<PageJob>();
            let cache = Arc::clone(&cache);
            let path = path.clone();
            let done = done_sender.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                while let Ok(job) = receiver.recv() {
                    // Паника в чтении (битый индекс, ошибка в кеше ядра) не должна
                    // убивать поток пула: страница не пришла бы никогда, а очередь
                    // осталась бы живой. Паника становится ошибкой страницы.
                    let result = catch_read(|| cache.read(&path, job.start, job.count));
                    let page = PageDone {
                        page: job.page,
                        generation: job.generation,
                        result,
                    };
                    if done.send(page).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
            jobs.push(sender);
        }
        Self {
            path,
            jobs,
            next: 0,
            done,
        }
    }

    /// Путь журнала, который читает пул.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Поставить страницу в очередь; задания распределяются по потокам по кругу.
    ///
    /// `false` — очередь закрыта: ни один поток пула не принимает задания, и страница
    /// не прочитается. Окно помечает её неудачной, а не ждёт ответа вечно.
    #[must_use]
    pub fn request(&mut self, job: PageJob) -> bool {
        let workers = self.jobs.len();
        for step in 0..workers {
            let at = (self.next + step) % workers;
            if self.jobs[at].send(job).is_ok() {
                self.next = (at + 1) % workers;
                return true;
            }
        }
        false
    }

    /// Готовый ответ, если он уже пришёл.
    pub fn poll(&self) -> Option<PageDone> {
        self.done.try_recv().ok()
    }

    /// Пул без живых потоков: очередь закрыта, задание не принимается.
    ///
    /// Так выглядит пул, у которого умерли потоки: страницы ждать неоткуда, и окно
    /// обязано завести новый (`BytetapApp::request_pages`).
    #[cfg(test)]
    pub(crate) fn dead(path: PathBuf) -> Self {
        let (_sender, done) = mpsc::channel();
        Self {
            path,
            jobs: Vec::new(),
            next: 0,
            done,
        }
    }
}

/// Индекс журнала для источника страниц: сводка, длины и таблица корзин.
#[derive(Debug, Clone)]
pub struct JournalOpen {
    /// Путь к журналу.
    pub path: PathBuf,
    /// Сводка журнала.
    pub summary: bytetap_core::journal::JournalSummary,
    /// Длины событий ленты в байтах.
    pub lengths: Vec<u64>,
    /// Таблица «первое событие корзины».
    pub first_event: Vec<usize>,
}

impl JournalOpen {
    /// Открыть журнал и собрать всё, что нужно ленте и карте.
    ///
    /// Индекс берётся из кеша ядра: он потокобезопасен, внешний замок не нужен, и
    /// поток окна держит его только на время сводки.
    ///
    /// # Errors
    ///
    /// Файла нет, он не читается или это не журнал bytetap.
    pub fn open(cache: &JournalCache, path: &Path) -> Result<Self, PageError> {
        let index = cache.index(path).map_err(|err| PageError {
            message: err.to_string(),
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            summary: index.summary(path),
            lengths: index.lengths().to_vec(),
            first_event: index.first_event().to_vec(),
        })
    }

    /// Дописать комментарий в остановленный журнал (FR-23).
    ///
    /// Вызов блокирующий: файл — работа фоновой задачи.
    ///
    /// # Errors
    ///
    /// Журнал идёт или занят другой программой, файл оборван или только для чтения,
    /// это не журнал bytetap.
    pub fn append_note(path: &Path, text: &str) -> Result<(), PageError> {
        bytetap_core::journal::view::append_note(path, text).map_err(|err| PageError {
            message: err.to_string(),
        })
    }

    /// Кеш журнала вместе с путём: [`JournalOpen::open`] и [`JournalOpen::append_note`]
    /// в одной фоновой задаче.
    ///
    /// Запись и перечитывание различаются: комментарий, который лёг в файл, — уже
    /// успех, и диалог закрывается; сбой перечитывания сводки показывается строкой под
    /// шапкой просмотра, а не как «комментарий не записан» (FR-23, U1-AC19).
    ///
    /// # Errors
    ///
    /// Комментарий не записан: журнал идёт или занят другой программой, файл оборван
    /// или только для чтения, это не журнал bytetap.
    pub fn append_and_reopen(
        cache: &JournalCache,
        path: &Path,
        text: &str,
    ) -> Result<NoteWritten, PageError> {
        Self::append_note(path, text)?;
        Ok(Self::reopen_outcome(cache, path))
    }

    /// Итог перечитывания журнала после комментария: сводка или причина, почему её нет.
    #[must_use]
    pub fn reopen_outcome(cache: &JournalCache, path: &Path) -> NoteWritten {
        match Self::open(cache, path) {
            Ok(opened) => NoteWritten {
                reopened: Some(opened),
                reread_error: None,
            },
            Err(err) => NoteWritten {
                reopened: None,
                reread_error: Some(err.message),
            },
        }
    }
}

/// Комментарий дописан: сводка перечитана, а если не удалось — почему.
///
/// Отдельный тип, а не `Result`: запись и перечитывание — разные шаги, и сбой второго
/// не отменяет первого (FR-23, U1-AC19).
#[derive(Debug)]
pub struct NoteWritten {
    /// Журнал с новым событием; `None` — файл перечитать не удалось.
    pub reopened: Option<JournalOpen>,
    /// Почему журнал не перечитан: строка под шапкой просмотра.
    pub reread_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Data, Dir, Event};

    use super::*;

    fn event(index: usize) -> Event {
        Event::Data(Data::new(
            "2026-10-19T10:15:03.000000+03:00",
            index as u64,
            1,
            Dir::AnalyzerToLis,
            false,
            b"A",
        ))
    }

    fn page_of(page: usize, total: usize) -> Vec<Event> {
        let start = page * PAGE_SIZE;
        let count = total.saturating_sub(start).min(PAGE_SIZE);
        (start..start + count).map(event).collect()
    }

    fn source_with(total: usize) -> PageSource {
        PageSource::new(PathBuf::from("journal.jsonl"), total, vec![0; total])
    }

    /// Журнал на диске: заголовок сессии и один фрагмент.
    fn journal_file(path: &Path) {
        use bytetap_core::journal::{Autoresponder, Host, Mode, Session};

        let header = Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            serde_json::json!({}),
            Host {
                os: "Linux".to_owned(),
            },
        );
        let mut text = serde_json::to_string(&Event::Session(header)).expect("заголовок");
        text.push('\n');
        text.push_str(&serde_json::to_string(&event(0)).expect("событие"));
        text.push('\n');
        std::fs::write(path, text).expect("журнал");
    }

    #[test]
    fn a_page_that_cannot_be_queued_is_marked_unreadable() {
        let mut source = source_with(1000);
        let generation = source.request_page(0);
        source.page_lost(0, generation);
        // Строка ошибки вместо вечного «…»: повторов не будет, ответа тоже.
        assert!(source.page_has_error(0));
        assert!(source.missing_pages(0, 10).is_empty());
        // Ответ от прошлого запроса не помечает свежую страницу.
        let mut source = source_with(1000);
        let stale = source.request_page(0);
        source.request_page(0);
        source.page_lost(0, stale);
        assert!(!source.page_has_error(0));
    }

    #[test]
    fn a_written_comment_is_not_a_failed_one_when_the_reread_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("journal.jsonl");
        journal_file(&path);
        let cache = JournalCache::default();
        // Заголовок испорчен до дописывания: комментарий ляжет — `append_note` сверяет
        // только `type` первой строки и хвост файла, — а индекс журнала такой файл уже
        // не соберёт.
        let text = std::fs::read_to_string(&path).expect("журнал");
        let mut lines: Vec<&str> = text.lines().collect();
        lines[0] = r#"{"type":"session"}"#;
        std::fs::write(&path, lines.join("\n") + "\n").expect("журнал");

        let written = JournalOpen::append_and_reopen(&cache, &path, "перезалили пробу")
            .expect("комментарий записан");
        assert!(
            written.reopened.is_none(),
            "сводки нет: журнал не перечитан"
        );
        assert!(
            written
                .reread_error
                .is_some_and(|reason| !reason.is_empty()),
            "причина сбоя перечитывания"
        );
        // Сбой перечитывания не отменяет записи: комментарий в файле.
        let text = std::fs::read_to_string(&path).expect("журнал");
        assert!(text.contains("перезалили пробу"), "{text}");
    }

    #[test]
    fn the_pages_are_asked_for_the_visible_window() {
        let mut source = source_with(2000);
        assert_eq!(source.missing_pages(0, 10), vec![(0, 0)]);
        source.request_page(0);
        assert!(
            source.missing_pages(0, 10).is_empty(),
            "запрошенная страница повторно не запрашивается"
        );
        // Вторая страница — за событиями 500.
        assert_eq!(source.missing_pages(500, 520), vec![(1, 500)]);
    }

    #[test]
    fn a_loaded_page_is_not_asked_again() {
        let mut source = source_with(1000);
        let generation = source.request_page(0);
        assert_eq!(
            source.store_page(0, generation, page_of(0, 1000)),
            PageLoad::Stored
        );
        assert!(source.missing_pages(0, 10).is_empty());
        assert!(source.event(3).is_some());
    }

    #[test]
    fn the_loaded_pages_are_evicted_by_lru() {
        let mut source = source_with(PAGE_SIZE * (PAGES_KEPT + 2));
        for page in 0..PAGES_KEPT + 2 {
            let generation = source.request_page(page);
            source.store_page(
                page,
                generation,
                page_of(page, PAGE_SIZE * (PAGES_KEPT + 2)),
            );
        }
        assert_eq!(
            source.pages_in_memory(),
            PAGES_KEPT,
            "LRU вытеснил лишние страницы"
        );
        assert!(!source.pages.contains_key(&0), "самый давний вытеснен");
        assert!(source.pages.contains_key(&(PAGES_KEPT + 1)));
    }

    #[test]
    fn a_touched_page_stays_in_the_cache() {
        let mut source = source_with(PAGE_SIZE * (PAGES_KEPT + 1));
        let total = PAGE_SIZE * (PAGES_KEPT + 1);
        for page in 0..PAGES_KEPT {
            let generation = source.request_page(page);
            source.store_page(page, generation, page_of(page, total));
        }
        // Страница 0 снова становится свежей: её видно в ленте.
        assert!(source.event(0).is_some());
        let generation = source.request_page(PAGES_KEPT);
        source.store_page(PAGES_KEPT, generation, page_of(PAGES_KEPT, total));
        assert!(
            source.pages.contains_key(&0),
            "недавно тронутая страница не вытесняется"
        );
    }

    #[test]
    fn the_pages_are_limited_by_memory_too() {
        // События по 100 КБ: страница — около 50 МБ, и в 64 МБ помещается одна-две.
        let total = PAGE_SIZE * PAGES_KEPT;
        let mut source = PageSource::new(
            PathBuf::from("journal.jsonl"),
            total,
            vec![100 * 1024; total],
        );
        for page in 0..PAGES_KEPT {
            let generation = source.request_page(page);
            source.store_page(page, generation, page_of(page, total));
        }
        assert!(
            source.bytes_in_memory() <= PAGES_BYTES,
            "в памяти {} байт",
            source.bytes_in_memory()
        );
        assert!(source.pages_in_memory() < PAGES_KEPT);
    }

    #[test]
    fn the_comment_extends_the_last_page() {
        let mut source = source_with(PAGE_SIZE);
        let generation = source.request_page(0);
        source.store_page(0, generation, page_of(0, PAGE_SIZE));
        assert_eq!(source.total(), PAGE_SIZE);

        source.extend(PAGE_SIZE + 1);
        assert_eq!(source.total(), PAGE_SIZE + 1);
        assert!(
            source.pages.contains_key(&0),
            "первая страница остаётся в кеше"
        );
        // Комментарий — событие 500: он в новой странице, её и надо прочитать.
        assert_eq!(
            source.missing_pages(PAGE_SIZE, PAGE_SIZE + 1),
            vec![(1, PAGE_SIZE)],
            "новая страница запрошена"
        );
    }

    #[test]
    fn the_grown_last_page_is_read_again() {
        let mut source = source_with(PAGE_SIZE + 10);
        let generation = source.request_page(1);
        source.store_page(1, generation, page_of(1, PAGE_SIZE + 10));
        assert!(source.pages.contains_key(&1));
        source.extend(PAGE_SIZE + 20);
        assert!(
            !source.pages.contains_key(&1),
            "последняя страница перечитывается после комментария"
        );
        assert_eq!(
            source.missing_pages(PAGE_SIZE, PAGE_SIZE + 11),
            vec![(1, PAGE_SIZE)],
            "перечитывается той же страницей"
        );
    }

    #[test]
    fn an_answer_of_an_old_generation_does_not_overwrite_the_new_one() {
        let mut source = source_with(PAGE_SIZE + 10);
        let old = source.request_page(1);
        // Журнал вырос: страница перечитывается новым запросом.
        source.extend(PAGE_SIZE + 20);
        let new = source.request_page(1);
        assert_ne!(old, new);
        assert_eq!(
            source.store_page(1, old, page_of(1, PAGE_SIZE + 10)),
            PageLoad::Stale,
            "ответ старого чтения отброшен"
        );
        assert_eq!(
            source.store_page(1, new, page_of(1, PAGE_SIZE + 20)),
            PageLoad::Stored
        );
        assert!(source.event(PAGE_SIZE + 5).is_some());
    }

    #[test]
    fn a_short_page_is_read_again_and_then_reported() {
        let mut source = source_with(PAGE_SIZE * 2);
        for attempt in 0..PAGE_TRIES {
            let generation = source.request_page(0);
            assert_eq!(
                source.store_page(0, generation, vec![event(0)]),
                PageLoad::Short,
                "попытка {attempt}: короткая страница не легла в кеш"
            );
            assert!(source.event(0).is_none(), "индексы не сдвинулись");
        }
        assert!(source.page_has_error(0), "после попыток — строка ошибки");
    }

    #[test]
    fn a_failed_page_is_retried_with_a_pause_and_then_reported() {
        let mut source = source_with(1000);
        let mut exhausted = false;
        for _ in 0..PAGE_TRIES {
            let generation = source.request_page(0);
            exhausted = source.page_failed(0, generation);
        }
        assert!(exhausted, "попытки исчерпаны");
        assert!(source.page_has_error(0));
        assert!(
            source.missing_pages(0, 10).is_empty(),
            "после исчерпания попыток страница не запрашивается"
        );
    }

    #[test]
    fn a_failed_page_waits_before_the_retry() {
        let mut source = source_with(1000);
        let generation = source.request_page(0);
        assert!(!source.page_failed(0, generation));
        assert!(
            source.missing_pages(0, 10).is_empty(),
            "сразу после сбоя страница ждёт паузы"
        );
        assert!(source.next_retry().is_some(), "окно знает, когда повторить");
    }

    #[test]
    fn the_notes_go_from_the_newest_page() {
        let total = PAGE_SIZE * 3;
        let mut source = source_with(total);
        // Страницы 2 и 0 загружены в обратном порядке: комментарий каждой — в её начале.
        for page in [2, 0] {
            let mut events = page_of(page, total);
            events[0] = Event::Note(bytetap_core::journal::Note {
                t: "t".into(),
                mono_us: None,
                text: format!("страница {page}"),
            });
            let generation = source.request_page(page);
            assert_eq!(
                source.store_page(page, generation, events),
                PageLoad::Stored
            );
        }
        // Порядок — по номеру страницы по убыванию, как в UI v2.
        assert_eq!(
            source.notes(3),
            vec!["страница 2".to_owned(), "страница 0".to_owned()]
        );
    }

    #[test]
    fn the_event_comes_from_its_page() {
        let mut source = source_with(600);
        let generation = source.request_page(0);
        source.store_page(0, generation, page_of(0, 600));
        let generation = source.request_page(1);
        source.store_page(1, generation, page_of(1, 600));
        assert!(source.event(0).is_some());
        assert!(source.event(500).is_some());
        assert!(
            source.event(600).is_none(),
            "за последним событием ничего нет"
        );
    }

    /// Паника в чтении становится ошибкой страницы, а не смертью потока пула.
    #[test]
    fn a_panicking_read_becomes_a_page_error() {
        use bytetap_core::journal::view::{ViewError, ViewErrorKind};

        let result = catch_read(|| {
            Err(ViewError {
                path: PathBuf::from("journal.jsonl"),
                kind: ViewErrorKind::NotAJournal,
            })
        });
        assert!(result.is_err(), "ошибка чтения остаётся ошибкой");
        let result = catch_read(|| panic!("broken index"));
        let error = result.expect_err("паника — ошибка страницы");
        assert!(
            error.message.contains("broken index"),
            "текст паники в причине: {}",
            error.message
        );
    }
}
