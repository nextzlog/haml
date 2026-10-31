use std::collections::HashMap;
use std::sync::OnceLock;

pub struct Texts {
    tables: Vec<(&'static str, HashMap<&'static str, &'static str>)>,
}

impl Texts {
    fn new(files: &[(&'static str, &'static str)]) -> Self {
        let tables = files
            .iter()
            .map(|(code, text)| (*code, parse(text)))
            .collect();
        Texts { tables }
    }

    pub fn get<'a>(&self, language: &str, key: &'a str) -> &'a str
    where
        'static: 'a,
    {
        let find = |code: &str| {
            self.tables
                .iter()
                .find(|(c, _)| *c == code)
                .and_then(|(_, t)| t.get(key).copied())
        };
        find(language)
            .or_else(|| self.tables.first().and_then(|(_, t)| t.get(key).copied()))
            .unwrap_or(key)
    }

    pub fn format(&self, language: &str, key: &str, args: &[&str]) -> String {
        let mut s = self.get(language, key).to_string();
        for (i, a) in args.iter().enumerate() {
            s = s.replace(&format!("{{{i}}}"), a);
        }
        s
    }

    pub fn missing(&self) -> Vec<(&'static str, &'static str)> {
        let Some((_, first)) = self.tables.first() else {
            return Vec::new();
        };
        let mut out: Vec<_> = self
            .tables
            .iter()
            .skip(1)
            .flat_map(|(code, t)| {
                first
                    .keys()
                    .filter(|k| !t.contains_key(*k))
                    .map(move |k| (*code, *k))
            })
            .collect();
        out.sort();
        out
    }
}

fn parse(text: &'static str) -> HashMap<&'static str, &'static str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

pub fn texts() -> &'static Texts {
    static TEXTS: OnceLock<Texts> = OnceLock::new();
    TEXTS.get_or_init(|| {
        Texts::new(&[
            ("en", include_str!("../lang/en.lang")),
            ("ja", include_str!("../lang/ja.lang")),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_is_complete() {
        assert!(texts().missing().is_empty(), "{:?}", texts().missing());
    }

    #[test]
    fn fallback_and_arguments() {
        assert_eq!(texts().get("fr", "connect"), texts().get("en", "connect"));
        assert_eq!(texts().get("en", "no-such-key"), "no-such-key");
        assert!(texts().format("en", "error", &["x"]).contains('x'));
        assert_eq!(texts().get("ja", "connect"), "接続");
    }

    #[test]
    fn japanese_follows_the_wording_rules() {
        let ja = include_str!("../lang/ja.lang");
        for line in ja.lines().filter(|l| !l.starts_with('#')) {
            assert!(!line.contains("QSO"), "{line}");
            assert!(
                !line.chars().any(|c| ('\u{FF01}'..='\u{FF5E}').contains(&c)),
                "{line}"
            );
            let v: Vec<char> = line
                .split_once(" = ")
                .map_or("", |(_, v)| v)
                .chars()
                .collect();
            for w in v.windows(2) {
                let ja = |c: char| {
                    ('\u{3040}'..='\u{30FF}').contains(&c) || ('\u{4E00}'..='\u{9FFF}').contains(&c)
                };
                let latin = |c: char| c.is_ascii_alphanumeric();
                assert!(
                    !(ja(w[0]) && latin(w[1]) || latin(w[0]) && ja(w[1])),
                    "{line}"
                );
            }
        }
    }
}
