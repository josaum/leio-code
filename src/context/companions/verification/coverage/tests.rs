use super::*;

fn fixture_report(
    expression: &str,
    creation: &str,
    second_test: &str,
) -> Option<(usize, String, String)> {
    let program = format!(
        r#"
        #[cfg(test)]
        mod trials {{
            fn scratch() {{
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                let dir = std::env::temp_dir().join({expression});
                {creation}
            }}
            #[test] fn first() {{ scratch(); scratch(); }}
            {second_test}
        }}"#
    );
    let mut inventory = Inventory::default();
    inventory
        .observe(
            "crate/src/transport.rs",
            &mut rust_parser().unwrap(),
            &program,
        )
        .unwrap();
    inventory.fixtures.pop()
}

#[test]
fn shared_nonexclusive_clock_fixture_needs_two_distinct_local_tests() {
    let name = r#"format!("garden-{stamp}")"#;
    let create = "std::fs::create_dir_all(&dir).unwrap();";
    let second = "#[test] fn another_case() { scratch(); }";
    let (count, path, why) = fixture_report(name, create, second).unwrap();
    assert_eq!(count, 2);
    assert_eq!(path, "crate/src/transport.rs");
    assert!(why.contains("scratch"));
    for other in [
        "",
        "#[cfg(test)] fn helper() { scratch(); }",
        "#[test] fn elsewhere() { remote::scratch(); }",
        "mod nested { #[test] fn other() { scratch(); } }",
    ] {
        assert!(fixture_report(name, create, other).is_none(), "{other}");
    }
    assert!(fixture_report(name, "std::fs::create_dir(&dir).unwrap();", second).is_none());
    assert!(
        fixture_report(
            name,
            "let dir = caller_supplies_a_path(); std::fs::create_dir_all(&dir).unwrap();",
            second
        )
        .is_none()
    );
    for safe in [
        r#"format!("{}-{}", stamp, next_sequence())"#,
        r#"format!("{stamp}-{identity}")"#,
        r#"format!("{}", std::time::SystemTime::now().with_random_suffix())"#,
    ] {
        assert!(fixture_report(safe, create, second).is_none(), "{safe}");
    }
}

#[test]
fn public_peer_gap_requires_calls_in_the_same_production_function() {
    let mut inventory = Inventory::default();
    let mut parser = rust_parser().unwrap();
    for (path, source) in [
        (
            "src/pair.rs",
            "pub fn decode() {} pub fn encode() {} #[test] fn roundtrip() { encode(); }",
        ),
        (
            "src/separate.rs",
            "pub fn open_gate() {} pub fn close_gate() {} #[test] fn closes() { close_gate(); }",
        ),
        (
            "src/user.rs",
            "fn conversion(){ pair::decode(); pair::encode(); } fn arrival(){ open_gate(); } fn leave(){ close_gate(); }",
        ),
    ] {
        inventory.observe(path, &mut parser, source).unwrap();
    }
    let peers: Vec<_> = inventory
        .leads(vec![])
        .into_iter()
        .filter(|(_, why)| why.contains("locally tested public peer"))
        .collect();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].0, "src/pair.rs");
    assert!(peers[0].1.contains("decode"));
}
