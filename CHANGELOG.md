# Changelog

Every green build of `main` is a release named `build-<run number>`; this file
records what each change brought. The newest entry is the one the next
release carries until its number is written in.

## Unreleased

- A game's version can be copied between a handheld and a computer
  (`core::gamecopy`, `game_pull` and `game_push`; Droidtop/tracker#469 part
  3). It arrives as a new folder beside the game's others, resumable and
  checked per file. On the computer it goes to the person's first game
  folder, never into a store's install folders.

## build-28

- Every game keeps its version history across devices (device, version,
  when first heard of; `GameRecord::versions`), filled from the install
  changes each device applies, so it travels across many devices
  (Droidtop/tracker#469 part 2). The mesh test checks that a handheld three
  hops away knows both computers' versions.

## build-27

- Lutris's favourites and hidden games ("favorite" and ".hidden"
  categories) travel as the person's marks. Only a change made in Lutris
  since the last scan counts, so a mark set on a handheld is not undone, and
  the agent never writes Lutris's database. The organization droidtop
  carries in marks grows to rating, its own title, sort title, kid game and
  collections (Droidtop/tracker#469 part 1); the core needed no change for
  that.

## build-26

- The scanner walks game folders with droidtop's rules and its engines
  database (`crates/agent/data/engines-database.json`): engine games of every
  engine the database knows, PC games whose programs sit in sub-folders,
  collections, version and payload folders, part folders. Steam libraries
  and ROM system folders inside a game folder are read too, and a Flashpoint
  install is its launcher and its downloaded games. A `Games` folder at the
  top of each fixed drive (`~/Games` elsewhere) is walked without being
  added. Titles drop the version a download adds. GOG DLC no longer appear as
  games of their own.

## build-21

- One identity and one pairing with windowcast: the agent keeps its identity
  and trusted handhelds in windowcast's host folder, and pairs through
  windowcast's exchange (`windowcast_pairing::exchange`, label
  `droidtop-agent pair v2`). Version-1 pairing peers are told to update.
  Existing pairings move once:
  - the agent's key becomes the shared one when windowcast has none;
  - otherwise the agent answers to both keys and tells each handheld of the
    move, signed by both (`moved_to`), until all have moved.
- Saves: the newest copy wins when both sides changed, with no question. A
  preferred side beats newer: the handheld's primary computer, or the
  computer's primary handheld (`droidtop-agent primary`). Each device keeps
  its own overwritten changed set as its copy (`archive/<game>/copies/<device>/`),
  and the 5 most recent older copies. Save letters follow the same rule.
- Any number of handhelds and computers pair with each other. A CI test runs
  three of each in a partial mesh.
- droidtop-agent-app: the agent with a window and a tray icon on Windows,
  Linux and macOS. Its pages are status, pairing (it shows or takes a code,
  with a QR code), the library it found, kept saves, plugin data (install
  or decline offered adapters) and settings, so nobody needs the command
  line. It is egui with tray-icon, or a StatusNotifierItem on Linux. The
  agent is now a library both programs share.
- Releases carry Linux aarch64 and macOS x86_64 builds too, and a
  `droidtop-agent.app` for macOS.
- Linux and macOS hosts:
  - the Steam Snap is found;
  - Lutris Wine games, and Heroic's GOG, Epic and Amazon games, now carry
    their Wine or Proton prefix, so their Windows saves sync;
  - Bottles' library and Minigalaxy's games are scanned.
- `droidtop-agent autostart on|off`: start at sign-in through a systemd user
  unit (or an XDG autostart entry) on Linux, a LaunchAgent on macOS, or the
  per-user Run key on Windows. It stays off unless the person turns it on.
- Installed applications, not only games: the Windows uninstall registry and
  Store packages, Linux desktop entries (with Flatpak and Snap), and macOS
  application bundles. They are synced to droidtop as library entries of
  the platform `app` (`app:<source>:<id>`). `droidtop-agent scan` lists
  them. The programs droidtop can sync with are listed apart, as "syncs
  with".
- Rendezvous away from home, the way Syncthing does it: STUN from the
  WireGuard socket, Syncthing's global discovery protocol to announce and
  look up addresses (a discovery ID per device, from a certificate made from
  its key), and UDP hole punching between paired devices. Syncthing's
  servers by default, at Syncthing's own client's pace; addresses only,
  never its relays. `droidtop-agent rendezvous` shows and sets it.
- droidtop learns which way a sync went (`path`: lan, wireguard,
  rendezvous) and the computer's discovery ID from each session.
- Rendezvous now runs on the windowcast-rendezvous crate that windowcast
  shares, in place of this repository's own copy; behaviour and device IDs
  are unchanged (a test pins the ID derivation).

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
