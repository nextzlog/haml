use std::time::{Duration, Instant};

use crate::so2r::Rx;
use crate::tx::{Signal, TxCommand, TxEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxMode {
    Focus,
    Both,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Alternate {
    signals: [Signal; 2],
    next: u8,
    interval: Duration,
    due: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    focus: u8,
    rx_mode: RxMode,
    auto_rx: bool,
    on_air: Option<u8>,
    alternate: Option<Alternate>,
}

impl Default for Operator {
    fn default() -> Self {
        Operator {
            focus: 1,
            rx_mode: RxMode::Focus,
            auto_rx: true,
            on_air: None,
            alternate: None,
        }
    }
}

impl Operator {
    pub fn focus(&self) -> u8 {
        self.focus
    }

    pub fn on_air(&self) -> Option<u8> {
        self.on_air
    }

    pub fn rx(&self) -> Rx {
        match self.rx_mode {
            RxMode::Focus => Rx::of(self.focus),
            RxMode::Both => Rx::Both,
            RxMode::Other => Rx::other(self.focus),
        }
    }

    pub fn set_auto_rx(&mut self, on: bool) {
        self.auto_rx = on;
    }

    pub fn set_focus(&mut self, radio: u8) -> Vec<TxCommand> {
        self.focus = radio;
        vec![TxCommand::Focus(radio), TxCommand::Listen(self.listening())]
    }

    pub fn toggle_focus(&mut self) -> Vec<TxCommand> {
        self.set_focus(if self.focus == 2 { 1 } else { 2 })
    }

    pub fn set_rx(&mut self, mode: RxMode) -> Vec<TxCommand> {
        self.rx_mode = mode;
        vec![TxCommand::Listen(self.listening())]
    }

    fn listening(&self) -> Rx {
        match self.on_air {
            Some(radio) if self.auto_rx => Rx::other(radio),
            _ => self.rx(),
        }
    }

    pub fn send(&self, signal: Signal) -> TxCommand {
        TxCommand::Send {
            radio: self.focus,
            signal,
        }
    }

    pub fn start_alternate(&mut self, signals: [Signal; 2], interval: Duration) -> Vec<TxCommand> {
        let first = signals[0].clone();
        self.alternate = Some(Alternate {
            signals,
            next: 2,
            interval,
            due: None,
        });
        vec![TxCommand::Send {
            radio: 1,
            signal: first,
        }]
    }

    pub fn stop_alternate(&mut self) {
        self.alternate = None;
    }

    pub fn alternating(&self) -> bool {
        self.alternate.is_some()
    }

    pub fn on_event(&mut self, event: &TxEvent, now: Instant) -> Vec<TxCommand> {
        match event {
            TxEvent::Started { radio, .. } => {
                self.on_air = Some(*radio);
                if self.auto_rx {
                    return vec![TxCommand::Listen(Rx::other(*radio))];
                }
            }
            TxEvent::Finished { .. } => {
                self.on_air = None;
                if let Some(a) = self.alternate.as_mut() {
                    a.due = Some(now + a.interval);
                }
                if self.auto_rx {
                    return vec![TxCommand::Listen(self.rx())];
                }
            }
            TxEvent::Stopped => {
                self.on_air = None;
                self.alternate = None;
            }
            _ => {}
        }
        Vec::new()
    }

    pub fn tick(&mut self, now: Instant) -> Vec<TxCommand> {
        let Some(a) = self.alternate.as_mut() else {
            return Vec::new();
        };
        match a.due {
            Some(t) if now >= t && self.on_air.is_none() => {
                a.due = None;
                let radio = a.next;
                a.next = if radio == 2 { 1 } else { 2 };
                vec![TxCommand::Send {
                    radio,
                    signal: a.signals[radio as usize - 1].clone(),
                }]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cq(s: &str) -> Signal {
        Signal::Cw(s.into())
    }

    #[test]
    fn focus_and_receiver_follow_the_operator() {
        let mut o = Operator::default();
        assert_eq!(
            o.toggle_focus(),
            [TxCommand::Focus(2), TxCommand::Listen(Rx::Rig2)]
        );
        assert_eq!(o.set_rx(RxMode::Both), [TxCommand::Listen(Rx::Both)]);
        assert_eq!(o.set_rx(RxMode::Other), [TxCommand::Listen(Rx::Rig1)]);
        assert_eq!(
            o.send(cq("TU")),
            TxCommand::Send {
                radio: 2,
                signal: cq("TU")
            }
        );
    }

    #[test]
    fn the_receiver_moves_to_the_other_radio_while_transmitting() {
        let mut o = Operator::default();
        let now = Instant::now();
        let started = TxEvent::Started {
            radio: 1,
            signal: cq("CQ"),
        };
        assert_eq!(o.on_event(&started, now), [TxCommand::Listen(Rx::Rig2)]);
        assert_eq!(o.on_air(), Some(1));
        let done = TxEvent::Finished { radio: 1 };
        assert_eq!(o.on_event(&done, now), [TxCommand::Listen(Rx::Rig1)]);
        o.set_auto_rx(false);
        assert!(o.on_event(&started, now).is_empty());
    }

    #[test]
    fn alternate_cq_takes_turns_after_each_message() {
        let mut o = Operator::default();
        let t = Instant::now();
        let gap = Duration::from_millis(500);
        let first = o.start_alternate([cq("CQ A"), cq("CQ B")], gap);
        assert_eq!(
            first,
            [TxCommand::Send {
                radio: 1,
                signal: cq("CQ A")
            }]
        );
        o.on_event(
            &TxEvent::Started {
                radio: 1,
                signal: cq("CQ A"),
            },
            t,
        );
        assert!(o.tick(t + gap * 4).is_empty(), "never while on the air");
        o.on_event(&TxEvent::Finished { radio: 1 }, t);
        assert!(o.tick(t + gap / 2).is_empty());
        assert_eq!(
            o.tick(t + gap),
            [TxCommand::Send {
                radio: 2,
                signal: cq("CQ B")
            }]
        );
        o.on_event(&TxEvent::Stopped, t);
        assert!(!o.alternating());
    }
}
