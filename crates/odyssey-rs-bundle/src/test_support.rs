use std::fs;
use std::path::Path;

pub fn write_bundle_project(
    root: &Path,
    id: &str,
    version: &str,
    resource_relative_path: &str,
    resource_contents: &str,
) {
    fs::create_dir_all(root.join("skills").join("repo-hygiene")).expect("create skill dir");
    fs::create_dir_all(root.join("agents").join(id)).expect("create agent dir");
    let resource_path = root.join("resources").join(resource_relative_path);
    if let Some(parent) = resource_path.parent() {
        fs::create_dir_all(parent).expect("create resource dir");
    }
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
    mode: workspace_write
    env: {{}}
    permissions:
      filesystem:
        exec: []
        mounts:
          read: []
          write: []
      network: []
    system_tools_mode: explicit
    system_tools: []
    resources:
      cpu: 1
      memory_mb: 512
  agents:
    - id: {id}
      spec: agents/{id}/agent.yaml
      default: true
"#
        ),
    )
    .expect("write manifest");
    fs::write(
        root.join("agents").join(id).join("agent.yaml"),
        format!(
            r#"apiVersion: odyssey.ai/v1
kind: Agent
metadata:
  name: {id}
  version: {version}
  description: test bundle
spec:
  kind: prompt
  prompt: keep responses concise
  model:
    provider: openai
    name: gpt-4.1-mini
  tools:
    allow: ["Read", "Skill"]
"#
        ),
    )
    .expect("write agent");
    fs::write(root.join("README.md"), format!("# {id}\n")).expect("write readme");
    fs::write(
        root.join("skills").join("repo-hygiene").join("SKILL.md"),
        "# Repo Hygiene\n",
    )
    .expect("write skill");
    fs::write(resource_path, resource_contents).expect("write resource");
}
