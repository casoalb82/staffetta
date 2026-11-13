use std::collections::HashMap;

use crate::message::{Prefix, RawMessage};

/// Parses one raw IRC line (CRLF already stripped or not, either is fine)
/// into a `RawMessage`. Returns `None` for an empty line or one with no
/// command.
pub fn parse(line: &str) -> Option<RawMessage> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        return None;
    }

    let mut rest = line;
    let mut tags = HashMap::new();

    if let Some(stripped) = rest.strip_prefix('@') {
        let (tag_str, remainder) = split_once_space(stripped);
        for tag in tag_str.split(';') {
            if tag.is_empty() {
                continue;
            }
            match tag.split_once('=') {
                Some((k, v)) => {
                    tags.insert(k.to_string(), Some(unescape_tag_value(v)));
                }
                None => {
                    tags.insert(tag.to_string(), None);
                }
            }
        }
        rest = remainder;
    }

    let mut prefix = None;
    if let Some(stripped) = rest.strip_prefix(':') {
        let (prefix_str, remainder) = split_once_space(stripped);
        prefix = Some(parse_prefix(prefix_str));
        rest = remainder;
    }

    let (command, remainder) = split_once_space(rest);
    if command.is_empty() {
        return None;
    }
    let command = command.to_uppercase();
    let params = parse_params(remainder);

    Some(RawMessage {
        tags,
        prefix,
        command,
        params,
    })
}

/// Splits on the first space and trims any extra leading spaces off the
/// remainder, so runs of multiple spaces between tokens don't produce
/// empty params.
fn split_once_space(s: &str) -> (&str, &str) {
    match s.split_once(' ') {
        Some((a, b)) => (a, b.trim_start_matches(' ')),
        None => (s, ""),
    }
}

fn parse_prefix(s: &str) -> Prefix {
    if let Some((name_user, host)) = s.split_once('@') {
        if let Some((name, user)) = name_user.split_once('!') {
            Prefix {
                name: name.to_string(),
                user: Some(user.to_string()),
                host: Some(host.to_string()),
            }
        } else {
            Prefix {
                name: name_user.to_string(),
                user: None,
                host: Some(host.to_string()),
            }
        }
    } else if let Some((name, user)) = s.split_once('!') {
        Prefix {
            name: name.to_string(),
            user: Some(user.to_string()),
            host: None,
        }
    } else {
        Prefix {
            name: s.to_string(),
            user: None,
            host: None,
        }
    }
}

fn parse_params(rest: &str) -> Vec<String> {
    let mut params = Vec::new();
    let mut rest = rest;
    loop {
        if rest.is_empty() {
            break;
        }
        if let Some(trailing) = rest.strip_prefix(':') {
            params.push(trailing.to_string());
            break;
        }
        match rest.split_once(' ') {
            Some((first, remainder)) => {
                params.push(first.to_string());
                rest = remainder.trim_start_matches(' ');
            }
            None => {
                params.push(rest.to_string());
                break;
            }
        }
    }
    params
}

/// IRCv3 message-tags value unescaping: `\:` -> `;`, `\s` -> space,
/// `\\` -> `\`, `\r`/`\n` -> CR/LF. An unknown escape drops the backslash
/// and keeps the following char; a trailing lone backslash is dropped.
fn unescape_tag_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(':') => out.push(';'),
                Some('s') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_message_no_prefix() {
        let msg = parse("PING :tungsten.libera.chat").unwrap();
        assert!(msg.tags.is_empty());
        assert!(msg.prefix.is_none());
        assert_eq!(msg.command, "PING");
        assert_eq!(msg.params, vec!["tungsten.libera.chat"]);
    }

    #[test]
    fn message_with_server_prefix() {
        let msg = parse(":irc.example.com 001 abcX :Welcome to the network").unwrap();
        let prefix = msg.prefix.unwrap();
        assert_eq!(prefix.name, "irc.example.com");
        assert!(prefix.user.is_none());
        assert!(prefix.host.is_none());
        assert_eq!(msg.command, "001");
        assert_eq!(msg.params, vec!["abcX", "Welcome to the network"]);
    }

    #[test]
    fn message_with_full_nick_prefix_and_trailing() {
        let msg = parse(":Nick!user@host PRIVMSG #chatnet :Hello world").unwrap();
        let prefix = msg.prefix.unwrap();
        assert_eq!(prefix.name, "Nick");
        assert_eq!(prefix.user.as_deref(), Some("user"));
        assert_eq!(prefix.host.as_deref(), Some("host"));
        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.params, vec!["#chatnet", "Hello world"]);
    }

    #[test]
    fn message_with_only_middle_params() {
        let msg = parse("MODE #chan +o Nick").unwrap();
        assert_eq!(msg.command, "MODE");
        assert_eq!(msg.params, vec!["#chan", "+o", "Nick"]);
    }

    #[test]
    fn message_with_tags_including_escaped_values() {
        // raw tag value literally: a\sb\:c\\d  -> unescaped: "a b;c\d"
        let msg = parse(r"@foo=a\sb\:c\\d;flag :nick!u@h PRIVMSG #c :hi").unwrap();
        assert_eq!(msg.tags.get("foo").cloned().flatten().as_deref(), Some("a b;c\\d"));
        assert_eq!(msg.tags.get("flag").cloned(), Some(None));
        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.params, vec!["#c", "hi"]);
    }

    #[test]
    fn cap_ls_multi_value_trailing() {
        let msg = parse("CAP * LS :multi-prefix sasl message-tags").unwrap();
        assert_eq!(msg.command, "CAP");
        assert_eq!(msg.params, vec!["*", "LS", "multi-prefix sasl message-tags"]);
    }

    #[test]
    fn crlf_is_stripped() {
        let msg = parse("PING :abc\r\n").unwrap();
        assert_eq!(msg.params, vec!["abc"]);
    }

    #[test]
    fn empty_line_is_none() {
        assert!(parse("").is_none());
        assert!(parse("\r\n").is_none());
    }

    #[test]
    fn tags_only_no_command_is_none() {
        assert!(parse("@id=1 ").is_none());
    }
}
