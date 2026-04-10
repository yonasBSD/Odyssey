//! Slash-command parsing, dispatch, and palette metadata.

use crate::app::{App, ViewerKind};
use crate::client::AgentRuntimeClient;
use crate::event::AppEvent;
use crate::handlers::{agent, bundle, model, session};
use crate::ui::theme::AVAILABLE_THEMES;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

// ── Palette metadata ──────────────────────────────────────────────────────────

/// Metadata for a single slash command shown in the palette.
pub struct SlashEntry {
    /// The bare command name (without leading `/`), used for prefix matching.
    pub trigger: &'static str,
    /// Argument placeholder shown after the command name (empty when no args).
    pub args: &'static str,
    /// Short description shown on the right of the palette row.
    pub description: &'static str,
}

/// All supported slash commands in display order.
pub const SLASH_COMMANDS: &[SlashEntry] = &[
    SlashEntry {
        trigger: "new",
        args: "",
        description: "Create a new session",
    },
    SlashEntry {
        trigger: "bundle",
        args: "install [path] | use <ref>",
        description: "Install or switch the active bundle",
    },
    SlashEntry {
        trigger: "bundles",
        args: "",
        description: "List installed bundles",
    },
    SlashEntry {
        trigger: "agents",
        args: "",
        description: "List available agents in the current bundle",
    },
    SlashEntry {
        trigger: "agent",
        args: "<id>",
        description: "Select the active agent in the current bundle",
    },
    SlashEntry {
        trigger: "sessions",
        args: "",
        description: "List all sessions",
    },
    SlashEntry {
        trigger: "skills",
        args: "",
        description: "List available skills",
    },
    SlashEntry {
        trigger: "models",
        args: "",
        description: "List available models",
    },
    SlashEntry {
        trigger: "theme",
        args: "",
        description: "Browse or set UI theme",
    },
];

/// Return the subset of `SLASH_COMMANDS` whose trigger starts with the text
/// the user has typed after the `/`.
pub fn filtered_commands(input: &str) -> Vec<&'static SlashEntry> {
    let prefix = input.trim().trim_start_matches('/').to_lowercase();
    // Stop filtering once the user has added a space (they're typing args).
    let prefix = prefix.split_whitespace().next().unwrap_or("");
    SLASH_COMMANDS
        .iter()
        .filter(|e| e.trigger.starts_with(prefix))
        .collect()
}

// ── Command enum ──────────────────────────────────────────────────────────────

/// Commands that can be entered in the input box with a leading `/`.
pub enum SlashCommand {
    New,
    BundleInstall(String),
    BundleUse(String),
    Bundles,
    Agents,
    Agent(String),
    Join(Uuid),
    Sessions,
    Skills,
    Models,
    Model(String),
    /// Open the themes viewer.
    Themes,
    /// Set a theme directly by name.
    Theme(String),
}

/// Parse a raw input string into a `SlashCommand`.
///
/// Returns `Ok(None)` when the string doesn't start with `/`.
/// Returns `Err(String)` with a usage hint when the command is malformed.
pub fn parse_slash_command(input: &str) -> Result<Option<SlashCommand>, String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Ok(None);
    }

    let parts = trimmed
        .trim_start_matches('/')
        .split_whitespace()
        .collect::<Vec<_>>();
    let Some((command, args)) = parts.split_first() else {
        return Ok(None);
    };
    dispatch_slash_command(command, args)
}

fn dispatch_slash_command(command: &str, args: &[&str]) -> Result<Option<SlashCommand>, String> {
    match command.to_lowercase().as_str() {
        "new" => Ok(Some(SlashCommand::New)),
        "bundles" => Ok(Some(SlashCommand::Bundles)),
        "agents" => Ok(Some(SlashCommand::Agents)),
        "skills" => Ok(Some(SlashCommand::Skills)),
        "sessions" => Ok(Some(SlashCommand::Sessions)),
        "models" => Ok(Some(SlashCommand::Models)),
        "agent" => Ok(Some(parse_named_item(
            args,
            SlashCommand::Agents,
            SlashCommand::Agent,
        ))),
        "bundle" => parse_bundle_command(args),
        "model" => Ok(Some(parse_named_item(
            args,
            SlashCommand::Models,
            SlashCommand::Model,
        ))),
        "theme" => Ok(Some(parse_named_item(
            args,
            SlashCommand::Themes,
            SlashCommand::Theme,
        ))),
        "join" => parse_join_command(args, "usage: /join <session_id>"),
        "session" => parse_session_command(args),
        _ => Err(format!("unknown command: {command}")),
    }
}

fn parse_named_item<F>(args: &[&str], list_command: SlashCommand, item_command: F) -> SlashCommand
where
    F: FnOnce(String) -> SlashCommand,
{
    match args.first().copied() {
        None | Some("list") => list_command,
        Some(id) => item_command(id.to_string()),
    }
}

fn parse_bundle_command(args: &[&str]) -> Result<Option<SlashCommand>, String> {
    match args.first().copied() {
        Some("install") => Ok(Some(SlashCommand::BundleInstall(
            args.get(1).copied().unwrap_or(".").to_string(),
        ))),
        Some("use") => {
            let Some(reference) = args.get(1) else {
                return Err("usage: /bundle use <bundle_ref>".to_string());
            };
            Ok(Some(SlashCommand::BundleUse((*reference).to_string())))
        }
        Some(other) => Ok(Some(SlashCommand::BundleUse(other.to_string()))),
        None => Err("usage: /bundle install [path] | /bundle use <bundle_ref>".to_string()),
    }
}

fn parse_join_command(args: &[&str], usage: &str) -> Result<Option<SlashCommand>, String> {
    let Some(id) = args.first().copied() else {
        return Err(usage.to_string());
    };
    parse_uuid_join(id)
}

fn parse_session_command(args: &[&str]) -> Result<Option<SlashCommand>, String> {
    match args.first().copied() {
        Some("new") => Ok(Some(SlashCommand::New)),
        Some("list") => Ok(Some(SlashCommand::Sessions)),
        Some("skills") => Ok(Some(SlashCommand::Skills)),
        Some("join") => parse_join_command(&args[1..], "usage: /session join <session_id>"),
        Some(id) => parse_uuid_join(id),
        None => Err("usage: /session <id>|new|join <id>".to_string()),
    }
}

fn parse_uuid_join(id: &str) -> Result<Option<SlashCommand>, String> {
    Uuid::parse_str(id)
        .map(|uuid| Some(SlashCommand::Join(uuid)))
        .map_err(|_| "invalid session id".to_string())
}

/// Execute a slash command entered in the input box.
pub async fn handle_slash_command(
    client: &Arc<AgentRuntimeClient>,
    app: &mut App,
    sender: mpsc::Sender<AppEvent>,
    stream_handle: &mut Option<tokio::task::JoinHandle<()>>,
    input: String,
) -> Result<(), String> {
    let Some(command) = parse_slash_command(&input)? else {
        return Ok(());
    };
    log::debug!("handling slash command");
    match command {
        SlashCommand::New => session::create_session(client, app, sender, stream_handle)
            .await
            .map_err(|e| e.to_string()),
        SlashCommand::BundleInstall(path) => {
            bundle::install_bundle(client, app, sender, stream_handle, path).await
        }
        SlashCommand::BundleUse(bundle_ref) => {
            bundle::switch_bundle(client, app, sender, stream_handle, bundle_ref).await
        }
        SlashCommand::Bundles => {
            bundle::refresh_bundles(client, app).await?;
            app.open_viewer(ViewerKind::Bundles);
            Ok(())
        }
        SlashCommand::Agents => {
            agent::refresh_agents(client, app)
                .await
                .map_err(|e| e.to_string())?;
            app.open_viewer(ViewerKind::Agents);
            Ok(())
        }
        SlashCommand::Agent(agent_id) => agent::set_agent_by_id(client, app, agent_id).await,
        SlashCommand::Join(session_id) => {
            session::join_session(client, app, session_id, sender, stream_handle)
                .await
                .map_err(|e| e.to_string())
        }
        SlashCommand::Sessions => {
            session::refresh_sessions(client, app)
                .await
                .map_err(|e| e.to_string())?;
            app.open_viewer(ViewerKind::Sessions);
            Ok(())
        }
        SlashCommand::Skills => {
            app.open_viewer(ViewerKind::Skills);
            Ok(())
        }
        SlashCommand::Models => {
            model::refresh_models(client, app)
                .await
                .map_err(|e| e.to_string())?;
            app.open_viewer(ViewerKind::Models);
            Ok(())
        }
        SlashCommand::Model(model_id) => model::set_model_by_id(client, app, model_id).await,
        SlashCommand::Themes => {
            app.open_viewer(ViewerKind::Themes);
            Ok(())
        }
        SlashCommand::Theme(name) => {
            if app.apply_theme_by_name(&name) {
                app.push_status(format!("theme set: {name}"));
                Ok(())
            } else {
                let available: Vec<&str> = AVAILABLE_THEMES.iter().map(|t| t.name).collect();
                Err(format!(
                    "unknown theme '{name}'. Available: {}",
                    available.join(", ")
                ))
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, ViewerKind};
    use crate::client::AgentRuntimeClient;
    use odyssey_rs_protocol::{DEFAULT_HUB_URL, SandboxMode};
    use odyssey_rs_runtime::{RuntimeConfig, RuntimeEngine};
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::task::JoinHandle;

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

    fn write_bundle_project(
        root: &Path,
        bundle_id: &str,
        agent_id: &str,
        model_name: &str,
        skill_name: &str,
    ) {
        let agent_root = root.join("agents").join(agent_id);
        fs::create_dir_all(root.join("skills").join(skill_name)).expect("create skill dir");
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
    - name: {skill_name}
      path: skills/{skill_name}
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
      network: []
    system_tools: []
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
    name: {model_name}
  tools:
    allow: ["Read", "Skill"]
"#
            ),
        )
        .expect("write agent");
        fs::write(root.join("README.md"), format!("# {bundle_id}\n")).expect("write readme");
        fs::write(
            root.join("skills").join(skill_name).join("SKILL.md"),
            format!("# {skill_name}\n"),
        )
        .expect("write skill");
        fs::write(
            root.join("resources").join("data").join("notes.txt"),
            "hello world\n",
        )
        .expect("write resource");
    }

    fn abort_stream(handle: &mut Option<JoinHandle<()>>) {
        if let Some(handle) = handle.take() {
            handle.abort();
        }
    }

    #[test]
    fn non_slash_input_returns_none() {
        assert!(matches!(parse_slash_command("hello"), Ok(None)));
        assert!(matches!(parse_slash_command("  plain text"), Ok(None)));
    }

    #[test]
    fn empty_slash_returns_none() {
        assert!(matches!(parse_slash_command("/"), Ok(None)));
        assert!(matches!(parse_slash_command("/  "), Ok(None)));
    }

    #[test]
    fn parse_new() {
        assert!(matches!(
            parse_slash_command("/new"),
            Ok(Some(SlashCommand::New))
        ));
        assert!(matches!(
            parse_slash_command("  /new  "),
            Ok(Some(SlashCommand::New))
        ));
    }

    #[test]
    fn parse_sessions() {
        assert!(matches!(
            parse_slash_command("/sessions"),
            Ok(Some(SlashCommand::Sessions))
        ));
    }

    #[test]
    fn parse_agents() {
        assert!(matches!(
            parse_slash_command("/agents"),
            Ok(Some(SlashCommand::Agents))
        ));
    }

    #[test]
    fn parse_bundles() {
        assert!(matches!(
            parse_slash_command("/bundles"),
            Ok(Some(SlashCommand::Bundles))
        ));
    }

    #[test]
    fn parse_agent_without_arg_returns_agents_list() {
        assert!(matches!(
            parse_slash_command("/agent"),
            Ok(Some(SlashCommand::Agents))
        ));
    }

    #[test]
    fn parse_agent_with_id() {
        assert!(matches!(
            parse_slash_command("/agent orchestrator"),
            Ok(Some(SlashCommand::Agent(id))) if id == "orchestrator"
        ));
    }

    #[test]
    fn parse_bundle_install_defaults_to_dot() {
        assert!(matches!(
            parse_slash_command("/bundle install"),
            Ok(Some(SlashCommand::BundleInstall(path))) if path == "."
        ));
    }

    #[test]
    fn parse_bundle_install_with_path() {
        assert!(matches!(
            parse_slash_command("/bundle install bundles/odyssey-cowork"),
            Ok(Some(SlashCommand::BundleInstall(path))) if path == "bundles/odyssey-cowork"
        ));
    }

    #[test]
    fn parse_bundle_use() {
        assert!(matches!(
            parse_slash_command("/bundle use odyssey-cowork@latest"),
            Ok(Some(SlashCommand::BundleUse(reference))) if reference == "odyssey-cowork@latest"
        ));
    }

    #[test]
    fn parse_skills() {
        assert!(matches!(
            parse_slash_command("/skills"),
            Ok(Some(SlashCommand::Skills))
        ));
    }

    #[test]
    fn parse_models() {
        assert!(matches!(
            parse_slash_command("/models"),
            Ok(Some(SlashCommand::Models))
        ));
    }

    #[test]
    fn parse_model_without_arg_returns_models_list() {
        assert!(matches!(
            parse_slash_command("/model"),
            Ok(Some(SlashCommand::Models))
        ));
        assert!(matches!(
            parse_slash_command("/model list"),
            Ok(Some(SlashCommand::Models))
        ));
    }

    #[test]
    fn parse_model_with_id() {
        let result = parse_slash_command("/model gpt-4");
        assert!(matches!(result, Ok(Some(SlashCommand::Model(_)))));
        if let Ok(Some(SlashCommand::Model(id))) = result {
            assert_eq!(id, "gpt-4");
        }
    }

    #[test]
    fn parse_join_valid_uuid() {
        let id = Uuid::new_v4();
        let input = format!("/join {id}");
        let result = parse_slash_command(&input);
        assert!(matches!(result, Ok(Some(SlashCommand::Join(_)))));
        if let Ok(Some(SlashCommand::Join(parsed))) = result {
            assert_eq!(parsed, id);
        }
    }

    #[test]
    fn parse_join_missing_id_returns_error() {
        let result = parse_slash_command("/join");
        assert!(result.is_err());
    }

    #[test]
    fn parse_join_invalid_uuid_returns_error() {
        let result = parse_slash_command("/join not-a-uuid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_session_new() {
        assert!(matches!(
            parse_slash_command("/session new"),
            Ok(Some(SlashCommand::New))
        ));
    }

    #[test]
    fn parse_session_list() {
        assert!(matches!(
            parse_slash_command("/session list"),
            Ok(Some(SlashCommand::Sessions))
        ));
    }

    #[test]
    fn parse_session_join_valid_uuid() {
        let id = Uuid::new_v4();
        let input = format!("/session join {id}");
        assert!(matches!(
            parse_slash_command(&input),
            Ok(Some(SlashCommand::Join(_)))
        ));
    }

    #[test]
    fn parse_session_no_arg_is_error() {
        assert!(parse_slash_command("/session").is_err());
    }

    #[test]
    fn unknown_command_returns_error() {
        assert!(parse_slash_command("/foobar").is_err());
    }

    #[test]
    fn filtered_commands_and_theme_dispatch_cover_remaining_parse_branches() {
        assert_eq!(
            filtered_commands("/Bun")
                .into_iter()
                .map(|entry| entry.trigger)
                .collect::<Vec<_>>(),
            vec!["bundle", "bundles"]
        );
        assert_eq!(
            filtered_commands("/theme odyssey")
                .into_iter()
                .map(|entry| entry.trigger)
                .collect::<Vec<_>>(),
            vec!["theme"]
        );
        assert!(filtered_commands("/zzz").is_empty());

        assert!(matches!(
            parse_slash_command("/theme"),
            Ok(Some(SlashCommand::Themes))
        ));
        assert!(matches!(
            parse_slash_command("/theme list"),
            Ok(Some(SlashCommand::Themes))
        ));
        assert!(matches!(
            parse_slash_command("/theme sunset"),
            Ok(Some(SlashCommand::Theme(name))) if name == "sunset"
        ));
        assert!(matches!(
            parse_slash_command("/bundle local/demo@0.1.0"),
            Ok(Some(SlashCommand::BundleUse(reference))) if reference == "local/demo@0.1.0"
        ));
        match parse_slash_command("/bundle") {
            Err(err) => assert_eq!(
                err,
                "usage: /bundle install [path] | /bundle use <bundle_ref>"
            ),
            Ok(_) => panic!("missing bundle args should be rejected"),
        }
        assert!(matches!(
            parse_slash_command("/session skills"),
            Ok(Some(SlashCommand::Skills))
        ));
        let session_id = Uuid::new_v4();
        assert!(matches!(
            parse_slash_command(&format!("/session {session_id}")),
            Ok(Some(SlashCommand::Join(id))) if id == session_id
        ));
    }

    #[tokio::test]
    async fn handle_slash_command_returns_early_for_plain_input_and_opens_viewers() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(temp.path())).expect("runtime"));
        let project = temp.path().join("alpha-project");
        fs::create_dir_all(&project).expect("create project");
        write_bundle_project(
            &project,
            "alpha",
            "alpha-agent",
            "gpt-4.1-mini",
            "repo-hygiene",
        );
        runtime.build_and_install(&project).expect("install bundle");

        let client = Arc::new(AgentRuntimeClient::new(
            runtime.clone(),
            "local/alpha@0.1.0".to_string(),
        ));
        let mut app = App {
            bundle_ref: "local/alpha@0.1.0".to_string(),
            ..App::default()
        };
        let (sender, _receiver) = mpsc::channel(16);
        let mut stream_handle = None;

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "plain input".to_string(),
        )
        .await
        .expect("plain input is ignored");
        assert!(app.viewer.is_none());

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/bundles".to_string(),
        )
        .await
        .expect("open bundles");
        assert_eq!(app.viewer, Some(ViewerKind::Bundles));

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/agents".to_string(),
        )
        .await
        .expect("open agents");
        assert_eq!(app.viewer, Some(ViewerKind::Agents));
        assert_eq!(app.agents, vec!["alpha-agent".to_string()]);

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/models".to_string(),
        )
        .await
        .expect("open models");
        assert_eq!(app.viewer, Some(ViewerKind::Models));
        assert_eq!(app.models, vec!["gpt-4.1-mini".to_string()]);

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/skills".to_string(),
        )
        .await
        .expect("open skills");
        assert_eq!(app.viewer, Some(ViewerKind::Skills));

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/theme".to_string(),
        )
        .await
        .expect("open themes");
        assert_eq!(app.viewer, Some(ViewerKind::Themes));

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            format!("/theme {}", AVAILABLE_THEMES[1].name),
        )
        .await
        .expect("apply theme");
        assert_eq!(
            app.status,
            format!("theme set: {}", AVAILABLE_THEMES[1].name)
        );

        let error = handle_slash_command(
            &client,
            &mut app,
            sender,
            &mut stream_handle,
            "/theme missing-theme".to_string(),
        )
        .await
        .expect_err("unknown theme should fail");
        assert!(error.contains("unknown theme 'missing-theme'"));
        abort_stream(&mut stream_handle);
    }

    #[tokio::test]
    async fn handle_slash_command_routes_bundle_agent_model_and_session_actions() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(RuntimeEngine::new(runtime_config(temp.path())).expect("runtime"));
        let alpha_project = temp.path().join("alpha-project");
        let beta_project = temp.path().join("beta-project");
        fs::create_dir_all(&alpha_project).expect("create alpha project");
        fs::create_dir_all(&beta_project).expect("create beta project");
        write_bundle_project(
            &alpha_project,
            "alpha",
            "alpha-agent",
            "gpt-4.1-mini",
            "repo-hygiene",
        );
        write_bundle_project(
            &beta_project,
            "beta",
            "beta-agent",
            "gpt-4.1",
            "deploy-checks",
        );
        runtime
            .build_and_install(&beta_project)
            .expect("install beta");

        let client = Arc::new(AgentRuntimeClient::new(runtime.clone(), String::default()));
        let mut app = App {
            cwd: temp.path().display().to_string(),
            ..App::default()
        };
        let (sender, _receiver) = mpsc::channel(16);
        let mut stream_handle = None;

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/bundle install alpha-project".to_string(),
        )
        .await
        .expect("install alpha through slash command");
        assert_eq!(app.bundle_ref, "local/alpha@0.1.0");
        assert_eq!(app.active_agent.as_deref(), Some("alpha-agent"));
        assert_eq!(app.model_id, "gpt-4.1-mini");
        assert!(app.active_session.is_some());
        assert_eq!(app.status, "bundle set: local/alpha@0.1.0");

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/bundle use local/beta@0.1.0".to_string(),
        )
        .await
        .expect("switch to beta");
        assert_eq!(app.bundle_ref, "local/beta@0.1.0");
        assert_eq!(app.active_agent.as_deref(), Some("beta-agent"));
        assert_eq!(app.model_id, "gpt-4.1");
        assert_eq!(app.status, "bundle set: local/beta@0.1.0");

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/agent beta-agent".to_string(),
        )
        .await
        .expect("set beta agent");
        assert_eq!(app.active_agent.as_deref(), Some("beta-agent"));
        assert_eq!(app.status, "agent set: beta-agent");

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/model gpt-4.1".to_string(),
        )
        .await
        .expect("set beta model");
        assert_eq!(app.model_id, "gpt-4.1");
        assert_eq!(app.status, "model set: gpt-4.1");

        let joined_session = runtime
            .create_session("local/beta@0.1.0")
            .expect("create beta session")
            .id;
        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            format!("/join {joined_session}"),
        )
        .await
        .expect("join existing session");
        assert_eq!(app.active_session, Some(joined_session));
        assert_eq!(app.status, "session joined");

        handle_slash_command(
            &client,
            &mut app,
            sender.clone(),
            &mut stream_handle,
            "/sessions".to_string(),
        )
        .await
        .expect("list sessions");
        assert_eq!(app.viewer, Some(ViewerKind::Sessions));

        let previous_session = app.active_session;
        handle_slash_command(
            &client,
            &mut app,
            sender,
            &mut stream_handle,
            "/new".to_string(),
        )
        .await
        .expect("create new session");
        assert!(app.active_session.is_some());
        assert_ne!(app.active_session, previous_session);
        assert_eq!(app.status, "session created");
        abort_stream(&mut stream_handle);
    }
}
