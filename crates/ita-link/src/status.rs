//! A tiny read-only HTTP status page.
//!
//! It shows only what owners explicitly made public (`SET #chan public on`):
//! channel name, network and mode, plus aggregate numbers. Never people, never
//! chat. Bind it to localhost and put a reverse proxy in front for the internet.

use anyhow::Result;
use itl_core::engine::Overview;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub type Shared = Arc<Mutex<Overview>>;

pub fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

pub fn render_html(o: &Overview) -> String {
    let mut rows = String::new();
    for l in &o.links {
        let chans: Vec<String> = l
            .channels
            .iter()
            .map(|c| {
                format!(
                    "<li><strong>{}</strong> <span class=\"net\">{}</span> <span class=\"mode {}\">{}{}</span></li>",
                    escape(&c.name),
                    escape(&c.network),
                    escape(&c.mode),
                    escape(&c.mode),
                    if c.paused { " (paused)" } else { "" }
                )
            })
            .collect();
        let hidden = if l.hidden > 0 { format!("<li class=\"muted\">+ {} channel(s) not listed</li>", l.hidden) } else { String::new() };
        rows.push_str(&format!("<section class=\"link\"><ul>{}{}</ul></section>", chans.join(""), hidden));
    }
    if rows.is_empty() {
        rows.push_str("<p class=\"muted\">No public links yet. / Nessun link pubblico per ora.</p>");
    }
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Ita Link status</title>\
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:44rem;margin:2rem auto;padding:0 1rem;color:#1b1b1b;background:#fafafa}}\
h1{{margin-bottom:.2rem}}.muted{{color:#666}}.link{{background:#fff;border:1px solid #ddd;border-radius:8px;padding:.5rem 1rem;margin:1rem 0}}\
ul{{list-style:none;padding:0;margin:.3rem 0}}li{{padding:.15rem 0}}.net{{color:#555}}.mode{{font-size:.8rem;border-radius:4px;padding:0 .4rem}}\
.live{{background:#d9f3dd}}.preview{{background:#fff3cd}}.off{{background:#eee}}\
@media (prefers-color-scheme:dark){{body{{background:#161616;color:#e8e8e8}}.link{{background:#1f1f1f;border-color:#333}}.net,.muted{{color:#aaa}}.live{{background:#1e4d2b}}.preview{{background:#5a4a12}}.off{{background:#333}}}}\
</style></head><body><h1>Ita Link</h1>\
<p class=\"muted\">Channels linked across IRC networks. Only channels whose owners chose to be listed are shown. / Canali collegati tra reti IRC: compaiono solo quelli i cui owner lo hanno scelto.</p>\
{rows}\
<p class=\"muted\">{} links &middot; {} channels ({} live) &middot; {} lines relayed</p>\
</body></html>",
        o.totals.links, o.totals.channels, o.totals.live_channels, o.totals.lines_relayed
    )
}

fn response(status: &str, ctype: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\n\
X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn handle(mut s: TcpStream, shared: Shared) {
    let mut buf = vec![0u8; 4096];
    let n = match tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let first = req.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = path.split('?').next().unwrap_or("");
    let reply = if method != "GET" && method != "HEAD" {
        response("405 Method Not Allowed", "text/plain; charset=utf-8", "read-only")
    } else {
        let o = shared.lock().map(|g| g.clone()).unwrap_or_default();
        match path {
            "/" | "/index.html" => response("200 OK", "text/html; charset=utf-8", &render_html(&o)),
            "/status.json" => response("200 OK", "application/json", &serde_json::to_string(&o).unwrap_or_default()),
            _ => response("404 Not Found", "text/plain; charset=utf-8", "not found"),
        }
    };
    let _ = s.write_all(reply.as_bytes()).await;
    let _ = s.shutdown().await;
}

/// Start the server; returns the address it is listening on.
pub async fn serve(listen: SocketAddr, shared: Shared) -> Result<SocketAddr> {
    let l = TcpListener::bind(listen).await?;
    let addr = l.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l.accept().await else { break };
            let sh = shared.clone();
            tokio::spawn(handle(sock, sh));
        }
    });
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use itl_core::engine::{ChanView, LinkView, Totals};

    #[test]
    fn escapes_everything_dangerous() {
        assert_eq!(escape("<script>\"x\"&'"), "&lt;script&gt;&quot;x&quot;&amp;&#39;");
    }

    #[test]
    fn page_shows_listed_channels_only_and_escapes_them() {
        let o = Overview {
            links: vec![LinkView {
                channels: vec![ChanView { network: "A <b>".into(), name: "#italy".into(), mode: "live".into(), paused: false }],
                hidden: 1,
            }],
            totals: Totals { links: 1, channels: 2, live_channels: 1, puppets: 3, lines_relayed: 42 },
        };
        let h = render_html(&o);
        assert!(h.contains("#italy") && h.contains("A &lt;b&gt;") && !h.contains("A <b>"));
        assert!(h.contains("+ 1 channel(s) not listed"));
        assert!(h.contains("42 lines relayed"));
        assert!(render_html(&Overview::default()).contains("No public links yet"));
    }
}
