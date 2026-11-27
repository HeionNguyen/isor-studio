use anyhow::{Context, Result, anyhow};
use audio::Playback;
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
    tracks: Vec<Track>,
    _watcher: RecommendedWatcher,
}
impl Song {
    fn load(path: &Path, dirty: mpsc::Sender<()>) -> Result<Self> {
        let path = fs::canonicalize(path)?;
        let root = path.parent().context("missing project directory")?;
        let manifest = path.clone();
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
        Ok(Self {
            path,
            project,
            tracks,
            _watcher: watcher,
        })
    }
}

struct Daemon {
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
    fn load(&mut self, path: &Path, reload: bool) -> Result<()> {
        let song = Song::load(path, self.dirty.clone())?;
        let playback = if reload && let Some(old) = &self.playback {
            Some(
                Playback::start(&song.project, &song.tracks, old.position())
                    .map_err(anyhow::Error::from_boxed)?,
            )
        } else {
            None
        };
        self.song = Some(song);
        self.playback = playback;
        self.changed();
        Ok(())
    }
    fn command(&mut self, cmd: Command) -> Result<Reply> {
        match cmd {
            Command::Ping => return Ok(Reply::Pong),
            Command::LoadProject { path } => self.load(&path, false)?,
            Command::Play if self.playback.is_none() => {
                let song = self.song.as_ref().context("load a project first")?;
                self.playback = Some(
                    Playback::start(&song.project, &song.tracks, 0)
                        .map_err(anyhow::Error::from_boxed)?,
                );
                self.changed();
            }
            Command::Stop => {
                self.playback = None;
                self.changed();
            }
            Command::Play | Command::GetState => {}
        }
        Ok(Reply::State(self.state()))
    }
    fn respond(&mut self, text: &str) -> Response {
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
                    Ok(req) => match self.command(req.command) {
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
                Some(Message::Text(text)) => serde_json::to_string(&daemon.borrow_mut().respond(&text))?,
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
    let (dirty, mut edits) = mpsc::channel(1);
    let daemon = Rc::new(RefCell::new(Daemon {
        song: None,
        playback: None,
        events: broadcast::channel(64).0,
        dirty,
    }));
    if let Some(path) = std::env::args_os().nth(1) {
        daemon.borrow_mut().load(Path::new(&path), false)?;
    }
    loop {
        tokio::select! {
            connection = listener.accept() => {
                let (socket,_) = connection?;
                let daemon = daemon.clone();
                tokio::task::spawn_local(async move { if let Err(e) = client(socket,daemon).await { eprintln!("client: {e}"); } });
            }
            Some(()) = edits.recv() => {
                tokio::time::sleep(Duration::from_millis(60)).await;
                let _ = edits.try_recv();
                let mut daemon = daemon.borrow_mut();
                if let Some(path) = daemon.song.as_ref().map(|s| s.path.clone())
                    && let Err(e) = daemon.load(&path,true) {
                        let _ = daemon.events.send(Event::ValidationError {message:format!("{e:#}")});
                    }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_renders_nonzero_stereo_and_serves_shared_state() -> Result<()> {
        let mut daemon = Daemon {
            song: None,
            playback: None,
            events: broadcast::channel(8).0,
            dirty: mpsc::channel(1).0,
        };
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/demo/project.yaml");
        daemon.command(Command::LoadProject { path })?;
        let song = daemon.song.as_ref().context("missing loaded demo")?;
        let frames = audio::render(&song.project, &song.tracks, 48_000)
            .map_err(anyhow::Error::from_boxed)?;
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
        let response = daemon.respond(&serde_json::to_string(&request)?);
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
