use autoagents_llm::chat::ChatMessage;
use chrono::Utc;
use log::{debug, info};
use odyssey_rs_agent_abi::{HostToolSpec, RunRequest, json_to_string};
use odyssey_rs_manifest::AgentKind;
use odyssey_rs_protocol::{
    BundleRef, EventMsg, ExecutionRequest, ModelSpec, SessionSandboxOverlay, Task, TurnContext,
};
use odyssey_rs_sandbox::SandboxMode;
use odyssey_rs_tools::{ToolContext, WorkspaceMount, tools_to_adaptors};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::{sync::broadcast, time::Instant};
use uuid::Uuid;

use crate::{
    RunOutput, RuntimeError,
    agent::{ExecutorRun, WasmExecutorRun, run_executor, run_wasm_executor},
    memory::build_memory,
    resolver::{bundle::ResolvedBundle, bundle::resolve_bundle_from_ref},
    runtime::{
        engine::OdysseyRuntimeInner,
        history::TurnHistoryCollector,
        prompt::build_system_prompt,
        tool_event::{RuntimeApprovalHandler, RuntimeToolEventSink},
    },
    sandbox::{
        PreparedToolSandbox, build_permission_rules, prepare_cell, prepare_operator_command_cell,
    },
    session::{SessionRecord, TurnChatMessageRecord, TurnRecord},
    skill::BundleSkillStore,
    tool::select_tools,
};

pub(crate) struct ScheduleExecutor {
    runtime: Arc<OdysseyRuntimeInner>,
}

impl ScheduleExecutor {
    pub fn new(runtime: Arc<OdysseyRuntimeInner>) -> Self {
        Self { runtime }
    }
    pub(crate) async fn execute_request(
        &self,
        turn_id: Uuid,
        request: ExecutionRequest,
    ) -> Result<RunOutput, RuntimeError> {
        info!("Received execution request : {}", request.request_id);
        let _session_guard = self
            .runtime
            .lock_session_execution(request.session_id)
            .await;
        let session = self.runtime.sessions.get(request.session_id)?;
        let resolved = resolve_bundle_from_ref(
            &self.runtime.store,
            &BundleRef::from(session.bundle_ref.clone()),
            Some(&session.agent_id),
            &self.runtime.config.default_model,
        )?;
        let sender = self.runtime.sessions.sender(request.session_id)?;
        let receiver = self.runtime.sessions.subscribe(request.session_id)?;
        let start_time = Instant::now();
        let response = self
            .execute_resolved_bundle(
                resolved,
                session.clone(),
                turn_id,
                request.input.clone(),
                request.turn_context.clone(),
                sender.clone(),
            )
            .await?;
        info!(
            "Execution : {}, completed with time: {}",
            request.request_id,
            start_time.elapsed().as_millis()
        );
        let chat_history = collect_turn_chat_history(turn_id, &request.input, &response, receiver);
        self.runtime.sessions.append_turn(
            request.session_id,
            TurnRecord::from_history(
                turn_id,
                &request.input,
                response.clone(),
                chat_history,
                Utc::now(),
            ),
        )?;
        Ok(RunOutput {
            session_id: request.session_id,
            turn_id,
            response,
        })
    }

    async fn execute_resolved_bundle(
        &self,
        resolved: ResolvedBundle,
        session: SessionRecord,
        turn_id: Uuid,
        task: Task,
        turn_context_override: Option<odyssey_rs_protocol::TurnContextOverride>,
        sender: broadcast::Sender<EventMsg>,
    ) -> Result<String, RuntimeError> {
        let session_id = session.id;
        let mode_override = self.runtime.config.sandbox_mode_override;

        let mode =
            effective_sandbox_mode(&resolved.manifest, session.sandbox.as_ref(), mode_override);
        debug!("Effective Sandbox mode: {:?}", mode);

        //Prepare sandbox cell
        let permissions = build_permission_rules(&resolved.agent)?;
        let cell = prepare_resolved_bundle_cell(
            &mode,
            &self.runtime,
            &resolved,
            session.sandbox.as_ref(),
            session_id,
        )
        .await?;
        info!("Prepared bundle cell");
        info!("Built permission rules");
        let working_dir = resolve_execution_work_dir(
            &cell.work_dir,
            &cell.workspace_mounts,
            turn_context_override.as_ref(),
        )?;
        let event_sink = Arc::new(RuntimeToolEventSink {
            session_id,
            turn_id,
            sender: sender.clone(),
            working_dir: working_dir.display().to_string(),
        });
        let approval_handler = Arc::new(RuntimeApprovalHandler {
            session_id,
            turn_id,
            sender: sender.clone(),
            approvals: self.runtime.approvals.clone(),
        });
        let skills = Arc::new(BundleSkillStore::load(&cell.root)?);
        let system_prompt = build_system_prompt(
            &resolved.agent.prompt,
            &skills,
            !resolved.manifest.skills.is_empty(),
        );
        info!("Prepared System Prompt");
        let ctx = ToolContext {
            session_id,
            turn_id,
            bundle_root: cell.root.clone(),
            working_dir: working_dir.clone(),
            workspace_mounts: cell.workspace_mounts.clone(),
            sandbox: cell.sandbox,
            permission_rules: permissions,
            event_sink: Some(event_sink),
            approval_handler: Some(approval_handler),
            skills: Some(skills),
        };
        let selected = select_tools(&self.runtime.tools, &resolved.manifest, &resolved.agent);
        let host_tools = selected
            .iter()
            .map(|tool| HostToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                args_schema: tool.args_schema(),
                output_schema: tool.output_schema(),
            })
            .collect::<Vec<_>>();
        info!("Prepared Tool Context");
        let adapted = tools_to_adaptors(selected, ctx.clone());
        let model = resolve_model_spec(&session, &resolved, turn_context_override.as_ref());
        let llm = self.runtime.resolve_llm(&model).await?;
        info!("Built LLM using resolver");
        let turn_context = build_turn_context(
            working_dir.display().to_string(),
            model.clone(),
            mode,
            turn_context_override.as_ref(),
        );
        info!("Starting executor");
        let start_time = Instant::now();
        let result = if resolved.agent.kind == AgentKind::Wasm {
            if resolved.manifest.abi_version != resolved.agent.abi_version {
                return Err(RuntimeError::Executor(format!(
                    "bundle ABI `{}` does not match agent ABI `{}` for `{}`",
                    resolved.manifest.abi_version, resolved.agent.abi_version, resolved.agent.id
                )));
            }
            run_wasm_executor(WasmExecutorRun {
                module_path: crate::agent::resolve_module_path(
                    &resolved.install_path,
                    &resolved.agent.program.entrypoint,
                )?,
                agent_id: resolved.agent.id.clone(),
                abi_version: resolved.agent.abi_version.clone(),
                llm,
                tools: adapted,
                session_id,
                turn_id,
                sender,
                turn_context,
                request: RunRequest {
                    session_id: session_id.to_string(),
                    turn_id: turn_id.to_string(),
                    prompt: task.prompt.clone(),
                    system_prompt: task
                        .system_prompt
                        .clone()
                        .or_else(|| Some(system_prompt.clone())),
                    history_json: Some(json_to_string(&session_history(&session.turns)).map_err(
                        |err| {
                            RuntimeError::Executor(format!(
                                "failed to serialize wasm session history: {err}"
                            ))
                        },
                    )?),
                    metadata_json: Some(
                        json_to_string(&serde_json::json!({
                            "bundle_id": resolved.manifest.id,
                            "agent_id": resolved.agent.id,
                            "model": {
                                "provider": model.provider,
                                "name": model.name,
                                "config": model.config,
                            }
                        }))
                        .map_err(|err| {
                            RuntimeError::Executor(format!(
                                "failed to serialize wasm run metadata: {err}"
                            ))
                        })?,
                    ),
                    host_tools,
                },
            })
            .await
        } else {
            let memory = build_memory(&resolved.agent, &session.turns)?;
            info!("Built Memory");
            run_executor(ExecutorRun {
                executor_id: resolved.agent.execution.executor.clone(),
                llm,
                system_prompt,
                task,
                memory,
                tools: adapted,
                session_id,
                turn_id,
                sender,
                turn_context,
            })
            .await
        };
        info!(
            "Completed executor - Time taken: {}",
            start_time.elapsed().as_millis()
        );
        result
    }
}

fn resolve_execution_work_dir(
    default_work_dir: &Path,
    workspace_mounts: &[WorkspaceMount],
    override_ctx: Option<&odyssey_rs_protocol::TurnContextOverride>,
) -> Result<PathBuf, RuntimeError> {
    let Some(cwd) = override_ctx.and_then(|ctx| ctx.cwd.as_ref()) else {
        return Ok(default_work_dir.to_path_buf());
    };
    if cwd.trim().is_empty() {
        return Ok(default_work_dir.to_path_buf());
    }

    let requested = PathBuf::from(cwd);
    if requested.is_absolute() {
        if requested.starts_with(default_work_dir) {
            return Ok(requested);
        }
        for mount in workspace_mounts {
            if requested == mount.host_root || requested.starts_with(&mount.host_root) {
                let suffix = requested.strip_prefix(&mount.host_root).map_err(|err| {
                    RuntimeError::Executor(format!("invalid cwd override: {err}"))
                })?;
                return Ok(mount.visible_root.join(suffix));
            }
        }
        return Err(RuntimeError::Executor(format!(
            "working directory is not visible inside sandbox: {}",
            requested.display()
        )));
    }

    Ok(default_work_dir.join(requested))
}

fn session_history(turns: &[TurnRecord]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for turn in turns {
        if !turn.chat_history.is_empty() {
            messages.extend(
                turn.chat_history
                    .iter()
                    .cloned()
                    .map(TurnChatMessageRecord::into_chat_message),
            );
        }
    }
    messages
}

fn collect_turn_chat_history(
    turn_id: Uuid,
    task: &Task,
    response: &str,
    mut receiver: broadcast::Receiver<EventMsg>,
) -> Vec<TurnChatMessageRecord> {
    let mut collector = TurnHistoryCollector::new(turn_id, task);
    while let Ok(event) = receiver.try_recv() {
        collector.observe(event);
    }
    collector.finish(response)
}

pub(crate) async fn prepare_resolved_bundle_cell(
    mode: &SandboxMode,
    runtime: &Arc<OdysseyRuntimeInner>,
    resolved: &ResolvedBundle,
    session_overlay: Option<&SessionSandboxOverlay>,
    session_id: Uuid,
) -> Result<PreparedToolSandbox, RuntimeError> {
    let sandbox_runtime = if mode == &SandboxMode::DangerFullAccess {
        runtime.host_sandbox.clone()
    } else {
        runtime.restricted_sandbox.clone()
    };

    prepare_cell(
        &sandbox_runtime,
        session_id,
        &resolved.agent.id,
        &resolved.install_path,
        &resolved.manifest,
        session_overlay,
        runtime.config.sandbox_mode_override,
    )
    .await
}

pub(crate) async fn prepare_resolved_bundle_command_cell(
    mode: &SandboxMode,
    runtime: &Arc<OdysseyRuntimeInner>,
    resolved: &ResolvedBundle,
    session_overlay: Option<&SessionSandboxOverlay>,
    session_id: Uuid,
) -> Result<PreparedToolSandbox, RuntimeError> {
    let sandbox_runtime = if mode == &SandboxMode::DangerFullAccess {
        runtime.host_sandbox.clone()
    } else {
        runtime.restricted_sandbox.clone()
    };

    prepare_operator_command_cell(
        &sandbox_runtime,
        session_id,
        &resolved.agent.id,
        &resolved.install_path,
        &resolved.manifest,
        session_overlay,
        runtime.config.sandbox_mode_override,
    )
    .await
}

pub(crate) fn effective_sandbox_mode(
    manifest: &odyssey_rs_manifest::BundleManifest,
    session_overlay: Option<&SessionSandboxOverlay>,
    override_mode: Option<SandboxMode>,
) -> SandboxMode {
    override_mode
        .or_else(|| session_overlay.and_then(|overlay| overlay.mode))
        .unwrap_or(manifest.sandbox.mode)
}

fn build_turn_context(
    cwd: String,
    model: ModelSpec,
    sandbox_mode: SandboxMode,
    override_ctx: Option<&odyssey_rs_protocol::TurnContextOverride>,
) -> TurnContext {
    let mut context = TurnContext {
        cwd: Some(cwd),
        model: Some(model),
        sandbox_mode: Some(sandbox_mode),
        approval_policy: None,
        metadata: serde_json::json!({}),
    };
    if let Some(override_ctx) = override_ctx {
        context.apply_override(override_ctx);
    }
    context
}

fn resolve_model_spec(
    session: &SessionRecord,
    resolved: &ResolvedBundle,
    override_ctx: Option<&odyssey_rs_protocol::TurnContextOverride>,
) -> ModelSpec {
    if let Some(model) = override_ctx.and_then(|ctx| ctx.model.clone()) {
        return model;
    }

    let default_model = &resolved.model;
    let config = if session.model_provider == default_model.provider
        && session.model_id == default_model.name
    {
        session
            .model_config
            .clone()
            .or_else(|| default_model.config.clone())
    } else {
        session.model_config.clone()
    };

    ModelSpec {
        provider: session.model_provider.clone(),
        name: session.model_id.clone(),
        config,
    }
}

#[cfg(test)]
mod tests {
    use autoagents_llm::chat::ChatRole;
    use chrono::Utc;
    use odyssey_rs_manifest::{
        BundleExecutor, BundleManifest, BundleMemory, BundleSandbox, BundleSignatures, BundleTool,
        ManifestVersion, ProviderKind,
    };
    use odyssey_rs_protocol::{EventMsg, EventPayload};
    use odyssey_rs_protocol::{ModelSpec, SessionSandboxOverlay, Task, TurnContextOverride};
    use odyssey_rs_sandbox::SandboxMode;
    use odyssey_rs_tools::WorkspaceMount;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::path::PathBuf;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    use crate::resolver::bundle::ResolvedBundle;
    use crate::runtime::executor::{
        build_turn_context, collect_turn_chat_history, effective_sandbox_mode,
        resolve_execution_work_dir, resolve_model_spec, session_history,
    };
    use crate::session::{SessionRecord, TurnChatMessageRecord, TurnRecord};

    fn manifest(mode: SandboxMode) -> BundleManifest {
        BundleManifest {
            api_version: "odyssey.ai/bundle.v1".to_string(),
            kind: "AgentBundle".to_string(),
            id: "demo".to_string(),
            version: "0.1.0".to_string(),
            manifest_version: ManifestVersion::V1,
            abi_version: "v1".to_string(),
            readme: "README.md".to_string(),
            agent_spec: "agents/demo/agent.yaml".to_string(),
            executor: BundleExecutor {
                kind: ProviderKind::Prebuilt,
                id: "react".to_string(),
                config: json!({}),
            },
            memory: BundleMemory::default(),
            skills: Vec::new(),
            tools: vec![BundleTool {
                name: "Read".to_string(),
                source: "builtin".to_string(),
            }],
            sandbox: BundleSandbox {
                mode,
                ..BundleSandbox::default()
            },
            signatures: BundleSignatures::default(),
            agents: Vec::new(),
        }
    }

    fn agent(model: ModelSpec) -> odyssey_rs_manifest::AgentSpec {
        odyssey_rs_manifest::AgentSpec {
            id: "demo".to_string(),
            name: "demo".to_string(),
            prompt: "You are demo".to_string(),
            model,
            ..odyssey_rs_manifest::AgentSpec::default()
        }
    }

    #[test]
    fn collect_turn_chat_history_preserves_tool_use_and_result_ids() {
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let tool_call_id = Uuid::new_v4();
        let (sender, receiver) = broadcast::channel(32);

        let _ = sender.send(EventMsg {
            id: Uuid::new_v4(),
            session_id,
            created_at: Utc::now(),
            payload: EventPayload::ToolCallStarted {
                turn_id,
                tool_call_id,
                tool_name: "Write".to_string(),
                arguments: json!({ "path": "helloworld.py" }),
            },
        });
        let _ = sender.send(EventMsg {
            id: Uuid::new_v4(),
            session_id,
            created_at: Utc::now(),
            payload: EventPayload::ToolCallFinished {
                turn_id,
                tool_call_id,
                result: json!({ "error": "permission denied" }),
                success: false,
            },
        });
        let _ = sender.send(EventMsg {
            id: Uuid::new_v4(),
            session_id,
            created_at: Utc::now(),
            payload: EventPayload::TurnCompleted {
                turn_id,
                message: "The write failed.".to_string(),
            },
        });

        let history = collect_turn_chat_history(
            turn_id,
            &Task::new("create file"),
            "The write failed.",
            receiver,
        );

        assert_eq!(history.len(), 4);
        assert_eq!(history[0].content, "create file");
        assert_eq!(history[1].tool_calls[0].id, tool_call_id.to_string());
        assert_eq!(history[2].tool_calls[0].id, tool_call_id.to_string());
        assert_eq!(history[3].content, "The write failed.");
    }

    #[test]
    fn effective_sandbox_mode_prefers_override() {
        assert_eq!(
            effective_sandbox_mode(
                &manifest(SandboxMode::WorkspaceWrite),
                None,
                Some(SandboxMode::DangerFullAccess)
            ),
            SandboxMode::DangerFullAccess
        );
        assert_eq!(
            effective_sandbox_mode(&manifest(SandboxMode::WorkspaceWrite), None, None),
            SandboxMode::WorkspaceWrite
        );
    }

    #[test]
    fn effective_sandbox_mode_uses_session_overlay_when_runtime_override_is_absent() {
        assert_eq!(
            effective_sandbox_mode(
                &manifest(SandboxMode::WorkspaceWrite),
                Some(&SessionSandboxOverlay {
                    mode: Some(SandboxMode::DangerFullAccess),
                    ..SessionSandboxOverlay::default()
                }),
                None
            ),
            SandboxMode::DangerFullAccess
        );
    }

    #[test]
    fn build_turn_context_applies_overrides() {
        let context = build_turn_context(
            "/workspace/demo".to_string(),
            ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-4.1-mini".to_string(),
                config: None,
            },
            SandboxMode::WorkspaceWrite,
            Some(&TurnContextOverride {
                model: Some(ModelSpec {
                    provider: "openai".to_string(),
                    name: "gpt-4o".to_string(),
                    config: None,
                }),
                ..TurnContextOverride::default()
            }),
        );

        assert_eq!(context.cwd.as_deref(), Some("/workspace/demo"));
        assert_eq!(context.model.unwrap().name, "gpt-4o");
        assert_eq!(context.metadata, json!({}));
    }

    #[test]
    fn resolve_model_spec_falls_back_to_bundle_default_config() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            bundle_ref: "demo@latest".to_string(),
            agent_id: "demo".to_string(),
            model_provider: "openai".to_string(),
            model_id: "gpt-5".to_string(),
            model_config: None,
            sandbox: None,
            created_at: Utc::now(),
            turns: Vec::new(),
        };
        let resolved = ResolvedBundle {
            install_path: std::path::PathBuf::from("/workspace/demo"),
            namespace: "local".to_string(),
            manifest: manifest(SandboxMode::WorkspaceWrite),
            agent: agent(ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            }),
            agents: Vec::new(),
            model: ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            },
        };

        let model = resolve_model_spec(&session, &resolved, None);

        assert_eq!(model.config, Some(json!({ "reasoning_effort": "medium" })));
    }

    #[test]
    fn resolve_model_spec_prefers_turn_override() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            bundle_ref: "demo@latest".to_string(),
            agent_id: "demo".to_string(),
            model_provider: "openai".to_string(),
            model_id: "gpt-5".to_string(),
            model_config: Some(json!({ "reasoning_effort": "medium" })),
            sandbox: None,
            created_at: Utc::now(),
            turns: Vec::new(),
        };
        let resolved = ResolvedBundle {
            install_path: std::path::PathBuf::from("/workspace/demo"),
            namespace: "local".to_string(),
            manifest: manifest(SandboxMode::WorkspaceWrite),
            agent: agent(ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            }),
            agents: Vec::new(),
            model: ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            },
        };
        let override_ctx = TurnContextOverride {
            model: Some(ModelSpec {
                provider: "anthropic".to_string(),
                name: "claude-sonnet".to_string(),
                config: Some(json!({ "temperature": 0.2 })),
            }),
            ..TurnContextOverride::default()
        };

        let model = resolve_model_spec(&session, &resolved, Some(&override_ctx));

        assert_eq!(model.provider, "anthropic");
        assert_eq!(model.name, "claude-sonnet");
        assert_eq!(model.config, Some(json!({ "temperature": 0.2 })));
    }

    #[test]
    fn resolve_model_spec_uses_session_config_for_matching_and_non_matching_models() {
        let resolved = ResolvedBundle {
            install_path: std::path::PathBuf::from("/workspace/demo"),
            namespace: "local".to_string(),
            manifest: manifest(SandboxMode::WorkspaceWrite),
            agent: agent(ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            }),
            agents: Vec::new(),
            model: ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "medium" })),
            },
        };

        let matching_session = SessionRecord {
            id: Uuid::new_v4(),
            bundle_ref: "demo@latest".to_string(),
            agent_id: "demo".to_string(),
            model_provider: "openai".to_string(),
            model_id: "gpt-5".to_string(),
            model_config: Some(json!({ "reasoning_effort": "low" })),
            sandbox: None,
            created_at: Utc::now(),
            turns: Vec::new(),
        };
        let alternate_session = SessionRecord {
            id: Uuid::new_v4(),
            bundle_ref: "demo@latest".to_string(),
            agent_id: "demo".to_string(),
            model_provider: "anthropic".to_string(),
            model_id: "claude-sonnet-4-5".to_string(),
            model_config: Some(json!({ "temperature": 0.2 })),
            sandbox: None,
            created_at: Utc::now(),
            turns: Vec::new(),
        };

        assert_eq!(
            resolve_model_spec(&matching_session, &resolved, None),
            ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-5".to_string(),
                config: Some(json!({ "reasoning_effort": "low" })),
            }
        );
        assert_eq!(
            resolve_model_spec(&alternate_session, &resolved, None),
            ModelSpec {
                provider: "anthropic".to_string(),
                name: "claude-sonnet-4-5".to_string(),
                config: Some(json!({ "temperature": 0.2 })),
            }
        );
    }

    #[test]
    fn resolve_execution_work_dir_maps_host_cwd_into_visible_mount() {
        let work_dir = PathBuf::from("/sandbox/app");
        let mounts = vec![WorkspaceMount {
            visible_root: PathBuf::from("/sandbox/app/mount/read/current"),
            host_root: PathBuf::from("/workspace/project"),
            writable: false,
        }];

        let resolved = resolve_execution_work_dir(
            &work_dir,
            &mounts,
            Some(&TurnContextOverride {
                cwd: Some("/workspace/project/src".to_string()),
                model: None,
            }),
        )
        .expect("resolved cwd");

        assert_eq!(
            resolved,
            PathBuf::from("/sandbox/app/mount/read/current/src")
        );
    }

    #[test]
    fn resolve_execution_work_dir_rejects_unmounted_absolute_cwd() {
        let error = resolve_execution_work_dir(
            &PathBuf::from("/sandbox/app"),
            &[],
            Some(&TurnContextOverride {
                cwd: Some("/workspace/project".to_string()),
                model: None,
            }),
        )
        .expect_err("unmounted cwd should fail");

        assert_eq!(
            error.to_string(),
            "executor error: working directory is not visible inside sandbox: /workspace/project"
        );
    }

    #[test]
    fn resolve_execution_work_dir_defaults_and_relative_paths_are_supported() {
        let default_work_dir = PathBuf::from("/sandbox/app");

        assert_eq!(
            resolve_execution_work_dir(&default_work_dir, &[], None).expect("default cwd"),
            default_work_dir
        );
        assert_eq!(
            resolve_execution_work_dir(
                &default_work_dir,
                &[],
                Some(&TurnContextOverride {
                    cwd: Some("   ".to_string()),
                    ..TurnContextOverride::default()
                }),
            )
            .expect("blank cwd"),
            PathBuf::from("/sandbox/app")
        );
        assert_eq!(
            resolve_execution_work_dir(
                &default_work_dir,
                &[],
                Some(&TurnContextOverride {
                    cwd: Some("src/bin".to_string()),
                    ..TurnContextOverride::default()
                }),
            )
            .expect("relative cwd"),
            PathBuf::from("/sandbox/app/src/bin")
        );
        assert_eq!(
            resolve_execution_work_dir(
                &default_work_dir,
                &[],
                Some(&TurnContextOverride {
                    cwd: Some("/sandbox/app/logs".to_string()),
                    ..TurnContextOverride::default()
                }),
            )
            .expect("visible absolute cwd"),
            PathBuf::from("/sandbox/app/logs")
        );
    }

    #[test]
    fn session_history_ignores_empty_turns_and_preserves_chat_messages() {
        let turns = vec![
            TurnRecord {
                turn_id: Uuid::new_v4(),
                prompt: "plain prompt".to_string(),
                response: "plain response".to_string(),
                chat_history: Vec::new(),
                created_at: Utc::now(),
            },
            TurnRecord::from_history(
                Uuid::new_v4(),
                &Task::new("ignored"),
                "ignored",
                vec![
                    TurnChatMessageRecord::from_text(ChatRole::User, "hello"),
                    TurnChatMessageRecord::from_text(ChatRole::Assistant, "world"),
                ],
                Utc::now(),
            ),
        ];

        let history = session_history(&turns);

        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].content, "world");
    }
}
