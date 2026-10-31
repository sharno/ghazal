//! The BlueZ backend: one thread owns the D-Bus connection, runs orders in turn,
//! answers BlueZ's pairing agent calls between them and reports what it sees every second.
//! The menu never blocks on Bluetooth, which can take tens of seconds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::backend::Backend;
use super::bluez::Session;
use super::model::{Event, Order};

const REFRESH: Duration = Duration::from_secs(1);
/// How long the thread waits for BlueZ's agent calls before looking at its queue again.
const TICK: Duration = Duration::from_millis(100);

pub struct Worker {
    orders: Sender<Order>,
    events: Receiver<Event>,
    abort: Arc<AtomicBool>,
}

impl Worker {
    pub fn spawn() -> Result<Self> {
        let (orders, queue) = channel();
        let (reports, events) = channel();
        let abort = Arc::new(AtomicBool::new(false));
        let session = Session::new(abort.clone());
        std::thread::Builder::new().name("ghazal-bluetooth".into()).spawn(move || run(session, &queue, &reports)).context("starting the Bluetooth worker")?;
        Ok(Self { orders, events, abort })
    }
}

impl Backend for Worker {
    fn send(&mut self, order: Order) {
        match order {
            // The worker is blocked inside the attempt being cancelled, so a queued order would never be seen.
            Order::Cancel => self.abort.store(true, Ordering::Relaxed),
            order => {
                // A worker that has gone away (its report channel closed) is not coming back.
                let _ = self.orders.send(order);
            }
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Worker {
    /// Leaving the screen interrupts an attempt in progress; the thread then stops scanning and exits.
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Relaxed);
    }
}

fn run(mut session: Session, orders: &Receiver<Order>, reports: &Sender<Event>) {
    let mut observe_at = Instant::now();
    loop {
        let reported = match orders.try_recv() {
            Ok(Order::Run(operation)) => {
                let result = session.execute(operation);
                reports.send(Event::Finished(operation, result)).and_then(|()| reports.send(Event::Observed(session.observe())))
            }
            Ok(Order::Scan(on)) => {
                let failed = session.scan(on).err().map(Event::ScanFailed);
                failed.map_or(Ok(()), |event| reports.send(event)).and_then(|()| reports.send(Event::Observed(session.observe())))
            }
            // Handled by the sender; see `Worker::send`.
            Ok(Order::Cancel) => Ok(()),
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) if Instant::now() >= observe_at => {
                observe_at = Instant::now() + REFRESH;
                reports.send(Event::Observed(session.observe()))
            }
            Err(TryRecvError::Empty) => {
                session.idle(TICK);
                Ok(())
            }
        };
        if reported.is_err() {
            break;
        }
    }
    session.shutdown();
}
