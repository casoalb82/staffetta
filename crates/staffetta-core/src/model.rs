//! Basic vocabulary shared by the whole core: networks, channels, users.
//!
//! Everything here is plain data. Names are always stored *case-folded* with
//! the RFC 1459 rules (`[]\~` fold to `{}|^`), because that is what the vast
//! majority of IRC servers use for nicks and channel names.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Fold a nick or channel name the way IRC servers compare them.
pub fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            '~' => '^',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// A network, e.g. `chatnet`. Always lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetId(pub String);

impl NetId {
    pub fn new(s: &str) -> Self {
        NetId(s.trim().to_ascii_lowercase())
    }
}

impl fmt::Display for NetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One channel on one network. The channel name is case-folded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChanKey {
    pub net: NetId,
    pub chan: String,
}

impl ChanKey {
    pub fn new(net: &str, chan: &str) -> Self {
        ChanKey {
            net: NetId::new(net),
            chan: fold(chan),
        }
    }
}

impl fmt::Display for ChanKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.net, self.chan)
    }
}

/// Stable internal identity of a real person seen on some network. The core
/// never lets this id (or anything derived from it by accident) leak into IRC
/// output: on the wire a user only ever has a [`Face`](crate::face::Face).
pub type Uid = u64;

/// What the network tells us about a real user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenUser {
    pub nick: String,
    pub ident: String,
    pub host: String,
    /// Services account, when the network tells us (account-tag, extended-join,
    /// WHOX...). `None` means "unknown or not logged in".
    pub account: Option<String>,
    /// True when the user is flagged as a bot (IRCv3 `bot` tag / mode).
    pub is_bot: bool,
}

impl SeenUser {
    pub fn new(nick: &str, ident: &str, host: &str) -> Self {
        SeenUser {
            nick: nick.to_string(),
            ident: ident.to_string(),
            host: host.to_string(),
            account: None,
            is_bot: false,
        }
    }

    pub fn with_account(mut self, account: &str) -> Self {
        self.account = Some(account.to_string());
        self
    }

    /// `nick!ident@host`, the form masks are matched against.
    pub fn mask(&self) -> String {
        format!("{}!{}@{}", self.nick, self.ident, self.host)
    }
}

/// A user's status in a channel, from the channel's own point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Role {
    Normal,
    Voice,
    Op,
}

/// The kind of line a user said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SayKind {
    Message,
    Action,
    Notice,
}

/// Case-insensitive glob match with `*` and `?`. Used for ban-style masks.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = fold(pattern).chars().collect();
    let t: Vec<char> = fold(text).chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Does `mask` match this user? A mask is either `account:<glob>` (matches the
/// services account) or a normal `nick!ident@host` glob.
pub fn mask_matches(mask: &str, user: &SeenUser) -> bool {
    if let Some(acc) = mask.strip_prefix("account:") {
        return user.account.as_deref().is_some_and(|a| glob(acc, a));
    }
    glob(mask, &user.mask())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_follows_rfc1459() {
        assert_eq!(fold("Nick[A]\\~"), "nick{a}|^");
        assert_eq!(fold("#ITALY"), "#italy");
    }

    #[test]
    fn chankey_is_case_insensitive() {
        assert_eq!(
            ChanKey::new("ChatNet", "#Italy"),
            ChanKey::new("chatnet", "#ITALY")
        );
    }

    #[test]
    fn glob_basics() {
        assert!(glob("*", "anything"));
        assert!(glob("a*c", "abbbc"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "abbc"));
        assert!(glob("*!*@*.example.org", "n!u@host.example.org"));
        assert!(!glob("*!*@*.example.org", "n!u@example.net"));
        assert!(glob("FOO*", "foobar"));
        assert!(glob("*a*b*", "xxaYYbzz"));
        assert!(!glob("*a*b*", "xxbYYazz"));
    }

    #[test]
    fn account_masks() {
        let u = SeenUser::new("mario", "m", "h.example").with_account("Mario");
        assert!(mask_matches("account:mario", &u));
        assert!(mask_matches("account:m*", &u));
        assert!(!mask_matches("account:luigi", &u));
        let anon = SeenUser::new("mario", "m", "h.example");
        assert!(!mask_matches("account:*", &anon));
        assert!(mask_matches("mario!*@*", &anon));
    }
}
