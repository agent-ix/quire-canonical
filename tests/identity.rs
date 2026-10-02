// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! FR-035/FR-287 (TC-035): the authority-qualified runtime subject reference
//! value object, its component types, and the componentwise comparison
//! discipline FR-287 requires.

use quire_canonical::{
    AuthorityDigest, AuthorityIdentity, AuthorityQualifiedSubjectReference, DigestDomain,
    IdentityError, ObjectIdentity, Revision, SemanticComparisonRefusal, Sha256Digest, SubjectKind,
};

fn digest(seed: u8) -> Sha256Digest {
    quire_canonical::sha256(&seed, quire_canonical::Limits::new(1 << 10)).unwrap()
}

fn reference(
    authority: &str,
    revision_ns: &str,
    revision_value: &str,
    domain: &str,
    digest_seed: u8,
    kind: &str,
    object_bytes: &[u8],
) -> AuthorityQualifiedSubjectReference {
    AuthorityQualifiedSubjectReference::new(
        AuthorityIdentity::new(authority).unwrap(),
        Revision::new(revision_ns, revision_value).unwrap(),
        AuthorityDigest::new(DigestDomain::new(domain).unwrap(), digest(digest_seed)),
        SubjectKind::new(kind).unwrap(),
        ObjectIdentity::new(object_bytes.to_vec()).unwrap(),
    )
}

fn base() -> AuthorityQualifiedSubjectReference {
    reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    )
}

/// FR-287-AC-1: two references have one canonical key exactly when all five
/// members compare equal; a one-member mutation produces a distinct key.
#[test]
fn ac1_canonical_key_equality_is_componentwise() {
    let a = base();
    let b = base();
    assert_eq!(a, b, "identical components must compare equal");

    let different_authority = reference(
        "authority-b",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    let different_revision = reference(
        "authority-a",
        "orders",
        "rev-2",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    let different_digest = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        2,
        "domain-package",
        b"order-42",
    );
    let different_kind = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "ir-node",
        b"order-42",
    );
    let different_object = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-43",
    );

    for mutated in [
        &different_authority,
        &different_revision,
        &different_digest,
        &different_kind,
        &different_object,
    ] {
        assert_ne!(&a, mutated, "a one-member mutation must change the key");
    }
}

/// FR-287-AC-1 (second half): a semantic comparison across authority or kind
/// refuses rather than returning a Boolean object-equality verdict.
#[test]
fn ac1_semantic_comparison_across_authority_or_kind_refuses() {
    let a = base();
    let different_authority = reference(
        "authority-b",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    assert_eq!(
        a.semantic_eq(&different_authority),
        Err(SemanticComparisonRefusal::ForeignAuthority)
    );

    let different_kind = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "ir-node",
        b"order-42",
    );
    assert_eq!(
        a.semantic_eq(&different_kind),
        Err(SemanticComparisonRefusal::ForeignKind)
    );

    // Same authority and kind: a genuine semantic verdict, not a refusal.
    let same_authority_and_kind_different_object = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-99",
    );
    assert_eq!(
        a.semantic_eq(&same_authority_and_kind_different_object),
        Ok(false)
    );
    assert_eq!(a.semantic_eq(&base()), Ok(true));
}

/// FR-287-AC-2 / FR-035 revision validation: missing or empty key members
/// refuse without a generated, normalized or default identity.
#[test]
fn ac2_empty_components_refuse() {
    assert_eq!(
        Revision::new("", "rev-1").unwrap_err(),
        IdentityError::EmptyRevisionNamespace
    );
    assert_eq!(
        Revision::new("orders", "").unwrap_err(),
        IdentityError::EmptyRevisionValue
    );
    assert_eq!(
        DigestDomain::new("").unwrap_err(),
        IdentityError::EmptyDigestDomain
    );
    assert_eq!(
        AuthorityIdentity::new("").unwrap_err(),
        IdentityError::EmptyAuthorityIdentity
    );
    assert_eq!(
        SubjectKind::new("").unwrap_err(),
        IdentityError::EmptySubjectKind
    );
    assert_eq!(
        ObjectIdentity::new(Vec::<u8>::new()).unwrap_err(),
        IdentityError::EmptyObjectIdentity
    );
}

/// FR-287-AC-3: display, locus, payload, trace, timestamp, arrival position
/// and transport status are not members. This type has no such fields to
/// carry them, so two references built from equal components are equal
/// regardless of how differently the caller might otherwise annotate the
/// call (there is nowhere on this type to attach such an annotation).
#[test]
fn ac3_no_non_member_fields_participate() {
    let from_one_call_site = base();
    let from_a_different_call_site = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    assert_eq!(from_one_call_site, from_a_different_call_site);

    // Serialize and confirm the wire form carries exactly the five members:
    // an accidental extra field would show up here.
    let json = serde_json::to_value(&from_one_call_site).unwrap();
    let object = json.as_object().expect("struct serializes as an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "authority_digest",
            "authority_identity",
            "authority_revision",
            "object_identity",
            "subject_kind",
        ]
    );
}

/// FR-287-AC-4: FR-287 states relationships are validated separately and do
/// not extend this value object — "Relationships ... are validated
/// separately and do not become additional identity components" (FR-035) /
/// "do not extend this value" (FR-287). Reusing a complete key with
/// incompatible relationship facts is therefore a refusal owned by the
/// binder that attaches those relationships, not by this value type, which
/// carries no relationship fields to be incompatible in the first place.
/// This test records that scope boundary rather than fabricating a
/// relationship concept this crate does not model.
#[test]
fn ac4_relationship_contradiction_is_out_of_scope_for_this_value_type() {
    let a = base();
    let b = base();
    // The value type itself only ever answers canonical-key equality; it has
    // no notion of a bound relationship to contradict.
    assert_eq!(a, b);
}

/// FR-287-AC-5: permuting a collection preserves lexicographic key order
/// without adding a causal, delivery or retry relation.
#[test]
fn ac5_ordering_is_lexicographic_over_the_declared_key_sequence() {
    let low_authority = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    let high_authority = reference(
        "authority-b",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        b"order-42",
    );
    assert!(low_authority < high_authority);

    let mut shuffled = vec![high_authority.clone(), low_authority.clone()];
    shuffled.sort();
    assert_eq!(
        shuffled,
        vec![low_authority.clone(), high_authority.clone()]
    );

    // Object-identity bytes order by exact byte order, not by any parsed
    // meaning.
    let object_a = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        &[0x01],
    );
    let object_b = reference(
        "authority-a",
        "orders",
        "rev-1",
        "sha256-jcs",
        1,
        "domain-package",
        &[0x02],
    );
    assert!(object_a < object_b);
}

/// FR-201: digests in different domains are never equal, even with
/// coincident bytes; and an underscored spelling is a different label than a
/// hyphenated one (FR-201-AC-1/AC-2).
#[test]
fn digest_domain_is_exact_and_distinct_across_labels() {
    let same_bytes_domain_a =
        AuthorityDigest::new(DigestDomain::new("sha256-jcs").unwrap(), digest(7));
    let same_bytes_domain_b = AuthorityDigest::new(
        DigestDomain::new("quire.simulation.state-key/v1").unwrap(),
        digest(7),
    );
    assert_ne!(same_bytes_domain_a, same_bytes_domain_b);

    assert_ne!(
        DigestDomain::new("package-fingerprint").unwrap(),
        DigestDomain::new("package_fingerprint").unwrap()
    );
}

/// FR-035 revision-namespace validation: a namespace and value are each
/// required and independent; a valid revision retains both exactly.
#[test]
fn revision_retains_namespace_and_value_exactly() {
    let revision = Revision::new("orders", "rev-1").unwrap();
    assert_eq!(revision.namespace(), "orders");
    assert_eq!(revision.value(), "rev-1");

    // Same value, different namespace: distinct revisions.
    let other_namespace = Revision::new("shipments", "rev-1").unwrap();
    assert_ne!(revision, other_namespace);
}
