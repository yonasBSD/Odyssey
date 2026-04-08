use crate::ManifestError;
use crate::agent_spec::{
    AgentExecution, AgentInterfaces, AgentKind, AgentPolicyHints, AgentProgram, AgentRequirements,
    AgentSpec, AgentToolPolicy, default_abi_version,
};
use crate::bundle_manifest::{
    BundleAgentEntry, BundleDescriptor, BundleExecutor, BundleManifest, BundleMemory,
    BundleSandbox, BundleSignatures, BundleSkill, BundleTool, ManifestVersion,
};
use odyssey_rs_protocol::ModelSpec;
use serde::Deserialize;
use std::fs;
use std::path::{Component, Path, PathBuf};
use wasmparser::Parser;

pub struct BundleLoader<'a> {
    root: &'a Path,
}

impl<'a> BundleLoader<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub fn load_project(&self) -> Result<BundleDescriptor, ManifestError> {
        let bundle = self.load_bundle_manifest(&self.root.join("odyssey.bundle.yaml"))?;
        let mut agents = Vec::with_capacity(bundle.agents.len());
        for entry in &bundle.agents {
            let agent_path = ensure_relative_file(self.root, &entry.spec, "agent spec path")?;
            let mut agent = self.load_agent_spec(&agent_path, entry)?;
            self.validate_agent(&bundle, &agent)?;
            if agent.program.entrypoint.is_empty()
                && let Some(module) = &entry.module
            {
                agent.program.entrypoint.clone_from(module);
            }
            if agent.is_wasm() && agent.program.entrypoint.trim().is_empty() {
                return Err(ManifestError::Invalid {
                    path: agent_path.display().to_string(),
                    message:
                        "wasm agents require program.entrypoint or bundle.spec.agents[].module"
                            .to_string(),
                });
            }
            if !agent.program.entrypoint.trim().is_empty() {
                let entrypoint = ensure_relative_entry(
                    self.root,
                    &agent.program.entrypoint,
                    "agent entrypoint",
                )?;
                if agent.is_wasm() {
                    validate_wasm_component(&entrypoint)?;
                }
            }
            validate_optional_schema(
                self.root,
                agent.interfaces.input_schema.as_deref(),
                "input schema",
            )?;
            validate_optional_schema(
                self.root,
                agent.interfaces.output_schema.as_deref(),
                "output schema",
            )?;
            agents.push(agent);
        }

        let descriptor = BundleDescriptor {
            manifest: bundle,
            agents,
        };
        self.validate_project(&descriptor)?;
        Ok(descriptor)
    }

    pub fn load_bundle_manifest(&self, path: &Path) -> Result<BundleManifest, ManifestError> {
        let content = fs::read_to_string(path).map_err(|err| ManifestError::Io {
            path: path.display().to_string(),
            message: err.to_string(),
        })?;
        let raw = serde_yaml::from_str::<RawBundleFile>(&content).map_err(|err| {
            ManifestError::YamlParse {
                path: path.display().to_string(),
                message: err.to_string(),
            }
        })?;

        Ok(BundleManifest {
            manifest_version: ManifestVersion::default(),
            api_version: raw.api_version,
            kind: raw.kind,
            id: raw.metadata.name,
            version: raw.metadata.version,
            abi_version: raw.spec.abi_version,
            readme: raw
                .metadata
                .readme
                .unwrap_or_else(|| "README.md".to_string()),
            agent_spec: raw
                .spec
                .agents
                .first()
                .map(|entry| entry.spec.clone())
                .unwrap_or_default(),
            executor: BundleExecutor::default(),
            memory: BundleMemory::default(),
            skills: raw.spec.skills,
            tools: raw.spec.tools,
            sandbox: raw.spec.sandbox,
            signatures: raw.spec.signatures,
            agents: raw.spec.agents,
        })
    }

    pub fn load_agent_spec(
        &self,
        path: &Path,
        entry: &BundleAgentEntry,
    ) -> Result<AgentSpec, ManifestError> {
        let content = fs::read_to_string(path).map_err(|err| ManifestError::Io {
            path: path.display().to_string(),
            message: err.to_string(),
        })?;
        let raw = serde_yaml::from_str::<RawAgentFile>(&content).map_err(|err| {
            ManifestError::YamlParse {
                path: path.display().to_string(),
                message: err.to_string(),
            }
        })?;
        let mut tools = raw.spec.tools;
        if !raw.spec.requires.tools.is_empty() {
            let mut required = tools.require;
            required.extend(raw.spec.requires.tools.iter().cloned());
            required.sort();
            required.dedup();
            tools.require = required;
        }
        Ok(AgentSpec {
            id: entry.id.clone(),
            name: raw.metadata.name,
            version: raw.metadata.version,
            description: raw.metadata.description.unwrap_or_default(),
            kind: raw.spec.kind,
            abi_version: raw.spec.abi_version,
            prompt: raw.spec.prompt,
            model: raw.spec.model.unwrap_or_else(default_prompt_model),
            program: raw.spec.program,
            execution: raw.spec.execution,
            interfaces: raw.spec.interfaces,
            requires: raw.spec.requires,
            policy_hints: raw.spec.policy_hints,
            tools,
        })
    }

    pub fn validate_project(&self, descriptor: &BundleDescriptor) -> Result<(), ManifestError> {
        validate_non_empty(
            self.root,
            &descriptor.manifest.id,
            "bundle id cannot be empty",
        )?;
        validate_non_empty(
            self.root,
            &descriptor.manifest.version,
            "bundle version cannot be empty",
        )?;
        ensure_relative_file(self.root, &descriptor.manifest.readme, "readme path")?;
        if descriptor.manifest.agents.is_empty() {
            return invalid(self.root, "bundle must declare at least one agent");
        }
        let default_agents = descriptor
            .manifest
            .agents
            .iter()
            .filter(|agent| agent.default)
            .count();
        if default_agents > 1 {
            return invalid(
                self.root,
                "bundle must not declare more than one default agent",
            );
        }
        if default_agents == 0 && descriptor.manifest.agents.len() > 1 {
            return invalid(
                self.root,
                "multi-agent bundles must declare exactly one default agent",
            );
        }
        for skill in &descriptor.manifest.skills {
            let _ = ensure_relative_entry(self.root, &skill.path, "skill path")?;
        }
        validate_sandbox(self.root, &descriptor.manifest.sandbox)?;
        Ok(())
    }

    fn validate_agent(
        &self,
        bundle: &BundleManifest,
        agent: &AgentSpec,
    ) -> Result<(), ManifestError> {
        validate_non_empty(self.root, &agent.id, "agent id cannot be empty")?;
        validate_non_empty(self.root, &agent.name, "agent name cannot be empty")?;
        if bundle.abi_version.trim().is_empty() || agent.abi_version.trim().is_empty() {
            return invalid(self.root, "bundle and agent abi versions are required");
        }
        if bundle.abi_version != agent.abi_version {
            return invalid(
                self.root,
                "bundle abi version must match each agent abi version",
            );
        }
        if agent.kind == AgentKind::Prompt && agent.prompt.trim().is_empty() {
            return invalid(self.root, "prompt agents require a non-empty prompt");
        }
        if agent.model.provider.trim().is_empty() || agent.model.name.trim().is_empty() {
            return invalid(self.root, "agent model provider and name are required");
        }
        validate_tool_group(self.root, &agent.tools.allow, "agent.tools.allow")?;
        validate_tool_group(self.root, &agent.tools.ask, "agent.tools.ask")?;
        validate_tool_group(self.root, &agent.tools.deny, "agent.tools.deny")?;
        validate_tool_group(self.root, &agent.tools.require, "agent.tools.require")?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundleFile {
    #[serde(rename = "apiVersion")]
    api_version: String,
    kind: String,
    metadata: RawBundleMetadata,
    spec: RawBundleSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundleMetadata {
    name: String,
    version: String,
    #[serde(default)]
    readme: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundleSpec {
    #[serde(rename = "abiVersion", default = "default_abi_version")]
    abi_version: String,
    agents: Vec<BundleAgentEntry>,
    #[serde(default)]
    skills: Vec<BundleSkill>,
    #[serde(default)]
    tools: Vec<BundleTool>,
    #[serde(default)]
    sandbox: BundleSandbox,
    #[serde(default)]
    signatures: BundleSignatures,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentFile {
    #[serde(rename = "apiVersion")]
    _api_version: String,
    #[serde(rename = "kind")]
    _kind: String,
    metadata: RawAgentMetadata,
    spec: RawAgentSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentMetadata {
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgentSpec {
    #[serde(default)]
    kind: AgentKind,
    #[serde(rename = "abiVersion", default = "default_abi_version")]
    abi_version: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    model: Option<ModelSpec>,
    #[serde(default)]
    tools: AgentToolPolicy,
    #[serde(default)]
    program: AgentProgram,
    #[serde(default)]
    execution: AgentExecution,
    #[serde(default)]
    interfaces: AgentInterfaces,
    #[serde(default)]
    requires: AgentRequirements,
    #[serde(rename = "policyHints", default)]
    policy_hints: AgentPolicyHints,
}

fn default_prompt_model() -> ModelSpec {
    ModelSpec {
        provider: "openai".to_string(),
        name: "gpt-4.1-mini".to_string(),
        config: None,
    }
}

fn validate_sandbox(root: &Path, sandbox: &BundleSandbox) -> Result<(), ManifestError> {
    for path in &sandbox.permissions.filesystem.exec {
        let _ = ensure_relative_entry(root, path, "sandbox exec path")?;
    }
    for path in &sandbox.permissions.filesystem.mounts.read {
        ensure_absolute_mount(root, path, "read mount")?;
    }
    for path in &sandbox.permissions.filesystem.mounts.write {
        ensure_absolute_mount(root, path, "write mount")?;
    }
    validate_network_permissions(root, &sandbox.permissions.network)?;
    validate_sandbox_env(root, &sandbox.env)
}

fn validate_network_permissions(root: &Path, values: &[String]) -> Result<(), ManifestError> {
    if values.is_empty() {
        return Ok(());
    }
    if values.len() == 1 && values[0] == "*" {
        return Ok(());
    }
    invalid(
        root,
        "sandbox.permissions.network only supports [] or [\"*\"]",
    )
}

fn validate_tool_group(root: &Path, values: &[String], label: &str) -> Result<(), ManifestError> {
    for value in values {
        if value.trim().is_empty() {
            return invalid(root, &format!("{label} entries cannot be empty"));
        }
    }
    Ok(())
}

fn validate_optional_schema(
    root: &Path,
    value: Option<&str>,
    label: &str,
) -> Result<(), ManifestError> {
    if let Some(value) = value {
        let _ = ensure_relative_file(root, value, label)?;
    }
    Ok(())
}

fn validate_sandbox_env(
    root: &Path,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<(), ManifestError> {
    for (target, source) in env {
        validate_env_name(root, target, "sandbox.env target")?;
        validate_env_name(root, source, "sandbox.env source")?;
    }
    Ok(())
}

fn validate_env_name(root: &Path, value: &str, label: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        return invalid(root, &format!("{label} cannot be empty"));
    }
    if value.contains('=') {
        return invalid(root, &format!("{label} must not contain `=`"));
    }
    Ok(())
}

fn validate_non_empty(root: &Path, value: &str, message: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        return invalid(root, message);
    }
    Ok(())
}

fn ensure_relative_entry(root: &Path, value: &str, label: &str) -> Result<PathBuf, ManifestError> {
    if value.trim().is_empty() {
        return invalid_path(root, &format!("{label} cannot be empty"));
    }
    let relative = Path::new(value);
    if relative.is_absolute() {
        return invalid_path(
            root,
            &format!("{label} must be relative to the project root"),
        );
    }
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return invalid_path(root, &format!("{label} must stay inside the project root"));
    }
    let path = root.join(relative);
    if !path.exists() {
        return Err(ManifestError::Invalid {
            path: path.display().to_string(),
            message: format!("{label} does not exist"),
        });
    }
    let canonical_root = canonicalize_root(root)?;
    let canonical = path.canonicalize().map_err(|err| ManifestError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    })?;
    if !canonical.starts_with(&canonical_root) {
        return invalid_path(root, &format!("{label} must stay inside the project root"));
    }
    Ok(canonical)
}

fn ensure_relative_file(root: &Path, value: &str, label: &str) -> Result<PathBuf, ManifestError> {
    let path = ensure_relative_entry(root, value, label)?;
    if !path.is_file() {
        return invalid_path(root, &format!("{label} must be a file"));
    }
    Ok(path)
}

fn ensure_absolute_mount(root: &Path, value: &str, label: &str) -> Result<(), ManifestError> {
    let path = Path::new(value);
    if !path.is_absolute() && !is_current_directory_mount(path) {
        return invalid(
            root,
            &format!("{label} must be an absolute host path or `.`"),
        );
    }
    Ok(())
}

fn is_current_directory_mount(path: &Path) -> bool {
    let mut components = path.components();
    components.next().is_some()
        && components.all(|component| component == std::path::Component::CurDir)
}

fn canonicalize_root(root: &Path) -> Result<PathBuf, ManifestError> {
    root.canonicalize().map_err(|err| ManifestError::Io {
        path: root.display().to_string(),
        message: err.to_string(),
    })
}

fn validate_wasm_component(path: &Path) -> Result<(), ManifestError> {
    let bytes = fs::read(path).map_err(|err| ManifestError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    })?;
    if Parser::is_component(&bytes) {
        return Ok(());
    }
    Err(ManifestError::Invalid {
        path: path.display().to_string(),
        message: format!(
            "wasm agent entrypoint must be a WebAssembly component; rebuild `{}` with a component-model toolchain such as `cargo component build`",
            path.display()
        ),
    })
}

fn invalid(root: &Path, message: &str) -> Result<(), ManifestError> {
    Err(ManifestError::Invalid {
        path: root.display().to_string(),
        message: message.to_string(),
    })
}

fn invalid_path(root: &Path, message: &str) -> Result<PathBuf, ManifestError> {
    Err(ManifestError::Invalid {
        path: root.display().to_string(),
        message: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::BundleLoader;
    use crate::AgentKind;
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn load_project_normalizes_bundle_and_agents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path().join("agents").join("reviewer");
        fs::create_dir_all(&agent_dir).expect("mkdir");
        fs::write(temp.path().join("README.md"), "# Demo\n").expect("readme");
        fs::write(
            temp.path().join("odyssey.bundle.yaml"),
            r#"
apiVersion: odyssey.ai/bundle.v1
kind: AgentBundle
metadata:
  name: demo
  version: 0.2.0
  readme: README.md
spec:
  abiVersion: v1
  agents:
    - id: reviewer
      spec: agents/reviewer/agent.yaml
      module: agents/reviewer/module.wasm
      default: true
"#,
        )
        .expect("bundle");
        fs::write(agent_dir.join("module.wasm"), b"\0asm\x0d\0\x01\0").expect("wasm");
        fs::write(
            agent_dir.join("agent.yaml"),
            r#"
apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: reviewer
  version: 0.2.0
spec:
  kind: wasm
  abiVersion: v1
  program:
    runner_class: wasm-component
  execution:
    executor: react/v1
    memory: session-window/v1
"#,
        )
        .expect("agent");

        let descriptor = BundleLoader::new(temp.path())
            .load_project()
            .expect("project");

        assert_eq!(descriptor.manifest.id, "demo");
        assert_eq!(descriptor.default_agent_id(), Some("reviewer"));
        assert_eq!(descriptor.agents.len(), 1);
        assert_eq!(descriptor.agents[0].kind, AgentKind::Wasm);
        assert_eq!(
            descriptor.agents[0].program.entrypoint,
            "agents/reviewer/module.wasm"
        );
    }

    #[test]
    fn load_project_rejects_core_wasm_modules_for_wasm_agents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path().join("agents").join("reviewer");
        fs::create_dir_all(&agent_dir).expect("mkdir");
        fs::write(temp.path().join("README.md"), "# Demo\n").expect("readme");
        fs::write(
            temp.path().join("odyssey.bundle.yaml"),
            r#"
apiVersion: odyssey.ai/bundle.v1
kind: AgentBundle
metadata:
  name: demo
  version: 0.2.0
spec:
  abiVersion: v1
  agents:
    - id: reviewer
      spec: agents/reviewer/agent.yaml
      module: agents/reviewer/module.wasm
"#,
        )
        .expect("bundle");
        fs::write(agent_dir.join("module.wasm"), b"\0asm\x01\0\0\0").expect("wasm");
        fs::write(
            agent_dir.join("agent.yaml"),
            r#"
apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: reviewer
  version: 0.2.0
spec:
  kind: wasm
  abiVersion: v1
  program:
    runner_class: wasm-component
  execution:
    executor: react/v1
    memory: session-window/v1
"#,
        )
        .expect("agent");

        let error = BundleLoader::new(temp.path())
            .load_project()
            .expect_err("core wasm should be rejected");

        assert_eq!(
            error.to_string(),
            format!(
                "invalid manifest at {}: wasm agent entrypoint must be a WebAssembly component; rebuild `{}` with a component-model toolchain such as `cargo component build`",
                agent_dir.join("module.wasm").display(),
                agent_dir.join("module.wasm").display()
            )
        );
    }
}
