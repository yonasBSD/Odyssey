use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use crossterm::style::Stylize;
use log::info;
use odyssey_rs_bundle::{BundleBuilder, BundleProject, BundleStore};
use odyssey_rs_protocol::{ExecutionRequest, SandboxMode, SessionSpec, Task};
use odyssey_rs_runtime::{OdysseyRuntime, RuntimeConfig};
use odyssey_rs_tui::{TuiRunConfig, resolve_bundle_ref};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::time::Instant;
use uuid::Uuid;

use crate::remote::RemoteRuntimeClient;

#[derive(Parser)]
#[command(name = "odyssey-rs", version)]
pub struct Cli {
    #[arg(long, global = true)]
    pub remote: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "Initialize a new Odyssey project")]
    Init { path: String },
    #[command(about = "Build Odyssey agent bundle")]
    Build {
        path: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    #[command(about = "Inspect Odyssey agent bundle")]
    Inspect { reference: String },
    #[command(about = "Run an Odyssey agent bundle")]
    Run {
        #[arg(help = "The bundle reference")]
        reference: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        dangerous_sandbox_mode: bool,
    },
    #[command(
        about = "Start a local runtimer server",
        long_about = "Starts a runtime server and listens for incoming requests. Useful when running multiple agent instancecs on a shared resources."
    )]
    Serve {
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        dangerous_sandbox_mode: bool,
    },
    #[command(about = "Publish Odyssey agent bundle to Hub")]
    Push {
        source: String,
        #[arg(long)]
        to: String,
        #[arg(long = "hub", visible_alias = "registry")]
        hub: Option<String>,
    },
    #[command(about = "Pull Odyssey agent bundle to Hub")]
    Pull {
        reference: String,
        #[arg(long = "hub", visible_alias = "registry")]
        hub: Option<String>,
    },
    #[command(about = "Export Odyssey to a .odyssey file")]
    Export {
        reference: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    #[command(about = "Import and Install from .odyssey file")]
    Import { path: PathBuf },
    #[command(about = "List Installed bundles")]
    Bundles,
    #[command(about = "List sessions")]
    Sessions,
    #[command(about = "Get or Delete Session")]
    Session {
        id: Uuid,
        #[arg(long)]
        delete: bool,
    },
    #[command(about = "Run the TUI")]
    Tui {
        /// Bundle reference to run, such as `hello-world@latest`.
        #[arg(long, short = 'b')]
        bundle: Option<String>,
        /// Optional working directory label shown in the header.
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        dangerous_sandbox_mode: bool,
    },
}

pub async fn run_cli(cli: Cli) -> Result<()> {
    info!("Running Odyssey CLI");
    validate_remote_usage(&cli)?;
    let config = build_runtime_config(&cli)?;
    let start_time = Instant::now();
    let runtime = OdysseyRuntime::new(config.clone())?;
    let bundles = BundleStore::new(config.cache_root.clone());
    let end_time = start_time.elapsed();
    info!("Initiated Runtime and Bundle in {:?}", end_time);
    let remote = cli
        .remote
        .as_deref()
        .map(RemoteRuntimeClient::new)
        .transpose()?;
    execute_command(cli.command, &config, &runtime, &bundles, remote.as_ref()).await?;
    Ok(())
}

fn build_runtime_config(cli: &Cli) -> Result<RuntimeConfig> {
    let mut config = RuntimeConfig::load()?;
    if let Some(remote) = &cli.remote {
        config.bind_addr.clone_from(remote);
    }
    if let Command::Serve {
        bind: Some(bind), ..
    } = &cli.command
    {
        config.bind_addr.clone_from(bind);
    }
    if dangerous_sandbox_mode_enabled(&cli.command) {
        config.sandbox_mode_override = Some(SandboxMode::DangerFullAccess);
        info!("Registered DangerFullAccess Sandbox mode");
    }
    if let Some(hub_url) = hub_override(&cli.command) {
        config.hub_url = hub_url;
    }
    Ok(config)
}

fn dangerous_sandbox_mode_enabled(command: &Command) -> bool {
    matches!(
        command,
        Command::Run {
            dangerous_sandbox_mode: true,
            ..
        } | Command::Serve {
            dangerous_sandbox_mode: true,
            ..
        } | Command::Tui {
            dangerous_sandbox_mode: true,
            ..
        }
    )
}

fn validate_remote_usage(cli: &Cli) -> Result<()> {
    if cli.remote.is_some() && !remote_command_supported(&cli.command) {
        return Err(anyhow!("--remote is not supported with this command"));
    }
    Ok(())
}

fn remote_command_supported(command: &Command) -> bool {
    matches!(
        command,
        Command::Inspect { .. }
            | Command::Run { .. }
            | Command::Pull { .. }
            | Command::Bundles
            | Command::Sessions
            | Command::Session { .. }
    )
}

async fn execute_command(
    command: Command,
    config: &RuntimeConfig,
    runtime: &OdysseyRuntime,
    bundles: &BundleStore,
    remote: Option<&RemoteRuntimeClient>,
) -> Result<()> {
    match command {
        Command::Init { path } => handle_init(runtime, &path),
        Command::Build { path, output } => handle_build(bundles, &path, output),
        Command::Inspect { reference } => handle_inspect(bundles, remote, &reference).await,
        Command::Run {
            reference, prompt, ..
        } => handle_run(runtime, remote, reference, prompt).await,
        Command::Serve { .. } => handle_serve(config.clone()).await,
        Command::Push { source, to, .. } => handle_push(bundles, config, &source, &to).await,
        Command::Pull { reference, .. } => handle_pull(bundles, remote, config, &reference).await,
        Command::Export { reference, output } => handle_export(bundles, &reference, output),
        Command::Import { path } => handle_import(bundles, path),
        Command::Bundles => handle_bundles(bundles, remote).await,
        Command::Sessions => handle_sessions(runtime, remote).await,
        Command::Session { id, delete } => handle_session(runtime, remote, id, delete).await,
        Command::Tui { bundle, cwd, .. } => handle_tui(bundle, cwd, runtime).await,
    }
}

fn handle_init(runtime: &OdysseyRuntime, path: &str) -> Result<()> {
    runtime.init(path)?;
    print_init_summary(path);
    Ok(())
}

fn handle_build(bundles: &BundleStore, path: &str, output: Option<PathBuf>) -> Result<()> {
    if let Some(output) = output {
        let project = BundleProject::load(path)?;
        let artifact = BundleBuilder::new(project).build(&output)?;
        println!(
            "{} {}@{} {} {}",
            "built".green().bold(),
            artifact.metadata.id,
            artifact.metadata.version,
            artifact.metadata.digest,
            artifact.path.display()
        );
    } else {
        let install = bundles.build_and_install(path)?;
        println!(
            "{} {}@{} {} {}",
            "installed".green().bold(),
            install.metadata.id,
            install.metadata.version,
            install.metadata.digest,
            install.path.display()
        );
    }
    Ok(())
}

async fn handle_inspect(
    bundles: &BundleStore,
    remote: Option<&RemoteRuntimeClient>,
    reference: &str,
) -> Result<()> {
    let metadata = if let Some(remote) = remote {
        remote.inspect(reference).await?
    } else {
        bundles.resolve(reference)?.metadata
    };
    println!("{}", "bundle metadata".cyan().bold());
    println!("{}", serde_json::to_string_pretty(&metadata)?);
    Ok(())
}

async fn handle_run(
    runtime: &OdysseyRuntime,
    remote: Option<&RemoteRuntimeClient>,
    reference: String,
    prompt: String,
) -> Result<()> {
    let result = if let Some(remote) = remote {
        remote.run(reference, prompt).await?
    } else {
        let session = runtime.create_session(SessionSpec::from(reference))?;
        let request_id = Uuid::new_v4();
        runtime
            .run(ExecutionRequest {
                request_id,
                session_id: session.id,
                input: Task::new(prompt),
                turn_context: None,
            })
            .await?
    };
    println!("{}", "assistant".cyan().bold());
    println!("{}", result.response);
    Ok(())
}

async fn handle_serve(config: RuntimeConfig) -> Result<()> {
    println!(
        "{} {}",
        "serving".green().bold(),
        config.bind_addr.as_str().cyan()
    );
    odyssey_rs_server::serve(config).await?;
    Ok(())
}

async fn handle_push(
    bundles: &BundleStore,
    config: &RuntimeConfig,
    source: &str,
    to: &str,
) -> Result<()> {
    let published = bundles.publish(source, to, &config.hub_url).await?;
    println!(
        "{} {} {}",
        "published".green().bold(),
        format!("{}@{}", published.id, published.version).cyan(),
        published.digest.cyan()
    );
    Ok(())
}

async fn handle_pull(
    bundles: &BundleStore,
    remote: Option<&RemoteRuntimeClient>,
    config: &RuntimeConfig,
    reference: &str,
) -> Result<()> {
    let install = if let Some(remote) = remote {
        remote.pull(reference, &config.hub_url).await?
    } else {
        bundles.pull(reference, &config.hub_url).await?
    };
    println!(
        "{} {} {}",
        "pulled".green().bold(),
        format!(
            "{}/{}@{}",
            install.metadata.namespace, install.metadata.id, install.metadata.version
        )
        .cyan(),
        install.path.display()
    );
    Ok(())
}

fn handle_export(bundles: &BundleStore, reference: &str, output: Option<PathBuf>) -> Result<()> {
    let output = output.unwrap_or_else(|| PathBuf::from("."));
    let path = bundles.export(reference, output)?;
    println!("{} {}", "exported".green().bold(), path.display());
    Ok(())
}

fn handle_import(bundles: &BundleStore, path: PathBuf) -> Result<()> {
    let install = bundles.import(path)?;
    println!(
        "{} {}/{}@{}",
        "imported".green().bold(),
        install.metadata.namespace,
        install.metadata.id,
        install.metadata.version
    );
    Ok(())
}

async fn handle_bundles(bundles: &BundleStore, remote: Option<&RemoteRuntimeClient>) -> Result<()> {
    let bundles = if let Some(remote) = remote {
        remote.list_bundles().await?
    } else {
        bundles.list_installed()?
    };
    if bundles.is_empty() {
        println!("{}", "no bundles installed".dark_grey());
    } else {
        for bundle in bundles {
            println!(
                "{} {}",
                format!("{}/{}@{}", bundle.namespace, bundle.id, bundle.version)
                    .cyan()
                    .bold(),
                bundle.path.display()
            );
        }
    }
    Ok(())
}

async fn handle_sessions(
    runtime: &OdysseyRuntime,
    remote: Option<&RemoteRuntimeClient>,
) -> Result<()> {
    let sessions = if let Some(remote) = remote {
        remote.list_sessions().await?
    } else {
        runtime.list_sessions(None)
    };
    if sessions.is_empty() {
        println!("{}", "no sessions".dark_grey());
    } else {
        for session in sessions {
            println!(
                "{} {} {}",
                session.id.to_string().cyan().bold(),
                session.agent_id,
                session.created_at
            );
        }
    }
    Ok(())
}

async fn handle_session(
    runtime: &OdysseyRuntime,
    remote: Option<&RemoteRuntimeClient>,
    id: Uuid,
    delete: bool,
) -> Result<()> {
    if delete {
        if let Some(remote) = remote {
            remote.delete_session(id).await?;
        } else {
            runtime.delete_session(id).await?;
        }
        println!("{} {}", "deleted".green().bold(), id);
    } else {
        let session = if let Some(remote) = remote {
            remote.get_session(id).await?
        } else {
            runtime.get_session(id)?
        };
        println!("{}", serde_json::to_string_pretty(&session)?);
    }
    Ok(())
}

async fn handle_tui(
    bundle: Option<String>,
    cwd: Option<PathBuf>,
    runtime: &OdysseyRuntime,
) -> Result<()> {
    let _ = env_logger::builder().format_timestamp_millis().try_init();
    let bundle_ref = resolve_bundle_ref(runtime, bundle)?;
    let runtime = Arc::new(runtime.clone());

    odyssey_rs_tui::run(runtime, TuiRunConfig { bundle_ref, cwd }).await?;
    Ok(())
}

fn hub_override(command: &Command) -> Option<String> {
    match command {
        Command::Push { hub: Some(hub), .. } | Command::Pull { hub: Some(hub), .. } => {
            Some(hub.clone())
        }
        _ => None,
    }
}

fn print_init_summary(path: &str) {
    let bundle_id = default_bundle_id(Path::new(path));
    let bundle_ref = format!("{bundle_id}@latest");

    println!(
        "{} {}",
        "initialized bundle".green().bold(),
        bundle_id.as_str().cyan().bold()
    );
    println!("{} {}", "path".dark_grey().bold(), path);
    println!();
    println!("{}", "Get Started".yellow().bold());
    println!(
        "{} {}",
        "build:".dark_grey().bold(),
        format!("odyssey-rs -- build {path}").cyan()
    );
    println!(
        "{} {}",
        "run:".dark_grey().bold(),
        format!("odyssey-rs -- run {bundle_ref} --prompt \"Hey, What is your name?\"").cyan()
    );
}

fn default_bundle_id(root: &Path) -> String {
    let raw = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("hello-world");
    let mut slug = String::with_capacity(raw.len());
    let mut previous_dash = false;
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            previous_dash = false;
            ch.to_ascii_lowercase()
        } else {
            if previous_dash {
                continue;
            }
            previous_dash = true;
            '-'
        };
        slug.push(mapped);
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "hello-world".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, Command, build_runtime_config, dangerous_sandbox_mode_enabled, default_bundle_id,
        execute_command, handle_build, handle_bundles, handle_export, handle_import, handle_init,
        handle_inspect, handle_session, handle_sessions, hub_override, remote_command_supported,
        validate_remote_usage,
    };
    use clap::Parser;
    use odyssey_rs_bundle::BundleStore;
    use odyssey_rs_protocol::{DEFAULT_HUB_URL, SandboxMode, SessionSpec};
    use odyssey_rs_runtime::{OdysseyRuntime, RuntimeConfig};
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

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

    #[test]
    fn derives_bundle_id_from_cli_path() {
        assert_eq!(
            default_bundle_id(Path::new("./bundles/My Starter Agent")),
            "my-starter-agent"
        );
    }

    #[test]
    fn tui_cli_accepts_dangerous_sandbox_mode_flag() {
        let cli = Cli::parse_from([
            "odyssey-rs",
            "tui",
            "--bundle",
            "local/demo@0.1.0",
            "--dangerous-sandbox-mode",
        ]);

        match cli.command {
            Command::Tui {
                bundle,
                dangerous_sandbox_mode,
                ..
            } => {
                assert_eq!(bundle, Some("local/demo@0.1.0".to_string()));
                assert!(dangerous_sandbox_mode);
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn tui_dangerous_sandbox_mode_overrides_runtime_config() {
        let cli = Cli::parse_from(["odyssey-rs", "tui", "--dangerous-sandbox-mode"]);

        let config = build_runtime_config(&cli).expect("runtime config");

        assert_eq!(
            config.sandbox_mode_override,
            Some(SandboxMode::DangerFullAccess)
        );
    }

    #[test]
    fn validate_remote_usage_rejects_unsupported_commands() {
        let cli = Cli {
            remote: Some("127.0.0.1:4000".to_string()),
            command: Command::Build {
                path: ".".to_string(),
                output: None,
            },
        };

        let error = validate_remote_usage(&cli).expect_err("build should reject --remote");

        assert_eq!(
            error.to_string(),
            "--remote is not supported with this command"
        );
    }

    #[test]
    fn validate_remote_usage_allows_supported_commands() {
        let cli = Cli {
            remote: Some("127.0.0.1:4000".to_string()),
            command: Command::Inspect {
                reference: "local/demo@0.1.0".to_string(),
            },
        };

        assert!(validate_remote_usage(&cli).is_ok());
    }

    #[test]
    fn serve_bind_takes_precedence_over_remote_bind_and_applies_danger_mode() {
        let cli = Cli {
            remote: Some("127.0.0.1:4000".to_string()),
            command: Command::Serve {
                bind: Some("127.0.0.1:5000".to_string()),
                dangerous_sandbox_mode: true,
            },
        };

        let config = build_runtime_config(&cli).expect("runtime config");

        assert_eq!(config.bind_addr, "127.0.0.1:5000");
        assert_eq!(
            config.sandbox_mode_override,
            Some(SandboxMode::DangerFullAccess)
        );
    }

    #[test]
    fn push_hub_override_updates_runtime_config() {
        let cli = Cli {
            remote: None,
            command: Command::Push {
                source: ".".to_string(),
                to: "team/demo:0.1.0".to_string(),
                hub: Some("https://hub.example.com".to_string()),
            },
        };

        let config = build_runtime_config(&cli).expect("runtime config");

        assert_eq!(config.hub_url, "https://hub.example.com");
    }

    #[test]
    fn command_helpers_match_supported_subcommands() {
        assert!(dangerous_sandbox_mode_enabled(&Command::Run {
            reference: "local/demo@0.1.0".to_string(),
            prompt: "hi".to_string(),
            dangerous_sandbox_mode: true,
        }));
        assert!(dangerous_sandbox_mode_enabled(&Command::Serve {
            bind: None,
            dangerous_sandbox_mode: true,
        }));
        assert!(dangerous_sandbox_mode_enabled(&Command::Tui {
            bundle: None,
            cwd: None,
            dangerous_sandbox_mode: true,
        }));
        assert!(!dangerous_sandbox_mode_enabled(&Command::Inspect {
            reference: "local/demo@0.1.0".to_string(),
        }));

        assert!(remote_command_supported(&Command::Inspect {
            reference: "local/demo@0.1.0".to_string(),
        }));
        assert!(remote_command_supported(&Command::Pull {
            reference: "team/demo@1.0.0".to_string(),
            hub: None,
        }));
        assert!(remote_command_supported(&Command::Session {
            id: uuid::Uuid::nil(),
            delete: false,
        }));
        assert!(!remote_command_supported(&Command::Export {
            reference: "local/demo@0.1.0".to_string(),
            output: None,
        }));

        assert_eq!(
            hub_override(&Command::Pull {
                reference: "team/demo@1.0.0".to_string(),
                hub: Some("https://hub.example.com".to_string()),
            }),
            Some("https://hub.example.com".to_string())
        );
        assert_eq!(hub_override(&Command::Bundles), None);
    }

    #[test]
    fn remote_bind_updates_runtime_config_for_supported_commands() {
        let cli = Cli {
            remote: Some("127.0.0.1:4900".to_string()),
            command: Command::Inspect {
                reference: "local/demo@0.1.0".to_string(),
            },
        };

        let config = build_runtime_config(&cli).expect("runtime config");

        assert_eq!(config.bind_addr, "127.0.0.1:4900");
    }

    #[test]
    fn default_bundle_id_falls_back_and_collapses_repeated_separators() {
        assert_eq!(default_bundle_id(Path::new("")), "hello-world");
        assert_eq!(default_bundle_id(Path::new("./bundles/   ")), "hello-world");
        assert_eq!(
            default_bundle_id(Path::new("./bundles/Hello___World!!!")),
            "hello-world"
        );
        assert_eq!(
            default_bundle_id(Path::new("./bundles/...Rust   Agent---Beta...")),
            "rust-agent-beta"
        );
    }

    #[tokio::test]
    async fn bundle_handlers_manage_local_bundle_lifecycle() {
        let temp = tempdir().expect("tempdir");
        let runtime = OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime");
        let bundles = runtime.bundle_store();
        let scaffold = temp.path().join("scaffold");

        handle_init(&runtime, scaffold.to_str().expect("utf8 scaffold path")).expect("init");
        assert!(scaffold.join("odyssey.bundle.yaml").exists());

        let empty_store = BundleStore::new(temp.path().join("empty-cache"));
        handle_bundles(&empty_store, None)
            .await
            .expect("list empty bundles");

        let project = temp.path().join("alpha-project");
        write_bundle_project(&project, "alpha", "alpha-agent");

        let artifacts = temp.path().join("artifacts");
        fs::create_dir_all(&artifacts).expect("create artifacts dir");
        handle_build(
            &bundles,
            project.to_str().expect("utf8 project path"),
            Some(artifacts.clone()),
        )
        .expect("build bundle archive");
        assert!(
            fs::read_dir(&artifacts)
                .expect("list artifacts")
                .next()
                .is_some()
        );

        handle_build(&bundles, project.to_str().expect("utf8 project path"), None)
            .expect("install bundle");
        assert_eq!(
            bundles.list_installed().expect("installed bundles").len(),
            1
        );

        handle_bundles(&bundles, None)
            .await
            .expect("list installed bundles");
        handle_inspect(&bundles, None, "local/alpha@0.1.0")
            .await
            .expect("inspect bundle");

        let exports = temp.path().join("exports");
        fs::create_dir_all(&exports).expect("create exports dir");
        handle_export(&bundles, "local/alpha@0.1.0", Some(exports.clone())).expect("export");
        let archive = fs::read_dir(&exports)
            .expect("read exports dir")
            .find_map(Result::ok)
            .map(|entry| entry.path())
            .expect("exported archive");

        let imported_store = BundleStore::new(temp.path().join("import-cache"));
        handle_import(&imported_store, archive).expect("import bundle");
        assert_eq!(
            imported_store
                .list_installed()
                .expect("imported bundles")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn session_handlers_list_show_and_delete_local_sessions() {
        let temp = tempdir().expect("tempdir");
        let runtime = OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime");
        let project = temp.path().join("alpha-project");
        write_bundle_project(&project, "alpha", "alpha-agent");
        runtime
            .build_and_install(&project)
            .expect("install bundle for sessions");

        handle_sessions(&runtime, None)
            .await
            .expect("list empty sessions");

        let session = runtime
            .create_session(SessionSpec::from("local/alpha@0.1.0"))
            .expect("create session");

        handle_sessions(&runtime, None)
            .await
            .expect("list populated sessions");
        handle_session(&runtime, None, session.id, false)
            .await
            .expect("show session");
        handle_session(&runtime, None, session.id, true)
            .await
            .expect("delete session");
        assert!(runtime.list_sessions(None).is_empty());
    }

    #[tokio::test]
    async fn execute_command_routes_local_bundle_and_session_variants() {
        let temp = tempdir().expect("tempdir");
        let runtime = OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime");
        let config = runtime.config().clone();
        let bundles = runtime.bundle_store();
        let scaffold = temp.path().join("scaffold");

        execute_command(
            Command::Init {
                path: scaffold.to_str().expect("utf8 scaffold path").to_string(),
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch init");

        let project = temp.path().join("alpha-project");
        write_bundle_project(&project, "alpha", "alpha-agent");
        let artifacts = temp.path().join("artifacts");
        fs::create_dir_all(&artifacts).expect("create artifacts dir");
        execute_command(
            Command::Build {
                path: project.to_str().expect("utf8 project path").to_string(),
                output: Some(artifacts.clone()),
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch build artifact");

        execute_command(
            Command::Build {
                path: project.to_str().expect("utf8 project path").to_string(),
                output: None,
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch build install");

        execute_command(
            Command::Inspect {
                reference: "local/alpha@0.1.0".to_string(),
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch inspect");

        let exports = temp.path().join("exports");
        fs::create_dir_all(&exports).expect("create exports dir");
        execute_command(
            Command::Export {
                reference: "local/alpha@0.1.0".to_string(),
                output: Some(exports.clone()),
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch export");

        execute_command(Command::Bundles, &config, &runtime, &bundles, None)
            .await
            .expect("dispatch bundles");

        let session = runtime
            .create_session(SessionSpec::from("local/alpha@0.1.0"))
            .expect("create session");
        execute_command(Command::Sessions, &config, &runtime, &bundles, None)
            .await
            .expect("dispatch sessions");
        execute_command(
            Command::Session {
                id: session.id,
                delete: false,
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch show session");
        execute_command(
            Command::Session {
                id: session.id,
                delete: true,
            },
            &config,
            &runtime,
            &bundles,
            None,
        )
        .await
        .expect("dispatch delete session");

        let archive = fs::read_dir(&exports)
            .expect("read exports dir")
            .find_map(Result::ok)
            .map(|entry| entry.path())
            .expect("exported archive");
        let imported_store = BundleStore::new(temp.path().join("import-cache"));
        execute_command(
            Command::Import { path: archive },
            &config,
            &runtime,
            &imported_store,
            None,
        )
        .await
        .expect("dispatch import");
    }

    #[tokio::test]
    async fn execute_command_surfaces_expected_errors_for_unavailable_variants() {
        let temp = tempdir().expect("tempdir");
        let runtime = OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime");
        let config = RuntimeConfig {
            bind_addr: "not-an-address".to_string(),
            ..runtime.config().clone()
        };
        let bundles = runtime.bundle_store();

        assert!(
            execute_command(
                Command::Run {
                    reference: "local/missing@0.1.0".to_string(),
                    prompt: "hello".to_string(),
                    dangerous_sandbox_mode: false,
                },
                &config,
                &runtime,
                &bundles,
                None,
            )
            .await
            .is_err()
        );

        assert!(
            execute_command(
                Command::Serve {
                    bind: None,
                    dangerous_sandbox_mode: false,
                },
                &config,
                &runtime,
                &bundles,
                None,
            )
            .await
            .is_err()
        );

        assert!(
            execute_command(
                Command::Push {
                    source: temp.path().join("missing").display().to_string(),
                    to: "team/demo@0.1.0".to_string(),
                    hub: None,
                },
                &config,
                &runtime,
                &bundles,
                None,
            )
            .await
            .is_err()
        );

        assert!(
            execute_command(
                Command::Pull {
                    reference: "local/demo@0.1.0".to_string(),
                    hub: None,
                },
                &config,
                &runtime,
                &bundles,
                None,
            )
            .await
            .is_err()
        );

        assert!(
            execute_command(
                Command::Tui {
                    // Keep this on the preflight error path so the test never
                    // enters the real alternate-screen TUI.
                    bundle: Some("local/missing@0.1.0".to_string()),
                    cwd: None,
                    dangerous_sandbox_mode: false,
                },
                &config,
                &runtime,
                &bundles,
                None,
            )
            .await
            .is_err()
        );
    }
}
