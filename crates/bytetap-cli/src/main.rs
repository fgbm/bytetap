//! Консольная версия bytetap.
//!
//! На M0 это заглушка: разбираются аргументы командной строки, логики захвата нет.

use clap::Parser;

/// Запись «сырого» обмена между лабораторным анализатором и ЛИС.
#[derive(Debug, Parser)]
#[command(name = "bytetap-cli", version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn version_matches_core() {
        let version = Cli::command().get_version().map(str::to_owned);
        assert_eq!(version.as_deref(), Some(bytetap_core::version()));
    }
}
