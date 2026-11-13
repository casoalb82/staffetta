//! Glue: networks produce events, the engine decides, actions go back out.
//! Also owns the on-disk state (links, rules, audit log).

use crate::config::Config;
use anyhow::{Context, Result};
use itl_core::engine::*;
use itl_core::model::{fold, NetId, SayKind};
use itl_net::{run_network, Inbound, ToNet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    engine: Snapshot,
    /// Channels the service was invited into, per network (folded names kept
    /// as given), so it comes back after a restart.
    #[serde(default)]
    invited: BTreeMap<String, BTreeSet<String>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn log(msg: &str) {
    let secs = now_ms() / 1000;
    eprintln!("[{secs}] {msg}");
}

struct Store {
    dir: PathBuf,
}

impl Store {
    fn new(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Store {
            dir: dir.to_path_buf(),
        })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn load(&self) -> Result<Persisted> {
        match std::fs::read_to_string(self.state_path()) {
            Ok(t) => Ok(serde_json::from_str(&t).context("state.json is damaged")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Persisted::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write atomically: a crash can never leave half a state file behind.
    fn save(&self, p: &Persisted) -> Result<()> {
        let tmp = self.dir.join("state.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(p)?)?;
        std::fs::rename(&tmp, self.state_path())?;
        Ok(())
    }

    fn audit(&self, r: &AuditRecord) {
        let line = serde_json::json!({
            "at": r.at,
            "chan": r.chan.as_ref().map(|c| c.to_string()),
            "actor": r.actor,
            "what": r.what,
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("audit.jsonl"))
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Run until `shutdown` fires (or forever when it is never sent).
pub async fn run(cfg: Config, mut shutdown: mpsc::UnboundedReceiver<()>) -> Result<()> {
    let store = Store::new(&cfg.state_dir)?;
    let mut persisted = store.load()?;
    let mut engine = Engine::restore(cfg.engine_config(), std::mem::take(&mut persisted.engine));

    let shared: crate::status::Shared =
        std::sync::Arc::new(std::sync::Mutex::new(engine.overview()));
    if let Some(l) = &cfg.status_listen {
        let addr = crate::status::serve(l.parse()?, shared.clone()).await?;
        log(&format!("status page on http://{addr}/"));
    }
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
    let mut nets: HashMap<NetId, mpsc::UnboundedSender<ToNet>> = HashMap::new();
    for spec in cfg.net_specs() {
        let (tx, rx) = mpsc::unbounded_channel();
        nets.insert(spec.id.clone(), tx.clone());
        // Channels to (re)join: those in links and those we were invited to.
        let mut chans: BTreeSet<String> = persisted
            .invited
            .get(&spec.id.0)
            .cloned()
            .unwrap_or_default();
        for k in engine.wanted_channels() {
            if k.net == spec.id {
                chans.insert(k.chan);
            }
        }
        for c in chans {
            let _ = tx.send(ToNet::ServiceJoin(c));
        }
        log(&format!(
            "network {} -> {}:{} (tls={})",
            spec.id, spec.host, spec.port, spec.tls
        ));
        tokio::spawn(run_network(spec, rx, in_tx.clone()));
    }
    drop(in_tx);

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.tick().await;
    loop {
        let mut dirty = false;
        let (acts, persist_now) = tokio::select! {
            _ = shutdown.recv() => {
                log("shutting down");
                let p = Persisted { engine: engine.snapshot(), invited: persisted.invited.clone() };
                store.save(&p)?;
                return Ok(());
            }
            _ = tick.tick() => (engine.handle(now_ms(), Event::Tick), false),
            msg = in_rx.recv() => {
                let Some(msg) = msg else { return Ok(()) };
                match msg {
                    Inbound::Log(l) => { log(&l); continue; }
                    Inbound::Connected(n) => { log(&format!("connected to {n}")); continue; }
                    Inbound::Invited { net, chan, by } => {
                        log(&format!("[{net}] invited into {chan} by {by}"));
                        persisted.invited.entry(net.0.clone()).or_default().insert(chan.clone());
                        let rec = AuditRecord { at: now_ms(), chan: Some(itl_core::model::ChanKey::new(&net.0, &chan)), actor: by, what: "invited the service".into() };
                        store.audit(&rec);
                        dirty = true;
                        (Vec::new(), true)
                    }
                    Inbound::Core(ev) => {
                        let is_cmd = matches!(ev, Event::Command { .. });
                        if let Event::ServiceLeft { chan } = &ev {
                            if let Some(set) = persisted.invited.get_mut(&chan.net.0) {
                                set.retain(|c| fold(c) != chan.chan);
                            }
                            dirty = true;
                        }
                        (engine.handle(now_ms(), ev), is_cmd)
                    }
                }
            }
        };
        let mut changed = persist_now || dirty;
        for a in acts {
            changed |= dispatch(&nets, &store, a);
        }
        if let Ok(mut g) = shared.lock() {
            *g = engine.overview();
        }
        if changed {
            let p = Persisted {
                engine: engine.snapshot(),
                invited: persisted.invited.clone(),
            };
            if let Err(e) = store.save(&p) {
                log(&format!("cannot save state: {e:#}"));
            }
        }
    }
}

/// Send one action to the right network. Returns true when it changed state
/// worth saving (an audit entry means an owner did something).
fn dispatch(nets: &HashMap<NetId, mpsc::UnboundedSender<ToNet>>, store: &Store, a: Action) -> bool {
    let send = |net: &NetId, m: ToNet| {
        if let Some(tx) = nets.get(net) {
            let _ = tx.send(m);
        }
    };
    match a {
        Action::SpawnPuppet { net, uid, face } => send(&net, ToNet::SpawnPuppet { uid, face }),
        Action::PuppetJoin { net, uid, chan } => send(&net, ToNet::PuppetJoin { uid, chan }),
        Action::PuppetPart {
            net,
            uid,
            chan,
            reason,
        } => send(&net, ToNet::PuppetPart { uid, chan, reason }),
        Action::PuppetQuit { net, uid, reason } => send(&net, ToNet::PuppetQuit { uid, reason }),
        Action::PuppetNick { net, uid, nick } => send(&net, ToNet::PuppetNick { uid, nick }),
        Action::Say {
            net,
            via,
            chan,
            kind,
            text,
        } => match via {
            Via::Puppet(uid) => send(
                &net,
                ToNet::PuppetSay {
                    uid,
                    chan,
                    kind,
                    text,
                },
            ),
            Via::Service => send(
                &net,
                ToNet::ServiceSay {
                    target: chan,
                    text,
                    notice: false,
                },
            ),
        },
        Action::Reply { net, nick, text } => send(
            &net,
            ToNet::ServiceSay {
                target: nick,
                text,
                notice: true,
            },
        ),
        Action::Notice { chan, text } => send(
            &chan.net.clone(),
            ToNet::ServiceSay {
                target: chan.chan,
                text,
                notice: true,
            },
        ),
        Action::Audit(r) => {
            log(&format!(
                "audit {} {}: {}",
                r.chan
                    .as_ref()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "-".into()),
                r.actor,
                r.what
            ));
            store.audit(&r);
            return true;
        }
    }
    let _ = SayKind::Message;
    false
}
