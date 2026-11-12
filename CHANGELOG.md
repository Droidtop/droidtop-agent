# Changelog

Every green build of `main` is a release named `build-<run number>`; this file
records what each change brought. The newest entry is the one the next
release carries until its number is written in.

## Unreleased

- On lib-sync 0.2 (Droidtop/tracker#573), with lib-games 0.4, lib-saves 0.3 and lib-plugins 0.3, the libraries' lib-sync 0.2
  releases. Nothing in the agent changes; its crates build unchanged against them.
- A game streams from away through the agent's own WireGuard tunnel (Droidtop/tracker#113, docs/DESIGN.md section
  22): on Windows, the embedded windowcast host serves a handheld over the tunnel's side link (signaling) and its
  datagrams (media), with windowcast's `serve_over`. The handheld's half, in droidtop's library, is next.
- Builds with Rust 1.85 again for what Pythia links (Droidtop/tracker#867): the workspace's `rust-version` is 1.85,
  on lib-sync 0.1.6, games-scan 0.3.1, saves-locate 0.2.1 and plugins-contexts 0.2.2, the releases that build with
  it. The tray window (`crates/app`) declares 1.88, which egui 0.33 needs. A new `msrv` CI job checks Pythia's
  crates with 1.85.
- Epic games are keyed by Epic's catalog id, `epic:<CatalogItemId>`, the key droidtop gives them, so an Epic game
  installed on both sides is one game and Stream from and Play on find it (Droidtop/tracker#871). A game with no
  store id is keyed `title2:` and its title's Lutris slug ("Being a DIK" is `title2:being-a-dik`), the one title key
  on every device (Droidtop/tracker#870). After a scan the agent links each game's old key to its new one wherever
  something was stored under the old key, so nothing a person set is lost (docs/DESIGN.md, "A game's identity
  across devices"). Built on games-scan 0.3.0.
- KDE Connect (Droidtop/tracker#645, docs/DESIGN.md section 23): droidtop
  and Pythia can now be KDE Connect devices. They are found by the official
  KDE Connect apps and by each other, pair with the 8-character code both
  screens show, and share files, links, text and the clipboard both ways,
  type and move the pointer, ring each other, show each other's battery and
  notifications. droidtop and Pythia reach it through their own native modules; this
  repository records the design (docs/DESIGN.md section 23).
- A Windows installer (Droidtop/tracker#116):
  `droidtop-agent-setup-windows-x86_64.exe` installs the agent for you
  alone, with no administrator, adds it to the Start menu, and can start it
  when you sign in. Uninstalling it stops the agent and keeps your
  pairings, settings and kept saves.
- Cross-store identity, first part (Droidtop/tracker#798, docs/DESIGN.md
  section 16): the library can say that two keys are one game (a `link`
  change, newest wins, undone with no target), and every device follows
  links to the same key. Only devices that state hello feature `identity`
  receive links; the cloud folder carries none (droidtop's side is in its
  `native/`). Pythia sets them with
  `store.link` and reads `store.canonical`.
- KDE Connect (Droidtop/tracker#645, docs/DESIGN.md section 23): droidtop
  and Pythia can now be KDE Connect devices. They are found by the official
  KDE Connect apps and by each other, pair with the 8-character code both
  screens show, and ping. File transfer, clipboard and remote input follow.
  droidtop and Pythia reach it through their own native modules; this
  repository records the design (docs/DESIGN.md section 23).
- What each handheld may watch when it streams from this computer
  (Droidtop/tracker#116): every window, as before, or only the games it
  starts. Choose it beside each handheld under Streaming games on the
  Status page, or with `droidtop-agent sharing <id> all|games`.
- The window's Status page has a Streaming games section
  (Droidtop/tracker#116): whether this computer can stream to a handheld,
  which handhelds can watch the games they start, and who is watching now,
  with how many windows and for how long.
- Releases no longer carry droidtop's Android library (`droidtop-agent-android.tar.zst`): droidtop builds
  `libdroidtop_agent.so` itself from its own `native/`, the code that was `crates/android` here, over the
  same shared libraries (Droidtop/tracker#867). Nothing changes for people using the agent.
- The Python module (`droidtop_agent`) can now keep a copy of a game's saves on demand (`saves.back_up`),
  take a desktop manager's launcher for the games it starts itself (`service.set_launcher`), and pair this
  computer with another by comparing a code (`pair.offer`, `find`, `pick`, `compare_status`, `confirm`,
  `stop`), as Pythia's own copy of these bindings already did. A desktop manager now builds its module
  over this one instead of copying it (Droidtop/tracker#867).
- Context adapters, protocol 2 (Droidtop/tracker#794, docs/DESIGN.md
  section 8): an adapter for an app that cannot publish its own copy
  (F95Checker) says `"snapshot": true`, and before a handheld reads the copy
  the agent asks it for the app's data and publishes it when it changed.
  `received` now carries the copy's records with its generation.
- A device this computer no longer knows (it was unpaired here) is told so
  as the connection opens, with `code: not_paired`, so the handheld says to
  pair again instead of that the computer did not answer
  (Droidtop/tracker#531). On the home network only: through WireGuard a
  forgotten device still gets no answer.
- Nothing changes for people using the agent. Behind it, the code it shares
  with droidtop and Pythia now lives in shared libraries of its own
  (Droidtop/tracker#867): pairing, sync and the agent's state
  (Droidtop/lib-sync), the scanner (Droidtop/lib-games), engine detection
  (Droidtop/lib-engines), save locations (Droidtop/lib-saves) and plugin
  context adapters (Droidtop/lib-plugins), each pinned here by tag.
- Streaming a game to the handheld (Droidtop/tracker#860, docs/DESIGN.md
  section 22): on Windows the agent embeds windowcast's host. When a
  handheld that may start games here asks to play a game and watch it, the
  agent trusts that handheld's windowcast viewer (no PIN: the pairing with
  the agent is enough), starts windowcast's host on port 47100 if it is not
  running yet, then starts the game; the handheld opens the game's window
  with its sound, and its gamepads drive the game (ViGEmBus needed). The
  hello states `stream`. Pythia streams the same way, through the agent.
- A handheld reaching the computer through WireGuard can make a second
  call right after the first: copying a newer version of a game straight
  after the library sync showed it no longer fails with "the computer did
  not answer through WireGuard" (Droidtop/tracker#529, #469 part 3).
- Service mode (docs/DESIGN.md section 19, windowcast#467): off unless the
  person turns it on, after it says what it does; `droidtop-agent
  service-mode [enable | disable | uninstall]` and Settings > Service mode
  install, enable, disable or remove windowcast's secure-screen helper
  through its client API, each change with Windows' administrator approval.
  The Windows release carries the helper
  (`windowcast-secure-helper-windows-x86_64.exe`).
- Syncthing with the paired devices (docs/DESIGN.md section 18,
  Droidtop/tracker#553): the running agent uses the Syncthing already
  answering in Syncthing's standard folder, or starts one there, announces
  its device ID and LAN addresses as `droidtop.syncthing`, and adds each
  paired device's announced Syncthing ID (removing only the ones it added
  once they are forgotten). `droidtop-agent syncthing [on | off]`.
- Pairing by comparing a code (droidtop SPEC 7o, "Pairing with Pythia"): a
  computer offering to pair answers `DTCQ1` with its id, name, port and
  program; the device that picks it runs a commit-and-reveal exchange
  (`core::confirm`, `droidtop-agent pair-confirm v1`) and both screens show
  one 6-digit code; both pin after the person says yes on both. On the
  computer: `droidtop-agent pair compare` and `pair find`; in droidtop's
  library: `computers_find`, `compare_start`, `compare_finish`,
  `compare_cancel`.
- A computer pairs with another computer (owner, 2026-10-16): `droidtop-agent
  pair` with nothing after it also prints an invitation another computer
  types (`droidtop-agent pair '<invitation>'`), and each pins the other as a
  computer. The refusal to pin a computer is gone (docs/DESIGN.md section 3).
- Every session is mutual TLS 1.3 (rustls), each device presenting a
  self-signed certificate over its own key, in place of the Noise channel
  (docs/DESIGN.md sections 3 and 4, Droidtop/tracker#556). Pairing (now
  version 3) carries each side's certificate fingerprint and kind
  (computer or handheld). The pin is the certificate's public key, so
  devices paired before keep their pairing, but **both sides must update
  together**: a device from before TLS cannot open the new channel.
- Handhelds pair with each other with the same screens, and one serves the
  others while droidtop asks (`serve_start`, `serve_stop`).
- Announcements: a device publishes a small value per plugin to the devices
  paired with it (`announce`), so the Syncthing plugin can accept exactly
  the paired devices (Droidtop/tracker#553). On the computer:
  `droidtop_agent.store.set_announce(plugin, value)`, and each device's
  `announced` in `store.devices()`.
- What the running agent does is an event a host program can read, not
  only a printed line: `droidtop_agent.service.events(since=0, wait=0)`
  gives each with its number, time, level, kind (`launch`, `session`,
  `saves`, `library`, `arrived`...), message and what it is about, so
  Pythia can log and show handhelds' launches, syncs and arriving games.
  `droidtop-agent run` prints what it printed before, plus a line when a
  session ends and when saves or library changes moved (docs/DESIGN.md
  section 16, Droidtop/tracker#461).
- A launcher's own folder (Battle.net, the EA app, Origin, GOG Galaxy's
  client, Ubisoft Connect, Epic's Launcher) in a game folder is no longer
  a game; `droidtop_agent.scan.launcher_folder(path)` gives the same rule
  to a desktop manager (docs/DESIGN.md section 11, Droidtop/tracker#538).
- The agent is a set of library crates, so a desktop manager is built from
  the same code as the programs: `store` (the state on disk), `scan` (the
  scanner, over the folders it is given), `saves` (where saves are, kept
  copies, restore), `pair`, `contexts` and `service` (the running agent);
  `droidtop-agent` and `droidtop-agent-app` are thin programs over them
  (docs/DESIGN.md section 13, Droidtop/tracker#461). Nothing changes for a
  person using them.
- The Python module `droidtop_agent` has a submodule per crate with typed
  functions (`droidtop_agent.store.devices()`, `.pair.show()`,
  `.scan.set_manager_games(...)`...) raising `droidtop_agent.AgentError`;
  the JSON `call(op, args)` is gone (docs/DESIGN.md section 16).
- The games a desktop manager tracks are kept by the agent
  (`manager-games.json`), so closing Pythia no longer takes its games off
  the handhelds when droidtop-agent serves after it. A manager game's
  folder is refused when it is a whole drive, holds the home folder or holds
  the agent's own files, since a handheld can copy a game's folder.

- A handheld can ask this computer to start a Steam game: the `launch_game`
  request (hello feature `launch`), so droidtop can offer "Stream with Steam
  Link". The agent builds `steam://rungameid/<id>` itself from a `steam:<id>`
  key and refuses any other, only for a handheld the person allowed
  (Settings > Start games, or `droidtop-agent launch allow <id>`; off by
  default), and answers started, not allowed, unknown game, Steam not running
  or failed (docs/DESIGN.md section 17, Droidtop/tracker#552).
- The agent as a Python extension module, `droidtop_agent` (crates/python,
  one abi3 wheel per system in each release), so Pythia is this computer's
  agent while it runs: same identity, pairings, library, saves, scanner and
  service, in its own process. The games Pythia tracks join the scan where
  the scanner has not found them (docs/DESIGN.md section 16,
  Droidtop/tracker#461).
- The window attaches to a running agent instead of competing with it, and
  every copy of the agent (headless, window, command line) reads the paired
  devices and the library from their files again when they changed and writes
  them under a lock. Before, a copy that started earlier wrote its empty
  device list over a pairing, so Status said "None yet", and wrote its old
  library over marks (Droidtop/tracker#528, #531).
- A folder named as one run of words (`DtSyncTest v1.1`) is titled
  "Dt Sync Test" as droidtop titles it, so it is one game on the computer and
  the handheld: the handheld sees the computer's v1.1 and its marks travel
  (Droidtop/tracker#529). Games found under the old title are re-found under
  the new one at the next scan.
- A handheld whose computer lost its library sends everything again and takes
  everything, so its favourites come back (Droidtop/tracker#531).
- Marks can be set on the computer: favourite, hidden and collections in the
  window's Library page, `droidtop-agent mark` and `marks` on a command line.
- The window's Library page lists every place games are found with a switch
  and a game count, adds and removes game and ROM folders with the system's
  folder picker, and searches the games with each one's source and place.
  Saves shows games by title, where their saves are (editable), each device's
  newest copy and the older ones with their dates, restore, open folder, and
  the preferred device.

- A game's version can be copied between a handheld and a computer
  (`core::gamecopy`, `game_pull` and `game_push`; Droidtop/tracker#469 part
  3). It arrives as a new folder beside the game's others, resumable and
  checked per file. On the computer it goes to the person's first game
  folder, never into a store's install folders.
- Linux packages with each release: a portable `.tar.zst`, one runnable
  `.run` file, a `.deb` and an `.rpm` for x86_64 and aarch64, plus
  `install.sh` (per user, no root, `--autostart` optional) and `uninstall.sh`
  (removes exactly what the install manifest lists; `--purge` also deletes
  your data), and `SHA256SUMS` for everything in the release
  (Droidtop/tracker#373).

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
