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
