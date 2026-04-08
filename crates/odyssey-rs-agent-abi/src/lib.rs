use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

pub const WIT_PATH: &str = "wit/odyssey-agent.wit";
pub const WORLD_NAME: &str = "odyssey-agent-world";
pub const ABI_VERSION: &str = "v3";
pub const RUNNER_CLASS: &str = "wasm-component";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentDescriptor {
    pub id: String,
    pub abi_version: String,
    pub runner_class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRequest {
    pub session_id: String,
    pub turn_id: String,
    pub prompt: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub history_json: Option<String>,
    #[serde(default)]
    pub metadata_json: Option<String>,
    #[serde(default)]
    pub host_tools: Vec<HostToolSpec>,
}

impl RunRequest {
    pub fn metadata<T>(&self) -> serde_json::Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        self.metadata_json
            .as_deref()
            .map(string_to_json)
            .transpose()
    }

    pub fn host_tool(&self, name: &str) -> Option<&HostToolSpec> {
        self.host_tools.iter().find(|tool| tool.name == name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResponse {
    pub output_json: String,
}

impl RunResponse {
    pub fn text(output: impl Into<String>) -> Self {
        Self {
            output_json: serde_json::to_string(&Value::String(output.into()))
                .unwrap_or_else(|_| "\"\"".to_string()),
        }
    }

    pub fn json(value: &Value) -> serde_json::Result<Self> {
        Ok(Self {
            output_json: serde_json::to_string(value)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmChatRequest {
    pub messages_json: String,
    #[serde(default)]
    pub tools_json: Option<String>,
    #[serde(default)]
    pub output_schema_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmChatResponse {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub tool_calls_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostToolSpec {
    pub name: String,
    pub description: String,
    pub args_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostToolCallRequest {
    pub tool: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostToolCallResponse {
    pub result_json: String,
}

pub fn json_to_string<T>(value: &T) -> serde_json::Result<String>
where
    T: Serialize,
{
    serde_json::to_string(value)
}

pub fn string_to_json<T>(value: &str) -> serde_json::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(value)
}

#[cfg(test)]
mod tests {
    use super::{HostToolCallRequest, HostToolSpec, RunRequest, RunResponse};
    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde_json::json;

    #[test]
    fn run_request_round_trips() {
        let request = RunRequest {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            prompt: "hello".to_string(),
            system_prompt: Some("be concise".to_string()),
            history_json: Some("[{\"role\":\"user\",\"content\":\"hi\"}]".to_string()),
            metadata_json: Some("{\"bundle_id\":\"demo\"}".to_string()),
            host_tools: vec![HostToolSpec {
                name: "Read".to_string(),
                description: "Read a text file".to_string(),
                args_schema: json!({
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string"}
                    }
                }),
            }],
        };

        let encoded = serde_json::to_value(&request).expect("serialize");
        let decoded: RunRequest = serde_json::from_value(encoded.clone()).expect("deserialize");

        assert_eq!(
            serde_json::to_value(decoded).expect("serialize decoded"),
            encoded
        );
    }

    #[test]
    fn run_response_text_serializes_as_json_string() {
        let response = RunResponse::text("hello");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.output_json).expect("json string"),
            json!("hello")
        );
    }

    #[test]
    fn tool_call_request_round_trips() {
        let request = HostToolCallRequest {
            tool: "Read".to_string(),
            arguments_json: "{\"path\":\"README.md\"}".to_string(),
        };

        let encoded = serde_json::to_value(&request).expect("serialize");
        let decoded: HostToolCallRequest =
            serde_json::from_value(encoded.clone()).expect("deserialize");

        assert_eq!(
            serde_json::to_value(decoded).expect("serialize decoded"),
            encoded
        );
    }

    #[test]
    fn metadata_deserializes_into_typed_value() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Metadata {
            bundle_id: String,
        }

        let request = RunRequest {
            session_id: "session".to_string(),
            turn_id: "turn".to_string(),
            prompt: "hello".to_string(),
            system_prompt: None,
            history_json: None,
            metadata_json: Some("{\"bundle_id\":\"demo\"}".to_string()),
            host_tools: Vec::new(),
        };

        assert_eq!(
            request.metadata::<Metadata>().expect("typed metadata"),
            Some(Metadata {
                bundle_id: "demo".to_string(),
            })
        );
    }
}
