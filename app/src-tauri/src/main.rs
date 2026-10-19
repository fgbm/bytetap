// В release на Windows не показывать дополнительное окно консоли.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    bytetap_lib::run();
}
