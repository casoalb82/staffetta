# Installing Staffetta

Staffetta is a single static-ish binary plus a configuration file. It needs no
database.

## Before you connect to anybody

Staffetta opens **real IRC connections** (the service, and one per remote person
in `puppets` mode). Read the bot and connection policy of every network you
configure and **ask its staff first**. Many networks want a service nick
reserved, a connection class or an exemption for the connection limit. Nothing in
this project works around a network's rules, and a link should only ever exist
between channels whose owners asked for it.

## Build

```sh
cargo build --release
install -m 0755 target/release/staffetta /usr/local/bin/staffetta
```

## Configure

```sh
cp config.example.toml /etc/staffetta/config.toml
staffetta check /etc/staffetta/config.toml
```

Every network needs `tls = true` (plain text is refused except to localhost) and
its certificate is always verified. `state_dir` holds `state.json` (links, rules,
channels the service was invited to) and `audit.jsonl` (who ran which command).
Back that directory up; it contains no chat.

## Run under systemd

See the comments at the top of `contrib/staffetta.service`. The unit runs as an
unprivileged user with no capabilities, a read-only system and a single writable
state directory.

```sh
sudo systemctl enable --now staffetta
journalctl -u staffetta -f
```

Stopping the service (`systemctl stop`) saves the state first.

## Per-IP connection limits: the IPv6 pool

If a network limits connections per address, give puppets their own addresses
from a routed IPv6 prefix (a `/64` from your hosting provider):

```toml
[[network]]
id = "examplea"
# ...
puppet_ipv6_pool = "2001:db8:1:2::/64"
```

Each person always connects from the same address inside the prefix. Linux must
accept binding to addresses of the prefix that are not configured on an interface:

```sh
sudo ip -6 route add local 2001:db8:1:2::/64 dev lo
sudo sysctl -w net.ipv6.ip_nonlocal_bind=1
```

(make both persistent with your distribution's network configuration). The target
IRC server must be reachable over IPv6. The unit-tested part is the address
derivation; the OS setup above has to be checked on your own host before you rely
on it.

## The public status page

```toml
status_listen = "127.0.0.1:8088"
```

It is read-only and shows only channels whose owners ran `SET #channel public on`:
channel, network and mode, plus aggregate numbers. Never people, never chat. Put a
reverse proxy in front for the internet, for example with nginx:

```nginx
location /links/ {
    proxy_pass http://127.0.0.1:8088/;
}
```

`/status.json` returns the same data for machines.

## Upgrading

Stop, replace the binary, start. `state.json` is versioned by its own fields and
unknown fields are ignored, so older state files keep working.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `plain-text IRC is only allowed to localhost` | Set `tls = true` for that network. |
| The service never joins a channel | An operator must `/invite` it first; check `journalctl` for `invited into`. |
| Puppets are killed or refused | The network's connection limit: use `puppet_ipv6_pool`, an exemption, or `transport = "service-bot"`. |
| Nobody sees anything | Both channels must be `LIVE`; ask an operator to run `STATUS`. |
