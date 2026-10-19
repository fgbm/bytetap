//! COM-порт «вместо ЛИС» на паре pty от `socat` (T-3, M3-AC2, M3-AC11).
//!
//! `socat -d -d pty,raw,echo=0 pty,raw,echo=0` создаёт два связанных псевдотерминала.
//! Сессия открывает первый как COM-порт, тест пишет во второй `ENQ` и кадр ASTM E1381
//! и получает `ACK`; в журнале видны оба направления, `peer` — имя порта, `local` пуст.
//!
//! Без `socat` тест помечается пропущенным явным сообщением, а не молча зелёным.
#![cfg(unix)]

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use bytetap_core::journal::{Autoresponder, ClosedBy, Dir, Event, Mode, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::serial::{SerialFormat, SerialSettings};

/// Пара pty от `socat`; дочерний процесс живёт, пока живёт значение.
struct Socat {
    child: Child,
    first: String,
    second: String,
}

impl Socat {
    /// Запускает `socat`; `None` — программы нет, тест пропускается.
    fn start() -> io::Result<Option<Self>> {
        let mut child = match Command::new("socat")
            .args(["-d", "-d", "pty,raw,echo=0", "pty,raw,echo=0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("socat: нет вывода"))?;
        let mut paths = Vec::new();
        for line in BufReader::new(stderr).lines() {
            let line = line?;
            if let Some(path) = line
                .split_whitespace()
                .find(|word| word.starts_with("/dev/") && !word.ends_with(".log"))
                && !paths.contains(&path.to_owned())
            {
                paths.push(path.to_owned());
            }
            if paths.len() == 2 {
                break;
            }
        }
        if paths.len() != 2 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "socat не назвал два pty: проверьте `socat -d -d pty,raw,echo=0 pty,raw,echo=0`",
            ));
        }
        let second = paths.pop().expect("второй pty");
        let first = paths.pop().expect("первый pty");
        Ok(Some(Self {
            child,
            first,
            second,
        }))
    }
}

impl Drop for Socat {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Кадр ASTM E1381 с верной контрольной суммой и содержанием, не требующим разбора.
fn astm_frame() -> Vec<u8> {
    let body = b"0H|\\^&||||||||||P|E1394-97|20261019101500\r";
    let checksum = body
        .iter()
        .chain(std::iter::once(&0x03u8))
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    let mut frame = vec![0x02];
    frame.extend_from_slice(body);
    frame.push(0x03);
    frame.extend(format!("{checksum:02X}").bytes());
    frame.extend_from_slice(b"\r\n");
    frame
}

/// Открывает второй конец pty, шлёт `ENQ` и кадр, читает по байту ответа.
fn exchange(port_name: &str) -> io::Result<Vec<u8>> {
    let mut port = serialport::new(port_name, 9600)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_secs(5))
        .open()
        .map_err(io::Error::from)?;
    let mut answers = Vec::new();
    for request in [vec![0x05u8], astm_frame()] {
        port.write_all(&request)?;
        port.flush()?;
        let mut answer = [0u8; 1];
        port.read_exact(&mut answer)?;
        answers.push(answer[0]);
    }
    Ok(answers)
}

/// Ошибка порта и неподдерживаемые настройки — ошибка старта до создания журнала (M3-AC3).
#[tokio::test]
async fn a_missing_port_is_a_start_error_before_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let settings =
        SerialSettings::new("/dev/bytetap-no-such-port", 9600, SerialFormat::EIGHT_N_ONE);
    let err = Session::start(
        SessionConfig::serial(settings, Duration::from_secs(1)).with_log_dir(dir.path()),
    )
    .await
    .expect_err("порта нет");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("папка").count(),
        0,
        "журнал не создан"
    );
}

#[tokio::test]
async fn broken_port_settings_are_rejected_before_the_journal() {
    let dir = tempfile::tempdir().expect("временная папка");
    let settings = SerialSettings::new("COM5", 0, SerialFormat::EIGHT_N_ONE);
    let err = Session::start(
        SessionConfig::serial(settings, Duration::from_secs(1)).with_log_dir(dir.path()),
    )
    .await
    .expect_err("скорость вне диапазона");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("папка").count(),
        0,
        "журнал не создан"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_astm_exchange_goes_through_a_pty_pair() {
    let socat = match Socat::start() {
        Ok(Some(socat)) => socat,
        Ok(None) => {
            eprintln!("тест пропущен: socat не найден; для COM-теста установите socat (M3-AC11)");
            return;
        }
        Err(err) => panic!("socat не запустился: {err}"),
    };

    let dir = tempfile::tempdir().expect("временная папка");
    let settings =
        SerialSettings::new(&socat.first, 9600, SerialFormat::EIGHT_N_ONE).with_lines(false, false);
    let handle = Session::start(
        SessionConfig::serial(settings, Duration::from_secs(1))
            .with_responder(Autoresponder::AstmE1381)
            .with_log_dir(dir.path()),
    )
    .await
    .expect("порт открывается");
    assert_eq!(handle.local_addr(), socat.first);
    let path = handle.journal_path().to_path_buf();

    let port_name = socat.second.clone();
    let answers = tokio::task::spawn_blocking(move || exchange(&port_name))
        .await
        .expect("задача обмена")
        .expect("обмен через pty");
    assert_eq!(answers, [0x06, 0x06], "ACK на ENQ и на кадр");

    let stats = handle.stop().await.expect("остановка");
    assert_eq!(stats.connections, 1);

    let outcome = read_all(&path).expect("журнал читается");
    assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
    let Event::Session(header) = &outcome.lines[0].event else {
        panic!("первая строка — заголовок");
    };
    assert_eq!(header.mode, Mode::Serial);
    assert_eq!(header.autoresponder, Autoresponder::AstmE1381);
    assert_eq!(header.settings["port"], socat.first);
    assert_eq!(header.settings["baud"], 9600);
    assert_eq!(header.settings["format"], "8N1");
    assert_eq!(header.settings["flow"], "none");
    assert_eq!(header.settings["dtr"], false);
    assert_eq!(header.settings["rts"], false);
    assert!(
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("-serial.jsonl")),
        "имя файла режима: {}",
        path.display()
    );

    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for line in &outcome.lines {
        match &line.event {
            Event::Open(open) => {
                assert_eq!(open.peer, socat.first, "peer — имя порта");
                assert_eq!(open.local, "", "у COM-порта нет локального адреса");
            }
            Event::Close(close) => {
                assert_eq!(close.by, ClosedBy::Bytetap);
                assert_eq!(close.reason, "stop");
            }
            Event::Data(data) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&data.b64)
                    .expect("b64");
                match data.dir {
                    Dir::AnalyzerToLis => incoming.extend(bytes),
                    Dir::LisToAnalyzer => {
                        assert!(data.synthetic, "ответы автоответчика синтетические");
                        outgoing.extend(bytes);
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        incoming,
        [vec![0x05u8], astm_frame()].concat(),
        "запросы записаны байт в байт"
    );
    assert_eq!(outgoing, [0x06, 0x06], "ответы записаны в журнал");
}
