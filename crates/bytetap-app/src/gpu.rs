//! Выбор графики wgpu (NFR-3, U1-AC2): DX12 на Windows, без видеодрайвера — WARP.
//!
//! WARP (`Microsoft Basic Render Driver`) — программный адаптер DX12, который есть в
//! каждой Windows 10/11: на виртуальной машине без GPU окно рисуется им. Аппаратный
//! адаптер предпочтительнее, `WGPU_ADAPTER_NAME` выбирает адаптер по части имени
//! (например, `WGPU_ADAPTER_NAME=basic render` включает WARP принудительно), `WGPU_BACKEND`
//! переопределяет бэкенд. Список адаптеров и выбор пишутся в журнал запуска. Если wgpu
//! на выбранном адаптере не поднялся, `main` один раз перезапускает процесс с WARP
//! ([`retry_with_warp`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;

use crate::diag;

/// Переменная окружения с частью имени адаптера, который выбрать.
pub const ADAPTER_ENV: &str = "WGPU_ADAPTER_NAME";

/// Часть имени WARP (`Microsoft Basic Render Driver`) для [`ADAPTER_ENV`].
pub const WARP_ADAPTER: &str = "basic render";

/// Диагностика цены кадра (`probe.rs`): режим представления, MSAA и задержка кадров.
///
/// Ключи читаются только вместе с `BYTETAP_TEST_PROBE` и `BYTETAP_TEST_HOOKS=1`
/// (`hooks.rs`): без пробы окно берёт [`egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT`] и
/// MSAA 0, как и раньше.
pub const PRESENT_ENV: &str = "BYTETAP_TEST_PROBE_PRESENT";
pub const MSAA_ENV: &str = "BYTETAP_TEST_PROBE_MSAA";
pub const LATENCY_ENV: &str = "BYTETAP_TEST_PROBE_LATENCY";

/// Диагностика включена: задан хотя бы один ключ замера.
#[must_use]
pub fn diagnostics() -> bool {
    [PRESENT_ENV, MSAA_ENV, LATENCY_ENV]
        .iter()
        .any(|key| crate::hooks::var(key).is_some())
}

/// Режим представления из [`PRESENT_ENV`]: `fifo`, `immediate`, `novsync`, `mailbox`.
#[must_use]
pub fn present() -> Option<wgpu::PresentMode> {
    let value = crate::hooks::var(PRESENT_ENV)?;
    Some(match value.trim().to_lowercase().as_str() {
        "fifo" | "vsync" => wgpu::PresentMode::Fifo,
        "immediate" => wgpu::PresentMode::Immediate,
        "novsync" | "auto-no-vsync" => wgpu::PresentMode::AutoNoVsync,
        "mailbox" => wgpu::PresentMode::Mailbox,
        _ => return None,
    })
}

/// Имя режима представления для журнала запуска.
#[must_use]
pub fn present_name() -> String {
    match present() {
        Some(mode) => format!("{mode:?}"),
        None => "HIGH_THROUGHPUT".into(),
    }
}

/// Число сэмплов MSAA из [`MSAA_ENV`].
#[must_use]
pub fn msaa() -> u16 {
    crate::hooks::var(MSAA_ENV)
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Желаемая задержка кадров из [`LATENCY_ENV`].
#[must_use]
pub fn latency() -> Option<u32> {
    crate::hooks::var(LATENCY_ENV).and_then(|value| value.trim().parse().ok())
}

/// Метка перезапуска с WARP: второй сбой графики уже не перезапускает процесс.
pub const RETRY_ENV: &str = "BYTETAP_WARP_RETRY";

/// Перезапустить ли процесс с WARP после ошибки запуска графики: только если wgpu не
/// поднялся (`wgpu_failed`, на Windows), адаптер не выбран явно (`adapter_set`) и это
/// ещё не повтор (`retried`). Аппаратный адаптер может «рисовать в окно» по ответу
/// драйвера и всё же падать при создании устройства — тогда помогает WARP.
#[must_use]
pub fn retry_with_warp(wgpu_failed: bool, adapter_set: bool, retried: bool) -> bool {
    wgpu_failed && !adapter_set && !retried
}

/// Выбранный адаптер рисует кадр на процессоре (`Microsoft Basic Render Driver`).
///
/// Такой кадр стоит сотни миллисекунд на 1200×800, поэтому окно во время записи просит
/// его реже (`live::next_period`). Флаг ставится при выборе адаптера, до первого кадра.
static SOFTWARE: AtomicBool = AtomicBool::new(false);

/// Кадры рисует программный растр: WARP или адаптер без GPU.
#[must_use]
pub fn software() -> bool {
    SOFTWARE.load(Ordering::Relaxed)
}

/// Бэкенды wgpu: DX12 на Windows, на остальных ОС — как у egui по умолчанию.
fn backends() -> wgpu::Backends {
    let default = if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::PRIMARY | wgpu::Backends::GL
    };
    wgpu::Backends::from_env().unwrap_or(default)
}

/// Настройка wgpu для `NativeOptions::wgpu_options`.
pub fn configuration() -> WgpuConfiguration {
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = backends();
    let wanted = std::env::var(ADAPTER_ENV).ok();
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        let candidates: Vec<Candidate> = adapters
            .iter()
            .map(|adapter| {
                let info = adapter.get_info();
                Candidate {
                    name: info.name,
                    device_type: info.device_type,
                    compatible: surface.is_none_or(|s| adapter.is_surface_supported(s)),
                }
            })
            .collect();
        for candidate in &candidates {
            diag::info(format!(
                "адаптер wgpu: {} ({:?}){}",
                candidate.name,
                candidate.device_type,
                if candidate.compatible {
                    ""
                } else {
                    ", не рисует в окно"
                }
            ));
        }
        // Текст ошибки попадает в системное окно «Не удалось запустить bytetap».
        let index = select(&candidates, wanted.as_deref()).ok_or_else(|| {
            crate::t!(
                "diag.noAdapter",
                backends = format!("{:?}", backends()),
                count = candidates.len()
            )
        })?;
        let chosen = &candidates[index];
        SOFTWARE.store(chosen.software(), Ordering::Relaxed);
        diag::info(format!("выбран адаптер wgpu: {}", chosen.name));
        Ok(adapters[index].clone())
    }));
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        surface: surface_config(),
        ..Default::default()
    }
}

/// Настройка поверхности: как у `WgpuConfiguration` по умолчанию
/// ([`egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT`]), а в диагностике пробы кадра —
/// режим представления и задержка из ключей замера.
fn surface_config() -> eframe::egui_wgpu::SurfaceConfig {
    let mut config = eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT;
    if let Some(mode) = present() {
        config.present_mode = mode;
    }
    if let Some(latency) = latency() {
        config.desired_maximum_frame_latency = Some(latency);
    }
    config
}

/// Адаптер в списке выбора.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub device_type: wgpu::DeviceType,
    /// Адаптер умеет выводить в поверхность окна.
    pub compatible: bool,
}

impl Candidate {
    /// Программный растр: WARP или адаптер без GPU — кадр рисует процессор.
    #[must_use]
    pub fn software(&self) -> bool {
        self.device_type == wgpu::DeviceType::Cpu || self.name.to_lowercase().contains(WARP_ADAPTER)
    }
}

/// Номер адаптера: из тех, что рисуют в окно, — по части имени из `wanted`, иначе
/// дискретный, встроенный, виртуальный, прочий и только потом программный (WARP).
pub fn select(candidates: &[Candidate], wanted: Option<&str>) -> Option<usize> {
    let usable = || {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.compatible)
    };
    if let Some(wanted) = wanted.map(str::to_lowercase).filter(|w| !w.is_empty())
        && let Some((index, _)) =
            usable().find(|(_, candidate)| candidate.name.to_lowercase().contains(&wanted))
    {
        return Some(index);
    }
    let rank = |device_type: wgpu::DeviceType| match device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    usable()
        .min_by_key(|(index, candidate)| (rank(candidate.device_type), *index))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::DeviceType;

    fn candidate(name: &str, device_type: DeviceType, compatible: bool) -> Candidate {
        Candidate {
            name: name.into(),
            device_type,
            compatible,
        }
    }

    fn machine() -> Vec<Candidate> {
        vec![
            candidate("Microsoft Basic Render Driver", DeviceType::Cpu, true),
            candidate("Intel(R) UHD Graphics", DeviceType::IntegratedGpu, true),
            candidate("NVIDIA GeForce", DeviceType::DiscreteGpu, false),
        ]
    }

    #[test]
    fn a_hardware_adapter_that_draws_to_the_window_wins() {
        assert_eq!(select(&machine(), None), Some(1));
    }

    #[test]
    fn warp_is_used_without_a_hardware_adapter() {
        let vm = vec![candidate(
            "Microsoft Basic Render Driver",
            DeviceType::Cpu,
            true,
        )];
        assert_eq!(select(&vm, None), Some(0));
    }

    #[test]
    fn the_adapter_name_from_the_environment_forces_the_choice() {
        assert_eq!(select(&machine(), Some("basic render")), Some(0));
        // Адаптер, который не рисует в окно, не выбирается и по имени.
        assert_eq!(select(&machine(), Some("nvidia")), Some(1));
        assert_eq!(select(&machine(), Some("")), Some(1));
    }

    #[test]
    fn the_warp_selector_picks_warp() {
        assert_eq!(select(&machine(), Some(WARP_ADAPTER)), Some(0));
    }

    /// Программный растр узнаётся и по типу устройства, и по имени WARP.
    #[test]
    fn a_software_adapter_is_not_a_gpu() {
        let warp = candidate("Microsoft Basic Render Driver", DeviceType::Cpu, true);
        assert!(warp.software());
        // WARP назван по имени, но тип устройства у него не `Cpu`.
        let named = candidate("Microsoft Basic Render Driver", DeviceType::Other, true);
        assert!(named.software());
        assert!(!machine()[1].software(), "встроенная графика — не WARP");
        assert!(!machine()[2].software());
    }

    #[test]
    fn a_graphics_failure_is_retried_with_warp_once() {
        assert!(retry_with_warp(true, false, false));
        // Повтор уже был — второй раз не перезапускается.
        assert!(!retry_with_warp(true, false, true));
        // Адаптер выбран явно — выбор пользователя не меняется.
        assert!(!retry_with_warp(true, true, false));
        // Ошибка не графики — перезапуск не поможет.
        assert!(!retry_with_warp(false, false, false));
    }

    #[test]
    fn no_usable_adapter_is_none() {
        assert_eq!(select(&[], None), None);
        let broken = vec![candidate("GPU", DeviceType::DiscreteGpu, false)];
        assert_eq!(select(&broken, None), None);
    }
}
