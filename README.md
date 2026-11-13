# Staffetta

**Link your IRC channel with channels on other networks - and stay the boss of your own channel.**

Small IRC channels are quiet. The people are out there, but they are spread over
many networks, each one with a handful of chatters. Staffetta lets channels on
different networks share one conversation, **without any channel giving up
control**.

> Status: **working prototype** (pre-release). The core and the network layer are
> tested end to end against a built-in fake IRC network, and it has been tried on
> **two real IRC networks** in a private test channel (relay both ways, kick, ban,
> unlink, restart recovery). It has not yet been used by public channels. See
> [Status and roadmap](#status-and-roadmap).

## What it looks like

In `#italy` on network A, a person called `alice` talks. In `#italia` on network B
the channel members see:

```
-!- alice|cha [cha-3fa91@host] has joined #italia
<alice|cha> ciao dall'Italia!
```

`alice|cha` is a **real IRC user** on network B, not a bot speaking for her. She is
in the user list, she can be voiced, kicked, banned and ignored with the tools the
operators of `#italia` already use. `cha` says where she comes from, and her
realname says plainly that she is a relayed user.

## Why owners can say yes

Most bridges ask an owner to trust somebody else's software with their channel.
Staffetta is built the other way round:

* **Nothing happens without both owners.** A link needs the operators of *both*
  channels to agree. A third channel needs everybody's yes.
* **Every link starts in PREVIEW.** Nothing is sent or shown. You can run
  `SAMPLE` and read what your channel *would* have received before you decide.
* **Each side switches LIVE by itself.** Chat only flows while *both* sides are live.
* **Your rules, applied on your side.** Who may speak, flood limits, minimum time in
  channel, only-logged-in-users, blocked people, muted channels. Change them any time.
* **Bans are ordinary bans.** Ban the puppet's mask (`*!*cha-3fa91@*`) with your
  normal IRC client. It sticks to the person behind it, on your channel only.
  A kick keeps them out for ten minutes, a ban forever.
* **One command stops everything**, instantly: `PAUSE`, `UNLINK`. Leaving costs nothing.
* **An audit log** records who changed what.
* **No surprises:** only channel operators can command the service, and it only
  enters a channel it was invited into.

See [docs/OWNERS.md](docs/OWNERS.md) for the owner's guide.

## How it works

For every person who talks in a linked channel, Staffetta opens a normal IRC
connection to each of the *other* networks and speaks through it. On networks that
do not allow extra connections, a `service-bot` mode relays with one clearly
labelled bot instead (`<alice|cha> ciao`). The channel owner can see and control
everything in both modes.

The core is a deterministic state machine with no network code, covered by
scenario tests. See [docs/DESIGN.md](docs/DESIGN.md).

## Quick start (try it locally)

```sh
cargo test                       # runs everything against a built-in fake network
cp config.example.toml config.toml
staffetta check config.toml
staffetta config.toml
```

Plain-text IRC is accepted **only towards localhost** (that is what the tests use).
Real networks need `tls = true`, and the certificate is always verified.

## Status and roadmap

Working and tested (fake network): invitation and two-sided approval, preview/live,
per-channel policy, flood control, blocks and mutes, kick/ban memory, pause/unlink,
state and audit log on disk, reconnect after a network drop, restart recovery,
puppet nick collisions, puppet cap per network, account-gated entry.

Replies and notices to channel operators are available in Italian and English
(per channel: `SET #chan lang it|en`, default set in the configuration).

Also done: services-account tracking through WHOX and `account-notify` (a person's
identity is anchored to their account), an IPv6 source-address pool for puppets
(against per-IP connection limits), an opt-in read-only status page, a hardened
systemd unit, CI. See [docs/INSTALL.md](docs/INSTALL.md).

Tried on real networks so far: one private test channel on two networks. What
the trial taught us is already in the code: puppet idents are kept to 9
characters so servers do not cut them, and banning the shared *host* removes every
relayed person at once, so owners block by ident or with `BLOCK` (see
[docs/OWNERS.md](docs/OWNERS.md)).

Not done yet: use by real, public channels. That needs only the agreement of the
channel owners involved; no network staff or server change is required, by design.

## License

MIT. See [LICENSE](LICENSE).
