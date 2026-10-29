use std::time::{Duration, Instant};

use haml::sim::{Impairments, Rng, Simulator};
use haml::{Cat, Command, Config, Dialect, Family, Mode, Status, Vfo};

const COMMANDS: usize = 2000;

fn profiles() -> Vec<(&'static str, Impairments)> {
    vec![
        ("clean", Impairments::clean()),
        (
            "lossy",
            Impairments {
                latency: Duration::from_millis(15),
                jitter: Duration::from_millis(10),
                drop: 0.002,
                garble: 0.002,
                chunk: 4,
                ..Impairments::clean()
            },
        ),
        ("harsh", Impairments::harsh()),
    ]
}

fn families() -> Vec<(&'static str, Family)> {
    vec![
        ("Icom CI-V", Family::Icom { address: 0x94 }),
        ("Kenwood", Family::Ascii(Dialect::Kenwood)),
        ("Elecraft", Family::Ascii(Dialect::Elecraft)),
        ("Yaesu ASCII", Family::Ascii(Dialect::Yaesu)),
        ("Yaesu binary", Family::YaesuBinary),
    ]
}

struct Report {
    done: usize,
    rejected: usize,
    failed: usize,
    retries: u64,
    timeouts: u64,
    latencies: Vec<Duration>,
    wall: Duration,
}

fn run(family: Family, imp: Impairments, seed: u64) -> Report {
    let sim = Simulator::new(family, imp, seed);
    let config = Config {
        interval: Duration::from_millis(10),
        timeout: Duration::from_millis(200),
        retries: 4,
        ..Config::default()
    };
    let mut rig = Cat::new(family.protocol(), sim.transport(), config);
    let mut rng = Rng::new(seed ^ 0x5EED);
    let modes = [Mode::Cw, Mode::Usb, Mode::Lsb, Mode::Am, Mode::Fm];
    let mut now = Instant::now();
    let wall = Instant::now();
    let mut outcomes = Vec::new();
    for i in 0..COMMANDS {
        let command = if i % 4 == 3 {
            Command::SetMode(Vfo::A, modes[rng.below(modes.len() as u64) as usize])
        } else {
            Command::SetFreq(Vfo::A, 7_000_000 + rng.below(300_000) / 10 * 10)
        };
        rig.send(command, now);
        while rig.busy() {
            now += Duration::from_millis(1);
            sim.advance(now);
            rig.tick(now);
        }
        outcomes.extend(rig.take_outcomes());
    }
    let count = |s| outcomes.iter().filter(|o| o.status == s).count();
    let mut latencies: Vec<Duration> = outcomes
        .iter()
        .filter(|o| o.status == Status::Done)
        .map(|o| o.latency)
        .collect();
    latencies.sort();
    Report {
        done: count(Status::Done),
        rejected: count(Status::Rejected),
        failed: count(Status::Failed),
        retries: rig.stats().retries,
        timeouts: rig.stats().timeouts,
        latencies,
        wall: wall.elapsed(),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let seed = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    println!("{COMMANDS} commands per run, seed {seed}, latency in virtual time");
    println!(
        "{:<13} {:<6} {:>8} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "protocol",
        "link",
        "success",
        "nak",
        "fail",
        "retries",
        "timeout",
        "mean ms",
        "p95 ms",
        "wall ms"
    );
    for (name, family) in families() {
        for (link, imp) in profiles() {
            let r = run(family, imp, seed);
            let n = r.latencies.len().max(1);
            let mean = r.latencies.iter().sum::<Duration>() / n as u32;
            let p95 = r.latencies.get(n * 95 / 100).copied().unwrap_or_default();
            println!(
                "{:<13} {:<6} {:>7.2}% {:>6} {:>6} {:>8} {:>8} {:>8.1} {:>8.1} {:>9.1}",
                name,
                link,
                100.0 * r.done as f64 / COMMANDS as f64,
                r.rejected,
                r.failed,
                r.retries,
                r.timeouts,
                ms(mean),
                ms(p95),
                ms(r.wall)
            );
        }
    }
}
