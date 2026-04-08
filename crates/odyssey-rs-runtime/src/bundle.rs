use crate::RuntimeError;
use odyssey_rs_bundle::{BundleInstall, BundleStore};
use odyssey_rs_manifest::{AgentSpec, BundleManifest};

#[derive(Clone)]
pub struct LoadedBundle {
    pub install: BundleInstall,
    pub manifest: BundleManifest,
    pub agents: Vec<AgentSpec>,
}

pub fn load_bundle(store: &BundleStore, reference: &str) -> Result<LoadedBundle, RuntimeError> {
    let install = store.resolve(reference)?;
    let manifest = install.metadata.bundle_manifest.clone();
    let agents = install.metadata.agents.clone();
    Ok(LoadedBundle {
        install,
        manifest,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use super::load_bundle;
    use odyssey_rs_bundle::BundleStore;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    fn write_bundle_project(root: &std::path::Path, id: &str, version: &str) {
        let agent_root = root.join("agents").join(id);
        fs::create_dir_all(root.join("skills").join("repo-hygiene")).expect("create skills");
        fs::create_dir_all(root.join("resources").join("data")).expect("create data dir");
        fs::create_dir_all(agent_root.join("schemas")).expect("create agent schemas");
        fs::write(
            root.join("odyssey.bundle.yaml"),
            format!(
                r#"apiVersion: odyssey.ai/bundle.v1
kind: AgentBundle
metadata:
  name: {id}
  version: {version}
  readme: README.md
spec:
  abiVersion: v1
  skills:
    - name: repo-hygiene
      path: skills/repo-hygiene
  tools:
    - name: Read
      source: builtin
  sandbox:
    permissions:
      filesystem:
        exec: []
        mounts:
          read: []
          write: []
      network: []
    system_tools: []
    resources: {{}}
  agents:
    - id: {id}
      spec: agents/{id}/agent.yaml
      default: true
"#
            ),
        )
        .expect("write manifest");
        fs::write(
            agent_root.join("agent.yaml"),
            format!(
                r#"apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: {id}
  version: {version}
  description: runtime loader test
spec:
  kind: prompt
  prompt: keep responses concise
  model:
    provider: openai
    name: gpt-4.1-mini
  tools:
    allow: ["Read"]
"#
            ),
        )
        .expect("write agent");
        fs::write(root.join("README.md"), format!("# {id}\n")).expect("write readme");
        fs::write(
            root.join("resources").join("data").join("notes.txt"),
            "hello world\n",
        )
        .expect("write resource");
    }

    #[test]
    fn load_bundle_returns_manifest_agent_and_install() {
        let temp = tempdir().expect("tempdir");
        let store = BundleStore::new(temp.path().join("store"));
        let project_root = temp.path().join("project");
        fs::create_dir_all(&project_root).expect("create project");
        write_bundle_project(&project_root, "demo", "0.1.0");
        let install = store
            .build_and_install(&project_root)
            .expect("build and install");

        let loaded = load_bundle(&store, "demo").expect("load bundle");

        assert_eq!(loaded.install.path, install.path);
        assert_eq!(loaded.manifest.id, "demo");
        assert_eq!(loaded.manifest.version, "0.1.0");
        assert_eq!(loaded.agents[0].id, "demo");
    }

    #[test]
    fn load_bundle_surfaces_store_resolution_errors() {
        let temp = tempdir().expect("tempdir");
        let store = BundleStore::new(temp.path().join("store"));

        let error = match load_bundle(&store, "missing") {
            Ok(_) => panic!("missing bundle should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("bundle error"));
    }
}
