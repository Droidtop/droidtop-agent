//! The running droidtop agent on a computer: what serves paired handhelds.
//! The command line (`droidtop-agent`), the window (`droidtop-agent-app`)
//! and a desktop manager that is this computer's agent (Pythia, through
//! `droidtop-agent-python`) are thin hosts over it (docs/DESIGN.md section 13).

mod agent;
pub mod autostart;
pub mod events;
pub mod host;
pub mod launch;
pub mod manager;
pub mod remote;
pub mod rendezvous;
pub mod secure_screen;
pub mod serve;
pub mod share;
pub mod syncthing;

pub use agent::{scan_options, Agent};
