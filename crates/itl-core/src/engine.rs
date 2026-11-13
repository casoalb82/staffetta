//! The relay engine: a deterministic state machine.
//!
//! `Engine::handle(now_ms, event)` takes something that happened on a network
//! and returns the list of [`Action`]s the I/O layer must perform. There is no
//! networking, no clock and no randomness in here, so every behaviour - from
//! "a kicked puppet stays kicked" to "a line over the flood limit is dropped" -
//! is covered by plain unit tests.
//!
//! Ground rules enforced here (see docs/DESIGN.md):
//!
//! * Nothing flows between two channels unless **both** owners approved the
//!   link **and** both channels went `LIVE` on their own.
//! * Each owner's policy is applied on their own side, to everything that
//!   enters or leaves their channel.
//! * Only a channel operator can change a channel's settings, and the service
//!   must have been invited into the channel first.
//! * Everything an owner does is written to an audit log.

use crate::command::{self, Command, ModeWord};
use crate::face::{make_face, Face, NickStyle};
use crate::model::*;
use crate::msg::Msg;
use crate::policy::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

const REQUEST_TTL_MS: u64 = 7 * 24 * 3600 * 1000;
const KICK_COOLDOWN_MS: u64 = 10 * 60 * 1000;
const AUDIT_RING: usize = 500;
const SAMPLE_RING: usize = 10;

// ---------------------------------------------------------------- config ---

/// How remote people are brought into a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    /// One real connection per remote person (the user list shows them).
    Puppets,
    /// A single labelled relay bot says everything. For networks that do not
    /// allow extra connections. Works anywhere, no network-admin help needed.
    ServiceBot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetConfig {
    /// Human name shown in realnames and notices, e.g. "Azzurra".
    pub label: String,
    pub max_nick: usize,
    pub transport: Transport,
    /// Maximum simultaneous puppet connections on this network.
    pub max_puppets: usize,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            label: String::new(),
            max_nick: 30,
            transport: Transport::Puppets,
            max_puppets: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub service_nick: String,
    pub nick_style: NickStyle,
    pub networks: HashMap<NetId, NetConfig>,
    /// Owners must be logged in to services to run commands.
    pub require_account_for_owner: bool,
    /// Relay users flagged as bots (off: they are never relayed).
    pub relay_bots: bool,
    /// Language new links start with (owners change it with `SET lang`).
    pub default_lang: Lang,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            service_nick: "ItaLink".into(),
            nick_style: NickStyle::Pipe,
            networks: HashMap::new(),
            require_account_for_owner: false,
            relay_bots: false,
            default_lang: Lang::En,
        }
    }
}

// ------------------------------------------------------------ persistent ---

/// JSON object keys must be strings, a channel key is not: store the member
/// map as a list of pairs instead.
mod chan_map {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        m: &BTreeMap<ChanKey, Member>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let v: Vec<(&ChanKey, &Member)> = m.iter().collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<ChanKey, Member>, D::Error> {
        let v: Vec<(ChanKey, Member)> = Vec::deserialize(d)?;
        Ok(v.into_iter().collect())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub policy: Policy,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub newcomer: ChanKey,
    /// Existing members that approved the newcomer.
    pub approvals: BTreeSet<ChanKey>,
    /// The newcomer's own operators agreed.
    pub newcomer_ok: bool,
    pub at: u64,
    pub by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Room {
    #[serde(with = "chan_map")]
    pub members: BTreeMap<ChanKey, Member>,
    pub requests: Vec<Request>,
}

/// Everything that must survive a restart: who is linked and each owner's rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub rooms: BTreeMap<u64, Room>,
    pub next_room: u64,
}

// ------------------------------------------------------------ events/acts ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostWhy {
    Kicked,
    /// Join refused because of a ban, or banned while inside.
    Banned,
    /// Disconnected by the network (KILL, ping timeout, ...).
    Killed,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// The service is in `chan` and learned its full member list.
    Sync {
        chan: ChanKey,
        users: Vec<(SeenUser, Role)>,
    },
    Join {
        chan: ChanKey,
        user: SeenUser,
        role: Role,
    },
    Part {
        chan: ChanKey,
        nick: String,
    },
    Quit {
        net: NetId,
        nick: String,
    },
    NickChange {
        net: NetId,
        old: String,
        new: String,
    },
    RoleChange {
        chan: ChanKey,
        nick: String,
        role: Role,
    },
    /// A person logged in to / out of services (account-notify).
    AccountChange {
        net: NetId,
        nick: String,
        account: Option<String>,
    },
    Say {
        chan: ChanKey,
        nick: String,
        text: String,
        kind: SayKind,
    },
    /// A private message to the service nick.
    Command {
        net: NetId,
        nick: String,
        text: String,
    },
    /// One of our puppets was removed from a channel or the network.
    PuppetLost {
        net: NetId,
        nick: String,
        chan: Option<String>,
        why: LostWhy,
    },
    /// A channel refused a line said by one of our puppets (ban, +m, +n...).
    SayRefused {
        net: NetId,
        nick: String,
        chan: String,
    },
    /// The service is no longer in `chan` (kicked, banned, channel gone).
    ServiceLeft {
        chan: ChanKey,
    },
    /// The connection to a whole network dropped.
    NetDown {
        net: NetId,
    },
    /// Periodic housekeeping (about once a minute).
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    Puppet(Uid),
    Service,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRecord {
    pub at: u64,
    pub chan: Option<ChanKey>,
    pub actor: String,
    pub what: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    SpawnPuppet {
        net: NetId,
        uid: Uid,
        face: Face,
    },
    PuppetJoin {
        net: NetId,
        uid: Uid,
        chan: String,
    },
    PuppetPart {
        net: NetId,
        uid: Uid,
        chan: String,
        reason: String,
    },
    PuppetQuit {
        net: NetId,
        uid: Uid,
        reason: String,
    },
    PuppetNick {
        net: NetId,
        uid: Uid,
        nick: String,
    },
    Say {
        net: NetId,
        via: Via,
        chan: String,
        kind: SayKind,
        text: String,
    },
    /// A private notice from the service to one person.
    Reply {
        net: NetId,
        nick: String,
        text: String,
    },
    /// A notice from the service into a channel it is in.
    Notice {
        chan: ChanKey,
        text: String,
    },
    Audit(AuditRecord),
}

// --------------------------------------------------------------- runtime ---

#[derive(Debug, Clone)]
struct ChanUser {
    uid: Uid,
    role: Role,
    since: u64,
}

#[derive(Debug, Clone, Default)]
struct ChanState {
    users: HashMap<String, ChanUser>, // folded nick -> info
}

#[derive(Debug, Clone)]
struct UserRec {
    net: NetId,
    seen: SeenUser,
}

#[derive(Debug, Clone)]
struct Puppet {
    face: Face,
    /// folded channel -> last activity (ms)
    chans: HashMap<String, u64>,
    last: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub out_lines: u64,
    pub in_lines: u64,
    pub would_out: u64,
    pub would_in: u64,
    pub dropped_rate: u64,
    pub dropped_filter: u64,
}

/// What the public status page may show. No people, no chat: only channels
/// whose owners opted in (`SET #chan public on`) and aggregate numbers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Overview {
    pub links: Vec<LinkView>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkView {
    pub channels: Vec<ChanView>,
    /// Channels of this link that are not listed (owners did not opt in).
    pub hidden: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChanView {
    pub network: String,
    pub name: String,
    pub mode: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Totals {
    pub links: usize,
    pub channels: usize,
    pub live_channels: usize,
    pub puppets: usize,
    pub lines_relayed: u64,
}

pub struct Engine {
    pub cfg: EngineConfig,
    rooms: BTreeMap<u64, Room>,
    next_room: u64,

    next_uid: Uid,
    users: HashMap<(NetId, String), Uid>,
    urec: HashMap<Uid, UserRec>,
    chans: HashMap<ChanKey, ChanState>,
    puppets: HashMap<(NetId, Uid), Puppet>,
    puppet_nicks: HashMap<(NetId, String), Uid>,
    buckets: HashMap<(Uid, ChanKey), Bucket>,
    cooldown: HashMap<(Uid, ChanKey), u64>,
    /// Who already got the "you are too new" notice (once per run, per channel).
    told_new: HashSet<(Uid, ChanKey)>,
    told_refused: HashSet<(Uid, ChanKey)>,
    samples: HashMap<ChanKey, VecDeque<String>>,
    stats: HashMap<ChanKey, Stats>,
    audit: VecDeque<AuditRecord>,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self {
        Engine {
            cfg,
            rooms: BTreeMap::new(),
            next_room: 1,
            next_uid: 1,
            users: HashMap::new(),
            urec: HashMap::new(),
            chans: HashMap::new(),
            puppets: HashMap::new(),
            puppet_nicks: HashMap::new(),
            buckets: HashMap::new(),
            cooldown: HashMap::new(),
            told_new: HashSet::new(),
            told_refused: HashSet::new(),
            samples: HashMap::new(),
            stats: HashMap::new(),
            audit: VecDeque::new(),
        }
    }

    pub fn restore(cfg: EngineConfig, snap: Snapshot) -> Self {
        let mut e = Engine::new(cfg);
        e.rooms = snap.rooms;
        e.next_room = snap.next_room.max(1);
        e
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            rooms: self.rooms.clone(),
            next_room: self.next_room,
        }
    }

    /// Channels the service must be in (all members and pending newcomers).
    pub fn wanted_channels(&self) -> Vec<ChanKey> {
        let mut v: BTreeSet<ChanKey> = BTreeSet::new();
        for r in self.rooms.values() {
            v.extend(r.members.keys().cloned());
            v.extend(r.requests.iter().map(|q| q.newcomer.clone()));
        }
        v.into_iter().collect()
    }

    pub fn stats(&self, chan: &ChanKey) -> Stats {
        self.stats.get(chan).cloned().unwrap_or_default()
    }

    pub fn member(&self, chan: &ChanKey) -> Option<&Member> {
        self.rooms.values().find_map(|r| r.members.get(chan))
    }

    pub fn puppet_count(&self, net: &NetId) -> usize {
        self.puppets.keys().filter(|(n, _)| n == net).count()
    }

    pub fn puppet_nick(&self, net: &NetId, uid: Uid) -> Option<String> {
        self.puppets
            .get(&(net.clone(), uid))
            .map(|p| p.face.nick.clone())
    }

    pub fn puppet_in(&self, net: &NetId, nick: &str, chan: &str) -> bool {
        let key = (net.clone(), fold(nick));
        self.puppet_nicks
            .get(&key)
            .and_then(|uid| self.puppets.get(&(net.clone(), *uid)))
            .is_some_and(|p| p.chans.contains_key(&fold(chan)))
    }

    /// Language to use when talking about `k`.
    fn lang_of(&self, k: &ChanKey) -> Lang {
        self.member(k)
            .map(|m| m.policy.lang)
            .unwrap_or(self.cfg.default_lang)
    }

    fn new_policy(&self) -> Policy {
        Policy {
            lang: self.cfg.default_lang,
            ..Policy::default()
        }
    }

    /// Public, privacy-safe summary for the status page.
    pub fn overview(&self) -> Overview {
        let mut o = Overview::default();
        for room in self.rooms.values().filter(|r| r.members.len() >= 2) {
            let mut link = LinkView::default();
            for (k, m) in &room.members {
                o.totals.channels += 1;
                if m.policy.mode == Mode::Live && !m.paused {
                    o.totals.live_channels += 1;
                }
                o.totals.lines_relayed += self.stats.get(k).map(|s| s.in_lines).unwrap_or(0);
                if m.policy.public {
                    link.channels.push(ChanView {
                        network: self.net_cfg(&k.net).label,
                        name: k.chan.clone(),
                        mode: match m.policy.mode {
                            Mode::Off => "off",
                            Mode::Preview => "preview",
                            Mode::Live => "live",
                        }
                        .into(),
                        paused: m.paused,
                    });
                } else {
                    link.hidden += 1;
                }
            }
            o.totals.links += 1;
            if !link.channels.is_empty() {
                o.links.push(link);
            }
        }
        o.totals.puppets = self.puppets.len();
        o
    }

    fn net_cfg(&self, net: &NetId) -> NetConfig {
        let mut c = self.cfg.networks.get(net).cloned().unwrap_or_default();
        if c.label.is_empty() {
            c.label = net.0.clone();
        }
        c
    }

    fn room_of(&self, chan: &ChanKey) -> Option<u64> {
        self.rooms
            .iter()
            .find(|(_, r)| r.members.contains_key(chan))
            .map(|(id, _)| *id)
    }

    // ----------------------------------------------------------- entry ---

    pub fn handle(&mut self, now: u64, ev: Event) -> Vec<Action> {
        let mut out = Vec::new();
        match ev {
            Event::Sync { chan, users } => self.on_sync(now, chan, users, &mut out),
            Event::Join { chan, user, role } => self.on_join(now, chan, user, role, &mut out),
            Event::Part { chan, nick } => self.on_part(chan, &nick, &mut out),
            Event::Quit { net, nick } => self.on_quit(&net, &nick, &mut out),
            Event::NickChange { net, old, new } => self.on_nick(&net, &old, &new, &mut out),
            Event::RoleChange { chan, nick, role } => {
                if let Some(cu) = self
                    .chans
                    .get_mut(&chan)
                    .and_then(|c| c.users.get_mut(&fold(&nick)))
                {
                    cu.role = role;
                }
            }
            Event::AccountChange { net, nick, account } => {
                if let Some(uid) = self.users.get(&(net, fold(&nick))).copied() {
                    if let Some(r) = self.urec.get_mut(&uid) {
                        r.seen.account = account;
                    }
                    self.reconcile_user(now, uid, &mut out);
                }
            }
            Event::Say {
                chan,
                nick,
                text,
                kind,
            } => self.on_say(now, chan, &nick, &text, kind, &mut out),
            Event::Command { net, nick, text } => {
                self.on_command(now, &net, &nick, &text, &mut out)
            }
            Event::PuppetLost {
                net,
                nick,
                chan,
                why,
            } => self.on_puppet_lost(now, &net, &nick, chan, why, &mut out),
            Event::SayRefused { net, nick, chan } => {
                self.on_say_refused(&net, &nick, &chan, &mut out)
            }
            Event::ServiceLeft { chan } => {
                self.chans.remove(&chan);
                self.note(
                    &mut out,
                    now,
                    Some(chan.clone()),
                    "service",
                    "service left the channel; link paused until it is back",
                );
                self.reconcile_all(now, &mut out);
            }
            Event::NetDown { net } => self.on_net_down(now, &net, &mut out),
            Event::Tick => self.on_tick(now, &mut out),
        }
        out
    }

    // ----------------------------------------------------------- audit ---

    fn note(
        &mut self,
        out: &mut Vec<Action>,
        at: u64,
        chan: Option<ChanKey>,
        actor: &str,
        what: &str,
    ) {
        let rec = AuditRecord {
            at,
            chan,
            actor: actor.to_string(),
            what: what.to_string(),
        };
        self.audit.push_back(rec.clone());
        while self.audit.len() > AUDIT_RING {
            self.audit.pop_front();
        }
        out.push(Action::Audit(rec));
    }

    // ------------------------------------------------------ user tracking ---

    fn register(&mut self, net: &NetId, seen: &SeenUser) -> Uid {
        let key = (net.clone(), fold(&seen.nick));
        if let Some(uid) = self.users.get(&key).copied() {
            if let Some(r) = self.urec.get_mut(&uid) {
                r.seen = seen.clone();
            }
            return uid;
        }
        let uid = self.next_uid;
        self.next_uid += 1;
        self.users.insert(key, uid);
        self.urec.insert(
            uid,
            UserRec {
                net: net.clone(),
                seen: seen.clone(),
            },
        );
        uid
    }

    fn forget_if_orphan(&mut self, uid: Uid) {
        if self
            .chans
            .values()
            .any(|c| c.users.values().any(|u| u.uid == uid))
        {
            return;
        }
        if let Some(r) = self.urec.remove(&uid) {
            self.users.remove(&(r.net, fold(&r.seen.nick)));
        }
        self.buckets.retain(|(u, _), _| *u != uid);
        self.cooldown.retain(|(u, _), _| *u != uid);
    }

    // ---------------------------------------------------------- the rules ---

    /// May lines/presence travel from `from` to `to`?
    fn flows(&self, from: &ChanKey, to: &ChanKey) -> bool {
        if from == to || from.net == to.net {
            return false;
        }
        let Some(rid) = self.room_of(from) else {
            return false;
        };
        let room = &self.rooms[&rid];
        let (Some(f), Some(t)) = (room.members.get(from), room.members.get(to)) else {
            return false;
        };
        self.chans.contains_key(from)
            && self.chans.contains_key(to)
            && !f.paused
            && !t.paused
            && f.policy.mode == Mode::Live
            && t.policy.mode == Mode::Live
            && f.policy.share_out
            && t.policy.accept_in
            && !t.policy.muted.contains(&from.to_string())
    }

    /// Is this person blocked by `policy`? A mask is tested against who they
    /// really are on their own network *and* against the face their puppet
    /// wears, so an owner can ban a puppet by the mask their own IRC client
    /// shows (`*!*cha-3fa91@*`) and it sticks to the person behind it.
    fn blocked(&self, policy: &Policy, origin: &NetId, user: &SeenUser) -> bool {
        if policy.block.is_empty() {
            return false;
        }
        let tag = crate::face::net_tag(origin);
        let as_puppet = SeenUser {
            nick: format!("{}|{}", user.nick, tag),
            ident: crate::face::ident_for(origin, user),
            host: "puppet.itl".into(),
            account: user.account.clone(),
            is_bot: user.is_bot,
        };
        policy
            .block
            .iter()
            .any(|m| mask_matches(m, user) || mask_matches(m, &as_puppet))
    }

    /// Should this person be visible in channel `to` (as a puppet)?
    fn shown(&self, now: u64, uid: Uid, to: &ChanKey) -> bool {
        let Some(rec) = self.urec.get(&uid) else {
            return false;
        };
        if rec.net == to.net || (rec.seen.is_bot && !self.cfg.relay_bots) {
            return false;
        }
        if self
            .cooldown
            .get(&(uid, to.clone()))
            .is_some_and(|until| *until > now)
        {
            return false;
        }
        let Some(rid) = self.room_of(to) else {
            return false;
        };
        let room = &self.rooms[&rid];
        let Some(t) = room.members.get(to) else {
            return false;
        };
        if self.blocked(&t.policy, &rec.net, &rec.seen)
            || (t.policy.require_account_in && rec.seen.account.is_none())
        {
            return false;
        }
        room.members.keys().any(|from| {
            self.chans
                .get(from)
                .is_some_and(|c| c.users.values().any(|u| u.uid == uid))
                && self.flows(from, to)
                && room
                    .members
                    .get(from)
                    .is_some_and(|f| !self.blocked(&f.policy, &rec.net, &rec.seen))
        })
    }

    // ------------------------------------------------------------ puppets ---

    fn face_for(&self, uid: Uid, target: &NetId) -> Option<Face> {
        let rec = self.urec.get(&uid)?;
        let origin = self.net_cfg(&rec.net);
        let tcfg = self.net_cfg(target);
        let service = fold(&self.cfg.service_nick);
        let own = self
            .puppets
            .get(&(target.clone(), uid))
            .map(|p| fold(&p.face.nick));
        let taken = |n: &str| {
            let f = fold(n);
            if Some(&f) == own.as_ref() {
                return false;
            }
            f == service
                || self.puppet_nicks.contains_key(&(target.clone(), f.clone()))
                || self.users.contains_key(&(target.clone(), f))
        };
        Some(make_face(
            &rec.net,
            &origin.label,
            &rec.seen,
            &self.cfg.nick_style,
            tcfg.max_nick,
            &taken,
        ))
    }

    fn drop_puppet(&mut self, net: &NetId, uid: Uid) {
        if let Some(p) = self.puppets.remove(&(net.clone(), uid)) {
            self.puppet_nicks.remove(&(net.clone(), fold(&p.face.nick)));
        }
    }

    fn ensure_puppet(&mut self, now: u64, net: &NetId, uid: Uid, out: &mut Vec<Action>) -> bool {
        if self.puppets.contains_key(&(net.clone(), uid)) {
            return true;
        }
        let max = self.net_cfg(net).max_puppets.max(1);
        while self.puppet_count(net) >= max {
            // Make room: the puppet that has been quiet the longest leaves.
            let victim = self
                .puppets
                .iter()
                .filter(|((n, _), _)| n == net)
                .min_by_key(|(_, p)| p.last)
                .map(|((_, u), _)| *u);
            match victim {
                Some(v) => {
                    out.push(Action::PuppetQuit {
                        net: net.clone(),
                        uid: v,
                        reason: "Ita Link: making room".into(),
                    });
                    self.drop_puppet(net, v);
                }
                None => break,
            }
        }
        let Some(face) = self.face_for(uid, net) else {
            return false;
        };
        self.puppet_nicks
            .insert((net.clone(), fold(&face.nick)), uid);
        self.puppets.insert(
            (net.clone(), uid),
            Puppet {
                face: face.clone(),
                chans: HashMap::new(),
                last: now,
            },
        );
        out.push(Action::SpawnPuppet {
            net: net.clone(),
            uid,
            face,
        });
        true
    }

    fn puppet_join(
        &mut self,
        now: u64,
        net: &NetId,
        uid: Uid,
        chan: &ChanKey,
        out: &mut Vec<Action>,
    ) {
        if !self.ensure_puppet(now, net, uid, out) {
            return;
        }
        if let Some(p) = self.puppets.get_mut(&(net.clone(), uid)) {
            p.last = now;
            if p.chans.insert(chan.chan.clone(), now).is_none() {
                out.push(Action::PuppetJoin {
                    net: net.clone(),
                    uid,
                    chan: chan.chan.clone(),
                });
            }
        }
    }

    fn puppet_part(
        &mut self,
        net: &NetId,
        uid: Uid,
        chan: &str,
        reason: &str,
        out: &mut Vec<Action>,
    ) {
        let mut empty = false;
        if let Some(p) = self.puppets.get_mut(&(net.clone(), uid)) {
            if p.chans.remove(chan).is_some() {
                out.push(Action::PuppetPart {
                    net: net.clone(),
                    uid,
                    chan: chan.to_string(),
                    reason: reason.to_string(),
                });
            }
            empty = p.chans.is_empty();
        }
        if empty {
            out.push(Action::PuppetQuit {
                net: net.clone(),
                uid,
                reason: reason.to_string(),
            });
            self.drop_puppet(net, uid);
        }
    }

    /// Bring one person's puppets in line with the current rules.
    fn reconcile_user(&mut self, now: u64, uid: Uid, out: &mut Vec<Action>) {
        let targets: Vec<(ChanKey, Presence)> = self
            .rooms
            .values()
            .flat_map(|r| {
                r.members
                    .iter()
                    .map(|(k, m)| (k.clone(), m.policy.presence))
            })
            .collect();
        for (to, presence) in targets {
            let want = self.shown(now, uid, &to);
            let has = self
                .puppets
                .get(&(to.net.clone(), uid))
                .is_some_and(|p| p.chans.contains_key(&to.chan));
            if want
                && !has
                && presence == Presence::Full
                && self.net_cfg(&to.net).transport == Transport::Puppets
            {
                self.puppet_join(now, &to.net, uid, &to, out);
            } else if !want && has {
                self.puppet_part(&to.net, uid, &to.chan, "Ita Link: no longer shared", out);
            }
        }
        // Channels that are not in any link any more (UNLINK, dissolved room).
        let sitting: Vec<ChanKey> = self
            .puppets
            .iter()
            .filter(|((_, u), _)| *u == uid)
            .flat_map(|((n, _), p)| {
                p.chans.keys().map(move |c| ChanKey {
                    net: n.clone(),
                    chan: c.clone(),
                })
            })
            .collect();
        for k in sitting {
            if !self.shown(now, uid, &k) {
                self.puppet_part(&k.net, uid, &k.chan, "Ita Link: no longer shared", out);
            }
        }
    }

    fn reconcile_all(&mut self, now: u64, out: &mut Vec<Action>) {
        // Puppets whose person is gone from every source disappear...
        let all: Vec<Uid> = self.urec.keys().copied().collect();
        for uid in all {
            self.reconcile_user(now, uid, out);
        }
        // ...and puppets of people we no longer know at all too.
        let orphans: Vec<(NetId, Uid)> = self
            .puppets
            .keys()
            .filter(|(_, u)| !self.urec.contains_key(u))
            .cloned()
            .collect();
        for (n, u) in orphans {
            out.push(Action::PuppetQuit {
                net: n.clone(),
                uid: u,
                reason: "Ita Link: user gone".into(),
            });
            self.drop_puppet(&n, u);
        }
    }

    // ------------------------------------------------------- channel events ---

    fn on_sync(
        &mut self,
        now: u64,
        chan: ChanKey,
        users: Vec<(SeenUser, Role)>,
        out: &mut Vec<Action>,
    ) {
        let mut state = ChanState::default();
        let mut present = Vec::new();
        for (seen, role) in users {
            if fold(&seen.nick) == fold(&self.cfg.service_nick)
                || self
                    .puppet_nicks
                    .contains_key(&(chan.net.clone(), fold(&seen.nick)))
            {
                continue; // ourselves and our own puppets are not "people"
            }
            let uid = self.register(&chan.net, &seen);
            let since = self
                .chans
                .get(&chan)
                .and_then(|c| c.users.get(&fold(&seen.nick)))
                .map(|u| u.since)
                .unwrap_or(now);
            state
                .users
                .insert(fold(&seen.nick), ChanUser { uid, role, since });
            present.push(uid);
        }
        let before: Vec<Uid> = self
            .chans
            .get(&chan)
            .map(|c| c.users.values().map(|u| u.uid).collect())
            .unwrap_or_default();
        self.chans.insert(chan.clone(), state);
        for uid in before.iter().chain(present.iter()) {
            self.reconcile_user(now, *uid, out);
        }
        for uid in before {
            self.forget_if_orphan(uid);
        }
        // This channel just became "known": other channels may now see it.
        self.reconcile_all(now, out);
    }

    fn on_join(
        &mut self,
        now: u64,
        chan: ChanKey,
        user: SeenUser,
        role: Role,
        out: &mut Vec<Action>,
    ) {
        if !self.chans.contains_key(&chan) {
            return;
        }
        if fold(&user.nick) == fold(&self.cfg.service_nick)
            || self
                .puppet_nicks
                .contains_key(&(chan.net.clone(), fold(&user.nick)))
        {
            return;
        }
        let uid = self.register(&chan.net, &user);
        if let Some(c) = self.chans.get_mut(&chan) {
            c.users.insert(
                fold(&user.nick),
                ChanUser {
                    uid,
                    role,
                    since: now,
                },
            );
        }
        self.reconcile_user(now, uid, out);
    }

    fn on_part(&mut self, chan: ChanKey, nick: &str, out: &mut Vec<Action>) {
        let uid = self
            .chans
            .get_mut(&chan)
            .and_then(|c| c.users.remove(&fold(nick)))
            .map(|u| u.uid);
        if let Some(uid) = uid {
            self.reconcile_user(0, uid, out);
            self.forget_if_orphan(uid);
        }
    }

    fn on_quit(&mut self, net: &NetId, nick: &str, out: &mut Vec<Action>) {
        let f = fold(nick);
        let mut uid = None;
        for (k, c) in self.chans.iter_mut() {
            if &k.net == net {
                if let Some(u) = c.users.remove(&f) {
                    uid = Some(u.uid);
                }
            }
        }
        if let Some(uid) = uid {
            self.reconcile_user(0, uid, out);
            self.forget_if_orphan(uid);
        }
    }

    fn on_nick(&mut self, net: &NetId, old: &str, new: &str, out: &mut Vec<Action>) {
        let (fo, fnw) = (fold(old), fold(new));
        let Some(uid) = self.users.remove(&(net.clone(), fo.clone())) else {
            return;
        };
        self.users.insert((net.clone(), fnw.clone()), uid);
        if let Some(r) = self.urec.get_mut(&uid) {
            r.seen.nick = new.to_string();
        }
        for (k, c) in self.chans.iter_mut() {
            if &k.net == net {
                if let Some(u) = c.users.remove(&fo) {
                    c.users.insert(fnw.clone(), u);
                }
            }
        }
        // Their puppets follow the new nick.
        let nets: Vec<NetId> = self
            .puppets
            .keys()
            .filter(|(_, u)| *u == uid)
            .map(|(n, _)| n.clone())
            .collect();
        for tn in nets {
            if let Some(face) = self.face_for(uid, &tn) {
                let cur = self.puppets[&(tn.clone(), uid)].face.nick.clone();
                if fold(&cur) != fold(&face.nick) {
                    self.puppet_nicks.remove(&(tn.clone(), fold(&cur)));
                    self.puppet_nicks
                        .insert((tn.clone(), fold(&face.nick)), uid);
                    if let Some(p) = self.puppets.get_mut(&(tn.clone(), uid)) {
                        p.face.nick = face.nick.clone();
                    }
                    out.push(Action::PuppetNick {
                        net: tn,
                        uid,
                        nick: face.nick,
                    });
                }
            }
        }
    }

    fn on_net_down(&mut self, now: u64, net: &NetId, out: &mut Vec<Action>) {
        let lost: Vec<ChanKey> = self
            .chans
            .keys()
            .filter(|k| &k.net == net)
            .cloned()
            .collect();
        // Tell the neighbours before we forget who they were linked with.
        let label = self.net_cfg(net).label;
        let mut told = BTreeSet::new();
        for k in &lost {
            if let Some(rid) = self.room_of(k) {
                for peer in self.rooms[&rid].members.keys() {
                    if &peer.net != net
                        && self.chans.contains_key(peer)
                        && told.insert(peer.clone())
                    {
                        out.push(Action::Notice {
                            chan: peer.clone(),
                            text: Msg::NetDown { label: &label }.render(self.lang_of(peer)),
                        });
                    }
                }
            }
        }
        for k in &lost {
            if let Some(c) = self.chans.remove(k) {
                for u in c.users.values() {
                    if let Some(r) = self.urec.remove(&u.uid) {
                        self.users.remove(&(r.net, fold(&r.seen.nick)));
                    }
                }
            }
        }
        // Puppet connections on that network died with it.
        let dead: Vec<(NetId, Uid)> = self
            .puppets
            .keys()
            .filter(|(n, _)| n == net)
            .cloned()
            .collect();
        for (n, u) in dead {
            self.drop_puppet(&n, u);
        }
        self.reconcile_all(now, out);
    }

    /// A remote channel did not accept a line from `nick`'s puppet. Tell the
    /// real person once, privately, so they do not talk into the void.
    fn on_say_refused(&mut self, net: &NetId, nick: &str, chan: &str, out: &mut Vec<Action>) {
        let Some(uid) = self.puppet_nicks.get(&(net.clone(), fold(nick))).copied() else {
            return;
        };
        let dest = ChanKey::new(&net.0, chan);
        if !self.told_refused.insert((uid, dest.clone())) {
            return;
        }
        let Some(rec) = self.urec.get(&uid).cloned() else {
            return;
        };
        let origin = self
            .chans
            .iter()
            .find(|(k, c)| k.net == rec.net && c.users.values().any(|u| u.uid == uid))
            .map(|(k, _)| k.clone());
        let Some(origin) = origin else { return };
        let lang = self.lang_of(&origin);
        let label = self.net_cfg(net).label;
        out.push(Action::Reply {
            net: rec.net.clone(),
            nick: rec.seen.nick.clone(),
            text: Msg::NotDelivered {
                chan: &dest.chan,
                net: &label,
            }
            .render(lang),
        });
    }

    fn on_puppet_lost(
        &mut self,
        now: u64,
        net: &NetId,
        nick: &str,
        chan: Option<String>,
        why: LostWhy,
        out: &mut Vec<Action>,
    ) {
        let Some(uid) = self.puppet_nicks.get(&(net.clone(), fold(nick))).copied() else {
            return;
        };
        let ident = self
            .puppets
            .get(&(net.clone(), uid))
            .map(|p| p.face.ident.clone())
            .unwrap_or_default();
        match (why, chan) {
            (LostWhy::Killed, _) | (_, None) => {
                self.drop_puppet(net, uid);
                self.note(
                    out,
                    now,
                    None,
                    "network",
                    &format!("puppet {nick} on {net} removed by the network"),
                );
            }
            (why, Some(c)) => {
                let key = ChanKey::new(&net.0, &c);
                if let Some(p) = self.puppets.get_mut(&(net.clone(), uid)) {
                    p.chans.remove(&key.chan);
                    if p.chans.is_empty() {
                        let n = p.face.nick.clone();
                        self.puppets.remove(&(net.clone(), uid));
                        self.puppet_nicks.remove(&(net.clone(), fold(&n)));
                    }
                }
                if why == LostWhy::Banned {
                    // A ban is a decision: remember it as a permanent block.
                    let mask = format!("*!*{ident}@*");
                    let mut added = false;
                    if let Some(rid) = self.room_of(&key) {
                        if let Some(m) = self
                            .rooms
                            .get_mut(&rid)
                            .and_then(|r| r.members.get_mut(&key))
                        {
                            if !m.policy.block.contains(&mask) {
                                m.policy.block.push(mask.clone());
                                added = true;
                            }
                        }
                    }
                    if added {
                        self.note(
                            out,
                            now,
                            Some(key),
                            "ban",
                            &format!("{nick} was banned: blocked {mask} permanently"),
                        );
                    }
                } else {
                    // A kick is a warning: keep them out for a while.
                    self.cooldown
                        .insert((uid, key.clone()), now + KICK_COOLDOWN_MS);
                    self.note(
                        out,
                        now,
                        Some(key),
                        "kick",
                        &format!("{nick} was kicked: kept out for 10 minutes"),
                    );
                }
            }
        }
    }

    fn on_tick(&mut self, now: u64, out: &mut Vec<Action>) {
        // Lazy puppets leave after their idle time.
        let mut parts: Vec<(NetId, Uid, String)> = Vec::new();
        for ((net, uid), p) in &self.puppets {
            for (chan, last) in &p.chans {
                let key = ChanKey {
                    net: net.clone(),
                    chan: chan.clone(),
                };
                let idle = self.member(&key).and_then(|m| match m.policy.presence {
                    Presence::Lazy { idle_secs } => Some(idle_secs * 1000),
                    Presence::Full => None,
                });
                if let Some(idle) = idle {
                    if now.saturating_sub(*last) > idle {
                        parts.push((net.clone(), *uid, chan.clone()));
                    }
                }
            }
        }
        for (net, uid, chan) in parts {
            self.puppet_part(&net, uid, &chan, "Ita Link: idle", out);
        }
        // Old requests expire.
        let mut expired = Vec::new();
        for (rid, room) in self.rooms.iter_mut() {
            room.requests.retain(|q| {
                let keep = now.saturating_sub(q.at) < REQUEST_TTL_MS;
                if !keep {
                    expired.push((*rid, q.newcomer.clone()));
                }
                keep
            });
        }
        for (_, who) in expired {
            self.note(
                out,
                now,
                Some(who),
                "system",
                "link request expired after 7 days",
            );
        }
        self.cooldown.retain(|_, until| *until > now);
    }

    // ----------------------------------------------------------- messages ---

    fn on_say(
        &mut self,
        now: u64,
        chan: ChanKey,
        nick: &str,
        text: &str,
        kind: SayKind,
        out: &mut Vec<Action>,
    ) {
        if kind == SayKind::Notice {
            return;
        }
        let f = fold(nick);
        if f == fold(&self.cfg.service_nick)
            || self
                .puppet_nicks
                .contains_key(&(chan.net.clone(), f.clone()))
        {
            return; // never relay ourselves or our puppets: that is how loops start
        }
        let Some(rid) = self.room_of(&chan) else {
            return;
        };
        let Some(cu) = self.chans.get(&chan).and_then(|c| c.users.get(&f)).cloned() else {
            return;
        };
        let Some(rec) = self.urec.get(&cu.uid).cloned() else {
            return;
        };
        let room = self.rooms[&rid].clone();
        let Some(src) = room.members.get(&chan) else {
            return;
        };
        if rec.seen.is_bot && !self.cfg.relay_bots {
            return;
        }

        // Source-side filters: the sending channel's own owner decides these.
        let sp = &src.policy;
        let mut why_not: Option<&str> = None;
        if src.paused || sp.mode == Mode::Off || !sp.share_out {
            why_not = Some("not sharing");
        } else if self.blocked(sp, &rec.net, &rec.seen) {
            why_not = Some("blocked");
        } else if kind == SayKind::Message
            && sp
                .drop_prefixes
                .iter()
                .any(|p| text.starts_with(p.as_str()))
        {
            why_not = Some("command");
        } else if now.saturating_sub(cu.since) < sp.min_presence_secs * 1000 {
            why_not = Some("too new");
        }
        let mut body = if sp.strip_formatting {
            strip_formatting(text)
        } else {
            text.to_string()
        };
        body = body.trim().to_string();
        if body.is_empty() {
            why_not = Some("empty");
        }
        if let Some(w) = why_not {
            if w != "not sharing" && w != "command" && w != "empty" {
                self.stats.entry(chan.clone()).or_default().dropped_filter += 1;
            }
            // Tell a newcomer once why nothing crossed, instead of dropping silently.
            if w == "too new"
                && kind == SayKind::Message
                && self.told_new.insert((cu.uid, chan.clone()))
            {
                let wait = (sp.min_presence_secs * 1000)
                    .saturating_sub(now.saturating_sub(cu.since))
                    .div_ceil(1000);
                let l = self.lang_of(&chan);
                out.push(Action::Reply {
                    net: chan.net.clone(),
                    nick: nick.to_string(),
                    text: Msg::TooNew {
                        chan: &chan.chan,
                        secs: wait,
                    }
                    .render(l),
                });
            }
            return;
        }

        let preview_only_source = sp.mode == Mode::Preview;
        let tag_label = self.net_cfg(&rec.net).label;
        let _ = tag_label;
        for (to, tm) in room.members.iter() {
            if to == &chan || to.net == chan.net {
                continue;
            }
            // Preview on either side: record what would have happened, send nothing.
            if preview_only_source {
                self.stats.entry(chan.clone()).or_default().would_out += 1;
                continue;
            }
            if tm.policy.mode == Mode::Preview {
                if !self.chans.contains_key(&chan)
                    || !self.chans.contains_key(to)
                    || tm.paused
                    || !tm.policy.accept_in
                {
                    continue;
                }
                if sp.mode == Mode::Live
                    && !src.paused
                    && sp.share_out
                    && !tm.policy.muted.contains(&chan.to_string())
                {
                    let line = self
                        .face_for(cu.uid, &to.net)
                        .map(|f| f.nick)
                        .unwrap_or_else(|| rec.seen.nick.clone());
                    let line = match kind {
                        SayKind::Action => format!("* {line} {body}"),
                        _ => format!("<{line}> {body}"),
                    };
                    let q = self.samples.entry(to.clone()).or_default();
                    q.push_back(truncate_chars(&line, 200));
                    while q.len() > SAMPLE_RING {
                        q.pop_front();
                    }
                    self.stats.entry(to.clone()).or_default().would_in += 1;
                }
                continue;
            }
            if !self.flows(&chan, to) || !self.shown(now, cu.uid, to) {
                continue;
            }
            // Receiver-side flood protection, per person.
            let rate = tm.policy.rate;
            let ok = self
                .buckets
                .entry((cu.uid, to.clone()))
                .or_insert_with(|| Bucket::new(rate, now))
                .allow(rate, now);
            if !ok {
                self.stats.entry(to.clone()).or_default().dropped_rate += 1;
                continue;
            }
            let line = truncate_chars(&body, sp.max_line.min(tm.policy.max_line));
            let tcfg = self.net_cfg(&to.net);
            match tcfg.transport {
                Transport::Puppets => {
                    self.puppet_join(now, &to.net, cu.uid, to, out);
                    if let Some(p) = self.puppets.get_mut(&(to.net.clone(), cu.uid)) {
                        p.chans.insert(to.chan.clone(), now);
                        p.last = now;
                    }
                    out.push(Action::Say {
                        net: to.net.clone(),
                        via: Via::Puppet(cu.uid),
                        chan: to.chan.clone(),
                        kind,
                        text: line,
                    });
                }
                Transport::ServiceBot => {
                    let label = self
                        .face_for(cu.uid, &to.net)
                        .map(|f| f.nick)
                        .unwrap_or_else(|| rec.seen.nick.clone());
                    let text = match kind {
                        SayKind::Action => format!("* {label} {line}"),
                        _ => format!("<{label}> {line}"),
                    };
                    out.push(Action::Say {
                        net: to.net.clone(),
                        via: Via::Service,
                        chan: to.chan.clone(),
                        kind: SayKind::Message,
                        text,
                    });
                }
            }
            self.stats.entry(to.clone()).or_default().in_lines += 1;
            self.stats.entry(chan.clone()).or_default().out_lines += 1;
        }
    }

    // ----------------------------------------------------------- commands ---

    fn reply(out: &mut Vec<Action>, net: &NetId, nick: &str, text: impl Into<String>) {
        out.push(Action::Reply {
            net: net.clone(),
            nick: nick.to_string(),
            text: text.into(),
        });
    }

    /// Check the caller may act for `chan`: service present, caller is an op.
    fn auth(&self, net: &NetId, nick: &str, chan: &str) -> Result<ChanKey, String> {
        let key = ChanKey::new(&net.0, chan);
        let l = self.lang_of(&key);
        let Some(cs) = self.chans.get(&key) else {
            return Err(Msg::NotInChannel {
                chan,
                svc: &self.cfg.service_nick,
            }
            .render(l));
        };
        let Some(cu) = cs.users.get(&fold(nick)) else {
            return Err(Msg::NotSeen { chan }.render(l));
        };
        if cu.role < Role::Op {
            return Err(Msg::NotOp { chan }.render(l));
        }
        if self.cfg.require_account_for_owner
            && self
                .urec
                .get(&cu.uid)
                .is_some_and(|r| r.seen.account.is_none())
        {
            return Err(Msg::NeedAccount.render(l));
        }
        Ok(key)
    }

    fn on_command(&mut self, now: u64, net: &NetId, nick: &str, text: &str, out: &mut Vec<Action>) {
        let cmd = match command::parse(text) {
            Ok(c) => c,
            Err(e) => {
                Self::reply(out, net, nick, e.to_string());
                return;
            }
        };
        let r = self.run_command(now, net, nick, cmd, out);
        if let Err(msg) = r {
            Self::reply(out, net, nick, msg);
        }
    }

    fn member_mut(&mut self, key: &ChanKey) -> Result<&mut Member, String> {
        let l = self.lang_of(key);
        self.rooms
            .values_mut()
            .find_map(|r| r.members.get_mut(key))
            .ok_or_else(|| Msg::NotLinkedYet { chan: &key.chan }.render(l))
    }

    fn run_command(
        &mut self,
        now: u64,
        net: &NetId,
        nick: &str,
        cmd: Command,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        use Command::*;
        match cmd {
            Help(topic) => {
                for l in crate::msg::help_lines(
                    topic.as_deref(),
                    &self.cfg.service_nick,
                    self.cfg.default_lang,
                ) {
                    Self::reply(out, net, nick, l);
                }
                Ok(())
            }
            Link { mine, other } => {
                let k = self.auth(net, nick, &mine)?;
                self.do_link(now, nick, k, other, out)
            }
            Accept { mine, other } => {
                let k = self.auth(net, nick, &mine)?;
                self.do_accept(now, nick, k, other, out)
            }
            Deny { mine, other } => {
                let k = self.auth(net, nick, &mine)?;
                self.do_deny(now, nick, k, other, out)
            }
            Unlink { chan } => {
                let k = self.auth(net, nick, &chan)?;
                let _l = self.lang_of(&k);
                self.do_unlink(now, nick, k, out)
            }
            Pause { chan } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                self.member_mut(&k)?.paused = true;
                self.note(out, now, Some(k.clone()), nick, "paused the link");
                self.reconcile_all(now, out);
                Self::reply(out, net, nick, Msg::Paused { chan: &chan }.render(l));
                Ok(())
            }
            Resume { chan } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                self.member_mut(&k)?.paused = false;
                self.note(out, now, Some(k.clone()), nick, "resumed the link");
                self.reconcile_all(now, out);
                Self::reply(out, net, nick, Msg::Resumed { chan: &chan }.render(l));
                Ok(())
            }
            SetMode { chan, mode } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let m = match mode {
                    ModeWord::Off => Mode::Off,
                    ModeWord::Preview => Mode::Preview,
                    ModeWord::Live => Mode::Live,
                };
                self.member_mut(&k)?.policy.mode = m;
                self.note(
                    out,
                    now,
                    Some(k.clone()),
                    nick,
                    &format!("mode set to {m:?}"),
                );
                self.reconcile_all(now, out);
                let msg = match m {
                    Mode::Live => Msg::ModeLive { chan: &chan },
                    Mode::Preview => Msg::ModePreview { chan: &chan },
                    Mode::Off => Msg::ModeOff { chan: &chan },
                }
                .render(l);
                Self::reply(out, net, nick, msg);
                Ok(())
            }
            Set { chan, key, value } => {
                let k = self.auth(net, nick, &chan)?;
                let _l = self.lang_of(&k);
                self.member_mut(&k)?
                    .policy
                    .set(&key, &value)
                    .map_err(|e| e.to_string())?;
                self.note(
                    out,
                    now,
                    Some(k.clone()),
                    nick,
                    &format!("set {key} = {value}"),
                );
                self.reconcile_all(now, out);
                let l = self.lang_of(&k);
                Self::reply(
                    out,
                    net,
                    nick,
                    Msg::SetOk {
                        chan: &chan,
                        key: &key,
                        value: &value,
                    }
                    .render(l),
                );
                Ok(())
            }
            Block { chan, mask } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let mask = normalise_mask(&mask).map_err(|_| Msg::BadMask.render(l))?;
                let m = self.member_mut(&k)?;
                if m.policy.block.len() >= 200 {
                    return Err(Msg::BlockFull.render(l));
                }
                if !m.policy.block.contains(&mask) {
                    m.policy.block.push(mask.clone());
                }
                self.note(out, now, Some(k), nick, &format!("blocked {mask}"));
                self.reconcile_all(now, out);
                Self::reply(
                    out,
                    net,
                    nick,
                    Msg::Blocked {
                        chan: &chan,
                        mask: &mask,
                    }
                    .render(l),
                );
                Ok(())
            }
            Unblock { chan, mask } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let mask = normalise_mask(&mask).map_err(|_| Msg::BadMask.render(l))?;
                let m = self.member_mut(&k)?;
                let before = m.policy.block.len();
                m.policy.block.retain(|b| b != &mask);
                if m.policy.block.len() == before {
                    return Err(Msg::NotBlocked { mask: &mask }.render(l));
                }
                self.note(out, now, Some(k), nick, &format!("unblocked {mask}"));
                self.reconcile_all(now, out);
                Self::reply(
                    out,
                    net,
                    nick,
                    Msg::Unblocked {
                        chan: &chan,
                        mask: &mask,
                    }
                    .render(l),
                );
                Ok(())
            }
            Mute { chan, other } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let m = self.member_mut(&k)?;
                let o = other.to_string();
                if !m.policy.muted.contains(&o) {
                    m.policy.muted.push(o.clone());
                }
                self.note(out, now, Some(k), nick, &format!("muted {o}"));
                self.reconcile_all(now, out);
                Self::reply(
                    out,
                    net,
                    nick,
                    Msg::Muted {
                        chan: &chan,
                        other: &o,
                    }
                    .render(l),
                );
                Ok(())
            }
            Unmute { chan, other } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let o = other.to_string();
                self.member_mut(&k)?.policy.muted.retain(|x| x != &o);
                self.note(out, now, Some(k), nick, &format!("unmuted {o}"));
                self.reconcile_all(now, out);
                Self::reply(
                    out,
                    net,
                    nick,
                    Msg::Unmuted {
                        chan: &chan,
                        other: &o,
                    }
                    .render(l),
                );
                Ok(())
            }
            Status { chan } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                for line in self.status_lines(&k, l) {
                    Self::reply(out, net, nick, line);
                }
                Ok(())
            }
            Sample { chan } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                match self.samples.get(&k).filter(|q| !q.is_empty()) {
                    None => Self::reply(out, net, nick, Msg::SampleNone.render(l)),
                    Some(q) => {
                        Self::reply(
                            out,
                            net,
                            nick,
                            Msg::SampleHeader {
                                n: q.len(),
                                chan: &chan,
                            }
                            .render(l),
                        );
                        for line in q.iter() {
                            Self::reply(out, net, nick, line.clone());
                        }
                    }
                }
                Ok(())
            }
            Audit { chan, count } => {
                let k = self.auth(net, nick, &chan)?;
                let l = self.lang_of(&k);
                let recs: Vec<&AuditRecord> = self
                    .audit
                    .iter()
                    .filter(|r| r.chan.as_ref() == Some(&k))
                    .collect();
                if recs.is_empty() {
                    Self::reply(out, net, nick, Msg::AuditNone.render(l));
                }
                for r in recs.iter().rev().take(count).rev() {
                    Self::reply(
                        out,
                        net,
                        nick,
                        format!("[{}s] {}: {}", r.at / 1000, r.actor, r.what),
                    );
                }
                Ok(())
            }
        }
    }

    fn status_lines(&self, k: &ChanKey, lang: Lang) -> Vec<String> {
        let it = lang == Lang::It;
        let mut v = Vec::new();
        let Some(rid) = self.room_of(k) else {
            v.push(if it {
                format!(
                    "{} non è collegato a niente. LINK <#canale> <rete> <#loro> avvia un link.",
                    k.chan
                )
            } else {
                format!(
                    "{} is not linked to anything. LINK <#chan> <network> <#theirs> starts a link.",
                    k.chan
                )
            });
            return v;
        };
        let room = &self.rooms[&rid];
        let m = &room.members[k];
        let word = |mode: Mode| match mode {
            Mode::Off => "OFF",
            Mode::Preview => "PREVIEW",
            Mode::Live => "LIVE",
        };
        let paused = if it { " (in pausa)" } else { " (paused)" };
        v.push(format!(
            "{} {} {}{}.",
            k.chan,
            if it { "è" } else { "is" },
            word(m.policy.mode),
            if m.paused { paused } else { "" }
        ));
        for (c, mm) in &room.members {
            if c == k {
                continue;
            }
            let here = if self.chans.contains_key(c) {
                ""
            } else if it {
                ", servizio assente"
            } else {
                ", service not present"
            };
            let pz = if mm.paused {
                if it {
                    ", in pausa"
                } else {
                    ", paused"
                }
            } else {
                ""
            };
            v.push(format!(
                "  {} {} ({}{}{})",
                if it { "collegato con" } else { "linked with" },
                c,
                word(mm.policy.mode).to_ascii_lowercase(),
                pz,
                here
            ));
        }
        for q in &room.requests {
            let mut w: Vec<String> = Vec::new();
            if !q.newcomer_ok {
                w.push(if it {
                    format!("{} stesso", q.newcomer)
                } else {
                    format!("{} itself", q.newcomer)
                });
            }
            w.extend(
                room.members
                    .keys()
                    .filter(|x| !q.approvals.contains(*x))
                    .map(|x| x.to_string()),
            );
            v.push(if it {
                format!(
                    "  in attesa: {} vuole entrare; aspetto {}",
                    q.newcomer,
                    w.join(", ")
                )
            } else {
                format!(
                    "  pending: {} wants in; waiting for {}",
                    q.newcomer,
                    w.join(", ")
                )
            });
        }
        v.extend(
            m.policy
                .describe(lang)
                .into_iter()
                .map(|l| format!("  {l}")),
        );
        let s = self.stats(k);
        v.push(if it {
            format!(
                "  righe in entrata {} | in uscita {} | scartate per flood {} / per filtri {} | anteprima: avrebbe mostrato {}, avrebbe inviato {}",
                s.in_lines, s.out_lines, s.dropped_rate, s.dropped_filter, s.would_in, s.would_out
            )
        } else {
            format!(
                "  lines in {} | out {} | dropped by rate {} / by filters {} | preview would-in {} would-out {}",
                s.in_lines, s.out_lines, s.dropped_rate, s.dropped_filter, s.would_in, s.would_out
            )
        });
        v
    }

    fn do_link(
        &mut self,
        now: u64,
        nick: &str,
        mine: ChanKey,
        other: ChanKey,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        let l = self.lang_of(&mine);
        if mine.net == other.net {
            return Err(Msg::SameNetwork.render(l));
        }
        if !self.chans.contains_key(&other) {
            return Err(Msg::TheirServiceMissing {
                chan: &other.chan,
                net: &other.net.0,
                svc: &self.cfg.service_nick,
            }
            .render(l));
        }
        let (rm, ro) = (self.room_of(&mine), self.room_of(&other));
        let (rid, newcomer, newcomer_ok, requester_is_member) = match (rm, ro) {
            (Some(a), Some(b)) if a == b => return Err(Msg::AlreadyLinked.render(l)),
            (Some(_), Some(_)) => return Err(Msg::BothInLinks.render(l)),
            (None, Some(r)) => (r, mine.clone(), true, false),
            (Some(r), None) => (r, other.clone(), false, true),
            (None, None) => {
                let r = self.next_room;
                self.next_room += 1;
                self.rooms.insert(r, Room::default());
                let pol = self.new_policy();
                self.rooms.get_mut(&r).unwrap().members.insert(
                    mine.clone(),
                    Member {
                        policy: pol,
                        paused: false,
                    },
                );
                self.note(out, now, Some(mine.clone()), nick, "created a new link");
                (r, other.clone(), false, true)
            }
        };
        {
            let room = self.rooms.get_mut(&rid).unwrap();
            if room.members.keys().any(|m| m.net == newcomer.net) {
                return Err(Msg::OnePerNet {
                    net: &newcomer.net.0,
                }
                .render(l));
            }
            if let Some(q) = room.requests.iter_mut().find(|q| q.newcomer == newcomer) {
                if requester_is_member {
                    q.approvals.insert(mine.clone());
                } else {
                    q.newcomer_ok = true;
                }
            } else {
                let mut approvals = BTreeSet::new();
                if requester_is_member {
                    approvals.insert(mine.clone());
                }
                room.requests.push(Request {
                    newcomer: newcomer.clone(),
                    approvals,
                    newcomer_ok,
                    at: now,
                    by: nick.to_string(),
                });
            }
        }
        self.note(
            out,
            now,
            Some(mine.clone()),
            nick,
            &format!("asked to link with {other}"),
        );
        out.push(Action::Notice {
            chan: other.clone(),
            text: Msg::AskNotice {
                mine_chan: &mine.chan,
                mine_net: &mine.net.0,
                other_chan: &other.chan,
                svc: &self.cfg.service_nick,
            }
            .render(self.lang_of(&other)),
        });
        self.try_admit(now, rid, out);
        let reply = Msg::RequestSent { other: &other.chan }.render(l);
        out.push(Action::Reply {
            net: mine.net.clone(),
            nick: nick.to_string(),
            text: reply,
        });
        Ok(())
    }

    fn do_accept(
        &mut self,
        now: u64,
        nick: &str,
        mine: ChanKey,
        other: ChanKey,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        let mut done = None;
        for (rid, room) in self.rooms.iter_mut() {
            // I am the newcomer, `other` belongs to the room that asked me.
            if room.members.contains_key(&other) {
                if let Some(q) = room.requests.iter_mut().find(|q| q.newcomer == mine) {
                    q.newcomer_ok = true;
                    done = Some(*rid);
                    break;
                }
            }
            // I am an existing member approving `other` as newcomer.
            if room.members.contains_key(&mine) {
                if let Some(q) = room.requests.iter_mut().find(|q| q.newcomer == other) {
                    q.approvals.insert(mine.clone());
                    done = Some(*rid);
                    break;
                }
            }
        }
        let Some(rid) = done else {
            return Err(Msg::NoPending {
                mine: &mine.chan,
                net: &other.net.0,
                chan: &other.chan,
            }
            .render(self.lang_of(&mine)));
        };
        self.note(
            out,
            now,
            Some(mine.clone()),
            nick,
            &format!("accepted the link with {other}"),
        );
        self.try_admit(now, rid, out);
        out.push(Action::Reply {
            net: mine.net.clone(),
            nick: nick.to_string(),
            text: Msg::Accepted.render(self.lang_of(&mine)),
        });
        Ok(())
    }

    fn do_deny(
        &mut self,
        now: u64,
        nick: &str,
        mine: ChanKey,
        other: ChanKey,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        let mut removed = false;
        for room in self.rooms.values_mut() {
            let before = room.requests.len();
            room.requests.retain(|q| {
                let involves = (q.newcomer == mine && room.members.contains_key(&other))
                    || (q.newcomer == other && room.members.contains_key(&mine));
                !involves
            });
            removed |= room.requests.len() != before;
        }
        if !removed {
            return Err(Msg::NoSuchRequest.render(self.lang_of(&mine)));
        }
        self.note(
            out,
            now,
            Some(mine.clone()),
            nick,
            &format!("denied/withdrew the link with {other}"),
        );
        self.drop_lonely_rooms();
        out.push(Action::Reply {
            net: mine.net.clone(),
            nick: nick.to_string(),
            text: Msg::RequestRemoved.render(self.lang_of(&mine)),
        });
        Ok(())
    }

    fn drop_lonely_rooms(&mut self) {
        self.rooms
            .retain(|_, r| r.members.len() >= 2 || !r.requests.is_empty());
    }

    fn try_admit(&mut self, now: u64, rid: u64, out: &mut Vec<Action>) {
        // First change the room, then talk about it: no borrow is held while
        // we write notices and audit entries.
        let mut admitted: Vec<(Request, Vec<ChanKey>)> = Vec::new();
        let pol = self.new_policy();
        {
            let Some(room) = self.rooms.get_mut(&rid) else {
                return;
            };
            let ready: Vec<usize> = room
                .requests
                .iter()
                .enumerate()
                .filter(|(_, q)| {
                    q.newcomer_ok && room.members.keys().all(|m| q.approvals.contains(m))
                })
                .map(|(i, _)| i)
                .collect();
            for i in ready.into_iter().rev() {
                let q = room.requests.remove(i);
                room.members.insert(
                    q.newcomer.clone(),
                    Member {
                        policy: pol.clone(),
                        paused: false,
                    },
                );
                admitted.push((q, room.members.keys().cloned().collect()));
            }
        }
        for (q, members) in admitted {
            let label = self
                .cfg
                .networks
                .get(&q.newcomer.net)
                .map(|c| c.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| q.newcomer.net.0.clone());
            for m in &members {
                if self.chans.contains_key(m) {
                    let lm = self.lang_of(m);
                    let txt = if m == &q.newcomer {
                        Msg::Admitted {
                            svc: &self.cfg.service_nick,
                            chan: &m.chan,
                        }
                        .render(lm)
                    } else {
                        Msg::AdmittedOther {
                            chan: &q.newcomer.chan,
                            label: &label,
                        }
                        .render(lm)
                    };
                    out.push(Action::Notice {
                        chan: m.clone(),
                        text: txt,
                    });
                }
            }
            self.note(
                out,
                now,
                Some(q.newcomer.clone()),
                "system",
                "link established (preview)",
            );
        }
    }

    fn do_unlink(
        &mut self,
        now: u64,
        nick: &str,
        k: ChanKey,
        out: &mut Vec<Action>,
    ) -> Result<(), String> {
        let l = self.lang_of(&k);
        let Some(rid) = self.room_of(&k) else {
            return Err(Msg::NotLinked { chan: &k.chan }.render(l));
        };
        let others: Vec<ChanKey> = {
            let room = self.rooms.get_mut(&rid).unwrap();
            room.members.remove(&k);
            room.requests.retain(|q| q.newcomer != k);
            for q in room.requests.iter_mut() {
                q.approvals.remove(&k);
            }
            room.members.keys().cloned().collect()
        };
        for o in others {
            if self.chans.contains_key(&o) {
                let lo = self.lang_of(&o);
                out.push(Action::Notice {
                    chan: o,
                    text: Msg::LeftNotice {
                        chan: &k.chan,
                        net: &k.net.0,
                    }
                    .render(lo),
                });
            }
        }
        self.drop_lonely_rooms();
        self.note(out, now, Some(k.clone()), nick, "left the link");
        self.reconcile_all(now, out);
        out.push(Action::Reply {
            net: k.net.clone(),
            nick: nick.to_string(),
            text: Msg::Unlinked { chan: &k.chan }.render(l),
        });
        Ok(())
    }
}

fn normalise_mask(m: &str) -> Result<String, String> {
    let m = m.trim();
    if m.is_empty() || m.len() > 100 {
        return Err("The mask must be 1-100 characters.".into());
    }
    if m.starts_with("account:") || m.contains('!') || m.contains('@') {
        return Ok(m.to_string());
    }
    Ok(format!("{m}!*@*"))
}
