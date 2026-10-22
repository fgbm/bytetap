//! Порядок времени в журнале прокси: `mono_us` относится к чтению, а не к записи (FR-22).
//!
//! Каждое направление ведёт свой насос, поэтому строки журнала двух направлений могут
//! вставать в любом порядке: публикация идёт после записи в сокет. Время `mono_us` при
//! этом берётся при чтении фрагмента, и по нему виден порядок причин и следствий:
//! ответ на круг не старше запроса этого круга, следующий запрос не старше ответа
//! предыдущего. Гарантия записана в `docs/reference/session-format.md`.
//!
//! ЛИС-эхо возвращает каждый фрагмент: 10⁴ кругов дают 10⁴ пар запрос-ответ, и любая
//! инверсия во времени видна сразу.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use bytetap_core::journal::{Dir, Event, read_all};
use bytetap_core::session::{Session, SessionConfig};
use bytetap_core::transport::ProxyRole;
use bytetap_testkit::lis::{LisBehaviour, LisEmulator};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

/// Сколько кругов «запрос — ответ» делает тест.
const ROUNDS: usize = 10_000;

/// Размер запроса одного круга.
const REQUEST: [u8; 10] = *b"0123456789";

/// Сколько тест ждёт события или обмена.
const WAIT: Duration = Duration::from_secs(60);

/// Моменты `mono_us` фрагментов одного направления по порядку журнала.
fn times(path: &Path, dir: Dir) -> Vec<u64> {
    read_all(path)
        .expect("журнал читается")
        .lines
        .iter()
        .filter_map(|line| match &line.event {
            Event::Data(data) if data.dir == dir && !data.synthetic => Some(data.mono_us),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_time_of_a_fragment_is_the_time_of_its_read() {
    let dir = tempfile::tempdir().expect("временная папка");
    let lis = LisEmulator::listen("127.0.0.1:0".parse().unwrap())
        .await
        .expect("ЛИС слушает")
        .with_behaviour(LisBehaviour::Echo)
        .with_timeout(WAIT);
    let lis_addr = lis.local_addr().expect("адрес ЛИС");
    let lis_task = tokio::spawn(lis.run());

    let handle = Session::start(
        SessionConfig::proxy(
            "127.0.0.1:0".parse().unwrap(),
            lis_addr.to_string(),
            ProxyRole::AnalyzerClient,
            Duration::from_millis(100),
        )
        .with_log_dir(dir.path()),
    )
    .await
    .expect("сессия прокси");
    let listen: SocketAddr = handle.local_addr().parse().expect("адрес");
    let path = handle.journal_path().to_path_buf();

    let mut stream = TcpStream::connect(listen).await.expect("анализатор");
    for round in 0..ROUNDS {
        stream
            .write_all(&REQUEST)
            .await
            .unwrap_or_else(|error| panic!("круг {}: {error}", round + 1));
        let mut echo = [0u8; REQUEST.len()];
        timeout(WAIT, stream.read_exact(&mut echo))
            .await
            .unwrap_or_else(|_| panic!("круг {}: эхо не пришло", round + 1))
            .unwrap_or_else(|error| panic!("круг {}: {error}", round + 1));
        assert_eq!(echo, REQUEST, "эхо круга {}", round + 1);
    }
    stream.shutdown().await.expect("FIN анализатора");

    timeout(WAIT, lis_task)
        .await
        .expect("ЛИС закончила")
        .expect("задача ЛИС")
        .expect("обмен ЛИС");
    for _ in 0..500 {
        let closes = read_all(&path)
            .expect("журнал читается")
            .lines
            .iter()
            .filter(|line| matches!(line.event, Event::Close(_)))
            .count();
        if closes == 1 {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("остановка");

    let requests = times(&path, Dir::AnalyzerToLis);
    let answers = times(&path, Dir::LisToAnalyzer);
    assert_eq!(requests.len(), ROUNDS, "по запросу на круг");
    assert_eq!(answers.len(), ROUNDS, "по ответу на круг");

    // Внутри направления время не убывает: фрагменты одного насоса идут по порядку чтения.
    for (round, pair) in requests.windows(2).enumerate() {
        assert!(
            pair[0] <= pair[1],
            "запрос круга {} прочитан раньше предыдущего: {pair:?}",
            round + 2
        );
    }
    for (round, pair) in answers.windows(2).enumerate() {
        assert!(
            pair[0] <= pair[1],
            "ответ круга {} прочитан раньше предыдущего: {pair:?}",
            round + 2
        );
    }
    // Между направлениями порядок причинный: ответ читается после запроса, следующий
    // запрос — после ответа на предыдущий.
    for (round, (request, answer)) in requests.iter().zip(&answers).enumerate() {
        assert!(
            request < answer,
            "ответ круга {} не позже его запроса: {request} и {answer}",
            round + 1
        );
        if let Some(next) = requests.get(round + 1) {
            assert!(
                answer <= next,
                "запрос круга {} прочитан раньше ответа на предыдущий: {answer} и {next}",
                round + 2
            );
        }
    }
}
