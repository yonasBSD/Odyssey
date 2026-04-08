use crate::RuntimeError;
use crate::bundle::load_bundle;
use odyssey_rs_bundle::BundleStore;
use odyssey_rs_manifest::{AgentSpec, BundleManifest};
use odyssey_rs_protocol::{BundleRef, ModelSpec};
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct ResolvedBundle {
    pub install_path: PathBuf,
    pub namespace: String,
    pub manifest: BundleManifest,
    pub agent: AgentSpec,
    pub agents: Vec<AgentSpec>,
    pub model: ModelSpec,
}

pub(crate) fn resolve_bundle_from_ref(
    store: &BundleStore,
    bundle_ref: &BundleRef,
    agent_id: Option<&str>,
    default_model: &ModelSpec,
) -> Result<ResolvedBundle, RuntimeError> {
    let loaded = load_bundle(store, bundle_ref.as_str())?;
    let selected_id = agent_id
        .or(loaded.manifest.default_agent_entry_id())
        .or_else(|| loaded.agents.first().map(|agent| agent.id.as_str()))
        .ok_or_else(|| RuntimeError::Invalid("bundle does not contain any agents".to_string()))?;
    let agent = loaded
        .agents
        .iter()
        .find(|candidate| candidate.id == selected_id)
        .cloned()
        .ok_or_else(|| RuntimeError::Invalid(format!("unknown agent `{selected_id}`")))?;
    let model = if agent.model.provider.trim().is_empty() || agent.model.name.trim().is_empty() {
        default_model.clone()
    } else {
        agent.model.clone()
    };
    Ok(ResolvedBundle {
        install_path: loaded.install.path,
        namespace: loaded.install.metadata.namespace,
        manifest: loaded.manifest,
        agents: loaded.agents,
        model,
        agent,
    })
}
