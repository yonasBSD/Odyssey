use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use autoagents_core::tool::{ToolCallError, ToolRuntime, ToolT};
use odyssey_rs_agent_abi::{HostToolSpec, RunRequest};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{AgentResult, AgentSdkError, host};

#[derive(Clone, Debug, Default)]
pub struct HostToolCatalog {
    specs: Arc<Vec<HostToolSpec>>,
}

impl HostToolCatalog {
    pub fn from_request(request: &RunRequest) -> Self {
        Self {
            specs: Arc::new(request.host_tools.clone()),
        }
    }

    pub fn specs(&self) -> &[HostToolSpec] {
        self.specs.as_slice()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.spec(name).is_some()
    }

    pub fn spec(&self, name: &str) -> Option<&HostToolSpec> {
        self.specs.iter().find(|tool| tool.name == name)
    }

    pub fn tool(&self, name: &str) -> Option<DynamicHostTool> {
        self.spec(name).cloned().map(DynamicHostTool::new)
    }

    pub fn tools(&self) -> Vec<Arc<dyn ToolT>> {
        self.specs
            .iter()
            .cloned()
            .map(|spec| Arc::new(DynamicHostTool::new(spec)) as Arc<dyn ToolT>)
            .collect()
    }

    pub fn read(&self) -> Option<TypedHostTool<ReadArgs>> {
        self.typed("Read")
    }

    pub fn write(&self) -> Option<TypedHostTool<WriteArgs>> {
        self.typed("Write")
    }

    pub fn edit(&self) -> Option<TypedHostTool<EditArgs>> {
        self.typed("Edit")
    }

    pub fn ls(&self) -> Option<TypedHostTool<LsArgs>> {
        self.typed("LS")
    }

    pub fn glob(&self) -> Option<TypedHostTool<GlobArgs>> {
        self.typed("Glob")
    }

    pub fn grep(&self) -> Option<TypedHostTool<GrepArgs>> {
        self.typed("Grep")
    }

    pub fn skill(&self) -> Option<TypedHostTool<SkillArgs>> {
        self.typed("Skill")
    }

    pub fn bash(&self) -> Option<TypedHostTool<BashArgs>> {
        self.typed("Bash")
    }

    fn typed<Args>(&self, name: &str) -> Option<TypedHostTool<Args>> {
        self.spec(name).cloned().map(TypedHostTool::new)
    }
}

#[derive(Clone, Debug)]
pub struct DynamicHostTool {
    spec: HostToolSpec,
}

impl DynamicHostTool {
    pub fn new(spec: HostToolSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &HostToolSpec {
        &self.spec
    }

    pub async fn call(&self, args: Value) -> AgentResult<Value> {
        host::call_tool(&self.spec.name, args).await
    }
}

#[async_trait]
impl ToolRuntime for DynamicHostTool {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        self.call(args)
            .await
            .map_err(|err| ToolCallError::RuntimeError(Box::new(err)))
    }
}

impl ToolT for DynamicHostTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn args_schema(&self) -> Value {
        self.spec.args_schema.clone()
    }

    fn output_schema(&self) -> Option<Value> {
        self.spec.output_schema.clone()
    }
}

pub struct TypedHostTool<Args> {
    inner: DynamicHostTool,
    _marker: PhantomData<fn(Args)>,
}

impl<Args> Clone for TypedHostTool<Args> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

impl<Args> std::fmt::Debug for TypedHostTool<Args> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedHostTool")
            .field("name", &self.inner.name())
            .field("description", &self.inner.description())
            .finish()
    }
}

impl<Args> TypedHostTool<Args> {
    pub fn new(spec: HostToolSpec) -> Self {
        Self {
            inner: DynamicHostTool::new(spec),
            _marker: PhantomData,
        }
    }

    pub fn spec(&self) -> &HostToolSpec {
        self.inner.spec()
    }
}

impl<Args> TypedHostTool<Args>
where
    Args: Serialize,
{
    pub async fn call_value(&self, args: Args) -> AgentResult<Value> {
        let value = serde_json::to_value(args)
            .map_err(|err| AgentSdkError::InvalidRequest(err.to_string()))?;
        self.inner.call(value).await
    }

    pub async fn call<R>(&self, args: Args) -> AgentResult<R>
    where
        R: DeserializeOwned,
    {
        let value = self.call_value(args).await?;
        serde_json::from_value(value).map_err(|err| AgentSdkError::InvalidResponse(err.to_string()))
    }
}

#[async_trait]
impl<Args> ToolRuntime for TypedHostTool<Args>
where
    Args: Send + Sync + 'static,
{
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        self.inner.execute(args).await
    }
}

impl<Args> ToolT for TypedHostTool<Args>
where
    Args: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn args_schema(&self) -> Value {
        self.inner.args_schema()
    }

    fn output_schema(&self) -> Option<Value> {
        self.inner.output_schema()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadArgs {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditArgs {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LsArgs {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlobArgs {
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrepArgs {
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillArgs {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BashArgs {
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{DynamicHostTool, HostToolCatalog, HostToolSpec, ReadArgs, RunRequest};
    use autoagents_core::tool::{ToolCallError, ToolRuntime, ToolT};
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serde_json::json;

    fn request() -> RunRequest {
        RunRequest {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            prompt: "hello".to_string(),
            system_prompt: None,
            history_json: None,
            metadata_json: None,
            host_tools: vec![
                HostToolSpec {
                    name: "Read".to_string(),
                    description: "Read a text file".to_string(),
                    args_schema: json!({
                        "type": "object",
                        "required": ["path"],
                        "properties": {
                            "path": {"type": "string"}
                        }
                    }),
                    output_schema: Some(json!({
                        "type": "object",
                        "required": ["path", "content"],
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        }
                    })),
                },
                HostToolSpec {
                    name: "Bash".to_string(),
                    description: "Run a sandboxed shell command".to_string(),
                    args_schema: json!({
                        "type": "object",
                        "required": ["command"],
                        "properties": {
                            "command": {"type": "string"},
                            "cwd": {"type": "string"}
                        }
                    }),
                    output_schema: Some(json!({
                        "type": "object",
                        "required": ["status_code", "stdout", "stderr", "stdout_truncated", "stderr_truncated"],
                        "properties": {
                            "status_code": {"anyOf": [{"type": "integer"}, {"type": "null"}]},
                            "stdout": {"type": "string"},
                            "stderr": {"type": "string"},
                            "stdout_truncated": {"type": "boolean"},
                            "stderr_truncated": {"type": "boolean"}
                        }
                    })),
                },
            ],
        }
    }

    fn full_request() -> RunRequest {
        RunRequest {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            prompt: "hello".to_string(),
            system_prompt: None,
            history_json: None,
            metadata_json: None,
            host_tools: vec![
                HostToolSpec {
                    name: "Read".to_string(),
                    description: "Read".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Write".to_string(),
                    description: "Write".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Edit".to_string(),
                    description: "Edit".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "LS".to_string(),
                    description: "List".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Glob".to_string(),
                    description: "Glob".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Grep".to_string(),
                    description: "Grep".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Skill".to_string(),
                    description: "Skill".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
                HostToolSpec {
                    name: "Bash".to_string(),
                    description: "Bash".to_string(),
                    args_schema: json!({"type": "object"}),
                    output_schema: None,
                },
            ],
        }
    }

    struct FailingArgs;

    impl Serialize for FailingArgs {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("serialize boom"))
        }
    }

    #[test]
    fn catalog_exposes_runtime_selected_tools() {
        let catalog = HostToolCatalog::from_request(&request());

        assert!(catalog.contains("Read"));
        assert!(catalog.contains("Bash"));
        assert!(!catalog.contains("Write"));
        assert_eq!(catalog.tools().len(), 2);
    }

    #[test]
    fn typed_accessors_follow_catalog_membership() {
        let catalog = HostToolCatalog::from_request(&request());

        let read = catalog.read().expect("read tool");
        assert_eq!(read.name(), "Read");
        assert_eq!(
            read.args_schema(),
            json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"}
                }
            })
        );
        assert_eq!(
            read.output_schema(),
            Some(json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }))
        );
        assert!(catalog.write().is_none());

        let _ = ReadArgs {
            path: "README.md".to_string(),
        };
    }

    #[test]
    fn dynamic_tool_reflects_runtime_spec() {
        let spec = request().host_tools.into_iter().next().expect("tool spec");
        let tool = DynamicHostTool::new(spec.clone());

        assert_eq!(tool.name(), "Read");
        assert_eq!(tool.description(), "Read a text file");
        assert_eq!(tool.args_schema(), spec.args_schema);
        assert_eq!(tool.output_schema(), spec.output_schema);
    }

    #[test]
    fn standard_accessors_return_matching_typed_tools_when_present() {
        let catalog = HostToolCatalog::from_request(&full_request());

        assert_eq!(catalog.read().expect("Read").name(), "Read");
        assert_eq!(catalog.write().expect("Write").name(), "Write");
        assert_eq!(catalog.edit().expect("Edit").name(), "Edit");
        assert_eq!(catalog.ls().expect("LS").name(), "LS");
        assert_eq!(catalog.glob().expect("Glob").name(), "Glob");
        assert_eq!(catalog.grep().expect("Grep").name(), "Grep");
        assert_eq!(catalog.skill().expect("Skill").name(), "Skill");
        assert_eq!(catalog.bash().expect("Bash").name(), "Bash");
        assert!(catalog.tool("Missing").is_none());
    }

    #[test]
    fn spec_and_tool_lookups_preserve_runtime_metadata() {
        let catalog = HostToolCatalog::from_request(&full_request());

        let spec = catalog.spec("Read").expect("spec lookup");
        let dynamic = catalog.tool("Read").expect("dynamic tool");
        let typed = catalog.read().expect("typed tool");

        assert_eq!(dynamic.spec(), spec);
        assert_eq!(typed.spec(), spec);
        assert_eq!(
            format!("{typed:?}"),
            r#"TypedHostTool { name: "Read", description: "Read" }"#
        );
    }

    #[tokio::test]
    async fn host_tool_calls_surface_unsupported_host_errors_outside_wasm() {
        let catalog = HostToolCatalog::from_request(&request());
        let dynamic = catalog.tool("Read").expect("dynamic tool");
        let typed = catalog.read().expect("typed tool");

        let dynamic_error = dynamic
            .call(json!({ "path": "README.md" }))
            .await
            .expect_err("dynamic call should fail outside wasm");
        assert_eq!(
            dynamic_error.to_string(),
            "unsupported outside wasm agent execution"
        );

        let typed_error = typed
            .call_value(ReadArgs {
                path: "README.md".to_string(),
            })
            .await
            .expect_err("typed call should fail outside wasm");
        assert_eq!(
            typed_error.to_string(),
            "unsupported outside wasm agent execution"
        );

        let typed_result_error = typed
            .call::<serde_json::Value>(ReadArgs {
                path: "README.md".to_string(),
            })
            .await
            .expect_err("typed result call should fail outside wasm");
        assert_eq!(
            typed_result_error.to_string(),
            "unsupported outside wasm agent execution"
        );
    }

    #[tokio::test]
    async fn typed_host_tools_reject_unserializable_arguments() {
        let spec = HostToolSpec {
            name: "Read".to_string(),
            description: "Read".to_string(),
            args_schema: json!({"type": "object"}),
            output_schema: None,
        };
        let tool = super::TypedHostTool::<FailingArgs>::new(spec);

        let error = tool
            .call_value(FailingArgs)
            .await
            .expect_err("serialization failure should surface");
        assert_eq!(error.to_string(), "invalid request payload: serialize boom");
    }

    #[tokio::test]
    async fn tool_runtime_execute_wraps_host_failures() {
        let catalog = HostToolCatalog::from_request(&request());
        let dynamic = catalog.tool("Read").expect("dynamic tool");
        let typed = catalog.read().expect("typed tool");

        let dynamic_error = dynamic
            .execute(json!({ "path": "README.md" }))
            .await
            .expect_err("dynamic runtime call should fail outside wasm");
        let typed_error = typed
            .execute(json!({ "path": "README.md" }))
            .await
            .expect_err("typed runtime call should fail outside wasm");

        match dynamic_error {
            ToolCallError::RuntimeError(error) => {
                assert_eq!(
                    error.to_string(),
                    "unsupported outside wasm agent execution"
                );
            }
            other => panic!("unexpected dynamic error: {other}"),
        }

        match typed_error {
            ToolCallError::RuntimeError(error) => {
                assert_eq!(
                    error.to_string(),
                    "unsupported outside wasm agent execution"
                );
            }
            other => panic!("unexpected typed error: {other}"),
        }
    }
}
