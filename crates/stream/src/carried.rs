//! A stream carried by the agent's own tunnel (Droidtop/tracker#113): the
//! owner, 2026-10-28, "Our standard carrier tunnel is also a remote
//! streaming tunnel". lib-sync's WireGuard session hands the agent two more
//! things beside its channel: a side link (a second reliable byte stream)
//! and datagrams, each as two blocking queues. windowcast's `serve_over`
//! takes an async byte stream and a conduit of datagrams. This module joins
//! the two, so windowcast's signaling runs on the side link and its media on
//! the datagrams, and nothing else leaves the tunnel.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use windowcast_transport::conduit::Conduit;

use crate::Carried;

/// How long a thread blocked on a tunnel queue waits before it looks
/// whether the session it feeds has ended.
const LOOK: Duration = Duration::from_secs(1);

/// What the side link buffers between the tunnel and windowcast's signaling,
/// which sends a few kilobytes at a time.
const SIDE_BUFFER: usize = 64 * 1024;

/// The side link as the async byte stream windowcast's signaling reads and
/// writes. `first` is what [`Carried::first_words`](crate::Carried::first_words) already took from it,
/// or nothing.
pub fn byte_stream(runtime: &Handle, side_out: Sender<Vec<u8>>, side_in: Receiver<Vec<u8>>, first: Vec<u8>) -> DuplexStream {
    let (ours, pump) = tokio::io::duplex(SIDE_BUFFER);
    let (mut from_session, mut to_session) = tokio::io::split(pump);
    // The tunnel's queue blocks, so it is read on a thread of its own.
    let feeding = runtime.clone();
    let _ = std::thread::Builder::new().name("stream-side-in".into()).spawn(move || {
        if !first.is_empty() && feeding.block_on(to_session.write_all(&first)).is_err() {
            return; // The session ended.
        }
        loop {
            match side_in.recv_timeout(LOOK) {
                // An empty read is the other end closing its side.
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    if feeding.block_on(to_session.write_all(&chunk)).is_err() {
                        return;
                    }
                }
                // A write of nothing fails once the session dropped its end.
                Err(RecvTimeoutError::Timeout) => {
                    if feeding.block_on(to_session.write(&[])).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = feeding.block_on(to_session.shutdown());
    });
    runtime.spawn(async move {
        let mut buf = vec![0u8; SIDE_BUFFER];
        loop {
            match from_session.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if side_out.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
        // Dropping side_out here closes the side link.
    });
    ours
}

/// The tunnel's datagrams as windowcast's conduit.
pub fn conduit(runtime: &Handle, datagrams_out: Sender<Vec<u8>>, datagrams_in: Receiver<Vec<u8>>) -> Conduit {
    let (to_session, inbound) = mpsc::unbounded_channel();
    let (outbound, mut from_session) = mpsc::unbounded_channel::<Bytes>();
    let _ = std::thread::Builder::new().name("stream-datagrams-in".into()).spawn(move || loop {
        match datagrams_in.recv_timeout(LOOK) {
            Ok(datagram) => {
                if to_session.send(Bytes::from(datagram)).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) if to_session.is_closed() => return,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    });
    runtime.spawn(async move {
        while let Some(datagram) = from_session.recv().await {
            if datagrams_out.send(datagram.to_vec()).is_err() {
                break;
            }
        }
    });
    Conduit { outbound, inbound }
}

/// Serves one handheld over [`carried`] with windowcast's host until the
/// session ends. `first` is what [`Carried::first_words`](crate::Carried::first_words) took; `address`
/// names the peer in the host's list of who is connected.
pub async fn serve(
    carried: Carried,
    first: Vec<u8>,
    address: String,
    control: std::sync::Arc<windowcast_host::HostControl>,
    source: std::sync::Arc<dyn windowcast_host::WindowSource>,
) -> Result<(), String> {
    let runtime = Handle::current();
    let Carried { side_out, side_in, datagrams_out, datagrams_in } = carried;
    let stream = byte_stream(&runtime, side_out, side_in, first);
    let conduit = conduit(&runtime, datagrams_out, datagrams_in);
    windowcast_host::serve_over(stream, address, control, source, conduit).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;

    use windowcast_host::{FrameSource, HostConfig, HostControl, WindowSource};
    use windowcast_identity::{Identity, TrustStore};
    use windowcast_protocol::{ControlMessage, VideoCodec, WindowId, WindowInfo};
    use windowcast_transport::{ClientCredential, Session};

    use super::*;

    /// A host with no windows: enough to answer a session.
    struct NoWindows;

    impl WindowSource for NoWindows {
        fn list_windows(&self) -> Vec<WindowInfo> {
            Vec::new()
        }
        fn encoders(&self) -> Vec<VideoCodec> {
            vec![VideoCodec::H264]
        }
        fn open(&self, _: WindowId, _: VideoCodec) -> Result<Box<dyn FrameSource>, String> {
            Err("no windows".into())
        }
    }

    /// Two ends of a tunnel session as lib-sync hands them over: each end's
    /// outbound queue is the other's inbound, for the side link and for the
    /// datagrams.
    fn tunnel() -> (Carried, Carried) {
        let (a_side, b_side_in) = mpsc::channel();
        let (b_side, a_side_in) = mpsc::channel();
        let (a_dg, b_dg_in) = mpsc::channel();
        let (b_dg, a_dg_in) = mpsc::channel();
        (
            Carried { side_out: a_side, side_in: a_side_in, datagrams_out: a_dg, datagrams_in: a_dg_in },
            Carried { side_out: b_side, side_in: b_side_in, datagrams_out: b_dg, datagrams_in: b_dg_in },
        )
    }

    /// A handheld's viewer reaches the computer's host through the tunnel's
    /// side link and datagrams alone (Droidtop/tracker#113): the host waits
    /// for its first words, serves it, and answers a Ping; the media went
    /// through the datagrams.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_trusted_viewer_is_served_through_the_tunnel() {
        let dir = std::env::temp_dir().join(format!("droidtop-stream-carried-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let viewer = Identity::generate();
        let mut trusted = TrustStore::default();
        trusted.pin(viewer.peer_id());
        trusted.save(&dir.join(windowcast_identity::HOST_TRUST_FILE)).unwrap();
        let control = HostControl::open(&HostConfig { listen: "127.0.0.1:0".into(), pairing: false, data_dir: dir.clone() }).unwrap();
        let mut viewer_trust = TrustStore::default();
        viewer_trust.pin(control.peer_id());

        let (computer, handheld) = tunnel();
        // The computer's side, as the agent's tunnel thread runs it.
        let host = tokio::task::spawn_blocking(move || {
            let first = computer.first_words().expect("the handheld spoke first");
            Handle::current().block_on(serve(computer, first, "the tunnel".into(), control, Arc::new(NoWindows)))
        });

        // The handheld's side: the same join, then windowcast's client.
        let runtime = Handle::current();
        let Carried { side_out, side_in, datagrams_out, datagrams_in } = handheld;
        let stream = byte_stream(&runtime, side_out, side_in, Vec::new());
        let session = Session::over_conduit(conduit(&runtime, datagrams_out, datagrams_in)).await.unwrap();
        let established = tokio::time::timeout(
            Duration::from_secs(40),
            windowcast_transport::connect(stream, session, &viewer, ClientCredential::Pinned(&viewer_trust)),
        )
        .await
        .expect("signaling hung")
        .expect("a trusted viewer gets in");
        established.session.send_control(&ControlMessage::Ping).await.unwrap();
        let answered = tokio::time::timeout(Duration::from_secs(20), async {
            while established.session.recv_control().await.unwrap() != ControlMessage::Pong {}
        })
        .await;
        assert!(answered.is_ok(), "no Pong");
        let (sent, received) = established.session.conduit_counts().unwrap();
        assert!(sent > 0 && received > 0, "the media went around the tunnel");
        established.session.close().await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(20), host).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tunnel ending before the handheld says anything is no stream.
    #[test]
    fn a_tunnel_that_ends_first_is_no_stream() {
        let (computer, handheld) = tunnel();
        drop(handheld);
        assert_eq!(computer.first_words(), None);
    }
}
