//! Экранирование байтов в читаемый текст (FR-22).
//!
//! Поле `text` события `data` — копия фрагмента «для чтения глазами», и оно однозначно
//! обратимо в исходные байты: `b64` остаётся единственным источником правды, а `text`
//! читается человеком и при этом не теряет информации.
//!
//! Правила (решение владельца, зафиксировано в `docs/reference/session-format.md`):
//!
//! - печатные ASCII `0x20`–`0x7E` пишутся как есть, кроме `<`: он пишется как `<x3C>`,
//!   чтобы любой `<` в тексте однозначно начинал экранированную последовательность;
//! - управляющие символы из FR-22 — именованные: `<STX>` `0x02`, `<ETX>` `0x03`, `<EOT>` `0x04`,
//!   `<ENQ>` `0x05`, `<ACK>` `0x06`, `<LF>` `0x0A`, `<VT>` `0x0B`, `<CR>` `0x0D`, `<NAK>` `0x15`,
//!   `<ETB>` `0x17`, `<FS>` `0x1C`;
//! - все остальные байты (`0x00`–`0x1F`, `0x7F`, `0x80`–`0xFF`) — как `<xHH>` с шестнадцатеричными
//!   цифрами в верхнем регистре.

use std::fmt::{self, Write as _};

/// Именованные управляющие символы FR-22, в порядке кодов.
const NAMED: &[(u8, &str)] = &[
    (0x02, "STX"),
    (0x03, "ETX"),
    (0x04, "EOT"),
    (0x05, "ENQ"),
    (0x06, "ACK"),
    (0x0A, "LF"),
    (0x0B, "VT"),
    (0x0D, "CR"),
    (0x15, "NAK"),
    (0x17, "ETB"),
    (0x1C, "FS"),
];

/// Имя именованной управляющей последовательности для байта, если она есть.
fn named(byte: u8) -> Option<&'static str> {
    NAMED
        .iter()
        .find(|(code, _)| *code == byte)
        .map(|(_, name)| *name)
}

/// Байт по имени именованной управляющей последовательности.
fn named_byte(name: &str) -> Option<u8> {
    NAMED
        .iter()
        .find(|(_, known)| *known == name)
        .map(|(code, _)| *code)
}

/// Экранирует байты фрагмента в текст по правилам FR-22.
///
/// Результат — ASCII и обратим [`unescape`]-ом.
#[must_use]
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        if (0x20..=0x7E).contains(&byte) && byte != b'<' {
            out.push(char::from(byte));
        } else if let Some(name) = named(byte) {
            out.push('<');
            out.push_str(name);
            out.push('>');
        } else {
            // Запись в String не может отказать.
            let _ = write!(out, "<x{byte:02X}>");
        }
    }
    out
}

/// Восстанавливает исходные байты из текста, полученного [`escape`].
///
/// На входе допускается только то, что производит [`escape`]; всё прочее — ошибка с индексом
/// байта во входной строке.
pub fn unescape(text: &str) -> Result<Vec<u8>, UnescapeError> {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text;

    while let Some(ch) = rest.chars().next() {
        let index = text.len() - rest.len();
        if ch == '<' {
            let mut chars = rest.char_indices();
            chars.next();
            let end = chars
                .find(|(_, c)| *c == '>')
                .map(|(offset, _)| offset)
                .ok_or(UnescapeError {
                    index,
                    kind: UnescapeErrorKind::UnterminatedTag,
                })?;
            let tag = &rest[1..end];
            out.push(decode_tag(tag).ok_or_else(|| UnescapeError {
                index,
                kind: UnescapeErrorKind::UnknownTag(tag.to_owned()),
            })?);
            rest = &rest[end + 1..];
        } else if ch == ' ' || ch.is_ascii_graphic() {
            // Печатные ASCII, кроме `<`, `escape` пишет как есть.
            out.push(u8::try_from(ch).unwrap_or(b'?'));
            rest = &rest[ch.len_utf8()..];
        } else {
            return Err(UnescapeError {
                index,
                kind: UnescapeErrorKind::UnexpectedChar(ch),
            });
        }
    }

    Ok(out)
}

/// Разбирает содержимое тега между `<` и `>`.
///
/// Допускается только то, что пишет [`escape`]: именованные управляющие — только именем,
/// `<xHH>` — только в верхнем регистре и только для байтов, которые [`escape`] пишет как
/// `<xHH>`. Запись вроде `<x0A>` (есть имя `<LF>`), `<x20>` (печатный байт) или `<x3c>`
/// (нижний регистр) отвергается: у одного байта должна быть одна запись.
fn decode_tag(tag: &str) -> Option<u8> {
    if let Some(byte) = named_byte(tag) {
        return Some(byte);
    }
    let hex = tag.strip_prefix('x')?;
    if hex.len() != 2
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
    {
        return None;
    }
    let byte = u8::from_str_radix(hex, 16).ok()?;
    let hex_written = named(byte).is_none() && !((0x20..=0x7E).contains(&byte) && byte != b'<');
    hex_written.then_some(byte)
}

/// Ошибка [`unescape`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnescapeError {
    /// Индекс байта во входной строке, где начинается ошибка.
    pub index: usize,
    /// Что именно не разобралось.
    pub kind: UnescapeErrorKind,
}

/// Вид ошибки [`unescape`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnescapeErrorKind {
    /// `<` без закрывающего `>`.
    UnterminatedTag,
    /// Неизвестное имя тега или неверная запись `<xHH>`.
    UnknownTag(String),
    /// Символ, который `escape` не производит: управляющий или не-ASCII.
    UnexpectedChar(char),
}

impl fmt::Display for UnescapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            UnescapeErrorKind::UnterminatedTag => {
                write!(f, "позиция {}: `<` без закрывающего `>`", self.index)
            }
            UnescapeErrorKind::UnknownTag(tag) => {
                write!(
                    f,
                    "позиция {}: неизвестная последовательность <{tag}>",
                    self.index
                )
            }
            UnescapeErrorKind::UnexpectedChar(ch) => {
                write!(
                    f,
                    "позиция {}: символ {ch:?} не может быть в экранированном тексте",
                    self.index
                )
            }
        }
    }
}

impl std::error::Error for UnescapeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_controls_match_fr22() {
        let cases = [
            (0x02, "<STX>"),
            (0x03, "<ETX>"),
            (0x04, "<EOT>"),
            (0x05, "<ENQ>"),
            (0x06, "<ACK>"),
            (0x0A, "<LF>"),
            (0x0B, "<VT>"),
            (0x0D, "<CR>"),
            (0x15, "<NAK>"),
            (0x17, "<ETB>"),
            (0x1C, "<FS>"),
        ];
        for (byte, expected) in cases {
            assert_eq!(escape(&[byte]), expected);
        }
    }

    #[test]
    fn mllp_frame_reads_like_a_frame() {
        let mut bytes = vec![0x0B];
        bytes.extend_from_slice(b"MSH|^~\\&");
        bytes.extend_from_slice(&[0x0D, 0x1C, 0x0D]);
        assert_eq!(escape(&bytes), "<VT>MSH|^~\\&<CR><FS><CR>");
    }

    #[test]
    fn all_bytes_have_expected_form() {
        for byte in 0x00..=0xFFu8 {
            let text = escape(&[byte]);
            if let Some(name) = named(byte) {
                assert_eq!(text, format!("<{name}>"), "байт {byte:#04X}");
            } else if (0x20..=0x7E).contains(&byte) && byte != b'<' {
                assert_eq!(text, char::from(byte).to_string(), "байт {byte:#04X}");
            } else {
                assert_eq!(text, format!("<x{byte:02X}>"), "байт {byte:#04X}");
            }
            assert_eq!(unescape(&text), Ok(vec![byte]), "байт {byte:#04X}");
        }
    }

    #[test]
    fn less_than_is_escaped_so_text_is_unambiguous() {
        assert_eq!(escape(b"a<b"), "a<x3C>b");
        assert_eq!(escape(b"<<STX>>"), "<x3C><x3C>STX>>");
        assert_eq!(unescape("<x3C><x3C>STX>>"), Ok(b"<<STX>>".to_vec()));
        assert_eq!(unescape("a<x3C>b"), Ok(b"a<b".to_vec()));
    }

    #[test]
    fn non_ascii_bytes_are_hex() {
        assert_eq!(escape(&[0xC0, 0xE0, 0xFF]), "<xC0><xE0><xFF>");
        assert_eq!(unescape("<xC0><xE0><xFF>"), Ok(vec![0xC0, 0xE0, 0xFF]));
    }

    #[test]
    fn only_the_canonical_form_is_accepted() {
        assert_eq!(escape(&[0xC0]), "<xC0>");
        // Нижний регистр, запись именованного байта через `<xHH>`, печатный байт.
        for text in ["<xc0>", "<x0A>", "<x20>", "<X3C>", "<x3c>"] {
            assert!(
                unescape(text).is_err(),
                "{text} не производится escape и должен отвергаться"
            );
        }
        assert_eq!(unescape("<x3C>"), Ok(vec![b'<']));
        assert_eq!(unescape("<LF>"), Ok(vec![0x0A]));
    }

    #[test]
    fn unescape_rejects_what_escape_never_produces() {
        assert_eq!(
            unescape("<x3>"),
            Err(UnescapeError {
                index: 0,
                kind: UnescapeErrorKind::UnknownTag("x3".to_owned()),
            })
        );
        assert_eq!(
            unescape("abc<STX"),
            Err(UnescapeError {
                index: 3,
                kind: UnescapeErrorKind::UnterminatedTag,
            })
        );
        assert_eq!(
            unescape("a\nb"),
            Err(UnescapeError {
                index: 1,
                kind: UnescapeErrorKind::UnexpectedChar('\n'),
            })
        );
        assert_eq!(
            unescape("привет"),
            Err(UnescapeError {
                index: 0,
                kind: UnescapeErrorKind::UnexpectedChar('п'),
            })
        );
    }

    #[test]
    fn empty_input_roundtrips() {
        assert_eq!(escape(b""), "");
        assert_eq!(unescape(""), Ok(Vec::new()));
    }

    #[test]
    fn escape_is_pure_ascii() {
        let bytes: Vec<u8> = (0x00..=0xFFu8).collect();
        assert!(escape(&bytes).is_ascii());
    }

    proptest::proptest! {
        #[test]
        fn escape_unescape_roundtrip(bytes in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..4096)) {
            let text = escape(&bytes);
            proptest::prop_assert!(text.is_ascii());
            let restored = unescape(&text);
            proptest::prop_assert_eq!(restored.as_deref(), Ok(bytes.as_slice()));
        }
    }
}
