//! Types of the module store (lmts `modules/types.ts`).

use crate::types::{PublishedCliSchema, Record, ValidationIssue};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// A moving channel of a module repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Latest,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Latest => "latest",
            Channel::Dev => "dev",
        }
    }

    pub fn parse(s: &str) -> Option<Channel> {
        match s {
            "latest" => Some(Channel::Latest),
            "dev" => Some(Channel::Dev),
            _ => None,
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed `@owner/repo[#channel]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleSpec {
    pub owner: String,
    pub repo: String,
    pub channel: Channel,
    /// Normalized key `@owner/repo#channel`.
    pub key: String,
    /// The text that was parsed.
    pub requested: String,
}

/// A verified cache entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedModule {
    pub spec: ModuleSpec,
    pub dir: PathBuf,
    pub path: PathBuf,
    pub sha256: String,
    /// `meta.json` `fetched_at` (empty when the file lacks it).
    pub fetched_at: String,
    /// `meta.json` `last_checked`, else `fetched_at`.
    pub last_checked: String,
    pub etag: Option<String>,
    pub channel: Channel,
    pub tool: String,
    pub version: Option<String>,
    pub document: PublishedCliSchema,
}

/// One row of [`super::list`]; corrupt entries carry `problem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    pub spec: String,
    pub channel: Option<Channel>,
    pub tool: Option<String>,
    pub version: Option<String>,
    pub sha256: Option<String>,
    pub fetched_at: Option<String>,
    pub dir: PathBuf,
    /// `Some(None)`: checked, not on PATH. `None`: not checked (corrupt entry).
    pub binary_on_path: Option<Option<PathBuf>>,
    pub problem: Option<String>,
}

/// What [`super::install`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    Installed,
    Unchanged,
    Replaced,
}

impl InstallAction {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallAction::Installed => "installed",
            InstallAction::Unchanged => "unchanged",
            InstallAction::Replaced => "replaced",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstallOutcome {
    pub action: InstallAction,
    pub entry: CachedModule,
}

/// One result of [`super::upgrade`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeOutcome {
    Unchanged {
        spec: String,
        channel: Channel,
        sha256: String,
        version: Option<String>,
    },
    Updated {
        spec: String,
        channel: Channel,
        from_sha: String,
        to_sha: String,
        from_version: Option<String>,
        to_version: Option<String>,
        operations_added: Vec<String>,
        operations_removed: Vec<String>,
    },
    Failed {
        spec: String,
        channel: Option<Channel>,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOutcome {
    pub removed: Vec<PathBuf>,
}

/// Where [`super::ensure`] got the module from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureSource {
    /// Fresh cache entry, no network.
    Cache,
    /// Revalidated (304) or replaced (200).
    Network,
    /// Refresh failed; the existing entry is used (see `warning`).
    StaleCache,
    /// Was missing and has been installed.
    Installed,
}

impl EnsureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EnsureSource::Cache => "cache",
            EnsureSource::Network => "network",
            EnsureSource::StaleCache => "stale-cache",
            EnsureSource::Installed => "installed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnsureOutcome {
    pub module: CachedModule,
    pub source: EnsureSource,
    pub warning: Option<String>,
}

/// Error codes of the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleErrorCode {
    SpecSyntax,
    SpecChannel,
    NotInstalled,
    CacheCorrupt,
    InvalidSchema,
    BadJson,
    OutputLimit,
    FetchFailed,
    PathTraversal,
    AtomicWriteFailed,
}

impl ModuleErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleErrorCode::SpecSyntax => "SPEC_SYNTAX",
            ModuleErrorCode::SpecChannel => "SPEC_CHANNEL",
            ModuleErrorCode::NotInstalled => "NOT_INSTALLED",
            ModuleErrorCode::CacheCorrupt => "CACHE_CORRUPT",
            ModuleErrorCode::InvalidSchema => "INVALID_SCHEMA",
            ModuleErrorCode::BadJson => "BAD_JSON",
            ModuleErrorCode::OutputLimit => "OUTPUT_LIMIT",
            ModuleErrorCode::FetchFailed => "FETCH_FAILED",
            ModuleErrorCode::PathTraversal => "PATH_TRAVERSAL",
            ModuleErrorCode::AtomicWriteFailed => "ATOMIC_WRITE_FAILED",
        }
    }
}

impl fmt::Display for ModuleErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for ModuleErrorCode {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<ModuleErrorCode> for &str {
    fn eq(&self, other: &ModuleErrorCode) -> bool {
        *self == other.as_str()
    }
}

/// A store failure: code, message and optional context.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleError {
    pub code: ModuleErrorCode,
    pub message: String,
    pub file: Option<PathBuf>,
    pub issues: Option<Vec<ValidationIssue>>,
    /// Extra context, e.g. `{status, url, file}` for a 404 (boxed to keep the error small).
    pub details: Option<Box<Record>>,
}

impl ModuleError {
    pub fn new(code: ModuleErrorCode, message: impl Into<String>) -> Self {
        ModuleError {
            code,
            message: message.into(),
            file: None,
            issues: None,
            details: None,
        }
    }

    pub(crate) fn file(mut self, file: impl Into<PathBuf>) -> Self {
        self.file = Some(file.into());
        self
    }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ModuleError {}

/// `meta.json` of a cache entry (same keys and order as lmts writes them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaDocument {
    pub spec: String,
    pub url: String,
    pub sha256: String,
    /// Absent only when an existing `meta.json` lacked it (kept absent on a 304 rewrite).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    pub last_checked: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub channel: Channel,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub bytes: u64,
}
