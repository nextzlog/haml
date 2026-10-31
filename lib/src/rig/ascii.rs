use crate::rig::protocol::{Protocol, MAX_PENDING};
use crate::{Command, Event, Mode, Vfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    Kenwood,
    Elecraft,
    Yaesu,
    Ftdx101,
    Ftdx5000,
}

impl Dialect {
    pub fn digits(self) -> usize {
        match self {
            Dialect::Kenwood | Dialect::Elecraft => 11,
            Dialect::Yaesu | Dialect::Ftdx101 => 9,
            Dialect::Ftdx5000 => 8,
        }
    }

    pub fn is_yaesu(self) -> bool {
        matches!(self, Dialect::Yaesu | Dialect::Ftdx101 | Dialect::Ftdx5000)
    }

    fn cycle(self) -> &'static [&'static str] {
        match self {
            Dialect::Kenwood | Dialect::Elecraft => &["IF;", "FA;", "IF;", "FB;"],
            Dialect::Yaesu => &["IF;", "OI;", "FT;"],
            Dialect::Ftdx101 | Dialect::Ftdx5000 => &["IF;", "OI;", "FR;"],
        }
    }

    pub fn mode_of(self, code: char) -> Mode {
        match code {
            '1' => Mode::Lsb,
            '2' => Mode::Usb,
            '3' => Mode::Cw,
            '4' => Mode::Fm,
            '5' => Mode::Am,
            '6' | '9' if self == Dialect::Elecraft => Mode::Data,
            '6' => Mode::Rtty,
            '7' => Mode::CwR,
            '8' if self == Dialect::Kenwood => Mode::Rtty,
            '9' if self.is_yaesu() => Mode::RttyR,
            _ => Mode::Other,
        }
    }

    pub fn mode_code(self, mode: Mode) -> Option<char> {
        Some(match mode {
            Mode::Lsb => '1',
            Mode::Usb => '2',
            Mode::Cw => '3',
            Mode::Fm => '4',
            Mode::Am => '5',
            Mode::Data if self == Dialect::Elecraft => '6',
            Mode::Rtty if self != Dialect::Elecraft => '6',
            Mode::CwR => '7',
            Mode::RttyR if self.is_yaesu() => '9',
            _ => return None,
        })
    }
}

pub struct AsciiCat {
    pub dialect: Dialect,
    buf: String,
    step: usize,
    vfo: Vfo,
    cw_reverse: bool,
}

impl AsciiCat {
    pub fn new(dialect: Dialect) -> Self {
        AsciiCat {
            dialect,
            buf: String::new(),
            step: 0,
            vfo: Vfo::A,
            cw_reverse: false,
        }
    }

    fn yaesu(&self) -> bool {
        self.dialect.is_yaesu()
    }

    fn mode(&mut self, mode: Mode, out: &mut Vec<Event>) {
        if !self.yaesu() {
            match mode {
                Mode::Cw => self.cw_reverse = false,
                Mode::CwR => self.cw_reverse = true,
                _ => {}
            }
        }
        out.push(Event::Mode(mode));
    }

    fn kenwood_info(&mut self, reply: &str, out: &mut Vec<Event>) {
        let b = reply.as_bytes();
        if b.len() < 37 {
            return;
        }
        match b[30] {
            b'0' => self.vfo = Vfo::A,
            b'1' => self.vfo = Vfo::B,
            _ => {}
        }
        out.push(Event::Vfo(self.vfo));
        if let Ok(hz) = reply[2..13].parse() {
            out.push(Event::Freq(self.vfo, hz));
        }
        if let Ok(hz) = reply[18..23].trim_start_matches(' ').parse() {
            out.push(Event::RitOffset(hz));
        }
        flag(b[23], Event::Rit, out);
        flag(b[24], Event::Xit, out);
        flag(b[28], Event::Ptt, out);
        flag(b[32], Event::Split, out);
        let m = self.dialect.mode_of(b[29] as char);
        self.mode(m, out);
    }

    fn yaesu_info(&mut self, vfo: Vfo, reply: &str, out: &mut Vec<Event>) {
        let d = self.dialect.digits();
        if reply.len() != 18 + d {
            return;
        }
        if let Ok(hz) = reply[5..5 + d].parse() {
            out.push(Event::Freq(vfo, hz));
        }
        if vfo == self.vfo {
            if let Ok(hz) = reply[5 + d..10 + d].parse() {
                out.push(Event::RitOffset(hz));
            }
            let b = reply.as_bytes();
            flag(b[10 + d], Event::Rit, out);
            flag(b[11 + d], Event::Xit, out);
            let m = self.dialect.mode_of(b[12 + d] as char);
            self.mode(m, out);
        }
    }

    fn select(&mut self, vfo: Vfo, out: &mut Vec<Event>) {
        self.vfo = vfo;
        out.push(Event::Vfo(vfo));
    }

    fn reply(&mut self, reply: &str, out: &mut Vec<Event>) {
        if reply == "?" {
            out.push(Event::Rejected);
            return;
        }
        if !reply.is_ascii() || reply.len() < 2 {
            return;
        }
        let (cmd, rest) = reply.split_at(2);
        let d = self.dialect;
        match cmd {
            "FA" | "FB" if rest.len() == d.digits() && rest.bytes().all(|b| b.is_ascii_digit()) => {
                if let Ok(hz) = rest.parse() {
                    out.push(Event::Freq(if cmd == "FA" { Vfo::A } else { Vfo::B }, hz));
                }
            }
            "IF" if !self.yaesu() => self.kenwood_info(reply, out),
            "IF" => self.yaesu_info(Vfo::A, reply, out),
            "OI" if self.yaesu() => self.yaesu_info(Vfo::B, reply, out),
            "FR" if !self.yaesu() => match rest {
                "0" => self.select(Vfo::A, out),
                "1" => self.select(Vfo::B, out),
                _ => {}
            },
            "FT" if !self.yaesu() => match rest {
                "0" => out.push(Event::Split(self.vfo != Vfo::A)),
                "1" => out.push(Event::Split(self.vfo != Vfo::B)),
                _ => {}
            },
            "FT" if d == Dialect::Yaesu => match rest {
                "0" => self.select(Vfo::A, out),
                "1" => self.select(Vfo::B, out),
                _ => {}
            },
            "FR" if d == Dialect::Ftdx101 => match rest {
                "10" => self.select(Vfo::B, out),
                "00" | "01" | "11" => self.select(Vfo::A, out),
                _ => {}
            },
            "FR" if d == Dialect::Ftdx5000 && !rest.is_empty() => {
                let vfo = if rest.starts_with(['0', '1']) {
                    Vfo::A
                } else {
                    Vfo::B
                };
                self.select(vfo, out);
            }
            "VS" if self.yaesu() => match rest {
                "0" => self.select(Vfo::A, out),
                "1" => self.select(Vfo::B, out),
                _ => {}
            },
            "ST" if self.yaesu() && rest.len() == 1 => flag(rest.as_bytes()[0], Event::Split, out),
            "TX" if self.yaesu() && rest.len() == 1 => match rest {
                "0" => out.push(Event::Ptt(false)),
                "1" | "2" => out.push(Event::Ptt(true)),
                _ => {}
            },
            "RT" | "XT" if rest.len() == 1 => {
                let event = if cmd == "RT" { Event::Rit } else { Event::Xit };
                flag(rest.as_bytes()[0], event, out);
            }
            "MD" => {
                let well_formed = rest.len() == if self.yaesu() { 2 } else { 1 };
                if let Some(c) = rest.chars().last().filter(|_| well_formed) {
                    let m = d.mode_of(c);
                    self.mode(m, out);
                }
            }
            _ => {}
        }
    }
}

fn flag(b: u8, event: fn(bool) -> Event, out: &mut Vec<Event>) {
    match b {
        b'0' => out.push(event(false)),
        b'1' => out.push(event(true)),
        _ => {}
    }
}

fn vfo_digit(vfo: Vfo) -> char {
    if vfo == Vfo::A {
        '0'
    } else {
        '1'
    }
}

impl Protocol for AsciiCat {
    fn encode(&mut self, command: &Command) -> Option<Vec<u8>> {
        let d = self.dialect;
        let yaesu = self.yaesu();
        let s = match *command {
            Command::SetFreq(vfo, hz) => {
                let cmd = if vfo == Vfo::A { "FA" } else { "FB" };
                let digits = d.digits();
                if hz >= 10u64.pow(digits as u32) {
                    return None;
                }
                format!("{cmd}{hz:0digits$};")
            }
            Command::SetMode(vfo, mode) => {
                let code = match mode {
                    Mode::Cw if !yaesu && self.cw_reverse => '7',
                    _ => d.mode_code(mode)?,
                };
                if yaesu {
                    format!("MD{}{code};", vfo_digit(vfo))
                } else {
                    format!("MD{code};")
                }
            }
            Command::SelectVfo(vfo) => {
                self.vfo = vfo;
                let v = vfo_digit(vfo);
                if yaesu {
                    format!("VS{v};")
                } else {
                    format!("FR{v};FT{v};")
                }
            }
            Command::Split(on) => match d {
                Dialect::Kenwood | Dialect::Elecraft => {
                    let tx = if on { self.vfo.other() } else { self.vfo };
                    format!("FT{};", vfo_digit(tx))
                }
                Dialect::Yaesu | Dialect::Ftdx101 => format!("ST{};", on as u8),
                Dialect::Ftdx5000 => format!("FT{};", if on { 3 } else { 2 }),
            },
            Command::Ptt(on) => match (yaesu, on) {
                (true, _) => format!("TX{};", on as u8),
                (false, true) => "TX;".into(),
                (false, false) => "RX;".into(),
            },
            Command::Rit(on) => format!("RT{};", on as u8),
            Command::Xit(on) => format!("XT{};", on as u8),
            Command::ClearRit => "RC;".into(),
            Command::RitOffset(hz) => {
                let n = hz.unsigned_abs();
                if n > 9999 {
                    return None;
                }
                let dir = if hz < 0 { "RD" } else { "RU" };
                match d {
                    _ if n == 0 => "RC;".into(),
                    Dialect::Elecraft => format!("RO{}{n:04};", if hz < 0 { '-' } else { '+' }),
                    Dialect::Kenwood => format!("RC;{dir}{n:05};"),
                    _ => format!("RC;{dir}{n:04};"),
                }
            }
            Command::Transceive(on) => match (yaesu, on) {
                (_, false) => "AI0;".into(),
                (true, true) => "AI1;".into(),
                (false, true) => "AI2;".into(),
            },
        };
        Some(s.into_bytes())
    }

    fn poll(&mut self) -> Vec<u8> {
        let cycle = self.dialect.cycle();
        let request = cycle[self.step % cycle.len()];
        self.step = (self.step + 1) % cycle.len();
        request.as_bytes().to_vec()
    }

    fn decode(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find(';') {
            let reply: String = self.buf.drain(..=end).collect();
            self.reply(reply.trim_end_matches(';').trim(), &mut out);
        }
        if self.buf.len() > MAX_PENDING {
            self.buf.clear();
        }
        out
    }

    fn acknowledges(&self) -> bool {
        false
    }

    fn verify(&mut self, command: &Command) -> Option<Vec<u8>> {
        let yaesu = self.yaesu();
        let s = match *command {
            Command::SetFreq(Vfo::A, _) => "FA;".to_string(),
            Command::SetFreq(Vfo::B, _) => "FB;".to_string(),
            Command::SetMode(vfo, _) if yaesu => format!("MD{};", vfo_digit(vfo)),
            Command::SetMode(..) => "MD;".to_string(),
            Command::SelectVfo(_) if yaesu => "VS;".to_string(),
            Command::SelectVfo(_) => "FR;".to_string(),
            Command::Split(_) if self.dialect == Dialect::Ftdx5000 => return None,
            Command::Split(_) if yaesu => "ST;".to_string(),
            Command::Ptt(_) if yaesu => "TX;".to_string(),
            Command::Split(_) | Command::Ptt(_) => "IF;".to_string(),
            Command::Rit(_) => "RT;".to_string(),
            Command::Xit(_) => "XT;".to_string(),
            _ => return None,
        };
        Some(s.into_bytes())
    }

    fn reset(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn kenwood_if(hz: u64, mode: char, vfo: char) -> String {
        format!("IF{hz:011}     +0000000000{mode}{vfo}000000;")
    }

    fn yaesu_if(cmd: &str, d: usize, hz: u64, mode: char) -> String {
        format!("{cmd}001{hz:0d$}+000000{mode}00000;")
    }

    fn enc(p: &mut AsciiCat, c: Command) -> String {
        String::from_utf8(p.encode(&c).unwrap()).unwrap()
    }

    #[test]
    fn frequency_needs_the_dialects_digits() {
        let mut k = AsciiCat::new(Dialect::Kenwood);
        assert!(k
            .decode(b"FA14025000;FB0001402500000;FA0001402500x;")
            .is_empty());
        let mut y = AsciiCat::new(Dialect::Ftdx5000);
        assert_eq!(
            y.decode(b"FA014025000;FA14025000;"),
            [Event::Freq(Vfo::A, 14_025_000)]
        );
        assert!(y.encode(&Command::SetFreq(Vfo::A, 100_000_000)).is_none());
    }

    #[test]
    fn kenwood_info_reports_everything() {
        let mut k = AsciiCat::new(Dialect::Kenwood);
        let polls: Vec<Vec<u8>> = (0..4).map(|_| k.poll()).collect();
        assert_eq!(polls, [&b"IF;"[..], b"FA;", b"IF;", b"FB;"]);
        let mut s = kenwood_if(14_025_000, '3', '1');
        s.replace_range(18..25, "-012010");
        s.replace_range(32..33, "1");
        assert_eq!(
            k.decode(s.as_bytes()),
            [
                Event::Vfo(Vfo::B),
                Event::Freq(Vfo::B, 14_025_000),
                Event::RitOffset(-120),
                Event::Rit(true),
                Event::Xit(false),
                Event::Ptt(false),
                Event::Split(true),
                Event::Mode(Mode::Cw),
            ]
        );
        assert!(k.decode(b"IF00014025000;IF;;;").is_empty());
    }

    #[test]
    fn kenwood_mode_codes_and_cw_reverse() {
        let mut k = AsciiCat::new(Dialect::Kenwood);
        assert_eq!(
            k.decode(b"MD8;MD6;MD9;"),
            [
                Event::Mode(Mode::Rtty),
                Event::Mode(Mode::Rtty),
                Event::Mode(Mode::Other)
            ]
        );
        assert!(k.decode(b"MD;MD12;").is_empty());
        assert_eq!(enc(&mut k, Command::SetMode(Vfo::A, Mode::Cw)), "MD3;");
        k.decode(b"MD7;");
        assert_eq!(enc(&mut k, Command::SetMode(Vfo::A, Mode::Cw)), "MD7;");
        k.decode(b"MD2;");
        assert_eq!(enc(&mut k, Command::SetMode(Vfo::A, Mode::Cw)), "MD7;");
        k.decode(kenwood_if(7_012_000, '3', '0').as_bytes());
        assert_eq!(enc(&mut k, Command::SetMode(Vfo::A, Mode::Cw)), "MD3;");
        assert!(k.encode(&Command::SetMode(Vfo::A, Mode::Data)).is_none());
    }

    #[test]
    fn elecraft_has_data_modes() {
        let mut e = AsciiCat::new(Dialect::Elecraft);
        assert_eq!(enc(&mut e, Command::SetMode(Vfo::A, Mode::Data)), "MD6;");
        assert_eq!(e.decode(b"MD6;MD9;"), [Event::Mode(Mode::Data); 2]);
        assert_eq!(enc(&mut e, Command::RitOffset(-50)), "RO-0050;");
        assert_eq!(enc(&mut e, Command::Split(true)), "FT1;");
        assert_eq!(enc(&mut e, Command::Split(false)), "FT0;");
        assert_eq!(enc(&mut e, Command::Transceive(true)), "AI2;");
    }

    #[test]
    fn kenwood_commands() {
        let mut k = AsciiCat::new(Dialect::Kenwood);
        assert_eq!(
            enc(&mut k, Command::SetFreq(Vfo::A, 14_025_000)),
            "FA00014025000;"
        );
        assert_eq!(enc(&mut k, Command::SelectVfo(Vfo::B)), "FR1;FT1;");
        assert_eq!(enc(&mut k, Command::Split(true)), "FT0;");
        assert_eq!(enc(&mut k, Command::Ptt(true)), "TX;");
        assert_eq!(enc(&mut k, Command::Ptt(false)), "RX;");
        assert_eq!(enc(&mut k, Command::RitOffset(30)), "RC;RU00030;");
        assert_eq!(enc(&mut k, Command::RitOffset(0)), "RC;");
        assert_eq!(k.decode(b"FT0;"), [Event::Split(true)]);
        assert_eq!(
            k.decode(b"FR0;FT0;"),
            [Event::Vfo(Vfo::A), Event::Split(false)]
        );
    }

    #[test]
    fn yaesu_polls_info_and_vfo() {
        let mut y = AsciiCat::new(Dialect::Yaesu);
        let polls: Vec<Vec<u8>> = (0..4).map(|_| y.poll()).collect();
        assert_eq!(polls, [&b"IF;"[..], b"OI;", b"FT;", b"IF;"]);
        assert_eq!(
            y.decode(yaesu_if("IF", 9, 7_012_000, '3').as_bytes()),
            [
                Event::Freq(Vfo::A, 7_012_000),
                Event::RitOffset(0),
                Event::Rit(false),
                Event::Xit(false),
                Event::Mode(Mode::Cw)
            ]
        );
        assert_eq!(
            y.decode(yaesu_if("OI", 9, 14_250_000, '2').as_bytes()),
            [Event::Freq(Vfo::B, 14_250_000)]
        );
        assert_eq!(y.decode(b"FT1;"), [Event::Vfo(Vfo::B)]);
        assert_eq!(
            y.decode(yaesu_if("OI", 9, 14_250_000, '2').as_bytes())
                .last(),
            Some(&Event::Mode(Mode::Usb))
        );
        assert!(y.decode(b"FT2;FT;").is_empty());
        assert!(y
            .decode(yaesu_if("IF", 8, 7_012_000, '3').as_bytes())
            .is_empty());
        assert_eq!(
            y.decode(b"ST1;TX1;TX0;"),
            [Event::Split(true), Event::Ptt(true), Event::Ptt(false)]
        );
    }

    #[test]
    fn yaesu_commands() {
        let mut y = AsciiCat::new(Dialect::Yaesu);
        assert_eq!(
            enc(&mut y, Command::SetFreq(Vfo::B, 7_012_000)),
            "FB007012000;"
        );
        assert_eq!(enc(&mut y, Command::SetMode(Vfo::A, Mode::Usb)), "MD02;");
        assert_eq!(enc(&mut y, Command::SetMode(Vfo::B, Mode::Lsb)), "MD11;");
        assert!(y.encode(&Command::SetMode(Vfo::A, Mode::Data)).is_none());
        assert_eq!(enc(&mut y, Command::Split(true)), "ST1;");
        assert_eq!(enc(&mut y, Command::Ptt(true)), "TX1;");
        assert_eq!(enc(&mut y, Command::RitOffset(-20)), "RC;RD0020;");
        assert_eq!(enc(&mut y, Command::SelectVfo(Vfo::B)), "VS1;");
        let mut y5 = AsciiCat::new(Dialect::Ftdx5000);
        assert_eq!(enc(&mut y5, Command::Split(true)), "FT3;");
        assert_eq!(
            y5.decode(b"FR4;FR0;FR;"),
            [Event::Vfo(Vfo::B), Event::Vfo(Vfo::A)]
        );
    }

    #[test]
    fn ftdx101_follows_the_receiving_vfo() {
        let mut y = AsciiCat::new(Dialect::Ftdx101);
        assert_eq!(y.decode(b"FR10;"), [Event::Vfo(Vfo::B)]);
        for p in ["00", "01", "11"] {
            assert_eq!(
                y.decode(format!("FR{p};").as_bytes()),
                [Event::Vfo(Vfo::A)],
                "{p}"
            );
        }
        for bad in ["FR;", "FR1;", "FR0;", "FR2;", "FR100;", "FRx0;", "FT1;"] {
            assert!(y.decode(bad.as_bytes()).is_empty(), "{bad}");
        }
    }

    #[test]
    fn yaesu_other_modes_are_other() {
        for (dialect, d) in [
            (Dialect::Yaesu, 9),
            (Dialect::Ftdx5000, 8),
            (Dialect::Ftdx101, 9),
        ] {
            let mut y = AsciiCat::new(dialect);
            for code in ['8', 'A', 'B', 'C', '0'] {
                assert_eq!(
                    y.decode(format!("MD0{code};").as_bytes()),
                    [Event::Mode(Mode::Other)]
                );
                let r = y.decode(yaesu_if("IF", d, 7_012_000, code).as_bytes());
                assert_eq!(r.last(), Some(&Event::Mode(Mode::Other)));
            }
            assert_eq!(y.decode(b"MD09;"), [Event::Mode(Mode::RttyR)]);
        }
    }

    #[test]
    fn question_mark_is_rejected() {
        for d in [
            Dialect::Kenwood,
            Dialect::Elecraft,
            Dialect::Yaesu,
            Dialect::Ftdx5000,
            Dialect::Ftdx101,
        ] {
            let mut p = AsciiCat::new(d);
            assert_eq!(p.decode(b"?;"), [Event::Rejected]);
            assert_eq!(p.decode(b" ? ;"), [Event::Rejected]);
            assert!(p.decode(b"?").is_empty());
            assert_eq!(p.decode(b";?;"), [Event::Rejected, Event::Rejected]);
            assert!(p.decode(b"??;").is_empty());
        }
    }

    #[test]
    fn partial_reads_and_bounded_buffer() {
        let mut y = AsciiCat::new(Dialect::Yaesu);
        assert!(y.decode(b"FA0140").is_empty());
        assert_eq!(
            y.decode(b"25000;MD03;"),
            [Event::Freq(Vfo::A, 14_025_000), Event::Mode(Mode::Cw)]
        );
        let mut k = AsciiCat::new(Dialect::Kenwood);
        assert!(k.decode(&[b'F'; 400]).is_empty());
        assert!(k.buf.len() <= MAX_PENDING);
        assert_eq!(
            k.decode(b"FA00014025000;"),
            [Event::Freq(Vfo::A, 14_025_000)]
        );
    }

    #[test]
    fn verify_requests() {
        let mut k = AsciiCat::new(Dialect::Kenwood);
        assert_eq!(k.verify(&Command::SetFreq(Vfo::B, 1)).unwrap(), b"FB;");
        assert_eq!(
            k.verify(&Command::SetMode(Vfo::A, Mode::Cw)).unwrap(),
            b"MD;"
        );
        let mut y = AsciiCat::new(Dialect::Yaesu);
        assert_eq!(
            y.verify(&Command::SetMode(Vfo::B, Mode::Cw)).unwrap(),
            b"MD1;"
        );
        assert!(y.verify(&Command::ClearRit).is_none());
    }
}
