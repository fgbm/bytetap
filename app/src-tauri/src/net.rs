//! Сеть ноутбука для подсказки анализатору (M2-AC2): IPv4-адреса без loopback.
//!
//! Только локальный перебор адресов: запросов в сеть нет (FR-33, NFR-3). Интерфейс
//! показывает адреса с именами, чтобы инженер выбрал нужный («Ethernet», «Wi-Fi»)
//! и вписал его в настройки анализатора вместе с портом.

use crate::api::Interface;

/// IPv4-адреса этой машины без loopback, с именем интерфейса.
///
/// IPv6 не показывается: анализаторы в режиме «вместо ЛИС» работают по IPv4, а список
/// из десятков временных адресов сбил бы с толку. Провал перебора — пустой список:
/// интерфейс работает и без подсказки.
#[must_use]
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
    fn the_list_is_sorted_and_without_duplicates() {
        let mut interfaces = list_interfaces();
        let sorted = {
            let mut copy = interfaces.clone();
            copy.sort_by(|left, right| {
                (left.name.as_str(), left.addr.as_str())
                    .cmp(&(right.name.as_str(), right.addr.as_str()))
            });
            copy
        };
        assert_eq!(interfaces, sorted);
        interfaces.dedup();
        assert_eq!(interfaces, list_interfaces());
    }
}
