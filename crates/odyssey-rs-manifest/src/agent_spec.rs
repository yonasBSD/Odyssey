use odyssey_rs_protocol::ModelSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    #[default]
    Prompt,
    Wasm,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentToolPolicy {
    #[serde(default)]
    pub require: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentProgram {
    #[serde(default = "default_runner_class")]
    pub runner_class: String,
    #[serde(default)]
    pub entrypoint: String,
}

impl Default for AgentProgram {
    fn default() -> Self {
        Self {
            runner_class: default_runner_class(),
            entrypoint: String::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentExecution {
    #[serde(default = "default_executor_class")]
    pub executor: String,
    #[serde(default = "default_memory_class")]
    pub memory: String,
    #[serde(default)]
    pub memory_config: Value,
}

impl Default for AgentExecution {
    fn default() -> Self {
        Self {
            executor: default_executor_class(),
            memory: default_memory_class(),
            memory_config: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentInterfaces {
    #[serde(default)]
    pub input_schema: Option<String>,
    #[serde(default)]
    pub output_schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRequirements {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentPolicyHints {
    #[serde(default = "default_isolation_class")]
    pub isolation_class: String,
    #[serde(default = "default_network_policy")]
    pub network: String,
    #[serde(default = "default_filesystem_policy")]
    pub filesystem: String,
}

impl Default for AgentPolicyHints {
    fn default() -> Self {
        Self {
            isolation_class: default_isolation_class(),
            network: default_network_policy(),
            filesystem: default_filesystem_policy(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSpec {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: AgentKind,
    #[serde(default = "default_abi_version")]
    pub abi_version: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default = "default_model")]
    pub model: ModelSpec,
    #[serde(default)]
    pub program: AgentProgram,
    #[serde(default)]
    pub execution: AgentExecution,
    #[serde(default)]
    pub interfaces: AgentInterfaces,
    #[serde(default)]
    pub requires: AgentRequirements,
    #[serde(default)]
    pub policy_hints: AgentPolicyHints,
    #[serde(default)]
    pub tools: AgentToolPolicy,
}

impl AgentSpec {
    pub fn is_wasm(&self) -> bool {
        self.kind == AgentKind::Wasm
    }
}

impl Default for AgentSpec {
    fn default() -> Self {
        Self {
            id: String::default(),
            name: String::default(),
            version: "0.1.0".to_string(),
            description: String::default(),
            kind: AgentKind::Prompt,
            abi_version: default_abi_version(),
            prompt: String::default(),
            model: default_model(),
            program: AgentProgram::default(),
            execution: AgentExecution::default(),
            interfaces: AgentInterfaces::default(),
            requires: AgentRequirements::default(),
            policy_hints: AgentPolicyHints::default(),
            tools: AgentToolPolicy::default(),
        }
    }
}

pub(crate) fn default_abi_version() -> String {
    "v1".to_string()
}

fn default_model() -> ModelSpec {
    ModelSpec {
        provider: "openai".to_string(),
        name: "gpt-4.1-mini".to_string(),
        config: None,
    }
}

fn default_runner_class() -> String {
    "prompt-inline".to_string()
}

fn default_executor_class() -> String {
    "react/v1".to_string()
}

fn default_memory_class() -> String {
    "session-window/v1".to_string()
}

fn default_isolation_class() -> String {
    "default".to_string()
}

fn default_network_policy() -> String {
    "denied".to_string()
}

fn default_filesystem_policy() -> String {
    "brokered".to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        AgentExecution, AgentInterfaces, AgentKind, AgentPolicyHints, AgentProgram,
        AgentRequirements, AgentSpec, AgentToolPolicy,
    };
    use odyssey_rs_protocol::ModelSpec;
    use pretty_assertions::assert_eq;

    #[test]
    fn prompt_agents_can_be_constructed_with_runtime_defaults() {
        let spec = AgentSpec {
            id: "demo".to_string(),
            name: "demo".to_string(),
            version: "0.1.0".to_string(),
            description: "Demo".to_string(),
            kind: AgentKind::Prompt,
            abi_version: "v1".to_string(),
            prompt: "be concise".to_string(),
            model: ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-4.1-mini".to_string(),
                config: None,
            },
            program: AgentProgram::default(),
            execution: AgentExecution::default(),
            interfaces: AgentInterfaces::default(),
            requires: AgentRequirements::default(),
            policy_hints: AgentPolicyHints::default(),
            tools: AgentToolPolicy::default(),
        };

        assert!(!spec.is_wasm());
        assert_eq!(spec.execution.executor, "react/v1");
        assert_eq!(spec.execution.memory, "session-window/v1");
    }

    #[test]
    fn wasm_agents_report_kind_from_descriptor() {
        let spec = AgentSpec {
            id: "reviewer".to_string(),
            name: "reviewer".to_string(),
            version: "0.2.0".to_string(),
            description: String::default(),
            kind: AgentKind::Wasm,
            abi_version: "v1".to_string(),
            prompt: String::default(),
            model: ModelSpec {
                provider: "openai".to_string(),
                name: "gpt-4.1-mini".to_string(),
                config: None,
            },
            program: AgentProgram {
                runner_class: "wasm-component".to_string(),
                entrypoint: "agents/reviewer/module.wasm".to_string(),
            },
            execution: AgentExecution::default(),
            interfaces: AgentInterfaces::default(),
            requires: AgentRequirements::default(),
            policy_hints: AgentPolicyHints::default(),
            tools: AgentToolPolicy::default(),
        };

        assert!(spec.is_wasm());
        assert_eq!(spec.program.entrypoint, "agents/reviewer/module.wasm");
    }
}
