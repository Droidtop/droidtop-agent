//! droidtop-agent: keeps this computer and droidtop on a handheld in step
//! (docs/DESIGN.md). Run without arguments it serves paired handhelds.

mod commands;

use std::sync::Arc;

use droidtop_agent_service::Agent;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help" | "help")) {
        print!("{}", commands::help());
        return;
    }
    // Service mode runs before the agent's state is opened: its elevated copy
    // may run under an administrator's account and must not touch that
    // account's files (docs/DESIGN.md section 19).
    if args.first().map(String::as_str) == Some("service-mode") {
        std::process::exit(service_mode(&args[1..]));
    }
    let agent = match Agent::open() {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("droidtop-agent could not start: {e}");
            std::process::exit(1);
        }
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = commands::dispatch(&agent, &words) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// `droidtop-agent service-mode [enable | disable | uninstall]`, and the
/// elevated copy that asks for (`<action> --confirmed [--grant-sid <sid>]`).
fn service_mode(args: &[String]) -> i32 {
    use droidtop_agent_service::secure_screen::{self, exit, Action};
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let state = || match secure_screen::state() {
        "enabled" => "on",
        "disabled" => "off (the helper is installed)",
        "absent" => "off",
        _ => "not available: it is only on Windows",
    };
    match words.as_slice() {
        [] | ["status"] => {
            println!("Service mode: {}", state());
            exit::DONE
        }
        [action, "--confirmed", rest @ ..] => {
            let Some(action) = Action::parse(action) else { return exit::BAD_REQUEST };
            let sid = match rest {
                ["--grant-sid", sid] => Some(*sid),
                [] => None,
                _ => return exit::BAD_REQUEST,
            };
            let helper = std::env::current_exe().ok().and_then(|e| e.parent().and_then(secure_screen::helper_beside));
            match secure_screen::perform(action, sid, helper.as_deref()) {
                Ok(()) => exit::DONE,
                Err((code, e)) => {
                    eprintln!("{e}");
                    code
                }
            }
        }
        [action] if Action::parse(action).is_some() => {
            let action = Action::parse(action).expect("checked");
            if action == Action::Enable {
                println!("{}\n", secure_screen::CONSENT);
                print!("Turn service mode on? [y/N] ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("Service mode stays off.");
                    return exit::DONE;
                }
            }
            let program = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    return exit::FAILED;
                }
            };
            match secure_screen::request(action, &program, &["service-mode"]) {
                Ok(done) => {
                    println!("{done}");
                    exit::DONE
                }
                Err(e) => {
                    eprintln!("{e}");
                    exit::FAILED
                }
            }
        }
        _ => {
            eprintln!("usage: droidtop-agent service-mode [enable | disable | uninstall]");
            exit::BAD_REQUEST
        }
    }
}
