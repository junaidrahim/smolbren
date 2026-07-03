use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

fn fixture_vault() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture_vault")
}

fn run(config: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("smolbren")
        .unwrap()
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap()
}

fn run_json(config: &Path, args: &[&str]) -> serde_json::Value {
    let out = run(config, args);
    assert!(
        out.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

#[test]
fn end_to_end_readonly() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");

    let added = run_json(&config, &["vault", "add", "test", fixture_vault().to_str().unwrap()]);
    assert_eq!(added["default"], true);

    // .obsidian/junk.json must be skipped: 9 markdown notes.
    let stats = run_json(&config, &["index"]);
    assert_eq!(stats["scanned"], 9);
    assert_eq!(stats["added"], 9);
    assert_eq!(stats["edges"], 15);
    assert_eq!(stats["unresolved_edges"], 0);

    // Second run is a no-op.
    let stats = run_json(&config, &["index"]);
    assert_eq!(stats["unchanged"], 9);
    assert_eq!(stats["added"], 0);

    let types = run_json(&config, &["types"]);
    let blog = types
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "blog")
        .expect("blog type present");
    assert_eq!(blog["count"], 3);

    let edges = run_json(&config, &["edges"]);
    let mentions = edges
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["edge_type"] == "mentions")
        .expect("mentions edge type present");
    assert_eq!(mentions["count"], 6);

    let note = run_json(&config, &["get", "blogs/context-engineering"]);
    assert_eq!(note["type"], "blog");
    assert_eq!(note["title"], "Context engineering");
    assert_eq!(note["frontmatter"]["status"], "draft");
    assert!(note.get("body").is_none());
    let with_body = run_json(&config, &["get", "blogs/context-engineering", "--body"]);
    assert!(with_body["body"].as_str().unwrap().contains("Draft thesis"));

    let links = run_json(&config, &["links", "blogs/context-engineering", "--type", "mentions"]);
    let targets: Vec<&str> = links
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["to_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        vec![
            "projects/prism",
            "repos/smolbren",
            "blogs/context-development-lifecycle",
            "blogs/context-platform-engineering"
        ]
    );

    // Backlinks include the basename-resolved [[prism]] link.
    let backlinks = run_json(&config, &["backlinks", "projects/prism"]);
    let froms: Vec<&str> = backlinks
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["from_id"].as_str().unwrap())
        .collect();
    assert!(froms.contains(&"blogs/context-platform-engineering"));
    assert!(froms.contains(&"Journal/2026, June 01"));

    let result = run_json(
        &config,
        &["query", "MATCH (b:blog)-[:merged_from]->(x:Note) RETURN b.id, x.id"],
    );
    assert_eq!(result["rows"].as_array().unwrap().len(), 2);

    let hits = run_json(&config, &["search", "context engineering", "--type", "blog", "--limit", "3"]);
    assert_eq!(hits[0]["id"], "blogs/context-engineering");
    assert!(hits[0]["score"].as_f64().unwrap() > 0.0);
}

#[test]
fn incremental_mutations() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");
    let vault = tmp.path().join("vault");
    copy_dir(&fixture_vault(), &vault);

    run_json(&config, &["vault", "add", "mut", vault.to_str().unwrap()]);
    run_json(&config, &["index"]);

    // Edit exactly one file.
    let journal = vault.join("Journal/2026, June 04.md");
    let mut content = std::fs::read_to_string(&journal).unwrap();
    content.push_str("\nA quixotic new paragraph.\n");
    std::fs::write(&journal, content).unwrap();
    let stats = run_json(&config, &["index"]);
    assert_eq!(stats["updated"], 1);
    assert_eq!(stats["unchanged"], 8);

    let hits = run_json(&config, &["search", "quixotic"]);
    assert_eq!(hits[0]["id"], "Journal/2026, June 04");

    // Delete a note: it and its outgoing edges disappear.
    std::fs::remove_file(vault.join("projects/prism.md")).unwrap();
    let stats = run_json(&config, &["index"]);
    assert_eq!(stats["removed"], 1);
    assert_eq!(stats["edges"], 14);

    // Full rebuild re-resolves: dangling mentions of prism become unresolved.
    let stats = run_json(&config, &["index", "--full"]);
    assert_eq!(stats["unresolved_edges"], 3);
}

#[test]
fn error_codes() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");

    // No vault configured.
    let out = run(&config, &["types"]);
    assert_eq!(out.status.code(), Some(3));

    run_json(&config, &["vault", "add", "test", fixture_vault().to_str().unwrap()]);

    // Vault registered but not indexed.
    let out = run(&config, &["types"]);
    assert_eq!(out.status.code(), Some(5));

    run_json(&config, &["index"]);

    // Unknown note.
    let out = run(&config, &["get", "nope/missing"]);
    assert_eq!(out.status.code(), Some(4));
    let err: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["code"], "note_not_found");

    // Unknown vault name.
    let out = run(&config, &["--vault", "ghost", "types"]);
    assert_eq!(out.status.code(), Some(3));
}
