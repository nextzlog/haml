use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::morse::{self, Timing};

pub struct SimKeyer {
    timing: Timing,
    until: Option<Instant>,
    pub ptt: Arc<AtomicBool>,
    pub sent: Arc<Mutex<Vec<String>>>,
}

impl SimKeyer {
    pub fn new(timing: Timing) -> Self {
        SimKeyer {
            timing,
            until: None,
            ptt: Arc::default(),
            sent: Arc::default(),
        }
    }

    pub fn ptt(&self) -> bool {
        self.ptt.load(Ordering::SeqCst)
    }
}

impl Keyer for SimKeyer {
    fn caps(&self) -> KeyerCaps {
        KeyerCaps {
            ptt: true,
            ..KeyerCaps::default()
        }
    }

    fn send(&mut self, text: &str) {
        let len: Duration = morse::total(&morse::keying(text, &self.timing));
        let from = self.until.filter(|t| *t > Instant::now());
        self.until = Some(from.unwrap_or_else(Instant::now) + len);
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.to_string());
    }

    fn abort(&mut self) {
        self.until = Some(Instant::now());
    }

    fn set_wpm(&mut self, wpm: u32) {
        self.timing.wpm = wpm.clamp(morse::MIN_WPM, morse::MAX_WPM);
    }

    fn wpm(&self) -> u32 {
        self.timing.wpm
    }

    fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
        self.set_wpm(timing.wpm);
    }

    fn set_ptt(&mut self, on: bool) {
        self.ptt.store(on, Ordering::SeqCst);
    }

    fn busy(&self) -> bool {
        self.until.is_some_and(|t| t > Instant::now())
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        match self.until {
            Some(t) if t <= Instant::now() => {
                self.until = None;
                vec![KeyerEvent::Finished]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_finishes_after_abort() {
        let mut k = SimKeyer::new(Timing::default());
        k.send("CQ TEST");
        assert!(k.busy());
        assert!(k.poll().is_empty());
        k.abort();
        assert!(!k.busy());
        assert_eq!(k.poll(), vec![KeyerEvent::Finished]);
        assert!(k.poll().is_empty());
        assert_eq!(*k.sent.lock().unwrap(), ["CQ TEST"]);
        k.set_wpm(200);
        assert_eq!(k.wpm(), morse::MAX_WPM);
        k.set_ptt(true);
        assert!(k.ptt());
    }
}
