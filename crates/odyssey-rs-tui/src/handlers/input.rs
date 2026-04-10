//! Keyboard input dispatch: routes key events to the appropriate handler.

use crate::app::{App, PendingPermission, ViewerKind};
use crate::client::AgentRuntimeClient;
use crate::event::AppEvent;
use crate::handlers::{agent, bundle, model, session, slash};
use crate::ui::theme::AVAILABLE_THEMES;
use crate::ui::widgets::input::PROMPT;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::info;
use odyssey_rs_protocol::ApprovalDecision;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthStr;

/// Handle a keyboard event.
///
/// Returns `true` when the application should exit.
pub async fn handle_input(
    key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    // Global quit / close shortcuts
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }
    if key.code == KeyCode::Esc {
        return handle_esc(app);
    }

    // Permission queue takes priority over everything else
    if let Some(permission) = app.pending_permissions.front().cloned()
        && matches!(
            key.code,
            KeyCode::Char('y') | KeyCode::Char('a') | KeyCode::Char('n')
        )
    {
        return handle_permission_input(key, client, app, permission).await;
    }

    // Viewer navigation
    if let Some(kind) = app.viewer {
        return handle_viewer_input(key, kind, client, app, sender, stream_handle).await;
    }

    // Normal input mode
    handle_normal_input(key, client, app, sender, stream_handle).await
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn handle_esc(app: &mut App) -> anyhow::Result<bool> {
    if app.viewer.is_some() {
        app.close_viewer();
        return Ok(false);
    }
    if app.show_slash_commands {
        app.show_slash_commands = false;
        app.input.clear();
        app.input_cursor = 0;
        return Ok(false);
    }
    Ok(true)
}

async fn handle_viewer_input(
    key: KeyEvent,
    kind: ViewerKind,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    match key.code {
        KeyCode::Up => handle_viewer_up(app, kind),
        KeyCode::Down => handle_viewer_down(app, kind),
        KeyCode::PageUp => app.viewer_scroll_up(5),
        KeyCode::PageDown => app.viewer_scroll_down(5),
        KeyCode::Home => app.viewer_scroll_up(u16::MAX),
        KeyCode::End => app.viewer_scroll_down(u16::MAX),
        KeyCode::Enter => {
            handle_viewer_enter(kind, client, app, sender, stream_handle).await?;
        }
        _ => {}
    }
    Ok(false)
}

fn handle_viewer_up(app: &mut App, kind: ViewerKind) {
    match kind {
        ViewerKind::Agents => decrement_selection(&mut app.selected_agent),
        ViewerKind::Bundles => decrement_selection(&mut app.selected_bundle),
        ViewerKind::Sessions => decrement_selection(&mut app.selected_session),
        ViewerKind::Skills => app.viewer_scroll_up(1),
        ViewerKind::Models => decrement_selection(&mut app.selected_model),
        ViewerKind::Themes => decrement_selection(&mut app.selected_theme),
    }
}

fn handle_viewer_down(app: &mut App, kind: ViewerKind) {
    match kind {
        ViewerKind::Agents => increment_selection(&mut app.selected_agent, app.agents.len()),
        ViewerKind::Bundles => increment_selection(&mut app.selected_bundle, app.bundles.len()),
        ViewerKind::Sessions => increment_selection(&mut app.selected_session, app.sessions.len()),
        ViewerKind::Skills => app.viewer_scroll_down(1),
        ViewerKind::Models => increment_selection(&mut app.selected_model, app.models.len()),
        ViewerKind::Themes => increment_selection(&mut app.selected_theme, AVAILABLE_THEMES.len()),
    }
}

async fn handle_viewer_enter(
    kind: ViewerKind,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<()> {
    match kind {
        ViewerKind::Agents => {
            agent::activate_selected_agent(app)?;
            app.close_viewer();
        }
        ViewerKind::Bundles => {
            bundle::activate_selected_bundle(client, app, sender, stream_handle)
                .await
                .map_err(anyhow::Error::msg)?;
            app.close_viewer();
        }
        ViewerKind::Sessions => {
            session::activate_selected_session(client, app, sender, stream_handle).await?;
            app.close_viewer();
        }
        ViewerKind::Models => {
            model::activate_selected_model(app)?;
            app.close_viewer();
        }
        ViewerKind::Themes => {
            app.apply_theme_at(app.selected_theme);
            let name = app.theme.name;
            app.push_status(format!("theme set: {name}"));
            app.close_viewer();
        }
        ViewerKind::Skills => {}
    }
    Ok(())
}

fn decrement_selection(selected: &mut usize) {
    if *selected > 0 {
        *selected -= 1;
    }
}

fn increment_selection(selected: &mut usize, len: usize) {
    if *selected + 1 < len {
        *selected += 1;
    }
}

async fn handle_normal_input(
    key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    // When the palette is open, intercept navigation / selection keys first.
    if app.show_slash_commands
        && handle_palette_key(key, client, app, sender.clone(), stream_handle).await?
    {
        return Ok(false);
    }

    if handle_normal_shortcut_key(key, client, app, &sender, stream_handle).await? {
        return Ok(false);
    }

    if handle_navigation_key(key, app) {
        return Ok(false);
    }

    if key.code == KeyCode::Enter {
        handle_enter(key, client, app, sender, stream_handle).await?;
        return Ok(false);
    }

    if handle_edit_key(key, app) {
        return Ok(false);
    }

    Ok(false)
}

async fn handle_normal_shortcut_key(
    key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: &mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    match key.code {
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            session::create_session(client, app, sender.clone(), stream_handle).await?;
            Ok(true)
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            session::refresh_sessions(client, app).await?;
            Ok(true)
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            session::activate_selected_session(client, app, sender.clone(), stream_handle).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_navigation_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Tab => {
            app.open_viewer(ViewerKind::Agents);
            true
        }
        KeyCode::PageUp => {
            app.scroll_up(5);
            true
        }
        KeyCode::PageDown => {
            app.scroll_down(5);
            true
        }
        KeyCode::Left => {
            handle_left_key(app);
            true
        }
        KeyCode::Right => {
            handle_right_key(app);
            true
        }
        KeyCode::Up => {
            handle_up_key(app);
            true
        }
        KeyCode::Down => {
            handle_down_key(app);
            true
        }
        KeyCode::Home => {
            handle_home_key(app);
            true
        }
        KeyCode::End => {
            handle_end_key(app);
            true
        }
        _ => false,
    }
}

fn handle_left_key(app: &mut App) {
    if !app.input.is_empty() {
        move_cursor_left(app);
    }
}

fn handle_right_key(app: &mut App) {
    if !app.input.is_empty() {
        move_cursor_right(app);
    }
}

fn handle_up_key(app: &mut App) {
    if app.input.is_empty() || (app.history_index.is_some() && cursor_on_first_line(app)) {
        history_up(app);
    } else {
        move_cursor_up(app);
    }
}

fn handle_down_key(app: &mut App) {
    if app.history_index.is_some() && cursor_on_last_line(app) {
        history_down(app);
    } else if !app.input.is_empty() {
        move_cursor_down(app);
    } else {
        app.scroll_down(1);
    }
}

fn handle_home_key(app: &mut App) {
    if !app.input.is_empty() {
        app.input_cursor = 0;
    } else {
        app.scroll_to_top();
    }
}

fn handle_end_key(app: &mut App) {
    if !app.input.is_empty() {
        app.input_cursor = app.input.len();
    } else {
        app.enable_auto_scroll();
    }
}

fn handle_edit_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Backspace => {
            handle_backspace(app);
            true
        }
        KeyCode::Delete => {
            handle_delete(app);
            true
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_input_char(app, ch);
            true
        }
        _ => false,
    }
}

fn handle_backspace(app: &mut App) {
    if app.input_cursor > 0 {
        let prev = prev_char_boundary(&app.input, app.input_cursor);
        app.input.drain(prev..app.input_cursor);
        app.input_cursor = prev;
        refresh_input_edit_state(app, true);
    }
}

fn handle_delete(app: &mut App) {
    if app.input_cursor < app.input.len() {
        let next = next_char_boundary(&app.input, app.input_cursor);
        app.input.drain(app.input_cursor..next);
        refresh_input_edit_state(app, false);
    }
}

fn insert_input_char(app: &mut App, ch: char) {
    app.input.insert(app.input_cursor, ch);
    app.input_cursor += ch.len_utf8();
    refresh_input_edit_state(app, true);
}

fn refresh_input_edit_state(app: &mut App, clear_history_index: bool) {
    app.show_slash_commands = app.input.trim_start().starts_with('/');
    app.slash_selected = 0;
    if clear_history_index {
        app.history_index = None;
    }
}

/// Handle a key press while the slash palette is visible.
///
/// Returns `true` when the key was consumed (caller should skip normal handling).
async fn handle_palette_key(
    key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<bool> {
    let filtered = slash::filtered_commands(&app.input);
    let count = filtered.len();

    match key.code {
        KeyCode::Up => {
            if app.slash_selected > 0 {
                app.slash_selected -= 1;
            }
            Ok(true)
        }
        KeyCode::Down => {
            if count > 0 && app.slash_selected + 1 < count {
                app.slash_selected += 1;
            }
            Ok(true)
        }
        KeyCode::Tab | KeyCode::Enter if count > 0 => {
            let entry = filtered[app.slash_selected.min(count - 1)];
            app.show_slash_commands = false;
            app.slash_selected = 0;

            if entry.args.is_empty() {
                // No arguments needed — execute the command immediately.
                let command = format!("/{}", entry.trigger);
                app.input.clear();
                app.input_cursor = 0;
                if let Err(err) =
                    slash::handle_slash_command(client, app, sender, stream_handle, command).await
                {
                    app.push_system_message(err);
                }
            } else {
                // Command needs arguments — complete the trigger and let the
                // user type the rest.
                app.input = format!("/{} ", entry.trigger);
                app.input_cursor = app.input.len();
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

// ── History helpers ──────────────────────────────────────────────────────────

/// Whether the cursor is on the first visual line of the input.
fn cursor_on_first_line(app: &App) -> bool {
    let width = app.input_inner_width.max(1) as usize;
    let prompt_w = UnicodeWidthStr::width(PROMPT);
    let (line, _) = visual_pos(&app.input, app.input_cursor, width, prompt_w);
    line == 0
}

/// Whether the cursor is on the last visual line of the input.
fn cursor_on_last_line(app: &App) -> bool {
    let width = app.input_inner_width.max(1) as usize;
    let prompt_w = UnicodeWidthStr::width(PROMPT);
    let (line, _) = visual_pos(&app.input, app.input_cursor, width, prompt_w);
    let total = crate::ui::input_line_count(&app.input, width) as usize;
    line + 1 >= total
}

fn history_up(app: &mut App) {
    if app.history.is_empty() {
        return;
    }
    let new_idx = match app.history_index {
        Some(idx) if idx > 0 => idx - 1,
        Some(0) => return, // already at oldest
        None => {
            // Start browsing: save current input as draft.
            app.history_draft.clone_from(&app.input);
            app.history.len() - 1
        }
        _ => return,
    };
    app.history_index = Some(new_idx);
    app.input.clone_from(&app.history[new_idx]);
    app.input_cursor = app.input.len();
}

fn history_down(app: &mut App) {
    let Some(idx) = app.history_index else {
        return;
    };
    if idx + 1 < app.history.len() {
        let new_idx = idx + 1;
        app.history_index = Some(new_idx);
        app.input.clone_from(&app.history[new_idx]);
        app.input_cursor = app.input.len();
    } else {
        // Past the newest entry — restore draft.
        app.history_index = None;
        app.input = std::mem::take(&mut app.history_draft);
        app.input_cursor = app.input.len();
    }
}

// ── Cursor movement helpers ──────────────────────────────────────────────────

/// Return the byte index of the previous character boundary before `pos`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut i = pos.saturating_sub(1);
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Return the byte index of the next character boundary after `pos`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut i = pos + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

fn move_cursor_left(app: &mut App) {
    if app.input_cursor > 0 {
        app.input_cursor = prev_char_boundary(&app.input, app.input_cursor);
    }
}

fn move_cursor_right(app: &mut App) {
    if app.input_cursor < app.input.len() {
        app.input_cursor = next_char_boundary(&app.input, app.input_cursor);
    }
}

/// Move cursor up one visual line, keeping the same visual column.
fn move_cursor_up(app: &mut App) {
    let width = app.input_inner_width.max(1) as usize;
    let prompt_w = UnicodeWidthStr::width(PROMPT);

    // Compute the visual column and line of the current cursor.
    let (cursor_line, cursor_col) = visual_pos(&app.input, app.input_cursor, width, prompt_w);

    if cursor_line == 0 {
        // Already on the first line — move cursor to start.
        app.input_cursor = 0;
        return;
    }

    // Find the byte offset that corresponds to (cursor_line - 1, cursor_col).
    app.input_cursor =
        byte_offset_for_visual(&app.input, cursor_line - 1, cursor_col, width, prompt_w);
}

/// Move cursor down one visual line, keeping the same visual column.
fn move_cursor_down(app: &mut App) {
    let width = app.input_inner_width.max(1) as usize;
    let prompt_w = UnicodeWidthStr::width(PROMPT);

    let (cursor_line, cursor_col) = visual_pos(&app.input, app.input_cursor, width, prompt_w);

    // Total number of visual lines.
    let total_lines = crate::ui::input_line_count(&app.input, width) as usize;
    if cursor_line + 1 >= total_lines {
        // Already on the last line — move cursor to end.
        app.input_cursor = app.input.len();
        return;
    }

    app.input_cursor =
        byte_offset_for_visual(&app.input, cursor_line + 1, cursor_col, width, prompt_w);
}

/// Compute (visual_line, visual_col) for a given byte offset in the input.
fn visual_pos(input: &str, byte_offset: usize, width: usize, prompt_w: usize) -> (usize, usize) {
    let mut line = 0usize;
    let mut col = prompt_w;

    for (i, ch) in input.char_indices() {
        if i >= byte_offset {
            break;
        }
        let ch_w = UnicodeWidthStr::width(ch.encode_utf8(&mut [0u8; 4]) as &str);
        if col + ch_w > width && col > 0 {
            line += 1;
            col = 0;
        }
        col += ch_w;
    }
    (line, col)
}

/// Find the byte offset in `input` that corresponds to (target_line, target_col).
/// Clamps to the end of the target line if `target_col` is beyond the line length.
fn byte_offset_for_visual(
    input: &str,
    target_line: usize,
    target_col: usize,
    width: usize,
    prompt_w: usize,
) -> usize {
    let mut line = 0usize;
    let mut col = prompt_w;
    let mut last_byte = 0usize;

    for (i, ch) in input.char_indices() {
        let ch_w = UnicodeWidthStr::width(ch.encode_utf8(&mut [0u8; 4]) as &str);
        if col + ch_w > width && col > 0 {
            if line == target_line {
                // We were on the target line but hit the end before reaching target_col.
                return i;
            }
            line += 1;
            col = 0;
        }
        if line == target_line && col >= target_col {
            return i;
        }
        col += ch_w;
        last_byte = i + ch.len_utf8();
    }
    // Past all characters — return end.
    last_byte
}

async fn handle_enter(
    _key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<()> {
    if app.input.trim().is_empty() {
        app.show_slash_commands = false;
        return Ok(());
    }
    // Save to history and reset browsing state.
    crate::history::push(&app.input);
    app.history.push(app.input.clone());
    app.history_index = None;
    app.history_draft.clear();

    app.show_slash_commands = false;
    if app.input.trim_start().starts_with('/') {
        let command = std::mem::take(&mut app.input);
        app.input_cursor = 0;
        if let Err(err) =
            slash::handle_slash_command(client, app, sender, stream_handle, command).await
        {
            app.push_system_message(err);
        }
    } else if app.input.trim_start().starts_with('!') {
        session::send_command(client, app, sender).await?;
    } else {
        session::send_message(client, app, sender).await?;
    }
    Ok(())
}

/// Handle keyboard input for a pending permission prompt.
pub async fn handle_permission_input(
    key: KeyEvent,
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    permission: PendingPermission,
) -> anyhow::Result<bool> {
    let decision = match key.code {
        KeyCode::Char('y') => Some(ApprovalDecision::AllowOnce),
        KeyCode::Char('a') => Some(ApprovalDecision::AllowAlways),
        KeyCode::Char('n') => Some(ApprovalDecision::Deny),
        KeyCode::Esc => {
            app.pending_permissions.pop_front();
            return Ok(false);
        }
        _ => None,
    };
    if let Some(decision) = decision {
        info!(
            "sending permission decision (request_id={}, decision={:?})",
            permission.request_id, decision
        );
        let resolved = client
            .resolve_permission(permission.request_id, decision)
            .await
            .unwrap_or(false);
        app.pending_permissions.pop_front();
        app.push_status(if resolved {
            "permission sent"
        } else {
            "permission request not found"
        });
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{
        byte_offset_for_visual, cursor_on_first_line, cursor_on_last_line, decrement_selection,
        handle_down_key, handle_edit_key, handle_end_key, handle_esc, handle_home_key,
        handle_input, handle_navigation_key, handle_normal_input, handle_normal_shortcut_key,
        handle_palette_key, handle_permission_input, handle_up_key, handle_viewer_down,
        handle_viewer_enter, handle_viewer_input, handle_viewer_up, history_down, history_up,
        increment_selection, move_cursor_left, move_cursor_right, next_char_boundary,
        prev_char_boundary, visual_pos,
    };
    use crate::app::{App, PendingPermission, ViewerKind};
    use crate::client::AgentRuntimeClient;
    use crate::event::AppEvent;
    use crate::handlers;
    use crate::ui::widgets::input::PROMPT;
    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use odyssey_rs_bundle::BundleInstallSummary;
    use odyssey_rs_protocol::{DEFAULT_HUB_URL, SandboxMode, SessionSummary};
    use odyssey_rs_runtime::{RuntimeConfig, RuntimeEngine};
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn runtime_config(root: &Path) -> RuntimeConfig {
        RuntimeConfig {
            cache_root: root.join("cache"),
            session_root: root.join("sessions"),
            sandbox_root: root.join("sandbox"),
            bind_addr: "127.0.0.1:0".to_string(),
            sandbox_mode_override: Some(SandboxMode::DangerFullAccess),
            hub_url: DEFAULT_HUB_URL.to_string(),
            worker_count: 2,
            queue_capacity: 32,
            ..RuntimeConfig::default()
        }
    }

    fn write_bundle_project(root: &Path, bundle_id: &str, agent_id: &str) {
        let agent_root = root.join("agents").join(agent_id);
        fs::create_dir_all(root.join("skills").join("repo-hygiene")).expect("create skill dir");
        fs::create_dir_all(root.join("resources").join("data")).expect("create data dir");
        fs::create_dir_all(&agent_root).expect("create agent dir");
        fs::write(
            root.join("odyssey.bundle.yaml"),
            format!(
                r#"apiVersion: odyssey.ai/bundle.v1
kind: AgentBundle
metadata:
  name: {bundle_id}
  version: 0.1.0
  readme: README.md
spec:
  abiVersion: v1
  skills:
    - name: repo-hygiene
      path: skills/repo-hygiene
  tools:
    - name: Read
      source: builtin
  sandbox:
    permissions:
      filesystem:
        exec: []
        mounts:
          read: []
          write: []
      network: ["*"]
    system_tools: ["sh"]
    resources: {{}}
  agents:
    - id: {agent_id}
      spec: agents/{agent_id}/agent.yaml
      default: true
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            agent_root.join("agent.yaml"),
            format!(
                r#"apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: {agent_id}
  version: 0.1.0
  description: test bundle
spec:
  kind: prompt
  abiVersion: v1
  prompt: keep responses concise
  model:
    provider: openai
    name: gpt-4.1-mini
  tools:
    allow: ["Read", "Skill"]
"#
            ),
        )
        .expect("write agent");
        fs::write(root.join("README.md"), format!("# {bundle_id}\n")).expect("write readme");
        fs::write(
            root.join("skills").join("repo-hygiene").join("SKILL.md"),
            "Keep commits focused.\n",
        )
        .expect("write skill");
        fs::write(
            root.join("resources").join("data").join("notes.txt"),
            "hello world\n",
        )
        .expect("write resource");
    }

    fn client(runtime_root: &Path, bundle_ref: &str) -> Arc<AgentRuntimeClient> {
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(runtime_root)).expect("runtime"));
        Arc::new(AgentRuntimeClient::new(runtime, bundle_ref.to_string()))
    }

    #[test]
    fn history_up_saves_draft_and_loads_latest_entry() {
        let mut app = App {
            input: "draft".to_string(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            ..App::default()
        };

        history_up(&mut app);

        assert_eq!(app.history_index, Some(1));
        assert_eq!(app.history_draft, "draft");
        assert_eq!(app.input, "latest");
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[test]
    fn history_down_restores_saved_draft_after_newest_entry() {
        let mut app = App {
            input: "latest".to_string(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            history_index: Some(1),
            history_draft: "draft".to_string(),
            ..App::default()
        };

        history_down(&mut app);

        assert_eq!(app.history_index, None);
        assert_eq!(app.history_draft, "");
        assert_eq!(app.input, "draft");
        assert_eq!(app.input_cursor, app.input.len());
    }

    #[tokio::test]
    async fn enter_routes_bang_prefix_to_session_command_execution() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(temp.path())).expect("runtime"));
        let project = temp.path().join("alpha-project");
        fs::create_dir_all(&project).expect("create project");
        write_bundle_project(&project, "alpha", "alpha-agent");
        runtime.build_and_install(&project).expect("install bundle");

        let client = Arc::new(AgentRuntimeClient::new(
            runtime,
            "local/alpha@0.1.0".to_string(),
        ));
        let mut app = App {
            bundle_ref: "local/alpha@0.1.0".to_string(),
            input: "!printf input-route".to_string(),
            ..App::default()
        };
        let (sender, mut receiver) = mpsc::channel::<AppEvent>(32);
        let mut stream_handle = None;

        handlers::session::create_session(&client, &mut app, sender.clone(), &mut stream_handle)
            .await
            .expect("create session");

        let exit = handle_input(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("handle enter");

        assert!(!exit);
        assert_eq!(app.input, "");
        assert_eq!(app.status, "running");
        assert!(app.messages.is_empty());

        let mut saw_begin = false;
        let mut saw_end = false;
        for _ in 0..4 {
            let Some(event) = receiver.recv().await else {
                break;
            };
            if let AppEvent::Server(message) = event {
                match message.payload {
                    odyssey_rs_protocol::EventPayload::ExecCommandBegin { .. } => {
                        saw_begin = true;
                    }
                    odyssey_rs_protocol::EventPayload::ExecCommandEnd { .. } => {
                        saw_end = true;
                        break;
                    }
                    _ => {}
                }
            }
        }

        assert!(saw_begin);
        assert!(saw_end);
        if let Some(handle) = stream_handle.take() {
            handle.abort();
        }
    }

    #[test]
    fn handle_esc_closes_viewers_and_palettes_before_exiting() {
        let mut viewer_app = App {
            viewer: Some(ViewerKind::Bundles),
            ..App::default()
        };
        assert!(!handle_esc(&mut viewer_app).expect("viewer escape"));
        assert!(viewer_app.viewer.is_none());

        let mut palette_app = App {
            show_slash_commands: true,
            input: "/bundle".to_string(),
            input_cursor: "/bundle".len(),
            ..App::default()
        };
        assert!(!handle_esc(&mut palette_app).expect("palette escape"));
        assert!(!palette_app.show_slash_commands);
        assert_eq!(palette_app.input, "");
        assert_eq!(palette_app.input_cursor, 0);

        assert!(handle_esc(&mut App::default()).expect("plain escape"));
    }

    #[test]
    fn selection_helpers_clamp_to_available_items() {
        let mut selected = 0usize;
        decrement_selection(&mut selected);
        assert_eq!(selected, 0);

        increment_selection(&mut selected, 3);
        assert_eq!(selected, 1);
        increment_selection(&mut selected, 3);
        increment_selection(&mut selected, 3);
        assert_eq!(selected, 2);
    }

    #[test]
    fn cursor_boundary_helpers_handle_multibyte_characters() {
        let text = "aé🙂";
        assert_eq!(prev_char_boundary(text, text.len()), "aé".len());
        assert_eq!(next_char_boundary(text, 1), "aé".len());

        let mut app = App {
            input: text.to_string(),
            input_cursor: text.len(),
            ..App::default()
        };
        move_cursor_left(&mut app);
        assert_eq!(app.input_cursor, "aé".len());
        move_cursor_right(&mut app);
        assert_eq!(app.input_cursor, text.len());
    }

    #[test]
    fn visual_position_helpers_track_wrapped_input() {
        let width = 6usize;
        let prompt_width = unicode_width::UnicodeWidthStr::width(PROMPT);

        assert_eq!(visual_pos("abcdef", 5, width, prompt_width), (1, 1));
        assert_eq!(
            byte_offset_for_visual("abcdef", 1, 0, width, prompt_width),
            4
        );
        assert_eq!(
            byte_offset_for_visual("abcdef", 9, 9, width, prompt_width),
            6
        );
        assert_eq!(
            byte_offset_for_visual("abcdef", 0, 99, width, prompt_width),
            4
        );
    }

    #[test]
    fn cursor_line_helpers_reflect_wrapped_input_boundaries() {
        let mut app = App {
            input: "abcdef".to_string(),
            input_inner_width: 6,
            input_cursor: 3,
            ..App::default()
        };
        assert!(cursor_on_first_line(&app));
        assert!(!cursor_on_last_line(&app));

        app.input_cursor = app.input.len();
        assert!(!cursor_on_first_line(&app));
        assert!(cursor_on_last_line(&app));
    }

    #[test]
    fn navigation_keys_open_viewers_and_move_the_input_cursor() {
        let mut app = App {
            input: "abc".to_string(),
            input_cursor: 1,
            ..App::default()
        };

        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &mut app
        ));
        assert_eq!(app.input_cursor, 0);
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut app
        ));
        assert_eq!(app.input_cursor, 1);

        let mut scroll_app = App {
            scroll: 5,
            chat_max_scroll: 10,
            auto_scroll: false,
            ..App::default()
        };
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &mut scroll_app
        ));
        assert_eq!(scroll_app.scroll, 0);
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            &mut scroll_app
        ));
        assert_eq!(scroll_app.scroll, 5);

        let mut vertical_app = App {
            input: "abcdef".to_string(),
            input_inner_width: 6,
            input_cursor: 6,
            ..App::default()
        };
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut vertical_app
        ));
        assert!(vertical_app.input_cursor < 6);
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &mut vertical_app
        ));
        assert_eq!(vertical_app.input_cursor, vertical_app.input.len());

        let mut endpoint_app = App {
            scroll: 4,
            chat_max_scroll: 9,
            auto_scroll: false,
            ..App::default()
        };
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            &mut endpoint_app
        ));
        assert_eq!(endpoint_app.scroll, 0);
        assert!(!endpoint_app.auto_scroll);
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            &mut endpoint_app
        ));
        assert_eq!(endpoint_app.scroll, 9);
        assert!(endpoint_app.auto_scroll);

        let mut viewer_app = App::default();
        assert!(handle_navigation_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut viewer_app
        ));
        assert_eq!(viewer_app.viewer, Some(ViewerKind::Agents));
    }

    #[test]
    fn viewer_helpers_update_each_selection_family() {
        let session_id = uuid::Uuid::new_v4();
        let mut app = App {
            agents: vec!["alpha".to_string(), "beta".to_string()],
            bundles: vec![
                BundleInstallSummary {
                    namespace: "local".to_string(),
                    id: "alpha".to_string(),
                    version: "0.1.0".to_string(),
                    path: "/workspace/alpha".into(),
                },
                BundleInstallSummary {
                    namespace: "local".to_string(),
                    id: "beta".to_string(),
                    version: "0.1.0".to_string(),
                    path: "/workspace/beta".into(),
                },
            ],
            sessions: vec![
                SessionSummary {
                    id: session_id,
                    agent_id: "alpha".to_string(),
                    message_count: 1,
                    created_at: Utc::now(),
                },
                SessionSummary {
                    id: uuid::Uuid::new_v4(),
                    agent_id: "beta".to_string(),
                    message_count: 2,
                    created_at: Utc::now(),
                },
            ],
            models: vec!["gpt-4.1-mini".to_string(), "gpt-4.1".to_string()],
            selected_agent: 1,
            selected_bundle: 1,
            selected_session: 1,
            selected_model: 1,
            selected_theme: 1,
            viewer_scroll: 2,
            viewer_max_scroll: 4,
            ..App::default()
        };

        handle_viewer_up(&mut app, ViewerKind::Agents);
        handle_viewer_up(&mut app, ViewerKind::Bundles);
        handle_viewer_up(&mut app, ViewerKind::Sessions);
        handle_viewer_up(&mut app, ViewerKind::Models);
        handle_viewer_up(&mut app, ViewerKind::Themes);
        handle_viewer_up(&mut app, ViewerKind::Skills);

        assert_eq!(app.selected_agent, 0);
        assert_eq!(app.selected_bundle, 0);
        assert_eq!(app.selected_session, 0);
        assert_eq!(app.selected_model, 0);
        assert_eq!(app.selected_theme, 0);
        assert_eq!(app.viewer_scroll, 1);

        handle_viewer_down(&mut app, ViewerKind::Agents);
        handle_viewer_down(&mut app, ViewerKind::Bundles);
        handle_viewer_down(&mut app, ViewerKind::Sessions);
        handle_viewer_down(&mut app, ViewerKind::Models);
        handle_viewer_down(&mut app, ViewerKind::Themes);
        handle_viewer_down(&mut app, ViewerKind::Skills);

        assert_eq!(app.selected_agent, 1);
        assert_eq!(app.selected_bundle, 1);
        assert_eq!(app.selected_session, 1);
        assert_eq!(app.selected_model, 1);
        assert_eq!(app.selected_theme, 1);
        assert_eq!(app.viewer_scroll, 2);
    }

    #[tokio::test]
    async fn viewer_input_scrolls_and_enters_non_remote_viewers() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(4);
        let mut app = App {
            viewer: Some(ViewerKind::Models),
            agents: vec!["alpha".to_string(), "beta".to_string()],
            models: vec!["gpt-4.1-mini".to_string(), "gpt-4.1".to_string()],
            selected_agent: 1,
            selected_model: 1,
            viewer_scroll: 3,
            viewer_max_scroll: 9,
            ..App::default()
        };
        let mut stream_handle = None;

        assert!(
            !handle_viewer_input(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                ViewerKind::Models,
                &client,
                &mut app,
                sender.clone(),
                &mut stream_handle,
            )
            .await
            .expect("viewer up")
        );
        assert_eq!(app.selected_model, 0);

        assert!(
            !handle_viewer_input(
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                ViewerKind::Skills,
                &client,
                &mut app,
                sender.clone(),
                &mut stream_handle,
            )
            .await
            .expect("viewer page down")
        );
        assert_eq!(app.viewer_scroll, 8);

        handle_viewer_input(
            KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            ViewerKind::Skills,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("viewer home");
        assert_eq!(app.viewer_scroll, 0);

        handle_viewer_input(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
            ViewerKind::Skills,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("viewer end");
        assert_eq!(app.viewer_scroll, 9);
        handle_viewer_input(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            ViewerKind::Skills,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("viewer page up");
        assert_eq!(app.viewer_scroll, 4);

        handle_viewer_input(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ViewerKind::Models,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("viewer enter");
        assert_eq!(app.model_id, "gpt-4.1-mini");
        assert_eq!(app.status, "model set: gpt-4.1-mini");
        assert!(app.viewer.is_none());

        app.viewer = Some(ViewerKind::Agents);
        app.selected_agent = 1;
        handle_viewer_enter(
            ViewerKind::Agents,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("agent enter");
        assert_eq!(app.active_agent.as_deref(), Some("beta"));
        assert_eq!(app.status, "agent set: beta");
        assert!(app.viewer.is_none());

        app.viewer = Some(ViewerKind::Skills);
        handle_viewer_enter(
            ViewerKind::Skills,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("skills enter");
        assert_eq!(app.viewer, Some(ViewerKind::Skills));

        handle_viewer_input(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            ViewerKind::Skills,
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("unknown viewer key");
        assert_eq!(app.viewer, Some(ViewerKind::Skills));
    }

    #[tokio::test]
    async fn viewer_enter_activates_selected_bundle_and_session() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(temp.path())).expect("runtime"));
        let project = temp.path().join("alpha-project");
        write_bundle_project(&project, "alpha", "alpha-agent");
        runtime.build_and_install(&project).expect("install bundle");

        let client = Arc::new(AgentRuntimeClient::new(runtime, String::default()));
        let bundles = client.list_bundles().await.expect("list bundles");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(16);
        let mut app = App {
            viewer: Some(ViewerKind::Bundles),
            bundles,
            ..App::default()
        };
        let mut stream_handle = None;

        handle_viewer_enter(
            ViewerKind::Bundles,
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("bundle enter");
        assert_eq!(app.bundle_ref, "local/alpha@0.1.0");
        assert!(app.active_session.is_some());
        assert_eq!(app.status, "bundle set: local/alpha@0.1.0");
        assert!(app.viewer.is_none());

        let active_session = app.active_session.expect("active session");
        app.sessions = client.list_sessions().await.expect("list sessions");
        app.viewer = Some(ViewerKind::Sessions);
        app.selected_session = app
            .sessions
            .iter()
            .position(|session| session.id == active_session)
            .expect("selected session");

        handle_viewer_enter(
            ViewerKind::Sessions,
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("session enter");
        assert_eq!(app.active_session, Some(active_session));
        assert_eq!(app.status, "session selected");
        assert!(app.viewer.is_none());

        if let Some(handle) = stream_handle.take() {
            handle.abort();
        }
    }

    #[test]
    fn home_end_and_edit_keys_update_input_and_scroll_state() {
        let mut app = App {
            input: "hé".to_string(),
            input_cursor: "h".len(),
            history_index: Some(0),
            ..App::default()
        };

        assert!(handle_edit_key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut app
        ));
        assert_eq!(app.input, "é");
        assert_eq!(app.input_cursor, 0);
        assert_eq!(app.history_index, None);

        app.input = "ab".to_string();
        app.input_cursor = 1;
        app.history_index = Some(1);
        assert!(handle_edit_key(
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            &mut app
        ));
        assert_eq!(app.input, "a");
        assert_eq!(app.input_cursor, 1);
        assert_eq!(app.history_index, Some(1));

        app.input.clear();
        app.input_cursor = 0;
        app.history_index = Some(2);
        assert!(handle_edit_key(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut app
        ));
        assert_eq!(app.input, "/");
        assert!(app.show_slash_commands);
        assert_eq!(app.history_index, None);

        assert!(!handle_edit_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            &mut app
        ));

        let mut non_empty = App {
            input: "hello".to_string(),
            input_cursor: 2,
            ..App::default()
        };
        handle_home_key(&mut non_empty);
        assert_eq!(non_empty.input_cursor, 0);
        handle_end_key(&mut non_empty);
        assert_eq!(non_empty.input_cursor, non_empty.input.len());

        let mut empty = App {
            scroll: 4,
            chat_max_scroll: 9,
            auto_scroll: true,
            ..App::default()
        };
        handle_home_key(&mut empty);
        assert_eq!(empty.scroll, 0);
        assert!(!empty.auto_scroll);
        handle_end_key(&mut empty);
        assert_eq!(empty.scroll, 9);
        assert!(empty.auto_scroll);
    }

    #[test]
    fn up_down_keys_choose_between_history_cursor_and_chat_scroll() {
        let prompt_width = unicode_width::UnicodeWidthStr::width(PROMPT);
        let mut history_app = App {
            input: String::default(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            ..App::default()
        };
        handle_up_key(&mut history_app);
        assert_eq!(history_app.history_index, Some(1));
        assert_eq!(history_app.input, "latest");

        let mut move_up_app = App {
            input: "abcdef".to_string(),
            input_inner_width: 6,
            input_cursor: 6,
            ..App::default()
        };
        let (_, cursor_col) = visual_pos(
            &move_up_app.input,
            move_up_app.input_cursor,
            move_up_app.input_inner_width as usize,
            prompt_width,
        );
        let expected_up = byte_offset_for_visual(
            &move_up_app.input,
            0,
            cursor_col,
            move_up_app.input_inner_width as usize,
            prompt_width,
        );
        handle_up_key(&mut move_up_app);
        assert_eq!(move_up_app.input_cursor, expected_up);

        let mut history_down_app = App {
            input: "latest".to_string(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            history_index: Some(1),
            history_draft: "draft".to_string(),
            input_inner_width: 20,
            ..App::default()
        };
        handle_down_key(&mut history_down_app);
        assert_eq!(history_down_app.history_index, None);
        assert_eq!(history_down_app.input, "draft");

        let mut move_down_app = App {
            input: "abcdef".to_string(),
            input_inner_width: 6,
            input_cursor: 1,
            ..App::default()
        };
        let (_, cursor_col) = visual_pos(
            &move_down_app.input,
            move_down_app.input_cursor,
            move_down_app.input_inner_width as usize,
            prompt_width,
        );
        let expected_down = byte_offset_for_visual(
            &move_down_app.input,
            1,
            cursor_col,
            move_down_app.input_inner_width as usize,
            prompt_width,
        );
        handle_down_key(&mut move_down_app);
        assert_eq!(move_down_app.input_cursor, expected_down);

        let mut scroll_app = App {
            chat_max_scroll: 3,
            auto_scroll: false,
            ..App::default()
        };
        handle_down_key(&mut scroll_app);
        assert_eq!(scroll_app.scroll, 1);

        let mut first_line_app = App {
            input: "abc".to_string(),
            input_cursor: 1,
            ..App::default()
        };
        handle_up_key(&mut first_line_app);
        assert_eq!(first_line_app.input_cursor, 0);

        let mut last_line_app = App {
            input: "abcdef".to_string(),
            input_inner_width: 6,
            input_cursor: 6,
            ..App::default()
        };
        handle_down_key(&mut last_line_app);
        assert_eq!(last_line_app.input_cursor, last_line_app.input.len());
    }

    #[test]
    fn history_helpers_cover_noop_and_incremental_branches() {
        let mut empty_history = App {
            history: Vec::new(),
            ..App::default()
        };
        history_up(&mut empty_history);
        assert_eq!(empty_history.history_index, None);

        let mut oldest = App {
            input: "oldest".to_string(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            history_index: Some(0),
            ..App::default()
        };
        history_up(&mut oldest);
        assert_eq!(oldest.history_index, Some(0));
        assert_eq!(oldest.input, "oldest");

        let mut no_index = App {
            history: Vec::new(),
            ..App::default()
        };
        history_down(&mut no_index);
        assert_eq!(no_index.history_index, None);

        let mut next_entry = App {
            input: "oldest".to_string(),
            history: vec!["oldest".to_string(), "latest".to_string()],
            history_index: Some(0),
            ..App::default()
        };
        history_down(&mut next_entry);
        assert_eq!(next_entry.history_index, Some(1));
        assert_eq!(next_entry.input, "latest");
    }

    #[tokio::test]
    async fn normal_input_handles_palette_shortcuts_edits_and_unknown_keys() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(8);
        let mut stream_handle = None;

        let mut palette_app = App {
            show_slash_commands: true,
            input: "/".to_string(),
            slash_selected: 1,
            ..App::default()
        };
        let exit = handle_normal_input(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &client,
            &mut palette_app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("palette normal input");
        assert!(!exit);
        assert_eq!(palette_app.slash_selected, 0);

        let mut edit_app = App::default();
        let exit = handle_normal_input(
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            &client,
            &mut edit_app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("edit normal input");
        assert!(!exit);
        assert_eq!(edit_app.input, "/");
        assert!(edit_app.show_slash_commands);

        let mut unknown_app = App::default();
        let exit = handle_normal_input(
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
            &client,
            &mut unknown_app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("unknown normal input");
        assert!(!exit);
        assert!(unknown_app.input.is_empty());
    }

    #[tokio::test]
    async fn normal_shortcut_keys_manage_sessions() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(temp.path())).expect("runtime"));
        let project = temp.path().join("alpha-project");
        write_bundle_project(&project, "alpha", "alpha-agent");
        runtime.build_and_install(&project).expect("install bundle");

        let client = Arc::new(AgentRuntimeClient::new(
            runtime,
            "local/alpha@0.1.0".to_string(),
        ));
        let (sender, _receiver) = mpsc::channel::<AppEvent>(16);
        let mut app = App {
            bundle_ref: "local/alpha@0.1.0".to_string(),
            agents: vec!["alpha-agent".to_string()],
            active_agent: Some("alpha-agent".to_string()),
            ..App::default()
        };
        let mut stream_handle = None;

        assert!(
            handle_normal_shortcut_key(
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
                &client,
                &mut app,
                &sender,
                &mut stream_handle,
            )
            .await
            .expect("create session shortcut")
        );
        let created_session = app.active_session.expect("created session");
        assert_eq!(app.status, "session created");

        app.sessions.clear();
        assert!(
            handle_normal_shortcut_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &client,
                &mut app,
                &sender,
                &mut stream_handle,
            )
            .await
            .expect("refresh sessions shortcut")
        );
        assert_eq!(app.sessions.len(), 1);

        app.active_session = None;
        app.selected_session = 0;
        assert!(
            handle_normal_shortcut_key(
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                &client,
                &mut app,
                &sender,
                &mut stream_handle,
            )
            .await
            .expect("select session shortcut")
        );
        assert_eq!(app.active_session, Some(created_session));
        assert_eq!(app.status, "session selected");

        if let Some(handle) = stream_handle.take() {
            handle.abort();
        }
    }

    #[tokio::test]
    async fn palette_selection_completes_commands_that_require_arguments() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(4);
        let mut app = App {
            show_slash_commands: true,
            input: "/".to_string(),
            slash_selected: 1,
            ..App::default()
        };
        let mut stream_handle = None;

        let consumed = handle_palette_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("handle palette key");

        assert!(consumed);
        assert_eq!(app.input, "/bundle ");
        assert_eq!(app.input_cursor, "/bundle ".len());
        assert!(!app.show_slash_commands);
    }

    #[tokio::test]
    async fn palette_key_navigates_and_executes_no_argument_entries() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(8);
        let mut stream_handle = None;

        let mut navigate_app = App {
            show_slash_commands: true,
            input: "/".to_string(),
            slash_selected: 1,
            ..App::default()
        };
        assert!(
            handle_palette_key(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                &client,
                &mut navigate_app,
                sender.clone(),
                &mut stream_handle,
            )
            .await
            .expect("palette up")
        );
        assert_eq!(navigate_app.slash_selected, 0);
        assert!(
            handle_palette_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &client,
                &mut navigate_app,
                sender.clone(),
                &mut stream_handle,
            )
            .await
            .expect("palette down")
        );
        assert_eq!(navigate_app.slash_selected, 1);

        let mut execute_app = App {
            show_slash_commands: true,
            input: "/".to_string(),
            ..App::default()
        };
        assert!(
            handle_palette_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &client,
                &mut execute_app,
                sender.clone(),
                &mut stream_handle,
            )
            .await
            .expect("palette execute")
        );
        assert_eq!(execute_app.input, "");
        assert_eq!(execute_app.input_cursor, 0);
        assert_eq!(execute_app.status, "install a local bundle first");
        assert!(!execute_app.show_slash_commands);

        let mut no_match_app = App {
            show_slash_commands: true,
            input: "/zzz".to_string(),
            ..App::default()
        };
        let consumed = handle_palette_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut no_match_app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("palette no match");
        assert!(!consumed);
    }

    #[tokio::test]
    async fn enter_handles_blank_unknown_slash_and_plain_messages() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(8);
        let mut stream_handle = None;

        let mut blank_app = App {
            input: "   ".to_string(),
            show_slash_commands: true,
            ..App::default()
        };
        let exit = handle_input(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut blank_app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("blank enter");
        assert!(!exit);
        assert!(!blank_app.show_slash_commands);

        let mut slash_app = App {
            input: "/unknown".to_string(),
            ..App::default()
        };
        handle_input(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut slash_app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("slash enter");
        assert_eq!(slash_app.input, "");
        assert_eq!(slash_app.input_cursor, 0);
        assert_eq!(slash_app.messages.len(), 1);
        assert_eq!(slash_app.messages[0].content, "unknown command: unknown");

        let mut message_app = App {
            input: "hello".to_string(),
            ..App::default()
        };
        handle_input(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &client,
            &mut message_app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("message enter");
        assert_eq!(message_app.input, "hello");
        assert_eq!(message_app.status, "no active session");
        assert!(message_app.messages.is_empty());
    }

    #[tokio::test]
    async fn ctrl_c_shortcut_exits_immediately() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let (sender, _receiver) = mpsc::channel::<AppEvent>(4);
        let mut app = App::default();
        let mut stream_handle = None;

        let exit = handle_input(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("handle ctrl-c");

        assert!(exit);
    }

    #[tokio::test]
    async fn permission_escape_removes_pending_request_without_sending_decision() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let request_id = uuid::Uuid::new_v4();
        let mut app = App {
            pending_permissions: std::collections::VecDeque::from([PendingPermission {
                request_id,
                summary: "permission".to_string(),
            }]),
            ..App::default()
        };

        let exit = handle_permission_input(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &client,
            &mut app,
            PendingPermission {
                request_id,
                summary: "permission".to_string(),
            },
        )
        .await
        .expect("handle permission escape");

        assert!(!exit);
        assert!(app.pending_permissions.is_empty());
    }

    #[tokio::test]
    async fn permission_choices_update_status_when_runtime_has_no_request() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let request_id = uuid::Uuid::new_v4();
        let permission = PendingPermission {
            request_id,
            summary: "permission".to_string(),
        };

        for key in [KeyCode::Char('y'), KeyCode::Char('a'), KeyCode::Char('n')] {
            let mut app = App {
                pending_permissions: std::collections::VecDeque::from([permission.clone()]),
                ..App::default()
            };
            let exit = handle_permission_input(
                KeyEvent::new(key, KeyModifiers::NONE),
                &client,
                &mut app,
                permission.clone(),
            )
            .await
            .expect("handle permission decision");
            assert!(!exit);
            assert!(app.pending_permissions.is_empty());
            assert_eq!(app.status, "permission request not found");
        }
    }

    #[tokio::test]
    async fn handle_input_prioritizes_permissions_before_viewers() {
        let temp = tempdir().expect("tempdir");
        let client = client(temp.path(), "");
        let request_id = uuid::Uuid::new_v4();
        let (sender, _receiver) = mpsc::channel::<AppEvent>(4);
        let mut app = App {
            viewer: Some(ViewerKind::Agents),
            agents: vec!["alpha".to_string(), "beta".to_string()],
            pending_permissions: std::collections::VecDeque::from([PendingPermission {
                request_id,
                summary: "permission".to_string(),
            }]),
            ..App::default()
        };
        let mut stream_handle = None;

        let exit = handle_input(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
        )
        .await
        .expect("permission input");
        assert!(!exit);
        assert!(app.pending_permissions.is_empty());
        assert_eq!(app.status, "permission request not found");
        assert_eq!(app.viewer, Some(ViewerKind::Agents));

        app.selected_agent = 0;
        let exit = handle_input(
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &client,
            &mut app,
            sender,
            &mut stream_handle,
        )
        .await
        .expect("viewer input");
        assert!(!exit);
        assert_eq!(app.selected_agent, 1);
    }
}
