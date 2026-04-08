use crate::BundleError;
use crate::constants::{
    AGENTS_DIR_NAME, BUNDLE_CONFIG_SCHEMA_VERSION, BUNDLE_GITIGNORE_FILE_NAME,
    BUNDLE_INSTALL_LAYOUT_DIR_NAME, BUNDLE_LOCAL_NAMESPACE, BUNDLE_MANIFEST_FILE_NAME,
    RESOURCES_DIR_NAME, SHARED_DIR_NAME, SKILLS_DIR_NAME,
};
use crate::constants::{
    BUNDLE_CONFIG_MEDIA_TYPE, BUNDLE_LAYER_MEDIA_TYPE, OCI_INDEX_MEDIA_TYPE, OCI_LAYOUT_VERSION,
    OCI_MANIFEST_MEDIA_TYPE,
};
use crate::layout::{
    BundleConfig, OciImageIndex, OciImageManifest, annotated_descriptor, descriptor, pack_payload,
    sha256_digest, write_blob,
};
use ignore::WalkBuilder;
use odyssey_rs_manifest::{AgentSpec, BundleDescriptor, BundleLoader, BundleManifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct BundleProject {
    pub root: PathBuf,
    pub descriptor: BundleDescriptor,
    pub readme: String,
}

impl BundleProject {
    pub fn load(root: impl Into<PathBuf>) -> Result<Self, BundleError> {
        let root = root.into();
        prepare_local_wasm_components(&root)?;
        let loader = BundleLoader::new(&root);
        let descriptor = loader.load_project()?;
        let readme_path = root.join(&descriptor.manifest.readme);
        let readme = fs::read_to_string(&readme_path).map_err(|err| BundleError::Io {
            path: readme_path.display().to_string(),
            message: err.to_string(),
        })?;
        Ok(Self {
            root,
            descriptor,
            readme,
        })
    }
}

fn prepare_local_wasm_components(root: &Path) -> Result<(), BundleError> {
    let loader = BundleLoader::new(root);
    let bundle_manifest_path = root.join(BUNDLE_MANIFEST_FILE_NAME);
    let bundle_manifest = loader.load_bundle_manifest(&bundle_manifest_path)?;
    for entry in &bundle_manifest.agents {
        let agent_spec_path = root.join(&entry.spec);
        let agent = loader.load_agent_spec(&agent_spec_path, entry)?;
        if !agent.is_wasm() || agent.program.runner_class != "wasm-component" {
            continue;
        }
        let Some(module_rel) = agent_entrypoint(&agent, entry.module.as_deref()) else {
            continue;
        };
        let Some(agent_root) = agent_spec_path.parent() else {
            return Err(BundleError::Invalid(format!(
                "agent spec path has no parent directory: {}",
                agent_spec_path.display()
            )));
        };
        let cargo_manifest_path = agent_root.join("Cargo.toml");
        if !cargo_manifest_path.is_file() {
            continue;
        }
        let module_path = root.join(module_rel);
        let artifact_path = build_wasm_component(root, &cargo_manifest_path)?;
        stage_component_artifact(&artifact_path, &module_path)?;
    }
    Ok(())
}

fn agent_entrypoint<'a>(agent: &'a AgentSpec, fallback: Option<&'a str>) -> Option<&'a str> {
    if !agent.program.entrypoint.trim().is_empty() {
        Some(agent.program.entrypoint.as_str())
    } else {
        fallback.filter(|path| !path.trim().is_empty())
    }
}

fn build_wasm_component(root: &Path, cargo_manifest_path: &Path) -> Result<PathBuf, BundleError> {
    let cargo_manifest_path =
        fs::canonicalize(cargo_manifest_path).map_err(|err| io_err(cargo_manifest_path, err))?;
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "component",
            "build",
            "--release",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&cargo_manifest_path)
        .output()
        .map_err(|err| BundleError::Io {
            path: cargo_manifest_path.display().to_string(),
            message: format!("failed to start `cargo component build`: {err}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let message = if detail.is_empty() {
            format!(
                "`cargo component build` failed for {}",
                cargo_manifest_path.display()
            )
        } else {
            format!(
                "`cargo component build` failed for {}: {detail}",
                cargo_manifest_path.display()
            )
        };
        return Err(BundleError::Invalid(message));
    }

    parse_component_artifact_path(&output.stdout, &cargo_manifest_path)
}

fn parse_component_artifact_path(
    stdout: &[u8],
    cargo_manifest_path: &Path,
) -> Result<PathBuf, BundleError> {
    let manifest_path =
        fs::canonicalize(cargo_manifest_path).map_err(|err| io_err(cargo_manifest_path, err))?;
    let stdout = String::from_utf8_lossy(stdout);
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(candidate_manifest_path) = message
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Ok(candidate_manifest_path) = fs::canonicalize(candidate_manifest_path) else {
            continue;
        };
        if candidate_manifest_path != manifest_path {
            continue;
        }
        let target_kinds = message
            .get("target")
            .and_then(|target| target.get("kind"))
            .and_then(serde_json::Value::as_array);
        if !target_kinds.is_some_and(|kinds| {
            kinds
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|kind| kind == "cdylib")
        }) {
            continue;
        }
        let filenames = message
            .get("filenames")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                BundleError::Invalid(format!(
                    "missing output filenames from `cargo component build` for {}",
                    cargo_manifest_path.display()
                ))
            })?;
        if let Some(path) = filenames
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|path| path.ends_with(".wasm"))
        {
            return Ok(PathBuf::from(path));
        }
    }

    Err(BundleError::Invalid(format!(
        "could not determine component artifact path for {} from `cargo component build` output",
        cargo_manifest_path.display()
    )))
}

fn stage_component_artifact(artifact_path: &Path, module_path: &Path) -> Result<(), BundleError> {
    if let Some(parent) = module_path.parent() {
        fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    fs::copy(artifact_path, module_path).map_err(|err| io_err(module_path, err))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleMetadata {
    pub namespace: String,
    pub id: String,
    pub version: String,
    pub digest: String,
    pub readme: String,
    pub bundle_manifest: BundleManifest,
    pub agent_spec: AgentSpec,
    pub agents: Vec<AgentSpec>,
}

impl BundleMetadata {
    pub fn default_agent(&self) -> Option<&AgentSpec> {
        let default_id = self.bundle_manifest.default_agent_entry_id()?;
        self.agents.iter().find(|agent| agent.id == default_id)
    }
}

#[derive(Debug, Clone)]
pub struct BundleArtifact {
    pub path: PathBuf,
    pub metadata: BundleMetadata,
}

#[derive(Debug, Clone)]
pub struct BundleBuilder {
    project: BundleProject,
    namespace: String,
}

impl BundleBuilder {
    pub fn new(project: BundleProject) -> Self {
        Self {
            project,
            namespace: BUNDLE_LOCAL_NAMESPACE.to_string(),
        }
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn build(self, output_root: impl AsRef<Path>) -> Result<BundleArtifact, BundleError> {
        let output_root = output_root.as_ref();
        fs::create_dir_all(output_root).map_err(|err| io_err(output_root, err))?;
        validate_path_component(&self.namespace, "bundle namespace")?;
        validate_path_component(&self.project.descriptor.manifest.id, "bundle id")?;
        validate_path_component(&self.project.descriptor.manifest.version, "bundle version")?;
        //Create a bundle directory structure -> <namespace>/<manifest_id>/<manifest_version>/
        let bundle_dir = output_root
            .join(&self.namespace)
            .join(&self.project.descriptor.manifest.id)
            .join(&self.project.descriptor.manifest.version);

        if bundle_dir.exists() {
            fs::remove_dir_all(&bundle_dir).map_err(|err| io_err(&bundle_dir, err))?;
        }

        fs::create_dir_all(&bundle_dir).map_err(|err| io_err(&bundle_dir, err))?;

        //Create and copy files into the bundle dir
        materialize_payload(&self.project, &bundle_dir)?;
        let layout_dir = bundle_dir.join(BUNDLE_INSTALL_LAYOUT_DIR_NAME);
        fs::create_dir_all(&layout_dir).map_err(|err| io_err(&layout_dir, err))?;

        // The payload layer contains the runtime files a bundle install needs to unpack.
        let payload_bytes = pack_payload(&bundle_dir)?;
        let layer_digest = write_blob(&layout_dir, &payload_bytes)?;
        let layer_descriptor =
            descriptor(BUNDLE_LAYER_MEDIA_TYPE, &layer_digest, payload_bytes.len());

        // The config blob stores resolved bundle metadata alongside the validated source manifest.
        let config = BundleConfig {
            schema_version: BUNDLE_CONFIG_SCHEMA_VERSION,
            id: self.project.descriptor.manifest.id.clone(),
            version: self.project.descriptor.manifest.version.clone(),
            namespace: self.namespace.clone(),
            readme: self.project.readme.clone(),
            bundle_manifest: self.project.descriptor.manifest.clone(),
            agents: self.project.descriptor.agents.clone(),
        };
        let config_bytes = serde_json::to_vec_pretty(&config)
            .map_err(|err| BundleError::Invalid(err.to_string()))?;
        let config_digest = write_blob(&layout_dir, &config_bytes)?;
        let config_descriptor =
            descriptor(BUNDLE_CONFIG_MEDIA_TYPE, &config_digest, config_bytes.len());

        let reference = format!(
            "{}/{id}:{version}",
            self.namespace,
            id = self.project.descriptor.manifest.id,
            version = self.project.descriptor.manifest.version
        );
        let mut annotations = BTreeMap::new();
        annotations.insert(
            "org.opencontainers.image.title".to_string(),
            reference.clone(),
        );
        // The OCI manifest ties the custom config and payload layer together under content digests.
        let manifest = OciImageManifest {
            schema_version: 2,
            media_type: OCI_MANIFEST_MEDIA_TYPE.to_string(),
            config: config_descriptor,
            layers: vec![layer_descriptor],
            annotations,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|err| BundleError::Invalid(err.to_string()))?;
        let manifest_digest = write_blob(&layout_dir, &manifest_bytes)?;

        // index.json is the OCI layout entrypoint; it points readers to the bundle manifest blob.
        let index = OciImageIndex {
            schema_version: 2,
            media_type: OCI_INDEX_MEDIA_TYPE.to_string(),
            manifests: vec![annotated_descriptor(
                OCI_MANIFEST_MEDIA_TYPE,
                &manifest_digest,
                manifest_bytes.len(),
                &reference,
            )],
        };
        let index_bytes = serde_json::to_vec_pretty(&index)
            .map_err(|err| BundleError::Invalid(err.to_string()))?;

        fs::write(
            layout_dir.join("oci-layout"),
            format!("{{\"imageLayoutVersion\":\"{OCI_LAYOUT_VERSION}\"}}\n"),
        )
        .map_err(|err| io_err(&layout_dir.join("oci-layout"), err))?;
        fs::write(layout_dir.join("index.json"), index_bytes)
            .map_err(|err| io_err(&layout_dir.join("index.json"), err))?;

        let default_agent = self
            .project
            .descriptor
            .default_agent()
            .cloned()
            .ok_or_else(|| {
                BundleError::Invalid("bundle must contain a default agent".to_string())
            })?;
        let metadata = BundleMetadata {
            namespace: self.namespace,
            id: self.project.descriptor.manifest.id.clone(),
            version: self.project.descriptor.manifest.version.clone(),
            digest: manifest_digest,
            readme: self.project.readme,
            bundle_manifest: self.project.descriptor.manifest,
            agent_spec: default_agent,
            agents: self.project.descriptor.agents,
        };
        fs::write(
            layout_dir.join("bundle.json"),
            serde_json::to_vec_pretty(&metadata)
                .map_err(|err| BundleError::Invalid(err.to_string()))?,
        )
        .map_err(|err| io_err(&layout_dir.join("bundle.json"), err))?;

        Ok(BundleArtifact {
            path: bundle_dir,
            metadata,
        })
    }
}

/// Copy the runtime-visible bundle payload into the staging directory before packing it as a layer.
fn materialize_payload(project: &BundleProject, bundle_dir: &Path) -> Result<(), BundleError> {
    let bundle_manifest_src = project.root.join(BUNDLE_MANIFEST_FILE_NAME);
    let bundle_manifest_dst = bundle_dir.join(BUNDLE_MANIFEST_FILE_NAME);
    fs::copy(&bundle_manifest_src, &bundle_manifest_dst)
        .map_err(|err| io_err(&bundle_manifest_dst, err))?;

    let readme_src = project.root.join(&project.descriptor.manifest.readme);
    let readme_dst = bundle_dir.join(&project.descriptor.manifest.readme);
    if let Some(parent) = readme_dst.parent() {
        fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    fs::copy(&readme_src, &readme_dst).map_err(|err| io_err(&readme_dst, err))?;

    let gitignore_src = project.root.join(BUNDLE_GITIGNORE_FILE_NAME);
    let gitignore_dst = bundle_dir.join(BUNDLE_GITIGNORE_FILE_NAME);
    if gitignore_src.is_file() {
        fs::copy(&gitignore_src, &gitignore_dst).map_err(|err| io_err(&gitignore_dst, err))?;
    }

    let project_agents = project.root.join(AGENTS_DIR_NAME);
    if project_agents.exists() {
        copy_dir_all(&project_agents, &bundle_dir.join(AGENTS_DIR_NAME))?;
    }

    let project_shared = project.root.join(SHARED_DIR_NAME);
    if project_shared.exists() {
        copy_dir_all(&project_shared, &bundle_dir.join(SHARED_DIR_NAME))?;
    }

    let skills_dir = bundle_dir.join(SKILLS_DIR_NAME);
    let resources_dir = bundle_dir.join(RESOURCES_DIR_NAME);
    fs::create_dir_all(&skills_dir).map_err(|err| io_err(&skills_dir, err))?;
    fs::create_dir_all(&resources_dir).map_err(|err| io_err(&resources_dir, err))?;

    for skill in &project.descriptor.manifest.skills {
        copy_dir_all(
            &project.root.join(&skill.path),
            &skills_dir.join(&skill.name),
        )?;
    }

    let project_resources = project.root.join(RESOURCES_DIR_NAME);
    if project_resources.exists() {
        if !project_resources.is_dir() {
            return Err(BundleError::Invalid(format!(
                "resources path must be a directory: {}",
                project_resources.display()
            )));
        }
        copy_dir_all(&project_resources, &resources_dir)?;
    }

    Ok(())
}

/// Utility to check if the value is a filename or directory and not a Path
fn validate_path_component(value: &str, label: &str) -> Result<(), BundleError> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Err(BundleError::Invalid(format!("{label} cannot be empty")));
    }
    if path.is_absolute() {
        return Err(BundleError::Invalid(format!(
            "{label} must be a relative path component"
        )));
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(BundleError::Invalid(format!(
            "{label} must not contain path separators or traversal segments"
        )));
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), BundleError> {
    fs::create_dir_all(dst).map_err(|err| io_err(dst, err))?;
    for entry in bundle_walk_builder(src).build() {
        let entry = entry.map_err(|err| BundleError::Invalid(err.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(src)
            .map_err(|err| BundleError::Invalid(err.to_string()))?;
        let target = dst.join(relative);
        if entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
        {
            fs::create_dir_all(&target).map_err(|err| io_err(&target, err))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
            }
            fs::copy(entry.path(), &target).map_err(|err| io_err(&target, err))?;
        }
    }
    Ok(())
}

fn bundle_walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .sort_by_file_path(|left, right| left.cmp(right));
    builder
}

fn io_err(path: &Path, err: std::io::Error) -> BundleError {
    BundleError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    }
}

#[allow(dead_code)]
fn _payload_digest(root: &Path) -> Result<String, BundleError> {
    Ok(sha256_digest(&pack_payload(root)?))
}

#[cfg(test)]
mod tests {
    use super::{BundleBuilder, BundleProject, parse_component_artifact_path};
    use crate::constants::BUNDLE_INSTALL_LAYOUT_DIR_NAME;
    use crate::layout::{read_config, read_manifest};
    use crate::test_support::write_bundle_project;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn builder_materializes_payload_and_metadata() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        let output_root = temp.path().join("output");
        fs::create_dir_all(&project_root).expect("create project");
        write_bundle_project(&project_root, "demo", "0.1.0", "logo.txt", "liquidos");

        let project = BundleProject::load(&project_root).expect("load project");
        let artifact = BundleBuilder::new(project)
            .with_namespace("team")
            .build(&output_root)
            .expect("build bundle");

        let layout_root = artifact.path.join(BUNDLE_INSTALL_LAYOUT_DIR_NAME);
        let (_, manifest, manifest_digest) = read_manifest(&layout_root).expect("read manifest");
        let config = read_config(&layout_root, &manifest).expect("read config");

        assert_eq!(artifact.metadata.namespace, "team");
        assert_eq!(artifact.metadata.id, "demo");
        assert_eq!(artifact.metadata.version, "0.1.0");
        assert_eq!(artifact.metadata.digest, manifest_digest);
        assert_eq!(artifact.metadata.readme, "# demo\n");
        assert_eq!(config.namespace, "team");
        assert_eq!(config.readme, "# demo\n");
        assert_eq!(config.bundle_manifest.id, "demo");
        assert_eq!(
            fs::read_to_string(artifact.path.join("agents").join("demo").join("agent.yaml"))
                .expect("read bundled agent"),
            "apiVersion: odyssey.ai/v1\nkind: Agent\nmetadata:\n  name: demo\n  version: 0.1.0\n  description: test bundle\nspec:\n  kind: prompt\n  prompt: keep responses concise\n  model:\n    provider: openai\n    name: gpt-4.1-mini\n  tools:\n    allow: [\"Read\", \"Skill\"]\n"
        );
        assert_eq!(
            fs::read_to_string(
                artifact
                    .path
                    .join("skills")
                    .join("repo-hygiene")
                    .join("SKILL.md")
            )
            .expect("read bundled skill"),
            "# Repo Hygiene\n"
        );
        assert_eq!(
            fs::read_to_string(artifact.path.join("resources").join("logo.txt"))
                .expect("read bundled resource"),
            "liquidos"
        );
        assert!(!artifact.path.join("bundle.json").exists());
        assert!(!artifact.path.join("index.json").exists());
        assert!(!artifact.path.join("oci-layout").exists());
        assert!(!artifact.path.join("blobs").exists());
        assert!(layout_root.join("bundle.json").exists());
        assert!(layout_root.join("index.json").exists());
        assert!(layout_root.join("oci-layout").exists());
        assert!(layout_root.join("blobs").exists());
    }

    #[test]
    fn builder_rejects_namespace_with_path_traversal() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        let output_root = temp.path().join("output");
        fs::create_dir_all(&project_root).expect("create project");
        write_bundle_project(&project_root, "demo", "0.1.0", "logo.txt", "liquidos");

        let project = BundleProject::load(&project_root).expect("load project");
        let error = BundleBuilder::new(project)
            .with_namespace("../escape")
            .build(&output_root)
            .expect_err("reject path traversal namespace");

        assert_eq!(
            error.to_string(),
            "invalid bundle: bundle namespace must not contain path separators or traversal segments"
        );
    }

    #[test]
    fn builder_respects_bundle_gitignore() {
        let temp = tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        let output_root = temp.path().join("output");
        let target_file = project_root
            .join("agents")
            .join("demo")
            .join("target")
            .join("debug.log");
        let package_file = project_root
            .join("agents")
            .join("demo")
            .join("node_modules")
            .join("pkg")
            .join("index.js");

        fs::create_dir_all(&project_root).expect("create project");
        write_bundle_project(&project_root, "demo", "0.1.0", "logo.txt", "liquidos");
        fs::write(
            project_root.join(".gitignore"),
            "/target\n/node_modules\n**/target\n**/node_modules\n",
        )
        .expect("write gitignore");
        fs::create_dir_all(target_file.parent().expect("target parent")).expect("mkdir target");
        fs::create_dir_all(package_file.parent().expect("package parent")).expect("mkdir package");
        fs::write(&target_file, "skip\n").expect("write target file");
        fs::write(&package_file, "skip\n").expect("write package file");

        let project = BundleProject::load(&project_root).expect("load project");
        let artifact = BundleBuilder::new(project)
            .with_namespace("team")
            .build(&output_root)
            .expect("build bundle");

        assert!(artifact.path.join(".gitignore").is_file());
        assert!(
            !artifact
                .path
                .join("agents")
                .join("demo")
                .join("target")
                .exists()
        );
        assert!(
            !artifact
                .path
                .join("agents")
                .join("demo")
                .join("node_modules")
                .exists()
        );
    }

    #[test]
    fn parses_component_artifact_path_from_cargo_output() {
        let temp = tempdir().expect("tempdir");
        let manifest_path = temp.path().join("Cargo.toml");
        fs::write(
            &manifest_path,
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write manifest");
        let artifact_path = temp.path().join("target").join("demo.wasm");
        let output = format!(
            "{{\"reason\":\"compiler-artifact\",\"manifest_path\":\"{}\",\"target\":{{\"kind\":[\"cdylib\"]}},\"filenames\":[\"{}\"]}}\n{{\"reason\":\"build-finished\",\"success\":true}}\n",
            manifest_path.display(),
            artifact_path.display()
        );

        assert_eq!(
            parse_component_artifact_path(output.as_bytes(), &manifest_path)
                .expect("artifact path"),
            artifact_path
        );
    }
}
