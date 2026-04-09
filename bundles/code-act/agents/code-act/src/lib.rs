use async_trait::async_trait;
use autoagents::core::tool::{ToolCallError, ToolRuntime, ToolT};
use autoagents_derive::{AgentHooks, ToolInput, agent, tool};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Serialize, Deserialize, ToolInput, Debug)]
pub struct AdditionArgs {
    #[input(description = "Left Operand for addition")]
    left: i64,
    #[input(description = "Right Operand for addition")]
    right: i64,
}

#[tool(
    name = "Addition",
    description = "Use this tool to Add two numbers",
    input = AdditionArgs,
)]
struct Addition {}

#[async_trait]
impl ToolRuntime for Addition {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        let typed_args: AdditionArgs = serde_json::from_value(args)?;
        let result = typed_args.left + typed_args.right;
        Ok(result.into())
    }
}

#[agent(
    name = "code-act",
    description = "CodeAct Agent Example for Odyssey",
    tools = [],
)]
#[derive(Default, Clone, AgentHooks)]
pub struct WorkspaceAgent {}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn app()
-> odyssey_rs_agent_sdk::OdysseyAgentApp<WorkspaceAgent, odyssey_rs_agent_sdk::CodeActExecutor> {
    odyssey_rs_agent_sdk::OdysseyAgentApp::codeact(WorkspaceAgent::default())
        .memory_window(20)
        .tool(Arc::new(Addition {}))
        .max_turns(12)
}

odyssey_rs_agent_sdk::export_odyssey_agent!("code-act", app());
