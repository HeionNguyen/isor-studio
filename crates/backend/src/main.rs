use anyhow::{Context, Result, anyhow};
use audio::{Clip, Playback};
use futures_util::{SinkExt, StreamExt, stream::FuturesOrdered};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use shared::*;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc},
    task::LocalSet,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

struct Song {
    path: PathBuf,
    project: Project,
    clip: Clip,
}
impl Song {
    fn read(path: PathBuf) -> Result<Self> {
        let root = path.parent().context("missing project directory")?;
        let project: Project = serde_yaml::from_slice(&fs::read(&path)?)?;
        project.validate().map_err(|e| anyhow!(e))?;
        let mut tracks = Vec::new();
        for id in &project.tracks {
            let file = root.join("tracks").join(format!("{id}.yaml"));
            let track: Track = serde_yaml::from_slice(
                &fs::read(&file).with_context(|| file.display().to_string())?,
            )
            .with_context(|| file.display().to_string())?;
            track.validate(id).map_err(|e| anyhow!("{id}: {e}"))?;
            tracks.push(track);
        }
        let clip = Clip::prepare(&project, &tracks).map_err(anyhow::Error::from_boxed)?;
        Ok(Self {
            path,
            project,
            clip,
        })
    }
}

struct ProjectWatcher {
    _watcher: RecommendedWatcher,
    dirty: Arc<AtomicBool>,
}

fn watch(path: &Path, wake: mpsc::Sender<()>) -> Result<ProjectWatcher> {
    let dirty = Arc::<AtomicBool>::default();
    let signal = dirty.clone();
    let root = path.parent().context("missing project directory")?;
    let manifest = path.to_owned();
    let track_dir = root.join("tracks");
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(e)
                if !matches!(e.kind, notify::EventKind::Access(_))
                    && e.paths
                        .iter()
                        .any(|p| p == &manifest || p.starts_with(&track_dir)) =>
            {
                signal.store(true, Ordering::Release);
                let _ = wake.try_send(());
            }
            Err(e) => eprintln!("watch error: {e}"),
            _ => {}
        })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(ProjectWatcher {
        _watcher: watcher,
        dirty,
    })
}

struct Daemon {
    watcher: Option<ProjectWatcher>,
    revision: u64,
    song_revision: u64,
    project_id: u64,
    stop_epoch: u64,
    loads: Rc<tokio::sync::Mutex<()>>,
    song: Option<Song>,
    playback: Option<Playback>,
    events: broadcast::Sender<(u64, Event)>,
    dirty: mpsc::Sender<()>,
}
impl Daemon {
    fn state(&self) -> State {
        State {
            title: self.song.as_ref().map(|s| s.project.title.clone()),
            bpm: self.song.as_ref().map(|s| s.project.bpm),
            playing: self.playback.is_some(),
        }
    }
    fn changed(&mut self) {
        self.revision += 1;
        let _ = self
            .events
            .send((self.revision, Event::StateChanged(self.state())));
    }
    async fn load(daemon: &Rc<RefCell<Self>>, path: Option<PathBuf>) -> Result<()> {
        let loads = daemon.borrow().loads.clone();
        let _guard = loads.lock().await;
        let reload = path.is_none();
        // Wakes are coalesced globally, but the dirty bit belongs to the
        // registration. Test it after acquiring the load lock so a queued
        // wake from a retired watcher cannot reload the replacement project.
        if reload
            && !daemon
                .borrow()
                .watcher
                .as_ref()
                .is_some_and(|w| w.dirty.swap(false, Ordering::AcqRel))
        {
            return Ok(());
        }
        let Some(path) = path.or_else(|| daemon.borrow().song.as_ref().map(|s| s.path.clone()))
        else {
            return Ok(());
        };
        let dirty = daemon.borrow().dirty.clone();
        let (song, watcher) = tokio::task::spawn_blocking(move || -> Result<_> {
            let path = fs::canonicalize(path)?;
            let watcher = if reload {
                None
            } else {
                Some(watch(&path, dirty)?)
            };
            Ok((Song::read(path)?, watcher))
        })
        .await??;
        let mut daemon = daemon.borrow_mut();
        let playback = if reload && let Some(old) = &daemon.playback {
            Some(Playback::start(&song.clip, old.position()).map_err(anyhow::Error::from_boxed)?)
        } else {
            None
        };
        if watcher.is_some() {
            daemon.watcher = watcher;
            daemon.project_id += 1;
        }
        daemon.song_revision = daemon.revision + 1;
        daemon.song = Some(song);
        daemon.playback = playback;
        daemon.changed();
        Ok(())
    }
    async fn command(daemon: &Rc<RefCell<Self>>, cmd: Command) -> Result<Reply> {
        match cmd {
            Command::Ping => return Ok(Reply::Pong),
            Command::LoadProject { path } => Self::load(daemon, Some(path)).await?,
            Command::Play => {
                let mut daemon = daemon.borrow_mut();
                if daemon.playback.is_none() {
                    let song = daemon.song.as_ref().context("load a project first")?;
                    daemon.playback =
                        Some(Playback::start(&song.clip, 0).map_err(anyhow::Error::from_boxed)?);
                    daemon.changed();
                }
            }
            Command::Stop => {
                let mut daemon = daemon.borrow_mut();
                daemon.stop_epoch += 1;
                daemon.playback = None;
                daemon.changed();
            }
            Command::GetState => {}
        }
        Ok(Reply::State(daemon.borrow().state()))
    }
    async fn respond(daemon: &Rc<RefCell<Self>>, request: Request) -> Response {
        let outcome = match Self::command(daemon, request.command).await {
            Ok(reply) => Outcome::Result(reply),
            Err(error) => Outcome::Error(RpcError {
                code: -32000,
                message: format!("{error:#}"),
            }),
        };
        Response {
            jsonrpc: Version::V2,
            id: Some(request.id),
            outcome,
        }
    }
}

fn parse_request(text: &str) -> Result<Request, Response> {
    let error = |id, code, message| Response {
        jsonrpc: Version::V2,
        id,
        outcome: Outcome::Error(RpcError { code, message }),
    };
    let value = serde_json::from_str::<serde_json::Value>(text)
        .map_err(|e| error(None, -32700, e.to_string()))?;
    let id = value.get("id").and_then(|v| v.as_u64());
    serde_json::from_value(value).map_err(|e| error(id, -32600, e.to_string()))
}

fn rejected(id: u64, message: &str) -> Response {
    Response {
        jsonrpc: Version::V2,
        id: Some(id),
        outcome: Outcome::Error(RpcError {
            code: -32000,
            message: message.into(),
        }),
    }
}

// Load and the Plays following it are FIFO within a socket. Stop and
// GetState bypass the queue. A Stop invalidates already enqueued Plays.
async fn ordered(
    daemon: &Rc<RefCell<Daemon>>,
    request: Request,
    stop_epoch: u64,
    target: Option<u64>,
) -> (Response, Option<u64>) {
    if matches!(request.command, Command::LoadProject { .. }) {
        let response = Daemon::respond(daemon, request).await;
        let target =
            matches!(response.outcome, Outcome::Result(_)).then(|| daemon.borrow().project_id);
        return (response, target);
    }
    if daemon.borrow().stop_epoch != stop_epoch {
        let state = Reply::State(daemon.borrow().state());
        return (
            Response {
                jsonrpc: Version::V2,
                id: Some(request.id),
                outcome: Outcome::Result(state),
            },
            target,
        );
    }
    if target != Some(daemon.borrow().project_id) {
        return (
            rejected(request.id, "requested project is unavailable"),
            target,
        );
    }
    (Daemon::respond(daemon, request).await, target)
}

async fn client(socket: TcpStream, daemon: Rc<RefCell<Daemon>>) -> Result<()> {
    let ws = accept_async(socket).await?;
    let events = daemon.borrow().events.subscribe();
    serve_client(ws, daemon, events).await
}

async fn serve_client(
    mut ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    daemon: Rc<RefCell<Daemon>>,
    mut events: broadcast::Receiver<(u64, Event)>,
) -> Result<()> {
    let mut active = FuturesOrdered::new();
    let mut queue = VecDeque::new();
    let mut target = None;
    let mut rejected_load = false;
    let mut snapshot_revision = 0;
    loop {
        if active.is_empty()
            && let Some((request, epoch)) = queue.pop_front()
        {
            active.push_back(ordered(&daemon, request, epoch, target));
        }
        let response = tokio::select! {
            biased;
            Some((response,project)) = active.next(), if !active.is_empty() => {target = project; response},
            msg = ws.next() => match msg.transpose()? {
                Some(Message::Text(text)) => match parse_request(&text) {
                    Err(error) => error,
                    Ok(req) if rejected_load && matches!(req.command,Command::Play) => rejected(req.id,"preceding load was rejected"),
                    Ok(req) if matches!(req.command,Command::LoadProject { .. }) || (!active.is_empty() && matches!(req.command,Command::Play)) => {
                        let full = queue.len() >= 16;
                        if matches!(req.command,Command::LoadProject { .. }) { rejected_load = full; }
                        if full { rejected(req.id,"too many pending transport commands") } else {
                            queue.push_back((req,daemon.borrow().stop_epoch));
                            continue
                        }
                    }
                    Ok(req) => Daemon::respond(&daemon,req).await,
                }
                Some(Message::Close(_)) | None => break,
                Some(_) => { ws.flush().await?; continue }
            },
            event = events.recv() => {
                let (revision,event) = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let current = daemon.borrow();
                        events = current.events.subscribe();
                        (current.revision,Event::StateChanged(current.state()))
                    },
                    Err(_) => break
                };
                match event {
                    Event::StateChanged(_) if revision < snapshot_revision => continue,
                    Event::StateChanged(_) => snapshot_revision = revision,
                    Event::ValidationError { .. } if revision < daemon.borrow().song_revision => continue,
                    Event::ValidationError { .. } => {}
                }
                let json = serde_json::to_string(&Notification {jsonrpc:Version::V2,event})?;
                ws.send(Message::text(json)).await?;
                continue;
            }
        };
        if matches!(response.outcome, Outcome::Result(Reply::State(_))) {
            snapshot_revision = daemon.borrow().revision;
        }
        ws.send(Message::text(serde_json::to_string(&response)?))
            .await?;
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    LocalSet::new().run_until(serve()).await
}

async fn serve() -> Result<()> {
    let listener = TcpListener::bind(
        DEFAULT_WS_URL
            .strip_prefix("ws://")
            .context("invalid shared endpoint")?,
    )
    .await?;
    let (dirty, edits) = mpsc::channel(1);
    let daemon = Rc::new(RefCell::new(Daemon {
        loads: Default::default(),
        watcher: None,
        revision: 0,
        song_revision: 0,
        project_id: 0,
        stop_epoch: 0,
        song: None,
        playback: None,
        events: broadcast::channel(64).0,
        dirty,
    }));
    if let Some(path) = std::env::args_os().nth(1) {
        Daemon::load(&daemon, Some(path.into())).await?;
    }
    tokio::task::spawn_local(watch_edits(daemon.clone(), edits));
    let mut health_check = tokio::time::interval(Duration::from_millis(100));
    loop {
        let (socket, _) = tokio::select! {
            connection = listener.accept() => connection?,
            _ = health_check.tick() => {
                let mut daemon = daemon.borrow_mut();
                if let Some(message) = daemon.playback.as_ref().and_then(Playback::failure) {
                    eprintln!("{message}");
                    daemon.playback = None;
                    daemon.changed();
                }
                continue;
            }
        };
        let daemon = daemon.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = client(socket, daemon).await {
                eprintln!("client: {e}");
            }
        });
    }
}

async fn watch_edits(daemon: Rc<RefCell<Daemon>>, mut edits: mpsc::Receiver<()>) {
    while edits.recv().await.is_some() {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = edits.try_recv();
        if let Err(e) = Daemon::load(&daemon, None).await {
            let daemon = daemon.borrow();
            let _ = daemon.events.send((
                daemon.revision,
                Event::ValidationError {
                    message: format!("{e:#}"),
                },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Rc<RefCell<Daemon>>, mpsc::Receiver<()>) {
        let (dirty, edits) = mpsc::channel(1);
        let daemon = Rc::new(RefCell::new(Daemon {
            loads: Default::default(),
            watcher: None,
            revision: 0,
            song_revision: 0,
            project_id: 0,
            stop_epoch: 0,
            song: None,
            playback: None,
            events: broadcast::channel(8).0,
            dirty,
        }));
        (daemon, edits)
    }

    const PROJECT: &str = include_str!("../../../examples/demo/project.yaml");
    const TRACK: &str = include_str!("../../../examples/demo/tracks/lead.yaml");

    fn demo_copy() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        fs::create_dir(dir.path().join("tracks"))?;
        fs::write(dir.path().join("project.yaml"), PROJECT)?;
        fs::write(dir.path().join("tracks/lead.yaml"), TRACK)?;
        Ok(dir)
    }

    type Socket = tokio_tungstenite::WebSocketStream<TcpStream>;

    async fn connect(daemon: Rc<RefCell<Daemon>>) -> Result<Socket> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::task::spawn_local(async move {
            let (socket, _) = listener.accept().await?;
            client(socket, daemon).await
        });
        let (ws, _) = tokio_tungstenite::client_async(
            format!("ws://{addr}"),
            TcpStream::connect(addr).await?,
        )
        .await?;
        Ok(ws)
    }

    async fn send(ws: &mut Socket, id: u64, command: Command) -> Result<()> {
        ws.send(Message::text(serde_json::to_string(&Request {
            jsonrpc: Version::V2,
            id,
            command,
        })?))
        .await?;
        Ok(())
    }

    async fn receive(ws: &mut Socket) -> Result<serde_json::Value> {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .context("socket closed")??;
        Ok(serde_json::from_str(msg.to_text()?)?)
    }

    #[tokio::test]
    async fn same_socket_transport_during_load_and_ordered_snapshots() -> Result<()> {
        LocalSet::new()
            .run_until(async {
                let dir = demo_copy()?;
                let (daemon, _edits) = fixture();
                let mut ws = connect(daemon.clone()).await?;
                let loads = daemon.borrow().loads.clone();
                let guard = loads.lock().await;
                send(
                    &mut ws,
                    1,
                    Command::LoadProject {
                        path: dir.path().join("project.yaml"),
                    },
                )
                .await?;
                send(&mut ws, 2, Command::Stop).await?;
                send(&mut ws, 3, Command::GetState).await?;
                for expected in [2, 3] {
                    loop {
                        let msg = receive(&mut ws).await?;
                        if msg.get("id").is_some() {
                            assert_eq!(msg["id"], expected);
                            assert_eq!(msg["result"]["state"]["playing"], false);
                            assert!(msg["result"]["state"]["title"].is_null());
                            break;
                        }
                    }
                }
                let (old_revision, old_state) = {
                    let d = daemon.borrow();
                    (d.revision, d.state())
                };
                drop(guard);
                while receive(&mut ws).await?["id"] != 1 {}
                // Deliver history retained by the broadcast queue after this client
                // has received a later authoritative LoadProject response.
                {
                    let d = daemon.borrow();
                    d.events
                        .send((old_revision, Event::StateChanged(old_state)))?;
                    d.events.send((
                        d.revision,
                        Event::ValidationError {
                            message: "end of batch".into(),
                        },
                    ))?;
                }
                loop {
                    let msg = receive(&mut ws).await?;
                    if msg["method"] == "ValidationError" {
                        break;
                    }
                    assert_eq!(msg["params"]["title"], "First Light");
                }
                Ok(())
            })
            .await
    }

    async fn response(ws: &mut Socket) -> Result<Response> {
        loop {
            let msg = receive(ws).await?;
            if msg.get("id").is_some() {
                return Ok(serde_json::from_value(msg)?);
            }
        }
    }

    async fn pipelined_play(cancel: bool, valid_load: bool) -> Result<()> {
        let dir = demo_copy()?;
        let (daemon, _edits) = fixture();
        Daemon::load(&daemon, Some(dir.path().join("project.yaml"))).await?;
        let new = demo_copy()?;
        let path = new.path().join("project.yaml");
        fs::write(
            &path,
            if valid_load {
                PROJECT.replace("First Light", "New song")
            } else {
                String::from("invalid: [")
            },
        )?;
        let mut ws = connect(daemon.clone()).await?;
        let loads = daemon.borrow().loads.clone();
        let guard = loads.lock().await;
        send(&mut ws, 1, Command::LoadProject { path }).await?;
        send(&mut ws, 2, Command::Play).await?;
        send(&mut ws, 3, Command::GetState).await?;
        let current = response(&mut ws).await?;
        assert_eq!(current.id, Some(3));
        assert!(
            matches!(current.outcome,Outcome::Result(Reply::State(State {title:Some(title),playing:false,..})) if title=="First Light")
        );
        if cancel {
            send(&mut ws, 4, Command::Stop).await?;
            assert_eq!(response(&mut ws).await?.id, Some(4));
        }
        drop(guard);
        let load = response(&mut ws).await?;
        assert_eq!(load.id, Some(1));
        let play = response(&mut ws).await?;
        assert_eq!(play.id, Some(2));
        if valid_load {
            assert!(matches!(load.outcome, Outcome::Result(_)));
            assert!(
                matches!(play.outcome,Outcome::Result(Reply::State(State {title:Some(title),playing,..})) if title=="New song" && playing!=cancel)
            );
        } else {
            assert!(matches!(load.outcome, Outcome::Error(_)));
            assert!(matches!(play.outcome, Outcome::Error(_)));
        }
        Ok(())
    }

    #[tokio::test]
    async fn later_stop_cancels_queued_play_and_failed_load_preserves_old_transport() -> Result<()>
    {
        LocalSet::new()
            .run_until(async {
                pipelined_play(true, true).await?;
                pipelined_play(false, false).await
            })
            .await
    }

    #[tokio::test]
    #[ignore = "requires a live default output device"]
    async fn play_after_pending_load_starts_requested_project() -> Result<()> {
        LocalSet::new().run_until(pipelined_play(false, true)).await
    }

    #[tokio::test]
    async fn stop_preserves_pending_validation_for_current_song() -> Result<()> {
        LocalSet::new()
            .run_until(async {
                let dir = demo_copy()?;
                let (daemon, _edits) = fixture();
                Daemon::load(&daemon, Some(dir.path().join("project.yaml"))).await?;
                let listener = TcpListener::bind("127.0.0.1:0").await?;
                let addr = listener.local_addr()?;
                let peer = async {
                    let (tcp, _) = listener.accept().await?;
                    Ok::<_, anyhow::Error>(accept_async(tcp).await?)
                };
                let connect = async {
                    Ok::<_, anyhow::Error>(
                        tokio_tungstenite::client_async(
                            format!("ws://{addr}"),
                            TcpStream::connect(addr).await?,
                        )
                        .await?
                        .0,
                    )
                };
                let (server, mut ws) = tokio::try_join!(peer, connect)?;
                let events = daemon.borrow().events.subscribe();
                send(&mut ws, 1, Command::Stop).await?;
                server.get_ref().readable().await?;
                {
                    let d = daemon.borrow();
                    d.events.send((
                        d.revision,
                        Event::ValidationError {
                            message: "partial YAML".into(),
                        },
                    ))?;
                }
                tokio::task::spawn_local(serve_client(server, daemon, events));
                let mut saw_stop = false;
                loop {
                    let msg = receive(&mut ws).await?;
                    if msg["id"] == 1 {
                        saw_stop = true;
                    }
                    if msg["method"] == "ValidationError" {
                        assert!(saw_stop, "Stop response should precede buffered diagnostic");
                        break;
                    }
                }
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn retired_watcher_wake_cannot_reload_new_websocket_project() -> Result<()> {
        LocalSet::new()
            .run_until(async {
                let old = demo_copy()?;
                let new = demo_copy()?;
                let (daemon, mut edits) = fixture();
                Daemon::load(&daemon, Some(old.path().join("project.yaml"))).await?;
                let mut ws = connect(daemon.clone()).await?;
                fs::write(old.path().join("tracks/lead.yaml"), "patterns: [")?;
                // Hold the real notify wake through the replacement, just as a
                // pending debounce or load lock can delay its consumption.
                tokio::time::timeout(Duration::from_secs(5), edits.recv())
                    .await?
                    .context("watcher stopped")?;
                send(
                    &mut ws,
                    1,
                    Command::LoadProject {
                        path: new.path().join("project.yaml"),
                    },
                )
                .await?;
                while receive(&mut ws).await?["id"] != 1 {}
                let revision = daemon.borrow().revision;
                Daemon::load(&daemon, None).await?;
                assert_eq!(daemon.borrow().revision, revision);
                send(&mut ws, 2, Command::GetState).await?;
                loop {
                    let msg = receive(&mut ws).await?;
                    if msg["id"] == 2 {
                        assert_eq!(msg["result"]["state"]["title"], "First Light");
                        break;
                    }
                    assert_ne!(msg["method"], "ValidationError");
                }
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn watcher_recovers_after_partial_track_rewrite() -> Result<()> {
        let dir = demo_copy()?;
        let (daemon, edits) = fixture();
        Daemon::load(&daemon, Some(dir.path().join("project.yaml"))).await?;
        let mut events = daemon.borrow().events.subscribe();
        let exercise = async {
            let track = dir.path().join("tracks/lead.yaml");
            fs::write(&track, "patterns: [")?;
            loop {
                if matches!(events.recv().await?.1, Event::ValidationError { .. }) {
                    break;
                }
            }
            assert_eq!(daemon.borrow().state().bpm, Some(120));
            fs::write(&track, TRACK.replace("pitch: 69", "pitch: 81"))?;
            loop {
                if matches!(events.recv().await?.1, Event::StateChanged(_)) {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::select! {
            _ = watch_edits(daemon.clone(),edits) => anyhow::bail!("watcher exited"),
            result = tokio::time::timeout(Duration::from_secs(10),exercise) => result??,
        }
        Ok(())
    }

    #[tokio::test]
    async fn long_reload_yields_to_stop_and_get_state() -> Result<()> {
        let dir = demo_copy()?;
        let (daemon, _edits) = fixture();
        Daemon::load(&daemon, Some(dir.path().join("project.yaml"))).await?;
        fs::write(
            dir.path().join("tracks/lead.yaml"),
            TRACK.replace("duration_beats: 1", "duration_beats: 300"),
        )?;
        daemon
            .borrow()
            .watcher
            .as_ref()
            .context("missing watcher")?
            .dirty
            .store(true, Ordering::Release);
        let reload = Daemon::load(&daemon, None);
        tokio::pin!(reload);
        // Poll the reload first. A synchronous renderer completes here before
        // either transport request can run on the single-threaded executor.
        tokio::select! {
            biased;
            result = &mut reload => anyhow::bail!("reload finished before transport could run: {result:?}"),
            result = async {
                for cmd in [Command::GetState, Command::Stop] {
                    assert!(matches!(Daemon::command(&daemon,cmd).await?,Reply::State(State {playing:false,..})));
                }
                Ok::<_,anyhow::Error>(())
            } => result?,
        }
        reload.await?;
        assert!(!daemon.borrow().state().playing);
        Ok(())
    }

    #[tokio::test]
    async fn demo_renders_nonzero_stereo_and_serves_shared_state() -> Result<()> {
        let (daemon, _edits) = fixture();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/demo/project.yaml");
        Daemon::load(&daemon, Some(path)).await?;
        let track = serde_yaml::from_str(TRACK)?;
        let project = serde_yaml::from_str(PROJECT)?;
        let frames =
            audio::render(&project, &[track], 48_000).map_err(anyhow::Error::from_boxed)?;
        assert_eq!(frames.len(), 96_000);
        assert!(
            frames
                .iter()
                .any(|s| s[0].abs() > 0.01 && s[1].abs() > 0.01)
        );
        let request = Request {
            jsonrpc: Version::V2,
            id: 42,
            command: Command::GetState,
        };
        let response = Daemon::respond(&daemon, request).await;
        assert_eq!(response.id, Some(42));
        assert_eq!(
            response.outcome,
            Outcome::Result(Reply::State(State {
                title: Some("First Light".into()),
                bpm: Some(120),
                playing: false
            }))
        );
        Ok(())
    }
}
