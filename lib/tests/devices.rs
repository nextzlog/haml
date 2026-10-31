use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haml::keyer::if4cw::{If4cwKeyer, Setup as If4cwSetup, Usb};
use haml::keyer::line::{LineKeyer, LineSetup};
use haml::keyer::winkeyer::{Setup, WinKeyer};
use haml::sim::{If4cwSim, NeoSim, OtrspSim, SimKeyer, SimLines, SimSo2r, WinKeyerSim};
use haml::so2r::neo::Neo;
use haml::so2r::otrsp::Otrsp;
use haml::so2r::Switch;
use haml::transport::Slot;
use haml::tx::{Delays, Signal};
use haml::{Arbiter, Keyer, KeyerEvent, Line, Rx, So2r, Timing, TxCommand, TxEvent};

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn at(wpm: u32) -> Timing {
    Timing {
        wpm,
        ..Timing::default()
    }
}

#[test]
fn the_line_keyer_keys_the_simulated_lines_and_reads_the_text_back() {
    let sim = SimLines::new(Line::Dtr, at(30));
    let setup = LineSetup {
        key: Line::Dtr,
        phone: Line::Dtr,
        ptt: true,
        ptt_before: Duration::from_millis(50),
        ptt_after: Duration::ZERO,
    };
    let mut k = LineKeyer::new(sim.clone(), setup, at(30));
    k.send("TEST");
    until("keyed", || !k.busy());
    std::thread::sleep(Duration::from_millis(300));
    let st = sim.state();
    assert_eq!(st.keyed.trim(), "TEST");
    assert!(!st.key && !st.ptt);
    let ptt_on = st.edges.iter().find(|e| e.0 == Line::Rts && e.1).unwrap().2;
    let key_on = st.edges.iter().find(|e| e.0 == Line::Dtr && e.1).unwrap().2;
    assert!(key_on - ptt_on >= Duration::from_millis(50));
}

#[test]
fn the_usbif4cw_simulator_reads_back_what_was_keyed() {
    let sim = If4cwSim::new();
    let usb = Usb::new(Box::new(sim.hid()), haml::sim::if4cw::PRODUCT);
    let setup = If4cwSetup {
        ptt: true,
        ptt_before_ms: 25,
        ptt_after_ms: 1,
        paddle_reverse: false,
        sync_wpm: false,
        gen3_mic: false,
    };
    let mut k = If4cwKeyer::open(usb, setup, at(30));
    let st = sim.state();
    assert_eq!((st.attached, st.paddle_wpm), (1, Some(30)));
    assert_eq!(st.ptt_delays, Some((25, 50)));
    k.send("TEST");
    until("keyed", || !k.busy());
    std::thread::sleep(Duration::from_millis(300));
    assert!(k.poll().contains(&KeyerEvent::Finished));
    let st = sim.state();
    assert_eq!(st.keyed, "TEST ");
    assert!(!st.key() && !st.ptt());
    let polls = |k: &mut If4cwKeyer| (0..3).flat_map(|_| k.poll()).collect::<Vec<_>>();
    sim.press(true, false);
    assert_eq!(polls(&mut k), [KeyerEvent::Paddle(true)]);
    sim.press(false, false);
    assert_eq!(polls(&mut k), [KeyerEvent::Paddle(false)]);
    let mut b = k.rig_select(true);
    b.select_rig(2);
    assert_eq!((sim.state().rig(), sim.state().mic()), (2, 2));
    drop(b);
    drop(k);
    until("the keyer let go", || sim.state().attached == 0);
}

fn winkeyer_on(slot: &Slot) -> (WinKeyerSim, WinKeyer<haml::transport::Shared>) {
    let sim = WinKeyerSim::new();
    let setup = Setup {
        wpm: 30,
        paddle_swap: false,
        sidetone: false,
        ptt_before_ms: 25,
        ptt_after_ms: 1,
        speed_pot: false,
    };
    let k = WinKeyer::publish(Box::new(sim.transport()), &setup, slot).unwrap();
    (sim, k)
}

#[test]
fn the_neo_on_the_keyer_port_reaches_the_winkeyer_simulator() {
    let slot = Slot::default();
    let (sim, mut k) = winkeyer_on(&slot);
    assert!(sim.state().open);
    let mut b = Neo::on_keyer(slot.clone());
    b.select_rig(2);
    b.blend(true, 50);
    assert!(b.error().is_none());
    assert_eq!(sim.state().neo, [0x94, 0x95, 0x87, 0xC7, 0xCF]);
    k.send("E");
    until("finished", || k.poll().contains(&KeyerEvent::Finished));
    assert_eq!(sim.state().sent, "E");
    drop(k);
    assert!(!sim.state().open);
}

#[test]
fn the_box_simulators_show_what_they_were_told() {
    let neo = NeoSim::new();
    let mut s = Switch::new(Neo::on_port(Box::new(neo.transport())));
    s.select_rig(2);
    s.listen(Rx::Both);
    s.blend(true, 30);
    let st = neo.state();
    assert_eq!(
        (st.tx, st.rx, st.blend, st.percent()),
        (2, Rx::Both, true, 30)
    );
    let otrsp = OtrspSim::new();
    let mut s = Switch::new(Otrsp::new(Box::new(otrsp.transport())));
    s.select_rig(1);
    s.transmit_on(2);
    s.transmit_on(2);
    s.transmit_on(1);
    assert_eq!(otrsp.state().lines, ["TX1", "RX1", "TX2", "TX1"]);
}

#[test]
fn the_arbiter_drives_simulated_devices_in_turn() {
    let keyer = SimKeyer::new(at(60));
    let sent = keyer.sent.clone();
    let log = Arc::new(Mutex::new(Vec::new()));
    let tx = Arbiter::new(2, Delays::default())
        .with_keyer(Box::new(keyer))
        .with_so2r(Box::new(SimSo2r::new(log.clone())))
        .spawn();
    let events = tx.subscribe();
    for (radio, text) in [(1, "CQ"), (2, "TU")] {
        tx.send(TxCommand::Send {
            id: haml::ticket(),
            radio,
            signal: Signal::Cw(text.into()),
        })
        .unwrap();
    }
    let mut finished = Vec::new();
    until("both sent", || {
        for e in events.try_iter() {
            if let TxEvent::Finished { radio, .. } = e {
                finished.push(radio);
            }
        }
        finished.len() == 2
    });
    assert_eq!(finished, [1, 2]);
    assert_eq!(*sent.lock().unwrap(), ["CQ", "TU"]);
    let switched: Vec<_> = log.lock().unwrap().clone();
    assert_eq!(
        switched,
        [
            haml::sim::so2r::SimCommand::Tx(1),
            haml::sim::so2r::SimCommand::Tx(2)
        ]
    );
}

#[test]
fn a_rig_on_its_own_thread_is_driven_through_a_client() {
    use haml::rig::{RigClient, RigDriver};
    use haml::sim::{Impairments, Simulator};
    use haml::{Cat, Command, Config, Dialect, Family, Rig, RigCaps, Vfo};
    let family = Family::Ascii(Dialect::Kenwood);
    let sim = Simulator::new(family, Impairments::clean(), 1);
    sim.realtime(true);
    let config = Config {
        interval: Duration::from_millis(5),
        ..Config::default()
    };
    let device = RigDriver::spawn(Cat::new(family.protocol(), sim.transport(), config));
    let mut rig = RigClient::new(&device, RigCaps::ALL);
    rig.send(Command::SetFreq(Vfo::A, 7_012_000), Instant::now());
    until("tuned", || {
        rig.tick(Instant::now());
        rig.state().freq() == Some(7_012_000)
    });
    assert_eq!(sim.radio().freq(), 7_012_000);
}
