//! Async task spawners for streaming, message sending, input polling, and ticks.

use crate::client::AgentRuntimeClient;
use crate::event::AppEvent;
use crossterm::event::{Event as CrosstermEvent, KeyModifiers, MouseEventKind};
use log::debug;
use odyssey_rs_protocol::Task;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

const INPUT_POLL_INTERVAL_MS: u64 = 30;
const MOUSE_SCROLL_LINES: i16 = 3;

/// Replace any existing stream task with a new one for `session_id`.
pub fn spawn_stream(
    client: Arc<AgentRuntimeClient>,
    session_id: Uuid,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) {
    if let Some(handle) = stream_handle.take() {
        handle.abort();
    }
    debug!("starting event stream (session_id={})", session_id);
    let handle = tokio::spawn(async move {
        if let Err(err) = client.stream_events(session_id, sender.clone()).await {
            let _ = sender.send(AppEvent::StreamError(err.to_string())).await;
        }
    });
    *stream_handle = Some(handle);
}

/// Spawn a background task to send a prompt to a session.
pub fn spawn_send_message(
    client: Arc<AgentRuntimeClient>,
    session_id: Uuid,
    prompt: String,
    agent_id: Option<String>,
    llm_id: String,
    sender: mpsc::Sender<AppEvent>,
) {
    let prompt_len = prompt.len();
    let agent_set = agent_id.is_some();
    tokio::spawn(async move {
        debug!(
            "dispatching send message (session_id={}, prompt_len={}, agent_set={})",
            session_id, prompt_len, agent_set
        );
        let task = Task::new(prompt);
        if let Err(err) = client
            .send_message(session_id, task, agent_id, llm_id)
            .await
        {
            let _ = sender
                .send(AppEvent::ActionError(format!("send message failed: {err}")))
                .await;
        }
    });
}

/// Spawn a background task to execute a direct sandbox command in a session.
pub fn spawn_run_command(
    client: Arc<AgentRuntimeClient>,
    session_id: Uuid,
    command_line: String,
    sender: mpsc::Sender<AppEvent>,
) {
    let command_len = command_line.len();
    tokio::spawn(async move {
        debug!(
            "dispatching session command (session_id={}, command_len={})",
            session_id, command_len
        );
        if let Err(err) = client.run_session_command(session_id, command_line).await {
            let _ = sender
                .send(AppEvent::ActionError(format!("run command failed: {err}")))
                .await;
        }
    });
}

fn translate_input_event(event: CrosstermEvent) -> Option<AppEvent> {
    match event {
        CrosstermEvent::Key(key) => Some(AppEvent::Input(key)),
        CrosstermEvent::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => Some(AppEvent::Scroll(-scroll_delta(mouse.modifiers))),
            MouseEventKind::ScrollDown => Some(AppEvent::Scroll(scroll_delta(mouse.modifiers))),
            _ => None,
        },
        _ => None,
    }
}

fn scroll_delta(modifiers: KeyModifiers) -> i16 {
    if modifiers.contains(KeyModifiers::SHIFT) {
        MOUSE_SCROLL_LINES.saturating_mul(2)
    } else {
        MOUSE_SCROLL_LINES
    }
}

/// Spawn a task that polls crossterm for keyboard and mouse events.
pub fn spawn_input_handler(sender: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        loop {
            if crossterm::event::poll(Duration::from_millis(INPUT_POLL_INTERVAL_MS))
                .unwrap_or(false)
            {
                // Drain all pending events before yielding
                while crossterm::event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    let event = match crossterm::event::read() {
                        Ok(e) => e,
                        Err(_) => break,
                    };
                    if let Some(app_event) = translate_input_event(event) {
                        let _ = sender.send(app_event).await;
                    }
                }
            }
        }
    });
}

/// Spawn a task that emits a [`AppEvent::Tick`] every 250 ms.
pub fn spawn_tick(sender: mpsc::Sender<AppEvent>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            interval.tick().await;
            let _ = sender.send(AppEvent::Tick).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{scroll_delta, spawn_tick, translate_input_event};
    use crate::event::AppEvent;
    use crossterm::event::{
        Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
    };
    use tokio::sync::mpsc;

    #[test]
    fn scroll_delta_doubles_when_shift_is_pressed() {
        assert_eq!(scroll_delta(KeyModifiers::NONE), 3);
        assert_eq!(scroll_delta(KeyModifiers::SHIFT), 6);
    }

    #[test]
    fn translate_input_event_maps_keys_and_scroll_events() {
        let key_event = translate_input_event(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        )));
        assert!(matches!(
            key_event,
            Some(AppEvent::Input(key)) if key.code == KeyCode::Char('j')
                && key.modifiers == KeyModifiers::CONTROL
        ));

        let scroll_up = translate_input_event(CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::SHIFT,
        }));
        assert!(matches!(scroll_up, Some(AppEvent::Scroll(-6))));

        let scroll_down = translate_input_event(CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(matches!(scroll_down, Some(AppEvent::Scroll(3))));
    }

    #[test]
    fn translate_input_event_ignores_non_scroll_mouse_events() {
        let event = translate_input_event(CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }));

        assert!(event.is_none());
    }

    #[tokio::test]
    async fn spawn_tick_emits_tick_events() {
        let (sender, mut receiver) = mpsc::channel(2);
        spawn_tick(sender);

        let event = tokio::time::timeout(std::time::Duration::from_millis(350), receiver.recv())
            .await
            .expect("tick should arrive before timeout");
        assert!(matches!(event, Some(AppEvent::Tick)));
    }
}
