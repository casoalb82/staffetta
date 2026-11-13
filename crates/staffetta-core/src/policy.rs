//! Per-channel rules. The channel owner decides every one of these, for their
//! own channel only, and can change them at any time with `SET`.
//!
//! Defaults are deliberately conservative: a freshly linked channel is polite
//! to its neighbours and cannot be flooded by them.

use serde::{Deserialize, Serialize};

/// Language the service uses when it talks to this channel's operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Lang {
    #[default]
    En,
    It,
}

/// What this channel does with the link right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Linked on paper, nothing flows.
    Off,
    /// Listen only: nothing is sent in or out, but ops can see (via `STATUS`
    /// and the audit log) exactly what *would* have been relayed. This is the
    /// "try before you commit" mode.
    Preview,
    /// Fully live.
    Live,
}

/// How remote people show up in this channel's user list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Presence {
    /// Everyone present in the other channels is in the user list.
    Full,
    /// A remote person appears when they speak and leaves after `idle_secs`
    /// of silence. Much lighter on connections.
    Lazy { idle_secs: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Lines a single remote person may send in a burst.
    pub burst: u32,
    /// The burst refills completely in this many seconds.
    pub per_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub mode: Mode,
    /// Language of the service's replies and notices for this channel.
    #[serde(default)]
    pub lang: Lang,
    /// List this channel on the public status page (opt-in, default off).
    #[serde(default)]
    pub public: bool,
    /// Lines said here are sent to the other channels.
    pub share_out: bool,
    /// Lines from the other channels are shown here.
    pub accept_in: bool,
    pub presence: Presence,
    /// Only relay *into* this channel people who are logged in to services.
    pub require_account_in: bool,
    /// Seconds someone must have been in *this* channel before their lines
    /// leave it (stops hit-and-run trolling of the neighbours).
    pub min_presence_secs: u64,
    pub rate: RateLimit,
    /// Longest relayed line, in characters.
    pub max_line: usize,
    pub strip_formatting: bool,
    /// Lines starting with one of these are bot commands, never relayed.
    pub drop_prefixes: Vec<String>,
    /// Masks (`nick!ident@host` globs or `account:name`) of people who are
    /// never relayed out of, nor shown in, this channel.
    pub block: Vec<String>,
    /// Linked channels (`net:#chan`, folded) this channel does not want to
    /// hear from, while staying linked with the others.
    pub muted: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            lang: Lang::En,
            public: false,
            mode: Mode::Preview, // a new link never goes live by itself
            share_out: true,
            accept_in: true,
            presence: Presence::Lazy { idle_secs: 1800 },
            require_account_in: false,
            min_presence_secs: 0,
            rate: RateLimit {
                burst: 5,
                per_secs: 10,
            },
            max_line: 350,
            strip_formatting: true,
            drop_prefixes: vec!["!".into(), ".".into()],
            block: Vec::new(),
            muted: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetError {
    #[error("unknown setting '{0}'")]
    UnknownKey(String),
    #[error("bad value for '{key}': {why}")]
    BadValue { key: String, why: String },
}

fn parse_bool(key: &str, v: &str) -> Result<bool, SetError> {
    match v.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        _ => Err(SetError::BadValue {
            key: key.into(),
            why: "use on or off".into(),
        }),
    }
}

fn parse_num<T: TryFrom<u64>>(key: &str, v: &str, lo: u64, hi: u64) -> Result<T, SetError> {
    let n: u64 = v.parse().map_err(|_| SetError::BadValue {
        key: key.into(),
        why: "expected a number".into(),
    })?;
    if n < lo || n > hi {
        return Err(SetError::BadValue {
            key: key.into(),
            why: format!("must be between {lo} and {hi}"),
        });
    }
    T::try_from(n).map_err(|_| SetError::BadValue {
        key: key.into(),
        why: "out of range".into(),
    })
}

impl Policy {
    /// All settable keys with a one-line help, for the `HELP SET` reply.
    pub fn keys(lang: Lang) -> Vec<(&'static str, &'static str)> {
        if lang == Lang::It {
            vec![
                ("share-out", "on|off - invia la chat di questo canale ai canali collegati"),
                ("accept-in", "on|off - mostra qui la chat dei canali collegati"),
                ("presence", "full | lazy [minuti] - come appaiono le persone remote nella lista utenti"),
                ("require-account", "on|off - mostra solo chi è identificato ai servizi"),
                ("min-presence", "secondi (0-86400) che uno deve essere qui prima che le sue righe escano"),
                ("rate", "<righe>/<secondi>, es. 5/10 - limite anti-flood per persona"),
                ("max-line", "caratteri (40-450) per riga inoltrata"),
                ("strip-formatting", "on|off - toglie colori e grassetto dalle righe inoltrate"),
                ("lang", "it | en - lingua delle risposte del servizio per questo canale"),
                ("public", "on|off - mostra questo canale nella pagina pubblica di stato (solo nome e modalità)"),
            ]
        } else {
            vec![
                (
                    "share-out",
                    "on|off - send this channel's chat to the linked channels",
                ),
                ("accept-in", "on|off - show the linked channels' chat here"),
                (
                    "presence",
                    "full | lazy [idle-minutes] - how remote people appear in the user list",
                ),
                (
                    "require-account",
                    "on|off - only show people who are logged in to services",
                ),
                (
                    "min-presence",
                    "seconds (0-86400) someone must be here before their lines leave",
                ),
                (
                    "rate",
                    "<lines>/<seconds>, e.g. 5/10 - per-person flood limit",
                ),
                ("max-line", "characters (40-450) per relayed line"),
                (
                    "strip-formatting",
                    "on|off - remove colours/bold from relayed lines",
                ),
                (
                    "lang",
                    "it | en - language of the service's replies for this channel",
                ),
                (
                    "public",
                    "on|off - list this channel on the public status page (name and mode only)",
                ),
            ]
        }
    }

    /// Apply one `SET <key> <value...>` change.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), SetError> {
        let key_l = key.to_ascii_lowercase();
        let value = value.trim();
        match key_l.as_str() {
            "share-out" => self.share_out = parse_bool(&key_l, value)?,
            "accept-in" => self.accept_in = parse_bool(&key_l, value)?,
            "require-account" => self.require_account_in = parse_bool(&key_l, value)?,
            "strip-formatting" => self.strip_formatting = parse_bool(&key_l, value)?,
            "public" => self.public = parse_bool(&key_l, value)?,
            "lang" => {
                self.lang = match value.to_ascii_lowercase().as_str() {
                    "it" | "italiano" => Lang::It,
                    "en" | "english" => Lang::En,
                    _ => {
                        return Err(SetError::BadValue {
                            key: key_l,
                            why: "use it or en".into(),
                        })
                    }
                }
            }
            "min-presence" => self.min_presence_secs = parse_num(&key_l, value, 0, 86_400)?,
            "max-line" => self.max_line = parse_num::<usize>(&key_l, value, 40, 450)?,
            "presence" => {
                let mut it = value.split_whitespace();
                match it.next().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("full") => self.presence = Presence::Full,
                    Some("lazy") => {
                        let mins: u64 = match it.next() {
                            Some(m) => parse_num(&key_l, m, 1, 1440)?,
                            None => 30,
                        };
                        self.presence = Presence::Lazy {
                            idle_secs: mins * 60,
                        };
                    }
                    _ => {
                        return Err(SetError::BadValue {
                            key: key_l,
                            why: "use 'full' or 'lazy [minutes]'".into(),
                        })
                    }
                }
            }
            "rate" => {
                let (a, b) = value.split_once('/').ok_or_else(|| SetError::BadValue {
                    key: key_l.clone(),
                    why: "use <lines>/<seconds>, e.g. 5/10".into(),
                })?;
                self.rate = RateLimit {
                    burst: parse_num(&key_l, a.trim(), 1, 50)?,
                    per_secs: parse_num(&key_l, b.trim(), 1, 3600)?,
                };
            }
            _ => return Err(SetError::UnknownKey(key.to_string())),
        }
        Ok(())
    }

    /// One short line per setting, for `STATUS`.
    pub fn describe(&self, lang: Lang) -> Vec<String> {
        let it = lang == Lang::It;
        let presence = match self.presence {
            Presence::Full => "full".to_string(),
            Presence::Lazy { idle_secs } => format!("lazy ({} min)", idle_secs / 60),
        };
        vec![
            format!(
                "share-out {} | accept-in {} | presence {}",
                onoff(self.share_out),
                onoff(self.accept_in),
                presence
            ),
            format!(
                "require-account {} | min-presence {}s | rate {}/{}s | max-line {}",
                onoff(self.require_account_in),
                self.min_presence_secs,
                self.rate.burst,
                self.rate.per_secs,
                self.max_line
            ),
            format!(
                "strip-formatting {} | {} {} | {} {} | lang {}",
                onoff(self.strip_formatting),
                if it { "bloccati" } else { "blocked" },
                self.block.len(),
                if it { "silenziati" } else { "muted" },
                self.muted.len(),
                if self.lang == Lang::It { "it" } else { "en" }
            ),
        ]
    }
}

fn onoff(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// Remove IRC colour/bold/underline/reverse/reset control codes.
pub fn strip_formatting(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x02' | '\x1d' | '\x1f' | '\x1e' | '\x11' | '\x16' | '\x0f' => {}
            '\x03' => {
                // \x03[fg[,bg]] - up to two digits each
                let mut digits = 0;
                while digits < 2 && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                    chars.next();
                    digits += 1;
                }
                if digits > 0 && chars.peek() == Some(&',') {
                    let mut look = chars.clone();
                    look.next();
                    if look.peek().is_some_and(|d| d.is_ascii_digit()) {
                        chars.next(); // ,
                        let mut d2 = 0;
                        while d2 < 2 && chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                            chars.next();
                            d2 += 1;
                        }
                    }
                }
            }
            c if c.is_control() => {} // never relay stray control characters
            c => out.push(c),
        }
    }
    out
}

/// Shorten to at most `max` characters, adding an ellipsis when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Token bucket, driven by caller-supplied millisecond timestamps so tests are
/// fully deterministic.
#[derive(Debug, Clone)]
pub struct Bucket {
    tokens: f64,
    last_ms: u64,
}

impl Bucket {
    pub fn new(rate: RateLimit, now_ms: u64) -> Self {
        Bucket {
            tokens: rate.burst as f64,
            last_ms: now_ms,
        }
    }

    /// Try to take one token. Returns false when the sender is over the limit.
    pub fn allow(&mut self, rate: RateLimit, now_ms: u64) -> bool {
        let per_ms = rate.burst as f64 / (rate.per_secs as f64 * 1000.0);
        let elapsed = now_ms.saturating_sub(self.last_ms) as f64;
        self.tokens = (self.tokens + elapsed * per_ms).min(rate.burst as f64);
        self.last_ms = now_ms;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_conservative() {
        let p = Policy::default();
        assert_eq!(p.mode, Mode::Preview);
        assert!(matches!(p.presence, Presence::Lazy { .. }));
    }

    #[test]
    fn set_parses_values_and_rejects_garbage() {
        let mut p = Policy::default();
        p.set("rate", "3/20").unwrap();
        assert_eq!(
            p.rate,
            RateLimit {
                burst: 3,
                per_secs: 20
            }
        );
        p.set("presence", "full").unwrap();
        assert_eq!(p.presence, Presence::Full);
        p.set("presence", "lazy 5").unwrap();
        assert_eq!(p.presence, Presence::Lazy { idle_secs: 300 });
        p.set("require-account", "on").unwrap();
        assert!(p.require_account_in);
        assert!(p.set("rate", "abc").is_err());
        assert!(p.set("rate", "0/10").is_err());
        assert!(p.set("max-line", "5").is_err());
        assert!(p.set("nope", "1").is_err());
        assert!(p.set("share-out", "maybe").is_err());
    }

    #[test]
    fn every_documented_key_is_settable() {
        let mut p = Policy::default();
        for (k, _) in Policy::keys(Lang::En) {
            let v = match k {
                "presence" => "full",
                "lang" => "it",
                "rate" => "5/10",
                "min-presence" | "max-line" => "100",
                _ => "on",
            };
            p.set(k, v).unwrap_or_else(|e| panic!("{k}: {e}"));
        }
        for (k, _) in Policy::keys(Lang::It) {
            assert!(
                Policy::keys(Lang::En).iter().any(|(e, _)| e == &k),
                "{k} missing in English"
            );
        }
    }

    #[test]
    fn strips_irc_formatting() {
        assert_eq!(
            strip_formatting("\x02bold\x02 and \x0304,12colour\x03 text"),
            "bold and colour text"
        );
        assert_eq!(strip_formatting("\x0301,hello"), ",hello"); // comma without bg digits stays
        assert_eq!(strip_formatting("a\x07b"), "ab");
    }

    #[test]
    fn truncates_by_characters_not_bytes() {
        assert_eq!(truncate_chars("àèìòù", 10), "àèìòù");
        assert_eq!(truncate_chars("àèìòùàèìòù", 5), "àèìò…");
    }

    #[test]
    fn bucket_limits_then_refills() {
        let rate = RateLimit {
            burst: 3,
            per_secs: 3,
        };
        let mut b = Bucket::new(rate, 0);
        assert!(b.allow(rate, 0));
        assert!(b.allow(rate, 0));
        assert!(b.allow(rate, 0));
        assert!(!b.allow(rate, 0));
        assert!(!b.allow(rate, 500));
        assert!(b.allow(rate, 1_100)); // ~1 token refilled per second
        assert!(!b.allow(rate, 1_100));
        assert!(b.allow(rate, 10_000)); // full again, capped at burst
    }
}
