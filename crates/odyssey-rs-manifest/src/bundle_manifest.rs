use crate::AgentSpec;
use odyssey_rs_protocol::SandboxMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Prebuilt,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ManifestVersion {
    #[serde(rename = "odyssey.bundle/v1")]
    #[default]
    V1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BundleExecutor {
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    pub id: String,
    #[serde(default)]
    pub config: Value,
}

impl Default for BundleExecutor {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Prebuilt,
            id: "react/v1".to_string(),
            config: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BundleMemory {
    #[serde(rename = "type")]
    pub kind: ProviderKind,
    pub id: String,
    #[serde(default)]
    pub config: Value,
}

impl Default for BundleMemory {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Prebuilt,
            id: "session-window/v1".to_string(),
            config: Value::Null,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSignatures {
    #[serde(default)]
    pub cosign: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleAgentEntry {
    pub id: String,
    pub spec: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSkill {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleTool {
    pub name: String,
    #[serde(default = "builtin_tool_source")]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSandbox {
    #[serde(default = "default_sandbox_mode")]
    pub mode: SandboxMode,
    #[serde(default)]
    pub permissions: BundleSandboxPermissions,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub system_tools_mode: BundleSystemToolsMode,
    #[serde(default)]
    pub system_tools: Vec<String>,
    #[serde(default)]
    pub resources: BundleSandboxLimits,
}

impl Default for BundleSandbox {
    fn default() -> Self {
        Self {
            mode: default_sandbox_mode(),
            permissions: BundleSandboxPermissions::default(),
            env: BTreeMap::new(),
            system_tools_mode: BundleSystemToolsMode::default(),
            system_tools: Vec::new(),
            resources: BundleSandboxLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleSystemToolsMode {
    #[default]
    Explicit,
    Standard,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSandboxPermissions {
    #[serde(default)]
    pub filesystem: BundleSandboxFilesystem,
    #[serde(default)]
    pub network: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSandboxFilesystem {
    #[serde(default)]
    pub exec: Vec<String>,
    #[serde(default)]
    pub mounts: BundleSandboxMounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSandboxMounts {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleSandboxLimits {
    pub cpu: Option<u64>,
    pub memory_mb: Option<u64>,
}

impl Default for BundleSandboxLimits {
    fn default() -> Self {
        Self {
            cpu: Some(1),
            memory_mb: Some(512),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleManifest {
    #[serde(default)]
    pub manifest_version: ManifestVersion,
    pub api_version: String,
    pub kind: String,
    pub id: String,
    pub version: String,
    pub abi_version: String,
    pub readme: String,
    #[serde(default)]
    pub agent_spec: String,
    #[serde(default)]
    pub executor: BundleExecutor,
    #[serde(default)]
    pub memory: BundleMemory,
    #[serde(default)]
    pub skills: Vec<BundleSkill>,
    #[serde(default)]
    pub tools: Vec<BundleTool>,
    #[serde(default)]
    pub sandbox: BundleSandbox,
    #[serde(default)]
    pub signatures: BundleSignatures,
    pub agents: Vec<BundleAgentEntry>,
}

impl BundleManifest {
    pub fn default_agent_entry_id(&self) -> Option<&str> {
        self.agents
            .iter()
            .find(|agent| agent.default)
            .map(|agent| agent.id.as_str())
            .or_else(|| {
                if self.agents.len() == 1 {
                    self.agents.first().map(|agent| agent.id.as_str())
                } else {
                    None
                }
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleDescriptor {
    pub manifest: BundleManifest,
    pub agents: Vec<AgentSpec>,
}

impl BundleDescriptor {
    pub fn default_agent_id(&self) -> Option<&str> {
        self.manifest.default_agent_entry_id()
    }

    pub fn default_agent(&self) -> Option<&AgentSpec> {
        let agent_id = self.default_agent_id()?;
        self.agent(agent_id)
    }

    pub fn agent(&self, agent_id: &str) -> Option<&AgentSpec> {
        self.agents.iter().find(|agent| agent.id == agent_id)
    }
}

fn builtin_tool_source() -> String {
    "builtin".to_string()
}

fn default_sandbox_mode() -> SandboxMode {
    SandboxMode::WorkspaceWrite
}

#[cfg(test)]
mod tests {
    use super::{
        BundleAgentEntry, BundleDescriptor, BundleExecutor, BundleManifest, BundleMemory,
        BundleSandbox, BundleSignatures, ManifestVersion,
    };
    use crate::{
        AgentExecution, AgentInterfaces, AgentKind, AgentPolicyHints, AgentProgram,
        AgentRequirements, AgentSpec, AgentToolPolicy,
    };
    use pretty_assertions::assert_eq;

    #[test]
    fn descriptor_returns_default_agent() {
        let descriptor = BundleDescriptor {
            manifest: BundleManifest {
                manifest_version: ManifestVersion::default(),
                api_version: "odyssey.ai/bundle.v1".to_string(),
                kind: "AgentBundle".to_string(),
                id: "acme".to_string(),
                version: "0.2.0".to_string(),
                abi_version: "v1".to_string(),
                readme: "README.md".to_string(),
                agent_spec: "agents/reviewer/agent.yaml".to_string(),
                executor: BundleExecutor::default(),
                memory: BundleMemory::default(),
                skills: Vec::new(),
                tools: Vec::new(),
                sandbox: BundleSandbox::default(),
                signatures: BundleSignatures::default(),
                agents: vec![BundleAgentEntry {
                    id: "reviewer".to_string(),
                    spec: "agents/reviewer/agent.yaml".to_string(),
                    module: Some("agents/reviewer/module.wasm".to_string()),
                    default: true,
                }],
            },
            agents: vec![AgentSpec {
                id: "reviewer".to_string(),
                name: "reviewer".to_string(),
                version: "0.2.0".to_string(),
                description: String::default(),
                kind: AgentKind::Wasm,
                abi_version: "v1".to_string(),
                prompt: String::default(),
                model: odyssey_rs_protocol::ModelSpec {
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
            }],
        };

        assert_eq!(descriptor.default_agent_id(), Some("reviewer"));
        assert_eq!(
            descriptor
                .default_agent()
                .map(|agent| agent.program.entrypoint.clone()),
            Some("agents/reviewer/module.wasm".to_string())
        );
    }
}
