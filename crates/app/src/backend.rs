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
                let request = Request { jsonrpc: Version::V2, id, command };
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
