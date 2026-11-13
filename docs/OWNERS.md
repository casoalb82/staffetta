# Owner's guide

You run an IRC channel. This page tells you exactly what Ita Link can and cannot do
to it. Everything below is something *you* decide.

## 1. Let the service in

The service never joins a channel by itself. A channel operator invites it:

```
/invite ItaLink #yourchannel
```

It only sits there and listens. Nothing is shared yet.

## 2. Ask for a link

Both channels must have invited the service. Then, as an operator of your channel:

```
/msg ItaLink LINK #yourchannel othernet #theirchannel
```

Their operators get a notice with the exact command to accept. Until they do,
nothing happens. When they accept, **both channels start in PREVIEW**.

## 3. Look before you leap

```
/msg ItaLink STATUS #yourchannel
/msg ItaLink SAMPLE #yourchannel
```

`SAMPLE` shows the last lines your channel *would* have received if it were live.
Nothing was sent anywhere. When you like what you see:

```
/msg ItaLink LIVE #yourchannel
```

Chat flows only while **both** sides are LIVE. You can step back at any time:
`PREVIEW`, `OFF`, `PAUSE`, `RESUME`, `UNLINK`. They take effect at once.

## Commands

| Command | What it does |
|---|---|
| `LINK <#you> <net> <#them>` | Ask to link (they must `ACCEPT`). |
| `ACCEPT` / `DENY <#you> <net> <#them>` | Agree to, or refuse, a request. |
| `PREVIEW` / `LIVE` / `OFF <#you>` | Set the mode. |
| `PAUSE` / `RESUME <#you>` | Stop and restart everything, keeping the link. |
| `UNLINK <#you>` | Leave. Your people disappear from the others at once. |
| `SET <#you> <setting> <value>` | Change a rule (below). |
| `BLOCK` / `UNBLOCK <#you> <mask>` | Never relay, nor show, matching people. |
| `MUTE` / `UNMUTE <#you> <net> <#them>` | Stop hearing one linked channel. |
| `STATUS`, `SAMPLE`, `AUDIT <#you>` | See what is going on and who changed what. |
| `HELP`, `HELP SET` | Built-in help. |

Only channel operators (`@`) can use them. If you want to, require services login
too (`require_account_for_owner = true` in the configuration).

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `share-out` | on | Send your channel's chat to the linked channels. |
| `accept-in` | on | Show the linked channels' chat in yours. |
| `presence` | lazy 30 min | `full`: everyone is in your user list. `lazy [minutes]`: a person appears when they speak and leaves after being quiet. |
| `require-account` | off | Only show people who are logged in to services. |
| `min-presence` | 120 s | A person must have been in *their* channel this long before their lines cross. Stops hit-and-run trolling. |
| `rate` | 5/10 | Lines per person per seconds. |
| `max-line` | 350 | Longest relayed line. |
| `strip-formatting` | on | Remove colours, bold and other control codes. |
| `lang` | `it` | Language of the service's replies for your channel: `it` or `en`. |

Lines starting with `!` or `.` are bot commands and are never relayed.

## Blocking and banning

Ban a remote person as you would anyone, using the mask your IRC client shows for
the puppet:

```
/mode #yourchannel +b *!*cha-3fa91c@*
```

The part after `cha-` is derived from who the person is on their own network (their
services account when they have one), so the same person always gets the same
ident, and one ban never catches anybody else. When a puppet is **banned**, Ita Link
remembers it as a permanent block. A **kick** keeps the person out for ten minutes.

You can also block by the person's real identity on the origin network:

```
/msg ItaLink BLOCK #yourchannel *!*@*.badisp.example
/msg ItaLink BLOCK #yourchannel account:somebody
```

## Common questions

**What do the other channels see of my channel?**
The lines your people say, under their nick plus a short network tag, and the fact
that they are present when your `presence` setting is `full`. Nothing else: no
topic, no mode changes, no join/part noise.

**Does the service log my chat?**
No. It keeps the links, your rules and an audit log of *commands*, nothing of the
conversation.

**What if I change my mind?**
`UNLINK #yourchannel`. Your people vanish from the other channels immediately and
nothing of yours is shared any more. The other channels are told you left.

**Do the people from the other network count in my channel's limits?**
They are real connections, so yes: they appear in your user list and in `/names`.
That is the point, and it is also why `presence lazy` exists.

**Can anybody abuse this to flood or spam my channel?**
Each person has a per-person flood limit that you choose, newcomers cannot speak
across for a while, and one `BLOCK`, `MUTE` or `PAUSE` stops it.
