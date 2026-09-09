use leio_code::conversation::{glue_records, prepare, restrict_records};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tempfile::TempDir;

const CHAT: &str = "[01/09/26, 09:00:00] Alex: Habitual earlier wording\n[02/09/26, 10:00:00] Sam: Can you clarify this?\n[02/09/26, 10:01:00] Alex: Yes, tomorrow\ncontinued line\n[02/09/26, 10:01:00] Sam: Same second\n[02/09/26, 10:03:00] Alex: Later clarification\n";
fn fixture() -> TempDir {
    let t = TempDir::new().unwrap();
    fs::write(t.path().join("chat.txt"), CHAT).unwrap();
    t
}
fn packet(t: &TempDir, target: Option<&str>) -> Value {
    prepare(
        t.path(),
        &[PathBuf::from("chat.txt")],
        "dmy",
        Some("Alex"),
        target,
        8,
    )
    .unwrap()
    .entities
    .remove(0)
}

#[test]
fn target_context_preserves_multiline_and_never_leaks_future_or_same_timestamp() {
    let t = fixture();
    let latest = packet(&t, None);
    let id = latest["evidence_records"]
        .as_object()
        .unwrap()
        .values()
        .find(|v| v["text"] == "Yes, tomorrow\ncontinued line")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let p = packet(&t, Some(id));
    assert_eq!(p["inventory"]["messages"], 5);
    assert_eq!(
        p["contexts"]["later_retrospective_only"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        p["contexts"]["same_timestamp_unordered"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for id in p["contexts"]["prediction_input"].as_array().unwrap() {
        assert!(
            p["evidence_records"][id.as_str().unwrap()]["timestamp"]
                .as_str()
                .unwrap()
                < "2026-09-02T10:01:00"
        );
    }
    for id in p["contexts"]["prior_account_history"].as_array().unwrap() {
        assert_ne!(
            p["evidence_records"][id.as_str().unwrap()]["session"],
            p["evidence_records"][p["selection"]["target_id"].as_str().unwrap()]["session"]
        );
    }
    assert!(p["authorship"]["probability"].is_null());
    assert_eq!(p["method_plan"][0]["status"], "proposed-not-run");
    assert!(!t.path().join(".leio-code").exists());
}

#[test]
fn zip_source_hashes_container_and_member_and_refuses_ambiguous_transcripts() {
    let t = fixture();
    let write_zip = |names: &[&str]| {
        let mut z = zip::ZipWriter::new(fs::File::create(t.path().join("chat.zip")).unwrap());
        for n in names {
            z.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(CHAT.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    };
    write_zip(&["_chat.txt"]);
    let env = prepare(t.path(), &["chat.zip".into()], "dmy", None, None, 8).unwrap();
    assert_eq!(env.entities[0]["inventory"]["messages"], 5);
    assert_eq!(
        env.entities[0]["sources"][0]["content_sha256"],
        packet(&t, None)["sources"][0]["content_sha256"]
    );
    write_zip(&["one.txt", "two.txt"]);
    assert!(prepare(t.path(), &["chat.zip".into()], "dmy", None, None, 8).is_err());
}

#[test]
fn date_convention_invalid_input_and_path_escape_are_explicit() {
    let t = fixture();
    let mdy = prepare(t.path(), &["chat.txt".into()], "mdy", None, None, 8).unwrap();
    assert_eq!(
        mdy.entities[0]["inventory"]["first_timestamp"],
        "2026-01-09T09:00:00"
    );
    fs::write(
        t.path().join("bad.txt"),
        "[31/02/26, 10:00:00] Alex: impossible",
    )
    .unwrap();
    assert!(prepare(t.path(), &["bad.txt".into()], "dmy", None, None, 8).is_err());
    fs::write(
        t.path().join("bad.txt"),
        "[02/09/26, 10:00 PM] Alex: unsupported",
    )
    .unwrap();
    assert!(prepare(t.path(), &["bad.txt".into()], "dmy", None, None, 8).is_err());
    let outside = fixture();
    assert!(
        prepare(
            t.path(),
            &[outside.path().join("chat.txt")],
            "dmy",
            None,
            None,
            8
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path().join("chat.txt"), t.path().join("link.txt"))
            .unwrap();
        assert!(prepare(t.path(), &["link.txt".into()], "dmy", None, None, 8).is_err());
    }
}

#[test]
fn category_restrictions_compose_and_gluing_rejects_conflicting_evidence() {
    let all = BTreeMap::from([("a".to_string(), json!(1)), ("b".to_string(), json!(2))]);
    let ids = BTreeSet::from(["a".to_string()]);
    let r = restrict_records(&all, &ids).unwrap();
    assert_eq!(restrict_records(&r, &ids).unwrap(), r);
    assert_eq!(glue_records(&all, &r).unwrap(), all);
    assert!(glue_records(&all, &BTreeMap::from([("a".to_string(), json!(3))])).is_err());
    assert!(restrict_records(&all, &BTreeSet::from(["missing".to_string()])).is_err());
}

#[test]
fn normalized_ids_are_traceable_and_duplicate_ids_refused_when_targeting() {
    let t = fixture();
    let data = json!([{"id":"x","timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"original"},{"id":"x","timestamp":"2026-09-02T00:00:00","sender":"Alex","text":"different"}]);
    fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
    assert!(
        prepare(
            t.path(),
            &["messages.json".into()],
            "dmy",
            None,
            Some("x"),
            8
        )
        .is_err()
    );
    let e = prepare(t.path(), &["messages.json".into()], "dmy", None, None, 8).unwrap();
    assert_eq!(e.entities[0]["baseline"]["text_messages"], 1);
    assert!(
        e.entities[0]["selection"]["target_id"]
            .as_str()
            .unwrap()
            .starts_with("msg:")
    );
}

#[test]
fn actual_cli_does_not_index_or_journal_transcripts() {
    let t = fixture();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .args(["--json", "--repo"])
        .arg(t.path())
        .args(["conversation", "--source", "chat.txt"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let e: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(e["kind"], "conversation");
    assert!(!t.path().join(".leio-code").exists());
}

#[test]
fn curated_exclusions_and_duplicate_source_provenance_survive() {
    let t = fixture();
    let data = json!([
      {"timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"quoted earlier", "eligible":false,"quote_reason":"pasted text"},
      {"timestamp":"2026-09-02T00:00:00","sender":"Alex","text":"ordinary example"},
      {"timestamp":"2026-09-03T00:00:00","sender":"Alex","text":"current"}
    ]);
    fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
    fs::write(t.path().join("copy.json"), data.to_string()).unwrap();
    let e = prepare(
        t.path(),
        &["messages.json".into(), "copy.json".into()],
        "dmy",
        None,
        None,
        8,
    )
    .unwrap();
    let p = &e.entities[0];
    assert_eq!(p["inventory"]["messages"], 3);
    assert_eq!(p["sources"].as_array().unwrap().len(), 2);
    assert_eq!(p["sources"][1]["duplicate_content_skipped"], true);
    assert_eq!(p["baseline"]["text_messages"], 1);
    assert_eq!(
        p["contexts"]["prior_account_history"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        p["evidence_records"]
            .as_object()
            .unwrap()
            .values()
            .any(|r| r["imported_annotations"]["quote_reason"] == "pasted text")
    );
}

#[test]
fn normalized_chat_labels_cannot_silently_merge_conversations() {
    let t = fixture();
    let data = json!([
        {"chat":"one","timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"unrelated"},
        {"chat":"two","timestamp":"2026-09-02T00:00:00","sender":"Alex","text":"target"}
    ]);
    fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
    let e = prepare(t.path(), &["messages.json".into()], "dmy", None, None, 8).unwrap();
    assert_eq!(e.entities[0]["contexts"]["prediction_input"], json!([]));
    assert_eq!(e.entities[0]["baseline"]["text_messages"], 0);
    for invalid in [json!(1), json!({"id":"one"}), json!(["one"]), json!(false)] {
        let mut invalid_data = data.clone();
        invalid_data[0]["chat"] = invalid;
        fs::write(t.path().join("messages.json"), invalid_data.to_string()).unwrap();
        assert!(prepare(t.path(), &["messages.json".into()], "dmy", None, None, 8).is_err());
    }
}

#[test]
fn marker_words_in_ordinary_prose_remain_account_examples() {
    let t = fixture();
    let texts = [
        "I omitted the details in yesterday's email",
        "Esse detalhe foi omitido no resumo",
        "The notice says this message was deleted, but I can still see it",
        "<Media omitted>",
        "imagem ocultada",
        "<anexado: photo.jpg>",
        "This message was deleted",
        "Mensagem apagada",
        "target",
    ];
    let rows: Vec<_> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| {
            json!({
                "timestamp":format!("2026-09-{:02}T00:00:00", i + 1),
                "sender":"Alex", "text":text
            })
        })
        .collect();
    fs::write(t.path().join("messages.json"), json!(rows).to_string()).unwrap();
    let e = prepare(t.path(), &["messages.json".into()], "dmy", None, None, 20).unwrap();
    let p = &e.entities[0];
    assert_eq!(p["baseline"]["text_messages"], 3);
    assert_eq!(
        p["contexts"]["prior_account_history"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    for row in p["evidence_records"].as_object().unwrap().values() {
        let text = row["text"].as_str().unwrap();
        let expected = if texts[..3].contains(&text) || text == "target" {
            "text"
        } else if texts[3..6].contains(&text) {
            "media-marker"
        } else {
            "deleted-marker"
        };
        assert_eq!(row["kind"], expected, "{text}");
    }
}

#[test]
fn reversed_source_order_never_becomes_predictive_context() {
    let t = fixture();
    let data = json!([
        {"id":"a","timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"prior"},
        {"id":"b","timestamp":"2026-09-03T00:00:00","sender":"Alex","text":"target"},
        {"id":"c","timestamp":"2026-09-02T00:00:00","sender":"Alex","text":"uncertain order"}
    ]);
    fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
    let e = prepare(
        t.path(),
        &["messages.json".into()],
        "dmy",
        None,
        Some("b"),
        8,
    )
    .unwrap();
    let p = &e.entities[0];
    for section in ["antecedents", "prior_account_history", "prediction_input"] {
        let ids = p["contexts"][section].as_array().unwrap();
        assert_eq!(ids.len(), 1, "{section}");
        assert_eq!(
            p["evidence_records"][ids[0].as_str().unwrap()]["imported_id"],
            "a"
        );
    }
    assert_eq!(p["baseline"]["text_messages"], 1);
    let conflicts = p["source_order_conflicts_retrospective_only"]
        .as_object()
        .unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts.values().next().unwrap()["imported_id"], "c");
    assert!(
        conflicts
            .keys()
            .all(|id| p["evidence_records"].get(id).is_none())
    );
    assert_eq!(p["contexts"].as_object().unwrap().len(), 6);
}

#[test]
fn identity_fields_and_account_inventory_have_explicit_limits() {
    let t = fixture();
    for key in ["account", "chat", "id"] {
        let mut data = json!([{"timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"short"}]);
        data[0][key] = json!("a".repeat(1025));
        fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
        assert!(
            prepare(t.path(), &["messages.json".into()], "dmy", None, None, 1).is_err(),
            "{key}"
        );
    }
    let data: Vec<_> = (0..1025)
        .map(|i| {
            json!({
                "timestamp":"2026-09-01T00:00:00", "sender":format!("account-{i}"), "text":"short"
            })
        })
        .collect();
    fs::write(t.path().join("messages.json"), json!(data).to_string()).unwrap();
    assert!(prepare(t.path(), &["messages.json".into()], "dmy", None, None, 1).is_err());
    fs::write(
        t.path().join("chat.txt"),
        format!("[01/09/26, 10:00:00] {}: short\n", "a".repeat(1025)),
    )
    .unwrap();
    assert!(prepare(t.path(), &["chat.txt".into()], "dmy", None, None, 1).is_err());
}

#[test]
fn terminal_view_separates_contexts_escapes_controls_and_does_not_journal() {
    let t = fixture();
    let data = json!([
        {"id":"prior","timestamp":"2026-09-01T00:00:00","sender":"Alex","text":"prior wording"},
        {"id":"target","timestamp":"2026-09-03T00:00:00","sender":"Alex","text":"current\n\u{1b}[31mreply"},
        {"id":"conflict","timestamp":"2026-09-02T00:00:00","sender":"Alex","text":"uncertain chronology"},
        {"id":"later","timestamp":"2026-09-04T00:00:00","sender":"Sam","text":"later response"}
    ]);
    fs::write(t.path().join("messages.json"), data.to_string()).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_leio-code"))
        .arg("--repo")
        .arg(t.path())
        .args([
            "conversation",
            "--source",
            "messages.json",
            "--target",
            "target",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Target (1)"));
    assert!(text.contains("Antecedents (strictly prior) (1)"));
    assert!(text.contains("Later context (retrospective only) (1)"));
    assert!(text.contains("Source-order conflicts (retrospective only) (1)"));
    assert!(text.contains("Warnings"));
    assert!(text.contains("authorship is not established"));
    assert!(text.contains("--json"));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("\\u{1b}[31mreply"));
    let prior = text
        .split("Antecedents (strictly prior)")
        .nth(1)
        .unwrap()
        .split("Prior account history")
        .next()
        .unwrap();
    assert!(prior.contains("prior wording"));
    assert!(!prior.contains("uncertain chronology"));
    assert!(!prior.contains("later response"));
    assert!(!t.path().join(".leio-code").exists());
}

#[test]
fn malformed_source_errors_identify_the_file_and_json_record() {
    let t = fixture();
    for wrapped in [false, true] {
        let rows = json!([
            {"timestamp":"2026-09-01T00:00:00", "sender":"Alex", "text":"valid"},
            {"sender":"Alex", "text":"private text must not appear in diagnostics"}
        ]);
        let data = if wrapped {
            json!({"messages":rows})
        } else {
            rows
        };
        fs::write(t.path().join("bad.json"), data.to_string()).unwrap();
        let error = prepare(
            t.path(),
            &["chat.txt".into(), "bad.json".into()],
            "dmy",
            None,
            None,
            8,
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("bad.json"), "{message}");
        assert!(
            message.contains(if wrapped { "/messages/1" } else { "/1" }),
            "{message}"
        );
        assert!(message.contains("message timestamp missing"));
        assert!(!message.contains("private text"));
    }
    let error = prepare(t.path(), &["missing.txt".into()], "dmy", None, None, 8).unwrap_err();
    assert!(format!("{error:#}").contains("missing.txt"));
    let error = prepare(
        t.path(),
        &["missing\u{1b}[31m.txt".into()],
        "dmy",
        None,
        None,
        8,
    )
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(!message.contains('\u{1b}'));
    assert!(message.contains("\\u{1b}[31m.txt"));
}

#[test]
fn terminal_view_traces_records_to_selected_sources_and_explains_bounds() {
    let t = fixture();
    let mut zip = zip::ZipWriter::new(fs::File::create(t.path().join("chat.zip")).unwrap());
    zip.start_file("nested/_chat.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(CHAT.as_bytes()).unwrap();
    zip.finish().unwrap();
    fs::write(
        t.path().join("messages.json"),
        json!({"messages":[{"id":"json-target", "timestamp":"2026-09-05T00:00:00", "sender":"Alex", "text":"Separate source"}]}).to_string(),
    )
    .unwrap();
    let sources = ["chat.zip".into(), "chat.txt".into(), "messages.json".into()];
    let latest = prepare(t.path(), &sources, "dmy", Some("Alex"), None, 1).unwrap();
    let view = leio_code::conversation::render_text(&latest).unwrap();
    assert!(view.contains("Selection: latest matching message, not anomaly selection"));
    assert!(view.contains("Context limit per section: 1; counts show selected records."));
    assert!(view.contains("Source: messages.json | /messages/0"));

    let zip_digest = latest.entities[0]["sources"][0]["content_sha256"]
        .as_str()
        .unwrap();
    let target = format!("msg:{zip_digest}:3");
    let explicit = prepare(t.path(), &sources, "dmy", None, Some(&target), 1).unwrap();
    let before = serde_json::to_value(&explicit).unwrap();
    let view = leio_code::conversation::render_text(&explicit).unwrap();
    assert!(view.contains("Selection: explicit target"));
    assert!(view.contains("Source: chat.zip -> nested/_chat.txt | lines:3-4"));
    assert!(view.contains("chat.txt (duplicate transcript skipped)"));
    assert!(view.contains("bounded evidence packet"));
    assert_eq!(serde_json::to_value(&explicit).unwrap(), before);
    assert!(!t.path().join(".leio-code").exists());
}

#[test]
fn terminal_view_escapes_bidi_controls_but_preserves_original_evidence() {
    let t = fixture();
    let controls = "\u{061c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}";
    let original = format!("Olá مرحبا שלום {controls}");
    let filename = "messages\u{202e}.json";
    fs::write(
        t.path().join(filename),
        json!([{"timestamp":"2026-09-01T00:00:00", "sender":original, "text":original}])
            .to_string(),
    )
    .unwrap();
    let envelope = prepare(t.path(), &[filename.into()], "dmy", None, None, 1).unwrap();
    let before = serde_json::to_value(&envelope).unwrap();
    let view = leio_code::conversation::render_text(&envelope).unwrap();
    for control in controls.chars() {
        assert!(!view.contains(control), "unescaped {control:?}");
        assert!(view.contains(&control.escape_default().to_string()));
    }
    assert!(view.contains("Olá مرحبا שלום"));
    assert!(view.contains("Source: messages\\u{202e}.json | /0"));
    let packet = &envelope.entities[0];
    let record = &packet["evidence_records"][packet["selection"]["target_id"].as_str().unwrap()];
    assert_eq!(record["text"], original);
    assert_eq!(record["account"], original);
    assert_eq!(serde_json::to_value(&envelope).unwrap(), before);
    assert!(!t.path().join(".leio-code").exists());
}
