# Design

## Goal

Let channels on different IRC networks share one conversation, where every
channel owner keeps full control of their own channel, and without needing the
network's staff to link servers.

## Prior art and what we learned

PyLink Relay (archived 2023) is the closest existing project: it creates a
"puppet" for every remote user and mirrors the user lists, over a server link.
Its author's own retrospective and the code show what to avoid:

| Problem in PyLink | What we do instead |
|---|---|
| Threads and global dictionaries guarded by per-network locks; the code itself admits races. | One async runtime. All relay state lives in one deterministic engine that is fed one event at a time. |
| Invented "virtual UIDs" for users the network gave no ID to, which leaked into modes and kicks and caused repeated bugs. | Stable **internal ids**. Names on the wire (nick, ident) are only a *face* attached to an id, built in one place. |
| Modes and nick rules hard-coded all over the plugin. | Every owner rule is a typed `Policy` with validated settings. |
| A `/net` suffix in the nick made ordinary ban masks impossible. | The puppet's **ident** carries the origin and is derived from the person's identity, so `*!*cha-3fa91@*` bans exactly that person. |
| No tests for the relay at all (the suite only covers parsers and protocols). | The engine has scenario tests; the whole stack has end-to-end tests against a fake network. |
| Wanted for years, never done: ban sync, flood protection, ignore. | Built in from the start, per channel. |
| Control by network (a link ACL) and by service commands, no owner-facing model. | Control per channel, by that channel's operators, with preview and an audit log. |

## Architecture

```
 network A ──┐                      ┌── network A  (service + puppets)
 network B ──┼─► itl-net ─ events ─► itl-core::Engine ─ actions ─► itl-net ─┼── network B
 network C ──┘   (sockets)          (no I/O, no clock)           (sockets)  └── network C
```

* `itl-proto` - parse and format IRC lines (with IRCv3 tags).
* `itl-core` - the engine. `Engine::handle(now_ms, Event) -> Vec<Action>`.
  No sockets, no clock, no randomness: every behaviour is a plain unit test.
* `itl-net` - one task per network. It keeps the service connection (learns who is
  in the channels, hears commands) and one connection per puppet. It turns lines
  into events and actions into lines.
* `ita-link` - configuration, on-disk state (atomic writes), audit log, glue.
* `itl-testnet` - a small fake IRC network on loopback used by the tests.

### Identity of a remote person (the "face")

* **nick**: `nick|tag` (the style is configurable), cut to the network's nick
  length without ever losing the tag; collisions add a counter.
* **ident**: `<3-letter network>-<5 hex>`, hashed from the services account when
  known, else from the nick. Stable, ban-friendly, at most 10 characters.
* **realname**: says it is a relayed user and from where.

### Consent model

A *room* is a set of channels, at most one per network. Adding a channel needs
the approval of the new channel **and** of every channel already in the room.
Each channel has its own mode (`OFF`, `PREVIEW`, `LIVE`) and may be paused.
Traffic from A to B flows only if both are live and not paused, A shares out, B
accepts in, B has not muted A, and the service is present in both.

### Per-person pipeline for a line

source filters (owner of the *sending* channel): sharing on, not blocked, not a
bot command, been present long enough, not empty, formatting stripped ->
receiver filters (owner of the *receiving* channel): visible to them
(blocks, account requirement, kick cool-down) -> flood bucket -> truncate -> send.

### Loop prevention

The engine never relays anything said by the service nick or by one of its own
puppets (it knows their nicks). A message echoed back by a network cannot start a
loop.

### Failure behaviour

* A network goes down: its people leave the other channels at once, the neighbours
  are told, the service reconnects with back-off and relearns the channels.
* The service is kicked from a channel: that link pauses itself.
* Restart: links, rules and invited channels come back from `state.json`.

## Security notes

* Plain-text IRC is refused except to loopback; TLS certificates are always verified.
* Relayed text has CR/LF/NUL removed before it is written, so a message can never
  carry a second IRC command.
* Only channel operators can command the service; every change is audited.
* Puppets show only what the person already shows in their own channel.

## Known limits

* Puppets are real connections: networks that limit connections per IP need an
  exemption (or use `service-bot` mode, which needs nothing).
* Without services-account information the identity falls back to the nick, which
  is weaker. Account tracking via WHOX is on the roadmap.
* Per-network nick rules differ; unusual networks may need a tuned `max_nick`.
