use std::time::{Duration, Instant};

use haml::sim::{Impairments, Rng, SimTransport, Simulator};
use haml::{Cat, Command, Config, Family, Mode, Model, Protocol, Status, Vfo, MODELS};

const MS: Duration = Duration::from_millis(1);

struct Bench {
    sim: Simulator,
    rig: Cat<SimTransport>,
    now: Instant,
}

impl Bench {
    fn new(family: Family, imp: Impairments, seed: u64) -> Self {
        let sim = Simulator::new(family, imp, seed);
        let config = Config {
            interval: Duration::from_millis(10),
            timeout: Duration::from_millis(150),
            retries: 4,
            ..Config::default()
        };
        let rig = Cat::new(family.protocol(), sim.transport(), config);
        Bench {
            sim,
            rig,
            now: Instant::now(),
        }
    }

    fn run(&mut self, ms: u64) {
        for _ in 0..ms {
            self.now += MS;
            self.sim.advance(self.now);
            self.rig.tick(self.now);
        }
    }

    fn settle(&mut self) {
        for _ in 0..10_000 {
            if !self.rig.busy() {
                return;
            }
            self.run(1);
        }
        panic!("commands never finished");
    }

    fn send(&mut self, c: Command) -> Status {
        self.rig.send(c, self.now);
        self.settle();
        let o = self.rig.take_outcomes();
        assert_eq!(o.len(), 1, "{c:?}");
        o[0].status
    }
}

fn freq_for(m: &Model, khz: u64) -> u64 {
    let base = m.min_hz.max(14_000_000);
    base + khz * 1000
}

fn applied(b: &Bench, c: Command) -> bool {
    let r = b.sim.radio();
    match c {
        Command::SetFreq(v, hz) => r.freq_of(v) == hz,
        Command::SetMode(_, m) => r.mode == m,
        Command::SelectVfo(v) => r.vfo == v,
        Command::Split(on) => r.split == on,
        Command::Ptt(on) => r.ptt == on,
        Command::Rit(on) => r.rit == on,
        Command::Xit(on) => r.xit == on,
        Command::RitOffset(hz) => r.rit_offset == hz,
        Command::ClearRit => r.rit_offset == 0,
        Command::Transceive(on) => r.auto_info == on,
    }
}

#[test]
fn every_model_round_trips_on_a_clean_link() {
    for m in MODELS {
        let mut b = Bench::new(m.family, Impairments::clean(), 1);
        b.run(100);
        let commands = [
            Command::SetFreq(Vfo::A, freq_for(m, 25)),
            Command::SetMode(Vfo::A, Mode::Usb),
            Command::SetMode(Vfo::A, Mode::Cw),
            Command::Rit(true),
            Command::RitOffset(120),
            Command::RitOffset(-40),
            Command::ClearRit,
            Command::Xit(true),
            Command::Ptt(true),
            Command::Ptt(false),
            Command::SelectVfo(Vfo::B),
            Command::SetFreq(Vfo::B, freq_for(m, 50)),
            Command::Split(true),
            Command::Split(false),
            Command::SelectVfo(Vfo::A),
            Command::Transceive(true),
        ];
        for c in commands {
            match b.send(c) {
                Status::Unsupported => {}
                Status::Done | Status::Sent => assert!(applied(&b, c), "{} {c:?}", m.name),
                s => panic!("{} {c:?} {s:?}", m.name),
            }
        }
        b.run(500);
        let r = b.sim.radio();
        let s = *b.rig.state();
        assert_eq!(s.freq_of(r.vfo), Some(r.freq()), "{}", m.name);
        assert_eq!(s.mode, Some(r.mode), "{}", m.name);
    }
}

#[test]
fn every_model_reports_front_panel_changes_by_polling() {
    for m in MODELS {
        let mut b = Bench::new(m.family, Impairments::clean(), 2);
        b.run(50);
        b.sim.turn_dial(freq_for(m, 77));
        b.sim.press_mode(Mode::Lsb);
        b.run(300);
        assert_eq!(b.rig.state().freq(), Some(freq_for(m, 77)), "{}", m.name);
        assert_eq!(b.rig.state().mode, Some(Mode::Lsb), "{}", m.name);
    }
}

fn families() -> [(&'static str, Family); 6] {
    use haml::Dialect::*;
    [
        ("icom", Family::Icom { address: 0x94 }),
        ("kenwood", Family::Ascii(Kenwood)),
        ("elecraft", Family::Ascii(Elecraft)),
        ("yaesu", Family::Ascii(Yaesu)),
        ("ftdx101", Family::Ascii(Ftdx101)),
        ("yaesu binary", Family::YaesuBinary),
    ]
}

#[test]
fn harsh_links_recover() {
    for (name, family) in families() {
        let mut b = Bench::new(family, Impairments::harsh(), 3);
        let mut rng = Rng::new(9);
        let mut done = 0;
        let n = 60;
        for _ in 0..n {
            let hz = 14_000_000 + rng.below(350) * 1000;
            if b.send(Command::SetFreq(Vfo::A, hz)) == Status::Done {
                done += 1;
                assert_eq!(b.sim.radio().freq_a, hz, "{name}");
            }
        }
        assert!(done * 100 >= n * 90, "{name}: {done}/{n}");
        assert!(b.rig.stats().retries > 0, "{name}");
        let target = b.sim.radio().freq_a;
        b.sim.set_impairments(Impairments::clean());
        b.run(500);
        assert_eq!(b.rig.state().freq_a, Some(target), "{name}");
    }
}

#[test]
fn icom_echo_and_collisions_are_survived() {
    let imp = Impairments {
        echo: true,
        collision: 0.15,
        ..Impairments::clean()
    };
    let mut b = Bench::new(Family::Icom { address: 0x94 }, imp, 4);
    for k in 0..30 {
        let hz = 7_000_000 + k * 1000;
        assert_eq!(b.send(Command::SetFreq(Vfo::A, hz)), Status::Done);
        assert_eq!(b.sim.radio().freq_a, hz);
    }
    assert!(b.sim.stats().collisions > 0);
}

#[test]
fn partial_reads_one_byte_at_a_time() {
    let imp = Impairments {
        chunk: 1,
        ..Impairments::clean()
    };
    for (name, family) in families() {
        let mut b = Bench::new(family, imp, 5);
        assert_eq!(
            b.send(Command::SetFreq(Vfo::A, 21_012_340)),
            Status::Done,
            "{name}"
        );
        b.run(300);
        assert_eq!(b.rig.state().freq_a, Some(21_012_340), "{name}");
    }
}

#[test]
fn transceive_without_polling() {
    let mut b = Bench::new(Family::Icom { address: 0x94 }, Impairments::clean(), 6);
    b.rig.config.polling = false;
    b.sim.with(|r| r.auto_info = true);
    b.sim.turn_dial(14_100_000);
    b.sim.press_mode(Mode::Usb);
    b.run(20);
    assert_eq!(b.rig.state().freq_a, Some(14_100_000));
    assert_eq!(b.rig.state().mode, Some(Mode::Usb));

    let mut b = Bench::new(
        Family::Ascii(haml::Dialect::Kenwood),
        Impairments::clean(),
        6,
    );
    b.rig.config.polling = false;
    assert_eq!(b.send(Command::Transceive(true)), Status::Sent);
    b.sim.turn_dial(3_510_000);
    b.run(20);
    assert_eq!(b.rig.state().freq_a, Some(3_510_000));
    assert_eq!(b.sim.received().last().unwrap(), b"AI2;");
}

#[test]
fn unsolicited_messages_interleave_with_polls() {
    let imp = Impairments {
        transceive: Some(Duration::from_millis(25)),
        chunk: 4,
        ..Impairments::clean()
    };
    let mut b = Bench::new(Family::Icom { address: 0x94 }, imp, 7);
    for k in 0..20 {
        assert_eq!(
            b.send(Command::SetFreq(Vfo::A, 50_100_000 + k)),
            Status::Done
        );
    }
    assert!(b.sim.stats().unsolicited > 10);
}

#[test]
fn a_dead_rig_fails_and_is_retried() {
    let mut b = Bench::new(Family::Ascii(haml::Dialect::Yaesu), Impairments::clean(), 8);
    b.sim.with(|r| r.power = false);
    assert_eq!(b.send(Command::SetMode(Vfo::A, Mode::Usb)), Status::Failed);
    assert_eq!(b.rig.stats().retries, 4);
    b.sim.with(|r| r.power = true);
    assert_eq!(b.send(Command::SetMode(Vfo::A, Mode::Usb)), Status::Done);
}

#[test]
fn the_simulator_is_deterministic() {
    let trace = |seed| {
        let mut b = Bench::new(
            Family::Ascii(haml::Dialect::Kenwood),
            Impairments::harsh(),
            seed,
        );
        for k in 0..10 {
            b.send(Command::SetFreq(Vfo::A, 7_000_000 + k * 100));
        }
        (*b.rig.stats(), b.sim.stats())
    };
    assert_eq!(trace(11), trace(11));
    assert_ne!(trace(11), trace(12));
}

#[test]
fn decoders_never_panic_on_noise() {
    let mut rng = Rng::new(0x2545_F491_4F6C_DD1D);
    let heads: [&[u8]; 10] = [
        b"IF",
        b"OI",
        b"FA",
        b"FB",
        b"FT",
        b"FR",
        b"MD",
        b"?",
        b"RT",
        &[0xFE, 0xFE],
    ];
    let mut protocols: Vec<Box<dyn Protocol>> =
        families().iter().map(|(_, f)| f.protocol()).collect();
    for round in 0..20_000 {
        let mut bytes = heads[rng.below(heads.len() as u64) as usize].to_vec();
        for _ in 0..rng.below(48) {
            let r = rng.next_u64();
            bytes.push(match r % 4 {
                0 => b'0' + (r >> 8) as u8 % 10,
                1 => 0x80 | (r >> 8) as u8,
                _ => (r >> 8) as u8,
            });
        }
        bytes.push(if round % 2 == 0 { b';' } else { 0xFD });
        for p in &mut protocols {
            p.decode(&bytes);
        }
    }
}

#[test]
fn a_zero_transceive_period_does_not_hang() {
    let imp = Impairments {
        transceive: Some(Duration::ZERO),
        ..Impairments::clean()
    };
    let sim = Simulator::new(Family::Icom { address: 0x94 }, imp, 1);
    sim.advance(Instant::now() + MS);
}
