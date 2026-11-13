//! One puppet = one real IRC connection wearing a remote person's face.

use crate::conn::{self, Target};
use crate::network::Inbound;
use itl_core::engine::{Event, LostWhy};
use itl_core::face::Face;
use itl_core::model::{fold, NetId, SayKind, Uid};
use itl_proto::parse;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug)]
pub enum PuppetCmd {
    Join(String),
    Part(String, String),
    Say {
        chan: String,
        kind: SayKind,
        text: String,
    },
    Nick(String),
    Quit(String),
}

fn lost(
    events: &UnboundedSender<Inbound>,
    net: &NetId,
    nick: &str,
    chan: Option<String>,
    why: LostWhy,
) {
    let _ = events.send(Inbound::Core(Event::PuppetLost {
        net: net.clone(),
        nick: nick.to_string(),
        chan,
        why,
    }));
}

pub async fn run_puppet(
    target: Target,
    net: NetId,
    _uid: Uid,
    face: Face,
    mut rx: UnboundedReceiver<PuppetCmd>,
    events: UnboundedSender<Inbound>,
) {
    let mut cur_nick = face.nick.clone();
    let (mut rd, mut wr) = match conn::connect(&target).await {
        Ok(x) => x,
        Err(e) => {
            let _ = events.send(Inbound::Log(format!(
                "[{net}] puppet {cur_nick} cannot connect: {e:#}"
            )));
            lost(&events, &net, &cur_nick, None, LostWhy::Killed);
            return;
        }
    };
    if wr.send(&format!("NICK {}", face.nick)).await.is_err()
        || wr
            .send(&format!("USER {} 0 * :{}", face.ident, face.realname))
            .await
            .is_err()
    {
        lost(&events, &net, &cur_nick, None, LostWhy::Killed);
        return;
    }

    let mut registered = false;
    let mut queue: VecDeque<PuppetCmd> = VecDeque::new();
    let mut quit_requested = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    'main: loop {
        tokio::select! {
            line = rd.next() => {
                let Ok(Some(line)) = line else { break 'main };
                let Some(m) = parse(&line) else { continue };
                match m.command.as_str() {
                    "PING" => { let _ = wr.send(&format!("PONG :{}", m.params.first().cloned().unwrap_or_default())).await; }
                    "001" => {
                        registered = true;
                        if let Some(n) = m.params.first() { cur_nick = n.clone(); }
                        while let Some(c) = queue.pop_front() {
                            if apply(&mut wr, &mut cur_nick, &mut quit_requested, c).await.is_err() { break 'main; }
                        }
                    }
                    "433" | "432" | "ERROR" if !registered => break 'main,
                    "ERROR" => break 'main,
                    "KICK" => {
                        let chan = m.params.first().cloned().unwrap_or_default();
                        let victim = m.params.get(1).cloned().unwrap_or_default();
                        if fold(&victim) == fold(&cur_nick) {
                            lost(&events, &net, &cur_nick, Some(chan), LostWhy::Kicked);
                        }
                    }
                    "474" => lost(&events, &net, &cur_nick, m.params.get(1).cloned(), LostWhy::Banned),
                    "471" | "473" | "475" | "403" | "405" | "477" => {
                        lost(&events, &net, &cur_nick, m.params.get(1).cloned(), LostWhy::Kicked)
                    }
                    _ => {}
                }
            }
            cmd = rx.recv() => {
                match cmd {
                    None => { let _ = wr.send("QUIT :Ita Link").await; quit_requested = true; break 'main; }
                    Some(c) => {
                        if registered {
                            if apply(&mut wr, &mut cur_nick, &mut quit_requested, c).await.is_err() { break 'main; }
                            if quit_requested { break 'main; }
                        } else {
                            queue.push_back(c);
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline), if !registered => {
                let _ = events.send(Inbound::Log(format!("[{net}] puppet {cur_nick}: registration timed out")));
                break 'main;
            }
        }
    }
    if !quit_requested {
        lost(&events, &net, &cur_nick, None, LostWhy::Killed);
    }
}

async fn apply(
    wr: &mut conn::LineWriter,
    cur_nick: &mut String,
    quit: &mut bool,
    c: PuppetCmd,
) -> anyhow::Result<()> {
    match c {
        PuppetCmd::Join(ch) => wr.send(&format!("JOIN {ch}")).await,
        PuppetCmd::Part(ch, why) => wr.send(&format!("PART {ch} :{why}")).await,
        PuppetCmd::Say { chan, kind, text } => match kind {
            SayKind::Action => {
                wr.send(&format!("PRIVMSG {chan} :\x01ACTION {text}\x01"))
                    .await
            }
            _ => wr.send(&format!("PRIVMSG {chan} :{text}")).await,
        },
        PuppetCmd::Nick(n) => {
            *cur_nick = n.clone();
            wr.send(&format!("NICK {n}")).await
        }
        PuppetCmd::Quit(why) => {
            *quit = true;
            wr.send(&format!("QUIT :{why}")).await
        }
    }
}
