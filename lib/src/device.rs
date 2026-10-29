use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{Error, Result};

pub trait Driver: Send + 'static {
    type Command: Send + 'static;
    type Event: Clone + Send + 'static;

    fn handle(&mut self, command: Self::Command, now: Instant, out: &mut Vec<Self::Event>);

    fn tick(&mut self, now: Instant, out: &mut Vec<Self::Event>);

    fn close(&mut self, _out: &mut Vec<Self::Event>) {}

    fn interval(&self) -> Duration {
        Duration::from_millis(5)
    }
}

type Subscribers<E> = Arc<Mutex<Vec<Sender<E>>>>;

pub struct Device<C, E> {
    commands: Option<Sender<C>>,
    subscribers: Subscribers<E>,
    thread: Option<JoinHandle<()>>,
}

fn publish<E: Clone>(subs: &Subscribers<E>, events: &mut Vec<E>) {
    if events.is_empty() {
        return;
    }
    let mut s = subs.lock().unwrap_or_else(|e| e.into_inner());
    for e in events.drain(..) {
        s.retain(|tx| tx.send(e.clone()).is_ok());
    }
}

impl<C: Send + 'static, E: Clone + Send + 'static> Device<C, E> {
    pub fn spawn<D: Driver<Command = C, Event = E>>(name: &str, mut driver: D) -> Self {
        let (commands, rx) = mpsc::channel::<C>();
        let subscribers: Subscribers<E> = Arc::default();
        let subs = subscribers.clone();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let mut out = Vec::new();
                loop {
                    match rx.recv_timeout(driver.interval()) {
                        Ok(c) => {
                            driver.handle(c, Instant::now(), &mut out);
                            for c in rx.try_iter() {
                                driver.handle(c, Instant::now(), &mut out);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    driver.tick(Instant::now(), &mut out);
                    publish(&subs, &mut out);
                }
                driver.close(&mut out);
                publish(&subs, &mut out);
            })
            .expect("device thread");
        Device {
            commands: Some(commands),
            subscribers,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: C) -> Result<()> {
        self.commander().send(command)
    }

    pub fn commander(&self) -> Commander<C> {
        Commander(self.commands.clone())
    }

    pub fn subscribe(&self) -> Receiver<E> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }
}

impl<C, E> Drop for Device<C, E> {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct Commander<C>(Option<Sender<C>>);

impl<C> Clone for Commander<C> {
    fn clone(&self) -> Self {
        Commander(self.0.clone())
    }
}

impl<C> Commander<C> {
    pub fn send(&self, command: C) -> Result<()> {
        self.0
            .as_ref()
            .ok_or(Error::Closed)?
            .send(command)
            .map_err(|_| Error::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(u32);

    impl Driver for Counter {
        type Command = u32;
        type Event = u32;

        fn handle(&mut self, c: u32, _now: Instant, out: &mut Vec<u32>) {
            self.0 += c;
            out.push(self.0);
        }

        fn tick(&mut self, _now: Instant, _out: &mut Vec<u32>) {}

        fn close(&mut self, out: &mut Vec<u32>) {
            out.push(0);
        }
    }

    #[test]
    fn commands_go_in_and_events_reach_every_subscriber() {
        let d = Device::spawn("counter", Counter(0));
        let (a, b) = (d.subscribe(), d.subscribe());
        d.send(2).unwrap();
        d.commander().send(3).unwrap();
        let t = Duration::from_secs(5);
        assert_eq!(
            (a.recv_timeout(t).unwrap(), a.recv_timeout(t).unwrap()),
            (2, 5)
        );
        assert_eq!(b.recv_timeout(t).unwrap(), 2);
        drop(b);
        drop(d);
        assert_eq!(a.recv_timeout(t).unwrap(), 0, "closed on drop");
    }
}
