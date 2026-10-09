# Changelog

Every green build of `main` is a release named `build-<run number>`; this file
records what each change brought. The newest entry is the one the next
release carries until its number is written in.

## Unreleased

- Rendezvous away from home, the way Syncthing does it: STUN from the
  WireGuard socket, Syncthing's global discovery protocol to announce and
  look up addresses (a discovery ID per device, from a certificate made from
  its key), and UDP hole punching between paired devices. Syncthing's
  servers by default, at Syncthing's own client's pace; addresses only,
  never its relays. `droidtop-agent rendezvous` shows and sets it.
- droidtop learns which way a sync went (`path`: lan, wireguard,
  rendezvous) and the computer's discovery ID from each session.

## build-12

- Pairing the other way round: `droidtop-agent pair` with no code shows this
  computer's addresses and a code, and the handheld connects (for a
  handheld the computer cannot reach, such as one behind an emulator's
  NAT). It listens on TCP 47612 while it waits.
- A plugin can offer its context adapter: droidtop sends the program's
  address and SHA-256 from the plugin's signed manifest, the computer keeps
  the offer, and `droidtop-agent contexts approve <context>` fetches,
  checks and installs it. Later versions from the same plugin follow
  without asking again.
- Off-LAN rendezvous through Syncthing's global discovery and public STUN
  was not built: neither publishes terms that let another program use it
  (docs/DESIGN.md section 10).

## build-10

- Plugin contexts come from context adapters: separate programs a plugin
  publishes, added with `droidtop-agent contexts add <program>`, speaking
  JSON over standard input and output (docs/DESIGN.md section 8). The
  F95Checker adapter left the agent; it is now the F95 plugin's
  `droidtop-agent-f95-adapter` in gamegrab-sources.
- droidtop's library carries the person's marks: only a mark that differs
  from the shared one is sent, and the marks that arrived from elsewhere are
  named for droidtop to write into its own library.
- droidtop's side of the cloud share: library letters both ways, and a
  game's saves left for the computer when it is away, made against the set
  it last had. The agent applies a set that already matches without
  archiving anything.
- The handheld keeps where a game's saves are from each live sync, so it can
  leave them in the share later.
- WireGuard from a network without IPv6 tries the computer's IPv4 endpoints
  instead of failing.

## build-7

- Direct WireGuard between paired devices: boringtun and smoltcp in
  userspace.
  - The computer listens on UDP 47611 and states its endpoints (a forwarded
    port, global IPv6 addresses) in `hello`.
  - The handheld tries them all when the LAN does not answer.

## build-5

- First version.
  - Pairing with a handheld: windowcast's SPAKE2 run, with a 6-digit code
    shown on the handheld.
  - The session channel: Noise_IK between the two paired keys.
  - Save sync, using the same rule as droidtop's Steam Cloud sync. Conflicts
    are the person's to settle, and the loser is archived.
  - The computer's scanner for Steam, Epic, GOG, Amazon, itch, Battle.net,
    Heroic, Lutris, game folders and ROM folders, plus other programs' state.
  - Two-way library changes, last writer wins per mark.
  - Plugin context sync, with F95Checker's database as the first context.
  - Store and forward through a cloud folder of the person's own.
  - droidtop's Android library behind one JNI call.
