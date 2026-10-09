# Changelog

Every green build of `main` is a release named `build-<run number>`; this file
records what each change brought.

## Unreleased

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
  - Direct WireGuard between paired devices: boringtun and smoltcp in
    userspace.
    - The computer listens on UDP 47611 and states its endpoints (a
      forwarded port, global IPv6 addresses) in `hello`.
    - The handheld tries them all when the LAN does not answer.
