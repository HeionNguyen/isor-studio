mod backend;

use async_channel::Sender;
use backend::Update;
use gpui::{
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use shared::{Command, State};

struct Studio {
    state: Option<State>,
    message: String,
    commands: Sender<Command>,
}

impl Studio {
    fn new(cx: &mut Context<Self>) -> Self {
        let (commands, requests) = async_channel::unbounded();
        let (events, updates) = async_channel::unbounded();
        let project = std::env::args_os()
            .nth(1)
            .map(Into::into)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../examples/demo/project.yaml")
            });
        let _ = commands.try_send(Command::LoadProject { path: project });
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = backend::connect(requests, events.clone()).await {
                    let _ = events
                        .send(Update::Disconnected(format!("{error:#}")))
                        .await;
                }
            })
            .detach();
        cx.spawn(async move |studio, cx| {
            while let Ok(update) = updates.recv().await {
                if studio
                    .update(cx, |studio, cx| studio.apply(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            state: None,
            message: "Connecting…".into(),
            commands,
        }
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::State(state) => {
                self.state = Some(state);
                self.message.clear();
            }
            Update::Error(message) => self.message = message,
            Update::Disconnected(message) => {
                self.state = None;
                self.message = message;
            }
        }
        cx.notify();
    }

    fn toggle(&self) {
        if let Some(state) = &self.state
            && state.title.is_some()
        {
            let _ = self.commands.try_send(if state.playing {
                Command::Stop
            } else {
                Command::Play
            });
        }
    }
}

impl Render for Studio {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.as_ref();
        let title = state.and_then(|s| s.title.clone());
        let enabled = title.is_some();
        let tempo = state
            .and_then(|s| s.bpm)
            .map(|bpm| format!("{bpm} BPM"))
            .unwrap_or_default();
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .p_8()
            .gap_3()
            .bg(rgb(0x192222))
            .text_color(rgb(0xf1f4ef))
            .child(
                div()
                    .text_3xl()
                    .child(title.unwrap_or_else(|| "isor-studio".into())),
            )
            .child(tempo)
            .child(
                div()
                    .id("transport")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(if enabled { 0x216859 } else { 0x424848 }))
                    .when(enabled, |button| {
                        button
                            .cursor_pointer()
                            .on_click(cx.listener(|studio, _, _, _| studio.toggle()))
                    })
                    .child(if state.is_some_and(|s| s.playing) {
                        "Stop"
                    } else {
                        "Play"
                    }),
            )
            .child(self.message.clone())
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.), px(300.)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Studio::new),
        ) {
            eprintln!("Opening studio window failed: {error}");
            cx.quit();
        }
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn transport_follows_backend(cx: &mut TestAppContext) {
        let (commands, requests) = async_channel::unbounded();
        let studio = cx.new(|_| Studio {
            commands,
            state: None,
            message: String::new(),
        });
        for (playing, command) in [
            (false, Command::Play),
            (true, Command::Stop),
            (false, Command::Play),
        ] {
            let frame = serde_json::json!({"jsonrpc":"2.0","method":"StateChanged","params":{"title":"First Light","bpm":120,"playing":playing}});
            let update = backend::decode(&frame.to_string()).unwrap().unwrap();
            studio.update(cx, |studio, cx| {
                studio.apply(update, cx);
                let state = studio.state.as_ref().unwrap();
                assert_eq!(state.title.as_deref(), Some("First Light"));
                assert_eq!(state.bpm, Some(120));
                assert_eq!(state.playing, playing);
                studio.toggle();
            });
            assert_eq!(requests.try_recv().unwrap(), command);
        }
    }
}
