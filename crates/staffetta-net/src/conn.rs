//! Raw connection handling: TCP, optional TLS (always verified), line framing.
//!
//! Plain-text connections are only allowed towards the loopback interface
//! (that is what the test networks use). Anything else must use TLS, and the
//! certificate is always verified: there is no "insecure" switch.

use anyhow::{bail, Context, Result};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::{lookup_host, TcpSocket};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

const MAX_LINE: usize = 8192;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Stream = Box<dyn Io>;

#[derive(Debug, Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Local address to connect from (puppets may use a dedicated address).
    pub source_ip: Option<IpAddr>,
}

pub struct LineReader {
    inner: BufReader<ReadHalf<Stream>>,
    buf: Vec<u8>,
}

pub struct LineWriter {
    inner: WriteHalf<Stream>,
}

fn tls_connector() -> Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        bail!("no trusted root certificates found in the system store");
    }
    let cfg = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(cfg)))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Check a target before connecting. Used at config load time too.
pub fn validate(t: &Target) -> Result<()> {
    if !t.tls && !is_loopback_host(&t.host) {
        bail!(
            "{}: plain-text IRC is only allowed to localhost; use tls = true",
            t.host
        );
    }
    Ok(())
}

pub async fn connect(t: &Target) -> Result<(LineReader, LineWriter)> {
    validate(t)?;
    let timeout = Duration::from_secs(20);
    let stream: Stream = tokio::time::timeout(timeout, async {
        let mut last: Option<anyhow::Error> = None;
        for addr in lookup_host((t.host.as_str(), t.port))
            .await
            .context("resolve")?
        {
            if let Some(src) = t.source_ip {
                if src.is_ipv6() != addr.is_ipv6() {
                    continue;
                }
            }
            let sock = if addr.is_ipv6() {
                TcpSocket::new_v6()?
            } else {
                TcpSocket::new_v4()?
            };
            if let Some(src) = t.source_ip {
                sock.bind(std::net::SocketAddr::new(src, 0))
                    .context("bind source address")?;
            }
            match sock.connect(addr).await {
                Ok(tcp) => {
                    let _ = tcp.set_nodelay(true);
                    if t.tls {
                        let name = ServerName::try_from(t.host.clone()).context("server name")?;
                        let s = tls_connector()?
                            .connect(name, tcp)
                            .await
                            .context("TLS handshake")?;
                        return Ok::<Stream, anyhow::Error>(Box::new(s));
                    }
                    return Ok(Box::new(tcp));
                }
                Err(e) => last = Some(e.into()),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no usable address for {}", t.host)))
    })
    .await
    .context("connect timed out")??;
    let (r, w) = tokio::io::split(stream);
    Ok((
        LineReader {
            inner: BufReader::new(r),
            buf: Vec::new(),
        },
        LineWriter { inner: w },
    ))
}

impl LineReader {
    /// Next line without its line ending, or `None` when the peer closed.
    /// Invalid UTF-8 is replaced, over-long lines are cut.
    pub async fn next(&mut self) -> Result<Option<String>> {
        self.buf.clear();
        let n = self.inner.read_until(b'\n', &mut self.buf).await?;
        if n == 0 {
            return Ok(None);
        }
        if self.buf.len() > MAX_LINE {
            self.buf.truncate(MAX_LINE);
        }
        while matches!(self.buf.last(), Some(b'\n' | b'\r')) {
            self.buf.pop();
        }
        Ok(Some(String::from_utf8_lossy(&self.buf).into_owned()))
    }
}

impl LineWriter {
    /// Send one line. CR/LF inside the text are removed so a relayed message
    /// can never smuggle in a second IRC command.
    pub async fn send(&mut self, line: &str) -> Result<()> {
        let clean: String = line
            .chars()
            .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
            .collect();
        self.inner.write_all(clean.as_bytes()).await?;
        self.inner.write_all(b"\r\n").await?;
        self.inner.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_only_to_loopback() {
        let t = |h: &str, tls| Target {
            host: h.into(),
            port: 6667,
            tls,
            source_ip: None,
        };
        assert!(validate(&t("127.0.0.1", false)).is_ok());
        assert!(validate(&t("localhost", false)).is_ok());
        assert!(validate(&t("::1", false)).is_ok());
        assert!(validate(&t("irc.example.org", false)).is_err());
        assert!(validate(&t("203.0.113.5", false)).is_err());
        assert!(validate(&t("irc.example.org", true)).is_ok());
    }
}
