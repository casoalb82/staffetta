//! The owner's command language: plain messages to the service nick.
//!
//! Every command that changes a channel names that channel first
//! (`SET #chan rate 5/10`), so there is never any doubt which channel a
//! command is about and the same command works from any window.

use crate::model::ChanKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help(Option<String>),
    /// Link `mine` with `other` (creates the link, or asks to join one).
    Link { mine: String, other: ChanKey },
    /// Agree to a pending link between `mine` and `other`.
    Accept { mine: String, other: ChanKey },
    /// Refuse or withdraw a pending request.
    Deny { mine: String, other: ChanKey },
    Unlink { chan: String },
    Pause { chan: String },
    Resume { chan: String },
    /// Switch the channel's mode: `OFF`, `PREVIEW` or `LIVE`.
    SetMode { chan: String, mode: ModeWord },
    Set { chan: String, key: String, value: String },
    Block { chan: String, mask: String },
    Unblock { chan: String, mask: String },
    Mute { chan: String, other: ChanKey },
    Unmute { chan: String, other: ChanKey },
    Status { chan: String },
    /// Show a few recent lines that preview mode would have shown.
    Sample { chan: String },
    Audit { chan: String, count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeWord {
    Off,
    Preview,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("empty command")]
    Empty,
    #[error("unknown command '{0}' - try HELP")]
    Unknown(String),
    #[error("usage: {0}")]
    Usage(&'static str),
}

fn chan_arg(a: Option<&str>, usage: &'static str) -> Result<String, ParseError> {
    match a {
        Some(c) if c.starts_with(['#', '&', '+', '!']) => Ok(c.to_string()),
        _ => Err(ParseError::Usage(usage)),
    }
}

fn other_arg(net: Option<&str>, chan: Option<&str>, usage: &'static str) -> Result<ChanKey, ParseError> {
    match (net, chan) {
        (Some(n), Some(c)) if c.starts_with(['#', '&', '+', '!']) && !n.is_empty() => Ok(ChanKey::new(n, c)),
        _ => Err(ParseError::Usage(usage)),
    }
}

pub fn parse(line: &str) -> Result<Command, ParseError> {
    let mut it = line.split_whitespace();
    let verb = it.next().ok_or(ParseError::Empty)?.to_ascii_uppercase();
    let a: Vec<&str> = it.collect();
    let g = |i: usize| a.get(i).copied();

    match verb.as_str() {
        "HELP" | "?" => Ok(Command::Help(g(0).map(|s| s.to_ascii_uppercase()))),
        "LINK" => {
            let u = "LINK <#yourchannel> <network> <#theirchannel>";
            Ok(Command::Link { mine: chan_arg(g(0), u)?, other: other_arg(g(1), g(2), u)? })
        }
        "ACCEPT" => {
            let u = "ACCEPT <#yourchannel> <network> <#theirchannel>";
            Ok(Command::Accept { mine: chan_arg(g(0), u)?, other: other_arg(g(1), g(2), u)? })
        }
        "DENY" => {
            let u = "DENY <#yourchannel> <network> <#theirchannel>";
            Ok(Command::Deny { mine: chan_arg(g(0), u)?, other: other_arg(g(1), g(2), u)? })
        }
        "UNLINK" => Ok(Command::Unlink { chan: chan_arg(g(0), "UNLINK <#yourchannel>")? }),
        "PAUSE" => Ok(Command::Pause { chan: chan_arg(g(0), "PAUSE <#yourchannel>")? }),
        "RESUME" => Ok(Command::Resume { chan: chan_arg(g(0), "RESUME <#yourchannel>")? }),
        "OFF" => Ok(Command::SetMode { chan: chan_arg(g(0), "OFF <#yourchannel>")?, mode: ModeWord::Off }),
        "PREVIEW" => Ok(Command::SetMode { chan: chan_arg(g(0), "PREVIEW <#yourchannel>")?, mode: ModeWord::Preview }),
        "LIVE" => Ok(Command::SetMode { chan: chan_arg(g(0), "LIVE <#yourchannel>")?, mode: ModeWord::Live }),
        "SET" => {
            let u = "SET <#yourchannel> <setting> <value>   (HELP SET lists the settings)";
            let chan = chan_arg(g(0), u)?;
            let key = g(1).ok_or(ParseError::Usage(u))?.to_string();
            let value = a.get(2..).map(|v| v.join(" ")).unwrap_or_default();
            if value.is_empty() {
                return Err(ParseError::Usage(u));
            }
            Ok(Command::Set { chan, key, value })
        }
        "BLOCK" => {
            let u = "BLOCK <#yourchannel> <mask>   e.g. *!*@*.example.org or account:name";
            Ok(Command::Block { chan: chan_arg(g(0), u)?, mask: g(1).ok_or(ParseError::Usage(u))?.to_string() })
        }
        "UNBLOCK" => {
            let u = "UNBLOCK <#yourchannel> <mask>";
            Ok(Command::Unblock { chan: chan_arg(g(0), u)?, mask: g(1).ok_or(ParseError::Usage(u))?.to_string() })
        }
        "MUTE" => {
            let u = "MUTE <#yourchannel> <network> <#theirchannel>";
            Ok(Command::Mute { chan: chan_arg(g(0), u)?, other: other_arg(g(1), g(2), u)? })
        }
        "UNMUTE" => {
            let u = "UNMUTE <#yourchannel> <network> <#theirchannel>";
            Ok(Command::Unmute { chan: chan_arg(g(0), u)?, other: other_arg(g(1), g(2), u)? })
        }
        "STATUS" => Ok(Command::Status { chan: chan_arg(g(0), "STATUS <#yourchannel>")? }),
        "SAMPLE" => Ok(Command::Sample { chan: chan_arg(g(0), "SAMPLE <#yourchannel>")? }),
        "AUDIT" => {
            let chan = chan_arg(g(0), "AUDIT <#yourchannel> [count]")?;
            let count = g(1).and_then(|n| n.parse().ok()).unwrap_or(10usize).clamp(1, 30);
            Ok(Command::Audit { chan, count })
        }
        other => Err(ParseError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_link_and_folds_names() {
        let c = parse("link #Italia AzzurRa #Italy").unwrap();
        assert_eq!(
            c,
            Command::Link { mine: "#Italia".into(), other: ChanKey::new("azzurra", "#italy") }
        );
    }

    #[test]
    fn set_keeps_the_whole_value() {
        let c = parse("SET #a presence lazy 5").unwrap();
        assert_eq!(
            c,
            Command::Set { chan: "#a".into(), key: "presence".into(), value: "lazy 5".into() }
        );
    }

    #[test]
    fn usage_errors_are_helpful() {
        assert!(matches!(parse("LINK #a"), Err(ParseError::Usage(_))));
        assert!(matches!(parse("LINK a net #b"), Err(ParseError::Usage(_))));
        assert!(matches!(parse("SET #a rate"), Err(ParseError::Usage(_))));
        assert!(matches!(parse("DANCE"), Err(ParseError::Unknown(_))));
        assert!(matches!(parse("   "), Err(ParseError::Empty)));
    }

    #[test]
    fn audit_count_is_clamped() {
        assert_eq!(parse("AUDIT #a 999").unwrap(), Command::Audit { chan: "#a".into(), count: 30 });
        assert_eq!(parse("AUDIT #a").unwrap(), Command::Audit { chan: "#a".into(), count: 10 });
    }

    #[test]
    fn modes() {
        assert_eq!(
            parse("live #a").unwrap(),
            Command::SetMode { chan: "#a".into(), mode: ModeWord::Live }
        );
        assert_eq!(parse("HELP set").unwrap(), Command::Help(Some("SET".into())));
    }
}
