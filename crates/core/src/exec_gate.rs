//! Writing a program and starting programs, one after the other in this process.
//!
//! Linux refuses to run a file while any process still has it open for writing ("text file busy", ETXTBSY). A program
//! written a moment ago is such a file while another thread of this process is between `fork` and `exec` of an
//! unrelated child: the child holds a copy of the write descriptor until it execs. The race is not in either side
//! alone, so it is closed between them: writing a program takes this gate exclusively, and every start of a program
//! takes it shared, so no child is forked while a program is open for writing. A caller that wrote a program may
//! then run it at once, with no retry. Every start of a program by the agent goes through [`spawn`], [`status`] or
//! [`output`]; one that does not is outside the guarantee.

use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::RwLock;

static GATE: RwLock<()> = RwLock::new(());

fn shared() -> std::sync::RwLockReadGuard<'static, ()> {
    // The gate guards no data, so a thread that panicked holding it left nothing half done.
    GATE.read().unwrap_or_else(|e| e.into_inner())
}

/// Writes [`bytes`] to [`path`] as a program that may be run (mode 0755 on Unix) and closes it before anything else
/// in this process starts a child.
pub fn write_program(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _exclusive = GATE.write().unwrap_or_else(|e| e.into_inner());
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// [`Command::spawn`], not while a program is being written.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    let _shared = shared();
    command.spawn()
}

/// [`Command::status`], not while a program is being written.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// [`Command::output`], not while a program is being written.
pub fn output(command: &mut Command) -> io::Result<Output> {
    let _shared = shared();
    let child =
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn()?;
    drop(_shared);
    child.wait_with_output()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Many threads start programs while others write one and run it straight away: none sees "text file busy".
    #[test]
    fn a_program_just_written_runs_while_other_threads_start_children() {
        let dir = std::env::temp_dir().join(format!("dta-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let noise: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = status(Command::new("true").stdout(std::process::Stdio::null()));
                    }
                })
            })
            .collect();
        for n in 0..60 {
            let path = dir.join(format!("p{n}"));
            write_program(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            let status = status(&mut Command::new(&path)).unwrap_or_else(|e| panic!("run {n}: {e}"));
            assert!(status.success());
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for t in noise {
            t.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
