//! «Авто»: профиль автоответчика по первому значимому байту соединения (FR-15).
//!
//! Обёртка над готовыми автоматами. Решение принимается один раз — по первому значимому
//! байту — и дальше не пересматривается: если на середине обмена пришёл байт другого
//! протокола, он достанется выбранному автомату как есть.
//!
//! - `0x0B` (`VT`) — начало рамки MLLP, профиль [`Hl7Mllp`];
//! - `0x05` (`ENQ`) — ASTM E1381, профиль [`Astm`];
//! - любой другой байт — [`NoResponder`] и [`ResponderEvent::Info`] с кодом
//!   [`EventCode::ProtocolUndetected`]: запись идёт, ответов для этого соединения нет.
//!
//! Незначимые ведущие байты — `0x00` (`NUL`), `0x04` (`EOT`), `0x0D` (`CR`), `0x0A` (`LF`),
//! `0x11` (`XON`) и `0x13` (`XOFF`) — решение не принимают: приборы и переходники шлют их
//! перед обменом. В журнал они попадают как обычные `data` (их пишет сессия), а автомату
//! не передаются: выбранному профилю они не нужны.
//!
//! Значимый байт не теряется: выбранный автомат получает фрагмент с него. Поэтому разбиение
//! потока на фрагменты не меняет ни ответов, ни событий — кроме одного случая: если `feed`
//! приходит пустым или состоит из одних незначимых байтов, решение откладывается.
//!
//! Для COM-порта «соединение» — это открытие порта: там решение принимается на открытие,
//! а [`Responder::reset`] забывает его к следующему.

use std::time::Duration;

use crate::responder::astm::{self, Astm};
use crate::responder::mllp::{self, Hl7Mllp};
use crate::responder::{
    Action, ControlIdSource, EventCode, NoResponder, Now, Responder, ResponderEvent,
};

/// Ведущие байты, которые решение не принимают: `NUL`, `EOT`, `CR`, `LF`, `XON`, `XOFF`.
const NOISE: [u8; 6] = [0x00, astm::EOT, 0x0D, 0x0A, 0x11, 0x13];

/// Ведущий байт не участвует в решении о протоколе.
fn is_noise(byte: u8) -> bool {
    NOISE.contains(&byte)
}

/// Профиль «Авто» для одного соединения.
pub struct Auto {
    chosen: Option<Box<dyn Responder + Send>>,
    ids: ControlIdSource,
    nak_on_bad_checksum: bool,
}

impl Auto {
    /// Обёртка с общим на сессию источником `MSH-10`: номера ACK не повторяются между
    /// соединениями, даже если протокол определился как HL7/MLLP.
    #[must_use]
    pub fn new(ids: ControlIdSource) -> Self {
        Self {
            chosen: None,
            ids,
            nak_on_bad_checksum: false,
        }
    }

    /// Передавать найденному ASTM-автомату настройку «`NAK` на неверную сумму» (FR-17).
    ///
    /// Без неё профиль «Авто» подтверждает кадр с неверной суммой, как ASTM по умолчанию.
    #[must_use]
    pub const fn with_nak_on_bad_checksum(mut self, nak_on_bad_checksum: bool) -> Self {
        self.nak_on_bad_checksum = nak_on_bad_checksum;
        self
    }

    /// Протокол уже определён: дальнейшие фрагменты идут выбранному автомату.
    #[must_use]
    pub const fn is_detected(&self) -> bool {
        self.chosen.is_some()
    }

    /// Выбирает автомат по первому значимому байту соединения.
    fn detect(&mut self, first: u8, actions: &mut Vec<Action>) {
        self.chosen = Some(match first {
            mllp::SB => Box::new(Hl7Mllp::new(self.ids.clone())),
            astm::ENQ => Box::new(Astm::new().with_nak_on_bad_checksum(self.nak_on_bad_checksum)),
            other => {
                // Ответов нет, но запись идёт: предупреждение уходит в журнал кодом, а
                // русскую строку интерфейс берёт по коду (FR-15).
                actions.push(
                    ResponderEvent::Info {
                        message: format!(
                            "protocol is not detected: the first significant byte 0x{other:02X} is neither 0x0B (HL7/MLLP) nor 0x05 (ASTM E1381); no answers are sent for this connection"
                        ),
                        code: Some(EventCode::ProtocolUndetected),
                    }
                    .into(),
                );
                Box::new(NoResponder)
            }
        });
    }
}

impl Default for Auto {
    fn default() -> Self {
        Self::new(ControlIdSource::new())
    }
}

impl std::fmt::Debug for Auto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auto")
            .field("detected", &self.is_detected())
            .field("nak_on_bad_checksum", &self.nak_on_bad_checksum)
            .finish()
    }
}

impl Responder for Auto {
    fn feed(&mut self, data: &[u8], now: Now) -> Vec<Action> {
        if let Some(chosen) = self.chosen.as_mut() {
            return chosen.feed(data, now);
        }
        // До решения ведущие незначимые байты не копятся: они уже в журнале как `data`.
        let Some(start) = data.iter().position(|&byte| !is_noise(byte)) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        self.detect(data[start], &mut actions);
        if let Some(chosen) = self.chosen.as_mut() {
            actions.extend(chosen.feed(&data[start..], now));
        }
        actions
    }

    fn finish(&mut self, now: Now) -> Vec<Action> {
        self.chosen
            .as_mut()
            .map_or_else(Vec::new, |chosen| chosen.finish(now))
    }

    fn reset(&mut self) {
        // Новое соединение (или новое открытие порта) определяется заново.
        self.chosen = None;
    }

    fn poll_timeout(&mut self, now: Now) -> Option<Duration> {
        self.chosen
            .as_mut()
            .and_then(|chosen| chosen.poll_timeout(now))
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    const MESSAGE: &[u8] =
        b"MSH|^~\\&|ANALYZER|LAB|LIS|LAB|20261019101500||ORU^R01|MSG0001|P|2.3.1\r\
          PID|1||PATIENT^TEST\r";

    fn now() -> Now {
        DateTime::parse_from_rfc3339("2026-10-19T10:15:30+03:00").unwrap()
    }

    fn framed(message: &[u8]) -> Vec<u8> {
        let mut stream = vec![mllp::SB];
        stream.extend_from_slice(message);
        stream.extend_from_slice(&[mllp::EB, mllp::CR]);
        stream
    }

    fn sends(actions: &[Action]) -> Vec<Vec<u8>> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(bytes) => Some(bytes.clone()),
                Action::Event(_) => None,
            })
            .collect()
    }

    fn events(actions: &[Action]) -> Vec<&ResponderEvent> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Event(event) => Some(event),
                Action::Send(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_vt_byte_chooses_mllp_and_the_first_byte_is_not_lost() {
        let mut auto = Auto::new(ControlIdSource::new());
        assert!(!auto.is_detected());
        let actions = auto.feed(&framed(MESSAGE), now());
        assert!(auto.is_detected());
        assert_eq!(
            sends(&actions).len(),
            1,
            "HL7/MLLP отвечает ACK: {actions:?}"
        );
        assert!(
            sends(&actions)[0].first() == Some(&mllp::SB),
            "ACK в рамке MLLP"
        );
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn an_enq_byte_chooses_astm() {
        let mut auto = Auto::new(ControlIdSource::new());
        let actions = auto.feed(&[astm::ENQ], now());
        assert_eq!(sends(&actions), vec![vec![astm::ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_leading_eot_does_not_prevent_the_astm_decision() {
        // Прибор шлёт `EOT` перед `ENQ`: решение принимает `ENQ`, а не шум перед ним.
        let mut auto = Auto::new(ControlIdSource::new());
        let actions = auto.feed(&[astm::EOT, astm::ENQ], now());
        assert!(auto.is_detected());
        assert_eq!(sends(&actions), vec![vec![astm::ACK]]);

        let mut stream = vec![astm::STX];
        stream.extend_from_slice(b"1P|1\r");
        stream.push(astm::ETX);
        stream.extend_from_slice(b"3E\r\n");
        let actions = auto.feed(&stream, now());
        assert_eq!(sends(&actions), vec![vec![astm::ACK]]);
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn leading_noise_before_a_vt_lane_chooses_mllp() {
        // `NUL CR LF XON XOFF` перед рамкой — шум линии, а не протокол.
        let mut auto = Auto::new(ControlIdSource::new());
        let mut request = vec![0x00, b'\r', b'\n', 0x11, 0x13];
        request.extend(framed(MESSAGE));
        let actions = auto.feed(&request, now());
        assert!(auto.is_detected());
        assert_eq!(sends(&actions).len(), 1, "ACK на сообщение: {actions:?}");
        assert_eq!(sends(&actions)[0].first(), Some(&mllp::SB));
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn a_fragment_of_noise_alone_does_not_decide() {
        let mut auto = Auto::new(ControlIdSource::new());
        assert!(
            auto.feed(&[0x00, b'\r', b'\n', 0x11, 0x13, astm::EOT], now())
                .is_empty()
        );
        assert!(!auto.is_detected(), "решение ещё не принято");
        assert_eq!(
            sends(&auto.feed(&[astm::ENQ], now())),
            vec![vec![astm::ACK]]
        );
    }

    #[test]
    fn any_other_first_byte_means_no_answers_with_a_code() {
        let mut auto = Auto::new(ControlIdSource::new());
        let actions = auto.feed(b"MSH|not framed\r", now());
        assert!(auto.is_detected(), "решение принято и не пересматривается");
        assert!(sends(&actions).is_empty(), "ответов нет");
        assert!(
            matches!(&events(&actions)[..], [ResponderEvent::Info { message, code }]
                if code == &Some(EventCode::ProtocolUndetected)
                    && message.contains("0x4D")),
            "{actions:?}"
        );
        // Байты после решения доходят до «Без ответов» и не дают событий.
        assert!(auto.feed(b"\x0bmore", now()).is_empty());
    }

    #[test]
    fn an_empty_fragment_does_not_decide() {
        let mut auto = Auto::new(ControlIdSource::new());
        assert!(auto.feed(&[], now()).is_empty());
        assert!(!auto.is_detected());
        assert_eq!(
            sends(&auto.feed(&[astm::ENQ], now())),
            vec![vec![astm::ACK]]
        );
    }

    #[test]
    fn the_decision_survives_a_split_first_byte() {
        // Первый байт пришёл отдельным фрагментом: второй фрагмент идёт тому же автомату.
        let mut split = Auto::new(ControlIdSource::new());
        let mut split_actions = split.feed(&[mllp::SB], now());
        split_actions.extend(split.feed(&framed(MESSAGE)[1..], now()));

        let mut whole = Auto::new(ControlIdSource::new());
        let whole_actions = whole.feed(&framed(MESSAGE), now());

        assert_eq!(sends(&split_actions), sends(&whole_actions));
        assert_eq!(events(&split_actions), events(&whole_actions));
    }

    #[test]
    fn the_decision_is_not_repeated_on_the_next_fragment() {
        let mut auto = Auto::new(ControlIdSource::new());
        auto.feed(&[astm::ENQ], now());
        // Второй фрагмент начинается с `0x0B`, но решение уже принято: ASTM его игнорирует.
        let actions = auto.feed(b"\x0bMSH|truncated\r", now());
        assert!(sends(&actions).is_empty(), "{actions:?}");
        assert!(events(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn reset_makes_the_next_connection_detect_again() {
        let mut auto = Auto::new(ControlIdSource::new());
        auto.feed(&[astm::ENQ], now());
        assert!(auto.is_detected());
        auto.reset();
        assert!(!auto.is_detected());
        assert_eq!(
            sends(&auto.feed(&[astm::ENQ], now())),
            vec![vec![astm::ACK]]
        );
    }

    #[test]
    fn the_detected_astm_automaton_takes_the_nak_setting() {
        let mut auto = Auto::new(ControlIdSource::new()).with_nak_on_bad_checksum(true);
        let mut stream = vec![astm::ENQ, astm::STX];
        stream.extend_from_slice(b"1x");
        stream.extend_from_slice(&[astm::ETX]);
        stream.extend_from_slice(b"AD\r\n");
        let actions = auto.feed(&stream, now());
        assert_eq!(
            sends(&actions),
            vec![vec![astm::ACK], vec![astm::NAK]],
            "ENQ подтверждён, кадр с неверной суммой отклонён"
        );
    }

    #[test]
    fn finish_and_timeout_are_delegated_to_the_chosen_automaton() {
        let mut auto = Auto::new(ControlIdSource::new());
        assert!(
            auto.finish(now()).is_empty(),
            "до решения нечего finish-ить"
        );
        assert_eq!(auto.poll_timeout(now()), None);

        let mut stream = vec![astm::ENQ, astm::STX];
        stream.extend_from_slice(b"0P|1\r");
        assert_eq!(sends(&auto.feed(&stream, now())), vec![vec![astm::ACK]]);
        assert_eq!(auto.poll_timeout(now()), Some(astm::FRAME_TIMEOUT));
        let actions = auto.finish(now());
        assert!(
            matches!(&actions[..], [Action::Event(ResponderEvent::Error { .. })]),
            "недособранный кадр ASTM: {actions:?}"
        );
    }
}
