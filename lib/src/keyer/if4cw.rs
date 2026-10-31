use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hid::Hid;
use crate::keyer::line::{LineKeyer, LineSetup};
use crate::keyer::{Keyer, KeyerCaps, KeyerEvent};
use crate::morse::Timing;
use crate::ptt::{Line, Lines};
use crate::so2r::{Rx, So2r, So2rCaps};

use report::{KEY, MIC, PTT, RIG};

pub mod report {
    pub const VID: u16 = 0x0BFE;
    pub const PID: u16 = 0x0E00;

    pub const KEY: u8 = 0x01;
    pub const PTT: u8 = 0x02;
    pub const RIG: u8 = 0x04;
    pub const MIC: u8 = 0x08;

    pub const QUERY: [u8; 9] = [0, 0x04, 0x0F, 0x04, 0, 0, 0, 0, 0];

    pub fn port(bits: u8) -> [u8; 9] {
        [0, 0x01, bits, 0, 0, 0, 0, 0, 0]
    }

    pub fn wpm(wpm: u32) -> [u8; 9] {
        let p = (600.0 / wpm.clamp(5, 50) as f64).round() as u8;
        [0, 0x24, p, 0, 0, 0, 0, 0, 0xFC]
    }

    pub fn ptt_delays(before_ms: u32, after_ms: u32) -> Vec<[u8; 9]> {
        if before_ms == 0 && after_ms == 0 {
            return vec![[0, 0x25, 0, 0, 0, 0, 0, 0, 0]];
        }
        let before = before_ms.min(254) as u8;
        let after = after_ms.clamp(50, 254) as u8;
        vec![
            [0, 0x22, before, after, 0, 0, 0, 0, 0],
            [0, 0x25, 1, 0, 0, 0, 0, 0, 0],
        ]
    }

    pub fn ptt(on: bool) -> [u8; 9] {
        [0, 0x35, 0, on as u8, 0, 0, 0, 0, 0xFC]
    }

    pub fn paddle(reverse: bool) -> [u8; 9] {
        [0, 0x26, 0, 0, reverse as u8, 0, 0, 0, 0xFC]
    }

    pub fn version(product: &str) -> u32 {
        let Some(i) = product.find("Ver.") else {
            return 0;
        };
        let v = &product[i + 4..];
        let end = v
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(v.len());
        v[..end]
            .parse::<f64>()
            .map_or(0, |v| (v * 10.0 + 1e-9).trunc() as u32)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reports_have_the_documented_bytes() {
            assert_eq!(port(0xFE), [0, 1, 0xFE, 0, 0, 0, 0, 0, 0]);
            assert_eq!(wpm(25), [0, 0x24, 24, 0, 0, 0, 0, 0, 0xFC]);
            assert_eq!(wpm(70)[2], 12, "50 WPM at most");
            assert_eq!(wpm(1)[2], 120, "5 WPM at least");
            assert_eq!(ptt_delays(0, 0), [[0, 0x25, 0, 0, 0, 0, 0, 0, 0]]);
            assert_eq!(
                ptt_delays(300, 10),
                [
                    [0, 0x22, 254, 50, 0, 0, 0, 0, 0],
                    [0, 0x25, 1, 0, 0, 0, 0, 0, 0]
                ]
            );
            assert_eq!(ptt(true), [0, 0x35, 0, 1, 0, 0, 0, 0, 0xFC]);
            assert_eq!(paddle(false), [0, 0x26, 0, 0, 0, 0, 0, 0, 0xFC]);
        }

        #[test]
        fn version_from_the_product_string() {
            assert_eq!(version("USBIF4CW Ver.2.3"), 23);
            assert_eq!(version("Ver.2.0 gen3"), 20);
            assert_eq!(version("USBIF4CW"), 0);
            assert_eq!(version("Ver.x"), 0);
        }
    }
}

struct Device {
    hid: Box<dyn Hid>,
    bits: u8,
    sent: u8,
    version: u32,
    error: Option<String>,
}

impl Device {
    fn write(&mut self, report: &[u8]) {
        if let Err(e) = self.hid.write(report) {
            self.error = Some(e.to_string());
        }
    }

    fn set(&mut self, mask: u8, active: bool) {
        if active {
            self.bits &= !mask;
        } else {
            self.bits |= mask;
        }
        if self.bits != self.sent {
            let r = report::port(self.bits);
            self.write(&r);
            self.sent = self.bits;
        }
    }
}

#[derive(Clone)]
pub struct Usb(Arc<Mutex<Device>>);

impl Usb {
    pub fn new(hid: Box<dyn Hid>, product: &str) -> Self {
        Usb(Arc::new(Mutex::new(Device {
            hid,
            bits: 0xFF,
            sent: 0x00,
            version: report::version(product),
            error: None,
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Device> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn version(&self) -> u32 {
        self.lock().version
    }

    pub fn set(&self, mask: u8, active: bool) {
        self.lock().set(mask, active);
    }

    fn command(&self, report: &[u8; 9]) {
        let mut d = self.lock();
        if d.version >= 20 {
            d.write(report);
        }
    }

    pub fn select_rig(&self, rig: u8, mic: bool) {
        let mut d = self.lock();
        d.set(RIG, rig == 2);
        if mic {
            d.set(MIC, rig == 2);
        }
    }
}

struct UsbLines(Usb);

impl Lines for UsbLines {
    fn set(&mut self, line: Line, on: bool) -> io::Result<()> {
        let mask = match line {
            Line::Dtr => KEY,
            Line::Rts => PTT,
        };
        let mut d = self.0.lock();
        d.set(mask, on);
        match d.error.take() {
            Some(e) => Err(io::Error::other(e)),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    pub ptt: bool,
    pub ptt_before_ms: u32,
    pub ptt_after_ms: u32,
    pub paddle_reverse: bool,
    pub sync_wpm: bool,
    pub gen3_mic: bool,
}

pub struct If4cwKeyer {
    lines: LineKeyer,
    usb: Usb,
    setup: Setup,
    paddle: bool,
}

impl If4cwKeyer {
    pub fn open(usb: Usb, setup: Setup, timing: Timing) -> Self {
        {
            let mut d = usb.lock();
            d.bits = 0xFF;
            d.sent = 0x00;
            d.set(RIG, false);
            d.set(MIC, false);
            d.write(&report::QUERY);
        }
        usb.command(&report::paddle(setup.paddle_reverse));
        usb.command(&report::wpm(timing.wpm));
        let ptt = if setup.ptt {
            report::ptt_delays(setup.ptt_before_ms, setup.ptt_after_ms)
        } else {
            report::ptt_delays(0, 0)
        };
        for r in &ptt {
            usb.command(r);
        }
        let lines = LineKeyer::new(
            UsbLines(usb.clone()),
            LineSetup {
                key: Line::Dtr,
                phone: Line::Rts,
                ptt: setup.ptt,
                ptt_before: Duration::from_millis(setup.ptt_before_ms as u64),
                ptt_after: Duration::from_millis(setup.ptt_after_ms as u64),
            },
            timing,
        );
        If4cwKeyer {
            lines,
            usb,
            setup,
            paddle: false,
        }
    }

    pub fn rig_select(&self, mic: bool) -> RigSelect {
        RigSelect::new(self.usb.clone(), mic)
    }

    fn read_paddle(&mut self, events: &mut Vec<KeyerEvent>) {
        let mut buf = [0u8; 16];
        loop {
            let mut d = self.usb.lock();
            let n = match d.hid.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    events.push(KeyerEvent::Error(e.to_string()));
                    return;
                }
            };
            let version = d.version;
            drop(d);
            if n == 0 {
                if version >= 20 {
                    self.usb.lock().write(&report::QUERY);
                }
                return;
            }
            if version < 20 || n < 3 || buf[0] != 0x04 || buf[2] != 0x04 || buf[1] == 0x0F {
                continue;
            }
            let down = buf[1] & 0x05 != 0x05;
            if down != self.paddle {
                self.paddle = down;
                events.push(KeyerEvent::Paddle(down));
            }
        }
    }
}

impl Drop for If4cwKeyer {
    fn drop(&mut self) {
        self.usb.set(KEY | PTT, false);
    }
}

impl Keyer for If4cwKeyer {
    fn caps(&self) -> KeyerCaps {
        KeyerCaps {
            ptt: self.setup.ptt,
            ptt_delays: self.setup.ptt,
            paddle: self.usb.version() >= 20,
            ..KeyerCaps::default()
        }
    }

    fn send(&mut self, text: &str) {
        self.lines.send(text);
    }

    fn abort(&mut self) {
        self.lines.abort();
    }

    fn set_wpm(&mut self, wpm: u32) {
        self.lines.set_wpm(wpm);
        if self.setup.sync_wpm {
            self.usb.command(&report::wpm(self.lines.wpm()));
        }
    }

    fn wpm(&self) -> u32 {
        self.lines.wpm()
    }

    fn set_timing(&mut self, timing: Timing) {
        self.lines.set_timing(timing);
        if self.setup.sync_wpm {
            self.usb.command(&report::wpm(self.lines.wpm()));
        }
    }

    fn set_ptt(&mut self, on: bool) {
        self.lines.set_ptt(on);
    }

    fn set_phone_ptt(&mut self, on: bool) {
        if self.setup.gen3_mic {
            self.usb.set(MIC, on);
        }
        self.lines.set_phone_ptt(on);
    }

    fn busy(&self) -> bool {
        self.lines.busy()
    }

    fn poll(&mut self) -> Vec<KeyerEvent> {
        let mut events = self.lines.poll();
        self.read_paddle(&mut events);
        if let Some(e) = self.usb.lock().error.take() {
            events.push(KeyerEvent::Error(e));
        }
        events
    }
}

pub struct RigSelect {
    usb: Usb,
    mic: bool,
}

impl RigSelect {
    pub fn new(usb: Usb, mic: bool) -> Self {
        RigSelect { usb, mic }
    }
}

impl So2r for RigSelect {
    fn caps(&self) -> So2rCaps {
        So2rCaps::default()
    }

    fn tx(&mut self, radio: u8) {
        self.usb.select_rig(radio, self.mic);
    }

    fn rx(&mut self, _rx: Rx) {}

    fn error(&self) -> Option<String> {
        self.usb.lock().error.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Clone, Default)]
    struct FakeHid {
        written: Arc<Mutex<Vec<[u8; 9]>>>,
        input: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl Hid for FakeHid {
        fn write(&mut self, report: &[u8]) -> io::Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(report.try_into().unwrap());
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.input.lock().unwrap().pop_front() {
                Some(r) => {
                    buf[..r.len()].copy_from_slice(&r);
                    Ok(r.len())
                }
                None => Ok(0),
            }
        }
    }

    impl FakeHid {
        fn take(&self) -> Vec<[u8; 9]> {
            std::mem::take(&mut self.written.lock().unwrap())
        }
    }

    fn port(bits: u8) -> [u8; 9] {
        [0, 1, bits, 0, 0, 0, 0, 0, 0]
    }

    fn keyer(product: &str, setup: Setup) -> (FakeHid, If4cwKeyer, ()) {
        let hid = FakeHid::default();
        let usb = Usb::new(Box::new(hid.clone()), product);
        let timing = Timing {
            wpm: 30,
            ..Timing::default()
        };
        (hid.clone(), If4cwKeyer::open(usb, setup, timing), ())
    }

    fn setup() -> Setup {
        Setup {
            ptt: false,
            ptt_before_ms: 25,
            ptt_after_ms: 1,
            paddle_reverse: false,
            sync_wpm: false,
            gen3_mic: false,
        }
    }

    #[test]
    fn opening_a_version_2_device_sends_the_setup_reports() {
        let s = Setup {
            ptt: true,
            ptt_before_ms: 30,
            ptt_after_ms: 10,
            paddle_reverse: true,
            ..setup()
        };
        let (hid, _k, _hub) = keyer("USBIF4CW Ver.2.3", s);
        assert_eq!(
            hid.take(),
            [
                port(0xFF),
                [0, 0x04, 0x0F, 0x04, 0, 0, 0, 0, 0],
                [0, 0x26, 0, 0, 1, 0, 0, 0, 0xFC],
                [0, 0x24, 20, 0, 0, 0, 0, 0, 0xFC],
                [0, 0x22, 30, 50, 0, 0, 0, 0, 0],
                [0, 0x25, 1, 0, 0, 0, 0, 0, 0],
            ]
        );
    }

    #[test]
    fn an_old_device_gets_only_the_port_bits() {
        let (hid, _k, _hub) = keyer("USBIF4CW Ver.1.5", setup());
        assert_eq!(hid.take(), [port(0xFF), report::QUERY]);
        let (hid, _k, _hub) = keyer("USBIF4CW", setup());
        assert_eq!(hid.take().len(), 2);
    }

    #[test]
    fn keying_clears_the_key_bit_and_sends_only_changes() {
        let (hid, mut k, _hub) = keyer("Ver.2.0", setup());
        hid.take();
        k.send("E");
        let start = std::time::Instant::now();
        while k.busy() {
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(hid.take(), [port(0xFE), port(0xFF)]);
        k.set_ptt(true);
        k.set_phone_ptt(true);
        k.set_phone_ptt(true);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(hid.take(), [port(0xFD)]);
    }

    #[test]
    fn the_paddle_speed_follows_the_keyer_when_asked() {
        let (hid, mut k, _hub) = keyer("Ver.2.0", setup());
        hid.take();
        k.set_wpm(50);
        assert!(hid.take().is_empty());
        let s = Setup {
            sync_wpm: true,
            ..setup()
        };
        let (hid, mut k, _hub) = keyer("Ver.2.0", s);
        hid.take();
        k.set_wpm(50);
        assert_eq!(hid.take(), [[0, 0x24, 12, 0, 0, 0, 0, 0, 0xFC]]);
    }

    #[test]
    fn gen3_mic_select_follows_phone_ptt() {
        let s = Setup {
            gen3_mic: true,
            ..setup()
        };
        let (hid, mut k, _hub) = keyer("Ver.2.0", s);
        hid.take();
        k.set_phone_ptt(true);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(hid.take(), [port(0xF7), port(0xF5)]);
    }

    #[test]
    fn the_paddle_stops_sending_from_version_2() {
        let (hid, mut k, _hub) = keyer("Ver.2.0", setup());
        hid.take();
        let answer = |b: u8| vec![0x04, b, 0x04, 0, 0, 0, 0, 0];
        hid.input.lock().unwrap().extend([
            answer(0x0F),
            answer(0x07),
            answer(0x06),
            vec![0x01, 0x02, 0x00, 0, 0, 0, 0, 0],
            answer(0x0F),
            answer(0x0D),
        ]);
        assert_eq!(
            k.poll(),
            [KeyerEvent::Paddle(true), KeyerEvent::Paddle(false)]
        );
        assert_eq!(hid.take(), [report::QUERY], "asked again for each poll");
        hid.input.lock().unwrap().push_back(answer(0x00));
        assert_eq!(k.poll(), [KeyerEvent::Paddle(true)]);
        assert_eq!(hid.take(), [report::QUERY]);
        let (hid, mut k, _hub) = keyer("Ver.1.0", setup());
        hid.take();
        hid.input.lock().unwrap().push_back(answer(0x06));
        assert!(k.poll().is_empty());
        assert!(hid.take().is_empty());
    }

    #[test]
    fn the_rig_bit_goes_through_the_keyers_device() {
        let (hid, k, _) = keyer("Ver.2.0", setup());
        hid.take();
        let mut b = k.rig_select(true);
        b.select_rig(2);
        b.select_rig(1);
        assert_eq!(hid.take(), [port(0xFB), port(0xF3), port(0xF7), port(0xFF)]);
        let mut b = k.rig_select(false);
        b.select_rig(2);
        assert_eq!(hid.take(), [port(0xFB)]);
    }
}
