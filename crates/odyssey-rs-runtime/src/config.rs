use directories::BaseDirs;
use odyssey_rs_protocol::{
    DEFAULT_HUB_URL, DEFAULT_RUNTIME_BIND_ADDR, ModelSpec, SandboxMode, SessionSandboxOverlay,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::RuntimeError;

const DEFAULT_WORKER_COUNT: usize = 4;
const DEFAULT_QUEUE_CAPACITY: usize = 128;
const CONFIG_FILE_NAME: &str = "config.yml";
const LOCAL_NAMESPACE: &str = "local";
const LATEST_SELECTOR: &str = "latest";

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub cache_root: PathBuf,
    pub session_root: PathBuf,
    pub sandbox_root: PathBuf,
    pub bind_addr: String,
    pub sandbox_mode_override: Option<SandboxMode>,
    pub hub_url: String,
    pub worker_count: usize,
    pub queue_capacity: usize,
    pub default_model: ModelSpec,
    pub bundle_overrides: BTreeMap<String, BundleRuntimeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AgentRuntimeConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub model_config: Option<Value>,
    #[serde(default)]
    pub sandbox: Option<SessionSandboxOverlay>,
}

impl AgentRuntimeConfig {
    fn resolved_model(&self, base: &ModelSpec) -> Option<ModelSpec> {
        if self.model.is_none() && self.model_provider.is_none() && self.model_config.is_none() {
            return None;
        }
        Some(ModelSpec {
            provider: self
                .model_provider
                .clone()
                .unwrap_or_else(|| base.provider.clone()),
            name: self.model.clone().unwrap_or_else(|| base.name.clone()),
            config: self.model_config.clone().or_else(|| base.config.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BundleRuntimeConfig {
    #[serde(default)]
    pub agents: BTreeMap<String, AgentRuntimeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RuntimeConfigFile {
    #[serde(default)]
    pub cache_root: Option<PathBuf>,
    #[serde(default)]
    pub session_root: Option<PathBuf>,
    #[serde(default)]
    pub sandbox_root: Option<PathBuf>,
    #[serde(default)]
    pub bind_addr: Option<String>,
    #[serde(default)]
    pub sandbox_mode_override: Option<SandboxMode>,
    #[serde(default)]
    pub hub_url: Option<String>,
    #[serde(default)]
    pub worker_count: Option<usize>,
    #[serde(default)]
    pub queue_capacity: Option<usize>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub model_config: Option<Value>,
    #[serde(default)]
    pub default_model: Option<ModelSpec>,
    #[serde(default)]
    pub bundles: BTreeMap<String, BundleRuntimeConfig>,
}

impl RuntimeConfig {
    pub fn odyssey_root() -> Result<PathBuf, RuntimeError> {
        let dirs = BaseDirs::new().ok_or_else(|| {
            RuntimeError::Invalid("unable to determine default home directories".to_string())
        })?;
        Ok(dirs.home_dir().join(".odyssey"))
    }

    pub fn config_path() -> Result<PathBuf, RuntimeError> {
        Ok(Self::odyssey_root()?.join(CONFIG_FILE_NAME))
    }

    pub fn from_default_dirs() -> Self {
        Self::default()
    }

    pub fn load() -> Result<Self, RuntimeError> {
        Self::load_from_file(&Self::config_path()?)
    }

    pub fn load_for_path(_active_path: Option<&Path>) -> Result<Self, RuntimeError> {
        Self::load()
    }

    pub fn session_sandbox_for_agent(
        &self,
        namespace: &str,
        bundle_id: &str,
        bundle_version: &str,
        agent_id: &str,
        explicit: Option<&SessionSandboxOverlay>,
    ) -> Option<SessionSandboxOverlay> {
        let configured = self
            .agent_runtime_config(namespace, bundle_id, bundle_version, agent_id)
            .and_then(|config| config.sandbox.as_ref());
        SessionSandboxOverlay::merge(configured, explicit)
    }

    pub fn session_model_for_agent(
        &self,
        namespace: &str,
        bundle_id: &str,
        bundle_version: &str,
        agent_id: &str,
        bundle_model: &ModelSpec,
        explicit: Option<&ModelSpec>,
    ) -> ModelSpec {
        explicit.cloned().unwrap_or_else(|| {
            self.agent_runtime_config(namespace, bundle_id, bundle_version, agent_id)
                .and_then(|config| config.resolved_model(bundle_model))
                .unwrap_or_else(|| bundle_model.clone())
        })
    }

    fn agent_runtime_config(
        &self,
        namespace: &str,
        bundle_id: &str,
        bundle_version: &str,
        agent_id: &str,
    ) -> Option<&AgentRuntimeConfig> {
        bundle_selector_keys(namespace, bundle_id, bundle_version)
            .into_iter()
            .find_map(|selector| {
                self.bundle_overrides
                    .get(&selector)
                    .and_then(|bundle| bundle.agents.get(agent_id))
            })
    }

    fn load_from_file(config_path: &Path) -> Result<Self, RuntimeError> {
        let mut config = Self::from_default_dirs();
        if !config_path.exists() {
            return Ok(config);
        }
        let raw = fs::read_to_string(config_path).map_err(|err| RuntimeError::Io {
            path: config_path.display().to_string(),
            message: err.to_string(),
        })?;
        let parsed: RuntimeConfigFile = serde_yaml::from_str(&raw).map_err(|err| {
            RuntimeError::Invalid(format!(
                "invalid runtime config `{}`: {err}",
                config_path.display()
            ))
        })?;
        config.apply_file_config(parsed);
        Ok(config)
    }

    fn apply_file_config(&mut self, file: RuntimeConfigFile) {
        if let Some(path) = file.cache_root {
            self.cache_root = path;
        }
        if let Some(path) = file.session_root {
            self.session_root = path;
        }
        if let Some(path) = file.sandbox_root {
            self.sandbox_root = path;
        }
        if let Some(bind_addr) = file.bind_addr {
            self.bind_addr = bind_addr;
        }
        if let Some(mode) = file.sandbox_mode_override {
            self.sandbox_mode_override = Some(mode);
        }
        if let Some(hub_url) = file.hub_url {
            self.hub_url = hub_url;
        }
        if let Some(worker_count) = file.worker_count {
            self.worker_count = worker_count;
        }
        if let Some(queue_capacity) = file.queue_capacity {
            self.queue_capacity = queue_capacity;
        }
        if let Some(default_model) = file.default_model {
            self.default_model = default_model;
        } else if file.model.is_some()
            || file.model_provider.is_some()
            || file.model_config.is_some()
        {
            self.default_model = ModelSpec {
                provider: file.model_provider.unwrap_or_else(|| "openai".to_string()),
                name: file
                    .model
                    .unwrap_or_else(|| self.default_model.name.clone()),
                config: file.model_config,
            };
        }
        self.bundle_overrides = file.bundles;
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let root = Self::odyssey_root().unwrap_or_else(|_| PathBuf::from(".odyssey"));
        Self {
            cache_root: root.join("bundles"),
            session_root: root.join("sessions"),
            sandbox_root: root.join("sandbox"),
            bind_addr: DEFAULT_RUNTIME_BIND_ADDR.to_string(),
            sandbox_mode_override: None,
            hub_url: DEFAULT_HUB_URL.to_string(),
            worker_count: DEFAULT_WORKER_COUNT,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            default_model: default_model(),
            bundle_overrides: BTreeMap::new(),
        }
    }
}

fn default_model() -> ModelSpec {
    ModelSpec {
        provider: "openai".to_string(),
        name: "gpt-4.1-mini".to_string(),
        config: None,
    }
}

fn bundle_selector_keys(namespace: &str, bundle_id: &str, bundle_version: &str) -> Vec<String> {
    let mut keys = Vec::new();
    push_unique(
        &mut keys,
        format!("{namespace}/{bundle_id}@{bundle_version}"),
    );
    push_unique(
        &mut keys,
        format!("{namespace}/{bundle_id}@{LATEST_SELECTOR}"),
    );
    push_unique(&mut keys, format!("{namespace}/{bundle_id}"));
    if namespace == LOCAL_NAMESPACE {
        push_unique(&mut keys, format!("{bundle_id}@{bundle_version}"));
        push_unique(&mut keys, format!("{bundle_id}@{LATEST_SELECTOR}"));
        push_unique(&mut keys, bundle_id.to_string());
    }
    keys
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|candidate| candidate == &value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRuntimeConfig, BundleRuntimeConfig, CONFIG_FILE_NAME, DEFAULT_QUEUE_CAPACITY,
        DEFAULT_WORKER_COUNT, RuntimeConfig, RuntimeConfigFile, bundle_selector_keys,
    };
    use odyssey_rs_protocol::{
        DEFAULT_HUB_URL, DEFAULT_RUNTIME_BIND_ADDR, ModelSpec, SandboxMode,
        SessionSandboxFilesystem, SessionSandboxMounts, SessionSandboxOverlay,
        SessionSandboxPermissions,
    };
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    const TEST_SANDBOX_WRITE_PATH: &str = "/odyssey-test/output";

    #[test]
    fn default_dirs_point_into_odyssey_home() {
        let config = RuntimeConfig::from_default_dirs();

        assert!(config.cache_root.ends_with(".odyssey/bundles"));
        assert!(config.session_root.ends_with(".odyssey/sessions"));
        assert!(config.sandbox_root.ends_with(".odyssey/sandbox"));
        assert_eq!(config.bind_addr, DEFAULT_RUNTIME_BIND_ADDR);
        assert_eq!(config.hub_url, DEFAULT_HUB_URL);
        assert_eq!(config.worker_count, DEFAULT_WORKER_COUNT);
        assert_eq!(config.queue_capacity, DEFAULT_QUEUE_CAPACITY);
        assert!(config.sandbox_mode_override.is_none());
        assert_eq!(config.default_model.provider, "openai");
        assert_eq!(config.default_model.name, "gpt-4.1-mini");
    }

    #[test]
    fn default_impl_matches_default_dirs() {
        let config = RuntimeConfig::default();
        let from_dirs = RuntimeConfig::from_default_dirs();

        assert_eq!(config.cache_root, from_dirs.cache_root);
        assert_eq!(config.session_root, from_dirs.session_root);
        assert_eq!(config.sandbox_root, from_dirs.sandbox_root);
        assert_eq!(config.bind_addr, from_dirs.bind_addr);
        assert_eq!(config.hub_url, from_dirs.hub_url);
        assert_eq!(config.worker_count, from_dirs.worker_count);
        assert_eq!(config.queue_capacity, from_dirs.queue_capacity);
        assert_eq!(
            config.default_model.provider,
            from_dirs.default_model.provider
        );
        assert_eq!(config.default_model.name, from_dirs.default_model.name);
    }

    #[test]
    fn load_from_file_applies_runtime_and_bundle_agent_overrides() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join(CONFIG_FILE_NAME);
        fs::write(
            &config_path,
            r#"
hub_url: https://hub.example.com
model: gpt-5.4
model_config:
  reasoning_effort: high
"#,
        )
        .expect("write config");
        let mut parsed = RuntimeConfig::load_from_file(&config_path).expect("load config");
        assert_eq!(parsed.hub_url, "https://hub.example.com");
        assert_eq!(parsed.default_model.name, "gpt-5.4");
        assert_eq!(
            parsed.default_model.config,
            Some(serde_json::json!({ "reasoning_effort": "high" }))
        );
        assert!(parsed.bundle_overrides.is_empty());

        let config = RuntimeConfigFile {
            bundles: BTreeMap::from([(
                "odyssey-agent@latest".to_string(),
                BundleRuntimeConfig {
                    agents: BTreeMap::from([(
                        "odyssey-agent".to_string(),
                        AgentRuntimeConfig {
                            model: Some("gpt-5.4".to_string()),
                            model_provider: Some("openai".to_string()),
                            model_config: Some(serde_json::json!({
                                "reasoning_effort": "high"
                            })),
                            sandbox: Some(SessionSandboxOverlay {
                                mode: Some(SandboxMode::WorkspaceWrite),
                                permissions: SessionSandboxPermissions {
                                    filesystem: SessionSandboxFilesystem {
                                        exec: vec!["/opt/bin".to_string()],
                                        mounts: SessionSandboxMounts {
                                            read: vec![".".to_string()],
                                            write: vec![TEST_SANDBOX_WRITE_PATH.to_string()],
                                        },
                                    },
                                },
                                env: BTreeMap::new(),
                                system_tools: vec!["git".to_string()],
                            }),
                        },
                    )]),
                },
            )]),
            ..RuntimeConfigFile::default()
        };
        fs::write(&config_path, serde_yaml::to_string(&config).expect("yaml")).expect("write");
        parsed = RuntimeConfig::load_from_file(&config_path).expect("reload config");

        assert!(parsed.bundle_overrides.contains_key("odyssey-agent@latest"));
    }

    #[test]
    fn session_sandbox_for_agent_merges_configured_and_explicit_overrides() {
        let mut config = RuntimeConfig::default();
        config.bundle_overrides.insert(
            "odyssey-agent@latest".to_string(),
            BundleRuntimeConfig {
                agents: BTreeMap::from([(
                    "odyssey-agent".to_string(),
                    AgentRuntimeConfig {
                        model: None,
                        model_provider: None,
                        model_config: None,
                        sandbox: Some(SessionSandboxOverlay {
                            mode: Some(SandboxMode::ReadOnly),
                            permissions: SessionSandboxPermissions {
                                filesystem: SessionSandboxFilesystem {
                                    exec: vec!["/usr/bin".to_string()],
                                    mounts: SessionSandboxMounts {
                                        read: vec!["/data".to_string()],
                                        write: Vec::new(),
                                    },
                                },
                            },
                            env: BTreeMap::from([("TOKEN".to_string(), "BASE_TOKEN".to_string())]),
                            system_tools: vec!["git".to_string()],
                        }),
                    },
                )]),
            },
        );
        let explicit = SessionSandboxOverlay {
            mode: None,
            permissions: SessionSandboxPermissions {
                filesystem: SessionSandboxFilesystem {
                    exec: vec!["/opt/bin".to_string()],
                    mounts: SessionSandboxMounts {
                        read: vec!["/workspace".to_string()],
                        write: vec![TEST_SANDBOX_WRITE_PATH.to_string()],
                    },
                },
            },
            env: BTreeMap::from([("TOKEN".to_string(), "EXPLICIT_TOKEN".to_string())]),
            system_tools: vec!["python3".to_string()],
        };

        let merged = config
            .session_sandbox_for_agent(
                "local",
                "odyssey-agent",
                "0.1.0",
                "odyssey-agent",
                Some(&explicit),
            )
            .expect("merged");

        assert_eq!(merged.mode, Some(SandboxMode::ReadOnly));
        assert_eq!(
            merged.permissions.filesystem.mounts.read,
            vec!["/data".to_string(), "/workspace".to_string()]
        );
        assert_eq!(
            merged.permissions.filesystem.mounts.write,
            vec![TEST_SANDBOX_WRITE_PATH.to_string()]
        );
        assert_eq!(merged.env.get("TOKEN"), Some(&"EXPLICIT_TOKEN".to_string()));
    }

    #[test]
    fn session_model_for_agent_prefers_exact_bundle_selector_and_explicit_override() {
        let mut config = RuntimeConfig::default();
        config.bundle_overrides.insert(
            "hello-world@latest".to_string(),
            BundleRuntimeConfig {
                agents: BTreeMap::from([(
                    "hello-world".to_string(),
                    AgentRuntimeConfig {
                        model: Some("gpt-5.4".to_string()),
                        model_provider: Some("openai".to_string()),
                        model_config: Some(serde_json::json!({ "reasoning_effort": "medium" })),
                        sandbox: None,
                    },
                )]),
            },
        );
        config.bundle_overrides.insert(
            "hello-world@0.2.0".to_string(),
            BundleRuntimeConfig {
                agents: BTreeMap::from([(
                    "hello-world".to_string(),
                    AgentRuntimeConfig {
                        model: Some("o3".to_string()),
                        model_provider: Some("openai".to_string()),
                        model_config: Some(serde_json::json!({ "reasoning_effort": "high" })),
                        sandbox: None,
                    },
                )]),
            },
        );
        let bundle_model = ModelSpec {
            provider: "openai".to_string(),
            name: "gpt-4.1-mini".to_string(),
            config: None,
        };

        let selected = config.session_model_for_agent(
            "local",
            "hello-world",
            "0.2.0",
            "hello-world",
            &bundle_model,
            None,
        );
        assert_eq!(selected.provider, "openai");
        assert_eq!(selected.name, "o3");
        assert_eq!(
            selected.config,
            Some(serde_json::json!({ "reasoning_effort": "high" }))
        );

        let explicit = ModelSpec {
            provider: "anthropic".to_string(),
            name: "claude-sonnet-4-5".to_string(),
            config: Some(serde_json::json!({ "max_tokens": 4096 })),
        };
        let selected = config.session_model_for_agent(
            "local",
            "hello-world",
            "0.2.0",
            "hello-world",
            &bundle_model,
            Some(&explicit),
        );
        assert_eq!(selected, explicit);
    }

    #[test]
    fn bundle_selector_keys_cover_exact_and_local_latest_aliases() {
        assert_eq!(
            bundle_selector_keys("local", "hello-world", "0.2.0"),
            vec![
                "local/hello-world@0.2.0".to_string(),
                "local/hello-world@latest".to_string(),
                "local/hello-world".to_string(),
                "hello-world@0.2.0".to_string(),
                "hello-world@latest".to_string(),
                "hello-world".to_string(),
            ]
        );
        assert_eq!(
            bundle_selector_keys("team", "hello-world", "0.2.0"),
            vec![
                "team/hello-world@0.2.0".to_string(),
                "team/hello-world@latest".to_string(),
                "team/hello-world".to_string(),
            ]
        );
    }
}
