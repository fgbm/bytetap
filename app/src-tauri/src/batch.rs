//! Батчер ленты: события сессии копятся в буфере и уходят в интерфейс пачкой (M2-AC16).
//!
//! Channel упорядочен, но каждое событие отдельным сообщением забило бы IPC на активном
//! обмене, поэтому отправка идёт по таймеру [`FLUSH_INTERVAL`] — примерно раз в 50 мс.
//! Пока интерфейс не успевает, буфер ограничен [`MAX_BUFFERED`]: лишние события ленты
//! отбрасываются, а их число уходит в `dropped` следующей пачки. Журнал при этом не
//! теряет ничего: отбрасывание — забота ленты, а не записи (FR-20, NFR-6).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use bytetap_core::session::{SessionCounters, SessionEvent};
use tokio::sync::{broadcast, watch};

use crate::api::SessionStats;
use crate::feed::{FeedBatch, FeedEvent, JournalFailureCode, StopReason};

/// Как часто накопленное уходит в интерфейс.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// Сколько событий ленты держим между отправками.
///
/// Больше — растёт память интерфейса и время разбора пачки; меньше — пачка на активном
/// обмене рвётся чаще, и накладные расходы IPC становятся заметны. Пять тысяч — примерно
/// секунда активного обмена при типичных сообщениях анализатора.
pub const MAX_BUFFERED: usize = 5000;

/// Куда отправлять пачки; в приложении это [`tauri::ipc::Channel`], в тестах — приёмник.
///
/// Ошибка отправки не интересна: если окно закрыто, терять уже нечего, а сессия и журнал
/// от этого не зависят.
pub trait FeedSink: Send + Sync {
    /// Отправляет пачку в интерфейс.
    fn send(&self, batch: FeedBatch);
}

impl FeedSink for tauri::ipc::Channel<FeedBatch> {
    fn send(&self, batch: FeedBatch) {
        let _ = tauri::ipc::Channel::send(self, batch);
    }
}

/// Буфер между сессией и отправкой: порядок событий и счётчик отброшенных.
#[derive(Debug, Default)]
pub struct Batcher {
    events: VecDeque<FeedEvent>,
    dropped: u64,
    stats: SessionStats,
}

impl Batcher {
    /// Пустой буфер.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Кладёт событие в буфер; при переполнении самое старое событие отбрасывается.
    pub fn push(&mut self, event: FeedEvent) {
        if self.events.len() == MAX_BUFFERED {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(event);
    }

    /// Кладёт событие сессии; строки журнала без ленты (`session`, `session_end`) пропускаются.
    pub fn push_session(&mut self, event: &bytetap_core::journal::Event) {
        if let Some(event) = FeedEvent::from_session(event) {
            self.push(event);
        }
    }

    /// Учитывает отставание подписчика: события уже потеряны в канале сессии.
    pub fn lagged(&mut self, skipped: u64) {
        self.dropped += skipped;
    }

    /// Счётчики сессии на момент отправки: их же увидит строка состояния.
    pub fn set_stats(&mut self, stats: SessionStats) {
        self.stats = stats;
    }

    /// Забирает накопленное; `None` — отправлять нечего.
    pub fn take(&mut self) -> Option<FeedBatch> {
        if self.events.is_empty() && self.dropped == 0 {
            return None;
        }
        Some(FeedBatch {
            events: self.events.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
            stats: self.stats,
        })
    }
}

/// Чем закончилась пересылка событий в интерфейс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpEnd {
    /// Сессия остановлена штатно: все её события уже отправлены.
    Closed,
    /// Журнал отказал: лента закончилась событием `stopped` с причиной.
    JournalFailure,
}

/// Пересылает события сессии в интерфейс пачками до её остановки.
///
/// `flush` — период отправки; в приложении это [`FLUSH_INTERVAL`], в тестах — короче.
/// `counters` дают строке состояния счётчики ядра: лента может терять события, а итоги
/// сессии — нет (M2-AC16). Отказ журнала виден по `failure` и заканчивает ленту сразу,
/// не дожидаясь следующего события: анализатор мог замолчать.
pub async fn pump(
    mut events: broadcast::Receiver<SessionEvent>,
    mut failure: watch::Receiver<Option<String>>,
    counters: SessionCounters,
    sink: Arc<dyn FeedSink>,
    flush: Duration,
) -> PumpEnd {
    let mut batcher = Batcher::new();
    let mut ticker = tokio::time::interval(flush);
    // Пропущенные тики не должны копиться: после паузы нужен один, ближайший.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Первый тик interval отдаёт сразу; пустую пачку отправлять незачем.
    ticker.tick().await;

    loop {
        batcher.set_stats(counters.snapshot().into());
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => batcher.push_session(&event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => batcher.lagged(skipped),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = wait_for_failure(&mut failure) => {
                // Хвост очереди: всё, что сессия успела отдать до отказа, идёт в ленту
                // раньше `stopped`, а не после него (FR-25).
                drain(&mut events, &mut batcher);
                batcher.set_stats(counters.snapshot().into());
                let reason = failure.borrow().clone().unwrap_or_default();
                batcher.push(FeedEvent::Stopped {
                    reason: StopReason::JournalFailure,
                    code: Some(JournalFailureCode::from_reason(&reason)),
                    message: Some(reason),
                });
                flush_batch(&mut batcher, sink.as_ref());
                return PumpEnd::JournalFailure;
            }
            _ = ticker.tick() => flush_batch(&mut batcher, sink.as_ref()),
        }
    }

    drain(&mut events, &mut batcher);
    batcher.set_stats(counters.snapshot().into());
    flush_batch(&mut batcher, sink.as_ref());
    PumpEnd::Closed
}

/// Дочитывает очередь подписчика без ожидания: события уже в канале, но ещё не в буфере.
fn drain(events: &mut broadcast::Receiver<SessionEvent>, batcher: &mut Batcher) {
    loop {
        match events.try_recv() {
            Ok(event) => batcher.push_session(&event),
            Err(broadcast::error::TryRecvError::Lagged(skipped)) => batcher.lagged(skipped),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                break;
            }
        }
    }
}

/// Ждёт отказа журнала; без отказа (и после выгрузки сессии) не завершается.
async fn wait_for_failure(failure: &mut watch::Receiver<Option<String>>) {
    if failure.borrow_and_update().is_some() {
        return;
    }
    if failure.changed().await.is_err() {
        // Отправитель выгружен: сессия закончилась без отказа, ленте ждать нечего.
        std::future::pending::<()>().await;
    }
    if failure.borrow().is_none() {
        std::future::pending::<()>().await;
    }
}

/// Отправляет накопленное, если есть что отправлять.
fn flush_batch(batcher: &mut Batcher, sink: &dyn FeedSink) {
    if let Some(batch) = batcher.take() {
        sink.send(batch);
    }
}

/// Пачка с единственным событием `stopped` о штатной остановке (FR-25).
#[must_use]
pub fn user_stopped_batch() -> FeedBatch {
    FeedBatch {
        events: vec![FeedEvent::Stopped {
            reason: StopReason::User,
            code: None,
            message: None,
        }],
        dropped: 0,
        stats: SessionStats::default(),
    }
}

/// Пачка с `stopped` об отказе журнала на финальной фиксации (NFR-5).
///
/// Такой отказ виден только по [`bytetap_core::session::SessionHandle::stop`]: сессия уже
/// не отвечает, а лента должна закончиться той же причиной, что и ошибка команды.
#[must_use]
pub fn journal_failed_batch(reason: &str) -> FeedBatch {
    FeedBatch {
        events: vec![FeedEvent::Stopped {
            reason: StopReason::JournalFailure,
            code: Some(JournalFailureCode::from_reason(reason)),
            message: Some(reason.to_owned()),
        }],
        dropped: 0,
        stats: SessionStats::default(),
    }
}

#[cfg(test)]
pub(crate) mod test_sink {
    use std::sync::{Mutex, PoisonError};

    use super::*;

    /// Приёмник пачек для тестов без окна: складывает всё, что отправил батчер.
    #[derive(Debug, Default)]
    pub struct TestSink {
        batches: Mutex<Vec<FeedBatch>>,
    }

    impl TestSink {
        /// Все полученные пачки в порядке отправки.
        pub fn batches(&self) -> Vec<FeedBatch> {
            self.batches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        /// Все полученные события в порядке отправки.
        pub fn events(&self) -> Vec<FeedEvent> {
            self.batches()
                .into_iter()
                .flat_map(|batch| batch.events)
                .collect()
        }

        /// Сколько событий отброшено по всем пачкам.
        pub fn dropped(&self) -> u64 {
            self.batches().iter().map(|batch| batch.dropped).sum()
        }
    }

    impl FeedSink for TestSink {
        fn send(&self, batch: FeedBatch) {
            self.batches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(batch);
        }
    }
}

#[cfg(test)]
mod tests {
    use bytetap_core::journal::{Autoresponder, Event, Mode, Note, Session};

    use super::test_sink::TestSink;
    use super::*;

    fn note(index: usize) -> FeedEvent {
        FeedEvent::Note {
            t: "t".to_owned(),
            text: format!("комментарий {index}"),
        }
    }

    fn session_note(index: usize) -> SessionEvent {
        Arc::new(Event::Note(Note {
            t: "t".to_owned(),
            mono_us: Some(index as u64),
            text: format!("комментарий {index}"),
        }))
    }

    #[test]
    fn batches_keep_the_order_of_events() {
        let mut batcher = Batcher::new();
        for index in 0..3 {
            batcher.push(note(index));
        }
        let batch = batcher.take().expect("пачка");
        assert_eq!(batch.events, vec![note(0), note(1), note(2)]);
        assert_eq!(batch.dropped, 0);
        assert!(batcher.take().is_none(), "повторно отправлять нечего");
    }

    #[test]
    fn the_buffer_drops_the_oldest_events_at_the_limit() {
        let mut batcher = Batcher::new();
        for index in 0..MAX_BUFFERED + 2 {
            batcher.push(note(index));
        }
        let batch = batcher.take().expect("пачка");
        assert_eq!(batch.events.len(), MAX_BUFFERED);
        assert_eq!(
            batch.events[0],
            note(2),
            "два самых старых события отброшены"
        );
        assert_eq!(batch.events[MAX_BUFFERED - 1], note(MAX_BUFFERED + 1));
        assert_eq!(batch.dropped, 2);
    }

    #[test]
    fn lagged_events_are_counted_in_the_next_batch() {
        let mut batcher = Batcher::new();
        batcher.lagged(7);
        batcher.push(note(0));
        let batch = batcher.take().expect("пачка");
        assert_eq!(batch.dropped, 7);
        assert_eq!(batch.events, vec![note(0)]);
    }

    #[test]
    fn a_dropped_counter_alone_is_still_a_batch() {
        let mut batcher = Batcher::new();
        batcher.lagged(3);
        assert_eq!(
            batcher.take(),
            Some(FeedBatch {
                events: Vec::new(),
                dropped: 3,
                stats: SessionStats::default(),
            })
        );
    }

    #[test]
    fn a_batch_carries_the_session_counters() {
        let mut batcher = Batcher::new();
        batcher.set_stats(SessionStats {
            connections: 2,
            bytes_analyzer_to_lis: 10,
            bytes_lis_to_analyzer: 4,
        });
        batcher.push(note(0));
        let batch = batcher.take().expect("пачка");
        assert_eq!(batch.stats.connections, 2);
        assert_eq!(
            batch.stats.bytes_analyzer_to_lis, 10,
            "счётчики ядра, не лента"
        );
        assert_eq!(batch.stats.bytes_lis_to_analyzer, 4);
    }

    #[test]
    fn journal_only_lines_are_not_buffered() {
        let mut batcher = Batcher::new();
        let header = Event::Session(Session::new(
            Mode::TcpServer,
            Autoresponder::None,
            serde_json::json!({}),
            bytetap_core::journal::Host {
                os: "Linux".to_owned(),
            },
        ));
        batcher.push_session(&header);
        assert!(batcher.take().is_none());
    }

    #[tokio::test]
    async fn pump_sends_events_and_stops_on_a_closed_channel() {
        let (sender, receiver) = broadcast::channel(64);
        for index in 0..3 {
            sender.send(session_note(index)).expect("подписчик");
        }
        drop(sender);
        let (_failure, failure) = watch::channel(None);
        let sink = Arc::new(TestSink::default());

        let end = pump(
            receiver,
            failure,
            SessionCounters::detached(),
            sink.clone(),
            Duration::from_millis(5),
        )
        .await;

        assert_eq!(end, PumpEnd::Closed);
        assert_eq!(
            sink.events(),
            vec![note(0), note(1), note(2)],
            "порядок сессии сохранён"
        );
        assert_eq!(sink.dropped(), 0);
    }

    #[tokio::test]
    async fn pump_counts_lagged_events_as_dropped() {
        let (sender, receiver) = broadcast::channel(4);
        for index in 0..100 {
            sender.send(session_note(index)).expect("подписчик");
        }
        drop(sender);
        let (_failure, failure) = watch::channel(None);
        let sink = Arc::new(TestSink::default());

        let end = pump(
            receiver,
            failure,
            SessionCounters::detached(),
            sink.clone(),
            Duration::from_millis(5),
        )
        .await;

        assert_eq!(end, PumpEnd::Closed);
        let received = sink.events().len() as u64;
        assert_eq!(sink.dropped() + received, 100, "ничего не потерялось молча");
        assert!(sink.dropped() >= 96, "отставание видно в dropped");
    }

    #[tokio::test]
    async fn pump_ends_the_feed_with_a_stopped_event_on_a_journal_failure() {
        let (sender, receiver) = broadcast::channel(16);
        let (failure_sender, failure) = watch::channel::<Option<String>>(None);
        let sink = Arc::new(TestSink::default());
        let task = {
            let sink = Arc::clone(&sink);
            tokio::spawn(pump(
                receiver,
                failure,
                SessionCounters::detached(),
                sink,
                Duration::from_millis(5),
            ))
        };

        sender.send(session_note(0)).expect("подписчик");
        // Ждём, пока лента дойдёт до интерфейса, и только потом отказывает журнал:
        // иначе `stopped` мог бы обогнать событие, отправленное раньше.
        for _ in 0..100 {
            if !sink.events().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        failure_sender
            .send(Some("disk full".to_owned()))
            .expect("подписчик отказа");

        let end = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("лента закончилась")
            .expect("задача жива");

        assert_eq!(end, PumpEnd::JournalFailure);
        let events = sink.events();
        assert_eq!(events.first(), Some(&note(0)));
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::JournalFailure,
                code: Some(JournalFailureCode::DiskFull),
                message: Some("disk full".to_owned()),
            })
        );
    }

    #[tokio::test]
    async fn a_journal_failure_drains_the_queue_before_stopped() {
        let (sender, receiver) = broadcast::channel(16);
        let (failure_sender, failure) = watch::channel::<Option<String>>(None);
        let sink = Arc::new(TestSink::default());
        // События уже в канале, но лента их ещё не забрала, а отказ виден сразу.
        for index in 0..3 {
            sender.send(session_note(index)).expect("подписчик");
        }
        failure_sender
            .send(Some("storage full (os error 112)".to_owned()))
            .expect("отказ");

        let end = tokio::time::timeout(
            Duration::from_secs(1),
            pump(
                receiver,
                failure,
                SessionCounters::detached(),
                sink.clone(),
                Duration::from_millis(50),
            ),
        )
        .await
        .expect("лента закончилась");

        assert_eq!(end, PumpEnd::JournalFailure);
        let events = sink.events();
        assert_eq!(
            events[..3].to_vec(),
            vec![note(0), note(1), note(2)],
            "хвост очереди идёт до stopped"
        );
        assert_eq!(
            events.last(),
            Some(&FeedEvent::Stopped {
                reason: StopReason::JournalFailure,
                code: Some(JournalFailureCode::DiskFull),
                message: Some("storage full (os error 112)".to_owned()),
            })
        );
    }
}
