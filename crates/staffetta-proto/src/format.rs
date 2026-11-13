use crate::message::RawMessage;

/// Formats a `RawMessage` back into a wire line (no trailing CRLF --
/// the caller's framing layer adds that).
pub fn format(msg: &RawMessage) -> String {
    let mut out = String::new();

    if !msg.tags.is_empty() {
        out.push('@');
        let mut first = true;
        for (k, v) in &msg.tags {
            if !first {
                out.push(';');
            }
            first = false;
            out.push_str(k);
            if let Some(v) = v {
                out.push('=');
                out.push_str(&escape_tag_value(v));
            }
        }
        out.push(' ');
    }

    if let Some(prefix) = &msg.prefix {
        out.push(':');
        out.push_str(&prefix.name);
        if let Some(user) = &prefix.user {
            out.push('!');
            out.push_str(user);
        }
        if let Some(host) = &prefix.host {
            out.push('@');
            out.push_str(host);
        }
        out.push(' ');
    }

    out.push_str(&msg.command);

    let last_index = msg.params.len().saturating_sub(1);
    for (i, param) in msg.params.iter().enumerate() {
        out.push(' ');
        let is_last = i == last_index;
        if is_last && (param.contains(' ') || param.starts_with(':') || param.is_empty()) {
            out.push(':');
        }
        out.push_str(param);
    }

    out
}

/// IRCv3 message-tags value escaping, inverse of the parser's unescaping.
fn escape_tag_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn round_trips_plain_message() {
        let msg = parse(":Nick!user@host PRIVMSG #chatnet :Hello world").unwrap();
        let line = format(&msg);
        assert_eq!(line, ":Nick!user@host PRIVMSG #chatnet :Hello world");
        // and it re-parses back to an identical struct
        assert_eq!(parse(&line).unwrap(), msg);
    }

    #[test]
    fn round_trips_middle_params_without_forcing_trailing_colon() {
        let msg = parse("MODE #chan +o Nick").unwrap();
        let line = format(&msg);
        assert_eq!(line, "MODE #chan +o Nick");
    }

    #[test]
    fn round_trips_single_tag_with_escaping() {
        // "hi" has no space/leading colon, so format() is free to omit the
        // trailing ':' -- a leading colon is only required when it's needed
        // for re-parsing, not to reproduce the exact original bytes.
        let msg = parse(r"@foo=a\sb\:c\\d :nick!u@h PRIVMSG #c :hi").unwrap();
        let line = format(&msg);
        assert_eq!(line, r"@foo=a\sb\:c\\d :nick!u@h PRIVMSG #c hi");
        assert_eq!(parse(&line).unwrap(), msg);
    }

    #[test]
    fn forces_trailing_colon_when_last_param_has_a_space() {
        let msg = parse(":nick!u@h PRIVMSG #c :hello world").unwrap();
        let line = format(&msg);
        assert_eq!(line, ":nick!u@h PRIVMSG #c :hello world");
        assert_eq!(parse(&line).unwrap(), msg);
    }
}
