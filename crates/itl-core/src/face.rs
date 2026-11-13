//! The "face" a remote person wears on a network: nick, ident, realname.
//!
//! Design rules (learned from PyLink's problems):
//!
//! * The nick carries a short network tag, so nobody can be mistaken for a
//!   local user. The tag style is configurable.
//! * The **ident is derived from the person's origin identity**, not random:
//!   `<net-tag>-<6 hex>`. The same person always gets the same ident, and a
//!   channel op can ban exactly that person with an ordinary `*!*ident@*`
//!   mask - without banning every puppet at once. (PyLink's suffixed nicks
//!   made ordinary bans impossible.)
//! * The realname always says plainly that this is a relayed user.

use crate::model::{fold, NetId, SeenUser};
use serde::{Deserialize, Serialize};

/// How the origin-network tag is attached to the nick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NickStyle {
    /// `nick|tag` (default)
    #[default]
    Pipe,
    /// `nick[tag]`
    Bracket,
    /// `nick_tag`
    Underscore,
}

impl NickStyle {
    fn join(&self, nick: &str, tag: &str) -> String {
        match self {
            NickStyle::Pipe => format!("{nick}|{tag}"),
            NickStyle::Bracket => format!("{nick}[{tag}]"),
            NickStyle::Underscore => format!("{nick}_{tag}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Face {
    pub nick: String,
    pub ident: String,
    pub realname: String,
}

/// Stable 64-bit FNV-1a hash. Not a security feature: it only has to be
/// deterministic and well spread so identical people get identical idents.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Keep only characters that are valid in a nick on practically every IRCd.
fn clean_nick(nick: &str) -> String {
    let mut out: String = nick
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "[]\\`_^{|}-".contains(*c))
        .collect();
    if out.is_empty() {
        out.push_str("guest");
    }
    // A nick must not start with a digit or '-'.
    if out.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        out.insert(0, '_');
    }
    out
}

/// 2-4 lowercase letters/digits taken from the network name.
pub fn net_tag(net: &NetId) -> String {
    let t: String = net
        .0
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(3)
        .collect();
    if t.is_empty() {
        "x".to_string()
    } else {
        t
    }
}

/// The stable identity string a person is hashed from. A services account is
/// the best anchor; otherwise we fall back to the nick (documented weakness).
fn identity_key(net: &NetId, user: &SeenUser) -> String {
    match &user.account {
        Some(a) => format!("{}:acct:{}", net, fold(a)),
        None => format!("{}:nick:{}", net, fold(&user.nick)),
    }
}

/// The ident a person gets on every remote network, e.g. `az-3fa91c`.
pub fn ident_for(origin: &NetId, user: &SeenUser) -> String {
    let h = fnv1a(&identity_key(origin, user));
    format!("{}-{:06x}", net_tag(origin), (h & 0xff_ffff) as u32)
}

/// Build the face of `user` (who lives on `origin`) for use on a remote
/// network whose nick limit is `max_nick`. `taken` tells whether a candidate
/// nick is already used on that remote network.
pub fn make_face(
    origin: &NetId,
    origin_label: &str,
    user: &SeenUser,
    style: &NickStyle,
    max_nick: usize,
    taken: &dyn Fn(&str) -> bool,
) -> Face {
    let tag = net_tag(origin);
    let base = clean_nick(&user.nick);
    let overhead = style.join("", &tag).chars().count();
    let room = max_nick.saturating_sub(overhead).max(1);

    let mut stem: String = base.chars().take(room).collect();
    let mut nick = style.join(&stem, &tag);
    let mut n = 1u32;
    while taken(&nick) {
        // Collision: shorten the stem and add a counter. Never lose the tag.
        let suffix = n.to_string();
        let keep = room.saturating_sub(suffix.len()).max(1);
        stem = base.chars().take(keep).collect::<String>() + &suffix;
        nick = style.join(&stem, &tag);
        n += 1;
        if n > 99 {
            break;
        }
    }

    Face {
        nick,
        ident: ident_for(origin, user),
        realname: format!(
            "{} on {} via Ita Link (relayed user)",
            user.nick, origin_label
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(nick: &str) -> SeenUser {
        SeenUser::new(nick, "id", "host.example")
    }

    #[test]
    fn ident_is_stable_and_account_anchored() {
        let net = NetId::new("azzurra");
        let a = u("mario").with_account("Mario");
        let b = u("mario_away").with_account("mario"); // other nick, same account
        assert_eq!(ident_for(&net, &a), ident_for(&net, &b));
        assert!(ident_for(&net, &a).starts_with("azz-"));
        assert!(ident_for(&net, &a).len() <= 10);
    }

    #[test]
    fn ident_differs_between_people_and_networks() {
        let a = u("mario");
        let b = u("luigi");
        let n1 = NetId::new("azzurra");
        let n2 = NetId::new("chatnet");
        assert_ne!(ident_for(&n1, &a), ident_for(&n1, &b));
        assert_ne!(ident_for(&n1, &a), ident_for(&n2, &a));
    }

    #[test]
    fn nick_carries_tag_and_respects_length() {
        let net = NetId::new("azzurra");
        let f = make_face(
            &net,
            "Azzurra",
            &u("averyveryverylongnickname"),
            &NickStyle::Pipe,
            16,
            &|_| false,
        );
        assert!(f.nick.chars().count() <= 16, "{}", f.nick);
        assert!(f.nick.ends_with("|azz"), "{}", f.nick);
        assert!(f.realname.contains("relayed"));
    }

    #[test]
    fn collisions_never_lose_the_tag() {
        let net = NetId::new("azzurra");
        let first = make_face(&net, "Azzurra", &u("mario"), &NickStyle::Pipe, 30, &|_| {
            false
        });
        let taken = first.nick.clone();
        let second = make_face(&net, "Azzurra", &u("mario"), &NickStyle::Pipe, 30, &|n| {
            n == taken
        });
        assert_ne!(first.nick, second.nick);
        assert!(second.nick.ends_with("|azz"));
    }

    #[test]
    fn weird_nicks_are_cleaned() {
        let net = NetId::new("x");
        let f = make_face(&net, "X", &u("9lives!"), &NickStyle::Bracket, 30, &|_| {
            false
        });
        assert!(f.nick.starts_with('_'), "{}", f.nick);
        assert!(f.nick.ends_with("[x]"));
        let g = make_face(&net, "X", &u("!!!"), &NickStyle::Pipe, 30, &|_| false);
        assert!(g.nick.starts_with("guest"));
    }
}
