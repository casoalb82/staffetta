//! A tiny in-process IRC server for tests.
//!
//! It only ever listens on `127.0.0.1` on a port the operating system picks,
//! so it cannot collide with - or be mistaken for - a real network. It speaks
//! enough IRC for a client/bridge to be tested honestly: registration, CAP
//! (multi-prefix, extended-join, account-notify, userhost-in-names), JOIN with
//! bans, PART, QUIT, NICK, PRIVMSG/NOTICE, NAMES, WHO, MODE o/v/b, KICK, INVITE.

use itl_core::model::{fold, glob};
use itl_proto::{parse, RawMessage};
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub target: String,
    pub from: String,
    pub text: String,
}

#[derive(Default)]
struct User {
    nick: String,
    ident: String,
    host: String,
    realname: String,
    account: Option<String>,
    caps: Vec<String>,
    registered: bool,
    tx: Option<mpsc::UnboundedSender<String>>,
    /// Open connection id, so a stale task cannot remove a newer user.
    conn: u64,
}

#[derive(Default)]
struct Chan {
    /// folded nick -> (op, voice)
    members: BTreeMap<String, (bool, bool)>,
    bans: Vec<String>,
}

#[derive(Default)]
struct State {
    name: String,
    users: HashMap<String, User>, // folded nick
    chans: HashMap<String, Chan>, // folded chan
    log: Vec<LogEntry>,
    connections: u64,
    total_connections: u64,
    kills: HashMap<u64, Arc<tokio::sync::Notify>>,
}

#[derive(Clone)]
pub struct TestNet {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
}

fn prefix_of(u: &User) -> String {
    format!("{}!{}@{}", u.nick, u.ident, u.host)
}

fn send_to(st: &State, nick: &str, line: String) {
    if let Some(tx) = st.users.get(&fold(nick)).and_then(|u| u.tx.as_ref()) {
        let _ = tx.send(line);
    }
}

fn broadcast(st: &State, chan: &str, line: String, except: Option<&str>) {
    if let Some(c) = st.chans.get(&fold(chan)) {
        for n in c.members.keys() {
            if Some(n.as_str()) == except.map(fold).as_deref() {
                continue;
            }
            send_to(st, n, line.clone());
        }
    }
}

impl TestNet {
    /// Start a server on 127.0.0.1 with an OS-chosen port.
    pub async fn start(name: &str) -> TestNet {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback());
        let state = Arc::new(Mutex::new(State { name: name.to_string(), ..Default::default() }));
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { break };
                let st = st.clone();
                tokio::spawn(async move {
                    let _ = serve(sock, st).await;
                });
            }
        });
        TestNet { addr, state }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Connections currently open / ever opened.
    pub fn connections(&self) -> (u64, u64) {
        let s = self.state.lock().unwrap();
        (s.connections, s.total_connections)
    }

    pub fn has_user(&self, nick: &str) -> bool {
        self.state.lock().unwrap().users.get(&fold(nick)).is_some_and(|u| u.registered)
    }

    pub fn user_ident(&self, nick: &str) -> Option<String> {
        self.state.lock().unwrap().users.get(&fold(nick)).map(|u| u.ident.clone())
    }

    pub fn user_realname(&self, nick: &str) -> Option<String> {
        self.state.lock().unwrap().users.get(&fold(nick)).map(|u| u.realname.clone())
    }

    /// Nicks in a channel (case as given by the users), op/voice not marked.
    pub fn members(&self, chan: &str) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.chans
            .get(&fold(chan))
            .map(|c| c.members.keys().map(|k| s.users.get(k).map(|u| u.nick.clone()).unwrap_or(k.clone())).collect())
            .unwrap_or_default()
    }

    pub fn is_in(&self, chan: &str, nick: &str) -> bool {
        let s = self.state.lock().unwrap();
        s.chans.get(&fold(chan)).is_some_and(|c| c.members.contains_key(&fold(nick)))
    }

    pub fn log(&self) -> Vec<LogEntry> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn clear_log(&self) {
        self.state.lock().unwrap().log.clear();
    }

    pub fn set_account(&self, nick: &str, account: &str) {
        let mut s = self.state.lock().unwrap();
        if let Some(u) = s.users.get_mut(&fold(nick)) {
            u.account = Some(account.to_string());
        }
    }

    /// Give or take ops (as a server/services would).
    pub fn set_op(&self, chan: &str, nick: &str, op: bool) {
        let mut s = self.state.lock().unwrap();
        let pfx = s.name.clone();
        let (changed, msg) = {
            let Some(c) = s.chans.get_mut(&fold(chan)) else { return };
            let Some(m) = c.members.get_mut(&fold(nick)) else { return };
            m.0 = op;
            (true, format!(":{pfx} MODE {chan} {}o {nick}", if op { "+" } else { "-" }))
        };
        if changed {
            broadcast(&s, chan, msg, None);
        }
    }

    pub fn add_ban(&self, chan: &str, mask: &str) {
        let mut s = self.state.lock().unwrap();
        let c = s.chans.entry(fold(chan)).or_default();
        c.bans.push(mask.to_string());
    }

    /// Kick `nick` out of `chan` as if an operator did it.
    pub fn kick(&self, chan: &str, nick: &str, reason: &str) {
        let mut s = self.state.lock().unwrap();
        let line = format!(":Operator!op@{}.test KICK {chan} {nick} :{reason}", s.name);
        broadcast(&s, chan, line, None);
        if let Some(c) = s.chans.get_mut(&fold(chan)) {
            c.members.remove(&fold(nick));
        }
    }

    /// Ban the mask and kick everyone matching, like `/mode +b` plus `/kick`.
    pub fn ban_and_kick(&self, chan: &str, nick: &str) {
        let mask = {
            let s = self.state.lock().unwrap();
            s.users.get(&fold(nick)).map(|u| format!("*!{}@*", u.ident)).unwrap_or_default()
        };
        self.add_ban(chan, &mask);
        self.kick(chan, nick, "banned");
    }

    /// Cut a user's connection like a network failure (no QUIT from them).
    pub fn disconnect(&self, nick: &str) {
        let s = self.state.lock().unwrap();
        if let Some(k) = s.users.get(&fold(nick)).and_then(|u| s.kills.get(&u.conn)) {
            k.notify_one();
        }
    }
}

async fn serve(sock: TcpStream, st: Arc<Mutex<State>>) -> std::io::Result<()> {
    let (rd, mut wr) = sock.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let conn_id = {
        let mut s = st.lock().unwrap();
        s.connections += 1;
        s.total_connections += 1;
        s.total_connections
    };
    let kill = Arc::new(tokio::sync::Notify::new());
    st.lock().unwrap().kills.insert(conn_id, kill.clone());
    let writer = tokio::spawn(async move {
        while let Some(l) = rx.recv().await {
            if wr.write_all(format!("{l}\r\n").as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });

    let mut lines = BufReader::new(rd).lines();
    let mut me = String::new(); // folded nick once known
    let mut pending_nick: Option<String> = None;
    let mut pending_user: Option<(String, String)> = None;
    let mut caps_ok: Vec<String> = Vec::new();
    let mut cap_negotiating = false;

    loop {
        let line = tokio::select! {
            r = lines.next_line() => match r { Ok(Some(l)) => l, _ => break },
            _ = kill.notified() => break,
        };
        let Some(m) = parse(&line) else { continue };
        let mut s = st.lock().unwrap();
        let sname = s.name.clone();
        match m.command.as_str() {
            "CAP" => {
                let sub = m.params.first().map(|x| x.to_uppercase()).unwrap_or_default();
                match sub.as_str() {
                    "LS" => {
                        cap_negotiating = true;
                        let _ = tx.send(format!(":{sname} CAP * LS :multi-prefix extended-join account-notify userhost-in-names"));
                    }
                    "REQ" => {
                        let want = m.params.get(1).cloned().unwrap_or_default();
                        for c in want.split_whitespace() {
                            caps_ok.push(c.to_string());
                        }
                        let _ = tx.send(format!(":{sname} CAP * ACK :{want}"));
                    }
                    "END" => cap_negotiating = false,
                    _ => {}
                }
            }
            "NICK" => {
                let new = m.params.first().cloned().unwrap_or_default();
                if me.is_empty() {
                    if s.users.get(&fold(&new)).is_some_and(|u| u.registered || u.conn != conn_id) {
                        let _ = tx.send(format!(":{sname} 433 * {new} :Nickname is already in use"));
                    } else {
                        pending_nick = Some(new);
                    }
                } else if s.users.get(&fold(&new)).is_some_and(|u| fold(&u.nick) != me) {
                    let _ = tx.send(format!(":{sname} 433 {} {new} :Nickname is already in use", s.users[&me].nick));
                } else {
                    // nick change: move the user, tell everyone who shares a channel
                    let mut u = s.users.remove(&me).unwrap();
                    let old_prefix = prefix_of(&u);
                    u.nick = new.clone();
                    let nf = fold(&new);
                    let chans: Vec<String> = s.chans.iter().filter(|(_, c)| c.members.contains_key(&me)).map(|(k, _)| k.clone()).collect();
                    for k in &chans {
                        let c = s.chans.get_mut(k).unwrap();
                        if let Some(v) = c.members.remove(&me) {
                            c.members.insert(nf.clone(), v);
                        }
                    }
                    s.users.insert(nf.clone(), u);
                    let line = format!(":{old_prefix} NICK :{new}");
                    let _ = tx.send(line.clone());
                    for k in &chans {
                        broadcast(&s, k, line.clone(), Some(&new));
                    }
                    me = nf;
                }
            }
            "USER" => {
                pending_user = Some((
                    m.params.first().cloned().unwrap_or_default(),
                    m.params.get(3).cloned().unwrap_or_default(),
                ));
            }
            "PING" => {
                let _ = tx.send(format!(":{sname} PONG {sname} :{}", m.params.first().cloned().unwrap_or_default()));
            }
            "QUIT" => break,
            _ if me.is_empty() => {}
            "JOIN" => {
                for ch in m.params.first().cloned().unwrap_or_default().split(',') {
                    join(&mut s, &me, ch, &tx);
                }
            }
            "PART" => {
                let ch = m.params.first().cloned().unwrap_or_default();
                let reason = m.params.get(1).cloned().unwrap_or_default();
                if s.chans.get(&fold(&ch)).is_some_and(|c| c.members.contains_key(&me)) {
                    let p = prefix_of(&s.users[&me]);
                    let line = format!(":{p} PART {ch} :{reason}");
                    broadcast(&s, &ch, line, None);
                    let k = fold(&ch);
                    s.chans.get_mut(&k).unwrap().members.remove(&me);
                    if s.chans[&k].members.is_empty() && s.chans[&k].bans.is_empty() {
                        s.chans.remove(&k);
                    }
                }
            }
            "PRIVMSG" | "NOTICE" => {
                let target = m.params.first().cloned().unwrap_or_default();
                let text = m.params.get(1).cloned().unwrap_or_default();
                let from = s.users[&me].nick.clone();
                let p = prefix_of(&s.users[&me]);
                let line = format!(":{p} {} {target} :{text}", m.command);
                if target.starts_with(['#', '&']) {
                    if !s.chans.get(&fold(&target)).is_some_and(|c| c.members.contains_key(&me)) {
                        let _ = tx.send(format!(":{sname} 404 {from} {target} :Cannot send to channel"));
                    } else {
                        broadcast(&s, &target, line, Some(&from));
                        s.log.push(LogEntry { target: target.clone(), from, text });
                    }
                } else if s.users.contains_key(&fold(&target)) {
                    send_to(&s, &target, line);
                    s.log.push(LogEntry { target: target.clone(), from, text });
                } else {
                    let _ = tx.send(format!(":{sname} 401 {from} {target} :No such nick/channel"));
                }
            }
            "NAMES" => {
                let ch = m.params.first().cloned().unwrap_or_default();
                names(&s, &me, &ch, &tx);
            }
            "WHO" => {
                let ch = m.params.first().cloned().unwrap_or_default();
                let my = s.users[&me].nick.clone();
                if let Some(c) = s.chans.get(&fold(&ch)) {
                    for (k, (op, voice)) in &c.members {
                        let u = &s.users[k];
                        let fl = format!("H{}{}", if *op { "@" } else { "" }, if *voice { "+" } else { "" });
                        let _ = tx.send(format!(":{sname} 352 {my} {ch} {} {} {sname} {} {fl} :0 {}", u.ident, u.host, u.nick, u.realname));
                    }
                }
                let _ = tx.send(format!(":{sname} 315 {my} {ch} :End of /WHO list."));
            }
            "MODE" => {
                let ch = m.params.first().cloned().unwrap_or_default();
                if let (Some(modes), Some(arg)) = (m.params.get(1), m.params.get(2)) {
                    let is_op = s.chans.get(&fold(&ch)).and_then(|c| c.members.get(&me)).is_some_and(|x| x.0);
                    if is_op {
                        let p = prefix_of(&s.users[&me]);
                        let plus = modes.starts_with('+');
                        match modes.trim_start_matches(['+', '-']) {
                            "o" | "v" => {
                                let which = modes.trim_start_matches(['+', '-']).to_string();
                                if let Some(x) = s.chans.get_mut(&fold(&ch)).and_then(|c| c.members.get_mut(&fold(arg))) {
                                    if which == "o" { x.0 = plus } else { x.1 = plus }
                                }
                                broadcast(&s, &ch, format!(":{p} MODE {ch} {modes} {arg}"), None);
                            }
                            "b" => {
                                let c = s.chans.entry(fold(&ch)).or_default();
                                if plus { c.bans.push(arg.clone()) } else { c.bans.retain(|b| b != arg) }
                                broadcast(&s, &ch, format!(":{p} MODE {ch} {modes} {arg}"), None);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "KICK" => {
                let ch = m.params.first().cloned().unwrap_or_default();
                let target = m.params.get(1).cloned().unwrap_or_default();
                let reason = m.params.get(2).cloned().unwrap_or_default();
                let is_op = s.chans.get(&fold(&ch)).and_then(|c| c.members.get(&me)).is_some_and(|x| x.0);
                if is_op {
                    let p = prefix_of(&s.users[&me]);
                    broadcast(&s, &ch, format!(":{p} KICK {ch} {target} :{reason}"), None);
                    if let Some(c) = s.chans.get_mut(&fold(&ch)) {
                        c.members.remove(&fold(&target));
                    }
                }
            }
            "INVITE" => {
                let target = m.params.first().cloned().unwrap_or_default();
                let ch = m.params.get(1).cloned().unwrap_or_default();
                let p = prefix_of(&s.users[&me]);
                send_to(&s, &target, format!(":{p} INVITE {target} {ch}"));
            }
            _ => {}
        }

        // Registration completes when we have NICK + USER and CAP is closed.
        if me.is_empty() && !cap_negotiating {
            if let (Some(n), Some((ident, real))) = (pending_nick.clone(), pending_user.clone()) {
                let nf = fold(&n);
                let account = None;
                s.users.insert(
                    nf.clone(),
                    User {
                        nick: n.clone(),
                        ident: ident.trim_start_matches('~').to_string(),
                        host: format!("{n}.test"),
                        realname: real,
                        account,
                        caps: caps_ok.clone(),
                        registered: true,
                        tx: Some(tx.clone()),
                        conn: conn_id,
                    },
                );
                me = nf;
                let sn = s.name.clone();
                let _ = tx.send(format!(":{sn} 001 {n} :Welcome to the {sn} test network, {n}"));
                let _ = tx.send(format!(":{sn} 005 {n} PREFIX=(ov)@+ NICKLEN=30 CHANTYPES=#& CASEMAPPING=rfc1459 :are supported by this server"));
                let _ = tx.send(format!(":{sn} 376 {n} :End of /MOTD command."));
            }
        }
    }

    // Connection ended: leave all channels with a QUIT, but only if this
    // connection still owns the nick.
    {
        let mut s = st.lock().unwrap();
        s.kills.remove(&conn_id);
        s.connections = s.connections.saturating_sub(1);
        if !me.is_empty() && s.users.get(&me).is_some_and(|u| u.conn == conn_id) {
            let u = s.users.remove(&me).unwrap();
            let line = format!(":{} QUIT :Connection closed", prefix_of(&u));
            let chans: Vec<String> = s.chans.iter().filter(|(_, c)| c.members.contains_key(&me)).map(|(k, _)| k.clone()).collect();
            for k in chans {
                s.chans.get_mut(&k).unwrap().members.remove(&me);
                broadcast(&s, &k, line.clone(), None);
                if s.chans[&k].members.is_empty() && s.chans[&k].bans.is_empty() {
                    s.chans.remove(&k);
                }
            }
        }
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn join(s: &mut State, me: &str, ch: &str, tx: &mpsc::UnboundedSender<String>) {
    let sname = s.name.clone();
    let u = &s.users[me];
    let mask = prefix_of(u);
    let my = u.nick.clone();
    if s.chans.get(&fold(ch)).is_some_and(|c| c.bans.iter().any(|b| glob(b, &mask))) {
        let _ = tx.send(format!(":{sname} 474 {my} {ch} :Cannot join channel (+b)"));
        return;
    }
    let k = fold(ch);
    let c = s.chans.entry(k.clone()).or_default();
    if c.members.contains_key(me) {
        return;
    }
    let first = c.members.is_empty();
    c.members.insert(me.to_string(), (first, false));
    let u = &s.users[me];
    // Each member gets the JOIN in the shape its capabilities ask for.
    let members: Vec<String> = s.chans[&k].members.keys().cloned().collect();
    for n in members {
        let r = &s.users[&n];
        let line = if r.caps.iter().any(|c| c == "extended-join") {
            format!(":{} JOIN {ch} {} :{}", prefix_of(u), u.account.clone().unwrap_or("*".into()), u.realname)
        } else {
            format!(":{} JOIN {ch}", prefix_of(u))
        };
        send_to(s, &r.nick.clone(), line);
    }
    names(s, me, ch, tx);
}

fn names(s: &State, me: &str, ch: &str, tx: &mpsc::UnboundedSender<String>) {
    let sname = &s.name;
    let u = &s.users[me];
    let uhn = u.caps.iter().any(|c| c == "userhost-in-names");
    let multi = u.caps.iter().any(|c| c == "multi-prefix");
    if let Some(c) = s.chans.get(&fold(ch)) {
        let list: Vec<String> = c
            .members
            .iter()
            .map(|(k, (op, voice))| {
                let m = &s.users[k];
                let mut p = String::new();
                if *op {
                    p.push('@');
                }
                if *voice && (multi || !*op) {
                    p.push('+');
                }
                if uhn {
                    format!("{p}{}!{}@{}", m.nick, m.ident, m.host)
                } else {
                    format!("{p}{}", m.nick)
                }
            })
            .collect();
        let _ = tx.send(format!(":{sname} 353 {} = {ch} :{}", u.nick, list.join(" ")));
    }
    let _ = tx.send(format!(":{sname} 366 {} {ch} :End of /NAMES list.", u.nick));
}

/// Parse helper re-exported for tests that want raw lines.
pub fn parse_line(l: &str) -> Option<RawMessage> {
    parse(l)
}

/// A scripted IRC client for tests.
pub struct Client {
    rd: tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    wr: tokio::net::tcp::OwnedWriteHalf,
    pub nick: String,
    /// Every line received so far.
    pub seen: Vec<String>,
}

impl Client {
    pub async fn connect(net: &TestNet, nick: &str) -> Client {
        let sock = TcpStream::connect(net.addr).await.expect("connect test net");
        let (rd, wr) = sock.into_split();
        let mut c = Client { rd: BufReader::new(rd).lines(), wr, nick: nick.to_string(), seen: Vec::new() };
        c.send(&format!("NICK {nick}")).await;
        c.send(&format!("USER {} 0 * :{nick} (test client)", nick.to_ascii_lowercase())).await;
        c.wait_for(|l| l.contains(" 001 "), 5).await.expect("welcome");
        c
    }

    pub async fn send(&mut self, line: &str) {
        self.wr.write_all(format!("{line}\r\n").as_bytes()).await.unwrap();
    }

    pub async fn join(&mut self, chan: &str) {
        self.send(&format!("JOIN {chan}")).await;
        let end = format!(" 366 {} {chan} ", self.nick);
        self.wait_for(|l| l.contains(&end), 5).await.expect("end of names");
    }

    pub async fn say(&mut self, target: &str, text: &str) {
        self.send(&format!("PRIVMSG {target} :{text}")).await;
    }

    /// Read lines until one matches, up to `secs` seconds.
    pub async fn wait_for(&mut self, pred: impl Fn(&str) -> bool, secs: u64) -> Option<String> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            match tokio::time::timeout(left, self.rd.next_line()).await {
                Ok(Ok(Some(l))) => {
                    self.seen.push(l.clone());
                    if let Some(m) = parse(&l) {
                        if m.command == "PING" {
                            let tok = m.params.first().cloned().unwrap_or_default();
                            let _ = self.wr.write_all(format!("PONG :{tok}\r\n").as_bytes()).await;
                        }
                    }
                    if pred(&l) {
                        return Some(l);
                    }
                }
                _ => return None,
            }
        }
    }

    /// True when nothing matching `pred` arrives within `ms` milliseconds.
    pub async fn silent_for(&mut self, pred: impl Fn(&str) -> bool, ms: u64) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            match tokio::time::timeout(left, self.rd.next_line()).await {
                Ok(Ok(Some(l))) => {
                    self.seen.push(l.clone());
                    if pred(&l) {
                        return false;
                    }
                }
                _ => return true,
            }
        }
    }
}
