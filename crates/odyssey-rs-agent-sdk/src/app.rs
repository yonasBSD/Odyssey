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
    use std::sync::Arc;

    use async_trait::async_trait;
    use autoagents_core::tool::{ToolCallError, ToolRuntime, ToolT};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::{AgentSdkError, validate_unique_tool_names};

    #[derive(Debug)]
    struct DummyTool(&'static str);

    #[async_trait]
    impl ToolRuntime for DummyTool {
        async fn execute(&self, _args: Value) -> Result<Value, ToolCallError> {
            Ok(Value::Null)
        }
    }

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
}
