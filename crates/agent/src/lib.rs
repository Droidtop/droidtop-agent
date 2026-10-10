//! droidtop-agent as a library: everything the command-line program
//! (`droidtop-agent`) and the window-and-tray program (`droidtop-agent-app`)
//! share, so both run the same agent (docs/DESIGN.md section 13).

pub mod autostart;
pub mod contexts;
pub mod host;
pub mod ludusavi;
pub mod pair;
pub mod rendezvous;
pub mod scan;
pub mod serve;
pub mod share;
pub mod state;
