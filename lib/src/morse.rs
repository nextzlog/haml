use std::time::Duration;

pub fn pattern(c: char) -> Option<&'static str> {
    Some(match c {
        'A' => ".-",
        'B' => "-...",
        'C' => "-.-.",
        'D' => "-..",
        'E' => ".",
        'F' => "..-.",
        'G' => "--.",
        'H' => "....",
        'I' => "..",
        'J' => ".---",
        'K' => "-.-",
        'L' => ".-..",
        'M' => "--",
        'N' => "-.",
        'O' => "---",
        'P' => ".--.",
        'Q' => "--.-",
        'R' => ".-.",
        'S' => "...",
        'T' => "-",
        'U' => "..-",
        'V' => "...-",
        'W' => ".--",
        'X' => "-..-",
        'Y' => "-.--",
        'Z' => "--..",
        '0' => "-----",
        '1' => ".----",
        '2' => "..---",
        '3' => "...--",
        '4' => "....-",
        '5' => ".....",
        '6' => "-....",
        '7' => "--...",
        '8' => "---..",
        '9' => "----.",
        '-' => "-....-",
        '=' => "-...-",
        '/' => "-..-.",
        '?' => "..--..",
        'a' => ".-.-.",
        'b' | '~' => "-...-.-",
        's' => "...-.-",
        'k' => "-.--.",
        't' => "-...-",
        _ => return None,
    })
}

const MARKS: usize = 16;

const DECODED: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/?=.,-";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decoder {
    down: Option<Duration>,
    up: Option<Duration>,
    marks: Vec<Duration>,
    unit: Option<Duration>,
    text: String,
    kept: usize,
    fixed: Option<Duration>,
}

impl Decoder {
    pub fn new(kept: usize) -> Self {
        Decoder {
            kept,
            ..Decoder::default()
        }
    }

    pub fn with_unit(kept: usize, unit: Duration) -> Self {
        Decoder {
            kept,
            unit: Some(unit),
            fixed: Some(unit),
            ..Decoder::default()
        }
    }

    pub fn hint(&mut self, unit: Duration) {
        self.unit.get_or_insert(unit);
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }

    pub fn key(&mut self, down: bool, now: Duration) {
        if down {
            self.settle(now);
            self.down = Some(now);
        } else if let Some(d) = self.down.take() {
            self.marks.push(now.saturating_sub(d));
            self.up = Some(now);
            if self.marks.len() >= MARKS {
                self.letter();
            }
        }
    }

    pub fn settle(&mut self, now: Duration) {
        let (Some(up), None, Some(unit)) = (
            self.up,
            self.down,
            self.unit.or(self.marks.first().copied()),
        ) else {
            return;
        };
        let gap = now.saturating_sub(up);
        if gap > unit * 2 {
            self.letter();
        }
        if gap > unit * 5 && !self.text.ends_with(' ') && !self.text.is_empty() {
            self.push(' ');
            self.up = None;
        }
    }

    fn letter(&mut self) {
        if self.marks.is_empty() {
            return;
        }
        let unit = self.marks.iter().copied().chain(self.unit).min();
        let unit = self.fixed.or(unit).unwrap_or_default();
        let code: String = self
            .marks
            .drain(..)
            .map(|m| if m < unit * 2 { '.' } else { '-' })
            .collect();
        let c = DECODED
            .chars()
            .find(|c| pattern(*c) == Some(code.as_str()))
            .unwrap_or('*');
        self.unit = Some(unit);
        self.push(c);
    }

    fn push(&mut self, c: char) {
        self.text.push(c);
        let n = self.text.chars().count();
        if self.kept > 0 && n > self.kept {
            self.text = self.text.chars().skip(n - self.kept).collect();
        }
    }
}

pub const MIN_WPM: u32 = 5;
pub const MAX_WPM: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub wpm: u32,
    pub weight: u32,
    pub space_factor: u32,
    pub ei_space_factor: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            wpm: 25,
            weight: 50,
            space_factor: 100,
            ei_space_factor: 100,
        }
    }
}

impl Timing {
    fn period_us(&self) -> f64 {
        1_200_000.0 / self.wpm.clamp(1, 99) as f64
    }

    fn weight(&self) -> f64 {
        self.weight.min(100) as f64 / 100.0
    }

    pub fn dot(&self) -> Duration {
        us(2.0 * self.weight() * self.period_us())
    }

    pub fn dash(&self) -> Duration {
        self.dot() * 3
    }

    pub fn element_gap(&self) -> Duration {
        us(2.0 * (1.0 - self.weight()) * self.period_us())
    }

    pub fn char_gap(&self) -> Duration {
        us(self.blank3() * self.space_factor as f64 / 100.0)
    }

    pub fn ei_gap(&self) -> Duration {
        us(self.blank3()
            * (self.space_factor as f64 / 100.0)
            * (self.ei_space_factor as f64 / 100.0))
    }

    fn blank3(&self) -> f64 {
        6.0 * (1.0 - self.weight()) * self.period_us()
    }
}

fn us(v: f64) -> Duration {
    Duration::from_micros(v.max(0.0).round() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub key: bool,
    pub len: Duration,
}

pub fn keying(text: &str, timing: &Timing) -> Vec<Step> {
    let mut t = *timing;
    let mut steps = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let sign = chars.get(i).copied();
                let n = chars.get(i + 1).and_then(|d| d.to_digit(10)).unwrap_or(0);
                if n > 0 && matches!(sign, Some('+') | Some('-')) {
                    t.wpm = if sign == Some('+') {
                        (t.wpm + n).min(MAX_WPM)
                    } else {
                        t.wpm.saturating_sub(n).max(MIN_WPM)
                    };
                    i += 2;
                }
            }
            ' ' => {
                space(&mut steps, t.element_gap());
                space(&mut steps, t.char_gap());
            }
            '_' => space(&mut steps, t.element_gap()),
            'u' => t.wpm = (t.wpm + 1).clamp(MIN_WPM, MAX_WPM),
            '^' => break,
            _ => {
                let Some(p) = pattern(c) else { continue };
                for (n, e) in p.chars().enumerate() {
                    if n > 0 {
                        space(&mut steps, t.element_gap());
                    }
                    let len = if e == '.' { t.dot() } else { t.dash() };
                    steps.push(Step { key: true, len });
                }
                let gap = if c == 'E' || c == 'I' {
                    t.ei_gap()
                } else {
                    t.char_gap()
                };
                space(&mut steps, gap);
            }
        }
    }
    steps
}

fn space(steps: &mut Vec<Step>, len: Duration) {
    match steps.last_mut() {
        Some(s) if !s.key => s.len += len,
        _ => steps.push(Step { key: false, len }),
    }
}

pub fn total(steps: &[Step]) -> Duration {
    steps.iter().map(|s| s.len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn at(wpm: u32) -> Timing {
        Timing {
            wpm,
            ..Timing::default()
        }
    }

    #[test]
    fn paris_is_fifty_dots() {
        let t = at(20);
        assert_eq!(t.dot(), ms(60));
        assert_eq!(t.element_gap(), ms(60));
        assert_eq!(t.dash(), ms(180));
        assert_eq!(t.char_gap(), ms(180));
        assert_eq!(total(&keying("PARIS ", &t)), ms(3000));
    }

    #[test]
    fn weight_shifts_mark_and_space() {
        let t = Timing {
            weight: 60,
            ..at(20)
        };
        assert_eq!(t.dot(), ms(72));
        assert_eq!(t.element_gap(), ms(48));
    }

    #[test]
    fn space_factors() {
        let t = Timing {
            space_factor: 150,
            ei_space_factor: 50,
            ..at(20)
        };
        assert_eq!(t.char_gap(), ms(270));
        assert_eq!(t.ei_gap(), ms(135));
        let e = keying("E", &t);
        let want = vec![
            Step {
                key: true,
                len: ms(60),
            },
            Step {
                key: false,
                len: ms(135),
            },
        ];
        assert_eq!(e, want);
    }

    #[test]
    fn speed_change_and_skips() {
        let steps = keying("(\\+4E)", &at(20));
        assert_eq!(
            steps[0],
            Step {
                key: true,
                len: ms(50)
            }
        );
        assert_eq!(steps.len(), 2);
        assert_eq!(keying("@:*", &at(20)), vec![]);
        assert_eq!(keying("\\-9E", &at(10))[0].len, ms(240));
    }

    #[test]
    fn pause_stops_keying() {
        let t = at(20);
        assert_eq!(keying("E^E", &t), keying("E", &t));
        assert_eq!(keying("^PARIS", &t), vec![]);
        assert_eq!(keying("E^", &t), keying("E", &t));
        assert_eq!(keying("^", &t), vec![]);
    }

    #[test]
    fn u_raises_speed_by_one() {
        assert_eq!(keying("uuuuE", &at(20))[0].len, ms(50));
        assert_eq!(keying("u", &at(20)), vec![]);
        assert_eq!(keying("uE", &at(MAX_WPM))[0].len, ms(20));
    }

    #[test]
    fn bracket_keys_nothing() {
        let t = at(20);
        assert_eq!(keying("]", &t), vec![]);
        assert_eq!(keying("E]E", &t), keying("EE", &t));
    }

    #[test]
    fn empty_text_keys_nothing() {
        assert_eq!(keying("", &at(20)), vec![]);
        assert_eq!(keying("\\+", &at(20)), vec![]);
    }

    #[test]
    fn prosigns_and_spaces_merge() {
        assert_eq!(pattern('a'), Some(".-.-."));
        let steps = keying("T  T", &at(20));
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[1].len, ms(180 + 2 * 240));
    }

    #[test]
    fn the_decoder_reads_keyed_text_back() {
        let mut d = Decoder::with_unit(0, at(30).dot());
        let mut now = Duration::ZERO;
        for step in keying("CQ TEST", &at(30)) {
            d.key(step.key, now);
            now += step.len;
        }
        d.key(false, now);
        d.settle(now + ms(1000));
        assert_eq!(d.text(), "CQ TEST ");
        d.clear();
        assert_eq!(d.text(), "");
        let mut short = Decoder::new(2);
        for (down, up) in [(40, 300), (40, 300), (40, 300)] {
            short.key(true, now);
            now += ms(down);
            short.key(false, now);
            now += ms(up);
            short.settle(now);
        }
        assert_eq!(short.text(), "E ");
    }

    #[test]
    fn keying_without_gaps_keeps_the_decoder_bounded() {
        let mut d = Decoder::with_unit(8, ms(60));
        let mut now = Duration::ZERO;
        for _ in 0..100_000 {
            d.key(true, now);
            now += ms(60);
            d.key(false, now);
            now += ms(60);
        }
        assert!(d.marks.len() < MARKS);
        assert_eq!(d.text(), "********");
    }
}
