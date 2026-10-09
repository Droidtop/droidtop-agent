# droidtop-agent

A small program for your computer (Windows, Linux or macOS) that keeps it in
step with [droidtop](https://github.com/Droidtop/droidtop) on a handheld:

- **Saves.** Before a game starts on the handheld and after it ends, its saves
  are brought up to date with the computer's copy. If both sides changed,
  droidtop asks which to keep, and the other copy is archived, not deleted.
- **Library.** The agent scans the computer for games: Steam, GOG, Epic,
  Amazon, itch, Battle.net, Heroic, Lutris, your own game folders, and ROM
  folders laid out by ES-DE system name. The two libraries stay in step both
  ways.
- **Plugin contexts.** Supporting data that droidtop's plugins share with
  programs on the computer, synced both ways. A plugin that needs one
  publishes a small context adapter program for the computer. When the
  plugin asks for it, the agent says so; `droidtop-agent contexts approve
  <context>` installs it, checked against the digest the plugin names.
  `droidtop-agent contexts add <program>` adds one by hand.

It does not stream your screen or control the computer remotely: that is
[windowcast](https://github.com/Droidtop/windowcast)'s job. It never needs
Tailscale or any other VPN. On your own network the two devices talk
directly. Away from it, they can use a direct WireGuard tunnel, or a folder
that your own sync tool already carries to both devices.

The design, and what droidtop already had before it, is in
[docs/DESIGN.md](docs/DESIGN.md).

## Use

```
droidtop-agent pair 123456      # the code droidtop shows under Settings > Computers > Pair a computer
droidtop-agent pair             # or: this computer shows its address and a code, typed on the handheld
droidtop-agent                  # keep it running; the handheld connects when it needs to
droidtop-agent scan             # what it found on this computer
droidtop-agent saves steam:440  # where a game's saves are here, and the files
droidtop-agent --help
```

The first time the agent runs, Windows may ask whether it may use the
network. It listens on port 47610 (TCP and UDP) on your local network, and
on UDP 47611 for WireGuard from outside it.

Away from home, the handheld reaches the computer through WireGuard when
the computer's UDP 47611 is reachable: forward it on your router and run
`droidtop-agent endpoint set <public ip>:47611`, or let a global IPv6
address through your firewall. The handheld learns these each time the two
meet. When neither works, `droidtop-agent share set <folder>` names a folder
your own sync tool carries to the handheld (pick the same folder under
droidtop's Settings > Computers > Cloud folder).

Save locations come from the [Ludusavi
manifest](https://github.com/mtkennerly/ludusavi-manifest) (data from
PCGamingWiki). The agent downloads it when it first needs it, or when you run
`droidtop-agent manifest update`. Your own Ludusavi custom games are used too,
and `droidtop-agent saves add` covers anything else.

## Build

```
cargo build --release -p droidtop-agent
```

CI builds the agent for all three systems, and droidtop's Android library
(`libdroidtop_agent.so`, arm64-v8a and x86_64), on every push. Each green
build on `main` is published as a release.

## Licence

GPL-3.0-only. The agent uses windowcast's `identity` and `pairing` crates,
which are GPL-3.0-only.
