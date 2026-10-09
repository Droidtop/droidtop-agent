# Changelog

Every green build of `main` is a release named `build-<run number>`; this file
records what each change brought. The newest entry is the one the next
release carries until its number is written in.

## Unreleased

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
