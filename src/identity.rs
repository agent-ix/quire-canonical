// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The authority-qualified runtime subject reference value object
//! (FR-287/FR-035), and the `Revision` (FR-035) and digest-domain (FR-201)
//! types it is built from.
//!
//! # Digest-domain design decision
//!
//! FR-201 enumerates a large, closed vocabulary of digest domains, including
//! several owned by other repositories' own concerns (`ir-canonical`,
//! `ir-bound`, `quire.contract-ir.semantic/v1`, the `quire.model.*` family).
//! This crate is the org's one shared, domain-agnostic content-identity
//! crate; it has no way to know, and no business declaring, the complete set
//! of domains every consumer will ever mint, and hardcoding today's FR-201
//! table as a closed Rust `enum` here would mean every future domain addition
//! — QSL's, Contract-IR's, or anyone else's — needs a `quire-canonical`
//! release before its owner can use it.
//!
//! [`DigestDomain`] is therefore an **open**, validated newtype over the
//! label string rather than a closed enum: it refuses an empty label (FR-287
//! requires every key member to refuse rather than default) and otherwise
//! carries the label verbatim, with no normalization of hyphen/underscore
//! spelling or aliasing (FR-201's own rule: "a hyphenated and an underscored
//! spelling are different labels"). The catalog of which labels are
//! meaningful, and what preimage each one covers, remains owned by
//! `quire-specification`'s FR-201 — this crate enforces only the shared
//! comparison discipline (exact string equality, lexicographic order,
//! distinctness of every non-identical label) that FR-201 and FR-287 require
//! of *any* domain label.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use crate::Sha256Digest;

/// A required identity component was missing or empty.
///
/// FR-287 requires every key member to refuse rather than default: "Missing
/// or empty identity, revision, digest, kind or object bytes refuses; none
/// compares equal to a default or empty value." Every constructor in this
/// module that can be handed an empty component returns this rather than
/// silently substituting a generated or empty value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    /// [`Revision::new`] was given an empty namespace.
    #[error("revision namespace must not be empty")]
    EmptyRevisionNamespace,
    /// [`Revision::new`] was given an empty value.
    #[error("revision value must not be empty")]
    EmptyRevisionValue,
    /// [`DigestDomain::new`] was given an empty label.
    #[error("digest domain label must not be empty")]
    EmptyDigestDomain,
    /// [`AuthorityIdentity::new`] was given an empty identity.
    #[error("authority identity must not be empty")]
    EmptyAuthorityIdentity,
    /// [`SubjectKind::new`] was given an empty kind.
    #[error("subject kind must not be empty")]
    EmptySubjectKind,
    /// [`ObjectIdentity::new`] was given empty bytes.
    #[error("authority-local object identity must not be empty")]
    EmptyObjectIdentity,
}

/// A namespaced authority revision (FR-035): "authority revision" is a
/// `{namespace, value}` pair, not inferred from an object identifier and not
/// defaulted.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Revision {
    namespace: String,
    value: String,
}

impl Revision {
    /// A revision in `namespace` with value `value`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::EmptyRevisionNamespace`] or
    /// [`IdentityError::EmptyRevisionValue`] when either is empty.
    pub fn new(
        namespace: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, IdentityError> {
        let namespace = namespace.into();
        let value = value.into();
        if namespace.is_empty() {
            return Err(IdentityError::EmptyRevisionNamespace);
        }
        if value.is_empty() {
            return Err(IdentityError::EmptyRevisionValue);
        }
        Ok(Self { namespace, value })
    }

    /// The revision's namespace.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The revision's value within its namespace.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A digest-domain label (FR-201): a digest is compared only against another
/// digest in the same domain, and a domain is never inferred, aliased or
/// normalized. See the module documentation for why this is an open,
/// validated string rather than a closed enum of FR-201's current table.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DigestDomain(String);

impl DigestDomain {
    /// The domain labeled exactly `label`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::EmptyDigestDomain`] when `label` is empty.
    pub fn new(label: impl Into<String>) -> Result<Self, IdentityError> {
        let label = label.into();
        if label.is_empty() {
            return Err(IdentityError::EmptyDigestDomain);
        }
        Ok(Self(label))
    }

    /// The domain label's exact text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DigestDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An authority digest (FR-035): a [`Sha256Digest`] together with its
/// declared [`DigestDomain`]. Digests in different domains are never equal,
/// even when their bytes coincide (FR-201).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AuthorityDigest {
    domain: DigestDomain,
    digest: Sha256Digest,
}

impl AuthorityDigest {
    /// The digest `digest`, declared under `domain`.
    #[must_use]
    pub const fn new(domain: DigestDomain, digest: Sha256Digest) -> Self {
        Self { domain, digest }
    }

    /// The declared digest domain.
    #[must_use]
    pub const fn domain(&self) -> &DigestDomain {
        &self.domain
    }

    /// The digest bytes.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

/// The exact identity of the selected binding authority (FR-035/FR-287).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AuthorityIdentity(String);

impl AuthorityIdentity {
    /// The authority identified exactly by `identity`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::EmptyAuthorityIdentity`] when `identity` is empty.
    pub fn new(identity: impl Into<String>) -> Result<Self, IdentityError> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(IdentityError::EmptyAuthorityIdentity);
        }
        Ok(Self(identity))
    }

    /// The authority identity's exact text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthorityIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The subject's exact typed discriminator (FR-035/FR-287): "domain package
/// identity, IR node identity, `sha256-jcs`", or any other effective
/// declaration key. Equal opaque object-identity bytes of different kinds
/// remain unequal.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SubjectKind(String);

impl SubjectKind {
    /// The subject kind identified exactly by `kind`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::EmptySubjectKind`] when `kind` is empty.
    pub fn new(kind: impl Into<String>) -> Result<Self, IdentityError> {
        let kind = kind.into();
        if kind.is_empty() {
            return Err(IdentityError::EmptySubjectKind);
        }
        Ok(Self(kind))
    }

    /// The subject kind's exact text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubjectKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The authority-local object identity: exact non-empty bytes retained
/// without parsing, trimming, normalization or reconstruction (FR-035).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ObjectIdentity(Vec<u8>);

impl ObjectIdentity {
    /// The object identified exactly by `bytes`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::EmptyObjectIdentity`] when `bytes` is empty.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, IdentityError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(IdentityError::EmptyObjectIdentity);
        }
        Ok(Self(bytes))
    }

    /// The exact object-identity bytes, unparsed and untrimmed.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Why [`AuthorityQualifiedSubjectReference::semantic_eq`] refused to
/// compare two references.
///
/// FR-287: "Semantic object equality is defined only within one authority and
/// kind; crossing either boundary yields the typed foreign/wrong-kind refusal
/// required by FR-204 rather than a Boolean verdict."
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SemanticComparisonRefusal {
    /// The two references select different authorities (identity, revision,
    /// or digest).
    #[error("semantic comparison refused: references select different authorities")]
    ForeignAuthority,
    /// The two references share an authority but declare different subject
    /// kinds.
    #[error("semantic comparison refused: references declare different subject kinds")]
    ForeignKind,
}

/// The authority-qualified runtime subject reference (FR-287): the exact
/// 5-member key `(authority identity, authority revision, authority digest,
/// subject kind, authority-local object identity)` that identifies a
/// concrete runtime subject bound for assessment.
///
/// This is a pure value: `Eq`, `Ord` and `Hash` are componentwise over
/// exactly these five members, in exactly this declared order, and nothing
/// else. Display text, source path, span, payload, trace identifier,
/// timestamp, arrival position and transport status are not members of this
/// type and have no field here to carry them (FR-287's "not members" list).
/// `Ord`'s lexicographic order over the five members is for deterministic
/// collection comparison only; it carries no causal, delivery, retry, or
/// arrival meaning (FR-035, FR-287).
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct AuthorityQualifiedSubjectReference {
    authority_identity: AuthorityIdentity,
    authority_revision: Revision,
    authority_digest: AuthorityDigest,
    subject_kind: SubjectKind,
    object_identity: ObjectIdentity,
}

impl AuthorityQualifiedSubjectReference {
    /// A reference with the exact given key members.
    ///
    /// Every component type already refuses an empty value at its own
    /// constructor (see [`AuthorityIdentity::new`], [`Revision::new`],
    /// [`SubjectKind::new`] and [`ObjectIdentity::new`]), so by the time a
    /// caller holds one of each, FR-287's "missing or empty component
    /// refuses" requirement is already satisfied; this constructor cannot
    /// itself fail.
    #[must_use]
    pub const fn new(
        authority_identity: AuthorityIdentity,
        authority_revision: Revision,
        authority_digest: AuthorityDigest,
        subject_kind: SubjectKind,
        object_identity: ObjectIdentity,
    ) -> Self {
        Self {
            authority_identity,
            authority_revision,
            authority_digest,
            subject_kind,
            object_identity,
        }
    }

    /// The selected binding authority's exact identity.
    #[must_use]
    pub const fn authority_identity(&self) -> &AuthorityIdentity {
        &self.authority_identity
    }

    /// The selected authority revision.
    #[must_use]
    pub const fn authority_revision(&self) -> &Revision {
        &self.authority_revision
    }

    /// The selected authority digest.
    #[must_use]
    pub const fn authority_digest(&self) -> &AuthorityDigest {
        &self.authority_digest
    }

    /// The subject's typed discriminator.
    #[must_use]
    pub const fn subject_kind(&self) -> &SubjectKind {
        &self.subject_kind
    }

    /// The authority-local object-identity bytes.
    #[must_use]
    pub const fn object_identity(&self) -> &ObjectIdentity {
        &self.object_identity
    }

    /// Whether `self` and `other` name the same object, defined only within
    /// one authority and one subject kind.
    ///
    /// This is distinct from [`PartialEq`]/[`Eq`] (canonical key equality,
    /// which compares all five members and never refuses): `semantic_eq`
    /// answers "is this the same object" once the authority and kind are
    /// already established to match, and refuses — rather than returning
    /// `false` — when they do not, per FR-287/FR-204.
    ///
    /// # Errors
    ///
    /// [`SemanticComparisonRefusal::ForeignAuthority`] when the references
    /// select different authority identity, revision or digest.
    /// [`SemanticComparisonRefusal::ForeignKind`] when they share an
    /// authority but declare different subject kinds.
    pub fn semantic_eq(&self, other: &Self) -> Result<bool, SemanticComparisonRefusal> {
        if self.authority_identity != other.authority_identity
            || self.authority_revision != other.authority_revision
            || self.authority_digest != other.authority_digest
        {
            return Err(SemanticComparisonRefusal::ForeignAuthority);
        }
        if self.subject_kind != other.subject_kind {
            return Err(SemanticComparisonRefusal::ForeignKind);
        }
        Ok(self.object_identity == other.object_identity)
    }
}
