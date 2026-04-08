mod agent_spec;
mod bundle_manifest;
mod error;
mod loader;
mod reference;

pub use agent_spec::{
    AgentExecution, AgentInterfaces, AgentKind, AgentPolicyHints, AgentProgram, AgentRequirements,
    AgentSpec, AgentToolPolicy,
};
pub use bundle_manifest::{
    BundleAgentEntry, BundleDescriptor, BundleExecutor, BundleManifest, BundleMemory,
    BundleSandbox, BundleSandboxFilesystem, BundleSandboxLimits, BundleSandboxMounts,
    BundleSandboxPermissions, BundleSignatures, BundleSkill, BundleSystemToolsMode, BundleTool,
    ManifestVersion, ProviderKind,
};
pub use error::ManifestError;
pub use loader::BundleLoader;
pub use reference::{BundleRef, BundleRefKind};
