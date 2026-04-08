//! Stable Odyssey WASM agent SDK for Rust-authored Odyssey agents.
//!
//! ```rust,ignore
//! use autoagents_derive::{AgentHooks, agent};
//!
//! #[agent(
//!     name = "math_agent",
//!     description = "You are a Math agent",
//!     tools = [],
//! )]
//! #[derive(Default, Clone, AgentHooks)]
//! pub struct MathAgent {}
//!
//! fn app() -> odyssey_rs_agent_sdk::OdysseyAgentApp<MathAgent, odyssey_rs_agent_sdk::ReactExecutor> {
//!     odyssey_rs_agent_sdk::OdysseyAgentApp::react(MathAgent::default())
//! }
//!
//! odyssey_rs_agent_sdk::export_odyssey_agent!("math_agent", app());
//! ```

extern crate self as odyssey_rs_agent_sdk;

use std::fmt;
use std::sync::Arc;

use autoagents_core::agent::error::RunnableAgentError;
use autoagents_core::agent::memory::MemoryProvider;
use autoagents_core::agent::{AgentDeriveT, AgentExecutor, AgentHooks, DirectAgentHandle};
use autoagents_core::tool::{ToolInputT, ToolT};
use autoagents_llm::LLMProvider;
use autoagents_protocol::Event;
use futures_util::StreamExt;

#[cfg(target_arch = "wasm32")]
use autoagents_llm::ToolCall;
#[cfg(target_arch = "wasm32")]
use autoagents_llm::chat::ChatResponse;

#[cfg(target_arch = "wasm32")]
wit_bindgen::generate!({
    path: "../odyssey-rs-agent-abi/wit",
    world: "odyssey-agent-world",
    default_bindings_module: "odyssey_rs_agent_sdk",
    pub_export_macro: true,
    type_section_suffix: "odyssey-rs-agent-sdk",
});

mod app;
mod host_tools;

pub use app::{AutoAgentApp, BasicExecutor, OdysseyAgentApp, ReactExecutor, RunnableApp, run_app};
pub use host_tools::{
    BashArgs, DynamicHostTool, EditArgs, GlobArgs, GrepArgs, HostToolCatalog, LsArgs, ReadArgs,
    SkillArgs, TypedHostTool, WriteArgs,
};
pub use odyssey_rs_agent_abi;
pub use odyssey_rs_agent_abi::{HostToolSpec, RunRequest, RunResponse};
#[cfg(target_arch = "wasm32")]
use odyssey_rs_agent_abi::{json_to_string, string_to_json};
pub use wit_bindgen;

pub type AgentResult<T> = Result<T, AgentSdkError>;

#[derive(Debug, thiserror::Error)]
pub enum AgentSdkError {
    #[error("invalid request payload: {0}")]
    InvalidRequest(String),
    #[error("invalid response payload: {0}")]
    InvalidResponse(String),
    #[error("host tool `{0}` is not available for this run")]
    HostToolUnavailable(String),
    #[error("host llm call failed: {0}")]
    HostLlm(String),
    #[error("host tool call failed: {0}")]
    HostTool(String),
    #[error("host event emission failed: {0}")]
    HostEvent(String),
    #[error("duplicate tool name `{0}` in agent tool set")]
    DuplicateTool(String),
    #[error("agent execution failed: {0}")]
    Execution(String),
    #[error("unsupported outside wasm agent execution")]
    UnsupportedHostBuild,
}

#[cfg(target_arch = "wasm32")]
pub mod host {
    use super::*;
    use crate::odyssey::agent::odyssey_host;
    use async_trait::async_trait;
    use autoagents_core::tool::{ToolCallError, ToolRuntime};
    use autoagents_llm::chat::{ChatMessage, ChatProvider, StructuredOutputFormat, Tool};
    use autoagents_llm::completion::{CompletionProvider, CompletionRequest, CompletionResponse};
    use autoagents_llm::embedding::EmbeddingProvider;
    use autoagents_llm::error::LLMError;
    use autoagents_llm::models::{ModelListRequest, ModelListResponse, ModelsProvider};
    use odyssey_rs_agent_abi::{
        HostToolCallRequest, HostToolCallResponse, HostToolDefinition, LlmChatRequest,
        LlmChatResponse, json_to_string, string_to_json,
    };
    use serde_json::Value;
    use std::fmt;
    use std::marker::PhantomData;

    #[derive(Clone, Debug, Default)]
    pub struct HostLlmProvider;

    #[derive(Clone)]
    pub struct HostTool<Args> {
        name: String,
        description: String,
        _marker: PhantomData<fn(Args)>,
    }

    impl<Args> fmt::Debug for HostTool<Args> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HostTool")
                .field("name", &self.name)
                .field("description", &self.description)
                .finish()
        }
    }

    impl HostLlmProvider {
        fn chat_impl(
            &self,
            messages: &[ChatMessage],
            tools: Option<&[Tool]>,
            output_schema: Option<StructuredOutputFormat>,
        ) -> Result<SimpleChatResponse, LLMError> {
            let request = LlmChatRequest {
                messages_json: json_to_string(&messages)
                    .map_err(|err| LLMError::Generic(err.to_string()))?,
                tools_json: tools
                    .map(|tools| {
                        tools
                            .iter()
                            .map(|tool| HostToolDefinition {
                                name: tool.function.name.clone(),
                                description: tool.function.description.clone(),
                                parameters: tool.function.parameters.clone(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .map(|tools| json_to_string(&tools))
                    .transpose()
                    .map_err(|err| LLMError::Generic(err.to_string()))?,
                output_schema_json: output_schema
                    .map(|schema| json_to_string(&schema))
                    .transpose()
                    .map_err(|err| LLMError::Generic(err.to_string()))?,
            };

            let raw = odyssey_host::llm_chat(
                &json_to_string(&request).map_err(|err| LLMError::Generic(err.to_string()))?,
            )
            .map_err(LLMError::ProviderError)?;

            let response: LlmChatResponse =
                string_to_json(&raw).map_err(|err| LLMError::Generic(err.to_string()))?;
            let tool_calls = response
                .tool_calls_json
                .as_deref()
                .map(string_to_json)
                .transpose()
                .map_err(|err| LLMError::Generic(err.to_string()))?
                .unwrap_or_default();

            Ok(SimpleChatResponse {
                text: response.text,
                reasoning: response.reasoning,
                tool_calls,
            })
        }
    }

    #[async_trait]
    impl ChatProvider for HostLlmProvider {
        async fn chat_with_tools(
            &self,
            messages: &[ChatMessage],
            tools: Option<&[Tool]>,
            json_schema: Option<StructuredOutputFormat>,
        ) -> Result<Box<dyn ChatResponse>, LLMError> {
            self.chat_impl(messages, tools, json_schema)
                .map(SimpleChatResponse::into_boxed)
        }
    }

    #[async_trait]
    impl CompletionProvider for HostLlmProvider {
        async fn complete(
            &self,
            req: &CompletionRequest,
            json_schema: Option<StructuredOutputFormat>,
        ) -> Result<CompletionResponse, LLMError> {
            let response = self.chat_impl(
                &[ChatMessage {
                    role: autoagents_llm::chat::ChatRole::User,
                    message_type: autoagents_llm::chat::MessageType::Text,
                    content: req.prompt.clone(),
                }],
                None,
                json_schema,
            )?;
            Ok(CompletionResponse {
                text: response.text,
            })
        }
    }

    #[async_trait]
    impl EmbeddingProvider for HostLlmProvider {
        async fn embed(&self, _input: Vec<String>) -> Result<Vec<Vec<f32>>, LLMError> {
            Err(LLMError::ProviderError(
                "embedding is not exposed through the Odyssey wasm host binding".to_string(),
            ))
        }
    }

    #[async_trait]
    impl ModelsProvider for HostLlmProvider {
        async fn list_models(
            &self,
            _request: Option<&ModelListRequest>,
        ) -> Result<Box<dyn ModelListResponse>, LLMError> {
            Err(LLMError::ProviderError(
                "list-models is not exposed through the Odyssey wasm host binding".to_string(),
            ))
        }
    }

    impl LLMProvider for HostLlmProvider {}

    pub fn llm_provider() -> Arc<dyn LLMProvider> {
        Arc::new(HostLlmProvider)
    }

    pub fn tool<Args>(name: impl Into<String>, description: impl Into<String>) -> Arc<dyn ToolT>
    where
        Args: ToolInputT + Send + Sync + 'static,
    {
        Arc::new(HostTool::<Args> {
            name: name.into(),
            description: description.into(),
            _marker: PhantomData,
        })
    }

    pub async fn call_tool(name: &str, args: Value) -> AgentResult<Value> {
        let request = HostToolCallRequest {
            tool: name.to_string(),
            arguments_json: json_to_string(&args)
                .map_err(|err| AgentSdkError::InvalidRequest(err.to_string()))?,
        };
        let raw = odyssey_host::tool_call(
            &json_to_string(&request)
                .map_err(|err| AgentSdkError::InvalidRequest(err.to_string()))?,
        )
        .map_err(AgentSdkError::HostTool)?;
        let response: HostToolCallResponse =
            string_to_json(&raw).map_err(|err| AgentSdkError::InvalidResponse(err.to_string()))?;
        string_to_json(&response.result_json)
            .map_err(|err| AgentSdkError::InvalidResponse(err.to_string()))
    }

    pub fn emit_event(event: &Event) -> AgentResult<()> {
        let payload =
            json_to_string(event).map_err(|err| AgentSdkError::HostEvent(err.to_string()))?;
        odyssey_host::emit_event(&payload).map_err(AgentSdkError::HostEvent)
    }

    pub fn preload_memory(
        memory: &mut dyn MemoryProvider,
        request: &RunRequest,
    ) -> AgentResult<()> {
        let history = request
            .history_json
            .as_deref()
            .map(string_to_json::<Vec<ChatMessage>>)
            .transpose()
            .map_err(|err| AgentSdkError::InvalidRequest(err.to_string()))?
            .unwrap_or_default();
        let _ = memory.preload(history);
        Ok(())
    }

    #[async_trait]
    impl<Args> ToolRuntime for HostTool<Args>
    where
        Args: ToolInputT + Send + Sync + 'static,
    {
        async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
            call_tool(&self.name, args)
                .await
                .map_err(|err| ToolCallError::RuntimeError(Box::new(err)))
        }
    }

    impl<Args> ToolT for HostTool<Args>
    where
        Args: ToolInputT + Send + Sync + 'static,
    {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn args_schema(&self) -> Value {
            serde_json::from_str(Args::io_schema()).unwrap_or(Value::Null)
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub mod host {
    use super::*;

    pub fn llm_provider() -> Arc<dyn LLMProvider> {
        panic!("odyssey wasm host bindings are only available for wasm32 agents")
    }

    pub fn tool<Args>(_name: impl Into<String>, _description: impl Into<String>) -> Arc<dyn ToolT>
    where
        Args: ToolInputT + Send + Sync + 'static,
    {
        panic!("odyssey wasm host bindings are only available for wasm32 agents")
    }

    pub async fn call_tool(
        _name: &str,
        _args: serde_json::Value,
    ) -> AgentResult<serde_json::Value> {
        Err(AgentSdkError::UnsupportedHostBuild)
    }

    pub fn emit_event(_event: &Event) -> AgentResult<()> {
        Err(AgentSdkError::UnsupportedHostBuild)
    }

    pub fn preload_memory(
        _memory: &mut dyn MemoryProvider,
        _request: &RunRequest,
    ) -> AgentResult<()> {
        Err(AgentSdkError::UnsupportedHostBuild)
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug)]
struct SimpleChatResponse {
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
}

#[cfg(target_arch = "wasm32")]
impl SimpleChatResponse {
    fn into_boxed(self) -> Box<dyn ChatResponse> {
        Box::new(self)
    }
}

#[cfg(target_arch = "wasm32")]
impl fmt::Display for SimpleChatResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(target_arch = "wasm32")]
impl ChatResponse for SimpleChatResponse {
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

pub fn task_from_request(request: &RunRequest) -> autoagents_protocol::Task {
    let mut task = autoagents_protocol::Task::new(request.prompt.clone());
    if let Some(system_prompt) = &request.system_prompt {
        task = task.with_system_prompt(system_prompt.clone());
    }
    task
}

pub async fn run_handle<T>(
    handle: DirectAgentHandle<T>,
    request: &RunRequest,
) -> AgentResult<RunResponse>
where
    T: AgentDeriveT + AgentExecutor + AgentHooks + Send + Sync + 'static,
    <T as AgentDeriveT>::Output: Into<String> + From<<T as AgentExecutor>::Output>,
    <T as AgentExecutor>::Error: Into<RunnableAgentError>,
{
    let task = task_from_request(request);
    let mut rx = handle.rx;
    let agent = handle.agent;

    let emit_future = async move {
        let mut first_error: Option<AgentSdkError> = None;
        while let Some(event) = rx.next().await {
            if let Err(err) = host::emit_event(&event)
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    };

    let run_future = async move {
        let result = agent
            .run(task)
            .await
            .map_err(|err| AgentSdkError::Execution(err.to_string()))?;
        Ok::<RunResponse, AgentSdkError>(RunResponse::text(Into::<String>::into(result)))
    };

    let (run_result, emit_result) = futures_util::future::join(run_future, emit_future).await;
    let response = run_result?;
    emit_result?;
    Ok(response)
}

/// Build a direct AutoAgents handle for the incoming request and run it to completion.
///
/// Rust-authored agent crates normally pair this with [`export_odyssey_agent!`] by exposing a single
/// `build_handle(&RunRequest)` async function and letting the macro wire the runner.
pub async fn build_and_run<T, E, F, Fut>(
    request: &RunRequest,
    builder: F,
) -> AgentResult<RunResponse>
where
    T: AgentDeriveT + AgentExecutor + AgentHooks + Send + Sync + 'static,
    <T as AgentDeriveT>::Output: Into<String> + From<<T as AgentExecutor>::Output>,
    <T as AgentExecutor>::Error: Into<RunnableAgentError>,
    E: fmt::Display,
    F: FnOnce(&RunRequest) -> Fut,
    Fut: std::future::Future<Output = Result<DirectAgentHandle<T>, E>>,
{
    let handle = builder(request)
        .await
        .map_err(|err| AgentSdkError::Execution(err.to_string()))?;
    run_handle(handle, request).await
}

#[cfg(target_arch = "wasm32")]
pub fn decode_run_request(raw: &str) -> AgentResult<RunRequest> {
    string_to_json(raw).map_err(|err| AgentSdkError::InvalidRequest(err.to_string()))
}

#[cfg(target_arch = "wasm32")]
pub fn encode_run_response(response: &RunResponse) -> AgentResult<String> {
    json_to_string(response).map_err(|err| AgentSdkError::InvalidResponse(err.to_string()))
}

#[doc(hidden)]
pub fn block_on_future<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    futures::executor::block_on(future)
}

#[macro_export]
macro_rules! export_odyssey_agent {
    ($id:literal, $app:expr) => {
        #[cfg(target_arch = "wasm32")]
        mod __odyssey_agent_export {
            use super::*;
            use $crate::exports::odyssey::agent::odyssey_agent::{
                AgentDescriptor as ComponentAgentDescriptor, Guest,
            };

            struct Component;

            impl Guest for Component {
                fn describe() -> ComponentAgentDescriptor {
                    ComponentAgentDescriptor {
                        id: $id.to_string(),
                        abi_version: $crate::odyssey_rs_agent_abi::ABI_VERSION.to_string(),
                        runner_class: $crate::odyssey_rs_agent_abi::RUNNER_CLASS.to_string(),
                    }
                }

                fn run(request_json: String) -> Result<String, String> {
                    let request = $crate::decode_run_request(&request_json)?;
                    let response = $crate::block_on_future($crate::run_app(($app), &request))
                        .map_err(|err| err.to_string())?;
                    $crate::encode_run_response(&response).map_err(Into::into)
                }
            }

            $crate::export!(Component);
        }
    };
}

#[macro_export]
macro_rules! export_agent {
    ($id:literal, $app:expr) => {
        $crate::export_odyssey_agent!($id, $app);
    };
    ($id:literal, $handler:expr, $wit_path:expr) => {
        #[cfg(target_arch = "wasm32")]
        mod __odyssey_agent_export {
            use super::*;
            $crate::wit_bindgen::generate!({
                path: $wit_path,
                world: "odyssey-agent-world",
            });

            use self::exports::odyssey::agent::odyssey_agent::{
                AgentDescriptor as ComponentAgentDescriptor, Guest,
            };

            struct Component;

            impl Guest for Component {
                fn describe() -> ComponentAgentDescriptor {
                    ComponentAgentDescriptor {
                        id: $id.to_string(),
                        abi_version: $crate::odyssey_rs_agent_abi::ABI_VERSION.to_string(),
                        runner_class: $crate::odyssey_rs_agent_abi::RUNNER_CLASS.to_string(),
                    }
                }

                fn run(request_json: String) -> Result<String, String> {
                    let request = $crate::decode_run_request(&request_json)?;
                    let response = $crate::block_on_future(($handler)(&request))
                        .map_err(|err| err.to_string())?;
                    $crate::encode_run_response(&response).map_err(Into::into)
                }
            }

            $crate::export!(Component);
        }
    };
}

impl From<AgentSdkError> for String {
    fn from(value: AgentSdkError) -> Self {
        value.to_string()
    }
}
