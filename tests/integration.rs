use quire_canonical::hello;

#[test]
fn hello_is_non_empty() {
    assert!(!hello().is_empty());
}
