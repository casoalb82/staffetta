//! Behavioural scenarios for the relay engine, written the way an owner would
//! describe them. No networking: events in, actions out.

use itl_core::engine::*;
use itl_core::model::*;

const SVC: &str = "ItaLink";

struct World {
    e: Engine,
    now: u64,
}

fn net(s: &str) -> NetId {
    NetId::new(s)
}

fn chan(n: &str, c: &str) -> ChanKey {
    ChanKey::new(n, c)
}

fn user(nick: &str) -> SeenUser {
    SeenUser::new(nick, "id", &format!("{nick}.example"))
}

impl World {
    fn new() -> World {
        let mut cfg = EngineConfig {
            service_nick: SVC.into(),
            ..Default::default()
        };
        for (n, l) in [
            ("chatnet", "ChatNet"),
            ("azzurra", "Azzurra"),
            ("irc-it", "IRC-IT"),
        ] {
            cfg.networks.insert(
                net(n),
                NetConfig {
                    label: l.into(),
                    ..Default::default()
                },
            );
        }
        World {
            e: Engine::new(cfg),
            now: 1_000_000,
        }
    }

    fn ev(&mut self, ev: Event) -> Vec<Action> {
        self.e.handle(self.now, ev)
    }

    fn sync(&mut self, c: &ChanKey, users: &[(&str, Role)]) -> Vec<Action> {
        let list = users.iter().map(|(n, r)| (user(n), *r)).collect();
        self.ev(Event::Sync {
            chan: c.clone(),
            users: list,
        })
    }

    fn cmd(&mut self, n: &str, nick: &str, text: &str) -> Vec<Action> {
        self.ev(Event::Command {
            net: net(n),
            nick: nick.into(),
            text: text.into(),
        })
    }

    fn say(&mut self, c: &ChanKey, nick: &str, text: &str) -> Vec<Action> {
        self.ev(Event::Say {
            chan: c.clone(),
            nick: nick.into(),
            text: text.into(),
            kind: SayKind::Message,
        })
    }

    fn advance(&mut self, secs: u64) {
        self.now += secs * 1000;
    }

    /// Two channels, ops "opA"/"opB" and one normal user each, linked and LIVE.
    fn linked_live() -> (World, ChanKey, ChanKey) {
        let mut w = World::new();
        let a = chan("chatnet", "#italy");
        let b = chan("azzurra", "#italia");
        w.sync(&a, &[("opA", Role::Op), ("alice", Role::Normal)]);
        w.sync(&b, &[("opB", Role::Op), ("bob", Role::Normal)]);
        w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
        w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy");
        w.cmd("chatnet", "opA", "LIVE #italy");
        w.cmd("azzurra", "opB", "LIVE #italia");
        w.advance(600); // everyone has been around long enough
        (w, a, b)
    }
}

fn replies(acts: &[Action]) -> Vec<String> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Reply { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn says(acts: &[Action]) -> Vec<(NetId, Via, String, String)> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Say {
                net,
                via,
                chan,
                text,
                ..
            } => Some((net.clone(), via.clone(), chan.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

fn has_spawn(acts: &[Action]) -> bool {
    acts.iter().any(|a| matches!(a, Action::SpawnPuppet { .. }))
}

// ------------------------------------------------------------ consent ---

#[test]
fn service_must_be_invited_before_a_link_can_be_asked() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    w.sync(&a, &[("opA", Role::Op)]);
    let r = replies(&w.cmd("chatnet", "opA", "LINK #italy azzurra #italia"));
    assert!(r[0].contains("/INVITE"), "{r:?}");
    // And you cannot manage a channel the service is not in.
    let r = replies(&w.cmd("azzurra", "opB", "STATUS #italia"));
    assert!(r[0].contains("/INVITE"), "{r:?}");
}

#[test]
fn only_operators_can_command() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    w.sync(&a, &[("opA", Role::Op), ("alice", Role::Voice)]);
    let r = replies(&w.cmd("chatnet", "alice", "SET #italy rate 1/10"));
    assert!(r[0].contains("operators"), "{r:?}");
    let r = replies(&w.cmd("chatnet", "nobody", "STATUS #italy"));
    assert!(r[0].contains("do not see you"), "{r:?}");
}

#[test]
fn a_link_needs_both_owners_and_starts_in_preview() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    let b = chan("azzurra", "#italia");
    w.sync(&a, &[("opA", Role::Op), ("alice", Role::Normal)]);
    w.sync(&b, &[("opB", Role::Op), ("bob", Role::Normal)]);

    let acts = w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
    // The other channel's operators are told, with the exact command to run.
    assert!(acts.iter().any(|x| matches!(x, Action::Notice { chan, text } if chan == &b && text.contains("ACCEPT #italia chatnet #italy"))));
    assert!(w.e.member(&b).is_none(), "not linked until they accept");

    // The target can also refuse.
    let acts = w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy");
    assert!(acts
        .iter()
        .any(|x| matches!(x, Action::Notice { text, .. } if text.contains("PREVIEW"))));
    assert_eq!(
        w.e.member(&a).unwrap().policy.mode,
        itl_core::policy::Mode::Preview
    );
    assert_eq!(
        w.e.member(&b).unwrap().policy.mode,
        itl_core::policy::Mode::Preview
    );

    // Preview: not one line crosses.
    w.advance(600);
    assert!(says(&w.say(&a, "alice", "ciao a tutti")).is_empty());
    assert!(says(&w.say(&b, "bob", "ciao")).is_empty());
}

#[test]
fn deny_removes_the_request() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    let b = chan("azzurra", "#italia");
    w.sync(&a, &[("opA", Role::Op)]);
    w.sync(&b, &[("opB", Role::Op)]);
    w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
    let r = replies(&w.cmd("azzurra", "opB", "DENY #italia chatnet #italy"));
    assert!(r[0].contains("removed"));
    let r = replies(&w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy"));
    assert!(r[0].contains("No pending"), "{r:?}");
}

#[test]
fn same_network_links_are_refused() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    let a2 = chan("chatnet", "#italia");
    w.sync(&a, &[("opA", Role::Op)]);
    w.sync(&a2, &[("opA", Role::Op)]);
    let r = replies(&w.cmd("chatnet", "opA", "LINK #italy chatnet #italia"));
    assert!(r[0].contains("same network"), "{r:?}");
}

#[test]
fn a_third_channel_needs_approval_from_everyone() {
    let (mut w, a, b) = World::linked_live();
    let c = chan("irc-it", "#chat");
    w.sync(&c, &[("opC", Role::Op), ("carl", Role::Normal)]);
    w.cmd("irc-it", "opC", "LINK #chat chatnet #italy");
    w.cmd("chatnet", "opA", "ACCEPT #italy irc-it #chat");
    assert!(w.e.member(&c).is_none(), "azzurra has not agreed yet");
    w.cmd("azzurra", "opB", "ACCEPT #italia irc-it #chat");
    assert!(w.e.member(&c).is_some());
    let _ = (a, b);
}

// -------------------------------------------------------------- flow ---

#[test]
fn live_on_one_side_only_shows_nothing_but_previews_it() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    let b = chan("azzurra", "#italia");
    w.sync(&a, &[("opA", Role::Op), ("alice", Role::Normal)]);
    w.sync(&b, &[("opB", Role::Op), ("bob", Role::Normal)]);
    w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
    w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy");
    w.cmd("chatnet", "opA", "LIVE #italy"); // only A goes live
    w.advance(600);
    assert!(
        says(&w.say(&a, "alice", "ciao")).is_empty(),
        "B is still preview: nothing is sent"
    );
    let r = replies(&w.cmd("azzurra", "opB", "SAMPLE #italia"));
    assert!(
        r.iter()
            .any(|l| l.contains("alice|cha") && l.contains("ciao")),
        "{r:?}"
    );
}

#[test]
fn live_both_sides_relays_with_a_tagged_puppet() {
    let (mut w, a, b) = World::linked_live();
    let acts = w.say(&a, "alice", "ciao da chatnet");
    assert!(has_spawn(&acts));
    let face = acts.iter().find_map(|x| match x {
        Action::SpawnPuppet { face, net, .. } if net == &b.net => Some(face.clone()),
        _ => None,
    });
    let face = face.expect("puppet on azzurra");
    assert_eq!(face.nick, "alice|cha");
    assert!(face.ident.starts_with("cha-"));
    assert!(face.realname.contains("relayed"));
    let s = says(&acts);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].0, b.net);
    assert!(matches!(s[0].1, Via::Puppet(_)));
    assert_eq!(s[0].3, "ciao da chatnet");
    // And the other way round.
    let acts = w.say(&b, "bob", "ciao da azzurra");
    assert_eq!(says(&acts)[0].0, a.net);
}

#[test]
fn lazy_puppets_join_when_they_speak_and_leave_when_idle() {
    let (mut w, a, b) = World::linked_live();
    let acts = w.say(&a, "alice", "eccomi");
    assert!(acts.iter().any(
        |x| matches!(x, Action::PuppetJoin { net, chan, .. } if net == &b.net && chan == "#italia")
    ));
    w.advance(31 * 60); // default idle is 30 minutes
    let acts = w.ev(Event::Tick);
    assert!(acts.iter().any(|x| matches!(x, Action::PuppetPart { .. })));
    assert!(acts.iter().any(|x| matches!(x, Action::PuppetQuit { .. })));
    assert_eq!(w.e.puppet_count(&b.net), 0);
}

#[test]
fn full_presence_fills_the_user_list_and_follows_parts() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "SET #italia presence full");
    let acts = w.ev(Event::Join {
        chan: a.clone(),
        user: user("carla"),
        role: Role::Normal,
    });
    assert!(acts.iter().any(
        |x| matches!(x, Action::PuppetJoin { net, chan, .. } if net == &b.net && chan == "#italia")
    ));
    let acts = w.ev(Event::Part {
        chan: a.clone(),
        nick: "carla".into(),
    });
    assert!(acts.iter().any(|x| matches!(x, Action::PuppetPart { .. })));
    assert!(acts.iter().any(|x| matches!(x, Action::PuppetQuit { .. })));
}

#[test]
fn bots_commands_and_formatting_are_handled() {
    let (mut w, a, _b) = World::linked_live();
    assert!(
        says(&w.say(&a, "alice", "!help")).is_empty(),
        "bot commands are not relayed"
    );
    assert!(says(&w.say(&a, "alice", ".duck")).is_empty());
    let s = says(&w.say(&a, "alice", "\x02ciao\x02 \x0304rosso\x03"));
    assert_eq!(s[0].3, "ciao rosso");
    let long = "x".repeat(1000);
    let s = says(&w.say(&a, "alice", &long));
    assert!(s[0].3.chars().count() <= 350);
    // Notices are never relayed.
    let acts = w.ev(Event::Say {
        chan: a.clone(),
        nick: "alice".into(),
        text: "hi".into(),
        kind: SayKind::Notice,
    });
    assert!(says(&acts).is_empty());
}

#[test]
fn newcomers_cannot_speak_across_immediately() {
    let (mut w, a, _b) = World::linked_live();
    w.ev(Event::Join {
        chan: a.clone(),
        user: user("troll"),
        role: Role::Normal,
    });
    w.advance(10);
    let acts = w.say(&a, "troll", "flame");
    assert!(says(&acts).is_empty(), "too new");
    // The newcomer is told once, privately, why nothing crossed.
    let told = replies(&acts);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("110"), "{}", told[0]);
    assert!(
        replies(&w.say(&a, "troll", "again")).is_empty(),
        "only once"
    );
    w.advance(120);
    assert_eq!(says(&w.say(&a, "troll", "ora si")).len(), 1);
}

#[test]
fn flood_protection_is_per_person_on_the_receiving_side() {
    let (mut w, a, b) = World::linked_live();
    let mut delivered = 0;
    for i in 0..10 {
        delivered += says(&w.say(&a, "alice", &format!("riga {i}"))).len();
    }
    assert_eq!(delivered, 5, "burst of 5, then dropped");
    assert!(w.e.stats(&b).dropped_rate >= 5);
    // Another person has their own allowance.
    w.ev(Event::Join {
        chan: a.clone(),
        user: user("dave"),
        role: Role::Normal,
    });
    w.advance(200);
    assert_eq!(says(&w.say(&a, "dave", "io")).len(), 1);
    // The owner can tighten it.
    w.cmd("azzurra", "opB", "SET #italia rate 1/60");
    w.advance(3600);
    let n: usize = (0..3)
        .map(|i| says(&w.say(&a, "alice", &format!("x{i}"))).len())
        .sum();
    assert_eq!(n, 1);
}

#[test]
fn never_relays_its_own_puppets_so_no_loops() {
    let (mut w, a, b) = World::linked_live();
    w.say(&a, "alice", "ciao"); // spawns "alice|cha" on azzurra
    let nick =
        w.e.puppet_nick(&b.net, 1)
            .or_else(|| w.e.puppet_nick(&b.net, 2))
            .unwrap();
    let acts = w.say(&b, &nick, "ciao"); // what azzurra's server would echo back to us
    assert!(says(&acts).is_empty());
}

// ------------------------------------------------------ owner control ---

#[test]
fn owner_can_block_a_person_on_their_own_side_only() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "BLOCK #italia alice");
    assert!(
        says(&w.say(&a, "alice", "ciao")).is_empty(),
        "alice is blocked in #italia"
    );
    // Alice can still talk to nobody else, but bob is unaffected.
    assert_eq!(says(&w.say(&b, "bob", "ciao")).len(), 1);
    w.cmd("azzurra", "opB", "UNBLOCK #italia alice");
    assert_eq!(says(&w.say(&a, "alice", "ciao")).len(), 1);
}

#[test]
fn require_account_filters_incoming_people() {
    let (mut w, a, _b) = World::linked_live();
    w.cmd("azzurra", "opB", "SET #italia require-account on");
    assert!(says(&w.say(&a, "alice", "ciao")).is_empty());
    w.ev(Event::Join {
        chan: a.clone(),
        user: user("erika").with_account("erika"),
        role: Role::Normal,
    });
    w.advance(200);
    assert_eq!(says(&w.say(&a, "erika", "ciao")).len(), 1);
}

#[test]
fn a_kick_is_a_warning_a_ban_is_forever() {
    let (mut w, a, b) = World::linked_live();
    w.say(&a, "alice", "ciao");
    let pn =
        w.e.puppet_nick(&b.net, 2)
            .or_else(|| w.e.puppet_nick(&b.net, 1))
            .unwrap();

    // Kick: kept out for 10 minutes only.
    w.ev(Event::PuppetLost {
        net: b.net.clone(),
        nick: pn.clone(),
        chan: Some("#italia".into()),
        why: LostWhy::Kicked,
    });
    w.advance(30);
    assert!(says(&w.say(&a, "alice", "di nuovo")).is_empty());
    w.advance(11 * 60);
    assert_eq!(says(&w.say(&a, "alice", "scusate")).len(), 1);

    // Ban: permanent, written into the owner's block list.
    let pn =
        w.e.puppet_nick(&b.net, 2)
            .or_else(|| w.e.puppet_nick(&b.net, 1))
            .unwrap();
    w.ev(Event::PuppetLost {
        net: b.net.clone(),
        nick: pn,
        chan: Some("#italia".into()),
        why: LostWhy::Banned,
    });
    w.advance(3600 * 24);
    assert!(says(&w.say(&a, "alice", "sono tornata")).is_empty());
    assert!(w
        .e
        .member(&b)
        .unwrap()
        .policy
        .block
        .iter()
        .any(|m| m.starts_with("*!*cha-")));
}

#[test]
fn pause_and_unlink_stop_everything_at_once() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "SET #italia presence full");
    w.advance(1);
    assert_eq!(says(&w.say(&a, "alice", "uno")).len(), 1);

    w.cmd("azzurra", "opB", "PAUSE #italia");
    assert!(says(&w.say(&a, "alice", "due")).is_empty());
    assert!(says(&w.say(&b, "bob", "tre")).is_empty());
    assert_eq!(w.e.puppet_count(&b.net), 0, "puppets leave when paused");
    w.cmd("azzurra", "opB", "RESUME #italia");
    w.advance(10);
    assert_eq!(says(&w.say(&a, "alice", "quattro")).len(), 1);

    let acts = w.cmd("azzurra", "opB", "UNLINK #italia");
    assert!(acts.iter().any(|x| matches!(x, Action::Notice { chan, text } if chan == &a && text.contains("left this link"))));
    assert!(w.e.member(&b).is_none());
    assert!(w.e.member(&a).is_none(), "a room of one is dissolved");
    assert_eq!(w.e.puppet_count(&b.net), 0);
    assert!(says(&w.say(&a, "alice", "cinque")).is_empty());
}

#[test]
fn mute_stops_hearing_one_channel_only() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "MUTE #italia chatnet #italy");
    assert!(
        says(&w.say(&a, "alice", "ciao")).is_empty(),
        "azzurra no longer hears chatnet"
    );
    assert_eq!(
        says(&w.say(&b, "bob", "ciao")).len(),
        1,
        "but chatnet still hears azzurra"
    );
}

#[test]
fn settings_are_validated_and_audited() {
    let (mut w, _a, b) = World::linked_live();
    let r = replies(&w.cmd("azzurra", "opB", "SET #italia rate 0/0"));
    assert!(r[0].contains("bad value"), "{r:?}");
    let r = replies(&w.cmd("azzurra", "opB", "SET #italia banana 1"));
    assert!(r[0].contains("unknown setting"), "{r:?}");
    w.cmd("azzurra", "opB", "SET #italia max-line 200");
    let r = replies(&w.cmd("azzurra", "opB", "AUDIT #italia 5"));
    assert!(
        r.iter()
            .any(|l| l.contains("opB") && l.contains("max-line")),
        "{r:?}"
    );
    let r = replies(&w.cmd("azzurra", "opB", "STATUS #italia"));
    assert!(
        r.iter().any(|l| l.contains("is LIVE")) && r.iter().any(|l| l.contains("linked with")),
        "{r:?}"
    );
    let _ = b;
}

// ------------------------------------------------------- robustness ---

#[test]
fn service_bot_transport_works_without_extra_connections() {
    let mut w = World::new();
    w.e.cfg.networks.get_mut(&net("azzurra")).unwrap().transport = Transport::ServiceBot;
    let a = chan("chatnet", "#italy");
    let b = chan("azzurra", "#italia");
    w.sync(&a, &[("opA", Role::Op), ("alice", Role::Normal)]);
    w.sync(&b, &[("opB", Role::Op)]);
    w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
    w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy");
    w.cmd("chatnet", "opA", "LIVE #italy");
    w.cmd("azzurra", "opB", "LIVE #italia");
    w.advance(600);
    let acts = w.say(&a, "alice", "ciao");
    assert!(
        !has_spawn(&acts),
        "no puppet connections on a service-bot network"
    );
    let s = says(&acts);
    assert_eq!(s[0].1, Via::Service);
    assert_eq!(s[0].3, "<alice|cha> ciao");
}

#[test]
fn network_down_removes_its_people_and_tells_the_neighbours() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "SET #italia presence full");
    w.cmd("chatnet", "opA", "SET #italy presence full");
    w.ev(Event::Join {
        chan: b.clone(),
        user: user("zed"),
        role: Role::Normal,
    });
    assert!(w.e.puppet_count(&a.net) >= 1);
    let acts = w.ev(Event::NetDown { net: b.net.clone() });
    assert!(acts.iter().any(|x| matches!(x, Action::Notice { chan, text } if chan == &a && text.contains("disconnected"))));
    assert_eq!(w.e.puppet_count(&a.net), 0, "azzurra's people left chatnet");
    // When it comes back and syncs, things resume.
    w.sync(&b, &[("opB", Role::Op), ("bob", Role::Normal)]);
    w.advance(600);
    assert_eq!(says(&w.say(&b, "bob", "eccomi")).len(), 1);
}

#[test]
fn service_kicked_from_a_channel_pauses_that_link_safely() {
    let (mut w, a, b) = World::linked_live();
    w.ev(Event::ServiceLeft { chan: b.clone() });
    assert!(says(&w.say(&a, "alice", "ciao")).is_empty());
    w.sync(&b, &[("opB", Role::Op), ("bob", Role::Normal)]);
    w.advance(600);
    assert_eq!(says(&w.say(&a, "alice", "ciao")).len(), 1);
}

#[test]
fn nick_changes_follow_and_collisions_never_clash() {
    let (mut w, a, b) = World::linked_live();
    // Someone on azzurra already uses the nick our puppet would want.
    w.sync(
        &b,
        &[
            ("opB", Role::Op),
            ("bob", Role::Normal),
            ("alice|cha", Role::Normal),
        ],
    );
    let acts = w.cmd("azzurra", "opB", "SET #italia presence full");
    let face = acts.iter().find_map(|x| match x {
        Action::SpawnPuppet { face, net, .. }
            if net == &b.net && face.nick.starts_with("alice") =>
        {
            Some(face.nick.clone())
        }
        _ => None,
    });
    let face = face.expect("alice gets a puppet on azzurra");
    assert_ne!(
        face, "alice|cha",
        "must not take a nick a real user already has"
    );
    assert!(face.ends_with("|cha"));
    let acts = w.ev(Event::NickChange {
        net: a.net.clone(),
        old: "alice".into(),
        new: "alicia".into(),
    });
    assert!(acts
        .iter()
        .any(|x| matches!(x, Action::PuppetNick { nick, .. } if nick.starts_with("alicia"))));
}

#[test]
fn bots_are_not_relayed_unless_enabled() {
    let (mut w, a, _b) = World::linked_live();
    let mut bot = user("robby");
    bot.is_bot = true;
    w.ev(Event::Join {
        chan: a.clone(),
        user: bot,
        role: Role::Normal,
    });
    w.advance(300);
    assert!(says(&w.say(&a, "robby", "beep")).is_empty());
}

#[test]
fn puppet_cap_makes_room_by_dropping_the_quietest() {
    let (mut w, a, b) = World::linked_live();
    w.e.cfg.networks.get_mut(&b.net).unwrap().max_puppets = 2;
    for n in ["u1", "u2", "u3"] {
        w.ev(Event::Join {
            chan: a.clone(),
            user: user(n),
            role: Role::Normal,
        });
    }
    w.advance(300);
    for n in ["u1", "u2", "u3"] {
        w.advance(1);
        w.say(&a, n, "ciao");
    }
    assert_eq!(w.e.puppet_count(&b.net), 2);
}

#[test]
fn state_survives_a_restart() {
    let (mut w, a, b) = World::linked_live();
    w.cmd("azzurra", "opB", "BLOCK #italia *!*@spam.example");
    w.cmd("azzurra", "opB", "SET #italia rate 2/30");
    let snap = w.e.snapshot();
    let json = serde_json::to_string(&snap).unwrap();
    let snap2: Snapshot = serde_json::from_str(&json).unwrap();
    let e2 = Engine::restore(w.e.cfg.clone(), snap2);
    let m = e2.member(&b).unwrap();
    assert_eq!(m.policy.rate.burst, 2);
    assert!(m.policy.block.contains(&"*!*@spam.example".to_string()));
    assert!(e2.member(&a).is_some());
    assert_eq!(e2.wanted_channels().len(), 2);
}

#[test]
fn ident_is_the_same_for_the_same_account_across_nick_changes() {
    let mut w = World::new();
    let a = chan("chatnet", "#italy");
    w.sync(&a, &[("opA", Role::Op)]);
    let u1 = user("mario").with_account("mario");
    let i1 = itl_core::face::ident_for(&net("chatnet"), &u1);
    let u2 = user("mario_away").with_account("mario");
    assert_eq!(i1, itl_core::face::ident_for(&net("chatnet"), &u2));
}

// -------------------------------------------------------------- language ---

#[test]
fn replies_follow_the_channel_language() {
    let mut w = World::new();
    w.e.cfg.default_lang = itl_core::policy::Lang::It;
    let a = chan("chatnet", "#italy");
    let b = chan("azzurra", "#italia");
    w.sync(&a, &[("opA", Role::Op)]);
    w.sync(&b, &[("opB", Role::Op)]);
    // Not linked yet: the default language applies.
    let r = replies(&w.cmd("chatnet", "opA", "STATUS #italy"));
    assert!(r[0].contains("non è collegato"), "{r:?}");
    w.cmd("chatnet", "opA", "LINK #italy azzurra #italia");
    let acts = w.cmd("azzurra", "opB", "ACCEPT #italia chatnet #italy");
    assert!(
        acts.iter()
            .any(|x| matches!(x, Action::Notice { text, .. } if text.contains("ora è collegato"))),
        "{acts:?}"
    );
    // Errors are Italian too.
    let r = replies(&w.cmd("chatnet", "nobody", "STATUS #italy"));
    assert!(r[0].contains("Non ti vedo"), "{r:?}");
    let r = replies(&w.cmd("chatnet", "opA", "HELP"));
    assert!(r.iter().any(|l| l.contains("chiedi un link")), "{r:?}");
    // One channel can pick English without affecting the other.
    let r = replies(&w.cmd("azzurra", "opB", "SET #italia lang en"));
    assert!(r[0].contains("lang = en"), "{r:?}");
    let r = replies(&w.cmd("azzurra", "opB", "LIVE #italia"));
    assert!(r[0].contains("is now LIVE"), "{r:?}");
    let r = replies(&w.cmd("chatnet", "opA", "LIVE #italy"));
    assert!(r[0].contains("ora è LIVE"), "{r:?}");
    let r = replies(&w.cmd("chatnet", "opA", "SET #italy lang fr"));
    assert!(r[0].contains("use it or en"), "{r:?}");
    let r = replies(&w.cmd("chatnet", "opA", "STATUS #italy"));
    assert!(
        r.iter()
            .any(|l| l.contains("collegato con") && l.contains("live")),
        "{r:?}"
    );
    assert!(r.iter().any(|l| l.contains("righe in entrata")), "{r:?}");
}

#[test]
fn every_message_exists_in_both_languages() {
    use itl_core::msg::Msg;
    use itl_core::policy::Lang;
    let all = [
        Msg::NotInChannel {
            chan: "#c",
            svc: "S",
        },
        Msg::NotSeen { chan: "#c" },
        Msg::NotOp { chan: "#c" },
        Msg::NeedAccount,
        Msg::NotLinkedYet { chan: "#c" },
        Msg::SameNetwork,
        Msg::TheirServiceMissing {
            chan: "#c",
            net: "n",
            svc: "S",
        },
        Msg::AlreadyLinked,
        Msg::BothInLinks,
        Msg::OnePerNet { net: "n" },
        Msg::AskNotice {
            mine_chan: "#a",
            mine_net: "n",
            other_chan: "#b",
            svc: "S",
        },
        Msg::RequestSent { other: "#b" },
        Msg::NoPending {
            mine: "#a",
            net: "n",
            chan: "#b",
        },
        Msg::Accepted,
        Msg::NoSuchRequest,
        Msg::RequestRemoved,
        Msg::Admitted {
            svc: "S",
            chan: "#a",
        },
        Msg::AdmittedOther {
            chan: "#a",
            label: "L",
        },
        Msg::LeftNotice {
            chan: "#a",
            net: "n",
        },
        Msg::Unlinked { chan: "#a" },
        Msg::NotLinked { chan: "#a" },
        Msg::Paused { chan: "#a" },
        Msg::Resumed { chan: "#a" },
        Msg::ModeLive { chan: "#a" },
        Msg::ModePreview { chan: "#a" },
        Msg::ModeOff { chan: "#a" },
        Msg::SetOk {
            chan: "#a",
            key: "k",
            value: "v",
        },
        Msg::Blocked {
            chan: "#a",
            mask: "m",
        },
        Msg::Unblocked {
            chan: "#a",
            mask: "m",
        },
        Msg::NotBlocked { mask: "m" },
        Msg::BlockFull,
        Msg::BadMask,
        Msg::Muted {
            chan: "#a",
            other: "o",
        },
        Msg::Unmuted {
            chan: "#a",
            other: "o",
        },
        Msg::SampleNone,
        Msg::SampleHeader { n: 3, chan: "#a" },
        Msg::AuditNone,
        Msg::NetDown { label: "L" },
    ];
    for m in &all {
        let (en, it) = (m.render(Lang::En), m.render(Lang::It));
        assert!(!en.is_empty() && !it.is_empty());
        // Translated messages that carry names must still carry them.
        if !matches!(m, Msg::Accepted | Msg::SetOk { .. }) {
            assert_ne!(en, it, "message not translated: {en}");
        }
    }
}
