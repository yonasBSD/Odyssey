use anyhow::{Result, anyhow};
use odyssey_rs_protocol::{Session, SessionSpec, SessionSummary, Task};
use odyssey_rs_runtime::RunOutput;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct RemoteRuntimeClient {
    client: reqwest::Client,
    base_url: String,
}

impl RemoteRuntimeClient {
    pub(crate) fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(anyhow!("remote URL cannot be empty"));
        }
        Ok(Self {
            client: reqwest::Client::new(),
            base_url,
        })
    }

    pub(crate) async fn inspect(
        &self,
        reference: &str,
    ) -> Result<odyssey_rs_bundle::BundleMetadata> {
        self.get(&format!("/bundles/inspect?reference={reference}"))
            .await
    }

    pub(crate) async fn run(&self, bundle_ref: String, input: String) -> Result<RunOutput> {
        let session = self.create_session(SessionSpec::from(bundle_ref)).await?;
        self.post(
            &format!("/sessions/{}/run-sync", session.id),
            serde_json::json!({
                "input": Task::new(input)
            }),
        )
        .await
    }

    pub(crate) async fn create_session(&self, spec: SessionSpec) -> Result<SessionSummary> {
        self.post("/sessions", serde_json::to_value(spec)?).await
    }

    pub(crate) async fn pull(
        &self,
        reference: &str,
        hub_url: &str,
    ) -> Result<odyssey_rs_bundle::BundleInstall> {
        self.post(
            "/bundles/pull",
            serde_json::json!({
                "reference": reference,
                "hub_url": hub_url
            }),
        )
        .await
    }

    pub(crate) async fn list_bundles(
        &self,
    ) -> Result<Vec<odyssey_rs_bundle::BundleInstallSummary>> {
        self.get("/bundles").await
    }

    pub(crate) async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        self.get("/sessions").await
    }

    pub(crate) async fn get_session(&self, id: Uuid) -> Result<Session> {
        self.get(&format!("/sessions/{id}")).await
    }

    pub(crate) async fn delete_session(&self, id: Uuid) -> Result<()> {
        let response = self
            .client
            .delete(self.url(&format!("/sessions/{id}")))
            .send()
            .await?;
        if !response.status().is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(anyhow!("remote request failed: {message}"));
        }
        Ok(())
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let response = self.client.get(self.url(path)).send().await?;
        Self::parse_json(response).await
    }

    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        let response = self.client.post(self.url(path)).json(&body).send().await?;
        Self::parse_json(response).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn parse_json<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(anyhow!("remote request failed ({status}): {message}"));
        }
        Ok(response.json::<T>().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteRuntimeClient;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct RemoteState {
        requests: Arc<Mutex<Vec<(String, Value)>>>,
    }

    async fn spawn_remote() -> (RemoteRuntimeClient, RemoteState) {
        let state = RemoteState::default();
        let app = Router::new()
            .route("/sessions", get(list_sessions).post(create_session))
            .route("/sessions/{id}", get(get_session).delete(delete_session))
            .route("/sessions/{id}/run-sync", post(run_sync))
            .route("/bundles", get(list_bundles))
            .route("/fail", get(fail_request))
            .route("/malformed", get(malformed_json))
            .with_state(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test remote");
        });

        let client =
            RemoteRuntimeClient::new(format!("http://{addr}/")).expect("construct remote client");
        (client, state)
    }

    #[derive(serde::Deserialize)]
    struct RunSyncBody {
        input: odyssey_rs_protocol::Task,
    }

    async fn create_session(
        State(state): State<RemoteState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        state
            .requests
            .lock()
            .expect("lock requests")
            .push(("create_session".to_string(), body));
        Json(json!({
            "id": Uuid::nil(),
            "agent_id": "alpha",
            "message_count": 0,
            "created_at": "2026-04-10T00:00:00Z"
        }))
    }

    async fn list_sessions() -> Json<Value> {
        Json(json!([{
            "id": Uuid::nil(),
            "agent_id": "alpha",
            "message_count": 2,
            "created_at": "2026-04-10T00:00:00Z"
        }]))
    }

    async fn get_session(Path(id): Path<Uuid>) -> Json<Value> {
        Json(json!({
            "id": id,
            "agent_id": "alpha",
            "bundle_ref": "local/demo@0.1.0",
            "model_id": "openai/gpt-4.1-mini",
            "sandbox": null,
            "created_at": "2026-04-10T00:00:00Z",
            "messages": [{
                "role": "user",
                "content": "hello"
            }]
        }))
    }

    async fn delete_session(Path(id): Path<Uuid>) -> (StatusCode, String) {
        if id == Uuid::max() {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "delete failed".to_string(),
            )
        } else {
            (StatusCode::NO_CONTENT, String::default())
        }
    }

    async fn run_sync(
        State(state): State<RemoteState>,
        Path(id): Path<Uuid>,
        Json(body): Json<RunSyncBody>,
    ) -> Json<Value> {
        state.requests.lock().expect("lock requests").push((
            "run_sync".to_string(),
            json!({
                "session_id": id,
                "prompt": body.input.prompt,
                "system_prompt": body.input.system_prompt
            }),
        ));
        Json(json!({
            "session_id": id,
            "turn_id": Uuid::from_u128(1),
            "response": "remote output"
        }))
    }

    async fn list_bundles() -> Json<Value> {
        Json(json!([{
            "namespace": "team",
            "id": "demo",
            "version": "1.2.3",
            "path": "/bundles/team/demo/1.2.3"
        }]))
    }

    async fn fail_request() -> (StatusCode, &'static str) {
        (StatusCode::BAD_GATEWAY, "upstream unavailable")
    }

    async fn malformed_json() -> &'static str {
        "not-json"
    }

    #[test]
    fn new_rejects_empty_urls_and_normalizes_base_paths() {
        let error = RemoteRuntimeClient::new("   ")
            .err()
            .expect("empty URL rejected");
        assert_eq!(error.to_string(), "remote URL cannot be empty");

        let client = RemoteRuntimeClient::new("http://127.0.0.1:9999///").expect("client");
        assert_eq!(client.base_url, "http://127.0.0.1:9999");
        assert_eq!(client.url("/sessions"), "http://127.0.0.1:9999/sessions");
    }

    #[tokio::test]
    async fn client_executes_session_requests_and_parses_success_responses() {
        let (client, state) = spawn_remote().await;

        let session = client
            .create_session(odyssey_rs_protocol::SessionSpec::from("local/demo@0.1.0"))
            .await
            .expect("create session");
        assert_eq!(session.id, Uuid::nil());
        assert_eq!(session.agent_id, "alpha");

        let output = client
            .run("local/demo@0.1.0".to_string(), "hello remote".to_string())
            .await
            .expect("run session");
        assert_eq!(output.session_id, Uuid::nil());
        assert_eq!(output.response, "remote output");

        let bundles = client.list_bundles().await.expect("list bundles");
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].namespace, "team");
        assert_eq!(bundles[0].id, "demo");

        let sessions = client.list_sessions().await.expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].message_count, 2);

        let full_session = client
            .get_session(Uuid::from_u128(42))
            .await
            .expect("get session");
        assert_eq!(full_session.id, Uuid::from_u128(42));
        assert_eq!(full_session.bundle_ref, "local/demo@0.1.0");
        assert_eq!(full_session.messages.len(), 1);
        assert_eq!(full_session.messages[0].content, "hello");

        client
            .delete_session(Uuid::from_u128(42))
            .await
            .expect("delete session");

        let requests = state.requests.lock().expect("lock requests");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].0, "create_session");
        assert_eq!(
            requests[0].1,
            json!({
                "bundle_ref": { "reference": "local/demo@0.1.0" },
                "agent_id": null,
                "model": null,
                "sandbox": null,
                "metadata": {}
            })
        );
        assert_eq!(requests[1].0, "create_session");
        assert_eq!(requests[2].0, "run_sync");
        assert_eq!(
            requests[2].1,
            json!({
                "session_id": Uuid::nil(),
                "prompt": "hello remote",
                "system_prompt": null
            })
        );
    }

    #[tokio::test]
    async fn client_surfaces_remote_status_and_decode_errors() {
        let (client, _) = spawn_remote().await;

        let status_error = client
            .get::<Value>("/fail")
            .await
            .expect_err("non-success status should fail");
        assert_eq!(
            status_error.to_string(),
            "remote request failed (502 Bad Gateway): upstream unavailable"
        );

        let decode_error = client
            .get::<Value>("/malformed")
            .await
            .expect_err("invalid JSON should fail");
        assert!(
            decode_error
                .to_string()
                .contains("error decoding response body"),
            "unexpected decode error: {decode_error}"
        );

        let delete_error = client
            .delete_session(Uuid::max())
            .await
            .expect_err("delete failure should surface");
        assert_eq!(
            delete_error.to_string(),
            "remote request failed: delete failed"
        );
    }
}
