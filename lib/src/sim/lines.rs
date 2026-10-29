use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::morse::{self, Timing};
use crate::ptt::{Line, Lines};

#[derive(Debug, Clone, PartialEq)]
pub struct SimState {
    pub key: bool,
    pub ptt: bool,
    pub edges: Vec<(Line, bool, Duration)>,
    pub keyed: String,
}

struct SimInner {
    key_line: Line,
    key: bool,
    ptt: bool,
    edges: VecDeque<(Line, bool, Duration)>,
    decoder: morse::Decoder,
    clock: Box<dyn Fn() -> Duration + Send>,
}

#[derive(Clone)]
pub struct SimLines(Arc<Mutex<SimInner>>);

const SIM_KEPT: usize = 200;

pub const SIM_EDGES: usize = 4096;

impl SimLines {
    pub fn new(key_line: Line, timing: Timing) -> Self {
        let start = Instant::now();
        SimLines::with_clock(key_line, timing, move || start.elapsed())
    }

    pub fn with_clock(
        key_line: Line,
        timing: Timing,
        clock: impl Fn() -> Duration + Send + 'static,
    ) -> Self {
        SimLines(Arc::new(Mutex::new(SimInner {
            key_line,
            key: false,
            ptt: false,
            edges: VecDeque::new(),
            decoder: morse::Decoder::with_unit(SIM_KEPT, timing.dot()),
            clock: Box::new(clock),
        })))
    }

    pub fn state(&self) -> SimState {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = (g.clock)();
        g.decoder.settle(now);
        SimState {
            key: g.key,
            ptt: g.ptt,
            edges: g.edges.iter().copied().collect(),
            keyed: g.decoder.text().to_string(),
        }
    }

    pub fn status(&self) -> String {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = (g.clock)();
        g.decoder.settle(now);
        let on = |b| if b { "ON" } else { "OFF" };
        format!(
            "KEY {}, PTT {}, keyed: {}",
            on(g.key),
            on(g.ptt),
            g.decoder.text().trim_end()
        )
    }
}

impl Lines for SimLines {
    fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = (g.clock)();
        if line == g.key_line {
            if g.key != on {
                g.decoder.key(on, now);
            }
            g.key = on;
        } else {
            g.ptt = on;
        }
        if g.edges.len() == SIM_EDGES {
            g.edges.pop_front();
        }
        g.edges.push_back((line, on, now));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simulator_keeps_only_recent_edges() {
        let mut sim = SimLines::with_clock(Line::Dtr, Timing::default(), || Duration::ZERO);
        for i in 0..SIM_EDGES + 10 {
            sim.set(Line::Rts, i % 2 == 0).unwrap();
        }
        let st = sim.state();
        assert_eq!(st.edges.len(), SIM_EDGES);
        assert!(st.edges[0].1);
        assert!(sim.status().starts_with("KEY OFF, PTT OFF"));
    }
}
