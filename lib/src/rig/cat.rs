use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::ports::Link;
use crate::rig::answers;
use crate::{Command, Event, Mode, Outcome, Protocol, RigCaps, State, Status, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub interval: Duration,
    pub timeout: Duration,
    pub retries: u32,
    pub polling: bool,
    pub verify: bool,
    pub offset: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval: Duration::from_millis(75),
            timeout: Duration::from_millis(1000),
            retries: 2,
            polling: true,
            verify: true,
            offset: 0,
        }
    }
}

impl Config {
    pub fn for_baud(baud: u32) -> Self {
        Config {
            interval: crate::rig::model::poll_interval(baud),
            ..Config::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub writes: u64,
    pub polls: u64,
    pub answered_polls: u64,
    pub events: u64,
    pub retries: u64,
    pub timeouts: u64,
    pub io_errors: u64,
}

#[derive(Debug, Clone, Copy)]
enum Wait {
    Poll,
    Ack,
    Verify,
}

#[derive(Debug, Clone, Copy)]
struct Job {
    command: Command,
    attempts: u32,
    queued: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Inflight {
    wait: Wait,
    job: Option<Job>,
    sent: Instant,
}

pub struct Cat<T> {
    protocol: Box<dyn Protocol>,
    transport: T,
    pub config: Config,
    state: State,
    stats: Stats,
    queue: VecDeque<Job>,
    inflight: Option<Inflight>,
    next_send: Option<Instant>,
    outcomes: Vec<Outcome>,
    error: Option<std::io::ErrorKind>,
    buf: Vec<u8>,
    link: Link,
}

impl<T: Transport> Cat<T> {
    pub fn new(protocol: Box<dyn Protocol>, transport: T, config: Config) -> Self {
        Cat {
            protocol,
            transport,
            config,
            state: State::default(),
            stats: Stats::default(),
            queue: VecDeque::new(),
            inflight: None,
            next_send: None,
            outcomes: Vec::new(),
            error: None,
            buf: Vec::new(),
            link: Link::Connected,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    pub fn error(&self) -> Option<std::io::ErrorKind> {
        self.error
    }

    pub fn busy(&self) -> bool {
        !self.queue.is_empty() || matches!(self.inflight, Some(Inflight { job: Some(_), .. }))
    }

    pub fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut self.outcomes)
    }

    pub fn send(&mut self, command: Command, now: Instant) {
        self.queue.push_back(Job {
            command,
            attempts: 0,
            queued: now,
        });
    }

    pub fn set_freq(&mut self, hz: u64, now: Instant) {
        self.send(Command::SetFreq(self.state.vfo, hz), now);
    }

    pub fn set_mode(&mut self, mode: Mode, now: Instant) {
        self.send(Command::SetMode(self.state.vfo, mode), now);
    }

    fn dial_of(&self, hz: u64) -> u64 {
        (hz as i128 - self.config.offset as i128).clamp(0, u64::MAX as i128) as u64
    }

    fn shown_of(&self, hz: u64) -> u64 {
        (hz as i128 + self.config.offset as i128).clamp(0, u64::MAX as i128) as u64
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        self.link = self.transport.maintain(now);
        self.buf.clear();
        if let Err(e) = self.transport.read(&mut self.buf) {
            self.io_error(e);
        }
        let events: Vec<Event> = self
            .protocol
            .decode(&self.buf)
            .into_iter()
            .map(|e| match e {
                Event::Freq(v, hz) => Event::Freq(v, self.shown_of(hz)),
                e => e,
            })
            .collect();
        self.stats.events += events.len() as u64;
        for e in &events {
            self.state.apply(*e);
        }
        if !events.is_empty() {
            self.error = None;
            self.settle(&events, now);
        }
        if let Some(f) = self.inflight {
            if now.duration_since(f.sent) >= self.config.timeout {
                self.stats.timeouts += 1;
                self.protocol.reset();
                self.inflight = None;
                if let Some(job) = f.job {
                    self.retry(job, now);
                }
                self.next_send = Some(now);
            }
        }
        if self.inflight.is_none() && self.next_send.is_none_or(|t| now >= t) {
            self.dispatch(now);
        }
        events
    }

    fn settle(&mut self, events: &[Event], now: Instant) {
        let Some(f) = self.inflight else {
            return;
        };
        let rejected = events.contains(&Event::Rejected);
        let done = match (f.wait, f.job) {
            (Wait::Poll, _) => {
                self.stats.answered_polls += 1;
                true
            }
            (Wait::Ack, Some(job)) if events.contains(&Event::Ok) => {
                match self.check(&job.command) {
                    Some(read) if self.write(&read) => {
                        self.wait(Wait::Verify, Some(job), now);
                        return;
                    }
                    Some(_) => self.retry(job, now),
                    None => self.finish(job, Status::Done, now),
                }
                true
            }
            (Wait::Ack, Some(job)) if rejected => {
                if job.attempts > self.config.retries {
                    self.finish(job, Status::Rejected, now);
                } else {
                    self.stats.retries += 1;
                    self.queue.push_front(job);
                }
                true
            }
            (Wait::Verify, Some(job)) if self.state.confirms(&job.command) => {
                self.finish(job, Status::Done, now);
                true
            }
            (Wait::Verify, Some(job))
                if rejected || events.iter().any(|e| answers(&job.command, e)) =>
            {
                self.retry(job, now);
                true
            }
            _ => false,
        };
        if done {
            self.inflight = None;
            self.next_send = Some(now + self.config.interval);
        }
    }

    fn retry(&mut self, job: Job, now: Instant) {
        if job.attempts > self.config.retries {
            self.finish(job, Status::Failed, now);
        } else {
            self.stats.retries += 1;
            self.queue.push_front(job);
        }
    }

    fn finish(&mut self, job: Job, status: Status, now: Instant) {
        self.outcomes.push(Outcome {
            command: job.command,
            status,
            attempts: job.attempts,
            latency: now.duration_since(job.queued),
        });
    }

    fn dispatch(&mut self, now: Instant) {
        while let Some(mut job) = self.queue.pop_front() {
            let dial = match job.command {
                Command::SetFreq(v, hz) => Command::SetFreq(v, self.dial_of(hz)),
                c => c,
            };
            let Some(bytes) = self.protocol.encode(&dial) else {
                self.finish(job, Status::Unsupported, now);
                continue;
            };
            job.attempts += 1;
            if !self.write(&bytes) {
                self.retry(job, now);
                self.next_send = Some(now + self.config.interval);
                return;
            }
            if self.protocol.acknowledges() {
                self.wait(Wait::Ack, Some(job), now);
                return;
            }
            match self.check(&job.command) {
                Some(read) if self.write(&read) => self.wait(Wait::Verify, Some(job), now),
                Some(_) => self.retry(job, now),
                None => self.finish(job, Status::Sent, now),
            }
            self.next_send = Some(now + self.config.interval);
            return;
        }
        if self.config.polling {
            let request = self.protocol.poll();
            self.stats.polls += 1;
            if self.write(&request) {
                self.wait(Wait::Poll, None, now);
            } else {
                self.next_send = Some(now + self.config.interval);
            }
        }
    }

    fn check(&mut self, command: &Command) -> Option<Vec<u8>> {
        if self.config.verify {
            self.protocol.verify(command)
        } else {
            None
        }
    }

    fn wait(&mut self, wait: Wait, job: Option<Job>, now: Instant) {
        self.inflight = Some(Inflight {
            wait,
            job,
            sent: now,
        });
    }

    fn write(&mut self, bytes: &[u8]) -> bool {
        self.stats.writes += 1;
        match self.transport.write(bytes) {
            Ok(()) => true,
            Err(e) => {
                self.io_error(e);
                false
            }
        }
    }

    fn io_error(&mut self, e: std::io::Error) {
        self.stats.io_errors += 1;
        self.error = Some(e.kind());
    }
}

impl<T: Transport + Send> crate::Rig for Cat<T> {
    fn caps(&self) -> RigCaps {
        self.protocol.caps()
    }

    fn send(&mut self, command: Command, now: Instant) {
        Cat::send(self, command, now)
    }

    fn tick(&mut self, now: Instant) -> Vec<Event> {
        Cat::tick(self, now)
    }

    fn take_outcomes(&mut self) -> Vec<Outcome> {
        Cat::take_outcomes(self)
    }

    fn state(&self) -> State {
        self.state
    }

    fn link(&self) -> Link {
        self.link
    }

    fn busy(&self) -> bool {
        Cat::busy(self)
    }

    fn error(&self) -> Option<String> {
        self.error.map(|e| e.to_string())
    }

    fn timeouts(&self) -> u64 {
        self.stats.timeouts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vfo;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Lines = (Vec<Vec<u8>>, Vec<u8>);

    #[derive(Clone, Default)]
    struct Wire(Rc<RefCell<Lines>>);

    impl Transport for Wire {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.0.borrow_mut().0.push(bytes.to_vec());
            Ok(())
        }

        fn read(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
            let mut w = self.0.borrow_mut();
            let n = w.1.len();
            buf.append(&mut w.1);
            Ok(n)
        }
    }

    impl Wire {
        fn sent(&self) -> Vec<Vec<u8>> {
            self.0.borrow().0.clone()
        }

        fn reply(&self, bytes: &[u8]) {
            self.0.borrow_mut().1.extend_from_slice(bytes);
        }
    }

    fn kenwood() -> (Wire, Cat<Wire>) {
        let w = Wire::default();
        let p = Box::new(crate::AsciiCat::new(crate::Dialect::Kenwood));
        (w.clone(), Cat::new(p, w, Config::default()))
    }

    #[test]
    fn a_silent_rig_is_polled_once_per_timeout() {
        let (w, mut r) = kenwood();
        let t = Instant::now();
        for ms in 0..999 {
            r.tick(t + Duration::from_millis(ms));
        }
        assert_eq!(w.sent().len(), 1);
        r.tick(t + Duration::from_millis(1000));
        assert_eq!(w.sent().len(), 2);
        assert_eq!(r.stats().timeouts, 1);
    }

    #[test]
    fn next_poll_waits_for_the_interval() {
        let (w, mut r) = kenwood();
        let t = Instant::now();
        r.tick(t);
        w.reply(b"FA00007012000;");
        r.tick(t + Duration::from_millis(10));
        r.tick(t + Duration::from_millis(84));
        assert_eq!(w.sent().len(), 1);
        r.tick(t + Duration::from_millis(85));
        assert_eq!(w.sent().len(), 2);
        assert_eq!(r.state().freq_a, Some(7_012_000));
    }

    #[test]
    fn unacknowledged_sets_are_read_back_and_retried() {
        let (w, mut r) = kenwood();
        let t = Instant::now();
        r.set_freq(14_025_000, t);
        r.tick(t);
        assert_eq!(w.sent(), [b"FA00014025000;".to_vec(), b"FA;".to_vec()]);
        w.reply(b"FA00007012000;");
        r.tick(t);
        r.tick(t + Duration::from_millis(75));
        assert_eq!(w.sent().len(), 4);
        w.reply(b"FA00014025000;");
        r.tick(t + Duration::from_millis(80));
        let o = r.take_outcomes();
        assert_eq!(o.len(), 1);
        assert_eq!((o[0].status, o[0].attempts), (Status::Done, 2));
    }

    #[test]
    fn commands_fail_after_the_retries() {
        let (w, mut r) = kenwood();
        let t = Instant::now();
        r.set_mode(Mode::Usb, t);
        for s in 0..4 {
            r.tick(t + Duration::from_secs(s));
        }
        assert_eq!(w.sent().len(), 7);
        assert_eq!(r.take_outcomes()[0].status, Status::Failed);
        assert!(!r.busy());
    }

    #[test]
    fn unsupported_commands_are_reported() {
        let (w, mut r) = kenwood();
        let t = Instant::now();
        r.set_mode(Mode::Data, t);
        r.tick(t);
        assert_eq!(r.take_outcomes()[0].status, Status::Unsupported);
        assert_eq!(w.sent(), [b"IF;".to_vec()]);
    }

    #[test]
    fn transverter_offset_both_ways() {
        let (w, mut r) = kenwood();
        r.config.offset = 1_150_000_000;
        r.config.verify = false;
        let t = Instant::now();
        r.set_freq(1_294_050_000, t);
        r.tick(t);
        assert_eq!(w.sent()[0], b"FA00144050000;");
        w.reply(b"FA00144050000;");
        r.tick(t + Duration::from_millis(100));
        assert_eq!(r.state().freq(), Some(1_294_050_000));
    }

    #[test]
    fn transverter_offset_is_verified() {
        let (w, mut r) = kenwood();
        r.config.offset = 1_150_000_000;
        let t = Instant::now();
        r.set_freq(1_294_050_000, t);
        r.tick(t);
        assert_eq!(w.sent()[0], b"FA00144050000;");
        w.reply(b"FA00144050000;");
        r.tick(t + Duration::from_millis(10));
        let o = r.take_outcomes();
        assert_eq!(o[0].status, Status::Done);
        assert_eq!(o[0].command, Command::SetFreq(Vfo::A, 1_294_050_000));
    }

    #[test]
    fn polling_can_be_off() {
        let (w, mut r) = kenwood();
        r.config.polling = false;
        let t = Instant::now();
        r.tick(t);
        assert!(w.sent().is_empty());
        w.reply(b"FB00007012000;");
        assert_eq!(r.tick(t), [Event::Freq(Vfo::B, 7_012_000)]);
    }
}
