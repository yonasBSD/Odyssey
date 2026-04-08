// File Names
pub(crate) const BUNDLE_MANIFEST_FILE_NAME: &str = "odyssey.bundle.yaml";
pub(crate) const BUNDLE_GITIGNORE_FILE_NAME: &str = ".gitignore";
pub(crate) const AGENTS_DIR_NAME: &str = "agents";
pub(crate) const SHARED_DIR_NAME: &str = "shared";
pub(crate) const SKILLS_DIR_NAME: &str = "skills";
pub(crate) const RESOURCES_DIR_NAME: &str = "resources";

// Bundle specific constants
pub(crate) const BUNDLE_INSTALL_ROOT_DIR_NAME: &str = "installs";
pub(crate) const BUNDLE_IMPORTS_ROOT_DIR_NAME: &str = "imports";
pub(crate) const BUNDLE_INSTALL_LAYOUT_DIR_NAME: &str = ".odyssey";
pub(crate) const BUNDLE_ODYSSEY_EXPORT_FILE_FORMAT: &str = ".odyssey";
pub(crate) const BUNDLE_LOCAL_NAMESPACE: &str = "local";
pub(crate) const BUNDLE_CONFIG_SCHEMA_VERSION: usize = 1;

// Layout specific constants
pub const OCI_LAYOUT_VERSION: &str = "1.0.0";
pub const OCI_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const BUNDLE_CONFIG_MEDIA_TYPE: &str = "application/vnd.odyssey.bundle.config.v1+json";
pub const BUNDLE_LAYER_MEDIA_TYPE: &str = "application/vnd.odyssey.bundle.layer.v1";
pub const REF_ANNOTATION: &str = "org.opencontainers.image.ref.name";
pub const ARCHIVE_MAGIC: &[u8; 6] = b"ODYB1\n"; //odyssey bundle
pub const LAYOUT_PAYLOAD_BUNDLE_MAGIC: &[u8; 7] = b"ODYLP1\n"; //odyssey layout payload
