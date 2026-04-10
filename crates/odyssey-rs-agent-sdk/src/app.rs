use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use autoagents_core::agent::memory::{MemoryProvider, SlidingWindowMemory};
use autoagents_core::agent::prebuilt::executor::{
    BasicAgent, BasicAgentOutput, ReActAgent, ReActAgentOutput,
};
#[cfg(feature = "codeact")]
use autoagents_core::agent::prebuilt::executor::{
    CodeActAgent, CodeActAgentOutput, CodeActSandboxLimits,
};
use autoagents_core::agent::{AgentBuilder, AgentDeriveT, AgentHooks, Context, DirectAgent};
use autoagents_core::tool::{ToolCallResult, ToolT, shared_tools_to_boxes};
use autoagents_llm::ToolCall;
use serde_json::Value;

use crate::host_tools::HostToolCatalog;
use crate::{AgentResult, AgentSdkError, RunRequest, RunResponse, host, run_handle};

const DEFAULT_MEMORY_WINDOW: usize = 20;
const DEFAULT_MAX_TURNS: usize = 10;

#[derive(Debug, Clone, Copy, Default)]
pub struct BasicExecutor;

#[derive(Debug, Clone, Copy)]
pub struct ReactExecutor {
    max_turns: usize,
}

impl Default for ReactExecutor {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
        }
    }
}

#[cfg(feature = "codeact")]
#[derive(Debug, Clone)]
pub struct CodeActExecutor {
    max_turns: usize,
    sandbox_limits: Option<CodeActSandboxLimits>,
}

#[cfg(feature = "codeact")]
impl Default for CodeActExecutor {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            sandbox_limits: None,
        }
    }
}

enum MemoryConfig {
    SlidingWindow(usize),
    Custom(Box<dyn MemoryProvider>),
    Disabled,
}

pub struct OdysseyAgentApp<A, E> {
    agent: A,
    executor: E,
    custom_tools: Vec<Arc<dyn ToolT>>,
    memory: MemoryConfig,
}

pub type AutoAgentApp<A, E> = OdysseyAgentApp<A, E>;

impl<A> OdysseyAgentApp<A, BasicExecutor> {
    pub fn basic(agent: A) -> Self {
        Self {
            agent,
            executor: BasicExecutor,
            custom_tools: Vec::new(),
            memory: MemoryConfig::SlidingWindow(DEFAULT_MEMORY_WINDOW),
        }
    }
}

impl<A> OdysseyAgentApp<A, ReactExecutor> {
    pub fn react(agent: A) -> Self {
        Self {
            agent,
            executor: ReactExecutor::default(),
            custom_tools: Vec::new(),
            memory: MemoryConfig::SlidingWindow(DEFAULT_MEMORY_WINDOW),
        }
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.executor.max_turns = max_turns.max(1);
        self
    }
}

#[cfg(feature = "codeact")]
impl<A> OdysseyAgentApp<A, CodeActExecutor> {
    pub fn codeact(agent: A) -> Self {
        Self {
            agent,
            executor: CodeActExecutor::default(),
            custom_tools: Vec::new(),
            memory: MemoryConfig::SlidingWindow(DEFAULT_MEMORY_WINDOW),
        }
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.executor.max_turns = max_turns.max(1);
        self
    }

    pub fn sandbox_limits(mut self, limits: CodeActSandboxLimits) -> Self {
        self.executor.sandbox_limits = Some(limits);
        self
    }
}

impl<A, E> OdysseyAgentApp<A, E> {
    pub fn tool(mut self, tool: Arc<dyn ToolT>) -> Self {
        self.custom_tools.push(tool);
        self
    }

    pub fn tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn ToolT>>,
    {
        self.custom_tools.extend(tools);
        self
    }

    pub fn memory_window(mut self, window: usize) -> Self {
        self.memory = MemoryConfig::SlidingWindow(window.max(1));
        self
    }

    pub fn without_memory(mut self) -> Self {
        self.memory = MemoryConfig::Disabled;
        self
    }

    pub fn memory(mut self, memory: Box<dyn MemoryProvider>) -> Self {
        self.memory = MemoryConfig::Custom(memory);
        self
    }

    pub fn host_tools(&self, request: &RunRequest) -> HostToolCatalog {
        HostToolCatalog::from_request(request)
    }
}

#[cfg_attr(all(target_arch = "wasm32", target_os = "wasi"), async_trait(?Send))]
#[cfg_attr(not(all(target_arch = "wasm32", target_os = "wasi")), async_trait)]
pub trait RunnableApp {
    async fn run(self, request: &RunRequest) -> AgentResult<RunResponse>;
}

pub async fn run_app<T>(app: T, request: &RunRequest) -> AgentResult<RunResponse>
where
    T: RunnableApp + Send,
{
    app.run(request).await
}

#[cfg_attr(all(target_arch = "wasm32", target_os = "wasi"), async_trait(?Send))]
#[cfg_attr(not(all(target_arch = "wasm32", target_os = "wasi")), async_trait)]
impl<A> RunnableApp for OdysseyAgentApp<A, BasicExecutor>
where
    A: AgentDeriveT + AgentHooks + Send + Sync + 'static,
    A::Output: Into<String> + From<BasicAgentOutput>,
{
    async fn run(self, request: &RunRequest) -> AgentResult<RunResponse> {
        let OdysseyAgentApp {
            agent,
            custom_tools,
            memory,
            ..
        } = self;
        let prepared = prepare_run(agent, custom_tools, memory, request)?;
        let mut builder =
            AgentBuilder::<_, DirectAgent>::new(BasicAgent::new(prepared.agent)).llm(prepared.llm);
        if let Some(memory) = prepared.memory {
            builder = builder.memory(memory);
        }
        let handle = builder
            .build()
            .await
            .map_err(|err| AgentSdkError::Execution(err.to_string()))?;
        run_handle(handle, request).await
    }
}

#[cfg_attr(all(target_arch = "wasm32", target_os = "wasi"), async_trait(?Send))]
#[cfg_attr(not(all(target_arch = "wasm32", target_os = "wasi")), async_trait)]
impl<A> RunnableApp for OdysseyAgentApp<A, ReactExecutor>
where
    A: AgentDeriveT + AgentHooks + Send + Sync + 'static,
    A::Output: Into<String> + From<ReActAgentOutput>,
{
    async fn run(self, request: &RunRequest) -> AgentResult<RunResponse> {
        let OdysseyAgentApp {
            agent,
            executor,
            custom_tools,
            memory,
        } = self;
        let prepared = prepare_run(agent, custom_tools, memory, request)?;
        let mut builder = AgentBuilder::<_, DirectAgent>::new(ReActAgent::with_max_turns(
            prepared.agent,
            executor.max_turns,
        ))
        .llm(prepared.llm)
        .stream(true);
        if let Some(memory) = prepared.memory {
            builder = builder.memory(memory);
        }
        let handle = builder
            .build()
            .await
            .map_err(|err| AgentSdkError::Execution(err.to_string()))?;
        run_handle(handle, request).await
    }
}

#[cfg(feature = "codeact")]
#[cfg_attr(all(target_arch = "wasm32", target_os = "wasi"), async_trait(?Send))]
#[cfg_attr(not(all(target_arch = "wasm32", target_os = "wasi")), async_trait)]
impl<A> RunnableApp for OdysseyAgentApp<A, CodeActExecutor>
where
    A: AgentDeriveT + AgentHooks + Send + Sync + 'static,
    A::Output: Into<String> + From<CodeActAgentOutput>,
{
    async fn run(self, request: &RunRequest) -> AgentResult<RunResponse> {
        let OdysseyAgentApp {
            agent,
            executor,
            custom_tools,
            memory,
        } = self;
        let prepared = prepare_run(agent, custom_tools, memory, request)?;
        let mut codeact_agent = CodeActAgent::with_max_turns(prepared.agent, executor.max_turns);
        if let Some(limits) = executor.sandbox_limits {
            codeact_agent = codeact_agent.with_sandbox_limits(limits);
        }
        let mut builder = AgentBuilder::<_, DirectAgent>::new(codeact_agent)
            .llm(prepared.llm)
            .stream(true);
        if let Some(memory) = prepared.memory {
            builder = builder.memory(memory);
        }
        let handle = builder
            .build()
            .await
            .map_err(|err| AgentSdkError::Execution(err.to_string()))?;
        run_handle(handle, request).await
    }
}

struct PreparedRun<A> {
    agent: AugmentedAgent<A>,
    llm: Arc<dyn autoagents_llm::LLMProvider>,
    memory: Option<Box<dyn MemoryProvider>>,
}

fn prepare_run<A>(
    agent: A,
    custom_tools: Vec<Arc<dyn ToolT>>,
    memory: MemoryConfig,
    request: &RunRequest,
) -> AgentResult<PreparedRun<A>>
where
    A: AgentDeriveT + AgentHooks + Send + Sync + 'static,
{
    let agent_tools = agent.tools();
    let host_tools = HostToolCatalog::from_request(request).tools();
    validate_unique_tool_names(&agent_tools, &custom_tools, &host_tools)?;
    let memory = build_memory(memory, request)?;
    Ok(PreparedRun {
        agent: AugmentedAgent::new(agent, custom_tools, host_tools),
        llm: host::llm_provider(),
        memory,
    })
}

fn build_memory(
    config: MemoryConfig,
    request: &RunRequest,
) -> AgentResult<Option<Box<dyn MemoryProvider>>> {
    match config {
        MemoryConfig::Disabled => Ok(None),
        MemoryConfig::SlidingWindow(window) => {
            let mut memory = SlidingWindowMemory::new(window.max(1));
            host::preload_memory(&mut memory, request)?;
            Ok(Some(Box::new(memory)))
        }
        MemoryConfig::Custom(mut memory) => {
            host::preload_memory(memory.as_mut(), request)?;
            Ok(Some(memory))
        }
    }
}

fn validate_unique_tool_names(
    agent_tools: &[Box<dyn ToolT>],
    custom_tools: &[Arc<dyn ToolT>],
    host_tools: &[Arc<dyn ToolT>],
) -> AgentResult<()> {
    let mut seen = HashSet::new();

    for tool in agent_tools {
        if !seen.insert(tool.name().to_string()) {
            return Err(AgentSdkError::DuplicateTool(tool.name().to_string()));
        }
    }
    for tool in custom_tools {
        if !seen.insert(tool.name().to_string()) {
            return Err(AgentSdkError::DuplicateTool(tool.name().to_string()));
        }
    }
    for tool in host_tools {
        if !seen.insert(tool.name().to_string()) {
            return Err(AgentSdkError::DuplicateTool(tool.name().to_string()));
        }
    }

    Ok(())
}

#[derive(Debug)]
struct AugmentedAgent<A> {
    inner: A,
    custom_tools: Vec<Arc<dyn ToolT>>,
    host_tools: Vec<Arc<dyn ToolT>>,
}

impl<A> AugmentedAgent<A> {
    fn new(inner: A, custom_tools: Vec<Arc<dyn ToolT>>, host_tools: Vec<Arc<dyn ToolT>>) -> Self {
        Self {
            inner,
            custom_tools,
            host_tools,
        }
    }
}

impl<A> AgentDeriveT for AugmentedAgent<A>
where
    A: AgentDeriveT,
{
    type Output = A::Output;

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn output_schema(&self) -> Option<Value> {
        self.inner.output_schema()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn tools(&self) -> Vec<Box<dyn ToolT>> {
        let mut tools = self.inner.tools();
        tools.extend(shared_tools_to_boxes(&self.custom_tools));
        tools.extend(shared_tools_to_boxes(&self.host_tools));
        tools
    }
}

#[async_trait]
impl<A> AgentHooks for AugmentedAgent<A>
where
    A: AgentDeriveT + AgentHooks + Send + Sync + 'static,
{
    async fn on_agent_create(&self) {
        self.inner.on_agent_create().await
    }

    async fn on_run_start(
        &self,
        task: &autoagents_core::agent::task::Task,
        ctx: &Context,
    ) -> autoagents_core::agent::HookOutcome {
        self.inner.on_run_start(task, ctx).await
    }

    async fn on_run_complete(
        &self,
        task: &autoagents_core::agent::task::Task,
        result: &Self::Output,
        ctx: &Context,
    ) {
        self.inner.on_run_complete(task, result, ctx).await
    }

    async fn on_turn_start(&self, turn_index: usize, ctx: &Context) {
        self.inner.on_turn_start(turn_index, ctx).await
    }

    async fn on_turn_complete(&self, turn_index: usize, ctx: &Context) {
        self.inner.on_turn_complete(turn_index, ctx).await
    }

    async fn on_tool_call(
        &self,
        tool_call: &ToolCall,
        ctx: &Context,
    ) -> autoagents_core::agent::HookOutcome {
        self.inner.on_tool_call(tool_call, ctx).await
    }

    async fn on_tool_start(&self, tool_call: &ToolCall, ctx: &Context) {
        self.inner.on_tool_start(tool_call, ctx).await
    }

    async fn on_tool_result(&self, tool_call: &ToolCall, result: &ToolCallResult, ctx: &Context) {
        self.inner.on_tool_result(tool_call, result, ctx).await
    }

    async fn on_tool_error(&self, tool_call: &ToolCall, err: Value, ctx: &Context) {
        self.inner.on_tool_error(tool_call, err, ctx).await
    }

    async fn on_agent_shutdown(&self) {
        self.inner.on_agent_shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use autoagents_core::agent::Context;
    use autoagents_core::agent::memory::{MemoryProvider, MemoryType};
    #[cfg(feature = "codeact")]
    use autoagents_core::agent::prebuilt::executor::CodeActSandboxLimits;
    use autoagents_core::agent::{AgentDeriveT, AgentHooks, HookOutcome};
    use autoagents_core::tool::{ToolCallError, ToolCallResult, ToolRuntime, ToolT};
    use autoagents_llm::LLMProvider;
    use autoagents_llm::ToolCall;
    use autoagents_llm::chat::{ChatMessage, ChatProvider, ChatResponse, StructuredOutputFormat};
    use autoagents_llm::completion::{CompletionProvider, CompletionRequest, CompletionResponse};
    use autoagents_llm::embedding::EmbeddingProvider;
    use autoagents_llm::error::LLMError;
    use autoagents_llm::models::{ModelListRequest, ModelListResponse, ModelsProvider};
    use futures::FutureExt;
    use odyssey_rs_agent_abi::{HostToolSpec, RunRequest, RunResponse};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::{
        AgentSdkError, AugmentedAgent, DEFAULT_MAX_TURNS, DEFAULT_MEMORY_WINDOW, MemoryConfig,
        OdysseyAgentApp, RunnableApp, build_memory, run_app, validate_unique_tool_names,
    };

    #[derive(Debug)]
    struct DummyTool(&'static str);

    #[async_trait]
    impl ToolRuntime for DummyTool {
        async fn execute(&self, _args: Value) -> Result<Value, ToolCallError> {
            Ok(Value::Null)
        }
    }

    #[derive(Clone, Debug, Default)]
    struct NoopLlm;

    #[derive(Debug)]
    struct EmptyChatResponse;

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

    impl ToolT for DummyTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            self.0
        }

        fn args_schema(&self) -> Value {
            json!({"type": "object"})
        }
    }

    #[derive(Debug, Default)]
    struct DummyMemory;

    #[async_trait]
    impl MemoryProvider for DummyMemory {
        async fn remember(&mut self, _message: &ChatMessage) -> Result<(), LLMError> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: Option<usize>,
        ) -> Result<Vec<ChatMessage>, LLMError> {
            Ok(Vec::new())
        }

        async fn clear(&mut self) -> Result<(), LLMError> {
            Ok(())
        }

        fn memory_type(&self) -> MemoryType {
            MemoryType::Custom
        }

        fn size(&self) -> usize {
            0
        }

        fn clone_box(&self) -> Box<dyn MemoryProvider> {
            Box::new(Self)
        }
    }

    #[derive(Debug, Clone, Default)]
    struct DummyAgent;

    impl AgentDeriveT for DummyAgent {
        type Output = String;

        fn description(&self) -> &str {
            "dummy agent"
        }

        fn output_schema(&self) -> Option<Value> {
            None
        }

        fn name(&self) -> &str {
            "dummy"
        }

        fn tools(&self) -> Vec<Box<dyn ToolT>> {
            vec![Box::new(DummyTool("AgentTool"))]
        }
    }

    #[async_trait]
    impl AgentHooks for DummyAgent {}

    #[derive(Debug, Clone)]
    struct RecordingAgent {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl AgentDeriveT for RecordingAgent {
        type Output = String;

        fn description(&self) -> &str {
            "recording agent"
        }

        fn output_schema(&self) -> Option<Value> {
            Some(json!({ "type": "string" }))
        }

        fn name(&self) -> &str {
            "recorder"
        }

        fn tools(&self) -> Vec<Box<dyn ToolT>> {
            vec![Box::new(DummyTool("RecorderTool"))]
        }
    }

    #[async_trait]
    impl AgentHooks for RecordingAgent {
        async fn on_agent_create(&self) {
            self.log
                .lock()
                .expect("lock log")
                .push("create".to_string());
        }

        async fn on_run_start(
            &self,
            task: &autoagents_core::agent::task::Task,
            _ctx: &Context,
        ) -> HookOutcome {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("run_start:{}", task.prompt));
            HookOutcome::Continue
        }

        async fn on_run_complete(
            &self,
            _task: &autoagents_core::agent::task::Task,
            result: &Self::Output,
            _ctx: &Context,
        ) {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("run_complete:{result}"));
        }

        async fn on_turn_start(&self, turn_index: usize, _ctx: &Context) {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("turn_start:{turn_index}"));
        }

        async fn on_turn_complete(&self, turn_index: usize, _ctx: &Context) {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("turn_complete:{turn_index}"));
        }

        async fn on_tool_call(&self, tool_call: &ToolCall, _ctx: &Context) -> HookOutcome {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("tool_call:{}", tool_call.function.name));
            HookOutcome::Continue
        }

        async fn on_tool_start(&self, tool_call: &ToolCall, _ctx: &Context) {
            self.log
                .lock()
                .expect("lock log")
                .push(format!("tool_start:{}", tool_call.function.name));
        }

        async fn on_tool_result(
            &self,
            tool_call: &ToolCall,
            result: &ToolCallResult,
            _ctx: &Context,
        ) {
            self.log.lock().expect("lock log").push(format!(
                "tool_result:{}:{}",
                tool_call.function.name, result.success
            ));
        }

        async fn on_tool_error(&self, tool_call: &ToolCall, err: Value, _ctx: &Context) {
            self.log.lock().expect("lock log").push(format!(
                "tool_error:{}:{}",
                tool_call.function.name,
                err["message"].as_str().unwrap_or_default()
            ));
        }

        async fn on_agent_shutdown(&self) {
            self.log
                .lock()
                .expect("lock log")
                .push("shutdown".to_string());
        }
    }

    fn request() -> RunRequest {
        RunRequest {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            prompt: "hello".to_string(),
            system_prompt: Some("stay concise".to_string()),
            history_json: None,
            metadata_json: None,
            host_tools: vec![HostToolSpec {
                name: "Read".to_string(),
                description: "Read file".to_string(),
                args_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }),
                output_schema: None,
            }],
        }
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let agent_tools = vec![Box::new(DummyTool("Read")) as Box<dyn ToolT>];
        let custom_tools = vec![Arc::new(DummyTool("Read")) as Arc<dyn ToolT>];

        let error = validate_unique_tool_names(&agent_tools, &custom_tools, &[])
            .expect_err("duplicate tool names must fail");

        assert_eq!(
            error.to_string(),
            AgentSdkError::DuplicateTool("Read".to_string()).to_string()
        );
    }

    #[test]
    fn unique_tool_sets_are_accepted() {
        let agent_tools = vec![Box::new(DummyTool("AgentTool")) as Box<dyn ToolT>];
        let custom_tools = vec![Arc::new(DummyTool("CustomTool")) as Arc<dyn ToolT>];
        let host_tools = vec![Arc::new(DummyTool("Read")) as Arc<dyn ToolT>];

        validate_unique_tool_names(&agent_tools, &custom_tools, &host_tools)
            .expect("unique tools should pass");
    }

    #[test]
    fn host_tool_duplicates_are_rejected_after_custom_tool_validation() {
        let custom_tools = vec![Arc::new(DummyTool("SharedTool")) as Arc<dyn ToolT>];
        let host_tools = vec![Arc::new(DummyTool("SharedTool")) as Arc<dyn ToolT>];

        let error = match validate_unique_tool_names(&[], &custom_tools, &host_tools) {
            Ok(()) => panic!("host tool duplicates must fail"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            AgentSdkError::DuplicateTool("SharedTool".to_string()).to_string()
        );
    }

    #[test]
    fn builders_initialize_defaults_and_clamp_turn_counts() {
        let basic = OdysseyAgentApp::basic(DummyAgent);
        assert_eq!(basic.custom_tools.len(), 0);
        assert!(matches!(
            basic.memory,
            MemoryConfig::SlidingWindow(DEFAULT_MEMORY_WINDOW)
        ));

        let react = OdysseyAgentApp::react(DummyAgent).max_turns(0);
        assert_eq!(react.executor.max_turns, 1);
        assert_ne!(DEFAULT_MAX_TURNS, 0);
    }

    #[cfg(feature = "codeact")]
    #[test]
    fn codeact_builders_initialize_defaults_and_capture_limits() {
        let app = OdysseyAgentApp::codeact(DummyAgent)
            .max_turns(0)
            .sandbox_limits(CodeActSandboxLimits::default());

        assert_eq!(app.executor.max_turns, 1);
        assert!(app.executor.sandbox_limits.is_some());
        assert!(matches!(
            app.memory,
            MemoryConfig::SlidingWindow(DEFAULT_MEMORY_WINDOW)
        ));
    }

    #[test]
    fn builder_methods_accumulate_tools_and_replace_memory_configuration() {
        let app = OdysseyAgentApp::basic(DummyAgent)
            .tool(Arc::new(DummyTool("CustomOne")))
            .tools(vec![Arc::new(DummyTool("CustomTwo")) as Arc<dyn ToolT>])
            .memory_window(0);
        let names = app
            .custom_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["CustomOne".to_string(), "CustomTwo".to_string()]
        );
        assert!(matches!(app.memory, MemoryConfig::SlidingWindow(1)));

        let no_memory = OdysseyAgentApp::basic(DummyAgent).without_memory();
        assert!(matches!(no_memory.memory, MemoryConfig::Disabled));

        let custom_memory = OdysseyAgentApp::basic(DummyAgent).memory(Box::new(DummyMemory));
        assert!(matches!(custom_memory.memory, MemoryConfig::Custom(_)));
    }

    #[test]
    fn host_tools_mirror_runtime_request_catalog() {
        let app = OdysseyAgentApp::basic(DummyAgent);
        let catalog = app.host_tools(&request());
        assert_eq!(catalog.specs().len(), 1);
        assert!(catalog.contains("Read"));
        assert!(!catalog.contains("Write"));
    }

    #[test]
    fn build_memory_returns_none_when_disabled() {
        let memory = build_memory(MemoryConfig::Disabled, &request()).expect("disabled memory");
        assert!(memory.is_none());
    }

    #[test]
    fn build_memory_surfaces_host_limitations_for_non_disabled_modes() {
        let sliding = match build_memory(MemoryConfig::SlidingWindow(2), &request()) {
            Ok(_) => panic!("sliding window memory should fail outside wasm"),
            Err(error) => error,
        };
        assert_eq!(
            sliding.to_string(),
            AgentSdkError::UnsupportedHostBuild.to_string()
        );

        let custom = match build_memory(MemoryConfig::Custom(Box::new(DummyMemory)), &request()) {
            Ok(_) => panic!("custom memory should fail outside wasm"),
            Err(error) => error,
        };
        assert_eq!(
            custom.to_string(),
            AgentSdkError::UnsupportedHostBuild.to_string()
        );
    }

    #[test]
    fn augmented_agent_exposes_inner_custom_and_host_tools() {
        let augmented = AugmentedAgent::new(
            DummyAgent,
            vec![Arc::new(DummyTool("CustomTool")) as Arc<dyn ToolT>],
            OdysseyAgentApp::basic(DummyAgent)
                .host_tools(&request())
                .tools(),
        );

        let names = augmented
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "AgentTool".to_string(),
                "CustomTool".to_string(),
                "Read".to_string()
            ]
        );
    }

    struct StubRunnable;

    #[async_trait]
    impl RunnableApp for StubRunnable {
        async fn run(self, request: &RunRequest) -> super::AgentResult<RunResponse> {
            Ok(RunResponse::text(format!("echo: {}", request.prompt)))
        }
    }

    #[tokio::test]
    async fn run_app_delegates_to_runnable_implementations() {
        let response = run_app(StubRunnable, &request()).await.expect("run app");
        assert_eq!(response, RunResponse::text("echo: hello"));
    }

    #[tokio::test]
    async fn augmented_agent_forwards_identity_and_hook_callbacks() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let augmented = AugmentedAgent::new(
            RecordingAgent { log: log.clone() },
            vec![Arc::new(DummyTool("CustomTool")) as Arc<dyn ToolT>],
            vec![Arc::new(DummyTool("HostTool")) as Arc<dyn ToolT>],
        );
        let context = Context::new(Arc::new(NoopLlm), None);
        let task = autoagents_core::agent::task::Task::new("investigate");
        let tool_call = ToolCall {
            id: "call-1".to_string(),
            call_type: "function".to_string(),
            function: autoagents_llm::FunctionCall {
                name: "Read".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            },
        };
        let tool_result = ToolCallResult {
            tool_name: "Read".to_string(),
            success: true,
            arguments: json!({ "path": "README.md" }),
            result: json!({ "content": "hello" }),
        };

        assert_eq!(augmented.description(), "recording agent");
        assert_eq!(augmented.output_schema(), Some(json!({ "type": "string" })));
        assert_eq!(augmented.name(), "recorder");
        assert_eq!(
            augmented
                .tools()
                .into_iter()
                .map(|tool| tool.name().to_string())
                .collect::<Vec<_>>(),
            vec![
                "RecorderTool".to_string(),
                "CustomTool".to_string(),
                "HostTool".to_string()
            ]
        );

        assert!(matches!(
            augmented.on_run_start(&task, &context).await,
            HookOutcome::Continue
        ));
        assert!(matches!(
            augmented.on_tool_call(&tool_call, &context).await,
            HookOutcome::Continue
        ));
        augmented.on_agent_create().await;
        augmented
            .on_run_complete(&task, &"done".to_string(), &context)
            .await;
        augmented.on_turn_start(1, &context).await;
        augmented.on_turn_complete(1, &context).await;
        augmented.on_tool_start(&tool_call, &context).await;
        augmented
            .on_tool_result(&tool_call, &tool_result, &context)
            .await;
        augmented
            .on_tool_error(&tool_call, json!({ "message": "boom" }), &context)
            .await;
        augmented.on_agent_shutdown().await;

        assert_eq!(
            log.lock().expect("lock log").clone(),
            vec![
                "run_start:investigate".to_string(),
                "tool_call:Read".to_string(),
                "create".to_string(),
                "run_complete:done".to_string(),
                "turn_start:1".to_string(),
                "turn_complete:1".to_string(),
                "tool_start:Read".to_string(),
                "tool_result:Read:true".to_string(),
                "tool_error:Read:boom".to_string(),
                "shutdown".to_string(),
            ]
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn basic_and_react_apps_panic_outside_wasm_host_builds() {
        let basic = std::panic::AssertUnwindSafe(run_app(
            OdysseyAgentApp::basic(DummyAgent).without_memory(),
            &request(),
        ))
        .catch_unwind()
        .await;
        assert!(basic.is_err());

        let react = std::panic::AssertUnwindSafe(run_app(
            OdysseyAgentApp::react(DummyAgent).without_memory(),
            &request(),
        ))
        .catch_unwind()
        .await;
        assert!(react.is_err());

        #[cfg(feature = "codeact")]
        {
            let codeact = std::panic::AssertUnwindSafe(run_app(
                OdysseyAgentApp::codeact(DummyAgent).without_memory(),
                &request(),
            ))
            .catch_unwind()
            .await;
            assert!(codeact.is_err());
        }
    }
}
