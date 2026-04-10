use crate::RuntimeError;
use autoagents_core::agent::prebuilt::executor::{CodeActAgent, ReActAgent};
use autoagents_core::agent::{
    AgentBuilder, AgentDeriveT, AgentHooks, Context, DirectAgent, HookOutcome, task::Task,
};
use autoagents_core::tool::{ToolCallResult, ToolT, shared_tools_to_boxes};
use autoagents_llm::LLMProvider;
use autoagents_llm::ToolCall;
use chrono::Utc;
use futures_util::StreamExt;
use log::info;
use odyssey_rs_protocol::{AutoAgentsEvent, AutoAgentsStreamChunk};
use odyssey_rs_protocol::{EventMsg, EventPayload, ExecStream, TurnContext};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

pub struct ExecutorRun {
    pub executor_id: String,
    pub llm: Arc<dyn LLMProvider>,
    pub system_prompt: String,
    pub task: Task,
    pub memory: Option<Box<dyn autoagents_core::agent::memory::MemoryProvider>>,
    pub tools: Vec<Arc<dyn ToolT>>,
    pub session_id: Uuid,
    pub turn_id: Uuid,
    pub sender: broadcast::Sender<EventMsg>,
    pub turn_context: TurnContext,
}

pub async fn run_executor(run: ExecutorRun) -> Result<String, RuntimeError> {
    match run.executor_id.as_str() {
        "react" | "react/v1" => run_react(run).await,
        "codeact" | "codeact/v1" => run_codeact(run).await,
        other => Err(RuntimeError::Unsupported(format!(
            "unsupported prebuilt executor: {other}"
        ))),
    }
}

async fn run_react(run: ExecutorRun) -> Result<String, RuntimeError> {
    info!("Running ReAct Executor");
    let agent = ReActAgent::new(OdysseyAgent::new(run.system_prompt.clone(), run.tools));
    let mut builder = AgentBuilder::<ReActAgent<OdysseyAgent>, DirectAgent>::new(agent)
        .llm(run.llm)
        .stream(true);
    if let Some(memory) = run.memory {
        builder = builder.memory(memory);
    }
    let mut handle = builder
        .build()
        .await
        .map_err(|err| RuntimeError::Executor(err.to_string()))?;
    info!("Built Agent instance");
    let events = handle.subscribe_events();
    let event_task = tokio::spawn(forward_autoagents_events(
        events,
        run.sender.clone(),
        run.session_id,
        run.turn_id,
        run.turn_context.clone(),
    ));
    let task = run.task.with_system_prompt(run.system_prompt);
    info!("Running agent with streaming");
    let stream = match handle.agent.run_stream(task).await {
        Ok(stream) => stream,
        Err(err) => {
            event_task.abort();
            return Err(RuntimeError::Executor(err.to_string()));
        }
    };
    let mut response = String::default();
    {
        tokio::pin!(stream);
        while let Some(output) = stream.next().await {
            match output {
                Ok(output) => response = output,
                Err(err) => {
                    event_task.abort();
                    return Err(RuntimeError::Executor(err.to_string()));
                }
            }
        }
    }
    drop(handle);
    match event_task.await {
        Ok(()) => {}
        Err(err) => {
            return Err(RuntimeError::Executor(format!(
                "failed to forward autoagents events: {err}"
            )));
        }
    }
    emit(
        &run.sender,
        run.session_id,
        EventPayload::TurnCompleted {
            turn_id: run.turn_id,
            message: response.clone(),
        },
    );
    Ok(response)
}

async fn run_codeact(run: ExecutorRun) -> Result<String, RuntimeError> {
    info!("Running CodeAct Executor");
    let agent: CodeActAgent<OdysseyAgent> =
        CodeActAgent::new(OdysseyAgent::new(run.system_prompt.clone(), run.tools));
    let mut builder: AgentBuilder<CodeActAgent<OdysseyAgent>, DirectAgent> =
        AgentBuilder::new(agent).llm(run.llm).stream(true);
    if let Some(memory) = run.memory {
        builder = builder.memory(memory);
    }
    let mut handle = builder
        .build()
        .await
        .map_err(|err| RuntimeError::Executor(err.to_string()))?;
    info!("Built Agent instance");
    let events = handle.subscribe_events();
    let event_task = tokio::spawn(forward_autoagents_events(
        events,
        run.sender.clone(),
        run.session_id,
        run.turn_id,
        run.turn_context.clone(),
    ));
    let task = run.task.with_system_prompt(run.system_prompt);
    info!("Running agent with streaming");
    let stream = match handle.agent.run_stream(task).await {
        Ok(stream) => stream,
        Err(err) => {
            event_task.abort();
            return Err(RuntimeError::Executor(err.to_string()));
        }
    };
    let mut response = String::default();
    {
        tokio::pin!(stream);
        while let Some(output) = stream.next().await {
            match output {
                Ok(output) => response = output,
                Err(err) => {
                    event_task.abort();
                    return Err(RuntimeError::Executor(err.to_string()));
                }
            }
        }
    }
    drop(handle);
    match event_task.await {
        Ok(()) => {}
        Err(err) => {
            return Err(RuntimeError::Executor(format!(
                "failed to forward autoagents events: {err}"
            )));
        }
    }
    emit(
        &run.sender,
        run.session_id,
        EventPayload::TurnCompleted {
            turn_id: run.turn_id,
            message: response.clone(),
        },
    );
    Ok(response)
}

async fn forward_autoagents_events(
    mut events: autoagents_core::utils::BoxEventStream<AutoAgentsEvent>,
    sender: broadcast::Sender<EventMsg>,
    session_id: Uuid,
    turn_id: Uuid,
    turn_context: TurnContext,
) {
    let mut bridge = AutoagentsEventBridge::new(turn_id, turn_context);
    info!("Forwaring AutoAgents Events");
    while let Some(event) = events.next().await {
        let mapped = bridge.map_event(event);
        for payload in mapped.payloads {
            emit(&sender, session_id, payload);
        }
        if mapped.terminal {
            break;
        }
    }
}

pub(crate) struct MappedEvent {
    pub(crate) payloads: Vec<EventPayload>,
    terminal: bool,
}

pub(crate) struct AutoagentsEventBridge {
    turn_id: Uuid,
    turn_context: TurnContext,
    reasoning_open: bool,
    execution_ids: HashMap<String, Uuid>,
    tool_index_ids: HashMap<usize, String>,
    tool_call_ids: HashMap<String, Uuid>,
    started_tool_calls: HashSet<Uuid>,
}

impl AutoagentsEventBridge {
    pub(crate) fn new(turn_id: Uuid, turn_context: TurnContext) -> Self {
        Self {
            turn_id,
            turn_context,
            reasoning_open: false,
            execution_ids: HashMap::new(),
            tool_index_ids: HashMap::new(),
            tool_call_ids: HashMap::new(),
            started_tool_calls: HashSet::new(),
        }
    }

    pub(crate) fn map_event(&mut self, event: AutoAgentsEvent) -> MappedEvent {
        match event {
            AutoAgentsEvent::TurnStarted { .. } => MappedEvent {
                payloads: vec![EventPayload::TurnStarted {
                    turn_id: self.turn_id,
                    context: self.turn_context.clone(),
                }],
                terminal: false,
            },
            AutoAgentsEvent::StreamChunk { chunk, .. } => MappedEvent {
                payloads: self.map_stream_chunk(chunk),
                terminal: false,
            },
            AutoAgentsEvent::StreamToolCall { tool_call, .. } => MappedEvent {
                payloads: self.map_stream_tool_call(tool_call),
                terminal: false,
            },
            AutoAgentsEvent::ToolCallRequested {
                id,
                tool_name,
                arguments,
                ..
            } => MappedEvent {
                payloads: self.map_tool_call_requested(id, tool_name, arguments),
                terminal: false,
            },
            AutoAgentsEvent::ToolCallCompleted { id, result, .. } => MappedEvent {
                payloads: self.map_tool_call_finished(id, result, true),
                terminal: false,
            },
            AutoAgentsEvent::ToolCallFailed { id, error, .. } => MappedEvent {
                payloads: self.map_tool_call_finished(id, json!({ "error": error }), false),
                terminal: false,
            },
            AutoAgentsEvent::CodeExecutionStarted {
                execution_id,
                language,
                ..
            } => MappedEvent {
                payloads: self.map_code_execution_started(execution_id, language),
                terminal: false,
            },
            AutoAgentsEvent::CodeExecutionConsole {
                execution_id,
                message,
                ..
            } => MappedEvent {
                payloads: self.map_code_execution_console(execution_id, message),
                terminal: false,
            },
            AutoAgentsEvent::CodeExecutionCompleted {
                execution_id,
                result,
                ..
            } => MappedEvent {
                payloads: self.map_code_execution_finished(
                    execution_id,
                    Some(stringify_exec_output(result)),
                    0,
                    ExecStream::Stdout,
                ),
                terminal: false,
            },
            AutoAgentsEvent::CodeExecutionFailed {
                execution_id,
                error,
                ..
            } => MappedEvent {
                payloads: self.map_code_execution_finished(
                    execution_id,
                    Some(error),
                    1,
                    ExecStream::Stderr,
                ),
                terminal: false,
            },
            AutoAgentsEvent::TaskError { error, .. } => {
                let mut payloads = self.close_reasoning_section();
                payloads.push(EventPayload::Error {
                    turn_id: Some(self.turn_id),
                    message: error,
                });
                MappedEvent {
                    payloads,
                    terminal: true,
                }
            }
            AutoAgentsEvent::TaskComplete { .. } | AutoAgentsEvent::StreamComplete { .. } => {
                MappedEvent {
                    payloads: self.close_reasoning_section(),
                    terminal: true,
                }
            }
            AutoAgentsEvent::TurnCompleted { .. }
            | AutoAgentsEvent::NewTask { .. }
            | AutoAgentsEvent::PublishMessage { .. }
            | AutoAgentsEvent::SendMessage { .. }
            | AutoAgentsEvent::TaskStarted { .. } => MappedEvent {
                payloads: Vec::new(),
                terminal: false,
            },
        }
    }

    fn map_stream_chunk(&mut self, chunk: AutoAgentsStreamChunk) -> Vec<EventPayload> {
        match chunk {
            AutoAgentsStreamChunk::Text(delta) => {
                let mut payloads = self.close_reasoning_section();
                payloads.push(EventPayload::AgentMessageDelta {
                    turn_id: self.turn_id,
                    delta,
                });
                payloads
            }
            AutoAgentsStreamChunk::ReasoningContent(delta) => {
                self.reasoning_open = true;
                vec![EventPayload::ReasoningDelta {
                    turn_id: self.turn_id,
                    delta,
                }]
            }
            AutoAgentsStreamChunk::ToolUseStart { index, id, .. } => {
                self.tool_index_ids.insert(index, id.clone());
                self.close_reasoning_section()
            }
            AutoAgentsStreamChunk::ToolUseInputDelta {
                index,
                partial_json,
            } => {
                let mut payloads = self.close_reasoning_section();
                let raw_tool_call_id = self
                    .tool_index_ids
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| index.to_string());
                let tool_call_id = self.tool_call_id(&raw_tool_call_id);
                payloads.push(EventPayload::ToolCallDelta {
                    turn_id: self.turn_id,
                    tool_call_id,
                    delta: json!({ "partial_json": partial_json }),
                });
                payloads
            }
            AutoAgentsStreamChunk::ToolUseComplete { tool_call, .. } => self
                .map_tool_call_requested(
                    tool_call.id,
                    tool_call.function.name,
                    tool_call.function.arguments,
                ),
            AutoAgentsStreamChunk::Done { .. } | AutoAgentsStreamChunk::Usage(_) => {
                self.close_reasoning_section()
            }
        }
    }

    fn map_stream_tool_call(&mut self, tool_call: Value) -> Vec<EventPayload> {
        let Some(tool_call_id) = tool_call.get("id").and_then(Value::as_str) else {
            return Vec::new();
        };
        let tool_name = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = tool_call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .map(parse_json_value)
            .unwrap_or_else(|| tool_call.clone());
        self.map_tool_call_started(tool_call_id.to_string(), tool_name, arguments)
    }

    fn map_tool_call_requested(
        &mut self,
        raw_tool_call_id: String,
        tool_name: String,
        arguments: String,
    ) -> Vec<EventPayload> {
        let arguments = parse_json_value(&arguments);
        self.map_tool_call_started(raw_tool_call_id, tool_name, arguments)
    }

    fn map_tool_call_started(
        &mut self,
        raw_tool_call_id: String,
        tool_name: String,
        arguments: Value,
    ) -> Vec<EventPayload> {
        let mut payloads = self.close_reasoning_section();
        let tool_call_id = self.tool_call_id(&raw_tool_call_id);
        if self.started_tool_calls.insert(tool_call_id) {
            payloads.push(EventPayload::ToolCallStarted {
                turn_id: self.turn_id,
                tool_call_id,
                tool_name,
                arguments,
            });
        } else {
            payloads.push(EventPayload::ToolCallDelta {
                turn_id: self.turn_id,
                tool_call_id,
                delta: arguments,
            });
        }
        payloads
    }

    fn map_tool_call_finished(
        &mut self,
        raw_tool_call_id: String,
        result: Value,
        success: bool,
    ) -> Vec<EventPayload> {
        let mut payloads = self.close_reasoning_section();
        let tool_call_id = self.tool_call_id(&raw_tool_call_id);
        payloads.push(EventPayload::ToolCallFinished {
            turn_id: self.turn_id,
            tool_call_id,
            result,
            success,
        });
        payloads
    }

    fn map_code_execution_started(
        &mut self,
        raw_execution_id: String,
        language: String,
    ) -> Vec<EventPayload> {
        let mut payloads = self.close_reasoning_section();
        let exec_id = self.execution_id(&raw_execution_id);
        payloads.push(EventPayload::ExecCommandBegin {
            turn_id: self.turn_id,
            exec_id,
            command: vec!["codeact".to_string(), language],
            cwd: self.turn_context.cwd.clone(),
        });
        payloads
    }

    fn map_code_execution_console(
        &mut self,
        raw_execution_id: String,
        message: String,
    ) -> Vec<EventPayload> {
        let mut payloads = self.close_reasoning_section();
        payloads.push(EventPayload::ExecCommandOutputDelta {
            turn_id: self.turn_id,
            exec_id: self.execution_id(&raw_execution_id),
            stream: ExecStream::Stdout,
            delta: message,
        });
        payloads
    }

    fn map_code_execution_finished(
        &mut self,
        raw_execution_id: String,
        output: Option<String>,
        exit_code: i32,
        stream: ExecStream,
    ) -> Vec<EventPayload> {
        let mut payloads = self.close_reasoning_section();
        let exec_id = self.execution_id(&raw_execution_id);
        if let Some(delta) = output.filter(|delta| !delta.is_empty()) {
            payloads.push(EventPayload::ExecCommandOutputDelta {
                turn_id: self.turn_id,
                exec_id,
                stream,
                delta,
            });
        }
        payloads.push(EventPayload::ExecCommandEnd {
            turn_id: self.turn_id,
            exec_id,
            exit_code,
        });
        self.execution_ids.remove(&raw_execution_id);
        payloads
    }

    fn close_reasoning_section(&mut self) -> Vec<EventPayload> {
        if self.reasoning_open {
            self.reasoning_open = false;
            return vec![EventPayload::ReasoningSectionBreak {
                turn_id: self.turn_id,
            }];
        }
        Vec::new()
    }

    fn tool_call_id(&mut self, raw_tool_call_id: &str) -> Uuid {
        *self
            .tool_call_ids
            .entry(raw_tool_call_id.to_string())
            .or_insert_with(Uuid::new_v4)
    }

    fn execution_id(&mut self, raw_execution_id: &str) -> Uuid {
        *self
            .execution_ids
            .entry(raw_execution_id.to_string())
            .or_insert_with(Uuid::new_v4)
    }
}

#[derive(Clone, Debug, Default)]
struct OdysseyAgent {
    system_prompt: String,
    tools: Vec<Arc<dyn ToolT>>,
}

impl OdysseyAgent {
    fn new(system_prompt: String, tools: Vec<Arc<dyn ToolT>>) -> Self {
        Self {
            system_prompt,
            tools,
        }
    }
}

#[async_trait::async_trait]
impl AgentDeriveT for OdysseyAgent {
    type Output = String;

    fn description(&self) -> &str {
        &self.system_prompt
    }

    fn output_schema(&self) -> Option<Value> {
        None
    }

    fn name(&self) -> &str {
        "odyssey-agent"
    }

    fn tools(&self) -> Vec<Box<dyn ToolT>> {
        shared_tools_to_boxes(&self.tools)
    }
}

#[async_trait::async_trait]
impl AgentHooks for OdysseyAgent {
    async fn on_run_start(&self, _task: &Task, _ctx: &Context) -> HookOutcome {
        HookOutcome::Continue
    }

    async fn on_tool_call(&self, _tool_call: &ToolCall, _ctx: &Context) -> HookOutcome {
        HookOutcome::Continue
    }

    async fn on_tool_result(
        &self,
        _tool_call: &ToolCall,
        _result: &ToolCallResult,
        _ctx: &Context,
    ) {
    }

    async fn on_tool_error(&self, _tool_call: &ToolCall, _err: Value, _ctx: &Context) {}
}

fn parse_json_value(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

fn stringify_exec_output(value: Value) -> String {
    match value {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

pub fn emit(sender: &broadcast::Sender<EventMsg>, session_id: Uuid, payload: EventPayload) {
    let _ = sender.send(EventMsg {
        id: Uuid::new_v4(),
        session_id,
        created_at: Utc::now(),
        payload,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        AutoagentsEventBridge, ExecutorRun, OdysseyAgent, emit, forward_autoagents_events,
        parse_json_value, run_executor, stringify_exec_output,
    };
    use async_trait::async_trait;
    use autoagents_core::agent::memory::SlidingWindowMemory;
    use autoagents_core::agent::{AgentDeriveT, AgentHooks, Context, HookOutcome, task::Task};
    use autoagents_core::tool::{ToolCallError, ToolCallResult, ToolRuntime, ToolT};
    use autoagents_llm::LLMProvider;
    use autoagents_llm::ToolCall;
    use autoagents_llm::chat::{
        ChatMessage, ChatProvider, ChatResponse, StreamChoice, StreamChunk, StreamDelta,
        StreamResponse, StructuredOutputFormat,
    };
    use autoagents_llm::completion::{CompletionProvider, CompletionRequest, CompletionResponse};
    use autoagents_llm::embedding::EmbeddingProvider;
    use autoagents_llm::error::LLMError;
    use autoagents_llm::models::{ModelListRequest, ModelListResponse, ModelsProvider};
    use futures_util::stream;
    use odyssey_rs_protocol::{AutoAgentsEvent, AutoAgentsStreamChunk};
    use odyssey_rs_protocol::{EventMsg, EventPayload, ExecStream, TurnContext};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use std::fmt;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use uuid::Uuid;

    #[derive(Clone, Debug, Default)]
    struct NoopLlm;

    #[derive(Debug)]
    struct EmptyChatResponse;

    #[derive(Clone, Debug)]
    struct StaticTextLlm {
        text: String,
    }

    #[derive(Debug)]
    struct StaticTextChatResponse {
        text: String,
    }

    impl fmt::Display for EmptyChatResponse {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("")
        }
    }

    impl ChatResponse for EmptyChatResponse {
        fn text(&self) -> Option<String> {
            Some(String::default())
        }

        fn tool_calls(&self) -> Option<Vec<ToolCall>> {
            None
        }
    }

    impl fmt::Display for StaticTextChatResponse {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.text)
        }
    }

    impl ChatResponse for StaticTextChatResponse {
        fn text(&self) -> Option<String> {
            Some(self.text.clone())
        }

        fn tool_calls(&self) -> Option<Vec<ToolCall>> {
            None
        }
    }

    #[async_trait]
    impl ChatProvider for NoopLlm {
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[autoagents_llm::chat::Tool]>,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<Box<dyn ChatResponse>, LLMError> {
            Ok(Box::new(EmptyChatResponse))
        }
    }

    #[async_trait]
    impl CompletionProvider for NoopLlm {
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<CompletionResponse, LLMError> {
            Ok(CompletionResponse {
                text: String::default(),
            })
        }
    }

    #[async_trait]
    impl EmbeddingProvider for NoopLlm {
        async fn embed(&self, _input: Vec<String>) -> Result<Vec<Vec<f32>>, LLMError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ModelsProvider for NoopLlm {
        async fn list_models(
            &self,
            _request: Option<&ModelListRequest>,
        ) -> Result<Box<dyn ModelListResponse>, LLMError> {
            Err(LLMError::ProviderError("unused in tests".to_string()))
        }
    }

    impl LLMProvider for NoopLlm {}

    #[async_trait]
    impl ChatProvider for StaticTextLlm {
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[autoagents_llm::chat::Tool]>,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<Box<dyn ChatResponse>, LLMError> {
            Ok(Box::new(StaticTextChatResponse {
                text: self.text.clone(),
            }))
        }

        async fn chat_stream(
            &self,
            _messages: &[ChatMessage],
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<
            std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<String, LLMError>> + Send>>,
            LLMError,
        > {
            let stream = stream::iter(vec![Ok(self.text.clone())]);
            Ok(Box::pin(stream))
        }

        async fn chat_stream_struct(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[autoagents_llm::chat::Tool]>,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<StreamResponse, LLMError>> + Send>,
            >,
            LLMError,
        > {
            let stream = stream::iter(vec![Ok(StreamResponse {
                choices: vec![StreamChoice {
                    delta: StreamDelta {
                        content: Some(self.text.clone()),
                        reasoning_content: None,
                        tool_calls: None,
                    },
                }],
                usage: None,
            })]);
            Ok(Box::pin(stream))
        }

        async fn chat_stream_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: Option<&[autoagents_llm::chat::Tool]>,
            _json_schema: Option<StructuredOutputFormat>,
        ) -> Result<
            std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<StreamChunk, LLMError>> + Send>,
            >,
            LLMError,
        > {
            let stream = stream::iter(vec![
                Ok(StreamChunk::Text(self.text.clone())),
                Ok(StreamChunk::Done {
                    stop_reason: "end_turn".to_string(),
                }),
            ]);
            Ok(Box::pin(stream))
        }
    }

    #[async_trait]
    impl CompletionProvider for StaticTextLlm {
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
    impl EmbeddingProvider for StaticTextLlm {
        async fn embed(&self, _input: Vec<String>) -> Result<Vec<Vec<f32>>, LLMError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ModelsProvider for StaticTextLlm {
        async fn list_models(
            &self,
            _request: Option<&ModelListRequest>,
        ) -> Result<Box<dyn ModelListResponse>, LLMError> {
            Err(LLMError::ProviderError("unused in tests".to_string()))
        }
    }

    impl LLMProvider for StaticTextLlm {}

    #[derive(Debug)]
    struct DummyTool;

    #[async_trait]
    impl ToolRuntime for DummyTool {
        async fn execute(&self, _args: Value) -> Result<Value, ToolCallError> {
            Ok(Value::Null)
        }
    }

    impl ToolT for DummyTool {
        fn name(&self) -> &str {
            "Read"
        }

        fn description(&self) -> &str {
            "Read a file"
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

    #[test]
    fn parse_json_value_falls_back_to_string() {
        assert_eq!(parse_json_value("{\"value\":1}"), json!({ "value": 1 }));
        assert_eq!(
            parse_json_value("not-json"),
            Value::String("not-json".to_string())
        );
        assert_eq!(stringify_exec_output(json!("done")), "done");
        assert_eq!(
            stringify_exec_output(json!({ "value": 1 })),
            "{\"value\":1}"
        );
    }

    #[tokio::test]
    async fn run_executor_rejects_unknown_executor_ids() {
        let (sender, _) = broadcast::channel(1);
        let error = run_executor(ExecutorRun {
            executor_id: "unknown".to_string(),
            llm: Arc::new(NoopLlm),
            system_prompt: "stay concise".to_string(),
            task: Task::new("hello"),
            memory: None,
            tools: Vec::new(),
            session_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            sender,
            turn_context: TurnContext::default(),
        })
        .await
        .expect_err("unknown executor should fail");

        assert!(error.to_string().contains("unsupported prebuilt executor"));
    }

    async fn run_supported_executor(executor_id: &str) -> (String, EventMsg) {
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let (sender, mut receiver) = broadcast::channel(64);
        let llm = Arc::new(StaticTextLlm {
            text: format!("{executor_id} response"),
        });
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_executor(ExecutorRun {
                executor_id: executor_id.to_string(),
                llm,
                system_prompt: "stay concise".to_string(),
                task: Task::new("hello"),
                memory: Some(Box::new(SlidingWindowMemory::new(2))),
                tools: vec![Arc::new(DummyTool) as Arc<dyn ToolT>],
                session_id,
                turn_id,
                sender,
                turn_context: TurnContext::default(),
            }),
        )
        .await
        .expect("executor timed out")
        .expect("executor should succeed");

        loop {
            let event = receiver.recv().await.expect("completion event");
            if matches!(event.payload, EventPayload::TurnCompleted { .. }) {
                return (response, event);
            }
        }
    }

    #[tokio::test]
    async fn run_executor_supports_react_streaming_path() {
        let (response, event) = run_supported_executor("react").await;

        assert_eq!(response, "react response");
        assert!(matches!(
            event.payload,
            EventPayload::TurnCompleted { ref message, .. } if message == "react response"
        ));
    }

    #[tokio::test]
    async fn run_executor_supports_codeact_streaming_path() {
        let (response, event) = run_supported_executor("codeact").await;

        assert_eq!(response, "codeact response");
        assert!(matches!(
            event.payload,
            EventPayload::TurnCompleted { ref message, .. } if message == "codeact response"
        ));
    }

    #[tokio::test]
    async fn forward_autoagents_events_stops_after_terminal_events() {
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let (sender, mut receiver) = broadcast::channel(8);
        let events = stream::iter(vec![
            AutoAgentsEvent::TurnStarted {
                sub_id: Uuid::new_v4(),
                actor_id: Uuid::new_v4(),
                turn_number: 1,
                max_turns: 4,
            },
            AutoAgentsEvent::TaskError {
                sub_id: Uuid::new_v4(),
                actor_id: Uuid::new_v4(),
                error: "failed".to_string(),
            },
            AutoAgentsEvent::TurnStarted {
                sub_id: Uuid::new_v4(),
                actor_id: Uuid::new_v4(),
                turn_number: 2,
                max_turns: 4,
            },
        ]);

        forward_autoagents_events(
            Box::pin(events),
            sender,
            session_id,
            turn_id,
            TurnContext::default(),
        )
        .await;

        let started = receiver.recv().await.expect("turn started event");
        assert!(matches!(
            started.payload,
            EventPayload::TurnStarted { turn_id: id, .. } if id == turn_id
        ));

        let errored = receiver.recv().await.expect("error event");
        assert!(matches!(
            errored.payload,
            EventPayload::Error { turn_id: Some(id), message } if id == turn_id && message == "failed"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn odyssey_agent_exposes_metadata_tools_and_default_hooks() {
        let agent = OdysseyAgent::new(
            "system prompt".to_string(),
            vec![Arc::new(DummyTool) as Arc<dyn ToolT>],
        );
        let context = Context::new(Arc::new(NoopLlm), None);
        let task = Task::new("hello");
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            call_type: "function".to_string(),
            function: autoagents_llm::FunctionCall {
                name: "Read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            },
        };

        assert_eq!(agent.description(), "system prompt");
        assert_eq!(agent.output_schema(), None);
        assert_eq!(agent.name(), "odyssey-agent");
        assert_eq!(
            agent
                .tools()
                .into_iter()
                .map(|tool| tool.name().to_string())
                .collect::<Vec<_>>(),
            vec!["Read".to_string()]
        );
        assert!(matches!(
            agent.on_run_start(&task, &context).await,
            HookOutcome::Continue
        ));
        assert!(matches!(
            agent.on_tool_call(&tool_call, &context).await,
            HookOutcome::Continue
        ));
        agent
            .on_tool_result(
                &tool_call,
                &ToolCallResult {
                    tool_name: "Read".to_string(),
                    success: true,
                    arguments: json!({ "path": "README.md" }),
                    result: json!({ "content": "hello" }),
                },
                &context,
            )
            .await;
        agent
            .on_tool_error(&tool_call, json!({ "message": "boom" }), &context)
            .await;
    }

    #[test]
    fn reasoning_content_emits_section_break_before_text() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let reasoning = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ReasoningContent("plan".to_string()),
        });
        assert!(!reasoning.terminal);
        assert_eq!(reasoning.payloads.len(), 1);
        assert!(matches!(
            &reasoning.payloads[0],
            EventPayload::ReasoningDelta { turn_id: id, delta }
                if *id == turn_id && delta == "plan"
        ));

        let text = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::Text("done".to_string()),
        });
        assert_eq!(text.payloads.len(), 2);
        assert!(matches!(
            &text.payloads[0],
            EventPayload::ReasoningSectionBreak { turn_id: id } if *id == turn_id
        ));
        assert!(matches!(
            &text.payloads[1],
            EventPayload::AgentMessageDelta { turn_id: id, delta }
                if *id == turn_id && delta == "done"
        ));
    }

    #[test]
    fn tool_call_events_keep_a_stable_runtime_id() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let started = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ToolUseStart {
                index: 0,
                id: "call_1".to_string(),
                name: "search".to_string(),
            },
        });
        let requested = bridge.map_event(AutoAgentsEvent::ToolCallRequested {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            id: "call_1".to_string(),
            tool_name: "search".to_string(),
            arguments: "{\"q\":\"rust\"}".to_string(),
        });
        let finished = bridge.map_event(AutoAgentsEvent::ToolCallCompleted {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            id: "call_1".to_string(),
            tool_name: "search".to_string(),
            result: json!({ "ok": true }),
        });

        assert!(started.payloads.is_empty());

        let started_id = match &requested.payloads[0] {
            EventPayload::ToolCallStarted { tool_call_id, .. } => *tool_call_id,
            other => panic!("unexpected payload: {other:?}"),
        };
        let finished_id = match &finished.payloads[0] {
            EventPayload::ToolCallFinished { tool_call_id, .. } => *tool_call_id,
            other => panic!("unexpected payload: {other:?}"),
        };

        assert_eq!(started_id, finished_id);
    }

    #[test]
    fn turn_started_maps_context_into_runtime_event() {
        let turn_id = Uuid::new_v4();
        let context = TurnContext {
            cwd: Some("/workspace".to_string()),
            ..TurnContext::default()
        };
        let mut bridge = AutoagentsEventBridge::new(turn_id, context.clone());

        let started = bridge.map_event(AutoAgentsEvent::TurnStarted {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            turn_number: 1,
            max_turns: 4,
        });

        assert!(!started.terminal);
        assert!(matches!(
            &started.payloads[0],
            EventPayload::TurnStarted { turn_id: id, context: mapped }
                if *id == turn_id && mapped.cwd == context.cwd
        ));
    }

    #[test]
    fn stream_tool_call_without_id_is_ignored() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let payloads = bridge.map_event(AutoAgentsEvent::StreamToolCall {
            sub_id: Uuid::new_v4(),
            tool_call: json!({
                "function": {
                    "name": "search",
                    "arguments": "{\"q\":\"rust\"}"
                }
            }),
        });

        assert!(payloads.payloads.is_empty());
    }

    #[test]
    fn stream_tool_call_delta_uses_partial_json_and_stable_id() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let _ = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ToolUseStart {
                index: 2,
                id: "call_2".to_string(),
                name: "search".to_string(),
            },
        });
        let delta = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ToolUseInputDelta {
                index: 2,
                partial_json: "{\"q\":\"rus".to_string(),
            },
        });
        let started = bridge.map_event(AutoAgentsEvent::StreamToolCall {
            sub_id: Uuid::new_v4(),
            tool_call: json!({
                "id": "call_2",
                "function": {
                    "name": "search",
                    "arguments": "{\"q\":\"rust\"}"
                }
            }),
        });

        let delta_id = match &delta.payloads[0] {
            EventPayload::ToolCallDelta {
                tool_call_id,
                delta,
                ..
            } => {
                assert_eq!(delta, &json!({ "partial_json": "{\"q\":\"rus" }));
                *tool_call_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let started_id = match &started.payloads[0] {
            EventPayload::ToolCallStarted {
                tool_call_id,
                arguments,
                ..
            } => {
                assert_eq!(arguments, &json!({ "q": "rust" }));
                *tool_call_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        assert_eq!(delta_id, started_id);
    }

    #[test]
    fn duplicate_tool_call_start_emits_delta_after_initial_start() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let first = bridge.map_event(AutoAgentsEvent::StreamToolCall {
            sub_id: Uuid::new_v4(),
            tool_call: json!({
                "id": "call_4",
                "function": {
                    "name": "search",
                    "arguments": "{\"q\":\"rust\"}"
                }
            }),
        });
        let second = bridge.map_event(AutoAgentsEvent::ToolCallRequested {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            id: "call_4".to_string(),
            tool_name: "search".to_string(),
            arguments: "{\"page\":2}".to_string(),
        });

        let started_id = match &first.payloads[0] {
            EventPayload::ToolCallStarted {
                tool_call_id,
                arguments,
                ..
            } => {
                assert_eq!(arguments, &json!({ "q": "rust" }));
                *tool_call_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let delta_id = match &second.payloads[0] {
            EventPayload::ToolCallDelta {
                tool_call_id,
                delta,
                ..
            } => {
                assert_eq!(delta, &json!({ "page": 2 }));
                *tool_call_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        assert_eq!(started_id, delta_id);
    }

    #[test]
    fn tool_call_failed_marks_unsuccessful_completion() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let failed = bridge.map_event(AutoAgentsEvent::ToolCallFailed {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            id: "call_3".to_string(),
            tool_name: "search".to_string(),
            error: "boom".to_string(),
        });

        assert!(matches!(
            &failed.payloads[0],
            EventPayload::ToolCallFinished { success, result, .. }
                if !success && result == &json!({ "error": "boom" })
        ));
    }

    #[test]
    fn code_execution_events_map_to_exec_payloads() {
        let turn_id = Uuid::new_v4();
        let context = TurnContext {
            cwd: Some("/workspace".to_string()),
            ..TurnContext::default()
        };
        let mut bridge = AutoagentsEventBridge::new(turn_id, context.clone());

        let started = bridge.map_event(AutoAgentsEvent::CodeExecutionStarted {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            execution_id: "exec_1".to_string(),
            language: "typescript".to_string(),
            source: "return 42;".to_string(),
        });
        let console = bridge.map_event(AutoAgentsEvent::CodeExecutionConsole {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            execution_id: "exec_1".to_string(),
            message: "console.log(42)".to_string(),
        });
        let completed = bridge.map_event(AutoAgentsEvent::CodeExecutionCompleted {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            execution_id: "exec_1".to_string(),
            result: json!(42),
            duration_ms: 7,
        });

        let started_id = match &started.payloads[0] {
            EventPayload::ExecCommandBegin {
                turn_id: id,
                exec_id,
                command,
                cwd,
            } => {
                assert_eq!(*id, turn_id);
                assert_eq!(
                    command,
                    &vec!["codeact".to_string(), "typescript".to_string()]
                );
                assert_eq!(cwd, &context.cwd);
                *exec_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let console_id = match &console.payloads[0] {
            EventPayload::ExecCommandOutputDelta {
                turn_id: id,
                exec_id,
                stream,
                delta,
            } => {
                assert_eq!(*id, turn_id);
                assert!(matches!(stream, ExecStream::Stdout));
                assert_eq!(delta, "console.log(42)");
                *exec_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let result_id = match &completed.payloads[0] {
            EventPayload::ExecCommandOutputDelta {
                turn_id: id,
                exec_id,
                stream,
                delta,
            } => {
                assert_eq!(*id, turn_id);
                assert!(matches!(stream, ExecStream::Stdout));
                assert_eq!(delta, "42");
                *exec_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };
        let finished_id = match &completed.payloads[1] {
            EventPayload::ExecCommandEnd {
                turn_id: id,
                exec_id,
                exit_code,
            } => {
                assert_eq!(*id, turn_id);
                assert_eq!(*exit_code, 0);
                *exec_id
            }
            other => panic!("unexpected payload: {other:?}"),
        };

        assert_eq!(started_id, console_id);
        assert_eq!(console_id, result_id);
        assert_eq!(result_id, finished_id);
    }

    #[test]
    fn code_execution_failure_maps_to_stderr_and_non_zero_exit() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let failed = bridge.map_event(AutoAgentsEvent::CodeExecutionFailed {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            execution_id: "exec_2".to_string(),
            error: "sandbox failure".to_string(),
            duration_ms: 11,
        });

        assert!(matches!(
            &failed.payloads[0],
            EventPayload::ExecCommandOutputDelta {
                turn_id: id,
                stream: ExecStream::Stderr,
                delta,
                ..
            } if *id == turn_id && delta == "sandbox failure"
        ));
        assert!(matches!(
            &failed.payloads[1],
            EventPayload::ExecCommandEnd {
                turn_id: id,
                exit_code: 1,
                ..
            } if *id == turn_id
        ));
    }

    #[test]
    fn code_execution_completion_omits_empty_stdout_delta() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());

        let completed = bridge.map_event(AutoAgentsEvent::CodeExecutionCompleted {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            execution_id: "exec_3".to_string(),
            result: json!(""),
            duration_ms: 3,
        });

        assert_eq!(completed.payloads.len(), 1);
        assert!(matches!(
            &completed.payloads[0],
            EventPayload::ExecCommandEnd {
                turn_id: id,
                exit_code: 0,
                ..
            } if *id == turn_id
        ));
    }

    #[test]
    fn task_error_closes_reasoning_and_terminates_bridge() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());
        let _ = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ReasoningContent("plan".to_string()),
        });

        let errored = bridge.map_event(AutoAgentsEvent::TaskError {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            error: "failed".to_string(),
        });

        assert!(errored.terminal);
        assert!(matches!(
            &errored.payloads[0],
            EventPayload::ReasoningSectionBreak { turn_id: id } if *id == turn_id
        ));
        assert!(matches!(
            &errored.payloads[1],
            EventPayload::Error { turn_id: Some(id), message }
                if *id == turn_id && message == "failed"
        ));
    }

    #[test]
    fn task_complete_closes_reasoning_and_terminates_bridge() {
        let turn_id = Uuid::new_v4();
        let mut bridge = AutoagentsEventBridge::new(turn_id, TurnContext::default());
        let _ = bridge.map_event(AutoAgentsEvent::StreamChunk {
            sub_id: Uuid::new_v4(),
            chunk: AutoAgentsStreamChunk::ReasoningContent("plan".to_string()),
        });

        let completed = bridge.map_event(AutoAgentsEvent::TaskComplete {
            sub_id: Uuid::new_v4(),
            actor_id: Uuid::new_v4(),
            actor_name: "demo".to_string(),
            result: "done".to_string(),
        });

        assert!(completed.terminal);
        assert_eq!(completed.payloads.len(), 1);
        assert!(matches!(
            &completed.payloads[0],
            EventPayload::ReasoningSectionBreak { turn_id: id } if *id == turn_id
        ));
    }

    #[tokio::test]
    async fn emit_sends_event_to_broadcast_subscribers() {
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let (sender, mut receiver) = broadcast::channel(1);

        emit(
            &sender,
            session_id,
            EventPayload::ReasoningSectionBreak { turn_id },
        );

        let message = receiver.recv().await.expect("receive event");
        assert_eq!(message.session_id, session_id);
        assert!(matches!(
            message.payload,
            EventPayload::ReasoningSectionBreak { turn_id: id } if id == turn_id
        ));
    }
}
