//! The command line: one table of commands, each with the usage lines it shows in `--help` and the function that runs it.
//!
//! The usage text and the dispatch used to be a 70-line string literal and a 287-line `match` on argument slices that had to
//! be kept in step by hand. Now a command is added in one place, its usage lines are what `--help` prints, and its body lives
//! in the module that owns the topic.

use std::sync::Arc;

use droidtop_agent_service::Agent;

mod devices;
mod library;
mod network;
mod pairing;
mod saves;

use droidtop_agent_core::library::title_key;
use droidtop_agent_core::proto::GameRef;
use droidtop_agent_core::sync::LockExt;
use droidtop_agent_store::Settings;

/// What a command's body gets: the running agent's state and the words after the command's name.
type Run = fn(&Arc<Agent>, &[&str]) -> Result<(), String>;

pub struct Command {
    pub name: &'static str,
    /// One entry per form: the arguments after the name, and what it does. A newline in the second continues it on the next line.
    pub usage: &'static [(&'static str, &'static str)],
    pub run: Run,
}

macro_rules! command {
    ($name:literal, $run:expr, [$(($syntax:literal, $about:literal)),+ $(,)?]) => {
        Command { name: $name, usage: &[$(($syntax, $about)),+], run: $run }
    };
}

/// In the order `--help` shows them. `service-mode` is not here: it runs before the agent's state is opened (see main.rs) and
/// its usage is added to the help by hand.
pub const COMMANDS: &[Command] = &[
    command!("run", network::run, [("[run]", "Serve paired handhelds (keep it running)")]),
    command!(
        "pair",
        pairing::pair,
        [
            (
                "pair <code>",
                "Pair with a handheld showing a pairing code\n(droidtop: Settings > Computers > Pair a computer).\n<code> is the 6 digits, or the text of its QR code."
            ),
            (
                "pair",
                "Show this computer's address and a code to type on the handheld\n(droidtop: Pair a computer > Use a code from the computer),\nfor a handheld this computer cannot reach"
            ),
            (
                "pair compare",
                "Offer to pair: a handheld or another computer picks this one\nfrom its list, and both screens show one code to confirm"
            ),
            ("pair find", "Pick a computer offering to pair, and confirm the code"),
        ]
    ),
    command!("devices", devices::devices, [("devices", "List paired handhelds")]),
    command!("unpair", devices::unpair, [("unpair <id>", "Forget a handheld (the start of its id is enough)")]),
    command!("id", devices::id, [("id", "Show this computer's name and id")]),
    command!("name", devices::name, [("name <name>", "Set the name the handheld shows for this computer")]),
    command!("scan", library::scan, [("scan [--json]", "Show the games and apps found on this computer")]),
    command!(
        "folders",
        library::folders,
        [("folders", "List game and ROM folders"), ("folders add [--roms] <path>", ""), ("folders remove <path>", "")]
    ),
    command!(
        "mark",
        library::mark,
        [("mark <game> favourite|hidden|completed on|off", ""), ("mark <game> collections <name>[,<name>...]", "")]
    ),
    command!(
        "marks",
        library::marks,
        [(
            "marks <game>",
            "Your marks on a game, from this computer (they reach your handhelds\nwhen they sync, like marks made on a handheld); <game> is a key such\nas steam:440 or a title in quotes"
        )]
    ),
    command!(
        "saves",
        saves::saves,
        [
            ("saves <game>", "Where a game keeps its saves here, and the files found\n(<game> is a key such as steam:440, or a title)"),
            (
                "saves add <game> <template>...",
                "State a game's save locations yourself, e.g.\nsaves add steam:440 \"<winDocuments>/My Games/TF2\""
            ),
            ("saves remove <game>", ""),
        ]
    ),
    command!("manifest", saves::manifest, [("manifest update", "Fetch the Ludusavi manifest of save locations now")]),
    command!(
        "share",
        network::share,
        [("share [set <folder> | clear]", "A folder your own sync tool carries to the handheld,\nused when the two are never online together")]
    ),
    command!(
        "contexts",
        network::contexts,
        [
            ("contexts", "The plugin contexts this computer can sync"),
            (
                "contexts add <program>",
                "Add a plugin's context adapter (a program its plugin\npublishes, e.g. droidtop-agent-f95-adapter)"
            ),
            (
                "contexts publish <context> <file>",
                "Make the copy in <file> (JSON with a records object) the current copy of\n<context> in one step: what a plugin's program here runs after it\nwrote the data to share; your handhelds read it at their next sync"
            ),
            ("contexts remove <context>", ""),
            (
                "contexts approve <context>",
                "Install the adapter a plugin on the handheld offers for\n<context> (asked for once; later versions follow)"
            ),
        ]
    ),
    command!(
        "endpoint",
        network::endpoint,
        [(
            "endpoint [set <ip:port> | clear]",
            "Where this computer's WireGuard port (UDP 47611) answers from\nthe internet, if you forwarded it on your router; the handheld\nuses it, and this computer's global IPv6 addresses, when away"
        )]
    ),
    command!(
        "rendezvous",
        network::rendezvous,
        [
            (
                "rendezvous [on | off]",
                "Finding each other away from home the way Syncthing does: STUN for\nthis computer's address, Syncthing's global discovery to announce it\n(addresses only; syncs go through the direct WireGuard tunnel)"
            ),
            ("rendezvous servers default | <url>...", "Discovery servers, in Syncthing's notation (default: Syncthing's)"),
            ("rendezvous stun default | <host:port>...", ""),
        ]
    ),
    command!(
        "syncthing",
        network::syncthing,
        [(
            "syncthing [on | off]",
            "Keep this computer's Syncthing in step with the paired devices' (on\nunless turned off): the one running here, or one started over\nSyncthing's standard folder; each device's Syncthing is added for it"
        )]
    ),
    command!(
        "offer",
        devices::offer,
        [
            (
                "offer",
                "What this computer offers each paired device: its library, its saves,\nand starting games here (a handheld starts with library and saves,\na computer with nothing)"
            ),
            ("offer <id> library|saves|launch on|off", "Change one (the start of its id is enough); saves need the library"),
        ]
    ),
    command!("offered", devices::offered, [("offered <id>", "What a paired device offers this computer, and its games")]),
    command!(
        "accept",
        devices::accept,
        [("accept <id> <game>", "Copy a game a device offers into this computer's first game folder\n(its key, or the start of its title)")]
    ),
    command!("play-on", devices::play_on, [("play-on <id> <game>", "Ask that device to start the game there and stream it")]),
    command!("accepted", devices::accepted, [("accepted", "The games this computer accepted from other devices")]),
    command!(
        "launch",
        devices::launch,
        [
            ("launch", "Which handhelds may start games on this computer (none until you allow one)"),
            (
                "launch allow <id>",
                "Let a handheld start Steam games here, e.g. for streaming them with\nSteam Link (the start of its id is enough)"
            ),
            ("launch deny <id>", ""),
        ]
    ),
    command!(
        "autostart",
        network::autostart,
        [(
            "autostart [on | off]",
            "Start when you sign in (a systemd user unit or autostart entry on\nLinux, a LaunchAgent on macOS, the Run key on Windows); off unless\nyou turn it on"
        )]
    ),
];

/// `service-mode`'s usage, which main.rs handles before the agent's state is opened.
const SERVICE_MODE: (&str, &str) = (
    "service-mode [enable | disable | uninstall]",
    "Windows: let a paired device see and answer this computer's security\nscreens (UAC, lock and sign-in) through windowcast's secure-screen\nhelper, which runs as SYSTEM; off unless you turn it on, and each\nchange asks Windows for administrator approval",
);

/// Where a usage line's description starts.
const COLUMN: usize = 41;

fn lines(entries: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (syntax, about) in entries {
        let head = format!("  droidtop-agent {syntax}");
        if about.is_empty() {
            out.push_str(&head);
            out.push('\n');
            continue;
        }
        let mut about = about.lines();
        if head.len() < COLUMN {
            out.push_str(&format!("{head:<COLUMN$}{}\n", about.next().unwrap_or_default()));
        } else {
            out.push_str(&head);
            out.push('\n');
            out.push_str(&format!("{:<COLUMN$}{}\n", "", about.next().unwrap_or_default()));
        }
        for rest in about {
            out.push_str(&format!("{:<COLUMN$}{rest}\n", ""));
        }
    }
    out
}

/// The whole `--help`, made from the table.
pub fn help() -> String {
    let all: Vec<(&str, &str)> = COMMANDS.iter().flat_map(|c| c.usage.iter().copied()).chain(std::iter::once(SERVICE_MODE)).collect();
    format!("droidtop-agent: keeps this computer and droidtop on a handheld in step.\n\nUsage:\n{}", lines(&all))
}

/// One command's usage lines, for a form it does not know.
pub fn usage(name: &str) -> String {
    match COMMANDS.iter().find(|c| c.name == name) {
        Some(command) => format!("Usage:\n{}", lines(command.usage)),
        None => help(),
    }
}

/// Changes the settings and saves them.
fn change(agent: &Agent, edit: impl FnOnce(&mut Settings)) -> Result<(), String> {
    edit(&mut agent.store.settings.locked());
    agent.store.save_settings().map_err(|e| e.to_string())
}

/// A game as the person typed it: a key (`steam:440`) or a title.
fn game_ref(text: &str) -> GameRef {
    if text.contains(':') && !text.contains(' ') {
        GameRef { key: text.to_string(), title: String::new() }
    } else {
        GameRef { key: title_key(text), title: text.to_string() }
    }
}

/// Runs the command [`words`] names; an unknown one gets the whole help.
pub fn dispatch(agent: &Arc<Agent>, words: &[&str]) -> Result<(), String> {
    let (name, rest) = words.split_first().map(|(n, r)| (*n, r)).unwrap_or(("run", &[]));
    match COMMANDS.iter().find(|c| c.name == name) {
        Some(command) => (command.run)(agent, rest),
        None => Err(format!("Unknown command.\n\n{}", help())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_usage_and_a_unique_name() {
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        assert!(COMMANDS.iter().all(|c| !c.usage.is_empty()));
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a command is listed twice");
    }

    #[test]
    fn the_help_lists_every_form_and_the_wrapped_text_lines_up() {
        let help = help();
        for command in COMMANDS {
            for (syntax, _) in command.usage {
                assert!(help.contains(&format!("droidtop-agent {syntax}")), "{syntax} is missing from the help");
            }
        }
        assert!(help.contains("service-mode [enable | disable | uninstall]"));
        // A description that wraps continues in the same column.
        let pair = help.lines().position(|l| l.contains("droidtop-agent pair <code>")).unwrap();
        let first = help.lines().nth(pair).unwrap();
        let next = help.lines().nth(pair + 1).unwrap();
        assert_eq!(first.find("Pair with").unwrap(), next.find("(droidtop:").unwrap());
    }

    #[test]
    fn a_form_a_command_does_not_know_shows_that_commands_usage_only() {
        let text = usage("launch");
        assert!(text.contains("launch allow <id>") && !text.contains("rendezvous"));
    }
}
