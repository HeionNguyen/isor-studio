use anyhow::{Context, Result, anyhow};
use audio::{Clip, Playback};
use futures_util::{SinkExt, StreamExt};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use shared::*;
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
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

fn watch(path: &Path, dirty: mpsc::Sender<()>) -> Result<RecommendedWatcher> {
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
                let _ = dirty.try_send(());
            }
            Err(e) => eprintln!("watch error: {e}"),
            _ => {}
        })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

struct Daemon {
    watcher: Option<RecommendedWatcher>,
    loads: Rc<tokio::sync::Mutex<()>>,
    song: Option<Song>,
    playback: Option<Playback>,
    events: broadcast::Sender<Event>,
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
    fn changed(&self) {
        let _ = self.events.send(Event::StateChanged(self.state()));
    }
    async fn load(daemon: &Rc<RefCell<Self>>, path: Option<PathBuf>) -> Result<()> {
        let loads = daemon.borrow().loads.clone();
        let _guard = loads.lock().await;
        let reload = path.is_none();
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
        }
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
                daemon.playback = None;
                daemon.changed();
            }
            Command::GetState => {}
        }
        Ok(Reply::State(daemon.borrow().state()))
    }
    async fn respond(daemon: &Rc<RefCell<Self>>, text: &str) -> Response {
        let (id, outcome) = match serde_json::from_str::<serde_json::Value>(text) {
            Err(e) => (
                None,
                Outcome::Error(RpcError {
                    code: -32700,
                    message: e.to_string(),
                }),
            ),
            Ok(value) => {
                let id = value.get("id").and_then(|v| v.as_u64());
                let outcome = match serde_json::from_value::<Request>(value) {
                    Ok(req) => match Self::command(daemon, req.command).await {
                        Ok(reply) => Outcome::Result(reply),
                        Err(e) => Outcome::Error(RpcError {
                            code: -32000,
                            message: format!("{e:#}"),
                        }),
                    },
                    Err(e) => Outcome::Error(RpcError {
                        code: -32600,
                        message: e.to_string(),
                    }),
                };
                (id, outcome)
            }
        };
        Response {
            jsonrpc: Version::V2,
            id,
            outcome,
        }
    }
}

async fn client(socket: TcpStream, daemon: Rc<RefCell<Daemon>>) -> Result<()> {
    let mut ws = accept_async(socket).await?;
    let mut events = daemon.borrow().events.subscribe();
    loop {
        let json = tokio::select! {
            msg = ws.next() => match msg.transpose()? {
                Some(Message::Text(text)) => serde_json::to_string(&Daemon::respond(&daemon, &text).await)?,
                Some(Message::Close(_)) | None => break,
                Some(_) => { ws.flush().await?; continue }
            },
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => Event::StateChanged(daemon.borrow().state()),
                    Err(_) => break
                };
                serde_json::to_string(&Notification {jsonrpc:Version::V2,event})?
            }
        };
        ws.send(Message::text(json)).await?;
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
        song: None,
        playback: None,
        events: broadcast::channel(64).0,
        dirty,
    }));
    if let Some(path) = std::env::args_os().nth(1) {
        Daemon::load(&daemon, Some(path.into())).await?;
    }
    tokio::task::spawn_local(watch_edits(daemon.clone(), edits));
    loop {
        let (socket, _) = listener.accept().await?;
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
            let _ = daemon.borrow().events.send(Event::ValidationError {
                message: format!("{e:#}"),
            });
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
                if matches!(events.recv().await?, Event::ValidationError { .. }) {
                    break;
                }
            }
            assert_eq!(daemon.borrow().state().bpm, Some(120));
            fs::write(&track, TRACK.replace("pitch: 69", "pitch: 81"))?;
            loop {
                if matches!(events.recv().await?, Event::StateChanged(_)) {
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
        let response = Daemon::respond(&daemon, &serde_json::to_string(&request)?).await;
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
