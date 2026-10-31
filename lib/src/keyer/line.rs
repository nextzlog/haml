use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::morse::{self, Timing, MAX_WPM, MIN_WPM};
use crate::ptt::{Line, Lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSetup {
    pub key: Line,
    pub ptt: bool,
    pub ptt_before: Duration,
    pub ptt_after: Duration,
    pub phone: Line,
}

enum Cmd {
    Send(String, u32),
    Ptt(bool),
    PhonePtt(bool),
}

pub struct LineKeyer {
    commands: Sender<Cmd>,
    events: Receiver<KeyerEvent>,
    pending: Arc<AtomicU32>,
    aborts: Arc<AtomicU32>,
    wpm: Arc<AtomicU32>,
    timing: Arc<Mutex<Timing>>,
    ptt: bool,
}

impl LineKeyer {
    pub fn new(lines: impl Lines, setup: LineSetup, timing: Timing) -> Self {
        let (commands, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let pending = Arc::new(AtomicU32::new(0));
        let aborts = Arc::new(AtomicU32::new(0));
        let wpm = Arc::new(AtomicU32::new(timing.wpm));
        let shared_timing = Arc::new(Mutex::new(timing));
        let worker = Worker {
            lines,
            setup,
            timing,
            shared_timing: shared_timing.clone(),
            commands: rx,
            events: tx,
            pending: pending.clone(),
            done: 0,
            aborts: aborts.clone(),
            wpm: wpm.clone(),
            ptt: false,
            manual_ptt: false,
        };
        thread::Builder::new()
            .name("haml-line-keyer".into())
            .spawn(move || worker.run())
            .expect("keyer thread");
        LineKeyer {
            commands,
            events,
            pending,
            aborts,
            wpm,
            timing: shared_timing,
            ptt: setup.ptt,
        }
    }
}

impl Keyer for LineKeyer {
    fn caps(&self) -> KeyerCaps {
        KeyerCaps {
            ptt: self.ptt,
            ptt_delays: self.ptt,
            ..KeyerCaps::default()
        }
    }

    fn send(&mut self, text: &str) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let epoch = self.aborts.load(Ordering::SeqCst);
        let _ = self.commands.send(Cmd::Send(text.to_string(), epoch));
    }

    fn abort(&mut self) {
        self.aborts.fetch_add(1, Ordering::SeqCst);
    }

    fn set_wpm(&mut self, wpm: u32) {
        self.wpm
            .store(wpm.clamp(MIN_WPM, MAX_WPM), Ordering::SeqCst);
    }

    fn wpm(&self) -> u32 {
        self.wpm.load(Ordering::SeqCst)
    }

    fn set_timing(&mut self, timing: Timing) {
        self.set_wpm(timing.wpm);
        *self.timing.lock().unwrap() = timing;
    }

    fn set_ptt(&mut self, on: bool) {
        let _ = self.commands.send(Cmd::Ptt(on));
    }

    fn set_phone_ptt(&mut self, on: bool) {
        let _ = self.commands.send(Cmd::PhonePtt(on));
    }

    fn busy(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        self.events.try_iter().collect()
    }
}

struct Worker<L: Lines> {
    lines: L,
    setup: LineSetup,
    timing: Timing,
    shared_timing: Arc<Mutex<Timing>>,
    commands: Receiver<Cmd>,
    events: Sender<KeyerEvent>,
    pending: Arc<AtomicU32>,
    done: u32,
    aborts: Arc<AtomicU32>,
    wpm: Arc<AtomicU32>,
    ptt: bool,
    manual_ptt: bool,
}

impl<L: Lines> Worker<L> {
    fn run(mut self) {
        'run: while let Ok(cmd) = self.commands.recv() {
            let mut next = Some(cmd);
            while let Some(cmd) = next.take() {
                match cmd {
                    Cmd::Ptt(on) => {
                        self.manual_ptt = on;
                        self.ptt_line(on);
                    }
                    Cmd::PhonePtt(on) => {
                        self.manual_ptt = on;
                        self.line(self.setup.phone, on);
                    }
                    Cmd::Send(text, epoch) => {
                        self.key(&text, epoch);
                        self.done += 1;
                    }
                }
                next = match self.commands.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        self.idle();
                        break 'run;
                    }
                };
            }
            self.idle();
        }
        let _ = self.lines.set(self.setup.key, false);
        if self.setup.ptt || self.ptt {
            let _ = self.lines.set(self.setup.key.other(), false);
        }
    }

    fn idle(&mut self) {
        if self.setup.ptt && self.ptt && !self.manual_ptt {
            self.wait(self.setup.ptt_after, None);
            if self.pending.load(Ordering::SeqCst) > self.done {
                return;
            }
            self.ptt_line(false);
        }
        if self.done == 0 {
            return;
        }
        let left = self.pending.fetch_sub(self.done, Ordering::SeqCst) - self.done;
        self.done = 0;
        if left == 0 {
            let _ = self.events.send(KeyerEvent::Finished);
        }
    }

    fn line(&mut self, line: Line, on: bool) {
        if let Err(e) = self.lines.set(line, on) {
            let _ = self.events.send(KeyerEvent::Error(e.to_string()));
        }
    }

    fn ptt_line(&mut self, on: bool) {
        if self.setup.ptt {
            self.ptt = on;
            self.line(self.setup.key.other(), on);
        }
    }

    fn key(&mut self, text: &str, epoch: u32) {
        if self.aborted(epoch) {
            return;
        }
        if self.setup.ptt && !self.ptt {
            self.ptt_line(true);
            self.wait(self.setup.ptt_before, None);
        }
        self.timing = *self.shared_timing.lock().unwrap();
        self.timing.wpm = self.wpm.load(Ordering::SeqCst);
        for step in morse::keying(text, &self.timing) {
            if self.aborted(epoch) {
                break;
            }
            self.line(self.setup.key, step.key);
            self.wait(step.len, Some(epoch));
        }
        self.line(self.setup.key, false);
    }

    fn aborted(&self, epoch: u32) -> bool {
        self.aborts.load(Ordering::SeqCst) != epoch
    }

    fn wait(&self, len: Duration, epoch: Option<u32>) {
        let until = Instant::now() + len;
        let spin = Duration::from_millis(2);
        loop {
            let now = Instant::now();
            if now >= until || epoch.is_some_and(|e| self.aborted(e)) {
                return;
            }
            let left = until - now;
            if left > spin {
                thread::sleep((left - spin).min(Duration::from_millis(20)));
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[derive(Clone, Default)]
    struct FakeLines(Arc<Mutex<Vec<(Line, bool, Instant)>>>);

    impl Lines for FakeLines {
        fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
            self.0.lock().unwrap().push((line, on, Instant::now()));
            Ok(())
        }
    }

    fn wait_idle(k: &mut LineKeyer) -> Vec<KeyerEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while k.busy() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(5));
        events.extend(k.poll());
        events
    }

    #[test]
    fn keys_with_ptt_on_the_other_line() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Dtr,
            phone: Line::Dtr,
            ptt: true,
            ptt_before: Duration::from_millis(10),
            ptt_after: Duration::from_millis(10),
        };
        let timing = Timing {
            wpm: 60,
            ..Timing::default()
        };
        let mut k = LineKeyer::new(lines.clone(), setup, timing);
        k.send("EE");
        assert!(k.busy());
        assert_eq!(wait_idle(&mut k), vec![KeyerEvent::Finished]);
        let log = lines.0.lock().unwrap().clone();
        let seq: Vec<(Line, bool)> = log.iter().map(|(l, o, _)| (*l, *o)).collect();
        use Line::*;
        assert_eq!(
            seq,
            vec![
                (Rts, true),
                (Dtr, true),
                (Dtr, false),
                (Dtr, true),
                (Dtr, false),
                (Dtr, false),
                (Rts, false)
            ]
        );
        let dot = log[2].2 - log[1].2;
        assert!(dot >= Duration::from_millis(20) && dot < Duration::from_millis(40));
        let lead = log[1].2 - log[0].2;
        assert!(lead >= Duration::from_millis(10));
    }

    #[test]
    fn send_during_the_ptt_tail_stays_busy() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Dtr,
            phone: Line::Dtr,
            ptt: true,
            ptt_before: Duration::from_millis(5),
            ptt_after: Duration::from_millis(150),
        };
        let timing = Timing {
            wpm: 60,
            ..Timing::default()
        };
        let mut k = LineKeyer::new(lines.clone(), setup, timing);
        k.send("E");
        let start = Instant::now();
        while !lines
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.0 == Line::Dtr && !e.1)
            && start.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(60));
        k.send("E");
        assert_eq!(wait_idle(&mut k), vec![KeyerEvent::Finished]);
        let log = lines.0.lock().unwrap().clone();
        let marks = log.iter().filter(|e| e.0 == Line::Dtr && e.1).count();
        assert_eq!(marks, 2);
        let ptt: Vec<bool> = log
            .iter()
            .filter(|e| e.0 == Line::Rts)
            .map(|e| e.1)
            .collect();
        assert_eq!(ptt, vec![true, false]);
    }

    #[test]
    fn timing_changes_apply_to_a_connected_keyer() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Dtr,
            phone: Line::Dtr,
            ptt: false,
            ptt_before: Duration::ZERO,
            ptt_after: Duration::ZERO,
        };
        let timing = Timing {
            wpm: 60,
            ..Timing::default()
        };
        let mut k = LineKeyer::new(lines.clone(), setup, timing);
        k.set_timing(Timing {
            weight: 80,
            ..timing
        });
        k.send("E");
        wait_idle(&mut k);
        let log = lines.0.lock().unwrap().clone();
        let on = log.iter().position(|e| e.0 == Line::Dtr && e.1).unwrap();
        let mark = log[on + 1].2 - log[on].2;
        assert!(mark >= Duration::from_millis(28), "{mark:?}");
    }

    #[test]
    fn dropping_turns_ptt_off() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Dtr,
            phone: Line::Dtr,
            ptt: true,
            ptt_before: Duration::ZERO,
            ptt_after: Duration::ZERO,
        };
        let mut k = LineKeyer::new(lines.clone(), setup, Timing::default());
        k.set_ptt(true);
        drop(k);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let log = lines.0.lock().unwrap().clone();
            if log.last().map(|e| (e.0, e.1)) == Some((Line::Rts, false)) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("PTT left on: {:?}", lines.0.lock().unwrap());
    }

    #[test]
    fn phone_ptt_keys_the_key_line_and_is_left_off() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Dtr,
            phone: Line::Dtr,
            ptt: false,
            ptt_before: Duration::ZERO,
            ptt_after: Duration::ZERO,
        };
        let mut k = LineKeyer::new(lines.clone(), setup, Timing::default());
        k.set_phone_ptt(true);
        let start = Instant::now();
        while lines.0.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            lines.0.lock().unwrap().first().map(|e| (e.0, e.1)),
            Some((Line::Dtr, true))
        );
        drop(k);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let log = lines.0.lock().unwrap().clone();
            if log.last().map(|e| (e.0, e.1)) == Some((Line::Dtr, false)) {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("PTT left on: {:?}", lines.0.lock().unwrap());
    }

    #[test]
    fn abort_stops_keying() {
        let lines = FakeLines::default();
        let setup = LineSetup {
            key: Line::Rts,
            phone: Line::Rts,
            ptt: false,
            ptt_before: Duration::ZERO,
            ptt_after: Duration::ZERO,
        };
        let mut k = LineKeyer::new(lines.clone(), setup, Timing::default());
        k.send("CQ CQ CQ TEST");
        thread::sleep(Duration::from_millis(30));
        k.abort();
        assert_eq!(wait_idle(&mut k), vec![KeyerEvent::Finished]);
        let log = lines.0.lock().unwrap().clone();
        assert!(log.len() < 10);
        assert_eq!(log.last().map(|e| (e.0, e.1)), Some((Line::Rts, false)));
        k.send("E");
        assert_eq!(wait_idle(&mut k), vec![KeyerEvent::Finished]);
    }
}
