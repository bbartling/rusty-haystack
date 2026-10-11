//! Repository values are independent of executable catalog admission.
use super::RepositoryError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{fmt, str::FromStr};

/// Three numeric components, as required for a library version (not SemVer).
/// Leading zeroes are normalized; overflow and suffixes are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibraryVersion([u64; 3]);
impl FromStr for LibraryVersion {
    type Err = RepositoryError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > 62 {
            return Err(RepositoryError::InvalidInput("library version length"));
        }
        let parts: Vec<_> = text.split('.').collect();
        if parts.len() != 3 {
            return Err(RepositoryError::InvalidInput("library version"));
        }
        let mut out = [0; 3];
        for (dest, part) in out.iter_mut().zip(parts) {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(RepositoryError::InvalidInput("library version"));
            }
            *dest = part
                .parse()
                .map_err(|_| RepositoryError::InvalidInput("library version overflow"))?;
        }
        Ok(Self(out))
    }
}
impl fmt::Display for LibraryVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0[0], self.0[1], self.0[2])
    }
}

/// Exact library identity. Construction validates the library-name grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryPin {
    lib: String,
    version: LibraryVersion,
}
impl LibraryPin {
    /// Create a named exact version; no dependency resolution is performed.
    pub fn new(lib: &str, version: LibraryVersion) -> Result<Self, RepositoryError> {
        validate_lib(lib)?;
        Ok(Self {
            lib: lib.to_owned(),
            version,
        })
    }
    /// Library name.
    pub fn lib(&self) -> &str {
        &self.lib
    }
    /// Exact normalized version.
    pub fn version(&self) -> LibraryVersion {
        self.version
    }
}
pub(super) fn validate_lib(lib: &str) -> Result<(), RepositoryError> {
    if lib.is_empty()
        || lib.len() > 128
        || !lib.split('.').all(|part| {
            let bytes = part.as_bytes();
            bytes.first().is_some_and(u8::is_ascii_lowercase)
                && bytes
                    .last()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && bytes.iter().enumerate().all(|(index, b)| {
                    b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || (*b == b'_'
                            && index > 0
                            && index + 1 < bytes.len()
                            && bytes[index - 1] != b'_'
                            && bytes[index + 1] != b'_')
                })
        })
    {
        return Err(RepositoryError::InvalidInput("library name"));
    }
    Ok(())
}

/// Pinned LibDependVersions grammar: three decimal or `x` segments, optionally
/// an inclusive range. Spaces around the dash are normalized. No SemVer syntax.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyVersions {
    text: String,
    low: [Option<u64>; 3],
    high: [Option<u64>; 3],
}
impl FromStr for DependencyVersions {
    type Err = RepositoryError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        fn parse(text: &str) -> Result<[Option<u64>; 3], RepositoryError> {
            let parts: Vec<_> = text.split('.').collect();
            if parts.len() != 3 {
                return Err(RepositoryError::InvalidInput("dependency versions"));
            }
            let mut result = [None; 3];
            for (out, part) in result.iter_mut().zip(parts) {
                if part == "x" {
                    continue;
                }
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(RepositoryError::InvalidInput("dependency versions"));
                }
                *out =
                    Some(part.parse().map_err(|_| {
                        RepositoryError::InvalidInput("dependency version overflow")
                    })?);
            }
            Ok(result)
        }
        if text.len() > 256 {
            return Err(RepositoryError::InvalidInput("dependency versions"));
        }
        let (lo, hi) = text
            .split_once('-')
            .map_or((text, text), |(a, b)| (a.trim(), b.trim()));
        let low = parse(lo)?;
        let high = parse(hi)?;
        if low.map(|v| v.unwrap_or(0)) > high.map(|v| v.unwrap_or(u64::MAX)) {
            return Err(RepositoryError::InvalidInput("reversed dependency range"));
        }
        Ok(Self {
            text: if lo == hi {
                lo.to_owned()
            } else {
                format!("{lo}-{hi}")
            },
            low,
            high,
        })
    }
}
impl DependencyVersions {
    /// Test the inclusive wildcard-aware bounds, without resolving dependencies.
    pub fn contains(&self, version: LibraryVersion) -> bool {
        fn compare(bound: [Option<u64>; 3], version: [u64; 3]) -> std::cmp::Ordering {
            for (bound, actual) in bound.into_iter().zip(version) {
                if let Some(bound) = bound {
                    let order = actual.cmp(&bound);
                    if order != std::cmp::Ordering::Equal {
                        return order;
                    }
                }
            }
            std::cmp::Ordering::Equal
        }
        compare(self.low, version.0) != std::cmp::Ordering::Less
            && compare(self.high, version.0) != std::cmp::Ordering::Greater
    }
    /// Wire spelling with separator whitespace normalized.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// SHA-256 of the exact transferred archive bytes. A hash alone is not authenticity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sha256Digest(pub(super) [u8; 32]);
impl FromStr for Sha256Digest {
    type Err = RepositoryError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let payload = text
            .strip_prefix("sha256:")
            .ok_or(RepositoryError::InvalidDigest)?;
        if payload.len() != 43 {
            return Err(RepositoryError::InvalidDigest);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| RepositoryError::InvalidDigest)?;
        let bytes = bytes
            .try_into()
            .map_err(|_| RepositoryError::InvalidDigest)?;
        Ok(Self(bytes))
    }
}
impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

/// Upstream maturity; independent of numeric version ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maturity {
    Alpha,
    Beta,
    Stable,
}
/// Mutable repository availability. Yanked artifacts may still be fetched exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Yanked,
    Deleted,
}
/// Advertised dependency metadata; never installed or resolved by this client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoDependency {
    pub lib: String,
    pub versions: DependencyVersions,
}
/// Repository advertisement; fields are retained independently of downloaded bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoLibVersion {
    pub pin: LibraryPin,
    pub maturity: Maturity,
    pub availability: Availability,
    pub availability_msg: Option<String>,
    pub doc: Option<String>,
    /// `None` is unknown; `Some([])` explicitly advertises no dependencies.
    pub depends: Option<Vec<RepoDependency>>,
    pub digest: Option<Sha256Digest>,
}

/// Metadata observed by this client at a repository endpoint. Source and metadata
/// are immutable together; callers can clone the DTO to make a separate assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoAdvertisement {
    pub(super) metadata: RepoLibVersion,
    pub(super) source: std::sync::Arc<RepositorySource>,
}
impl RepoAdvertisement {
    /// The exact decoded advertisement observed at `source()`.
    pub fn metadata(&self) -> &RepoLibVersion {
        &self.metadata
    }
    /// Metadata request/final URL and redirects, independent of a later fetch.
    pub fn source(&self) -> &RepositorySource {
        &self.source
    }
}
impl std::ops::Deref for RepoAdvertisement {
    type Target = RepoLibVersion;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}
/// Verification metadata has either an observed repository source or is an
/// explicitly caller-supplied assertion. Neither form grants admission authority.
#[derive(Clone, Copy, Debug)]
pub enum MetadataEvidence<'a> {
    Observed(&'a RepoAdvertisement),
    Supplied(&'a RepoLibVersion),
}
impl<'a> MetadataEvidence<'a> {
    pub(super) fn metadata(self) -> &'a RepoLibVersion {
        match self {
            Self::Observed(value) => value.metadata(),
            Self::Supplied(value) => value,
        }
    }
    pub(super) fn source(self) -> Option<&'a RepositorySource> {
        match self {
            Self::Observed(value) => Some(value.source()),
            Self::Supplied(_) => None,
        }
    }
}
impl<'a> From<&'a RepoAdvertisement> for MetadataEvidence<'a> {
    fn from(value: &'a RepoAdvertisement) -> Self {
        Self::Observed(value)
    }
}
impl<'a> From<&'a RepoLibVersion> for MetadataEvidence<'a> {
    fn from(value: &'a RepoLibVersion) -> Self {
        Self::Supplied(value)
    }
}

/// Which separately supplied expectations matched the computed exact-byte hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    Unverified,
    Advertised,
    Caller,
    AdvertisedAndCaller,
    /// A digest in unbound caller-supplied metadata matched.
    SuppliedMetadata,
    /// Both an unbound metadata digest and the separate caller digest matched.
    SuppliedMetadataAndCaller,
}
/// Request and final source of a metadata or artifact response. URLs exclude userinfo;
/// redirect targets with userinfo or fragments are rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositorySource {
    pub request_url: url::Url,
    pub final_url: url::Url,
    pub redirects: usize,
}
/// An inert artifact. No extraction, execution, filesystem write or catalog mutation.
#[derive(Clone, Debug)]
pub struct DownloadedLibrary {
    pub pin: LibraryPin,
    pub bytes: Vec<u8>,
    pub metadata: Option<RepoLibVersion>,
    /// Source of an observed advertisement, independent of the artifact source.
    /// `None` means absent metadata or explicitly caller-supplied metadata.
    pub metadata_source: Option<RepositorySource>,
    pub computed_digest: Sha256Digest,
    pub caller_digest: Option<Sha256Digest>,
    pub integrity: Integrity,
    pub source: RepositorySource,
}
