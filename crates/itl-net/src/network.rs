//! The service connection of one network and the plumbing for its puppets.

use crate::conn::{self, Target};
use crate::puppet::{run_puppet, PuppetCmd};
use anyhow::Result;
use itl_core::engine::Event;
use itl_core::face::Face;
use itl_core::model::{fold, ChanKey, NetId, Role, SayKind, SeenUser, Uid};
use itl_proto::parse;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone)]
pub struct NetSpec {
    pub id: NetId,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub service_nick: String,
    pub service_realname: String,
    /// Source address for puppet connections (the service uses the default).
    pub puppet_source: Option<IpAddr>,
}

impl NetSpec {
    fn target(&self, source: Option<IpAddr>) -> Target {
        Target { host: self.host.clone(), port: self.port, tls: self.tls, source_ip: source }
    }
}

/// Commands from the engine to one network.
#[derive(Debug, Clone)]
pub enum ToNet {
    ServiceJoin(String),
    ServicePart(String),
    ServiceSay { target: String, text: String, notice: bool },
    SpawnPuppet { uid: Uid, face: Face },
    PuppetJoin { uid: Uid, chan: String },
    PuppetPart { uid: Uid, chan: String, reason: String },
    PuppetQuit { uid: Uid, reason: String },
    PuppetNick { uid: Uid, nick: String },
    PuppetSay { uid: Uid, chan: String, kind: SayKind, text: String },
}

/// Things a network reports back.
#[derive(Debug, Clone)]
pub enum Inbound {
    Core(Event),
    /// Someone invited the service into a channel; it has joined.
    Invited { net: NetId, chan: String, by: String },
    Connected(NetId),
    Log(String),
}

#[derive(Debug, Clone)]
struct Member {
    seen: SeenUser,
    op: bool,
    voice: bool,
}

impl Member {
    fn role(&self) -> Role {
        if self.op {
            Role::Op
        } else if self.voice {
            Role::Voice
        } else {
            Role::Normal
        }
    }
}

struct Session {
    spec: NetSpec,
    nick: String,
    registered: bool,
    want: BTreeMap<String, String>, // folded -> as given
    members: HashMap<String, HashMap<String, Member>>, // folded chan -> folded nick -> member
    who_buf: HashMap<String, Vec<(SeenUser, Role)>>,
    puppets: HashMap<Uid, UnboundedSender<PuppetCmd>>,
    events: UnboundedSender<Inbound>,
}

/// Run one network forever: connect, serve, reconnect with back-off.
pub async fn run_network(spec: NetSpec, mut cmds: UnboundedReceiver<ToNet>, events: UnboundedSender<Inbound>) {
    let mut want: BTreeMap<String, String> = BTreeMap::new();
    let mut backoff = 3u64;
    loop {
        match session(&spec, &mut cmds, &events, &mut want).await {
            Ok(()) => backoff = 3,
            Err(e) => {
                let _ = events.send(Inbound::Log(format!("[{}] connection failed: {e:#}", spec.id)));
            }
        }
        let _ = events.send(Inbound::Core(Event::NetDown { net: spec.id.clone() }));
        // While down, keep the wanted-channel list current and drop the rest.
        let sleep = tokio::time::sleep(Duration::from_secs(backoff));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                c = cmds.recv() => match c {
                    None => return,
                    Some(ToNet::ServiceJoin(ch)) => { want.insert(fold(&ch), ch); }
                    Some(ToNet::ServicePart(ch)) => { want.remove(&fold(&ch)); }
                    Some(_) => {}
                }
            }
        }
        backoff = (backoff * 2).min(60);
    }
}

async fn session(
    spec: &NetSpec,
    cmds: &mut UnboundedReceiver<ToNet>,
    events: &UnboundedSender<Inbound>,
    want: &mut BTreeMap<String, String>,
) -> Result<()> {
    let (mut rd, mut wr) = conn::connect(&spec.target(None)).await?;
    wr.send("CAP LS 302").await?;
    wr.send(&format!("NICK {}", spec.service_nick)).await?;
    wr.send(&format!("USER {} 0 * :{}", spec.service_nick.to_ascii_lowercase(), spec.service_realname)).await?;

    let mut s = Session {
        spec: spec.clone(),
        nick: spec.service_nick.clone(),
        registered: false,
        want: std::mem::take(want),
        members: HashMap::new(),
        who_buf: HashMap::new(),
        puppets: HashMap::new(),
        events: events.clone(),
    };
    let mut caps_offered: Vec<String> = Vec::new();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                line = rd.next() => {
                    let Some(line) = line? else { return Ok(()) };
                    let Some(m) = parse(&line) else { continue };
                    let nick_of = |m: &itl_proto::RawMessage| m.prefix.as_ref().map(|p| p.name.clone()).unwrap_or_default();
                    match m.command.as_str() {
                        "PING" => wr.send(&format!("PONG :{}", m.params.first().cloned().unwrap_or_default())).await?,
                        "CAP" => {
                            let sub = m.params.get(1).map(|x| x.to_uppercase()).unwrap_or_default();
                            match sub.as_str() {
                                "LS" => {
                                    let more = m.params.get(2).is_some_and(|x| x == "*");
                                    let list = m.params.last().cloned().unwrap_or_default();
                                    caps_offered.extend(list.split_whitespace().map(|c| c.split('=').next().unwrap_or("").to_string()));
                                    if !more {
                                        let want_caps = ["multi-prefix", "extended-join", "account-notify", "userhost-in-names"];
                                        let req: Vec<&str> = want_caps.iter().copied().filter(|c| caps_offered.iter().any(|o| o == c)).collect();
                                        if req.is_empty() { wr.send("CAP END").await?; }
                                        else { wr.send(&format!("CAP REQ :{}", req.join(" "))).await?; }
                                    }
                                }
                                "ACK" | "NAK" => wr.send("CAP END").await?,
                                _ => {}
                            }
                        }
                        "001" => {
                            s.registered = true;
                            if let Some(n) = m.params.first() { s.nick = n.clone(); }
                            let _ = events.send(Inbound::Connected(spec.id.clone()));
                            let chans: Vec<String> = s.want.values().cloned().collect();
                            for c in chans { wr.send(&format!("JOIN {c}")).await?; }
                        }
                        "433" if !s.registered => {
                            s.nick.push('_');
                            wr.send(&format!("NICK {}", s.nick)).await?;
                        }
                        "ERROR" => return Ok(()),
                        "JOIN" => s.on_join(&m, &mut wr).await?,
                        "352" => s.on_who(&m),
                        "315" => s.on_who_end(&m),
                        "PART" => s.on_part(&m, &nick_of(&m)),
                        "QUIT" => s.on_quit(&nick_of(&m)),
                        "NICK" => s.on_nick(&m, &nick_of(&m)),
                        "KICK" => s.on_kick(&m),
                        "MODE" => s.on_mode(&m),
                        "ACCOUNT" => s.on_account(&m, &nick_of(&m)),
                        "PRIVMSG" => s.on_privmsg(&m),
                        "INVITE" => s.on_invite(&m, &nick_of(&m), &mut wr).await?,
                        "474" | "473" | "471" | "475" | "403" | "405" => {
                            let _ = events.send(Inbound::Log(format!("[{}] cannot join {}: {}", spec.id, m.params.get(1).cloned().unwrap_or_default(), m.command)));
                        }
                        _ => {}
                    }
                }
                cmd = cmds.recv() => {
                    let Some(cmd) = cmd else { let _ = wr.send("QUIT :shutting down").await; return Ok(()) };
                    s.on_cmd(cmd, &mut wr).await?;
                }
            }
        }
    }
    .await;
    *want = std::mem::take(&mut s.want);
    result
}

impl Session {
    fn key(&self, chan: &str) -> ChanKey {
        ChanKey::new(&self.spec.id.0, chan)
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(Inbound::Core(ev));
    }

    fn is_me(&self, nick: &str) -> bool {
        fold(nick) == fold(&self.nick)
    }

    async fn on_cmd(&mut self, cmd: ToNet, wr: &mut conn::LineWriter) -> Result<()> {
        match cmd {
            ToNet::ServiceJoin(ch) => {
                self.want.insert(fold(&ch), ch.clone());
                if self.registered && !self.members.contains_key(&fold(&ch)) {
                    wr.send(&format!("JOIN {ch}")).await?;
                }
            }
            ToNet::ServicePart(ch) => {
                self.want.remove(&fold(&ch));
                if self.registered {
                    wr.send(&format!("PART {ch} :Ita Link")).await?;
                }
            }
            ToNet::ServiceSay { target, text, notice } => {
                wr.send(&format!("{} {target} :{text}", if notice { "NOTICE" } else { "PRIVMSG" })).await?;
            }
            ToNet::SpawnPuppet { uid, face } => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                self.puppets.insert(uid, tx);
                let t = self.spec.target(self.spec.puppet_source);
                tokio::spawn(run_puppet(t, self.spec.id.clone(), uid, face, rx, self.events.clone()));
            }
            ToNet::PuppetJoin { uid, chan } => self.to_puppet(uid, PuppetCmd::Join(chan)),
            ToNet::PuppetPart { uid, chan, reason } => self.to_puppet(uid, PuppetCmd::Part(chan, reason)),
            ToNet::PuppetQuit { uid, reason } => {
                self.to_puppet(uid, PuppetCmd::Quit(reason));
                self.puppets.remove(&uid);
            }
            ToNet::PuppetNick { uid, nick } => self.to_puppet(uid, PuppetCmd::Nick(nick)),
            ToNet::PuppetSay { uid, chan, kind, text } => self.to_puppet(uid, PuppetCmd::Say { chan, kind, text }),
        }
        Ok(())
    }

    fn to_puppet(&self, uid: Uid, c: PuppetCmd) {
        if let Some(tx) = self.puppets.get(&uid) {
            let _ = tx.send(c);
        }
    }

    async fn on_join(&mut self, m: &itl_proto::RawMessage, wr: &mut conn::LineWriter) -> Result<()> {
        let Some(p) = &m.prefix else { return Ok(()) };
        let chan = m.params.first().cloned().unwrap_or_default();
        let fc = fold(&chan);
        if self.is_me(&p.name) {
            self.members.insert(fc.clone(), HashMap::new());
            self.who_buf.insert(fc, Vec::new());
            wr.send(&format!("WHO {chan}")).await?;
            return Ok(());
        }
        let Some(map) = self.members.get_mut(&fc) else { return Ok(()) };
        let mut seen = SeenUser::new(&p.name, p.user.as_deref().unwrap_or("*"), p.host.as_deref().unwrap_or("*"));
        // extended-join: JOIN #chan account :realname
        if let Some(acc) = m.params.get(1) {
            if acc != "*" {
                seen.account = Some(acc.clone());
            }
        }
        map.insert(fold(&p.name), Member { seen: seen.clone(), op: false, voice: false });
        let key = self.key(&chan);
        self.emit(Event::Join { chan: key, user: seen, role: Role::Normal });
        Ok(())
    }

    fn on_who(&mut self, m: &itl_proto::RawMessage) {
        // :srv 352 me #chan ident host server nick flags :hops realname
        if m.params.len() < 7 {
            return;
        }
        let chan = fold(&m.params[1]);
        let flags = &m.params[6];
        let mut seen = SeenUser::new(&m.params[5], &m.params[2], &m.params[3]);
        // Keep an account we already learned from extended-join.
        if let Some(old) = self.members.get(&chan).and_then(|c| c.get(&fold(&seen.nick))) {
            seen.account = old.seen.account.clone();
        }
        let (op, voice) = (flags.contains('@'), flags.contains('+'));
        let role = if op { Role::Op } else if voice { Role::Voice } else { Role::Normal };
        if let Some(buf) = self.who_buf.get_mut(&chan) {
            buf.push((seen, role));
        }
    }

    fn on_who_end(&mut self, m: &itl_proto::RawMessage) {
        let Some(chan) = m.params.get(1) else { return };
        let fc = fold(chan);
        let Some(users) = self.who_buf.remove(&fc) else { return };
        let map = self.members.entry(fc).or_default();
        map.clear();
        for (seen, role) in &users {
            map.insert(
                fold(&seen.nick),
                Member { seen: seen.clone(), op: *role == Role::Op, voice: *role == Role::Voice },
            );
        }
        let key = self.key(chan);
        self.emit(Event::Sync { chan: key, users });
    }

    fn on_part(&mut self, m: &itl_proto::RawMessage, nick: &str) {
        let chan = m.params.first().cloned().unwrap_or_default();
        let fc = fold(&chan);
        if self.is_me(nick) {
            self.members.remove(&fc);
            return;
        }
        if let Some(map) = self.members.get_mut(&fc) {
            if map.remove(&fold(nick)).is_some() {
                let key = self.key(&chan);
                self.emit(Event::Part { chan: key, nick: nick.to_string() });
            }
        }
    }

    fn on_quit(&mut self, nick: &str) {
        let f = fold(nick);
        let mut found = false;
        for map in self.members.values_mut() {
            found |= map.remove(&f).is_some();
        }
        if found {
            self.emit(Event::Quit { net: self.spec.id.clone(), nick: nick.to_string() });
        }
    }

    fn on_nick(&mut self, m: &itl_proto::RawMessage, old: &str) {
        let new = m.params.first().cloned().unwrap_or_default();
        if self.is_me(old) {
            self.nick = new;
            return;
        }
        let (fo, fnw) = (fold(old), fold(&new));
        let mut found = false;
        for map in self.members.values_mut() {
            if let Some(mut mem) = map.remove(&fo) {
                mem.seen.nick = new.clone();
                map.insert(fnw.clone(), mem);
                found = true;
            }
        }
        if found {
            self.emit(Event::NickChange { net: self.spec.id.clone(), old: old.to_string(), new });
        }
    }

    fn on_kick(&mut self, m: &itl_proto::RawMessage) {
        let chan = m.params.first().cloned().unwrap_or_default();
        let victim = m.params.get(1).cloned().unwrap_or_default();
        let fc = fold(&chan);
        let key = self.key(&chan);
        if self.is_me(&victim) {
            self.members.remove(&fc);
            self.emit(Event::ServiceLeft { chan: key });
        } else if self.members.get_mut(&fc).and_then(|c| c.remove(&fold(&victim))).is_some() {
            self.emit(Event::Part { chan: key, nick: victim });
        }
    }

    fn on_account(&mut self, m: &itl_proto::RawMessage, nick: &str) {
        let acc = m.params.first().cloned().unwrap_or_default();
        let f = fold(nick);
        for map in self.members.values_mut() {
            if let Some(mem) = map.get_mut(&f) {
                mem.seen.account = if acc == "*" { None } else { Some(acc.clone()) };
            }
        }
    }

    fn on_mode(&mut self, m: &itl_proto::RawMessage) {
        let chan = m.params.first().cloned().unwrap_or_default();
        if !chan.starts_with(['#', '&']) {
            return;
        }
        let Some(modes) = m.params.get(1) else { return };
        let mut args = m.params.iter().skip(2);
        let mut plus = true;
        for c in modes.chars() {
            match c {
                '+' => plus = true,
                '-' => plus = false,
                'o' | 'v' => {
                    let Some(nick) = args.next() else { break };
                    let fc = fold(&chan);
                    if let Some(mem) = self.members.get_mut(&fc).and_then(|x| x.get_mut(&fold(nick))) {
                        if c == 'o' { mem.op = plus } else { mem.voice = plus }
                        let role = mem.role();
                        let key = self.key(&chan);
                        self.emit(Event::RoleChange { chan: key, nick: nick.clone(), role });
                    }
                }
                'h' | 'a' | 'q' | 'b' | 'e' | 'I' | 'k' => {
                    args.next();
                }
                'l' if plus => {
                    args.next();
                }
                _ => {}
            }
        }
    }

    fn on_privmsg(&mut self, m: &itl_proto::RawMessage) {
        let Some(p) = &m.prefix else { return };
        if p.user.is_none() {
            return; // from a server, not a person
        }
        let target = m.params.first().cloned().unwrap_or_default();
        let text = m.params.get(1).cloned().unwrap_or_default();
        if target.starts_with(['#', '&']) {
            let (kind, body) = match text.strip_prefix("\x01ACTION ") {
                Some(rest) => (SayKind::Action, rest.trim_end_matches('\x01').to_string()),
                None => (SayKind::Message, text),
            };
            if body.starts_with('\x01') {
                return; // other CTCP
            }
            let key = self.key(&target);
            self.emit(Event::Say { chan: key, nick: p.name.clone(), text: body, kind });
        } else if self.is_me(&target) {
            self.emit(Event::Command { net: self.spec.id.clone(), nick: p.name.clone(), text });
        }
    }

    async fn on_invite(&mut self, m: &itl_proto::RawMessage, by: &str, wr: &mut conn::LineWriter) -> Result<()> {
        let target = m.params.first().cloned().unwrap_or_default();
        let chan = m.params.get(1).cloned().unwrap_or_default();
        if self.is_me(&target) && chan.starts_with(['#', '&']) {
            self.want.insert(fold(&chan), chan.clone());
            wr.send(&format!("JOIN {chan}")).await?;
            let _ = self.events.send(Inbound::Invited { net: self.spec.id.clone(), chan, by: by.to_string() });
        }
        Ok(())
    }
}
