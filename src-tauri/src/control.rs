//! The loopback control channel: how a command typed at a shell becomes a
//! thread in the window the user is already looking at (P12).
//!
//! `AppEvent`s are an in-process `tokio::sync::broadcast`, so the store being
//! shared SQLite buys nothing here: a row another process writes produces no
//! event, and the open window never re-reads. The only way a turn can show up
//! live on screen is for the process that owns that window to start it. This
//! module is that door — a Unix socket beside the store, on which one JSON
//! line asks for a turn and one JSON line answers.
//!
//! Deliberately not a general RPC surface. It carries exactly what a headless
//! caller cannot do for itself, and every failure it can suffer — a peer that
//! hung up, a line that is not JSON, a turn the runtime refused — is a line
//! on stderr and another `accept`, never a window that goes down.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use neo_agent::Runtime;
use neo_core::{ConversationId, RunId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::commands;
use crate::state::Runs;

/// The most a request may be. A peer that opens the socket and writes without
/// ever sending a newline is the shape of an accident, and an unbounded
/// `read_line` would answer it by growing this process's heap until the
/// window died.
const MAX_REQUEST: u64 = 64 * 1024;

/// How much of a first message becomes a thread name when the caller did not
/// choose one: enough to recognise the thread in the switcher, short enough
/// not to push the rest of the row off it.
const TITLE_LIMIT: usize = 60;

/// One request, one connection. `title` is optional because most callers have
/// nothing better to say than what they already said in `say`.
#[derive(Debug, Deserialize)]
struct Request {
    say: String,
    #[serde(default)]
    title: Option<String>,
}

/// The answer, in the two shapes a caller has to tell apart.
///
/// `ok` is a field rather than a tag because the caller is often a shell
/// pipeline: `.ok` is one `jq` away, and a client that only knows this much
/// of the protocol can still branch correctly. Untagged with the flag carried
/// inside each variant so that no code path can build a success that says
/// `false`, or a failure that says `true`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Response {
    Started {
        ok: bool,
        conversation: ConversationId,
        run: RunId,
    },
    Failed {
        ok: bool,
        error: String,
    },
}

impl Response {
    fn started(conversation: ConversationId, run: RunId) -> Self {
        Self::Started {
            ok: true,
            conversation,
            run,
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self::Failed {
            ok: false,
            error: error.into(),
        }
    }
}

/// Bind the socket and serve it until the process ends.
///
/// Answers `Err` only when the socket itself could not be taken, which the
/// caller reports and then lives without: a window with no control channel is
/// still a window, and refusing to open one because a stale file could not be
/// removed would be a worse failure than the one being reported.
pub async fn serve(socket: PathBuf, runtime: Arc<Runtime>, runs: Arc<Runs>) -> io::Result<()> {
    let listener = bind(&socket)?;
    eprintln!("neo-desktop: control socket at {}", socket.display());
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                // Descriptor exhaustion answers every `accept` instantly, so a
                // bare `continue` here would spin a core for as long as the
                // condition lasts. The pause costs a failing caller nothing.
                eprintln!("neo-desktop: control socket could not accept: {error}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let runtime = Arc::clone(&runtime);
        let runs = Arc::clone(&runs);
        // Each connection on its own task: a peer that stops reading half way
        // through the answer must not hold up the next caller.
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, runtime, runs).await {
                eprintln!("neo-desktop: control connection ended: {error}");
            }
        });
    }
}

/// Take the socket, refusing to steal one that is already being served.
///
/// A file being there means one of two things, and they need opposite
/// answers: a window that is running right now (in which case this process
/// must not bind, or `neo say` would reach whichever of the two happened to
/// win) or the remains of one that crashed (in which case `bind` would fail
/// with `AddrInUse` forever until the file is removed). Connecting is what
/// distinguishes them — a socket with no listener refuses immediately, so
/// this blocking call never waits on anything.
fn bind(socket: &Path) -> io::Result<UnixListener> {
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "{} is already served by another Starkbot window",
                    socket.display()
                ),
            ));
        }
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    // This is a door into the user's agent: anything that can open it can
    // spend the user's tokens and drive their applications. The mode is set
    // rather than left to the process umask, which a launcher is free to have
    // set to anything at all.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// One request and one answer, then the peer is closed.
async fn serve_connection(
    stream: UnixStream,
    runtime: Arc<Runtime>,
    runs: Arc<Runs>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader.take(MAX_REQUEST))
        .read_line(&mut line)
        .await?;
    let response = handle(&line, runtime, runs).await;
    // The newline is part of the protocol, not decoration: it is what lets a
    // caller read one answer without waiting for the socket to close.
    writer.write_all(encode(&response).as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// Answer one request line. Every failure becomes a `Failed` response rather
/// than an error: the caller is owed words it can print, and a connection
/// that dropped silently would look to a shell like the window ignoring it.
pub(crate) async fn handle(line: &str, runtime: Arc<Runtime>, runs: Arc<Runs>) -> Response {
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(error) => {
            return Response::failed(format!(
                "expected one JSON object of the form {{\"say\": \"…\"}}: {error}"
            ));
        }
    };

    // Its own thread, always. A control message arrives with no idea what the
    // user has on screen, and appending to whatever thread happened to be
    // open would interleave a shell's work with a conversation someone is in
    // the middle of.
    let title = request
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| thread_title(&request.say));
    let conversation = {
        let runtime = Arc::clone(&runtime);
        match commands::blocking(move || runtime.new_conversation(Some(title))).await {
            Ok(conversation) => conversation,
            Err(error) => return Response::failed(error.message),
        }
    };

    // The same path the composer takes, on this process's runtime — which is
    // the whole point of the socket, because that runtime is the one whose
    // `AppEvent`s the open window is subscribed to.
    match commands::start_turn(runtime, runs, conversation.id, request.say).await {
        Ok(run) => Response::started(conversation.id, run),
        Err(error) => Response::failed(error.message),
    }
}

/// A thread name taken from the message itself: its first line, trimmed, cut
/// on a character boundary so a multi-byte first sentence cannot panic here.
fn thread_title(say: &str) -> String {
    let first = say.lines().next().unwrap_or(say).trim();
    if first.is_empty() {
        return "control".to_owned();
    }
    match first.char_indices().nth(TITLE_LIMIT) {
        Some((cut, _)) => format!("{}…", &first[..cut]),
        None => first.to_owned(),
    }
}

/// The response as a line. Serialisation of two owned strings and two uuids
/// cannot fail, but this crate denies `unwrap`, and a caller stuck waiting
/// because the answer could not be written would be a worse bug than the
/// impossible one this guards.
fn encode(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|error| {
        eprintln!("neo-desktop: a control response could not be encoded: {error}");
        r#"{"ok":false,"error":"the response could not be encoded"}"#.to_owned()
    })
}
