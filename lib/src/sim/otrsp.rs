use crate::transport::{Emulated, Emulator, Transport};

pub const NAME: &str = "HaML OTRSP simulator";
const KEPT: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub tx: u8,
    pub rx: String,
    pub lines: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            tx: 1,
            rx: "1".into(),
            lines: Vec::new(),
        }
    }
}

#[derive(Default)]
struct Inner {
    state: State,
    input: Vec<u8>,
    out: Vec<u8>,
}

impl Inner {
    fn line(&mut self, line: &str) {
        let s = &mut self.state;
        s.lines.push(line.into());
        if s.lines.len() > KEPT {
            s.lines.remove(0);
        }
        if let Some(r) = line.strip_prefix("TX") {
            if let Ok(n @ 1..=2) = r.parse() {
                s.tx = n;
            }
        } else if let Some(r) = line.strip_prefix("RX") {
            if matches!(r.as_bytes().first(), Some(b'1' | b'2')) {
                s.rx = r.into();
            }
        } else if line == "?NAME" {
            self.out.extend(format!("{NAME}\r").bytes());
        }
    }
}

impl Emulator for Inner {
    fn receive(&mut self, bytes: &[u8]) {
        self.input.extend_from_slice(bytes);
        while let Some(i) = self.input.iter().position(|b| *b == b'\r') {
            let line: Vec<u8> = self.input.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..i]).trim().to_uppercase();
            self.line(&text);
        }
    }

    fn send(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.out);
    }
}

#[derive(Clone)]
pub struct OtrspSim(Emulated<Inner>);

impl Default for OtrspSim {
    fn default() -> Self {
        OtrspSim(Emulated::new(Inner::default()))
    }
}

impl OtrspSim {
    pub fn new() -> Self {
        OtrspSim::default()
    }

    pub fn transport(&self) -> impl Transport + Send + use<> {
        self.0.clone()
    }

    pub fn state(&self) -> State {
        self.0.with(|g| g.state.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_set_the_state_and_name_is_answered() {
        let sim = OtrspSim::new();
        let mut p = sim.transport();
        p.write(b"TX2\rRX").unwrap();
        assert_eq!(sim.state().tx, 2);
        p.write(b"1S\rTX9\rAUX12\r?name\r").unwrap();
        let s = sim.state();
        assert_eq!((s.tx, s.rx.as_str()), (2, "1S"));
        assert_eq!(s.lines, ["TX2", "RX1S", "TX9", "AUX12", "?NAME"]);
        let mut got = Vec::new();
        p.read(&mut got).unwrap();
        assert_eq!(got, format!("{NAME}\r").as_bytes());
        for _ in 0..30 {
            p.write(b"RX2\r").unwrap();
        }
        assert_eq!(sim.state().lines.len(), KEPT);
        assert_eq!(sim.state().rx, "2");
    }
}
