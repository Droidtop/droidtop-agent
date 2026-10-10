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
8. Plugin contexts
9. Conflicts
10. Transports, in order
11. The computer's scanner
12. The droidtop side
13. Repository layout and builds
14. Decisions for the owner
15. The window and the tray

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
  Noise and WireGuard static key. One key, one trust decision, for every
  transport. On a computer the identity file lives in the agent's config
  folder. windowcast can read the same file when the two become one desktop
  app (decision 3). On the handheld, droidtop keeps the key sealed with
  `KeystoreSecretCipher` and passes it to the library for each call.
- **Trust** is windowcast's `TrustStore`: the set of pinned peers. The agent
  keeps a separate peers file for names and last-known addresses, so no
  trust decision is ever made from it.
- **Pairing** is windowcast's SPAKE2 run (`windowcast-pairing`), unchanged:
  6 digits from `generate_pin`, HKDF to a session key, and HMAC tags that both
  sides check.
  1. On the handheld: Settings > Computers > "Pair a computer". droidtop opens
     a short-lived listener (only while that screen is open), shows the 6-digit
     code in large type, and shows a QR code of
     `droidtop-pair:1?code=<code>&id=<PeerId hex>&name=<name>&at=<ip>:<port>`.
     The handheld is the SPAKE2 host (it shows the code).
  2. On the computer: `droidtop-agent pair 123456`. The agent finds the
     handheld on the LAN by a UDP broadcast query that only a handheld in
     pairing mode answers. `droidtop-agent pair 'droidtop-pair:1?...'` (the
     QR's text, from a phone or webcam) goes straight to the address in it.
  3. The two run SPAKE2 over TCP. Each side then sends
     `authenticate_fingerprint(key, transcript)` over the transcript
     `"droidtop-agent pair v1" || host PeerId || client PeerId || host name ||
     client name`, and checks the other's tag. A wrong code or a substituted
     key fails here. Each side then pins the other's PeerId.
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

Every live connection is the same channel, whatever carries it:
**Noise_IK_25519_ChaChaPoly_BLAKE2s** (the `snow` crate) with the prologue
`droidtop-agent/1`. Noise_IK is the handshake family WireGuard uses.

- The handheld is always the initiator: it knows the computer's static key
  from pairing. The computer learns the handheld's static key from the first
  handshake message and refuses it unless that key's PeerId is pinned.
- Framing: each Noise message is sent as a 2-byte big-endian length followed
  by the ciphertext (at most 65535 bytes). An application message is a 4-byte
  length followed by its bytes, split across as many Noise messages as it
  needs.
- Application messages are JSON objects (`serde`, internally tagged by `t`).
  File contents travel as separate binary messages after the JSON header
  that announces them, in 1 MiB pieces.

Over the WireGuard path (section 10) this channel runs inside the tunnel as
well. That is two layers of ChaCha20-Poly1305, which costs little on the
handheld's CPU, and it keeps one authentication path for every transport.

## 5. The protocol

Requests come from the handheld, and the computer answers. Each request gets
exactly one reply (`{"t":"error","message":...}` on failure).

| Request | Reply | Purpose |
|---|---|---|
| `hello {name, version, features}` | `hello {...}` | names, versions, what each side supports |
| `library_pull {since}` | `library_changes {changes, cursor}` | the computer's library changes since a cursor |
| `library_push {changes}` | `ok {cursor}` | the handheld's library changes |
| `save_spec {game}` | `save_spec {spec}` or `unknown` | where this game keeps its saves, as templates |
| `save_manifest {game}` | `manifest {files}` | the computer's save files: name, size, mtime, SHA-256 |
| `file_get {game, name}` | `file {name, size, sha256}` + data | one save file, computer to handheld |
| `file_put {game, name, size, sha256, mtime}` + data | `ok` | one save file, handheld to computer |
| `save_apply {game, remove, archive}` | `ok` | removals, and archiving the computer's side first when it lost a conflict |
| `context_pull {context}` | `context {records}` | a plugin context's records on the computer |
| `context_push {context, changes}` | `ok` or `deferred` | record changes to apply on the computer |

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
- **The decision is the same rule droidtop's Steam Cloud sync uses**
  (`SteamCloudPlan.decide`), now in the shared core so it is one
  implementation. The handheld keeps a baseline per paired computer and game:
  the files as they were after the last sync (name, SHA-256, size, mtime).
  - If only one side changed since the baseline, that side wins: its changed
    files are copied over, and files it deleted are deleted on the other side.
  - If both changed and still differ, it is a conflict (section 9).
  - A first sync with differing files on both sides is a conflict too.
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
- **No versioning.** The owner ruled it out for storage reasons. The one
  exception is a conflict loser, which is archived (section 9).

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
  `hidden` and `completed` (booleans) today.
  - Before each exchange droidtop reports every mark it keeps on its own
    games, unset ones included. Only a mark that differs from the shared one
    becomes a change (`Library::note_marks`); unset, absent, false, zero and
    empty all count as the same. So a mark that arrived from elsewhere and
    was written into droidtop's library is not sent back with a newer stamp.
  - After it, the core names the shared marks that differ from what droidtop
    reported (`Library::marks_to_write`), and droidtop writes them into its
    own library.
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

## 8. Plugin contexts

The owner (#380): "It's context SYNC. It's meant to allow you to do things on
the computer and have them synced." A context is a plugin's supporting state,
and it syncs both ways between the device and the computer. The first context
is F95Checker's database for the F95 plugin. The plugin API side is in
droidtop's `docs/plugin-api.md` ("Context sync"). The shape:

- **A context is a set of records**, `key -> {field -> JSON value}`, plus a
  declaration from the plugin: which fields exist, whether each is two-way,
  computer-to-device or device-to-computer, and its conflict rule (`device`,
  `computer`, or `ask`).
- **The computer side is a context adapter: a separate program the plugin
  publishes**, one per kind of third-party store. The agent carries no
  third-party format itself (coordinator decision, 2026-10-09: the F95Checker
  adapter belongs with the F95 plugin, in gamegrab-sources). The agent asks
  an adapter which context it serves and keeps the pair in its settings.
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
- **The adapter contract (protocol 1)**, JSON over standard input and output,
  one run per pull or push:
  - `<adapter> describe` prints `{"protocol": 1, "id": "<context>",
    "description": "..."}`;
  - `<adapter> pull` prints `{"records": {<key>: {<field>: <value>}}}`;
  - `<adapter> push` reads `{"changes": [...]}` (the core's `RecordChange`:
    `{"op": "upsert", "key", "fields"}` or `{"op": "remove", "key"}`) and
    prints `{"deferred": null}`, or `{"deferred": "<why>"}` when the changes
    must wait;
  - any of them may print `{"error": "<words a screen can show>"}`; a
    non-zero exit is an error too, with what it wrote to standard error. A
    run is stopped after 120 s.
- **Why a program and not a loadable module.** Rust has no stable ABI, so a
  dynamic module would need a C interface, `unsafe` on both sides and a build
  per system matching the agent's own; a crash in it would take the agent
  down with it. A program needs none of that: it can be written in any
  language (F95Checker itself is Python), it is built and released by the
  plugin's own repository on the plugin's own schedule, a fault ends only
  that run, and it is a plain process boundary between two separately
  licensed works. A run costs a process start per sync, which a sync that
  happens around a game's launch or on request can afford.
- **Merge is three-way per field**, against the baseline the handheld keeps
  per computer and context. If only one side changed a field, that change
  wins. If both changed it differently, the field's rule decides; `ask` puts
  the record in the context's conflict list for the person.
- **Writing a third-party app's store** happens only while that app is closed.
  F95Checker keeps its database in memory and writes it back, so a change made
  under it would be lost. While it runs, the adapter replies `deferred` and
  the change is made at the next sync after it closes.
- **The F95 plugin's adapter** is gamegrab-sources/droidtop-agent-f95-adapter.
  It finds F95Checker's `db.sqlite3` (Windows `%APPDATA%\f95checker`, Linux
  `~/.config/f95checker`, macOS `~/Library/Application Support/f95checker`),
  reads only the declared columns of the `games` table, and never reads or
  carries the `cookies` table or the settings table's passwords and tokens.
  Its fields:
  - two-way: `installed`, `finished`, `archived`, `rating`, `notes`, and the
    record's presence (watching a thread);
  - computer to device: `name`, `url`, `version`, `developer`, `status`,
    `type`, `last_updated`.
  A thread watched on the device is inserted with its id, name and url, and
  F95Checker's own refresh fills in the rest.

## 9. Conflicts

One question, everywhere: droidtop's existing "Saves differ" dialog
(`SaveConflictPrompts`). It names the computer instead of "Cloud" and shows
each side's last change, file count and which is newer. The person picks a
side. "Later" (B) changes nothing, and the game starts on this device's files.

**Losers are archived, not deleted.** Before the losing side is overwritten,
its files are moved to an archive:
- on the computer: `<agent data>/archive/<game>/<UTC time>/`;
- on the handheld: droidtop's `files/agent/archive/<computer>/<game>/<UTC time>/`.

Only the most recent loser per game is kept, because the owner ruled out
versioning for storage reasons (decision 8). A context's conflicts are per
record and per field, never per file, so nothing there is overwritten without
a rule or an answer.

## 10. Transports, in order

All of them carry the same Noise channel between paired keys. A relay or
share only ever sees ciphertext.

1. **LAN direct.** TCP to the agent's port (47610). The handheld finds the
   agent by a UDP broadcast query on the same port, which only paired agents
   answer (the reply is signed with the agent's key), and also tries the last
   address that worked. Tailscale, ZeroTier or a plain WireGuard VPN, when the
   person has one, is just another address to try, never a requirement.
2. **Direct WireGuard.** A userspace WireGuard tunnel (boringtun) between the
   two paired keys over UDP, with UDP hole punching for NAT. The tunnel's
   addresses come from the PeerIds (a `fd64:` ULA per device). Inside it, a
   userspace TCP stack (smoltcp) carries the Noise channel, so nothing on
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
| Folders | user-chosen roots; a folder with a program in it is a game | same | `.app` bundles too |
| ROMs | user-chosen roots laid out by ES-DE system name; ES-DE's own `es_settings.xml` ROM folder and `gamelists/` | same | same |
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
  - while the pairing screen is open.
  Nothing polls, and nothing listens in the background.
- **Where the code is:**
  - `:net-core`, `dev.droidtop.net.peer`: the JNI binding to the agent core
    (`libdroidtop_agent.so`) and the sealed identity;
  - `:library-core`, `dev.droidtop.library.computers`: paired computers, the
    save sync beside `StoreSaves`, and library and context sync;
  - `:app`: the Computers settings catalog and the pairing screen (code and QR).
- **The native library is built here**, in this repository's CI, for
  arm64-v8a and x86_64, and published with each release. droidtop fetches it
  against a pin file (tag and SHA-256), the way it fetches the lsfg-vk layer.
  So droidtop's CI needs no Rust toolchain, and the core has one build.

## 13. Repository layout and builds

- `crates/core` (`droidtop-agent-core`): identity, pairing, channel,
  protocol, sync rules, transports. It has no UI and no platform-specific code.
- `crates/agent` (`droidtop-agent`): the agent as a library (scanner,
  adapters, pairing, serving, settings), plus the command-line program.
- `crates/app` (`droidtop-agent-app`): the same agent with a window and a
  tray icon (section 15). It is what people run; the command line stays for
  headless machines and scripts.
- `crates/android` (`droidtop-agent-android`): the `cdylib` with droidtop's
  JNI surface.
- CI (`.github/workflows/ci.yml`): build and test on Linux (x86_64 and
  aarch64), Windows and macOS (arm64, with the x86_64 build made beside it),
  plus the Android library for both ABIs. Each system's release has both
  programs; macOS also gets `droidtop-agent.app` (`packaging/macos`). Each green build on `main`
  publishes a release (the computer binaries and the Android library) and adds
  a CHANGELOG entry. Releases are permanent history.

## 14. Decisions for the owner

1. **Licence.** The agent is GPL-3.0-only because it links windowcast's
   GPL-3.0-only `identity` and `pairing` crates. Relicense those two crates as
   MIT/Apache-2.0 so the agent can be Apache-2.0? Default: stay GPL-3.0-only.
2. **Pairing input.** The code is typed on the computer, and the QR code
   serves cameras (laptop webcam, phone). Decided (coordinator, 2026-10-09):
   the computer can also show the code and the handheld connect (section 3).
3. **One identity with windowcast on a computer.** Default: yes, one key file,
   one pairing, once the two share the desktop app.
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
8. **Conflict archive depth.** Default: the most recent loser per game.
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

- **One agent, two front ends.** `droidtop-agent-app` runs the same service as
  `droidtop-agent run`, in its own process, and shows it. If another copy of
  the agent already listens, the window says so and does not serve.
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
  - **Library:** what the last scan found (games, installed apps, programs
    it can sync with, sources it could not read), a filter, and Scan now.
  - **Saves:** the copies the agent kept: this computer's side when it lost
    a conflict, and save letters it refused. Each opens in the file manager.
    The conflict question itself is asked on the handheld, where the game
    is about to run (section 9).
  - **Plugin data:** adapters plugins offered (Install or Decline; a declined
    plugin's offer for that context is not kept again) and the ones
    installed (Remove).
  - **Settings:** name, rescan interval, away from home, forwarded port,
    cloud folder, game and ROM folders, and starting at sign-in.
- **Behaviour.** Closing the window leaves the agent in the tray. The tray
  menu offers Open, Pair a handheld and Quit. Autostart starts it with
  `--hidden`, in the tray only. Slow work (scanning, pairing, fetching an
  adapter) runs on threads of its own. Files are read when a page opens and
  every few seconds after that, never on every frame.

