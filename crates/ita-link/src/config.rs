//! Configuration file (TOML).
//!
//! ```toml
//! service_nick = "ItaLink"
//! state_dir = "state"
//!
//! [[network]]
//! id = "azzurra"
//! label = "Azzurra"
//! host = "irc.azzurra.chat"
//! port = 6697
//! tls = true
//! transport = "puppets"      # or "service-bot"
//! max_puppets = 60
//! ```

use anyhow::{bail, Context, Result};
use itl_core::engine::{EngineConfig, NetConfig, Transport};
use itl_core::face::NickStyle;
use itl_core::policy::Lang;
use itl_core::model::NetId;
use itl_net::conn::Target;
use itl_net::NetSpec;
use serde::Deserialize;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_nick")]
    pub service_nick: String,
    #[serde(default = "default_realname")]
    pub service_realname: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default)]
    pub require_account_for_owner: bool,
    #[serde(default)]
    pub relay_bots: bool,
    #[serde(default)]
    pub nick_style: NickStyleCfg,
    /// Language links start with: "it" (default) or "en". Owners can change
    /// it per channel with `SET #channel lang en`.
    #[serde(default = "default_lang")]
    pub default_lang: String,
    #[serde(default, rename = "network")]
    pub networks: Vec<NetworkCfg>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NickStyleCfg {
    #[default]
    Pipe,
    Bracket,
    Underscore,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkCfg {
    pub id: String,
    pub label: Option<String>,
    pub host: String,
    pub port: u16,
    #[serde(default = "yes")]
    pub tls: bool,
    #[serde(default)]
    pub transport: TransportCfg,
    #[serde(default = "default_max_puppets")]
    pub max_puppets: usize,
    #[serde(default = "default_max_nick")]
    pub max_nick: usize,
    /// Local address puppets connect from (optional).
    pub puppet_source_ip: Option<IpAddr>,
    /// IPv6 prefix, e.g. "2001:db8:1:2::/64": every puppet connects from its
    /// own address inside it (beats per-IP connection limits).
    pub puppet_ipv6_pool: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TransportCfg {
    #[default]
    Puppets,
    ServiceBot,
}

fn yes() -> bool {
    true
}
fn default_lang() -> String {
    "it".into()
}
fn default_nick() -> String {
    "ItaLink".into()
}
fn default_realname() -> String {
    "Ita Link channel relay service (relay bot, not a person)".into()
}
fn default_state_dir() -> PathBuf {
    "state".into()
}
fn default_max_puppets() -> usize {
    60
}
fn default_max_nick() -> usize {
    30
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.networks.len() < 2 {
            bail!("at least two [[network]] entries are needed to link anything");
        }
        let mut seen = std::collections::BTreeSet::new();
        for n in &self.networks {
            if !seen.insert(n.id.to_ascii_lowercase()) {
                bail!("network id '{}' appears twice", n.id);
            }
            itl_net::conn::validate(&Target { host: n.host.clone(), port: n.port, tls: n.tls, source_ip: None })?;
            if let Some(p) = &n.puppet_ipv6_pool {
                itl_net::pool::parse_prefix(p).map_err(|e| anyhow::anyhow!("{}: puppet_ipv6_pool: {e}", n.id))?;
                if n.puppet_source_ip.is_some() {
                    bail!("{}: use either puppet_source_ip or puppet_ipv6_pool, not both", n.id);
                }
            }
            if n.max_nick < 9 {
                bail!("{}: max_nick must be at least 9", n.id);
            }
        }
        if !["it", "en"].contains(&self.default_lang.to_ascii_lowercase().as_str()) {
            bail!("default_lang must be \"it\" or \"en\"");
        }
        if self.service_nick.is_empty() || self.service_nick.contains(' ') {
            bail!("service_nick is not a valid nick");
        }
        Ok(())
    }

    pub fn engine_config(&self) -> EngineConfig {
        let mut e = EngineConfig {
            service_nick: self.service_nick.clone(),
            nick_style: match self.nick_style {
                NickStyleCfg::Pipe => NickStyle::Pipe,
                NickStyleCfg::Bracket => NickStyle::Bracket,
                NickStyleCfg::Underscore => NickStyle::Underscore,
            },
            require_account_for_owner: self.require_account_for_owner,
            relay_bots: self.relay_bots,
            default_lang: if self.default_lang.eq_ignore_ascii_case("en") { Lang::En } else { Lang::It },
            ..Default::default()
        };
        for n in &self.networks {
            e.networks.insert(
                NetId::new(&n.id),
                NetConfig {
                    label: n.label.clone().unwrap_or_else(|| n.id.clone()),
                    max_nick: n.max_nick,
                    transport: match n.transport {
                        TransportCfg::Puppets => Transport::Puppets,
                        TransportCfg::ServiceBot => Transport::ServiceBot,
                    },
                    max_puppets: n.max_puppets,
                },
            );
        }
        e
    }

    pub fn net_specs(&self) -> Vec<NetSpec> {
        self.networks
            .iter()
            .map(|n| NetSpec {
                id: NetId::new(&n.id),
                host: n.host.clone(),
                port: n.port,
                tls: n.tls,
                service_nick: self.service_nick.clone(),
                service_realname: self.service_realname.clone(),
                puppet_source: n.puppet_source_ip,
                puppet_pool: n.puppet_ipv6_pool.as_deref().and_then(|p| itl_net::pool::parse_prefix(p).ok()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config> {
        let c: Config = toml::from_str(s)?;
        c.validate()?;
        Ok(c)
    }

    #[test]
    fn minimal_config_loads_with_defaults() {
        let c = parse(
            r#"
            [[network]]
            id = "a"
            host = "irc.a.example"
            port = 6697
            [[network]]
            id = "b"
            host = "irc.b.example"
            port = 6697
            "#,
        )
        .unwrap();
        assert_eq!(c.service_nick, "ItaLink");
        assert!(c.networks[0].tls);
        assert_eq!(c.networks[0].max_puppets, 60);
    }

    #[test]
    fn plain_text_to_a_real_host_is_refused() {
        let e = parse(
            r#"
            [[network]]
            id = "a"
            host = "irc.a.example"
            port = 6667
            tls = false
            [[network]]
            id = "b"
            host = "127.0.0.1"
            port = 6667
            tls = false
            "#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("only allowed to localhost"), "{e}");
    }

    #[test]
    fn needs_two_distinct_networks() {
        assert!(parse("[[network]]\nid='a'\nhost='h'\nport=1\n").is_err());
        let dup = "[[network]]\nid='a'\nhost='h'\nport=1\n[[network]]\nid='A'\nhost='h'\nport=1\n";
        assert!(parse(dup).is_err());
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    #[test]
    fn pool_is_validated() {
        let ok = "[[network]]\nid='a'\nhost='h'\nport=1\npuppet_ipv6_pool='2001:db8:1:2::/64'\n[[network]]\nid='b'\nhost='h'\nport=1\n";
        let c: Config = toml::from_str(ok).unwrap();
        c.validate().unwrap();
        assert!(c.net_specs()[0].puppet_pool.is_some());
        let bad = ok.replace("/64", "/8");
        let c: Config = toml::from_str(&bad).unwrap();
        assert!(c.validate().is_err());
        let both = ok.replace("puppet_ipv6_pool", "puppet_source_ip='::1'\npuppet_ipv6_pool");
        let c: Config = toml::from_str(&both).unwrap();
        assert!(c.validate().is_err());
    }
}
