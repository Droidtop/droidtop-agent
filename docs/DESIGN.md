# droidtop-agent: design

droidtop-agent is a small program for the user's own computers (Windows,
Linux, macOS). It pairs with droidtop on a handheld and keeps the two in step:
the games each side has, their saves, and the supporting state of droidtop's
plugins (for example an F95Checker database). The aim, in the owner's words:
"Droidtop's sync system needs to allow the droidtop device to be a full
functioned computer. If I already have a gaming computer or a game library, I
should be able to sync to and from it."

What it is not: it never streams a screen or remote-controls anything (that is
windowcast's job), it never runs a Syncthing instance, and it never needs
Tailscale or any other VPN. It is meant to be light: one process on the
computer, no resident process on the handheld.

Tracked as Droidtop/tracker#373 (with #380 for plugin context sync, #284 and
#224 for saves, #364 for the server VM).

## Contents

1. What droidtop already has (audit, droidtop main c5dd7d8d, 2026-10-08)
2. Language and licence
3. Identity and pairing
4. The session channel
5. The protocol
6. Saves
7. Library
8. Syncable data from plugins
9. Conflicts
10. Transports, in order
11. The computer's scanner
12. The droidtop side
13. Repository layout and builds
14. Decisions for the owner
15. The window and the tray
16. Inside a desktop manager (Pythia)
17. Starting a game for a handheld
18. Syncthing with the paired devices
19. Service mode: Windows security screens
20. Offers: what one device shares with another
21. Plugins on the computer

## 1. What droidtop already has

Read from droidtop main at c5dd7d8d.

- **Store cloud saves (Droidtop/tracker#224).** Steam Cloud is built:
  `StoreSaves` (`library-core/.../stores/StoreSaves.kt`) is the launch seam.
  `PcGameProvider` calls `StoreSaves.beforeLaunch` (waits at most 45 s), and
  `WineGameActivity` calls `StoreSaves.afterExit`, which runs the upload as a
  job in the Downloads place. `SteamCloudSync` and `SteamSaveFiles` in
  `:stores` do the work. Steam's `ufs` product info says where the saves are,
  and `SaveLayout.windowsDirs` maps Windows roots into the game's Wine prefix.
  The decision (`SteamCloudPlan.decide`) is Steam's client rule, made per game:
  compare both sides with the files as they were at the last sync. If one side
  changed, it wins. If both changed, it is a conflict.
- **Conflict UI.** The conflict question is the person's: `SaveConflictPrompts`
  feeds the Gaming shell's `SaveConflictDialog` ("Cloud saves differ", each
  side's last change, file count, which is newer). `SaveConflict.cloudLabel`
  already lets the other side be named.
- **GOG and Epic cloud saves are not reached.** The vendored GameNative code
  for them resolves save folders inside its own containers. Amazon has none.
  That part of #224 is still open.
- **No save-location data beyond the stores.** droidtop has no Ludusavi or
  PCGamingWiki data. #373's "Ludusavi data, same as droidtop" is not true
  today: the only save locations droidtop knows are Steam's `ufs` lists.
- **The `saves.sync` extension point** is declared (`ExtensionPoints.kt`,
  high risk) and documented as A7 in `docs/plugin-api.md`. Nothing provides
  it or calls it (#284).
- **F95Checker** is a one-time core import (`F95CheckerImport`, Settings >
  Library > "Import from F95Checker"). It opens the picked `db.sqlite3`
  read-only and offers thread links to confirm. Nothing syncs (#380).
- **Secrets at rest** use one mechanism: `KeystoreSecretCipher` in
  `:net-core` (AES-256-GCM under a non-exportable Android Keystore key).
- **QR codes** are made on the device (`app/ui/QrCode.kt`, zxing).
- **SPEC 7a, "No PC-side helper in droidtop"**, says anything that runs on
  the PC belongs to windowcast. The owner has since decided (#373, 2026-10-08)
  that the desktop client is droidtop's agent and later becomes windowcast's
  interface on the PC too ("one desktop app, not two"). Its network layer is
  a shared module that windowcast bakes in. droidtop itself still carries no
  PC program: the agent is this separate repository.

## 2. Language and licence

**Rust.** Three reasons:

1. windowcast is Rust. The owner wants this network layer to be "a shared
   module that windowcast bakes in", and windowcast's `identity` and `pairing`
   crates are exactly what the agent needs. The agent uses them as they are,
   with no second implementation.
2. The same crate builds for Windows, Linux and macOS, and also as an
   Android library (`cdylib`, arm64-v8a and x86_64). droidtop calls it through
   JNI, so both ends of the protocol, the merge rules and the crypto are one
   piece of code. Writing SPAKE2 or WireGuard a second time in Kotlin would be
   two mechanisms for one job, and the riskier one.
3. Userspace WireGuard exists as a Rust library (boringtun, BSD-3-Clause),
   and so does a userspace TCP stack to carry a stream inside the tunnel
   (smoltcp, 0BSD). Both are the parts `onetun` is built from.

Go was the other candidate: wireguard-go is the reference implementation, and
droidtop's CI already cross-compiles Go (crane). It lost on point 1: windowcast
cannot bake in a Go module.

**Licence: GPL-3.0-only, because a dependency forces it.** windowcast and its
`identity` and `pairing` crates are GPL-3.0-only, and the agent links them.
droidtop is GPL-3.0 too, so nothing downstream is affected. If the owner
relicenses those two windowcast crates under MIT or Apache-2.0, the agent can
follow (decision 1).

## 3. Identity and pairing

- **One identity per device:** a persistent Ed25519 key pair, the windowcast
  `Identity` (`windowcast-identity`). Its public key is the device's `PeerId`.
  The same key, converted to X25519 (the standard birational map, as
  libsodium's `crypto_sign_ed25519_*_to_curve25519` does), is the device's
  WireGuard static key. One key, one trust decision, for every transport.
- **A device certificate over that key** (Droidtop/tracker#556, `core::cert`):
  self-signed X.509 that the device makes itself, issued by no one (no
  signing root of ours, Droidtop Core's included, plays any part: pinning at
  pairing is the trust). Its subject is the device's short id; it is valid
  from 2020 to 9999, since the pin, not a date, is what is trusted. Pairing
  pins its **fingerprint**: the SHA-256 of its SubjectPublicKeyInfo, the
  usual public-key pin. A pin of the key and not of the certificate's bytes
  lets a device reissue its certificate without being paired again, and a
  device paired by its PeerId before certificates existed is already pinned
  (an Ed25519 SubjectPublicKeyInfo is a fixed prefix and the key), so no one
  pairs again for this change. A new key is a new identity, handed over by
  `moved` below; forgetting a device unpins it everywhere at once. On the handheld, droidtop keeps the key sealed with
  `KeystoreSecretCipher` and passes it to the library for each call.
- **One identity with windowcast on a computer** (owner, 2026-10-10: a
  computer paired once is paired for both; decision 3). The agent's identity
  and trusted devices are windowcast's host files, in
  `windowcast_identity::computer_dir()`: the reference app's `host` folder,
  `%APPDATA%\windowcast\app\host` or `~/.config/windowcast/app/host`, holding
  `agent-identity.key` and `agent-trusted-clients`. So a device paired with
  either program is trusted by both, and forgetting it in either forgets it
  in both. Both programs change the list with `TrustStore::update`, against
  what is on disk, and the agent reads it at every check.
  `DROIDTOP_AGENT_HOME` keeps everything in one folder (portable use, tests).
- **Moving existing pairings** (`store::migrate`, once, at start):
  1. Only the agent had an identity: its key becomes the shared one, and
     nothing changes for the paired handhelds.
  2. windowcast had one and the agent had no pairings: the agent adopts it.
  3. Both had one and the agent had pairings: windowcast's stays. The agent
     keeps its own as `previous-identity.key` and answers to both
     (`Channel::accept_any`). A handheld that reaches it at the old identity
     gets `moved_to` in the hello reply: the new id, signed by the old key
     and by the new one (`core::moved`). droidtop checks both signatures and
     re-pins the computer. Once every paired handheld has reached the new
     identity, the old key is deleted. Until a handheld has, it moves at its
     next session on the home network (the WireGuard path answers only the
     current key).
  4. The agent's trusted handhelds join the shared list. The agent's old
     files are renamed `*.moved`.
- **Trust** is windowcast's `TrustStore`: the set of pinned peers. The agent
  keeps a separate peers file for names and last-known addresses, so no
  trust decision is ever made from it.
- **Kinds of device.** Pairing says what each side is: a `computer`
  (droidtop-agent, or a desktop manager such as Pythia) or a `handheld`
  (droidtop). Handhelds pair with each other with the same screens and the
  same exchange as with a computer (one shows the code, the other types it),
  and a handheld serves the handhelds paired with it while droidtop asks
  (`serve_start`, section 12).
- **A computer pairs with a computer too** (owner, 2026-10-16, relayed by
  the droidtop coordinator: "Computer to computer pairing is ABSOLUTELY a
  pythia feature, it's the whole fucking reason it exists"). The same
  exchange and the same device certificates: one computer shows a code
  (`droidtop-agent pair` with nothing after it, or Pythia's Show a code)
  and prints an invitation, `droidtop-pair:1?code=…&id=…&name=…&at=…`; the
  other types it (`droidtop-agent pair '<invitation>'`). Each pins the
  other as a `computer` (`pair::keep`). What two paired computers then
  carry between them is Pythia's to define (Pythia docs/SPEC.md, C1); until
  it does, the pin is kept and nothing else uses it.
- **Any number of devices, any pairing graph** (owner, 2026-10-10: "if we have
  5 droidtop devices and 5 desktops, we should be able to connect and pair any
  number of permutations of these devices and have them work"). A computer
  trusts any number of handhelds, and a handheld keeps any number of
  computers. Each pair is its own trust decision, session, WireGuard tunnel and
  rendezvous lookup. Section 9 says how saves, the library and contexts travel
  between devices that are not paired directly.
- **Pairing is windowcast's exchange** (`windowcast_pairing::exchange`), the
  one windowcast's signaling runs, under this program's label (`droidtop-agent
  pair v3`):
  - both sides send a hello: identity, a fresh nonce and the SPAKE2 message
    (6 digits from `generate_pin`, HKDF to a session key);
  - each then describes itself, `{name, kind, cert}` (`cert` its
    certificate's fingerprint), with a proof over the transcript (label,
    side, both nonces, both identities, the description's exact text): its
    identity's signature and the PIN key's HMAC tag;
  - each checks the fingerprint is the one of the key that proved it, and
    pins it.
  - A wrong code, a substituted key, a certificate of another key or a
    replayed transcript fails the check.
  - A peer running an older exchange (version 1 or 2) is told to update.
  1. On the handheld: Settings > Computers > "Pair a computer". droidtop opens
     a short-lived listener (only while that screen is open), shows the 6-digit
     code in large type, and shows a QR code of
     `droidtop-pair:1?code=<code>&id=<PeerId hex>&name=<name>&at=<ip>:<port>`.
     The handheld is the SPAKE2 host (it shows the code).
  2. On the computer: `droidtop-agent pair 123456`. The agent finds the
     handheld on the LAN by a UDP broadcast query that only a handheld in
     pairing mode answers. `droidtop-agent pair 'droidtop-pair:1?...'` (the
     QR's text, from a phone or webcam) goes straight to the address in it.
  3. The two run the exchange above over TCP, and each pins the other's
     certificate.
  The pairing listener accepts one attempt per code. Three wrong attempts end
  the pairing screen's session.
- **Why the code is typed on the computer and not scanned:** neither a desktop
  PC nor the Retroid Pocket 5 has a camera. The QR code is there for a computer
  that has one (a laptop webcam) and for a phone acting for the computer
  (decision 2).
- **The other way round, for a handheld the computer cannot reach**
  (coordinator, 2026-10-09): one behind an emulator's or a guest network's
  NAT, where the computer's connection to the handheld would need a port
  forward. `droidtop-agent pair` with no code makes the computer the SPAKE2
  host: it listens on TCP 47612 (or any free port), prints its addresses and
  a new code, and waits up to 10 minutes. On the handheld, Pair a computer >
  "Use a code from the computer" takes the address and the code and
  connects (`pair_connect`). The same tags and pins follow, and the handheld
  keeps the computer's address with the agent's port 47610. Three wrong codes
  end it.

## 4. The session channel

Every live connection is the same channel, whatever carries it: **mutual
TLS 1.3** (rustls on the ring provider, `core::channel`), each side
presenting its device certificate (section 3). It replaced a Noise_IK
channel (Droidtop/tracker#556, owner: "use mtls for device co-identification
for pairing"): one mechanism, standard TLS, the same on the computer and on
the handheld, since droidtop runs this core through `AgentNative`.

- TLS 1.3 only, Ed25519 only (handshake signatures and certificates), no
  session tickets or resumption, no certificate authorities.
- The side that connects (a handheld) knows the other's key from pairing
  and accepts only a certificate carrying it. It names the device it wants
  as the server name (`<short id>.droidtop.invalid`), so a computer moving
  between two identities presents the one asked for.
- The side that accepts reads the connecting device's key from its
  certificate, after TLS has checked the handshake signature with it, and
  refuses the session unless that device is pinned.
- An application message is a 4-byte big-endian length followed by its
  bytes, at most 64 MiB.
- Application messages are JSON objects (`serde`, internally tagged by `t`).
  File contents travel as separate binary messages after the JSON header
  that announces them, in 1 MiB pieces.

Over the WireGuard path (section 10) this channel runs inside the tunnel as
well. That is two layers of encryption, which costs little on the
handheld's CPU, and it keeps one authentication path for every transport.

A device from before TLS (protocol version 1) cannot open this channel, and
this one cannot open its: both sides update together. The handheld's error
says the other side may be older. droidtop pins the agent library by
release, so it moves with the agent it is built against.

## 5. The protocol

Requests come from the handheld, and the computer answers. Each request gets
exactly one reply (`{"t":"error","message":...}` on failure).

| Request | Reply | Purpose |
|---|---|---|
| `hello {name, version, features}` | `hello {...}` | names, versions, what each side supports |
| `library_pull {since}` | `library_changes {changes, cursor}` | the computer's library changes since a cursor |
| `library_push {changes}` | `ok {cursor}` | the handheld's library changes |
| `save_spec {game}` | `save_spec {spec}` or `unknown` | where this game keeps its saves, as templates |
| `save_manifest {game}` | `manifest {files, primary}` | the computer's save files: name, size, mtime, SHA-256; `primary` when this handheld is the computer's primary handheld |
| `file_get {game, name}` | `file {name, size, sha256}` + data | one save file, computer to handheld |
| `file_put {game, name, size, sha256, mtime}` + data | `ok` | one save file, handheld to computer |
| `save_apply {game, remove, archive}` | `ok` | removals, and keeping the computer's own changed set as its copy first (`archive`) |
| `context_pull {context}` | `context {records}` | a plugin context's records on the computer |
| `context_push {context, changes}` | `ok` or `deferred` | record changes to apply on the computer |
| `launch_game {game}` | `launch {outcome}` | start a Steam game on the computer (section 17) |
| `announce {values}` | `announced {values}` | each side's per-plugin values for the devices paired with it (below) |

**Announcements** (hello feature `announce`, Droidtop/tracker#556, #553):
a device publishes a small value per plugin to the devices paired with it,
such as the Syncthing plugin's `{"syncthing": "<its device ID>"}`, so that
plugin can accept exactly the devices the person paired, with no pairing
of its own. `values` is every plugin's value (at most 64 plugins, ids of 1
to 128 of `A-Z a-z 0-9 . _ -`, each value at most 4096 bytes of JSON) and
replaces what that device announced before; the reply carries the other
side's. A computer keeps them per device (`announced` in `devices`) and
announces what its own modules set (`store.set_announce`); a handheld
serving another keeps them in the file droidtop names. droidtop shows each
plugin only its own value. This computer's Syncthing announces under the
Syncthing plugin's id (section 18).

Names inside a save set are canonical: a root token and a path with forward
slashes (`<winAppData>/Game/save1.dat`), compared case-insensitively.

## 6. Saves

- **Where saves are.** A save set is a list of templates in Ludusavi's
  vocabulary: `<base>` (the game's folder), `<home>`, `<winAppData>`,
  `<winLocalAppData>`, `<winLocalAppDataLow>`, `<winDocuments>`,
  `<winPublic>`, `<winProgramData>`, `<winDir>`, `<xdgData>`, `<xdgConfig>`,
  `<storeUserId>`, plus globs. The computer side gets them from the Ludusavi
  manifest (fetched by the agent at run time from the ludusavi-manifest
  repository, never bundled; its data comes from PCGamingWiki) and from the
  user's own entries (`droidtop-agent saves add`). The handheld does not carry
  the manifest. It asks for the spec (`save_spec`) and resolves each token in
  its own copy of the game:
  - a Windows game resolves tokens inside the game's Wine prefix, through
    the `WinePrefixLocator` droidtop already has;
  - an engine game resolves `<base>` to its folder (Ren'Py and RPG Maker keep
    saves there, per the standing save policy).
- **On a Linux or macOS computer** the Windows templates are resolved in the
  prefix the game runs in there: Steam's Proton prefix, Heroic's per-game
  `winePrefix`, a Lutris Wine game's `prefix`, a Bottles bottle, or a
  Minigalaxy game's own prefix. Both sides then hold the Windows build's
  saves. A game the computer runs as a native Linux or macOS build keeps its
  saves elsewhere, often in another format. Those saves are not matched to
  the handheld's Windows copy, and only the person's own `saves add` entries
  cover such a game.
- **The decision: the newest copy wins** (owner, 2026-10-10: "newest copy wins
  isn't the worst idea. We keep the newest copy from EACH device, and an
  archive if possible. We can also set a primary desktop and primary device to
  prefer saves from. It's not like syncthing where we're handling lots of
  arbitrary files"). The handheld keeps a baseline per paired computer and
  game: the files as they were after the last sync (name, SHA-256, size,
  mtime).
  - If only one side changed since the baseline, that side wins: its changed
    files are copied over, and files it deleted are deleted on the other side.
  - If both changed, or a first sync finds different files on each side, a
    preferred side wins: the handheld's primary computer (droidtop, Settings
    > Computers) or the computer's primary handheld (`droidtop-agent
    primary`, the window's Settings). When neither side, or both, is
    preferred, the set whose newest file is newer wins.
  - The other side's changed set is kept, never lost (section 9).
  - A save set is a game's whole set, not files picked one by one: a game's
    files belong together (an index and its slots), so mixing files from two
    devices could break a save. There are no version vectors: a game's saves
    are a small, known set of files.
  - A file whose size and mtime match its baseline entry is not re-hashed.
- **When it runs:** before a game starts and after it ends, on the same seam
  as `StoreSaves` (one launch path, two save sources). A computer that cannot
  be reached is skipped once and said once. A game whose store already syncs
  its saves (Steam Cloud) is left to the store by default, so two syncs never
  fight over the same files (decision 5).
- **Writes are safe:** each file is written beside its target as
  `.<name>.dtpart` and renamed over the target only when its SHA-256 matches.
  A failure keeps the baseline entry as it was, so the next sync retries the
  file instead of reading the failure as a change.
- **No general versioning.** Only the copies section 9 keeps.

## 7. Library

The aim: a game on the computer shows up on the handheld, and the other way
round. Each device is the authority on what it has installed. The person's own
marks on a game are shared.

- **A game's identity across devices** is its store key where it has one
  (`steam:<appid>`, `gog:<id>`, `epic:<app name>`, `amazon:<id>`,
  `itch:<game id>`, `battlenet:<product>`). A game without one uses
  `title:<normalised title>`. A ROM uses
  `rom:<es-de system>/<normalised file stem>`. droidtop already names
  platforms with ES-DE's system names (SPEC 7b).
- **Facts per device:** installed, where it is (path), size, version, and
  which launcher owns it. Only the device that has the game writes these, so
  they never conflict. A removed game is a tombstone.
- **Shared marks:** favourite, hidden, completion state, rating, notes, tags
  and collections. Each field is last-writer-wins on a hybrid logical clock
  (wall time, counter, device id), so a mark made on either side lands on both
  and a later mark beats an earlier one. droidtop carries `favourite`,
  `hidden`, `completed` and `kid_game` (booleans), `rating` (0 to 1),
  `title` (the person's own name for the game), `sort_name`, and
  `collections` (a sorted list of collection names, made where missing).
  Per-device choices (emulator, launch screen, media paths) do not travel
  (Droidtop/tracker#469 part 1). On a computer, Lutris's "favorite" and
  ".hidden" categories are read as marks, but only when Lutris changed them
  since the last scan, so a mark set elsewhere is not undone. The agent never
  writes a launcher's store.
  - Before each exchange droidtop reports every mark it keeps on its own
    games, unset ones included. Only a mark that differs from the shared one
    becomes a change (`Library::note_marks`); unset, absent, false, zero and
    empty all count as the same. So a mark that arrived from elsewhere and
    was written into droidtop's library is not sent back with a newer stamp.
  - After it, the core names the shared marks that differ from what droidtop
    reported (`Library::marks_to_write`), and droidtop writes them into its
    own library.
- **Versions** (Droidtop/tracker#469 part 2): each device states the version
  it has installed in its facts. That is a store's build or version, the
  version a folder game's name carries, or droidtop's set or derived
  version. Each device keeps a game's version history (`GameRecord::versions`:
  device, version, when first heard of, at most 30), filled from the install
  changes it applies, so a device learns the versions of devices it never
  meets. droidtop turns a newer version on a computer into "an update is
  available" (its SPEC 7o).
- **Game updates between devices** (Droidtop/tracker#469 part 3; owner,
  2026-10-10: "If I install an update to a game on my desktop, I should be
  able to sync it to the handheld and version-manage it automatically").
  `core::gamecopy` copies a game's folder as a NEW folder beside the game's
  others, named for its version (droidtop's "a version is a folder", its
  SPEC 7m). The previous version stays playable, rolling back is choosing
  it, and nothing is changed in place.
  - The copy is made in `<name>.dtpart/` and renamed into place only when
    every file is whole.
  - A copy that stopped continues: a file already the right size and time
    is kept, and a shorter one goes on from where it ends.
  - Files travel in 1 MiB pieces over the session channel (LAN or
    WireGuard; never the cloud folder), each followed by the whole file's
    SHA-256, which the receiver checks.
  - Paths are plain relative paths; anything else is refused on both sides,
    and symbolic links are not followed.
  - From the computer: the game's folder as the scan found it (a store's
    install folder included). To the computer: the person's first game
    folder, never a store's install folders. The computer rescans when one
    arrives.
  - droidtop runs it as a job, with progress (`game_pull`, `game_push`).
- **Games meet by key, so both ends must make the same key from the same
  folder name.** A folder game with no store id is keyed by its title
  (`title_key`). droidtop reads a name written as one run of words,
  `DtSyncTest`, as "Dt Sync Test" (`GameTitleParser.splitRunTogether`,
  Droidtop/tracker#282); the scanner does the same (`split_run_together`),
  or the same folder was two games, one per device, and neither a computer's
  newer version nor a mark reached the other (Droidtop/tracker#529).
- **A computer that lost its library is told.** A handheld that pulls from a
  computer and finds its sequence number lower than the one it pulled up to
  before knows the computer lost state (a restore, a reinstall). It sends
  everything it has again and pulls everything; both are newest-wins, so
  nothing is doubled (Droidtop/tracker#531).
- **Play time:** each device reports its own total and last played time. A
  game's total is the sum, and its last played time is the latest. Nothing
  overwrites another device's numbers.
- **Change tracking both ways:** each side keeps an append-only change log
  with a sequence number. A peer asks for changes "since" the last cursor it
  saw, so a sync costs as much as what changed, not the size of the library.
- **What droidtop does with the computer's games:** they are listed under the
  computer's name ("On DESKTOP-PC"). The actions are copying a game to the
  handheld (a per-game transfer over the same channel, on request) and, later,
  streaming it with windowcast. How far they merge into droidtop's main
  library is decision 6.

## 8. Syncable data from plugins

The owner (#380): "It's context SYNC. It's meant to allow you to do things on
the computer and have them synced." And (2026-10-23), on what may travel:
"Plugin STATE should not be synced. Plugins can be permitted to expose a
'syncable data' context, for data that CAN be synced, but in general, plugin
states can include sensitive user data and cookies, and we can't guarantee a
third party plugin has secured it correctly." A syncable data context "is an
explicit NON-USABLE context. It is a separate context of data, probably in a
database, that is syncable. Plugins will notify when they have updated the
syncable dataset, and plugins will be updated when they have received an
update to it." It "should be part of the plugin sandbox"; "the point is that
the sync is atomic, and it's not the live dataset". The plugin API side is in
droidtop's `docs/plugin-api.md` (F8). The shape:

- **A plugin's own state never syncs.** Nothing here reads or writes the data
  a plugin, or the app it serves, works from. Cookies, tokens and passwords
  stay where they are because the sync never reaches them.
- **A syncable data context is a copy kept only to be synced.** The plugin
  chooses what goes into it and writes it there itself; nothing runs from it.
  It lives in the plugin's sandbox: on the handheld, droidtop's; on the
  computer, the computer-side plugin's (the program below, under the agent's
  data folder, `adapters/<context>/`). Both ends have the same context, by
  the same id. F95's is a copy of the declared columns of F95Checker's
  database, not `db.sqlite3` itself.
- **A context is a set of records**, `key -> {field -> JSON value}`, with the
  plugin's declaration: which fields exist, whether each is two-way,
  computer-to-device or device-to-computer, and its conflict rule (`device`,
  `computer`, or `ask`).
- **Atomic, both ways.** A plugin publishes a whole new copy in one step (it
  writes it apart, then makes it current); the sync reads only a published
  copy. A copy that arrives is written apart and made current in one step. A
  plugin never sees half a copy, and a sync that stops changes nothing.
- **Notified over the plugin API, both ways.** The plugin says it has
  published a new copy (`updated`), and the sync then runs for that context.
  When a new copy has arrived, the plugin is told (`received`). Bringing it
  into its own data is the plugin's, when that is safe: the F95 plugin's
  computer side waits until F95Checker is closed, because F95Checker keeps its
  database in memory and writes it back over anything changed under it.
- **Merge is three-way per field**, between the two copies and the baseline
  the handheld keeps per computer and context. If only one side changed a
  field, that change wins. If both changed it differently, the field's rule
  decides; `ask` puts the record in the context's conflict list for the
  person. The merged copy is what each side then receives, atomically.
- **Not part of offers** (section 20): an offer never carries a context.
- **The computer-side plugin is a program the plugin publishes**, one per
  kind of third-party store. The agent carries no third-party format itself
  (coordinator decision, 2026-10-09: the F95Checker adapter belongs with the
  F95 plugin, in gamegrab-sources).

- **How an adapter gets onto the computer** (coordinator, 2026-10-09):
  - **Offered by the plugin.** The plugin's signed manifest declares the
    program in droidtop's `computers.context_adapter` point: the context, and
    one https download per system (`windows-x86_64`, `linux-x86_64`,
    `macos-aarch64`, Rust's OS and arch names), each pinned by SHA-256.
    droidtop sends that offer with the context's `context_pull`.
  - **Approved once, on the computer.** A computer with no adapter for that
    context keeps the offer, says on its console what to run, and answers
    the handheld that it waits for the person. `droidtop-agent contexts
    approve <context>` shows the plugin, the address and the digest, fetches
    the program (64 MiB at most), checks the digest, checks that the program
    says it serves that context, and installs it under the agent's data
    folder (`adapters/<context>/`). Nothing is fetched or run before that.
  - **Later versions follow.** The approval is for that plugin and context:
    an offer from the same plugin with a different digest is fetched and
    checked the same way at the next sync, without asking again. An offer
    from another plugin for that context, or for a context the person set up
    by hand, changes nothing.
  - **By hand.** `droidtop-agent contexts add <program>` still installs one
    the person downloaded; it is never replaced by an offer.
- **Why a program and not a loadable module.** Rust has no stable ABI, so a
  dynamic module would need a C interface, `unsafe` on both sides and a build
  per system matching the agent's own; a crash in it would take the agent
  down with it. A program needs none of that: it can be written in any
  language (F95Checker itself is Python), it is built and released by the
  plugin's own repository on the plugin's own schedule, a fault ends only
  that program, and it is a plain process boundary between two separately
  licensed works.
- **Today (adapter protocol 1) is the old shape**: the agent runs the adapter
  once per pull or push (`describe`, `pull`, `push` over standard input and
  output), and the F95 adapter reads and writes F95Checker's live
  `db.sqlite3`, replying `deferred` while F95Checker runs. Protocol 2 replaces
  it with the copy and the two notifications; protocol 1 adapters stay
  readable until every published one moves.
- **The F95 plugin's computer side** is gamegrab-sources/droidtop-agent-f95-adapter.
  It finds F95Checker's `db.sqlite3` (Windows `%APPDATA%\f95checker`, Linux
  `~/.config/f95checker`, macOS `~/Library/Application Support/f95checker`),
  copies only the declared columns of the `games` table into its context, and
  never the `cookies` table or the settings table's passwords and tokens.
  Its fields:
  - two-way: `installed`, `finished`, `archived`, `rating`, `notes`, and the
    record's presence (watching a thread);
  - computer to device: `name`, `url`, `version`, `developer`, `status`,
    `type`, `last_updated`.
  A thread watched on the device is added to F95Checker with its id, name and
  url when the copy is brought in, and F95Checker's own refresh fills in the
  rest.

- **The notices are plugin API calls on both ends** (owner, 2026-10-23: the
  plugin "is notified, and notifies, over the API"; droidtop's Python plugin
  system is reused unchanged on computers). On the computer the plugin runs
  in the agent's plugin host, confined, and calls the same API through the
  host's broker as it does on the handheld: `updated` is a host call, and
  `received` reaches the plugin as an event. The host, its broker and its
  sandbox are Droidtop/tracker#606 and the computer plugin host's own
  design. Until that host runs on a system, adapter protocol 1 stays the
  way contexts sync there.

## 9. Both sides changed, and many devices

**Saves: nothing is lost.** When both sides changed, the newest (or the
preferred) copy wins without asking (section 6). Before a device's own
changed set is overwritten, it keeps it as its own copy (`savesync::keep_copy`):
- the newest copy from each device, in `archive/<game>/copies/<device name>/`;
- the copy that replaces moves to the dated archive,
  `archive/<game>/<UTC time>/` (with `device.txt`), which keeps the 5 most
  recent per game (decision 8).

That is `<agent data>/archive/` on the computer, and the archive folder
droidtop names for each computer on the handheld. The person can still pick
the other side (`choice`), to bring back a copy: the side that pick
overwrites keeps its own the same way. droidtop says which side's saves were
kept (`kept` in the outcome). Steam Cloud games keep the store's own rule and
its dialog. Save letters in the cloud folder follow the same rule on the
computer: when its saves changed too, the newest or preferred set wins, the
other is kept, and a refusal tells the handheld where its copy is.

**Devices that are not paired directly** reach each other through the
devices they share. The pairing graph is any set of handhelds and computers.
A handheld and a computer exchange what each has, so changes travel along
pairings:
- **Saves:** a second handheld paired with the same computer gets the first
  one's saves at its next sync with it. Each pair keeps its own baseline, so
  a set that arrives through a third device is a change on one side and moves
  without a question. Two devices that both changed meet the newest-copy
  rule wherever they meet.
- **Library:** each device's facts are written only by that device and travel
  through any device in between: the change log is relayed, and a change
  that moves nothing stops. A game installed on two computers is one entry
  with an install per device, so two locations.
- **Plugin contexts:** merged per field against a baseline the handheld keeps
  per computer and context. A field changed on two computers meets the
  field's rule (or the person) on the handheld that carries both.

A context's conflicts are per record and per field, never per file, so
nothing there is overwritten without a rule or an answer.

CI covers this with three handhelds and three computers in one process,
paired in a partial mesh (`crates/android/tests/mesh.rs`):
- the library relaying across the mesh, with one entry per game and two
  locations;
- saves edited on two devices at once (the newest wins, and the other
  device's copy is kept);
- a preferred computer beating a newer copy;
- saves reaching a handheld through a computer it shares;
- an identity move.

## 10. Transports, in order

All of them carry the same TLS channel between paired devices. A relay or
share only ever sees ciphertext.

1. **LAN direct.** TCP to the agent's port (47610). The handheld finds the
   agent by a UDP broadcast query on the same port, which only paired agents
   answer (the reply is signed with the agent's key), and also tries the last
   address that worked. Tailscale, ZeroTier or a plain WireGuard VPN, when the
   person has one, is just another address to try, never a requirement.
2. **Direct WireGuard.** A userspace WireGuard tunnel (boringtun) between the
   two paired keys over UDP, with UDP hole punching for NAT. The tunnel's
   addresses come from the PeerIds (a `fd64:` ULA per device). Inside it, a
   userspace TCP stack (smoltcp) carries the TLS channel, so nothing on
   either device needs a TUN interface, root or a VPN slot. The tunnel exists
   only while a sync runs.
   - **Finding each other's public endpoint:** endpoints are learned
     - from the last direct connection;
     - from small signed announcements (endpoint and key, a few hundred bytes)
       left in the person's own cloud share (transport 3);
     - optionally, from a STUN server the person configures.
   - Syncthing's global discovery and STUN servers carry addresses only
     (decision 4, below); no relay carries data. A droidtop-run discovery
     service on the server VM (#364) can take their place in the settings.
   - **Rendezvous, the way Syncthing does it** (owner, 2026-10-09: "We ARE
     using syncthing's detection and routing implementation"; "there's a
     reason we aren't passing DATA over it"). Syncthing's global discovery
     protocol (v3) and STUN, with Syncthing's default servers as the default
     and a setting for droidtop's own on #364 later; Syncthing's relays are
     never used, and every byte of a sync goes through the direct tunnel.
     - **Discovery ID.** Global discovery names a device by the SHA-256 of
       the TLS client certificate it announces with (Syncthing's device ID,
       base32 with Luhn check characters). Each device's certificate is made
       from a key derived from its seed, with fixed fields and a
       deterministic Ed25519 signature, so the ID never changes and nothing
       more is stored. The two sides tell each other their IDs in `hello`.
     - **The computer** sends STUN binding requests from the WireGuard
       socket itself (Syncthing's STUN list; keepalive every 180 s, down to
       20 s when the NAT forgets sooner, Syncthing's figures). It announces
       `{"addresses": ["wg://<mapped>", forwarded port, global IPv6]}` when
       that changes (after a 5 s settle) and then when `Reannounce-After`
       says (30 minutes by default), never before a `Retry-After`, and 5
       minutes after a failure: Syncthing's own client's pace. It looks its
       paired handhelds up on Syncthing's schedule (a found address is kept
       5 minutes; not found, asked again after a minute or the server's
       `Retry-After`) and sends a one-byte punch to each address found every
       2 seconds while it is fresh.
     - **The handheld**, only when the LAN and the stated endpoints did not
       answer: from one new socket it asks STUN for its own mapped address,
       looks the computer up (a found address kept 5 minutes, a miss not
       asked again before the server's `Retry-After` or a minute; kept in a
       small state file), announces its own address when it changed or the
       server asked, and sends WireGuard handshakes from that socket to the
       computer's addresses for up to 75 s, long enough for the computer's
       next lookup and punches.
     - **What it cannot do:** two NATs that change the port for every
       destination (symmetric NAT) cannot be punched, and a handheld the
       discovery server has never seen may be asked about only after the
       server's adaptive `Retry-After`, so a first contact can take longer
       than one sync; the next one finds the cached address.
     - Settings on the computer: `droidtop-agent rendezvous on|off`,
       `rendezvous servers default|<url>...`, `rendezvous stun
       default|<host:port>...`; on droidtop, Settings > Computers.
   - **How it works now.** Each time they meet, the computer's `hello` reply
     tells the handheld its WireGuard endpoints (`wg:<ip>:<port>`):
     - the endpoint the person forwarded on their router
       (`droidtop-agent endpoint set`);
     - the computer's global IPv6 addresses (no NAT in the way, only a
       firewall that has to let UDP 47611 in).

     Away from the LAN, the handheld sends the handshake to all of them at
     once and keeps the one that answers. The computer answers only keys it
     has pinned. Through two NATs, the rendezvous below punches the hole.
   - droidtop keeps the endpoints from the last `hello` with the computer
     and passes them with its LAN addresses on every call; a call tries the
     LAN addresses, then the LAN broadcast, then the endpoints. A network
     without IPv6 tries only the IPv4 ones. An address reached through the
     tunnel is not remembered as a LAN address.
3. **The person's own cloud share, store and forward.** A folder that the
   person's own sync tool already carries to both devices: Google Drive,
   OneDrive or Dropbox clients, Nextcloud, a Syncthing folder they already run,
   or Syncthing-Fork or FolderSync on the handheld. Each device writes sealed
   messages for a peer into `droidtop-agent/<peer id>/inbox/`. Each message
   is sealed with X25519 between the two identities and ChaCha20-Poly1305,
   and holds save files, library changes or context changes. The other side
   applies and deletes them when they arrive. This is the path when the two
   are never online at the same time.
   - **Library letters** carry the changes the recipient has not had, by the
     same cursor as a live exchange. The agent leaves them once a minute; the
     handheld leaves its own and opens the computer's when a library sync
     finds the computer away.
   - **Save letters** go one way, handheld to computer, after a game ends
     with the computer away. A letter holds the whole save set and the set
     it was made against: the live baseline, or the set posted before it
     when no live sync has settled that one yet. The computer applies it only
     while its own files still match that set (or already equal the letter),
     and otherwise keeps it in its archive and answers with a refusal the
     handheld shows. The handheld learns where a game's saves are only from
     a live sync (it keeps the computer's answer beside the baseline), so a
     game never synced live is not posted.
   - **droidtop reaches the share through Android's document picker**, which
     the core cannot open. It keeps two folders of its own with the share's
     layout: one it fills from the share before the call (`inbox`) and one
     the core writes into and droidtop empties into the share after it
     (`outbox`). The core seals, opens and applies; droidtop only moves
     files, and deletes from the share only letters the core has opened.

   A WebDAV share, reached directly by
   both, is the next backend. Credentials for any share are the person's own,
   entered through droidtop's in-app sign-in helper and stored with
   `KeystoreSecretCipher`; nothing that authenticates ships in either program.
4. **A droidtop-run relay, later**, on the server VM (#364): a DERP-style
   relay that forwards ciphertext between two keys. Only as a fallback, and
   never a community-run one.

Bulk traffic never goes through anyone else's relay.

## 11. The computer's scanner

`droidtop-agent scan` (and the running agent, on a timer and when asked)
builds the computer's library from what is installed. It reads files only:
no store APIs and no network.

| Source | Windows | Linux | macOS |
|---|---|---|---|
| Steam | registry `HKCU\Software\Valve\Steam\SteamPath`; `steamapps/libraryfolders.vdf`, `appmanifest_*.acf` | `~/.steam/steam`, `~/.local/share/Steam`, Flatpak, Snap; a game's Proton prefix | `~/Library/Application Support/Steam` |
| GOG | registry `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\*` | Heroic `gog_store/installed.json` | Heroic, registry-less GOG Galaxy installs under `/Applications` |
| Epic | `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests\*.item` | Heroic and legendary `installed.json` | same as Linux |
| Amazon | `%LOCALAPPDATA%\Amazon Games\Data\Games\Sql\GameInstallInfo.sqlite` | Heroic nile `installed.json` | none |
| itch | butler's `db/butler.db` (caves joined with games) | same | same |
| Battle.net | uninstall registry entries published by Blizzard | Lutris | none |
| Lutris | none | `pga.db` (native and Flatpak); a Wine game's prefix from `games/<configpath>.yml` | none |
| Heroic prefixes | none | `GamesConfig/<app>.json` `winePrefix`, for its GOG, Epic and Amazon games | same |
| Bottles | none | `library.yml`: each program the person put in the library, with its bottle as the prefix (native and Flatpak) | none |
| Minigalaxy | none | `config.json` `install_dir`; each game's `gameinfo` (title, version) or `goggame-<id>.info` | none |
| Folders | user-chosen roots plus a `Games` folder at the top of each fixed drive, walked with droidtop's rules (below) | user-chosen roots plus `~/Games` | same, `.app` bundles too |
| ROMs | user-chosen roots laid out by ES-DE system name; ES-DE's own `es_settings.xml` ROM folder and `gamelists/`; system folders met in a game folder | same | same |

**Game folders are walked the way droidtop walks them** (droidtop SPEC 7g,
7i, 7m: `GameEngineDetector.scan` and `PcFolderScan`), so the computer and
the handheld find the same games in the same tree. Engines are decided by
droidtop-platforms' `engines-database.json`, which the agent embeds
(`crates/scan/data/`, refreshed by copying the file in) and evaluates row by
row, every row, with droidtop's three byte probes (Godot's pack, a Twine page,
Unity's player). A folder with an engine's layout or a program is a game and
ends the walk; a folder holding two engine games is a collection; a lone game
below a version or payload folder takes that folder's name; a folder with its
own files and PC games below is their game; part and version folders cost no
depth, anything else stops four folders down. A Steam library met on the way
is read from its appmanifests (a folder with no manifest is a leftover), a
folder named for a ROM system (`platforms-database.json`, droidtop's aliases)
is read by the ROM rules when it holds that system's files, and a Flashpoint
install is its launcher plus the games it downloaded. A launcher's own
folder is no game and is not walked (Droidtop/tracker#538: Pythia's scan of
G:\games tracked `battle.net`): a folder named for Battle.net, the EA app
(`EA Desktop`), GOG Galaxy or Ubisoft Connect, one holding a program only a
launcher has (`Battle.net.exe`, `EADesktop.exe`, `Origin.exe`,
`GalaxyClient.exe`, `upc.exe`, `EpicGamesLauncher.exe`), and Epic's
`Launcher` inside `Epic Games`. Their games come from the launchers' own
records, and a store tree's games folder (GOG Galaxy's `Games`) is still
read. `folders::launcher_folder` is the rule, which a desktop manager asks
too (section 16), so both leave out the same folders. Titles drop the version
and platform a download adds (`Eternum-0.9.5-pc` is Eternum 0.9.5). The
`Games` folders are walked unasked because a library kept outside the stores
is otherwise invisible: G:\games held 189 games, the stores' records 76.
| Emulators | RetroArch, Dolphin, PCSX2, DuckStation, PPSSPP, RPCS3, Cemu, Ryujinx and others, detected for their save folders | same | same |

**Installed applications** (owner, 2026-10-08: "we need to track third party
apps and stuff too"). Everything installed on the computer, games or not, is
in the library too, as entries of the platform `app` keyed
`app:<source>:<id>`. It travels in the same change log as the games, with
the same marks. The agent reads only what each system keeps about its
installs:

| System | Read from | Key |
|---|---|---|
| Windows | the uninstall registry: HKLM in both views, and HKCU. It leaves out system components, entries with a parent, updates, runtimes and redistributables, and "Steam App <id>" | `app:win:<entry key>` |
| Windows | Store (MSIX/AppX) packages registered for the user: each package's `AppxManifest.xml`, leaving out frameworks, resource packages, packages with no Start entry, and Windows' own `SystemApps` | `app:msix:<package name>` |
| Linux | XDG desktop entries in `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, plus Flatpak's and Snap's exports. The first file with a desktop id wins, and `NoDisplay`, `Hidden` and launchers' game shortcuts are left out | `app:flatpak:<app id>`, `app:snap:<snap>`, `app:desktop:<desktop id>` |
| macOS | `.app` bundles in `/Applications` (and its subfolders one level down), `/System/Applications` and `~/Applications`, from `Info.plist` | `app:mac:<bundle id>` |

An application inside a game folder the scan found is that game, and is left
out. droidtop lists a computer's applications beside its games ("Apps on
<computer>"). Whether they also join the handheld's own Apps place is
decision 6.

**Programs droidtop syncs with:** F95Checker (`db.sqlite3`), Lutris (`pga.db`), Heroic
(JSON), ES-DE (gamelists), and Ludusavi's own config if present (custom games
and paths the person already set up). Playnite keeps its library in LiteDB, for
which there is no maintained Rust reader, so v1 only detects it.

The scan runs in the agent's own process, off any UI. It keeps no handles open
and skips folders it cannot read.

## 12. The droidtop side

- **No resident process.** droidtop connects only:
  - before a game launches and after it exits (the `StoreSaves` seam);
  - when the person starts a sync or a transfer (Settings > Computers >
    "Sync now", or a game's menu);
  - while the pairing screen is open;
  - while droidtop serves the handhelds paired with it (`serve_start` to
    `serve_stop`), the one thing that listens, and only while droidtop asks.
  Nothing polls.
- **The operations** (`AgentNative.call(op, json)`, `crates/android/src/api.rs`).
  Every one takes the device's `seed`; the ones that reach a paired device
  also take its `peer` id, its known `addresses`, `name`, and for the
  computer path `disco` and `rendezvous`. A failure is `{"error"}`, a device
  that did not answer `{"unreachable"}`.

  | Op | Arguments | Reply |
  |---|---|---|
  | `identity_new` | | `{seed, id, fingerprint}` |
  | `identity_id` | `seed` | `{id, fingerprint}` |
  | `pair_start` | `seed, name, kind?, address?` | `{code, uri, port}`: this device shows the code |
  | `pair_wait` | `timeout_ms?` | `{peer, name, kind, fingerprint, address}` |
  | `pair_connect` | `seed, name, kind?, code, address?` | the same: this device typed the other's code |
  | `pair_cancel` | | `{}` |
  | `announce` | `peer, addresses, values` | `{announced, computer, path, ...}`: the other device's values |
  | `serve_start` | `seed, name, peers, values, state, port?` | `{port}`: serve the handhelds in `peers` |
  | `serve_stop` | | `{}` |
  | `find`, `sync_saves`, `sync_library`, `sync_context`, `game_pull`, `game_push`, `launch_game`, `share_library`, `share_post_saves` | as before | as before |

  `kind` is `handheld` (the default) or `computer`; `fingerprint` is the
  paired device's certificate pin (section 3), and `peer` its id, which
  droidtop keeps as before. `state` is a JSON file the serving handheld
  writes: by peer id, `{values, ms}`, what each announced and when.
- **Where the code is:**
  - `:net-core`, `dev.droidtop.net.peer`: the JNI binding to the agent core
    (`libdroidtop_agent.so`) and the sealed identity;
  - `:library-core`, `dev.droidtop.library.computers`: paired computers, the
    save sync beside `StoreSaves`, and library and context sync;
  - `:app`: the Computers settings catalog and the pairing screen (code and QR);
  - droidtop's own next step (Droidtop/tracker#556): paired devices with a
    kind, the handheld-to-handheld pairing screen, and the plugin API's
    `devices.list_paired`, `devices.announce` and `devices.changed` (#553)
    over the operations above.
- **The native library is built here**, in this repository's CI, for
  arm64-v8a and x86_64, and published with each release. droidtop fetches it
  against a pin file (tag and SHA-256), the way it fetches the lsfg-vk layer.
  So droidtop's CI needs no Rust toolchain, and the core has one build.

## 13. Repository layout and builds

The computer's side is a set of library crates, so that a desktop manager
that is this computer's agent (Pythia, section 16) is built from the same
code as the programs (owner, 2026-10-14: "We will modularize the desktop
agent, and use its modules as part of pythia"). A lower crate never depends
on a higher one. Every crate is `droidtop-agent-<name>`, its library
`droidtop_agent_<name>`. The design went through a two-round society review
on Droidtop/tracker#461.

| Crate | What it is | Depends on |
|---|---|---|
| `crates/core` | Both ends: identity and its certificate, the pairing exchange, the TLS channel, the protocol and the `Host` trait the server answers through, the transport primitives (WireGuard tunnel, LAN discovery, rendezvous client, cloud-share letters), the library model, the save and context sync rules, game copy. No UI and no platform-specific code. | windowcast's crates |
| `crates/store` | This computer's agent state on disk, `Store`: `Dirs`, `Settings`, the identity shared with windowcast and the migration onto it, the trusted and device lists, `library.json` under its cross-process lock, the person's marks. No networking. | core |
| `crates/scan` | The scanner (section 11), over `scan::Options` (game and ROM folders, what is switched off), never the agent's state. The engines and platforms databases are in `crates/scan/data`. | core |
| `crates/saves` | Where a game's saves are on this computer (the person's entries, Ludusavi custom games, the Ludusavi index, token roots natively or in a prefix), the kept-copies listing and restore (sections 6 and 9). Looks games up in a scan it is given. | core, scan, store |
| `crates/pair` | Pairing from the computer: typing the handheld's code, or showing one (section 3). | core, store |
| `crates/contexts` | Context adapters: offers, approval, install, runs (section 8). | core, store |
| `crates/service` | The running agent, `Agent`: a `Store` plus the last scan, the save catalog, the rendezvous's state; its `Host` answers (with the launch rule, section 17); serving (LAN, discovery, WireGuard), the computer's rendezvous, the cloud share, a desktop manager's games, autostart. | all above |
| `crates/agent` | The `droidtop-agent` command line: a thin host over the crates. | service and below |
| `crates/app` | `droidtop-agent-app`, the window and tray (section 15): what people run. | service and below |
| `crates/android` | droidtop's `cdylib` (`libdroidtop_agent.so`) and its JNI surface. | core |
| `crates/python` | `droidtop_agent`, the Python extension module a desktop manager loads (section 16). | service and below |

- Why core stays one crate: both ends link all of it, its modules meet in
  the protocol (a request names library changes, save specs, game copy and
  context types), and splitting it would change droidtop's JNI build for
  nothing a desktop manager needs. Game copy stays in it because both ends
  run it. The transports are core's primitives plus the service's wiring.
- The service's `Agent` holds its `Store` as `agent.store`; pairing and the
  adapters take `&Store`, so they work without a running service.
- CI (`.github/workflows/ci.yml`): build and test on Linux (x86_64 and
  aarch64), Windows and macOS (arm64, with the x86_64 build made beside it),
  plus the Android library for both ABIs. Each system's release has both
  programs; macOS also gets `droidtop-agent.app` (`packaging/macos`). Each green build on `main`
  publishes a release (the computer binaries and the Android library) and adds
  a CHANGELOG entry. Releases are permanent history.
  Linux is also packed four ways (`packaging/linux`, built by the
  `linux-packages` job and attached to each release with `install.sh`,
  `uninstall.sh` and `SHA256SUMS`; owner, 2026-10-10): a portable
  `.tar.zst`, one runnable file (uruntime's RunImage squashfs runtime plus a
  squashfs image; never AppImage or its tools), a `.deb` and an `.rpm` (nfpm).
  No Flatpak: sandboxing a resident monitoring program would defeat its
  purpose. `install.sh` installs per user without root, lists everything it
  makes in `~/.local/share/droidtop-agent/install-manifest`, and
  `uninstall.sh` removes exactly that, stops the agent and its autostart, and
  keeps the person's data unless `--purge`. The packages ship the systemd
  user unit disabled. The downloaded tools (nfpm, uruntime) are pinned by
  version and SHA-256 in `packaging/linux/pins.env`.

## 14. Decisions for the owner

1. **Licence.** The agent is GPL-3.0-only because it links windowcast's
   GPL-3.0-only `identity` and `pairing` crates. Relicense those two crates as
   MIT/Apache-2.0 so the agent can be Apache-2.0? Default: stay GPL-3.0-only.
2. **Pairing input.** The code is typed on the computer, and the QR code
   serves cameras (laptop webcam, phone). Decided (coordinator, 2026-10-09):
   the computer can also show the code and the handheld connect (section 3).
3. **One identity with windowcast on a computer.** Decided (owner, 2026-10-10):
   one key file, one trusted list, one pairing exchange (section 3).
4. **Finding endpoints for WireGuard off the LAN.** Default: last-known
   endpoints, plus signed announcements in the person's own cloud share. Should
   a STUN server or Syncthing's global discovery also be used (each needs its
   usage policy read first), or should we wait for droidtop's own discovery on
   #364? Decided (owner, 2026-10-09): Syncthing's global discovery and STUN
   servers by default, addresses only, at Syncthing's own client's pace;
   droidtop's own server (#364) can replace them in the settings (section 10).
5. **Store cloud and agent on the same game.** Default: the store's cloud wins
   and the agent skips that game's saves.
6. **The computer's games in droidtop's library.** Default: listed under the
   computer in Settings > Computers and on its own Places row. Should they
   also merge into the main library as "on <computer>" entries next to local
   copies?
7. **Writing F95Checker's database.** Default: only while F95Checker is
   closed; changes wait until then.
8. **Archive depth.** Decided (owner, 2026-10-10): the newest copy from each
   device, plus the 5 most recent older copies per game (section 9).
9. **Autostart on the computer.** Default: off. `droidtop-agent autostart
   on` (or the checkbox in the window) adds a per-user autostart: a systemd
   user unit on Linux (tied to the graphical session for the window
   program), an XDG autostart entry where the session has no systemd user
   instance, a LaunchAgent on macOS, and the per-user Run key on Windows.
   None needs an administrator. It starts `droidtop-agent-app` when that is
   installed beside the agent, else `droidtop-agent run`.

## 15. The window and the tray

The owner's ask (2026-10-10): a tray app and window on Windows, Linux and
macOS, so nobody needs the command line.

- **One agent, any number of front ends.** `droidtop-agent-app` runs the same
  service as `droidtop-agent run`, in its own process. If another copy of the
  agent already listens, the window attaches to it: it does not serve, says
  that the running agent serves, and shows and changes the same files.
  Copies of the agent in different processes (the headless one, the window,
  a command line) each hold the paired devices and the library in memory, so
  they never write a copy back that they read earlier: `devices.json` and
  `library.json` are read again when their file changed (`Store::devices_now`,
  `Store::library_now`) and every change is read, changed and written under a
  lock file (`edit_devices`, `edit_library`). A copy that wrote its start-up
  state back used to empty the paired devices and drop marks
  (Droidtop/tracker#528, #531).
- **The stack is windowcast's reference app's:** egui through eframe (OpenGL
  via glow). The tray uses `tray-icon` on Windows and macOS. On Linux it is
  a StatusNotifierItem over D-Bus (`ksni`, pure Rust), so the Linux build
  needs no GTK. KDE, most other desktops, and GNOME with the AppIndicator
  extension show it. Where no tray answers, closing the window quits, so
  the agent is never left running out of reach. The folder picker is the
  system's own (`rfd`; the XDG portal on Linux).
- **Pages:**
  - **Status:** whether it serves, the firewall hint (TCP and UDP 47610 at
    home, UDP 47611 away), the paired handhelds with when each was last seen
    (each can be forgotten), and how it is reached away from home.
  - **Pair a handheld:** type the code the handheld shows, or "Show a code":
    this computer listens (TCP 47612) and shows its addresses, the code and
    a QR code of the same invitation. Stop ends it.
  - **Library:** every place games are found, each with its game count and
    a switch: the stores and launchers the scan reads (only those it found
    something in, and any the person switched off), the game folders found on
    their own (a `Games` folder at the top of each fixed drive) and the ones
    the person added (Add a game folder opens the system's folder picker;
    Remove), and ROM folders. A switched-off place is skipped by the scan
    (`Settings::disabled_sources`, `disabled_folders`), so its games leave
    the library on every handheld too. Below: the games with a search, each
    with its source and location; picking one shows its favourite and hidden
    marks and its collections, which the person can change here (they travel
    like marks made on a handheld, section 7; `droidtop-agent mark` does the
    same from a command line). Then the programs droidtop can sync with and
    the sources that could not be read.
  - **Saves:** which device's saves win when both changed (the newest, or a
    named handheld's); a search for a game; and for the picked game where its
    saves are (detected or stated by the person, with the files found now),
    adding or removing a location, and the copies kept on this computer
    (the newest from each device with its date, then the dated older ones,
    section 9). Each can be restored (what is there now is first kept as this
    computer's own copy, so a restore can be undone) or opened in the file
    manager. An older copy kept after both sides changed is marked "to look
    at" until the person restores one or says they have looked (`reviewed.txt`
    in the game's archive folder). Games are shown by title, never by key.
  - **Plugin data:** adapters plugins offered (Install or Decline; a declined
    plugin's offer for that context is not kept again) and the ones
    installed (Remove).
  - **Settings:** name, rescan interval, which handhelds may start games
    (section 17), away from home, forwarded port, cloud folder, game and ROM
    folders, and starting at sign-in.
- **Behaviour.** Closing the window leaves the agent in the tray. The tray
  menu offers Open, Pair a handheld and Quit. Autostart starts it with
  `--hidden`, in the tray only. Slow work (scanning, pairing, fetching an
  adapter) runs on threads of its own. Files are read when a page opens and
  every few seconds after that, never on every frame.

## 16. Inside a desktop manager (Pythia)

Owner, 2026-10-14: "pythia will NEVER call droidtop-agent. If it's running,
it will BE our agent", and "We can step away from python whenever we want".
[Pythia](https://github.com/bi0shacker001/pythia), the owner's desktop game
library manager (largely based on Lutris), is this computer's agent while it
runs. It does not speak to a separate agent program. It loads the agent
itself, in its own process, so there is still one implementation of the
identity, pairing, channel, transports, library, saves and scanner.

- **`crates/python`** builds `droidtop_agent`, a Python extension module
  (PyO3, the stable ABI from Python 3.10 on), over the same crates and the
  same files as `droidtop-agent run` (section 3; `DROIDTOP_AGENT_HOME` works
  the same). Pythia is built from these modules (owner, 2026-10-14: "We
  will modularize the desktop agent, and use its modules as part of
  pythia"; section 13).
  - **One extension module, one submodule per crate**:
    `droidtop_agent.store`, `.pair`, `.scan`, `.saves` and `.service`, each
    importable by that name. Not one module per crate: each extension
    module links its own copy of the Rust code, and one process must hold
    one agent.
  - **Typed functions:** keyword arguments in; dicts, lists and plain values
    out; a refusal raises `droidtop_agent.AgentError` with words a screen
    can show. Every call releases the interpreter while Rust works, and a
    function's docstring says when it is slow (a scan, a pairing wait, a
    save lookup that may rescan).
    - `store`: `identity` (with its certificate's `fingerprint`), `settings`,
      `set_settings(**changes)`, `devices` (each with its `fingerprint`,
      `kind` and what it `announced`), `unpair(id)`, `set_primary(id=None)`,
      `set_launch(id, allowed)`, `library`, `set_mark(game, field, value)`,
      `announcements`, `set_announce(plugin, value=None)` (section 5)
    - `pair`: `pair(code)` (the handheld shows the code), `show` / `status`
      / `cancel` (this computer shows one: the code, every address with its
      label, and the invitation a QR code carries), `places`
    - `scan`: `scan(max_age_s=0)`, `rescan`, `set_manager_games(manager,
      games, rescan=True)`, `scan_with(game_folders=..., rom_folders=...,
      ...)` (the scanner alone, without the agent's state),
      `launcher_folder(path)` (the launcher whose own folder it is, or
      None: section 11)
    - `saves`: `lookup(key, title="", fetch=False)`, `copies`,
      `restore(key, path, title="")`
    - `service`: `serve`, `serving`, `events(since=0, wait=0)`
- **One agent per computer.** `serve` starts the same service as
  `droidtop-agent run`, on threads of its own. If another agent already
  listens on port 47610 (droidtop-agent, its window, or another Pythia),
  `serve` fails with that reason and the other one keeps serving.
- **Taking the agent role loses nothing** (owner, 2026-10-16: "pythia
  imports the droidtop agent context when it takes the agent role, too, so
  data shouldn't be lost"). Pythia keeps its state in a folder of its own
  and, before it serves, turns this program off and takes in its state,
  reading this program's folders and writing only its own (Pythia
  docs/SPEC.md 1.8; `pythia._native.agent.sync`):
  - the identity, its device certificate and the trusted devices are
    shared in windowcast's host folder; an older agent's own
    `identity.key` and `trusted-devices` are taken before Pythia opens its
    state, so `store::migrate` makes them this computer's identity;
  - `devices.json`, `settings.json` (with the adapters' approvals),
    `saves.json`, `manager-games.json`, `launcher-marks.json` and
    `adapter-offers.json`: the first time, every one this program has
    comes in whatever its age, the paired devices merged with any Pythia
    has; after that the newer copy wins;
  - the library: both change logs are merged;
  - the archive's save copies and the `adapters` folder: a file Pythia
    lacks is copied, none is replaced or deleted.

  While this program is off, Pythia writes its state back into this
  program's folders, so turning it back on finds the same pairings,
  library and save copies.
- **Events for the host.** What the service does is an event, not only a
  printed line, so the program hosting it can log and show it (a handheld
  asking to start a game, a session ending or refused, saves and library
  changes synced, a game folder arriving, a transport stopping).
  `droidtop_agent_service::events` keeps the last 500, numbered, on the
  `Agent`; each has `seq`, `ms`, `level` (`info`, `warn`), `kind`, the
  `message` the command line prints (each event is still printed, so
  `droidtop-agent run`'s output is unchanged), and what it is about by name
  (`peer`, `device`, `title`, `key`...). A host reads from the last `seq`
  it saw; `events(since, wait)` waits up to `wait` seconds for one without
  holding the interpreter. Read, not pushed: a Python callback on the
  agent's threads would take the interpreter in the middle of a session.
  Save syncs are reported once per game at the end of a session, through
  core's `Host::saves_synced`, since a handheld's sync is many requests.
- **The manager's own games** (`set_manager_games`): the games Pythia
  tracks, as library records with their folders, are kept in the agent's
  data folder (`manager-games.json`, with the manager's name) and joined
  into every rescan, whichever program serves; so closing Pythia does not
  take its games off the handhelds. A game the scanner already found, by
  key or by folder, keeps the scanner's record. The rest become this
  computer's install facts like any other source's, and their saves are
  found the same way (section 6). A handheld can copy a library game's
  folder (`game_pull`), so a manager game's folder is taken only when it
  exists and is not a whole drive, the home folder or a folder holding it,
  or a folder holding the agent's or windowcast's own files.
- **Wheels** come from CI's `python` job for Windows x86_64, Linux x86_64
  and aarch64 (glibc 2.39, like the programs) and macOS arm64. Each is
  installed, every submodule imported and called once, before it is
  offered. They are attached to each release beside the programs.
- **Pythia's better logic moves into the crates**, each its own change,
  recorded on Droidtop/tracker#461, in this order:
  1. cross-store identity: Pythia's opaque game id, its `platform_ids`
     aliases and the merges a person confirmed, as a library change kind in
     core (both ends keep the same identity model), behind a hello feature;
  2. ownership of store games that are not installed (owner, 2026-10-14),
     another library fact, written by Pythia's account sync through `store`;
  3. reconciliation's duplicate search (Pythia's difflib pass over every
     entry per root) as a `scan` function with droidtop's part-folder rule,
     growing with the library rather than its square;
  4. Pythia's store scanners give way to `scan`; account-level ownership
     stays Pythia's (it needs the network; the scanner reads files only);
  5. credentials encrypted at rest on each device and carried inside the
     channel (owner, 2026-10-14), a protocol change.
- **Later:** a small record of which program serves, so the other can name
  it ("Pythia serves this computer").

## 17. Starting a game for a handheld

Owner (2026-10-14, Droidtop/tracker#552): "Steam link streaming is actually
a pretty huge gap for us", and, approving the agent request and the Steam
module as its home: "Yep. Wire it into the steam module specifically,
because that controls how we handle steam games. Simplifies stuff".

Steam Link takes no link that picks a computer or a game (rig run on a
Retroid Pocket 5, Steam Link 1.3.32: every `steamlink://`, connect and
remote play link ends in "Couldn't locate Remote Play invitation"; a plain
launch shows the paired computer and its games). So the handheld asks the
computer to start the game, then opens Steam Link plainly. The start is
this request.

- **Request** `launch_game {game}`, advertised as hello feature `launch`.
  A handheld sends it only to an agent that states the feature, so an older
  agent is told apart without being sent a request it cannot read.
- **The agent builds what it runs.** From `steam:<digits>` (one to ten ASCII
  digits) it makes `steam://rungameid/<digits>` and gives that to the
  system's URL handler (`rundll32 url.dll,FileProtocolHandler` on Windows,
  `open` on macOS, `xdg-open` elsewhere). Any other key is refused. A string
  from the handheld is never executed, and the title is only logged.
- **A switch per handheld, off by default.** `Settings::launch_devices`
  holds the ids of the handhelds the person allowed: a checkbox per paired
  handheld in Settings ("Start games"), or `droidtop-agent launch allow
  <id>` and `launch deny <id>`. It is read again at each request, so a
  change in the window or a command line holds at once. Starting a program
  is more than syncing, so pairing alone does not grant it.
- **Typed results**, one per request (`Launch`): `started`, `not_allowed`,
  `unknown_game` (a key it cannot map, or a game this computer's scan does
  not list as installed), `steam_not_running` and `failed {message}`. They
  are checked in that order for what a handheld may learn: one that is not
  allowed learns nothing about the computer's games.
- **Steam must already run.** With the client closed nothing is started and
  the result is `steam_not_running`: opening the URL would only start the
  client, possibly behind a sign-in, and Steam Link could not stream yet.
  `started` means Steam was asked; it may still be updating or syncing.
  A signed-out client is not detected.
- The agent logs each request with the handheld's name and the result.
- On the handheld the Android library's `launch_game` operation connects as
  for any sync, checks the `launch` feature (`agent_too_old` otherwise) and
  returns `{"result": ...}` with the usual `computer` and `path`.
- No streaming code is here (windowcast is the streaming project); the
  computer only starts the game that Steam Link, or windowcast, then streams.

### Games the host starts (`launch_host`)

Owner (2026-10-16): the agent launching only for Steam Link is the Steam
case; "given windowcast, yeah, have it be able to do general launches".
A program that hosts the agent's service in its own process and keeps a
library of its own (Pythia) can start those games for a handheld.

- **The host registers a `Launcher`** (droidtop-agent-service
  `launch::Launcher`, `Agent::launcher`). The hello then also states feature
  `launch_host` (the core's `Host::extra_features`), so a handheld can offer
  "start on the computer" for games that are not Steam's.
- **The same order and switch.** The per-handheld switch is checked first,
  so a handheld that is not allowed learns nothing and the host is not
  asked. `steam:` keys stay Steam's, exactly as above; any other key goes to
  the host's launcher, which finds the key among its own games and starts
  that game its own way; `None` (not one of its games) is `unknown_game`.
  As for Steam, no text from the handheld is ever run: the key is only
  looked up.
- **`started` says how to show it.** `Launch::Started` carries an optional
  `stream`: `"windowcast"` when the host started a game whose window
  windowcast streams; absent for Steam, whose reply is unchanged on the
  wire. A handheld that does not know the field reads `started` as before.
- The command-line agent and its window register no launcher: they state
  no `launch_host` and answer as before.

## 18. Syncthing with the paired devices

droidtop's Syncthing plugin (`droidtop.syncthing`, Droidtop/tracker#553;
droidtop docs/plugin-api.md D21) pairs through droidtop's own device
pairing, so Syncthing's pairing is never shown: it announces
`{"syncthing": "<device ID>", "addresses": [...]}` to every paired device
and accepts exactly the IDs its counterparts announce. Its README names
droidtop-agent and Pythia as counterparts announcing the same shape under
the same plugin id. This is that counterpart, in the service
(`droidtop_agent_service::syncthing`), so the agent and Pythia both have it.

- **Which Syncthing** (owner, 2026-10-16: "We control an existing one if it
  exists, or spawn our own if one doesn't. We use the standard syncthing
  config location regardless, to make sure the user can switch to or from a
  dedicated instance if they want"):
  - the home is Syncthing's standard one: `STHOMEDIR` when set; else
    `%LOCALAPPDATA%\Syncthing` on Windows, `~/Library/Application
    Support/Syncthing` on macOS, and on Linux `$XDG_STATE_HOME/syncthing`
    unless an older `~/.config/syncthing` holds a configuration;
  - a Syncthing already answering there is used as it is;
  - otherwise the agent starts one over that home (`syncthing serve
    --no-browser --no-restart --home=<home>`: `DROIDTOP_SYNCTHING`, one
    beside the agent's program, or one on the PATH), which makes the home
    the first time. A pass starts it again if it stopped. With no Syncthing
    program, the agent says so once and does nothing more.
  - It reaches Syncthing's REST API on loopback with the API key from its
    `config.xml`. A web interface on HTTPS or a Unix socket is not reached
    yet, and the agent says so.
- **Announcing:** its device ID (from `cert.pem`, as Syncthing computes it)
  and this computer's LAN addresses on its listening port, as `tcp://` and
  `quic://`, through `store.set_announce`, only when the value changed.
- **Accepting:** every 30 seconds, and from each trusted paired device's
  announced value, each valid ID other than its own is added through
  `PUT /rest/config/devices/<id>` over Syncthing's device defaults: named
  as the person named the device, with the announced addresses plus
  `dynamic`, trusted, not an introducer, and with its folders accepted
  automatically (so the plugin's `droidtop` folder, the same ID
  everywhere, is accepted at Syncthing's default folder path). A device
  forgotten, or no longer announcing, is removed again.
- **Only its own:** the devices it added are kept in `syncthing.json` in the
  data folder, and only those are ever changed or removed. A device the
  person added to Syncthing themselves is left as they set it, even when a
  paired device announces it.
- **Off:** `droidtop-agent syncthing off` (the `syncthing` setting, on by
  default) withdraws the announcement and changes nothing in Syncthing.

## 19. Service mode: Windows security screens

windowcast's secure-screen helper (windowcast#467, windowcast
docs/SECURITY.md, "Secure screen threat model") is a small fixed-purpose
program that runs as SYSTEM so the Windows security screens ordinary
programs cannot see (User Account Control prompts, the lock and sign-in
screen) can be shown and answered from a paired device. It is part of the
windowcast library; each application that wants it asks the person, then
installs and enables it through windowcast's client API
(`windowcast_secure_helper::Client`). This agent is one such application,
and Pythia, built from these crates, is another
(`droidtop_agent_service::secure_screen`).

- **Asked, never assumed** (owner, on #467: "We won't just run code as
  SYSTEM without user approval, we'll ask, and let them enable/disable in
  settings at their own leisure"): service mode is off on every install.
  Turning it on first shows what it does (`secure_screen::CONSENT`): the
  helper is installed in Program Files and runs as SYSTEM; it only shows the
  security screen, types and clicks on it, and says whether one is open; it
  answers only installed programs running under the person's own Windows
  account; and Windows asks for administrator approval to turn it on and
  again to turn it off.
- **Every change through UAC:** the unelevated program reads the person's
  account SID, then asks Windows to run itself again with the `runas` verb
  (`droidtop-agent service-mode <enable|disable|uninstall> --confirmed
  [--grant-sid <sid>]`), in `System32`, and waits for its exit code. That
  elevated copy does only the change, through windowcast's API: enable
  installs the helper from beside the program into Program Files when it is
  not there yet and grants the permission to that SID; disable removes the
  permission and the helper's SYSTEM task; uninstall also removes the
  helper. It runs before the agent opens any of its state, so it never
  writes an administrator's profile. Declining the UAC prompt changes
  nothing and says so.
- **Where:** `droidtop-agent service-mode [enable | disable | uninstall]`
  and Settings > Service mode in the window (on, off, or "Remove the
  helper"); off Windows it says it is only on Windows and does nothing.
- **The helper itself** is windowcast's own program at the windowcast this
  workspace pins, built by CI's Windows job (`cargo install --locked`) and
  released beside the agent as `windowcast-secure-helper-windows-x86_64.exe`;
  the agent finds it beside itself under that name or as
  `windowcast-secure-helper.exe`.
- **Not yet:** using the helper. Showing and answering a security screen
  from a handheld needs the screen-sharing side, which is windowcast's; this
  section covers only the person's switch.

## 20. Offers: what one device shares with another

Owner (2026-10-22): two paired computers "don't share anything by
default. The rest should be controlled by the settings menu." And, on the
shape (2026-10-23): "consoles advertise what they can offer, but the
sharing is requested. A console host can OFFER its game library. A console
client can then ACCEPT a specific game for transfer, or TELL the console to
play it." An offer goes "to ANOTHER device, not ALL devices"; "saves should
be assumed to transfer, but we can probably wire up a special 'saves too'
toggle"; ongoing sync "CAN be"; handhelds use the same model.

Pairing makes two devices trust each other and nothing more. What one
gives the other is an offer, made by the giving side, to one device.

- **Two sides.** The *host* is the device whose games they are; it offers.
  The *client* is the device that was offered; it browses, then accepts a
  game or asks the host to play it. Every pair of devices has two
  directions, each with its own offer: A offering to B says nothing about
  B offering to A.
- **What an offer holds.** Per device, in `Settings::offers` (by device
  id), each off unless set:
  - `library`: the client may read this device's library (`library_pull`)
    and copy any game in it (`game_files`, `game_file_get`);
  - `saves`: the client may read and write the saves of those games
    (`save_spec`, `save_manifest`, `file_get`, `file_put`, `save_apply`).
    It needs `library`;
  - `launch`: the client may ask this device to start a game
    (`launch_game`). This is `launch_devices` (section 17), kept as it is
    and now listed with the rest, because starting a program is more than
    sharing files.
- **Advertised per device.** The hello this host answers states, after the
  core's features, what it offers *to the device that asked*:
  `offers_library` and `offers_saves` (`Host::offers`). So a client knows
  what to show without asking for something it will be refused. `launch`
  stays stated whatever the switch says: it names what the agent can do,
  and a device that may not start games is answered `not_allowed`
  (section 17), which a handheld that sees no `launch` would read as an
  agent too old instead.
- **Gated where it is served.** The server checks the offer for each request
  above, again at each request so a change holds at once. One that is not
  offered gets an `error` whose `code` is `not_offered` and whose message
  says only that, and learns nothing about the host's games. A save file
  sent to a host that does not offer saves is read through and refused, so
  the session goes on.
  What a client sends of its own (`library_push`, `game_put_*`) is the
  client's choice to give and is accepted as now.
- **Accepting a game** is the client's: it copies the game's folder with
  `core::gamecopy` (section 7) into its first game folder, as a version
  folder. "Saves too", on by default, also brings the game's saves when the
  host offers `saves`. The client keeps a list of the games it accepted
  from each host, and for each whether to keep in step (off unless the
  person turns it on; owner, 2026-10-23): when on, each later
  session syncs that game's saves both ways (section 6) and a newer version
  on the host is shown as an update, as droidtop shows one (section 7).
- **Built: the client on a computer** (`droidtop_agent_service::remote`;
  `droidtop-agent offered | accept | play-on | accepted`; Pythia through
  `droidtop_agent.remote`). It reaches the paired device the way the
  handheld does (`core::client`, shared with droidtop's library: where it
  was last seen and the agent's port on that host, the LAN, its WireGuard
  endpoints, a rendezvous when that is on), reads what the hello says it
  offers, and then:
  - browses: pulls the offered library and lists the games installed on
    that device, with their versions and the person's marks;
  - accepts: copies one game under the other device's own folder name (" (2)"
    and on when that is taken, never over a folder that is there), rescans,
    and records it in `accepted.json` (`key`, `title`, `from`, `folder`,
    `version`, `at_ms`, `keep_in_step`);
  - asks to play: `launch_game`, answered by the other device's own switch.

  A computer serving another computer sends only its own games: installs of
  other devices, and marks on games only they have, stay with it, and the
  applications its scan lists (`app:` keys) are never offered. Not built
  yet: "saves too" and keeping in step, which follow.
- **Never merged** (owner, 2026-10-23: "The computer can be aware of it, but
  mostly for sync and update purposes"). Another device's games are that
  device's, on either side: a client shows a host's offered library as "On
  Desk", and a computer keeps what a handheld tells it as the handheld's,
  never as its own games. It uses them to sync saves and to see which
  versions each device has. A game a client accepted joins its own library
  because it is now installed there.
- **Plugin data is not offered** (owner, 2026-10-23: syncing plugin state
  "is a security risk, so no"). An offer never carries plugin contexts
  (section 8), and a computer never syncs them with another computer
  (`Host::syncs_contexts`: a paired handheld only).
- **Handhelds start offered.** A handheld assumes a computer it pairs with
  is its library host and its save host. So when a handheld pairs, the
  computer's offer to it starts with `library` and `saves` on (and `launch`
  off, as now), which is today's behaviour, and an onboarding step at
  pairing lets the person change that. A computer that pairs with a
  computer starts with nothing offered either way. Devices paired before
  offers existed are read the same way by kind: a handheld (or a device with
  no kind, which is one) starts with `library` and `saves`, a computer with
  nothing.
- **Where the person sets it.** The window's Settings lists each paired
  device under "Offered" with three switches (Library, Saves, Starting games
  here), replacing the "Start games" list; Pythia shows the same switches on
  each device's page (`droidtop_agent.store.set_offer`, and `offered` in
  `devices()`). `droidtop-agent offer` lists them and `droidtop-agent offer
  <id> library|saves|launch on|off` changes one. They are kept in
  `Settings::offers` by device id only once the person changes one, so a
  device's default follows its kind until then (`offer_for`).

## 21. Plugins on the computer

Owner (2026-10-23): computer-side plugins can do what a handheld's can where
it makes sense ("Downloaders and stuff are great"), and droidtop's Python
plugins are the computer's plugins too: "We're simplifying by just
completely reusing the python plugin system." Both the command-line agent
and Pythia host them, through a broker in Rust; the agent gets "a minimal
python environment", bundled, because the sandbox must control it. And:
"sandboxing needs to ensure that calls CAN'T be run if they aren't allowed."

- **The same plugins.** A droidtop plugin of kind `python` (contract 2: a
  module-level `handle(call_json) -> reply_json`, and `droidtop.host.call`
  and `droidtop.host.open` for everything it asks; droidtop
  docs/plugin-api.md 1.3) runs on the computer unchanged. Its manifest,
  signature, catalog source and trust tier are droidtop's (plugin-api 1.2
  and 4). `native_bundle` and `flutter_embed` plugins are Android programs
  and do not run here.
- **One package for both** (owner, 2026-10-23, "Yeah, that's the idea"): a
  plugin's manifest says which points it provides on the handheld and which
  on a computer, and each host calls only its own.
- **One host, in Rust** (`crates/plugins`): it installs plugins, keeps their
  grants, starts each one confined, and answers its calls. The agent uses it
  directly and Pythia through `pythia._native`, so both run one
  implementation and one permission check.
- **A bundled Python.** A small CPython, its version pinned, ships beside the
  agent and in Pythia's wheels: the interpreter, the standard library as one
  zip, and the standard library's own extension modules. No `pip` and no
  `site-packages`. A plugin brings only its own Python files. Pythia never
  runs a plugin in its own interpreter. The `droidtop` and `droidtop.host`
  modules are this host's, speaking the same JSON as droidtop's
  (`plugin-host/native/src/droidtoppy_jni.c` on the handheld) over the
  sandbox's channel to the broker.
- **Confined by the operating system, so a call that is not allowed cannot
  be made.** Each plugin runs in its own process under bi0shacker001/sandbox:
  seccomp lockdown with every path call brokered on Linux (built), an
  AppContainer in a job object on Windows, and a Seatbelt profile on macOS
  (Droidtop/tracker#606). The process holds only its channel to the broker:
  it can open no file by itself, has no network, and can start no program.
  Python is not trusted for any of this (`ctypes`, sockets and `subprocess`
  end at the same refused system calls).
- **Fail closed.** Plugins do not run on a system whose sandbox backend is not
  built and passing its escape tests; the host says so and starts nothing.
- **The broker checks every call** against what the manifest declares and the
  person granted, at each call. It answers with droidtop's replies and closed
  set of error codes: a call outside the grants is `PERMISSION_DENIED`, and
  an op this computer does not serve is `UNSUPPORTED`, so a plugin written for
  the handheld learns what is not here the way it already does.
- **What a computer serves** (from plugin-api 3):
  - as on the handheld: jobs (J1), host info (J4), events (J2), the plugin's
    own storage (H1, under the host's data folder), the vault (G1, in the
    system's keychain), background services and schedules (E8, E9), network
    state and requests to declared domains (D1, D2), web sessions (G3), the
    GitHub token (G4), sources and downloads (A2: `search`, `lookup`,
    `options`, `acquire` into a game folder the host chose, then
    `library.files.report_changed`), metadata, artwork, update checks and
    BIOS providers (A3, A4, A6, A11), and UI as data: settings rows,
    notifications, toasts, prompts and rows on a game's page (C1, C6, C6a,
    C14, C18), drawn from the view schema (plugin-api 1.6) by Pythia and the
    agent's window;
  - in a computer's own form: library reads (A1, this computer's library),
    launch providers and hooks (B1, B3, through the `Launcher` of section
    17), save locations and sync (A7), syncable data (F8, section 8),
    installed programs (F1, F4), storage and files (D3, D5), the tray (C8),
    "open with" (C9), and a plugin's own UI (C16, a window);
  - not at all (`UNSUPPORTED`): the gaming shell and Quick Menu points,
    in-game overlays, controller mapping, performance, RetroArch's commands,
    Android app catalogs, intents, root and Shizuku, and the device's
    hardware.
- **Granted on the computer.** Installing a plugin there and granting its
  permissions is the person's, on that computer: Pythia's plugin page, the
  agent's window, or `droidtop-agent plugins`. A grant on the handheld does
  not grant on the computer.
- **Where a computer finds plugins** (owner, 2026-10-23): from droidtop's
  catalog sources added on the computer, and from a handheld offering the
  plugins it has to its computers. Either way a plugin is installed only when
  the person approves it on the computer.
- **Context adapters go away** (owner, 2026-10-23: "anything that's missing
  gets ported"): once a plugin's computer half runs in this host, section 8's
  `computers.context_adapter` programs are retired, and whatever they do that
  the host does not yet serve is ported to it first. Protocol 1 adapters keep
  working until then.
