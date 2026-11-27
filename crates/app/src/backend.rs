use anyhow::{Context, Result, bail};
use async_channel::{Receiver, Sender};
use async_net::TcpStream;
use async_tungstenite::{client_async, tungstenite::Message};
use futures_util::{FutureExt, StreamExt, select};
use shared::{Command, Event, Notification, Outcome, Reply, Request, Response, State, Version};

pub enum Update {
    State(State),
    Error(String),
    Disconnected(String),
}

pub fn decode(text: &str) -> Result<Option<Update>> {
    if let Ok(notification) = serde_json::from_str::<Notification>(text) {
        return Ok(Some(match notification.event {
            Event::StateChanged(state) => Update::State(state),
            Event::ValidationError { message } => Update::Error(message),
        }));
    }
    Ok(match serde_json::from_str::<Response>(text)?.outcome {
        Outcome::Result(Reply::State(state)) => Some(Update::State(state)),
        Outcome::Result(Reply::Pong) => None,
        Outcome::Error(error) => Some(Update::Error(error.message)),
    })
}

fn outgoing_request(id: u64, mut command: Command) -> Result<Request> {
    if let Command::LoadProject { path } = &mut command
        && path.is_relative()
    {
        let cwd = std::env::current_dir().with_context(|| {
            format!(
                "Resolving project path {} from the app working directory",
                path.display()
            )
        })?;
        *path = cwd.join(&*path);
    }
    Ok(Request {
        jsonrpc: Version::V2,
        id,
        command,
    })
}

pub async fn connect(commands: Receiver<Command>, updates: Sender<Update>) -> Result<()> {
    let url = shared::DEFAULT_WS_URL;
    let address = url.strip_prefix("ws://").context("Invalid backend URL")?;
    let tcp = TcpStream::connect(address)
        .await
        .context("Connecting to backend")?;
    let (mut socket, _) = client_async(url, tcp).await?;
    let mut id = 0;
    loop {
        select! {
            command = commands.recv().fuse() => {
                let Ok(command) = command else { return Ok(()) };
                id += 1;
                let request = outgoing_request(id, command)?;
                socket.send(Message::text(serde_json::to_string(&request)?)).await?;
            },
            frame = socket.next().fuse() => match frame.transpose()? {
                Some(Message::Text(text)) => {
                    if let Some(update) = decode(&text)? {
                        updates.send(update).await?;
                    }
                }
                Some(Message::Ping(bytes)) => socket.send(Message::Pong(bytes)).await?,
                None | Some(Message::Close(_)) => bail!("Backend closed connection"),
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_load_project_resolves_from_app_even_when_daemon_cwd_differs() {
        let app_cwd = std::env::current_dir().unwrap();
        let daemon_cwd = app_cwd.join("daemon");
        let relative = std::path::Path::new("songs/project.yaml");
        let request = outgoing_request(
            1,
            Command::LoadProject {
                path: relative.into(),
            },
        )
        .unwrap();
        let wire = serde_json::to_string(&request).unwrap();
        let Request {
            command: Command::LoadProject { path },
            ..
        } = serde_json::from_str(&wire).unwrap()
        else {
            panic!("expected LoadProject")
        };
        assert_eq!(daemon_cwd.join(path), app_cwd.join(relative));
    }
}
