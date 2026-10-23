//! Просмотр журнала: шапка, строка о персональных данных, карта сессии и лента
//! (FR-29, FR-34, U1-AC18, U1-AC19).
//!
//! Экран открывается из меню «Журналы → Открыть журнал…», из списка последних
//! записей и из строки отказа журнала. Данные журнала читает модуль ядра
//! `bytetap_core::journal::view`: окно получает сводку, таблицу корзин и страницы
//! событий, а читает их фоновая задача — поток окна не ждёт ввод-вывод.
//!
//! Лента — та же, что при записи: те же строки, тот же HEX-вид, те же кодировки.
//! Отличается источник: события приходят страницами по 500, а не из кольцевого буфера.

use std::path::{Path, PathBuf};

use bytetap_core::journal::view::DENSITY_BUCKETS;
use bytetap_core::journal::{Event, JournalSummary, Mode, Session};
use eframe::egui::Rect;

use crate::details::Details;
use crate::feed::FeedEvent;
use crate::feedrows::Pair;
use crate::feedscroll::FeedScroll;
use crate::journal::{PAGE_SIZE, PageSource};
use crate::selection::{self, CopyKind, Selection};
use crate::settings::ProxyMode;
use crate::t;
use crate::theme::Palette;

/// Сколько комментариев журнала показывает строка под шапкой.
pub const NOTES_SHOWN: usize = 3;

/// Экран просмотра журнала: открытый файл, источник страниц и рамка карты.
pub struct JournalView {
    /// Путь к журналу.
    pub path: PathBuf,
    /// Сводка журнала: события, длительность, плотность, таблица корзин.
    pub summary: JournalSummary,
    /// Источник страниц ленты.
    pub source: PageSource,
    /// Таблица «первое событие корзины»: перевод доли времени в номер события.
    pub first_event: Vec<usize>,
    /// Видимая область ленты в долях времени — рамка на карте сессии.
    pub visible: Option<(f32, f32)>,
    /// Прокрутка к номеру события: `None` — не прокручивать.
    pub seek: Option<usize>,
    /// Строка, по которой кликнули при закрытой панели: её место на экране до того, как
    /// панель забрала низ ленты. Подтягивается со следующего кадра — по высоте панели.
    pub reveal: Option<Rect>,
    /// Прокрутка ленты в единицах строк: смещение не теряет точность на миллионах строк.
    pub scroll: FeedScroll,
    /// Видимая область ленты без полосы прокрутки: по ней видно, помещается ли строка
    /// целиком (лента высотой не кратной строке режет последнюю строку).
    pub feed_rect: Option<Rect>,
    /// Префиксные суммы строк HEX-дампа: считаются один раз при первом HEX-виде.
    hex_prefix: Option<Vec<u64>>,
    /// Выбранное событие и панель деталей (L1): событие берётся копией, поэтому
    /// выгруженная из памяти страница журнала панель не закрывает.
    pub details: Details,
    /// Выделенные строки блоком: номера событий журнала (L1, шаг 2).
    pub selection: Selection,
    /// Копирование выделения, ждущее страниц: строки на незагруженных страницах сперва
    /// дочитываются, а в буфер уходит уже весь выделенный блок.
    pub copy_job: Option<CopyJob>,
    /// Что сказать о последнем копировании: ждём страницы или предел сработал.
    pub copy_notice: Option<String>,
}

/// Копирование выделения в просмотре журнала, идущее по мере чтения страниц.
///
/// Страницы журнала читаются по видимой области, поэтому выделенный блок может лежать
/// на страницах, которых ещё нет в памяти. Строки собираются **курсором по возрастанию
/// номеров**: страница прочитана — её строки сразу ложатся в текст. Поэтому большой
/// блок не держит в памяти весь журнал, не пересобирается каждый кадр, а LRU страниц не
/// вытесняет то, что ещё не прочитано: его читают по порядку и один раз (находка B1
/// ревью шага 2).
#[derive(Clone, Debug, Default)]
pub struct CopyJob {
    /// Что класть в буфер: текст с временем, HEX или Base64.
    pub kind: CopyKind,
    /// Номера выделенных событий по возрастанию — не больше предела строк.
    pub indices: Vec<usize>,
    /// Сколько событий было выделено: по нему видно, что предел строк сработал.
    pub total: usize,
    /// Сколько номеров уже разобрано.
    pub cursor: usize,
    /// Сколько строк добавлено в текст.
    pub rows: usize,
    /// Сколько байт занял текст.
    pub bytes: usize,
    /// Сколько строк не прочитано: страница журнала не читается.
    pub skipped: usize,
    /// Текст для буфера: копится по мере прихода страниц.
    pub text: String,
    /// Предел строк или байт сработал.
    pub truncated: bool,
}

impl CopyJob {
    /// Задание на выделение: номера не длиннее предела строк.
    pub fn new(kind: CopyKind, mut indices: Vec<usize>, total: usize) -> Self {
        // Предел строк уже мог отсечь номера при сборе задания: тогда в буфер войдёт
        // меньше, чем выделено, и об этом надо сказать.
        let truncated = total > indices.len();
        indices.truncate(selection::COPY_MAX_ROWS);
        Self {
            kind,
            indices,
            total,
            truncated,
            ..Self::default()
        }
    }
}

impl JournalView {
    /// Экран для открытого журнала: сводка, таблица корзин и источник страниц.
    pub fn new(
        path: PathBuf,
        summary: JournalSummary,
        lengths: Vec<u64>,
        first_event: Vec<usize>,
    ) -> Self {
        let total = summary.events;
        Self {
            path: path.clone(),
            summary,
            source: PageSource::new(path, total, lengths),
            first_event,
            visible: None,
            seek: None,
            reveal: None,
            scroll: FeedScroll::default(),
            feed_rect: None,
            hex_prefix: None,
            details: Details::default(),
            selection: Selection::default(),
            copy_job: None,
            copy_notice: None,
        }
    }

    /// Общее число событий журнала.
    pub fn events(&self) -> usize {
        self.summary.events
    }

    /// Номер события по доле времени на карте: как в UI v2.
    ///
    /// Плотность нарисована по времени, а лента считает строки событиями: таблица
    /// `first_event` связывает их.
    pub fn event_at_fraction(&self, fraction: f32) -> usize {
        event_at_fraction(&self.first_event, self.summary.events, fraction)
    }

    /// Доля времени по номеру события: обратное к [`Self::event_at_fraction`].
    pub fn fraction_at_event(&self, index: usize) -> f32 {
        fraction_at_event(&self.first_event, index)
    }

    /// Сколько строк в ленте: в виде «Текст» — по событиям, в HEX — по 16 байт.
    pub fn rows(&mut self, hex: bool) -> usize {
        if !hex {
            return self.source.total();
        }
        self.hex_rows()
    }

    /// Префиксные суммы строк HEX-дампа: `prefix[i]` — строк до события `i`.
    fn hex_prefix(&mut self) -> &[u64] {
        if self.hex_prefix.is_none() {
            let total = self.source.total();
            let mut prefix = Vec::with_capacity(total + 1);
            prefix.push(0u64);
            for index in 0..total {
                let lines = self.source.hex_lines(index) as u64;
                let last = prefix.last().copied().unwrap_or(0);
                prefix.push(last + lines);
            }
            self.hex_prefix = Some(prefix);
        }
        self.hex_prefix.as_deref().unwrap_or_default()
    }

    /// Сколько строк в HEX-виде.
    fn hex_rows(&mut self) -> usize {
        self.hex_prefix().last().copied().unwrap_or(0) as usize
    }

    /// Событие и номер его строки в HEX-виде для строки экрана `row`.
    ///
    /// `None` — строка за последним событием.
    pub fn hex_locate(&mut self, row: usize) -> Option<(usize, usize)> {
        let prefix = self.hex_prefix();
        let line = row as u64;
        if prefix.len() < 2 {
            return None;
        }
        // Первое событие, строки которого кончаются не раньше `line`.
        let mut low = 0usize;
        let mut high = prefix.len() - 1;
        while low < high {
            let middle = low.div_ceil(2) + (high - low) / 2;
            let middle = middle.max(low + 1).min(high);
            if prefix[middle] <= line {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let start = prefix[low];
        if line >= prefix[low + 1] {
            return None;
        }
        Some((low, (line - start) as usize))
    }

    /// Номер события для строки экрана `row` в HEX-виде.
    pub fn event_of_row(&mut self, row: usize, hex: bool) -> Option<(usize, usize)> {
        if hex {
            self.hex_locate(row)
        } else {
            (row < self.source.total()).then_some((row, 0))
        }
    }

    /// Сбросить префиксные суммы: журнал вырос после комментария.
    pub fn invalidate(&mut self) {
        self.hex_prefix = None;
    }

    /// Номер строки HEX-вида, с которой начинается событие `index`.
    pub fn hex_row_of(&mut self, index: usize) -> usize {
        self.hex_prefix().get(index).copied().unwrap_or(0) as usize
    }
}

/// Номер события для доли времени на карте журнала.
///
/// `first_event[b]` — первое событие корзины `b`; правая граница сессии событий не
/// содержит, поэтому к последнему событию.
#[must_use]
pub fn event_at_fraction(first_event: &[usize], total: usize, fraction: f32) -> usize {
    if total == 0 || first_event.is_empty() {
        return 0;
    }
    let point = (fraction.clamp(0.0, 1.0) * DENSITY_BUCKETS as f32).round() as usize;
    let point = point.min(DENSITY_BUCKETS);
    first_event
        .get(point)
        .copied()
        .unwrap_or(total)
        .min(total.saturating_sub(1))
}

/// Доля времени для номера события: корзина, в которой оно первое.
///
/// Обратное к [`event_at_fraction`]: рамка видимой области показывает время первой
/// видимой строки, а не её место в списке.
#[must_use]
pub fn fraction_at_event(first_event: &[usize], index: usize) -> f32 {
    if first_event.is_empty() {
        return 0.0;
    }
    let mut low = 0usize;
    let mut high = DENSITY_BUCKETS;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if first_event.get(middle).copied().unwrap_or(usize::MAX) <= index {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low as f32 / DENSITY_BUCKETS as f32
}

/// Плотность корзин в доли высоты дорожки: пустой журнал даёт нули.
#[must_use]
pub fn density_bars(values: &[u64]) -> Vec<f32> {
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return vec![0.0; values.len()];
    }
    values
        .iter()
        .map(|value| (*value as f32 / max as f32).clamp(0.0, 1.0))
        .collect()
}

/// Имя файла журнала без папки: путь приходит и из Windows, и из Unix.
#[must_use]
pub fn file_name(path: &Path) -> String {
    let text = path.to_string_lossy();
    let cut = text.rfind(['/', '\\']).map_or(0, |at| at + 1);
    text[cut..].to_owned()
}

/// Длительность журнала «00:42:18»: с часами всегда.
#[must_use]
pub fn duration(duration_us: u64) -> String {
    let total = duration_us / 1_000_000;
    let hours = total / 3600;
    let minutes = total % 3600 / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Комментарии журнала в одну строку: сначала из заголовка, затем дописанные.
///
/// `appended` — новые первыми, в строке они идут от старых к новым.
#[must_use]
pub fn notes_line(header_note: &str, appended: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !header_note.trim().is_empty() {
        parts.push(header_note);
    }
    for note in appended.iter().rev() {
        if !note.trim().is_empty() {
            parts.push(note);
        }
    }
    parts.join(" · ")
}

/// Пара сторон журнала: у прокси — по `settings.role` заголовка (M4-AC13), у прослушки —
/// всегда пара двух портов (M5-В8).
///
/// У остальных режимов пары нет, и строки ленты остаются прежними: «#1 отключился · ЛИС».
#[must_use]
pub fn pair_of_journal(header: &Session) -> Pair {
    match header.mode {
        Mode::ComMonitor => Pair::Monitor,
        Mode::Proxy => match role_of_journal(header) {
            ProxyMode::AnalyzerClient => Pair::TargetLis,
            ProxyMode::AnalyzerServer => Pair::TargetAnalyzer,
        },
        _ => Pair::None,
    }
}

/// Топология из заголовка журнала; в старом журнале поля нет — «анализатор — клиент»,
/// как у умолчания настроек.
#[must_use]
pub fn role_of_journal(header: &Session) -> ProxyMode {
    match header
        .settings
        .get("role")
        .and_then(serde_json::Value::as_str)
    {
        Some("analyzer_server") => ProxyMode::AnalyzerServer,
        _ => ProxyMode::AnalyzerClient,
    }
}

/// Сводка пары сторон прокси из заголовка: `settings` знает порт прослушивания и цель,
/// а принятая сторона в заголовок не пишется — её даёт только `open` ленты.
#[must_use]
pub fn proxy_summary(header: &Session) -> Option<String> {
    if header.mode != Mode::Proxy {
        return None;
    }
    let listen = header
        .settings
        .get("listen")
        .and_then(serde_json::Value::as_str)?;
    let target = header
        .settings
        .get("target")
        .and_then(serde_json::Value::as_str)?;
    let port = listen
        .rsplit_once(':')
        .map_or_else(|| listen.to_owned(), |(_, port)| format!(":{port}"));
    Some(crate::recording::pair_summary(
        role_of_journal(header),
        &port,
        target,
        None,
        None,
    ))
}

/// Порты прослушки из заголовка журнала: анализатор и ЛИС (M5-AC6).
///
/// Пара портов известна целиком с самого старта — в отличие от прокси, где вторая сторона
/// появляется только со своим соединением. Сводку по ним собирает та же функция, что у
/// шапки записи (`recording::monitor_summary`), чтобы подписи не расходились.
fn monitor_ports(header: &Session) -> Option<(String, String)> {
    if header.mode != Mode::ComMonitor {
        return None;
    }
    let port = |side: &str| {
        header
            .settings
            .get(side)?
            .get("port")?
            .as_str()
            .map(str::to_owned)
    };
    Some((port("analyzer")?, port("lis")?))
}

/// Подпись журнала в шапке просмотра: «ANALYZER · 00:42:18»; у журнала прокси вместо
/// модели анализатора — сводка двух сторон, у журнала прослушки — сводка двух портов,
/// а имя анализатора, если оно записано, стоит перед ней (M4-AC13, M5-AC6).
#[must_use]
pub fn journal_meta(header: &Session, duration_us: u64) -> String {
    let span = duration(duration_us);
    let analyzer = header.analyzer.clone().filter(|value| !value.is_empty());
    let monitor = monitor_ports(header)
        .map(|(analyzer, lis)| crate::recording::monitor_summary(&analyzer, &lis));
    match monitor.or_else(|| proxy_summary(header)) {
        Some(summary) => match analyzer {
            Some(analyzer) => format!("{analyzer} · {summary} · {span}"),
            None => format!("{summary} · {span}"),
        },
        None => {
            let analyzer = analyzer.unwrap_or_else(|| t!("journal.notSpecified").to_owned());
            format!("{analyzer} · {span}")
        }
    }
}

/// Событие ленты из строки журнала: заголовок и итоги в ленту не идут.
#[must_use]
pub fn feed_event(event: &Event) -> Option<FeedEvent> {
    FeedEvent::from_session(event)
}

/// Сколько событий в одной странице журнала.
pub const PAGE_EVENTS: usize = PAGE_SIZE;

/// Палитра экрана просмотра — общая с записью.
pub type ViewPalette = Palette;

#[cfg(test)]
mod tests {
    use super::*;

    /// Таблица корзин: события в двух пачках — в начале и в конце сессии.
    ///
    /// `first_event[b]` — первое событие корзины `b` или правее, поэтому таблица
    /// не убывает: корзина 0 начинается с события 0, корзины 1…599 — с события 2
    /// (вторая пачка в конце сессии), последняя точка — с общего числа событий.
    fn table() -> Vec<usize> {
        let mut out = vec![2; DENSITY_BUCKETS + 1];
        out[0] = 0;
        out[DENSITY_BUCKETS] = 4;
        out
    }

    /// Таблица, где каждая корзина начинается со своего события: перевод точный.
    fn even_table() -> Vec<usize> {
        (0..=DENSITY_BUCKETS).collect()
    }

    #[test]
    fn the_fraction_gives_the_event_of_its_bucket() {
        let table = table();
        assert_eq!(event_at_fraction(&table, 4, 0.0), 0);
        assert_eq!(event_at_fraction(&table, 4, 0.5), 2);
        // Правая граница сессии событий не содержит: к последнему событию.
        assert_eq!(event_at_fraction(&table, 4, 1.0), 3);
    }

    #[test]
    fn the_event_gives_the_fraction_of_its_bucket() {
        let table = table();
        assert_eq!(fraction_at_event(&table, 0), 0.0);
        // Вторая пачка лежит в последней корзине: её время — конец сессии.
        assert!(
            (fraction_at_event(&table, 2) - 599.0 / 600.0).abs() < 1e-6,
            "{}",
            fraction_at_event(&table, 2)
        );
        // Позднее событие — правее.
        assert!(fraction_at_event(&table, 3) > fraction_at_event(&table, 1));
    }

    #[test]
    fn the_translations_are_inverses() {
        let table = even_table();
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let event = event_at_fraction(&table, DENSITY_BUCKETS, fraction);
            let back = fraction_at_event(&table, event);
            assert!(
                (back - fraction).abs() < 2.0 / DENSITY_BUCKETS as f32 + 1e-6,
                "{fraction} → {event} → {back}"
            );
        }
    }

    #[test]
    fn the_event_of_a_later_fraction_is_not_earlier() {
        let table = table();
        let mut previous = 0;
        for step in 0..=10 {
            let event = event_at_fraction(&table, 4, step as f32 / 10.0);
            assert!(event >= previous, "доля {step} → {event} < {previous}");
            previous = event;
        }
    }

    #[test]
    fn the_density_is_normalized_to_the_maximum() {
        let bars = density_bars(&[0, 10, 20, 40]);
        assert_eq!(bars, vec![0.0, 0.25, 0.5, 1.0]);
        assert_eq!(density_bars(&[0, 0]), vec![0.0, 0.0]);
    }

    /// Подпись журнала прослушки: оба порта из заголовка, без сети (M5-AC6).
    #[test]
    fn the_meta_of_a_monitor_journal_names_both_ports() {
        use bytetap_core::journal::{Autoresponder, Host};
        let mut header = Session::new(
            Mode::ComMonitor,
            Autoresponder::None,
            serde_json::json!({
                "analyzer": {"port": "COM3", "baud": 9600, "format": "8N1"},
                "lis": {"port": "COM4", "baud": 9600, "format": "8N1"},
            }),
            Host {
                os: "Windows 11".into(),
            },
        );
        assert_eq!(
            journal_meta(&header, 6_120_000),
            "анализатор COM3 ⇄ bytetap ⇄ ЛИС COM4 · 00:00:06",
            "сводка та же, что в шапке записи"
        );
        // Строки ленты прослушки — пара портов: «#1 открыто: анализатор COM3 → ЛИС COM4»
        // и «#1 закрыто: ЛИС, порт пропал» (M5-В8).
        assert_eq!(pair_of_journal(&header), Pair::Monitor);
        header.analyzer = Some("ANALYZER".into());
        assert_eq!(
            journal_meta(&header, 6_120_000),
            "ANALYZER · анализатор COM3 ⇄ bytetap ⇄ ЛИС COM4 · 00:00:06"
        );
        // Заголовок без портов не выдумывает сводку: остаётся имя анализатора и время.
        header.settings = serde_json::json!({});
        assert_eq!(journal_meta(&header, 6_120_000), "ANALYZER · 00:00:06");
    }

    /// Подпись журнала прокси: имя анализатора из заголовка, сводка двух сторон и время
    /// записи (M4-AC13).
    #[test]
    fn the_meta_of_a_proxy_journal_names_the_analyzer_and_both_sides() {
        use bytetap_core::journal::{Autoresponder, Host};
        let mut header = Session::new(
            Mode::Proxy,
            Autoresponder::None,
            serde_json::json!({
                "listen": "0.0.0.0:50025",
                "target": "10.0.0.5:5000",
                "role": "analyzer_client",
            }),
            Host {
                os: "Windows 11".into(),
            },
        );
        assert_eq!(
            journal_meta(&header, 6_120_000),
            "bytetap :50025 ⇄ ЛИС 10.0.0.5:5000 · 00:00:06"
        );
        header.analyzer = Some("ANALYZER".into());
        assert_eq!(
            journal_meta(&header, 6_120_000),
            "ANALYZER · bytetap :50025 ⇄ ЛИС 10.0.0.5:5000 · 00:00:06"
        );
        // Топология «анализатор — сервер»: цель — анализатор, роль своя.
        header.settings = serde_json::json!({
            "listen": "0.0.0.0:5000",
            "target": "192.168.1.51:5000",
            "role": "analyzer_server",
        });
        assert_eq!(
            journal_meta(&header, 6_120_000),
            "ANALYZER · анализатор 192.168.1.51:5000 ⇄ bytetap :5000 · 00:00:06"
        );
    }

    #[test]
    fn the_duration_always_has_hours() {
        assert_eq!(duration(2_538_000_000), "00:42:18");
        assert_eq!(duration(3_661_000_000), "01:01:01");
        assert_eq!(duration(0), "00:00:00");
    }

    #[test]
    fn the_file_name_has_no_folder() {
        assert_eq!(
            file_name(Path::new(r"C:\Users\operator\Documents\bytetap\a.jsonl")),
            "a.jsonl"
        );
        assert_eq!(
            file_name(Path::new("/home/operator/bytetap/a.jsonl")),
            "a.jsonl"
        );
    }

    #[test]
    fn the_notes_go_from_the_header_to_the_newest() {
        let appended = vec!["четвёртый".to_owned(), "третий".to_owned()];
        assert_eq!(notes_line("старт", &appended), "старт · третий · четвёртый");
        assert_eq!(notes_line("", &appended), "третий · четвёртый");
        assert_eq!(notes_line("старт", &[]), "старт");
    }
}
