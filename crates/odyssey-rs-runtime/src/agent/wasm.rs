use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use autoagents_llm::LLMProvider;
use autoagents_llm::chat::{ChatMessage, StructuredOutputFormat, Tool};
use autoagents_protocol::Event as AutoAgentsEvent;
use odyssey_rs_agent_abi::{
    ABI_VERSION, AgentDescriptor, HostToolCallRequest, HostToolCallResponse, HostToolDefinition,
    LlmChatRequest, LlmChatResponse, RUNNER_CLASS, RunRequest, RunResponse, json_to_string,
    string_to_json,
};
use odyssey_rs_protocol::{EventMsg, EventPayload, TurnContext};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::RuntimeError;

use super::{AutoagentsEventBridge, emit};
use autoagents_core::tool::ToolT;

wasmtime::component::bindgen!({
    path: "../odyssey-rs-agent-abi/wit",
    world: "odyssey-agent-world",
});

use self::exports::odyssey::agent::odyssey_agent;
use self::odyssey::agent::odyssey_host;

#[derive(Clone)]
pub(crate) struct WasmExecutorRun {
    pub module_path: PathBuf,
    pub agent_id: String,
    pub abi_version: String,
    pub llm: Arc<dyn LLMProvider>,
    pub tools: Vec<Arc<dyn ToolT>>,
    pub session_id: Uuid,
    pub turn_id: Uuid,
    pub sender: broadcast::Sender<EventMsg>,
    pub turn_context: TurnContext,
    pub request: RunRequest,
}

pub(crate) async fn run_wasm_executor(run: WasmExecutorRun) -> Result<String, RuntimeError> {
    let host = ComponentHost::new(
        run.llm,
        run.tools,
        run.session_id,
        run.turn_id,
        run.sender.clone(),
        run.turn_context.clone(),
    );
    let request_json = json_to_string(&run.request).map_err(|err| {
        RuntimeError::Executor(format!("failed to serialize wasm run request: {err}"))
    })?;
    let module_path = run.module_path.clone();
    let expected_agent_id = run.agent_id.clone();
    let expected_abi_version = run.abi_version.clone();
    let response_json = tokio::task::spawn_blocking(move || {
        execute_component(
            &module_path,
            host,
            &request_json,
            &expected_agent_id,
            &expected_abi_version,
        )
    })
    .await
    .map_err(|err| RuntimeError::Executor(format!("failed to join wasm executor task: {err}")))??;
    let response: RunResponse = string_to_json(&response_json).map_err(|err| {
        RuntimeError::Executor(format!("failed to decode wasm run response: {err}"))
    })?;
    let message = render_output(response.output_json);
    emit(
        &run.sender,
        run.session_id,
        EventPayload::TurnCompleted {
            turn_id: run.turn_id,
            message: message.clone(),
        },
    );
    Ok(message)
}

struct WasiHost {
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiHost {
    fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        }
    }
}

impl WasiView for WasiHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

struct ComponentHost {
    wasi: WasiHost,
    llm: Arc<dyn LLMProvider>,
    tools: HashMap<String, Arc<dyn ToolT>>,
    session_id: Uuid,
    sender: broadcast::Sender<EventMsg>,
    event_bridge: Mutex<AutoagentsEventBridge>,
}

impl ComponentHost {
    fn new(
        llm: Arc<dyn LLMProvider>,
        tools: Vec<Arc<dyn ToolT>>,
        session_id: Uuid,
        turn_id: Uuid,
        sender: broadcast::Sender<EventMsg>,
        turn_context: TurnContext,
    ) -> Self {
        Self {
            wasi: WasiHost::new(),
            llm,
            tools: tools
                .into_iter()
                .map(|tool| (tool.name().to_string(), tool))
                .collect(),
            session_id,
            sender,
            event_bridge: Mutex::new(AutoagentsEventBridge::new(turn_id, turn_context)),
        }
    }
}

impl WasiView for ComponentHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        self.wasi.ctx()
    }
}

fn block_on_future<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

impl odyssey_host::Host for ComponentHost {
    fn llm_chat(&mut self, request_json: String) -> Result<String, String> {
        let request: LlmChatRequest = match string_to_json(&request_json) {
            Ok(request) => request,
            Err(err) => return Err(format!("invalid llm chat request: {err}")),
        };
        let messages: Vec<ChatMessage> = match string_to_json(&request.messages_json) {
            Ok(messages) => messages,
            Err(err) => return Err(format!("invalid llm chat messages: {err}")),
        };
        let tools: Option<Vec<Tool>> = match request.tools_json.as_deref() {
            Some(raw) => match string_to_json::<Vec<HostToolDefinition>>(raw) {
                Ok(tools) => Some(
                    tools
                        .into_iter()
                        .map(|tool| Tool {
                            tool_type: "function".to_string(),
                            function: autoagents_llm::chat::FunctionTool {
                                name: tool.name,
                                description: tool.description,
                                parameters: tool.parameters,
                            },
                        })
                        .collect(),
                ),
                Err(err) => return Err(format!("invalid llm tool schema payload: {err}")),
            },
            None => None,
        };
        let output_schema: Option<StructuredOutputFormat> =
            match request.output_schema_json.as_deref() {
                Some(raw) => match string_to_json(raw) {
                    Ok(schema) => Some(schema),
                    Err(err) => return Err(format!("invalid llm output schema payload: {err}")),
                },
                None => None,
            };

        let response = block_on_future(self.llm.chat_with_tools(
            &messages,
            tools.as_deref(),
            output_schema,
        ));
        let response = match response {
            Ok(response) => response,
            Err(err) => return Err(err.to_string()),
        };
        let payload = LlmChatResponse {
            text: response.text().unwrap_or_default(),
            reasoning: response.thinking().unwrap_or_default(),
            tool_calls_json: response
                .tool_calls()
                .map(|tool_calls| json_to_string(&tool_calls))
                .transpose()
                .map_err(|err| err.to_string())?,
        };
        json_to_string(&payload).map_err(|err| err.to_string())
    }

    fn tool_call(&mut self, request_json: String) -> Result<String, String> {
        let request: HostToolCallRequest = match string_to_json(&request_json) {
            Ok(request) => request,
            Err(err) => return Err(format!("invalid tool request: {err}")),
        };
        let args: Value = match string_to_json(&request.arguments_json) {
            Ok(args) => args,
            Err(err) => return Err(format!("invalid tool arguments: {err}")),
        };
        let Some(tool) = self.tools.get(&request.tool).cloned() else {
            return Err(format!("tool `{}` is not available", request.tool));
        };
        let result = block_on_future(tool.execute(args));
        let result = match result {
            Ok(result) => result,
            Err(err) => return Err(err.to_string()),
        };
        let payload = HostToolCallResponse {
            result_json: json_to_string(&result).map_err(|err| err.to_string())?,
        };
        json_to_string(&payload).map_err(|err| err.to_string())
    }

    fn emit_event(&mut self, event_json: String) -> Result<(), String> {
        let event: AutoAgentsEvent = match string_to_json(&event_json) {
            Ok(event) => event,
            Err(err) => return Err(format!("invalid autoagents event: {err}")),
        };
        let mapped = self.event_bridge.lock().map_event(event);
        for payload in mapped.payloads {
            emit(&self.sender, self.session_id, payload);
        }
        Ok(())
    }
}

pub(crate) fn resolve_module_path(
    install_path: &Path,
    entrypoint: &str,
) -> Result<PathBuf, RuntimeError> {
    let path = install_path.join(entrypoint);
    if path.is_file() {
        Ok(path)
    } else {
        Err(RuntimeError::Executor(format!(
            "wasm module entrypoint `{entrypoint}` was not found under {}",
            install_path.display()
        )))
    }
}

fn execute_component(
    module_path: &Path,
    host: ComponentHost,
    request_json: &str,
    expected_agent_id: &str,
    expected_abi_version: &str,
) -> Result<String, RuntimeError> {
    let bytes = fs::read(module_path).map_err(|err| RuntimeError::Io {
        path: module_path.display().to_string(),
        message: err.to_string(),
    })?;
    if !wasmparser::Parser::is_component(&bytes) {
        return Err(RuntimeError::Executor(format!(
            "wasm agent entrypoint `{}` is not a component",
            module_path.display()
        )));
    }

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)
        .map_err(|err| RuntimeError::Executor(format!("failed to build wasmtime engine: {err}")))?;
    let component = Component::from_binary(&engine, &bytes).map_err(|err| {
        RuntimeError::Executor(format!(
            "failed to load WebAssembly component `{}`: {err}",
            module_path.display()
        ))
    })?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|err| {
        RuntimeError::Executor(format!("failed to wire WASI host functions: {err}"))
    })?;
    odyssey_host::add_to_linker::<ComponentHost, wasmtime::component::HasSelf<ComponentHost>>(
        &mut linker,
        |state: &mut ComponentHost| state,
    )
    .map_err(|err| {
        RuntimeError::Executor(format!("failed to wire Odyssey host functions: {err}"))
    })?;

    let mut store = Store::new(&engine, host);
    let bindings =
        OdysseyAgentWorld::instantiate(&mut store, &component, &linker).map_err(|err| {
            RuntimeError::Executor(format!("failed to instantiate wasm component: {err}"))
        })?;

    let descriptor = bindings
        .interface0
        .call_describe(&mut store)
        .map_err(|err| {
            RuntimeError::Executor(format!("failed to describe wasm component: {err}"))
        })?;
    validate_descriptor(
        expected_agent_id,
        expected_abi_version,
        from_component_descriptor(descriptor),
    )?;

    bindings
        .interface0
        .call_run(&mut store, request_json)
        .map_err(|err| RuntimeError::Executor(format!("failed to execute wasm component: {err}")))?
        .map_err(RuntimeError::Executor)
}

fn validate_descriptor(
    expected_agent_id: &str,
    expected_abi_version: &str,
    descriptor: AgentDescriptor,
) -> Result<(), RuntimeError> {
    if descriptor.id != expected_agent_id {
        return Err(RuntimeError::Executor(format!(
            "wasm component id `{}` does not match selected agent `{expected_agent_id}`",
            descriptor.id
        )));
    }
    if descriptor.runner_class != RUNNER_CLASS {
        return Err(RuntimeError::Executor(format!(
            "unsupported wasm runner class `{}`",
            descriptor.runner_class
        )));
    }
    if descriptor.abi_version != expected_abi_version {
        return Err(RuntimeError::Executor(format!(
            "wasm component ABI `{}` does not match selected agent ABI `{expected_abi_version}`",
            descriptor.abi_version
        )));
    }
    if descriptor.abi_version != ABI_VERSION {
        return Err(RuntimeError::Executor(format!(
            "unsupported wasm agent ABI `{}`; expected `{ABI_VERSION}`",
            descriptor.abi_version
        )));
    }
    Ok(())
}

fn from_component_descriptor(descriptor: odyssey_agent::AgentDescriptor) -> AgentDescriptor {
    AgentDescriptor {
        id: descriptor.id,
        abi_version: descriptor.abi_version,
        runner_class: descriptor.runner_class,
    }
}

fn render_output(output_json: String) -> String {
    match serde_json::from_str::<Value>(&output_json) {
        Ok(Value::String(text)) => text,
        Ok(other) => serde_json::to_string_pretty(&other).unwrap_or(output_json),
        Err(_) => output_json,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComponentHost, WasmExecutorRun, execute_component, from_component_descriptor,
        render_output, resolve_module_path, run_wasm_executor, validate_descriptor,
    };
    use async_trait::async_trait;
    use autoagents_core::tool::{ToolCallError, ToolRuntime, ToolT};
    use autoagents_llm::chat::{
        ChatMessage, ChatProvider, ChatResponse, ChatRole, MessageType, StructuredOutputFormat,
        Tool,
    };
    use autoagents_llm::completion::{CompletionProvider, CompletionRequest, CompletionResponse};
    use autoagents_llm::embedding::EmbeddingProvider;
    use autoagents_llm::error::LLMError;
    use autoagents_llm::models::{ModelListRequest, ModelListResponse, ModelsProvider};
    use autoagents_llm::{FunctionCall, LLMProvider, ToolCall};
    use autoagents_protocol::Event as AutoAgentsEvent;
    use odyssey_rs_agent_abi::{
        ABI_VERSION, AgentDescriptor, HostToolCallRequest, HostToolCallResponse,
        HostToolDefinition, HostToolSpec, LlmChatRequest, LlmChatResponse, RUNNER_CLASS,
        RunRequest, json_to_string, string_to_json,
    };
    use odyssey_rs_protocol::{EventPayload, TurnContext};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use std::fmt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[derive(Clone, Debug)]
    struct LlmInvocation {
        messages_json: String,
        tool_names: Vec<String>,
        output_schema: Option<StructuredOutputFormat>,
    }

    #[derive(Clone, Default)]
    struct RecordingLlmProvider {
        calls: Arc<Mutex<Vec<LlmInvocation>>>,
        error: Option<String>,
        text: String,
        reasoning: String,
        tool_calls: Vec<ToolCall>,
    }

    #[derive(Clone, Debug)]
    struct StubChatResponse {
        text: String,
        reasoning: String,
        tool_calls: Vec<ToolCall>,
    }

    impl fmt::Display for StubChatResponse {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.text)
        }
    }

    impl ChatResponse for StubChatResponse {
        fn text(&self) -> Option<String> {
            Some(self.text.clone())
        }

        fn tool_calls(&self) -> Option<Vec<ToolCall>> {
            if self.tool_calls.is_empty() {
                None
            } else {
                Some(self.tool_calls.clone())
            }
        }

        fn thinking(&self) -> Option<String> {
            if self.reasoning.is_empty() {
                None
            } else {
                Some(self.reasoning.clone())
            }
        }
    }

    #[async_trait]
    impl ChatProvider for RecordingLlmProvider {
        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            tools: Option<&[Tool]>,
            json_schema: Option<StructuredOutputFormat>,
        ) -> Result<Box<dyn ChatResponse>, LLMError> {
            self.calls.lock().expect("lock calls").push(LlmInvocation {
                messages_json: serde_json::to_string(messages).expect("serialize messages"),
                tool_names: tools
                    .unwrap_or(&[])
                    .iter()
                    .map(|tool| tool.function.name.clone())
                    .collect(),
                output_schema: json_schema,
            });

            if let Some(error) = &self.error {
                return Err(LLMError::ProviderError(error.clone()));
            }

            Ok(Box::new(StubChatResponse {
                text: self.text.clone(),
                reasoning: self.reasoning.clone(),
                tool_calls: self.tool_calls.clone(),
            }))
        }
    }

    #[async_trait]
    impl CompletionProvider for RecordingLlmProvider {
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<CompletionResponse, LLMError> {
            Ok(CompletionResponse {
                text: self.text.clone(),
            })
        }
    }

    #[async_trait]
    impl EmbeddingProvider for RecordingLlmProvider {
        async fn embed(&self, _input: Vec<String>) -> Result<Vec<Vec<f32>>, LLMError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ModelsProvider for RecordingLlmProvider {
        async fn list_models(
            &self,
            _request: Option<&ModelListRequest>,
        ) -> Result<Box<dyn ModelListResponse>, LLMError> {
            Err(LLMError::ProviderError("not used in tests".to_string()))
        }
    }

    impl LLMProvider for RecordingLlmProvider {}

    #[derive(Debug)]
    struct EchoTool {
        name: &'static str,
    }

    #[derive(Debug)]
    struct FailingTool;

    #[async_trait]
    impl ToolRuntime for EchoTool {
        async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
            Ok(json!({ "echo": args }))
        }
    }

    impl ToolT for EchoTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "echo tool"
        }

        fn args_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            })
        }
    }

    #[async_trait]
    impl ToolRuntime for FailingTool {
        async fn execute(&self, _args: Value) -> Result<Value, ToolCallError> {
            Err(ToolCallError::RuntimeError(
                std::io::Error::other("tool failed").into(),
            ))
        }
    }

    impl ToolT for FailingTool {
        fn name(&self) -> &str {
            "Write"
        }

        fn description(&self) -> &str {
            "failing tool"
        }

        fn args_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            })
        }
    }

    fn component_host(
        llm: Arc<dyn LLMProvider>,
        tools: Vec<Arc<dyn ToolT>>,
    ) -> (
        ComponentHost,
        broadcast::Receiver<odyssey_rs_protocol::EventMsg>,
    ) {
        let (sender, receiver) = broadcast::channel(8);
        (
            ComponentHost::new(
                llm,
                tools,
                Uuid::from_u128(10),
                Uuid::from_u128(11),
                sender,
                TurnContext::default(),
            ),
            receiver,
        )
    }

    fn write_file(root: &Path, relative: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = root.join(relative);
        std::fs::write(&path, contents).expect("write file");
        path
    }

    #[test]
    fn resolve_module_path_accepts_existing_files_and_rejects_missing_entrypoints() {
        let temp = tempdir().expect("tempdir");
        let module_path = write_file(temp.path(), "agent.wasm", b"\0asm");

        assert_eq!(
            resolve_module_path(temp.path(), "agent.wasm").expect("resolve existing"),
            module_path
        );

        let error = resolve_module_path(temp.path(), "missing.wasm")
            .expect_err("missing entrypoint must fail");
        assert_eq!(
            error.to_string(),
            format!(
                "executor error: wasm module entrypoint `missing.wasm` was not found under {}",
                temp.path().display()
            )
        );
    }

    #[test]
    fn execute_component_rejects_missing_files_and_non_component_bytes() {
        let temp = tempdir().expect("tempdir");
        let llm = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;

        let (missing_host, _) = component_host(llm.clone(), Vec::new());
        let missing_path = temp.path().join("missing.wasm");
        let missing_error =
            execute_component(&missing_path, missing_host, "{}", "agent", ABI_VERSION)
                .expect_err("missing module should fail");
        assert_eq!(
            missing_error.to_string(),
            format!(
                "io error at {}: No such file or directory (os error 2)",
                missing_path.display()
            )
        );

        let non_component_path =
            write_file(temp.path(), "not-a-component.wasm", b"\0asm\x01\0\0\0");
        let (non_component_host, _) = component_host(llm, Vec::new());
        let non_component_error = execute_component(
            &non_component_path,
            non_component_host,
            "{}",
            "agent",
            ABI_VERSION,
        )
        .expect_err("plain wasm module should fail");
        assert_eq!(
            non_component_error.to_string(),
            format!(
                "executor error: wasm agent entrypoint `{}` is not a component",
                non_component_path.display()
            )
        );
    }

    #[test]
    fn validate_descriptor_rejects_incompatible_components() {
        let base = AgentDescriptor {
            id: "demo".to_string(),
            abi_version: ABI_VERSION.to_string(),
            runner_class: RUNNER_CLASS.to_string(),
        };

        validate_descriptor("demo", ABI_VERSION, base.clone()).expect("matching descriptor");

        let wrong_id = validate_descriptor(
            "other",
            ABI_VERSION,
            AgentDescriptor {
                id: "demo".to_string(),
                ..base.clone()
            },
        )
        .expect_err("id mismatch should fail");
        assert_eq!(
            wrong_id.to_string(),
            "executor error: wasm component id `demo` does not match selected agent `other`"
        );

        let wrong_runner = validate_descriptor(
            "demo",
            ABI_VERSION,
            AgentDescriptor {
                runner_class: "native".to_string(),
                ..base.clone()
            },
        )
        .expect_err("runner mismatch should fail");
        assert_eq!(
            wrong_runner.to_string(),
            "executor error: unsupported wasm runner class `native`"
        );

        let wrong_abi = validate_descriptor(
            "demo",
            "v4",
            AgentDescriptor {
                abi_version: ABI_VERSION.to_string(),
                ..base.clone()
            },
        )
        .expect_err("selected ABI mismatch should fail");
        assert_eq!(
            wrong_abi.to_string(),
            format!(
                "executor error: wasm component ABI `{ABI_VERSION}` does not match selected agent ABI `v4`"
            )
        );

        let unsupported_abi = validate_descriptor(
            "demo",
            "v999",
            AgentDescriptor {
                abi_version: "v999".to_string(),
                ..base
            },
        )
        .expect_err("unsupported ABI should fail");
        assert_eq!(
            unsupported_abi.to_string(),
            "executor error: unsupported wasm agent ABI `v999`; expected `v3`"
        );
    }

    #[test]
    fn from_component_descriptor_preserves_export_metadata() {
        let descriptor = from_component_descriptor(super::odyssey_agent::AgentDescriptor {
            id: "math".to_string(),
            abi_version: ABI_VERSION.to_string(),
            runner_class: RUNNER_CLASS.to_string(),
        });

        assert_eq!(
            descriptor,
            AgentDescriptor {
                id: "math".to_string(),
                abi_version: ABI_VERSION.to_string(),
                runner_class: RUNNER_CLASS.to_string(),
            }
        );
    }

    #[test]
    fn render_output_handles_text_json_and_invalid_payloads() {
        assert_eq!(render_output("\"hello\"".to_string()), "hello");
        assert_eq!(
            render_output("{\"status\":\"ok\"}".to_string()),
            "{\n  \"status\": \"ok\"\n}"
        );
        assert_eq!(render_output("not-json".to_string()), "not-json");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn component_host_llm_chat_serializes_messages_tools_and_schema() {
        use super::odyssey_host::Host as _;

        let provider = RecordingLlmProvider {
            text: "hello".to_string(),
            reasoning: "think".to_string(),
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "Read".to_string(),
                    arguments: "{\"path\":\"README.md\"}".to_string(),
                },
            }],
            ..RecordingLlmProvider::default()
        };
        let provider = Arc::new(provider);
        let (mut host, _) = component_host(provider.clone(), Vec::new());

        let messages = vec![ChatMessage {
            role: ChatRole::User,
            message_type: MessageType::Text,
            content: "hello".to_string(),
        }];
        let tools = vec![HostToolDefinition {
            name: "Read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }),
        }];
        let output_schema = StructuredOutputFormat {
            name: "answer".to_string(),
            description: Some("Structured answer".to_string()),
            schema: Some(json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": { "type": "string" }
                }
            })),
            strict: Some(true),
        };

        let request = LlmChatRequest {
            messages_json: json_to_string(&messages).expect("serialize messages"),
            tools_json: Some(json_to_string(&tools).expect("serialize tools")),
            output_schema_json: Some(json_to_string(&output_schema).expect("serialize schema")),
        };

        let raw_response = host
            .llm_chat(json_to_string(&request).expect("serialize request"))
            .expect("llm chat should succeed");
        let response: LlmChatResponse = string_to_json(&raw_response).expect("decode response");
        assert_eq!(response.text, "hello");
        assert_eq!(response.reasoning, "think");
        assert_eq!(
            response.tool_calls_json,
            Some(
                json_to_string(&vec![ToolCall {
                    id: "call-1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "Read".to_string(),
                        arguments: "{\"path\":\"README.md\"}".to_string(),
                    },
                }])
                .expect("serialize tool calls")
            )
        );

        let calls = provider.calls.lock().expect("lock calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].messages_json,
            json_to_string(&messages).expect("serialize expected messages")
        );
        assert_eq!(calls[0].tool_names, vec!["Read".to_string()]);
        assert_eq!(calls[0].output_schema, Some(output_schema));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn component_host_llm_chat_defaults_optional_response_fields() {
        use super::odyssey_host::Host as _;

        let provider = Arc::new(RecordingLlmProvider::default());
        let (mut host, _) = component_host(provider.clone() as Arc<dyn LLMProvider>, Vec::new());

        let raw_response = host
            .llm_chat(
                json_to_string(&LlmChatRequest {
                    messages_json: "[]".to_string(),
                    tools_json: None,
                    output_schema_json: None,
                })
                .expect("serialize request"),
            )
            .expect("llm chat should succeed");
        let response: LlmChatResponse = string_to_json(&raw_response).expect("decode response");
        assert_eq!(
            response,
            LlmChatResponse {
                text: String::new(),
                reasoning: String::new(),
                tool_calls_json: None,
            }
        );

        let calls = provider.calls.lock().expect("lock calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_names, Vec::<String>::new());
        assert_eq!(calls[0].output_schema, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn component_host_llm_chat_rejects_invalid_payloads_and_provider_errors() {
        use super::odyssey_host::Host as _;

        let provider = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;
        let (mut host, _) = component_host(provider, Vec::new());

        let invalid_request = host
            .llm_chat("{".to_string())
            .expect_err("invalid request must fail");
        assert!(
            invalid_request.starts_with("invalid llm chat request:"),
            "unexpected error: {invalid_request}"
        );

        let invalid_messages = host
            .llm_chat(
                json_to_string(&LlmChatRequest {
                    messages_json: "{".to_string(),
                    tools_json: None,
                    output_schema_json: None,
                })
                .expect("serialize request"),
            )
            .expect_err("invalid messages must fail");
        assert!(
            invalid_messages.starts_with("invalid llm chat messages:"),
            "unexpected error: {invalid_messages}"
        );

        let invalid_tools = host
            .llm_chat(
                json_to_string(&LlmChatRequest {
                    messages_json: "[]".to_string(),
                    tools_json: Some("{".to_string()),
                    output_schema_json: None,
                })
                .expect("serialize request"),
            )
            .expect_err("invalid tool schema must fail");
        assert!(
            invalid_tools.starts_with("invalid llm tool schema payload:"),
            "unexpected error: {invalid_tools}"
        );

        let invalid_schema = host
            .llm_chat(
                json_to_string(&LlmChatRequest {
                    messages_json: "[]".to_string(),
                    tools_json: None,
                    output_schema_json: Some("{".to_string()),
                })
                .expect("serialize request"),
            )
            .expect_err("invalid output schema must fail");
        assert!(
            invalid_schema.starts_with("invalid llm output schema payload:"),
            "unexpected error: {invalid_schema}"
        );

        let failing_provider = Arc::new(RecordingLlmProvider {
            error: Some("provider down".to_string()),
            ..RecordingLlmProvider::default()
        }) as Arc<dyn LLMProvider>;
        let (mut host, _) = component_host(failing_provider, Vec::new());
        let provider_error = host
            .llm_chat(
                json_to_string(&LlmChatRequest {
                    messages_json: "[]".to_string(),
                    tools_json: None,
                    output_schema_json: None,
                })
                .expect("serialize request"),
            )
            .expect_err("provider error must surface");
        assert_eq!(provider_error, "Provider Error: provider down");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn component_host_tool_call_validates_requests_and_executes_tools() {
        use super::odyssey_host::Host as _;

        let llm = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;
        let tools = vec![Arc::new(EchoTool { name: "Read" }) as Arc<dyn ToolT>];
        let (mut host, _) = component_host(llm, tools);

        let invalid_request = host
            .tool_call("{".to_string())
            .expect_err("invalid request must fail");
        assert!(
            invalid_request.starts_with("invalid tool request:"),
            "unexpected error: {invalid_request}"
        );

        let invalid_args = host
            .tool_call(
                json_to_string(&HostToolCallRequest {
                    tool: "Read".to_string(),
                    arguments_json: "{".to_string(),
                })
                .expect("serialize request"),
            )
            .expect_err("invalid args must fail");
        assert!(
            invalid_args.starts_with("invalid tool arguments:"),
            "unexpected error: {invalid_args}"
        );

        let missing_tool = host
            .tool_call(
                json_to_string(&HostToolCallRequest {
                    tool: "Write".to_string(),
                    arguments_json: "{}".to_string(),
                })
                .expect("serialize request"),
            )
            .expect_err("missing tool must fail");
        assert_eq!(missing_tool, "tool `Write` is not available");

        let raw_response = host
            .tool_call(
                json_to_string(&HostToolCallRequest {
                    tool: "Read".to_string(),
                    arguments_json: "{\"path\":\"README.md\"}".to_string(),
                })
                .expect("serialize request"),
            )
            .expect("tool call should succeed");
        let response: HostToolCallResponse =
            string_to_json(&raw_response).expect("decode tool response");
        assert_eq!(
            string_to_json::<Value>(&response.result_json).expect("decode result payload"),
            json!({
                "echo": {
                    "path": "README.md"
                }
            })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn component_host_tool_call_surfaces_tool_runtime_errors() {
        use super::odyssey_host::Host as _;

        let llm = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;
        let tools = vec![Arc::new(FailingTool) as Arc<dyn ToolT>];
        let (mut host, _) = component_host(llm, tools);

        let error = host
            .tool_call(
                json_to_string(&HostToolCallRequest {
                    tool: "Write".to_string(),
                    arguments_json: "{\"path\":\"README.md\"}".to_string(),
                })
                .expect("serialize request"),
            )
            .expect_err("failing tool should surface its runtime error");

        assert_eq!(error, "Runtime Error tool failed");
    }

    #[test]
    fn component_host_emit_event_rejects_invalid_json_and_forwards_payloads() {
        use super::odyssey_host::Host as _;

        let llm = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;
        let (mut host, mut receiver) = component_host(llm, Vec::new());

        let invalid = host
            .emit_event("{".to_string())
            .expect_err("invalid event must fail");
        assert!(
            invalid.starts_with("invalid autoagents event:"),
            "unexpected error: {invalid}"
        );

        host.emit_event(
            json_to_string(&AutoAgentsEvent::ToolCallRequested {
                sub_id: Uuid::nil(),
                actor_id: Uuid::nil(),
                id: "call-1".to_string(),
                tool_name: "Read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            })
            .expect("serialize event"),
        )
        .expect("emit valid event");

        let event = receiver.try_recv().expect("receive mapped event");
        match event.payload {
            EventPayload::ToolCallStarted {
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(tool_name, "Read");
                assert_eq!(arguments, json!({ "path": "README.md" }));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_wasm_executor_propagates_component_failures_without_emitting_events() {
        let temp = tempdir().expect("tempdir");
        let missing_path = temp.path().join("missing.wasm");
        let llm = Arc::new(RecordingLlmProvider::default()) as Arc<dyn LLMProvider>;
        let (sender, mut receiver) = broadcast::channel(4);

        let error = run_wasm_executor(WasmExecutorRun {
            module_path: missing_path.clone(),
            agent_id: "demo".to_string(),
            abi_version: ABI_VERSION.to_string(),
            llm,
            tools: Vec::new(),
            session_id: Uuid::nil(),
            turn_id: Uuid::from_u128(42),
            sender,
            turn_context: TurnContext::default(),
            request: RunRequest {
                session_id: Uuid::nil().to_string(),
                turn_id: Uuid::from_u128(42).to_string(),
                prompt: "hello".to_string(),
                system_prompt: None,
                history_json: None,
                metadata_json: None,
                host_tools: Vec::new(),
            },
        })
        .await
        .expect_err("missing component should fail");

        assert_eq!(
            error.to_string(),
            format!(
                "io error at {}: No such file or directory (os error 2)",
                missing_path.display()
            )
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_wasm_executor_executes_workspace_component_and_emits_completion() {
        let provider = Arc::new(RecordingLlmProvider {
            text: "hello from wasm".to_string(),
            ..RecordingLlmProvider::default()
        });
        let module_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundles/odyssey-agent/agents/odyssey-agent/module.wasm");
        let (sender, mut receiver) = broadcast::channel(8);

        let message = run_wasm_executor(WasmExecutorRun {
            module_path,
            agent_id: "odyssey-agent".to_string(),
            abi_version: ABI_VERSION.to_string(),
            llm: provider.clone() as Arc<dyn LLMProvider>,
            tools: vec![Arc::new(EchoTool { name: "Read" }) as Arc<dyn ToolT>],
            session_id: Uuid::nil(),
            turn_id: Uuid::from_u128(7),
            sender,
            turn_context: TurnContext::default(),
            request: RunRequest {
                session_id: Uuid::nil().to_string(),
                turn_id: Uuid::from_u128(7).to_string(),
                prompt: "Say hello".to_string(),
                system_prompt: Some("Stay concise".to_string()),
                history_json: None,
                metadata_json: None,
                host_tools: vec![HostToolSpec {
                    name: "Read".to_string(),
                    description: "Read a file".to_string(),
                    args_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        }
                    }),
                    output_schema: None,
                }],
            },
        })
        .await
        .expect("wasm executor should succeed");

        assert_eq!(message, "hello from wasm");
        assert_eq!(provider.calls.lock().expect("lock calls").len(), 1);

        for _ in 0..8 {
            let event = receiver.recv().await.expect("executor event");
            if matches!(
                event.payload,
                EventPayload::TurnCompleted { ref message, .. } if message == "hello from wasm"
            ) {
                return;
            }
        }

        panic!("expected turn completed event");
    }
}
