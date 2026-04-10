use crate::routes::router;
use axum::Router;
use odyssey_rs_bundle::BundleStore;
use odyssey_rs_runtime::{OdysseyRuntime, RuntimeConfig, RuntimeError};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<OdysseyRuntime>,
    pub bundles: BundleStore,
    pub hub_url: String,
}

pub fn build_app(runtime: Arc<OdysseyRuntime>) -> Router {
    let state = AppState {
        bundles: runtime.bundle_store(),
        hub_url: runtime.config().hub_url.clone(),
        runtime,
    };
    router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

pub async fn serve(config: RuntimeConfig) -> Result<(), RuntimeError> {
    let runtime = Arc::new(OdysseyRuntime::new(config.clone())?);
    let app = build_app(runtime);
    let listener = tokio::net::TcpListener::bind(&config.bind_addr)
        .await
        .map_err(|err| RuntimeError::Io {
            path: config.bind_addr.clone(),
            message: err.to_string(),
        })?;
    axum::serve(listener, app)
        .await
        .map_err(|err| RuntimeError::Executor(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{build_app, serve};
    use axum::body::Body;
    use axum::http::{
        Method, Request, StatusCode,
        header::{ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN},
    };
    use odyssey_rs_protocol::{DEFAULT_HUB_URL, SandboxMode};
    use odyssey_rs_runtime::{OdysseyRuntime, RuntimeConfig, RuntimeError};
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tower::util::ServiceExt;

    fn runtime_config(root: &std::path::Path) -> RuntimeConfig {
        RuntimeConfig {
            cache_root: root.join("bundles"),
            session_root: root.join("sessions"),
            sandbox_root: root.join("sandbox"),
            bind_addr: "127.0.0.1:0".to_string(),
            sandbox_mode_override: Some(SandboxMode::DangerFullAccess),
            hub_url: DEFAULT_HUB_URL.to_string(),
            worker_count: 2,
            queue_capacity: 32,
            ..RuntimeConfig::default()
        }
    }

    #[tokio::test]
    async fn app_builder_wraps_router() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime"));
        let app = build_app(runtime);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn app_builder_registers_permissive_cors_for_preflight_requests() {
        let temp = tempdir().expect("tempdir");
        let runtime = Arc::new(OdysseyRuntime::new(runtime_config(temp.path())).expect("runtime"));
        let app = build_app(runtime);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/bundles")
                    .header(ORIGIN, "https://example.com")
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&"*".parse().expect("header value"))
        );
    }

    #[tokio::test]
    async fn serve_returns_io_error_for_invalid_bind_address() {
        let temp = tempdir().expect("tempdir");
        let mut config = runtime_config(temp.path());
        config.bind_addr = "not-a-socket-address".to_string();

        let error = serve(config).await.expect_err("invalid bind should fail");

        assert!(matches!(error, RuntimeError::Io { .. }));
        assert!(error.to_string().contains("not-a-socket-address"));
    }
}
