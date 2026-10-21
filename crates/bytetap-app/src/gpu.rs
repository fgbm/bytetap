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

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;

use crate::diag;

/// Переменная окружения с частью имени адаптера, который выбрать.
pub const ADAPTER_ENV: &str = "WGPU_ADAPTER_NAME";

/// Часть имени WARP (`Microsoft Basic Render Driver`) для [`ADAPTER_ENV`].
pub const WARP_ADAPTER: &str = "basic render";

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
        diag::info(format!("выбран адаптер wgpu: {}", candidates[index].name));
        Ok(adapters[index].clone())
    }));
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Адаптер в списке выбора.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub device_type: wgpu::DeviceType,
    /// Адаптер умеет выводить в поверхность окна.
    pub compatible: bool,
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
