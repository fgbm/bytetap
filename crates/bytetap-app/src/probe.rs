//! Цена кадра на живой машине (U1-AC24): `BYTETAP_TEST_PROBE=<секунды>`.
//!
//! Замер в CI — это `cargo test` на egui_kittest (раскладка и тесселяция кадра) и
//! живой процесс на `windows-latest`: сколько кадров в секунду окно рисует и во что
//! ему обходится кадр. Здесь второй замер: проба просит перерисовку каждый кадр
//! `N` секунд и пишет в журнал запуска промежуток между кадрами, время самой
//! отрисовки (раскладка и тесселяция) и ожидание GPU. Итог — строка
//! `проба кадра: итог ...`, её и читает CI.
//!
//! Проба включает диагностические переключатели отрисовки: что рисовать
//! (`BYTETAP_TEST_PROBE_MODE`: `full`, `empty`, `no-icons`), режим представления
//! (`BYTETAP_TEST_PROBE_PRESENT`), число сэмплов MSAA (`BYTETAP_TEST_PROBE_MSAA`),
//! желаемую задержку кадров (`BYTETAP_TEST_PROBE_LATENCY`) и размер окна
//! (`BYTETAP_TEST_PROBE_SIZE=ШxВ`). Без `BYTETAP_TEST_PROBE` они не читаются.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::diag;

/// Сколько секунд мерить: `BYTETAP_TEST_PROBE=8`. Не задана — проба выключена.
pub const ENV: &str = "BYTETAP_TEST_PROBE";

/// Что рисовать во время пробы.
pub const MODE_ENV: &str = "BYTETAP_TEST_PROBE_MODE";

/// Режим представления: `fifo`, `immediate`, `novsync`, `mailbox`.
pub const PRESENT_ENV: &str = "BYTETAP_TEST_PROBE_PRESENT";

/// Число сэмплов MSAA.
pub const MSAA_ENV: &str = "BYTETAP_TEST_PROBE_MSAA";

/// Сколько кадров держит цепочка представления.
pub const LATENCY_ENV: &str = "BYTETAP_TEST_PROBE_LATENCY";

/// Размер окна для замера: `640x480`.
pub const SIZE_ENV: &str = "BYTETAP_TEST_PROBE_SIZE";

/// Весь интерфейс: так окно работает на самом деле.
pub const FULL: &str = "full";

/// Только фон: цена чистого кадра (очистка, растр, представление) без интерфейса.
pub const EMPTY: &str = "empty";

/// Интерфейс без иконок: столько стоит растр SVG-иконок в кадре.
pub const NO_ICONS: &str = "no-icons";

/// Сколько секунд по умолчанию мерить.
const DEFAULT_SECONDS: u64 = 8;

/// Проба включена: ключ окружения уже прочитан.
static ON: AtomicBool = AtomicBool::new(false);

/// Режим отрисовки пробы.
static MODE: OnceLock<String> = OnceLock::new();

/// Идущий замер.
static PROBE: Mutex<Option<Probe>> = Mutex::new(None);

/// Прочитанный ключ окружения и запись режима в журнал запуска — до создания окна.
pub fn start() {
    MODE.set(std::env::var(MODE_ENV).unwrap_or_else(|_| FULL.into()))
        .ok();
    let seconds = seconds();
    ON.store(true, Ordering::Relaxed);
    *PROBE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Probe::new(seconds));
    diag::info(format!(
        "проба кадра: {seconds} с, режим {}{}",
        mode(),
        if crate::gpu::diagnostics() {
            format!(
                ", диагностика: представление {}, MSAA {}, задержка {}",
                crate::gpu::present_name(),
                crate::gpu::msaa(),
                crate::gpu::latency().map_or_else(|| "по умолчанию".into(), |n| n.to_string())
            )
        } else {
            String::new()
        }
    ));
}

/// Проба идёт: значение `BYTETAP_TEST_PROBE`.
#[must_use]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Режим отрисовки: [`FULL`], [`EMPTY`] или [`NO_ICONS`].
#[must_use]
pub fn mode() -> &'static str {
    MODE.get().map_or(FULL, String::as_str)
}

/// Сколько секунд мерить: из `BYTETAP_TEST_PROBE`, по умолчанию [`DEFAULT_SECONDS`].
#[must_use]
pub fn seconds() -> u64 {
    std::env::var(ENV)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_SECONDS)
}

/// Размер окна для замера из [`SIZE_ENV`]: `640x480`.
#[must_use]
pub fn size() -> Option<[f32; 2]> {
    parse_size(&std::env::var(SIZE_ENV).ok()?)
}

/// Размер окна из строки вида `640x480` (латинская или русская «х»).
fn parse_size(value: &str) -> Option<[f32; 2]> {
    let (width, height) = value.trim().split_once(['x', 'х'])?;
    Some([width.trim().parse().ok()?, height.trim().parse().ok()?])
}

/// Начало кадра пробы: `None`, когда проба выключена.
#[must_use]
pub fn frame_start() -> Option<Instant> {
    enabled().then(Instant::now)
}

/// Конец кадра: запись замера, просьба о следующем кадре и, когда время вышло, итог
/// в журнал и выход процесса.
pub fn frame_end(ctx: &egui::Context, frame: &mut eframe::Frame, started: Option<Instant>) {
    let Some(started) = started else {
        return;
    };
    {
        let mut guard = PROBE.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(probe) = guard.as_mut() else {
            return;
        };
        if let Some(finished) = probe.record(ctx, frame, started) {
            diag::info(finished);
            drop(guard);
            std::process::exit(0);
        }
    }
    // Кадр за кадром без паузы: так проба видит наибольшую частоту кадров.
    ctx.request_repaint();
}

/// Замер одного кадра в секундах.
struct Probe {
    duration: Duration,
    started: Instant,
    last_frame: Option<Instant>,
    spacings: Vec<Duration>,
    draws: Vec<Duration>,
    waits: Vec<Duration>,
    report: Instant,
    report_frames: usize,
    textures: usize,
    texture_bytes: usize,
    header: bool,
}

impl Probe {
    fn new(seconds: u64) -> Self {
        let now = Instant::now();
        Self {
            duration: Duration::from_secs(seconds),
            started: now,
            last_frame: None,
            spacings: Vec::new(),
            draws: Vec::new(),
            waits: Vec::new(),
            report: now,
            report_frames: 0,
            textures: 0,
            texture_bytes: 0,
            header: false,
        }
    }

    /// Записать кадр. Итоговая строка — когда время пробы вышло.
    fn record(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
        started: Instant,
    ) -> Option<String> {
        let ended = Instant::now();
        // Кадр предыдущей пробы уже отправлен в очередь: ожидание показывает, сколько
        // работы растра осталось незавершённой, когда окно начало новый кадр.
        let waited = frame.wgpu_render_state().map_or(Duration::ZERO, |state| {
            let start = Instant::now();
            let _ = state
                .device
                .poll(eframe::wgpu::PollType::wait_indefinitely());
            start.elapsed()
        });
        if let Some(last) = self.last_frame {
            self.spacings.push(started.saturating_duration_since(last));
        }
        self.last_frame = Some(started);
        self.draws.push(ended.saturating_duration_since(started));
        self.waits.push(waited);
        self.report_frames += 1;

        if !self.header
            && let Some(state) = frame.wgpu_render_state()
        {
            self.header = true;
            let info = state.adapter.get_info();
            let ppp = ctx.pixels_per_point();
            let screen = ctx.input(|i| {
                i.viewport()
                    .inner_rect
                    .unwrap_or_else(|| ctx.content_rect())
            });
            diag::info(format!(
                "проба кадра: адаптер {} ({:?}), {:?}, формат {:?}, точек на пиксель {ppp:.2}, \
                 поверхность {:.0}x{:.0} точек ({:.0}x{:.0} пикселей), текстур {} ({:.1} МБ)",
                info.name,
                info.device_type,
                info.backend,
                state.target_format,
                screen.width(),
                screen.height(),
                screen.width() * ppp,
                screen.height() * ppp,
                self.textures,
                self.texture_bytes as f64 / (1024.0 * 1024.0)
            ));
        }

        // Раз в секунду — строка замера: по ней видно, как кадр ведёт себя во времени,
        // и растут ли текстуры (атлас шрифтов, иконки) от кадра к кадру.
        if self.report.elapsed() >= Duration::from_secs(1) {
            self.report = Instant::now();
            let frames = std::mem::take(&mut self.report_frames);
            let report = self.report_line(ctx, frames);
            diag::info(report);
        }

        if self.started.elapsed() >= self.duration {
            return Some(self.summary(ctx));
        }
        None
    }

    /// Строка секунды замера: кадры, промежуток и отрисовка за неё, число текстур.
    fn report_line(&mut self, ctx: &egui::Context, frames: usize) -> String {
        let (textures, bytes) = textures(ctx);
        self.textures = textures;
        self.texture_bytes = bytes;
        format!(
            "проба кадра: {:.1} с, кадров {frames}, промежуток {}, отрисовка {}",
            self.started.elapsed().as_secs_f32(),
            durations(&self.spacings, self.spacings.len().saturating_sub(frames)),
            durations(&self.draws, self.draws.len().saturating_sub(frames)),
        )
    }

    /// Итог пробы: медиана и размах промежутка, отрисовки и ожидания GPU.
    fn summary(&self, ctx: &egui::Context) -> String {
        let spacing = median(&self.spacings);
        // Цена кадра — промежуток, только когда он больше отрисовки: остальное —
        // растр, отправка и представление кадра.
        let paint = spacing.saturating_sub(median(&self.draws));
        format!(
            "проба кадра: итог {}, кадров {} за {:.1} с, промежуток {:.1} мс (мин {:.1}, макс {:.1}), \
             отрисовка {:.2} мс, цена кадра {:.1} мс, ожидание GPU {:.2} мс, текстур {}",
            mode(),
            self.spacings.len(),
            self.started.elapsed().as_secs_f32(),
            as_ms(spacing),
            as_ms(min(&self.spacings)),
            as_ms(max(&self.spacings)),
            as_ms(median(&self.draws)),
            as_ms(paint),
            as_ms(median(&self.waits)),
            textures(ctx).0,
        )
    }
}

/// Текстуры, загруженные в egui: сколько и на сколько байт.
fn textures(ctx: &egui::Context) -> (usize, usize) {
    let manager = ctx.tex_manager();
    let manager = manager.read();
    let bytes = manager
        .allocated()
        .map(|(_, meta)| meta.size[0] * meta.size[1] * meta.bytes_per_pixel)
        .sum();
    (manager.num_allocated(), bytes)
}

fn median(values: &[Duration]) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn min(values: &[Duration]) -> Duration {
    values.iter().copied().min().unwrap_or(Duration::ZERO)
}

fn max(values: &[Duration]) -> Duration {
    values.iter().copied().max().unwrap_or(Duration::ZERO)
}

/// Последние `take` замеров строкой «медиана (мин…макс)».
fn durations(values: &[Duration], from: usize) -> String {
    let tail = &values[from.min(values.len())..];
    format!(
        "{:.1} мс ({:.1}…{:.1})",
        as_ms(median(tail)),
        as_ms(min(tail)),
        as_ms(max(tail))
    )
}

fn as_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Медиана промежутка между кадрами — середина, а не среднее: один медленный
    /// кадр (загрузка шрифтов) не должен выдавать себя за цену кадра.
    #[test]
    fn the_median_of_frame_times_is_the_middle_one() {
        let values = [
            Duration::from_millis(30),
            Duration::from_millis(10),
            Duration::from_millis(20),
        ];
        assert_eq!(median(&values), Duration::from_millis(20));
        assert_eq!(median(&[]), Duration::ZERO);
    }

    /// Размер окна пробы: `640x480`, в том числе с русской «х» из привычной записи.
    #[test]
    fn the_probe_window_size_is_parsed_from_the_key() {
        assert_eq!(parse_size("640x480"), Some([640.0, 480.0]));
        assert_eq!(parse_size(" 800х600 "), Some([800.0, 600.0]));
        assert_eq!(parse_size("800"), None);
        assert_eq!(parse_size("широкоxвысоко"), None);
    }
}
