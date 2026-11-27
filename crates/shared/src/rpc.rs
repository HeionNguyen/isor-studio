use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default Phase 0 loopback WebSocket endpoint.
pub const DEFAULT_WS_URL: &str = "ws://127.0.0.1:7810";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: Version,
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Command {
    Ping,
    LoadProject {
        /// Path to project.yaml.
        path: PathBuf,
    },
    Play,
    Stop,
    GetState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub title: Option<String>,
    pub bpm: Option<u16>,
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: Version,
    pub id: Option<u64>,
    #[serde(flatten)]
    pub outcome: Outcome,
}
/// Ping returns Pong. All other successful commands return the current state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Result(Reply),
    Error(RpcError),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Pong,
    State(State),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: Version,
    #[serde(flatten)]
    pub event: Event,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Event {
    StateChanged(State),
    ValidationError { message: String },
}
