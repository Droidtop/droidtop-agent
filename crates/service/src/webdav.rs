//! A WebDAV share for store and forward (docs/DESIGN.md section 10, transport 3; Droidtop/tracker#800):
//! the next backend after a folder the person's own sync tool carries. The client is lib-net's
//! `net-remote` (PROPFIND, GET, PUT to a temporary name then MOVE, DELETE, MKCOL), not one of the
//! agent's own, and the letters on it are the mailbox's, sealed as they are in a folder.
//!
//! The share's address and user name are in `webdav.json` beside the agent's settings. The password
//! is there only sealed to this computer's own key ([`seal_secret`]), so the file can be read, copied
//! or backed up without giving the password away, and nothing here prints or logs it.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use droidtop_agent_core::mailbox;
use droidtop_agent_core::share::{check, is_partial, Share};
use droidtop_agent_store::{read_json, write_json};
use net_remote::{Remote, RemotePath, WebDav, WebDavConfig};
use serde::{Deserialize, Serialize};

use crate::Agent;

/// What `webdav.json` holds. The password only ever as [`seal_secret`] made it.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct WebDavShare {
    /// The folder the share starts at, without a sign-in in it.
    pub url: String,
    #[serde(default)]
    pub user: Option<String>,
    /// The password, sealed to this computer's key, in hex.
    #[serde(default)]
    pub password_sealed: Option<String>,
    /// Plain http, the person's choice for a share on their own network.
    #[serde(default)]
    pub allow_plain_http: bool,
}

impl std::fmt::Debug for WebDavShare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavShare")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &self.password_sealed.as_ref().map(|_| "…"))
            .field("allow_plain_http", &self.allow_plain_http)
            .finish()
    }
}

fn path(agent: &Agent) -> PathBuf {
    agent.store.dirs.config.join("webdav.json")
}

/// The WebDAV share set on this computer, if any.
pub fn current(agent: &Agent) -> io::Result<Option<WebDavShare>> {
    let share: WebDavShare = read_json(&path(agent))?;
    Ok((!share.url.is_empty()).then_some(share))
}

/// Sets the WebDAV share, sealing [`password`] to this computer's key. It is checked first: an
/// address the client refuses (a sign-in in it, plain http not allowed) is not saved.
pub fn set(agent: &Agent, url: &str, user: Option<&str>, password: Option<&str>, allow_plain_http: bool) -> io::Result<()> {
    let password_sealed = match password {
        Some(p) => Some(seal_secret(agent, p)?),
        None => None,
    };
    let share = WebDavShare { url: url.trim().to_string(), user: user.map(str::to_string), password_sealed, allow_plain_http };
    connect(agent, &share)?;
    write_private(&path(agent), &share)
}

/// Forgets the WebDAV share and its sealed password.
pub fn clear(agent: &Agent) -> io::Result<()> {
    match std::fs::remove_file(path(agent)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The share as a store-and-forward [`Share`].
pub fn connect(agent: &Agent, share: &WebDavShare) -> io::Result<RemoteShare<WebDav>> {
    let mut config = WebDavConfig::new(share.url.clone()).allow_plain_http(share.allow_plain_http).timeout(Duration::from_secs(30));
    if let Some(user) = &share.user {
        let password = match &share.password_sealed {
            Some(sealed) => open_secret(agent, sealed)?,
            None => String::new(),
        };
        config = config.sign_in(user.clone(), password);
    }
    Ok(RemoteShare(WebDav::new(config)?))
}

/// Seals [`secret`] to this computer's own key with the mailbox's sealing, in hex.
pub fn seal_secret(agent: &Agent, secret: &str) -> io::Result<String> {
    let key = &agent.store.key;
    let sealed = mailbox::seal(key, &key.peer_id(), secret.as_bytes()).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(droidtop_agent_core::hex::encode(&sealed))
}

/// What [`seal_secret`] sealed.
pub fn open_secret(agent: &Agent, sealed: &str) -> io::Result<String> {
    let key = &agent.store.key;
    let bytes = droidtop_agent_core::hex::decode(sealed).ok_or_else(|| io::Error::other("the share's sealed password is damaged"))?;
    let me = key.peer_id();
    let (_, secret) = mailbox::open(key, &bytes, |p| *p == me)
        .map_err(|_| io::Error::other("the share's password was sealed by another identity: set the share again"))?;
    String::from_utf8(secret).map_err(|_| io::Error::other("the share's sealed password is damaged"))
}

fn write_private(path: &std::path::Path, share: &WebDavShare) -> io::Result<()> {
    write_json(path, share)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Any lib-net share as a lib-sync [`Share`]. lib-sync has the same adapter as `share::RemoteShare`
/// from v0.1.7 (its `net-remote` feature); it is here until lib-saves, lib-plugins and lib-games take
/// that lib-sync too, since two lib-syncs in one build are two incompatible copies of its types.
pub struct RemoteShare<R: Remote>(pub R);

fn remote_path(path: &str) -> io::Result<RemotePath> {
    RemotePath::new(check(path)?).map_err(io::Error::from)
}

impl<R: Remote + Send + Sync> Share for RemoteShare<R> {
    fn files(&self, dir: &str) -> io::Result<Vec<String>> {
        match self.0.list(&remote_path(dir)?) {
            Ok(entries) => Ok(entries.into_iter().filter(|e| !e.is_dir && !is_partial(&e.name)).map(|e| e.name).collect()),
            Err(net_remote::Error::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        self.0.read(&remote_path(path)?).map_err(io::Error::from)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.write(&remote_path(path)?, bytes).map_err(io::Error::from)
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        self.0.remove(&remote_path(path)?).map_err(io::Error::from)
    }

    fn make_dirs(&self, dir: &str) -> io::Result<()> {
        self.0.make_dirs(&remote_path(dir)?).map_err(io::Error::from)
    }
}
