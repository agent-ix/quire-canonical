// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX

use quire_canonical::{
    CanonicalValue, CanonicalValueError, Error, Limits, ProtocolViolation, Sink, Writer,
};

#[test]
fn construction_requires_one_complete_canonical_value() {
    let value = CanonicalValue::new(br#"{"a":[1,true]}"#, 64).expect("canonical value");
    assert_eq!(value.as_bytes(), br#"{"a":[1,true]}"#);

    assert!(matches!(
        CanonicalValue::new(b"{", 64),
        Err(CanonicalValueError::Read(_))
    ));
    assert!(matches!(
        CanonicalValue::new(b"null false", 64),
        Err(CanonicalValueError::Read(_))
    ));
    assert!(matches!(
        CanonicalValue::new(b" {\"a\":1}", 64),
        Err(CanonicalValueError::NotCanonical)
    ));
    assert!(matches!(
        CanonicalValue::new(br#"{"b":1,"a":2}"#, 64),
        Err(CanonicalValueError::NotCanonical)
    ));
    assert!(CanonicalValue::new(b"null", 4).is_ok());
    assert!(matches!(
        CanonicalValue::new(b"null", 3),
        Err(CanonicalValueError::Read(_))
    ));
}

#[test]
fn embeds_values_at_top_level_and_inside_arrays_and_objects() {
    let top = CanonicalValue::new(br#"{"a":1}"#, 64).expect("canonical value");
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(64));
    writer.canonical_value(&top).expect("write top-level value");
    assert_eq!(writer.finish().expect("complete value"), 7);
    assert_eq!(bytes, br#"{"a":1}"#);

    let nested = CanonicalValue::new(br#"{"z":false}"#, 64).expect("canonical value");
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(64));
    writer.begin_object().expect("begin object");
    writer.name("items").expect("member name");
    writer.begin_array().expect("begin array");
    writer.canonical_value(&nested).expect("write nested value");
    writer.null().expect("write next array item");
    writer.end_array().expect("end array");
    writer.end_object().expect("end object");
    writer.finish().expect("complete value");
    assert_eq!(bytes, br#"{"items":[{"z":false},null]}"#);
}

#[test]
fn embedded_bytes_are_spliced_as_one_sink_write() {
    #[derive(Default)]
    struct Writes(Vec<Vec<u8>>);
    impl Sink for Writes {
        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
            self.0.push(bytes.to_vec());
            Ok(())
        }
    }

    let value = CanonicalValue::new(br#"{"a":[1,2]}"#, 64).expect("canonical value");
    let mut sink = Writes::default();
    let mut writer = Writer::new(&mut sink, Limits::new(64));
    writer.begin_array().expect("begin array");
    writer
        .canonical_value(&value)
        .expect("splice canonical bytes");
    writer.end_array().expect("end array");
    writer.finish().expect("complete value");

    assert_eq!(
        sink.0,
        [b"[".to_vec(), value.as_bytes().to_vec(), b"]".to_vec()]
    );
}

#[test]
fn byte_limit_counts_embedded_bytes_with_surrounding_events() {
    let value = CanonicalValue::new(br#"{"a":1}"#, 64).expect("canonical value");
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(9));
    writer.begin_array().expect("begin array");
    writer
        .canonical_value(&value)
        .expect("write value at exact budget");
    writer.end_array().expect("end array at exact budget");
    assert_eq!(writer.finish().expect("complete value"), 9);
    assert_eq!(bytes, br#"[{"a":1}]"#);

    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(7));
    writer.begin_array().expect("begin array");
    assert!(matches!(
        writer.canonical_value(&value),
        Err(Error::Limit(limit)) if limit.required == 8 && limit.bound == 7
    ));
    assert!(matches!(
        writer.null(),
        Err(Error::Protocol(ProtocolViolation::AfterRefusal))
    ));
}

#[test]
fn sink_failure_from_spliced_bytes_refuses_writer() {
    struct Refuse;
    impl Sink for Refuse {
        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
            if bytes == b"null" {
                Err(Error::Internal {
                    invariant: "test sink refusal",
                })
            } else {
                Ok(())
            }
        }
    }

    let value = CanonicalValue::new(b"null", 16).expect("canonical value");
    let mut sink = Refuse;
    let mut writer = Writer::new(&mut sink, Limits::new(16));
    assert!(matches!(
        writer.canonical_value(&value),
        Err(Error::Internal {
            invariant: "test sink refusal"
        })
    ));
    assert!(matches!(
        writer.null(),
        Err(Error::Protocol(ProtocolViolation::AfterRefusal))
    ));
}
