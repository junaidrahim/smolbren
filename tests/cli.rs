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

/// Like `run`, but with the deterministic hash embedder so tests never
/// download the real model.
fn run_hash(config: &Path, args: &[&str]) -> Output {
    Command::cargo_bin("smolbren")
        .unwrap()
        .env("SMOLBREN_EMBEDDER", "hash")
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap()
}

fn run_hash_json(config: &Path, args: &[&str]) -> serde_json::Value {
    let out = run_hash(config, args);
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
fn embedding_and_similarity() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");
    let vault = tmp.path().join("vault");
    copy_dir(&fixture_vault(), &vault);

    run_hash_json(&config, &["vault", "add", "emb", vault.to_str().unwrap()]);
    run_hash_json(&config, &["index"]);

    // Similarity surfaces are gated on `embed` having run.
    let out = run_hash(&config, &["similar", "anything"]);
    assert_eq!(out.status.code(), Some(6));
    let err: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["code"], "embeddings_missing");
    let out = run_hash(&config, &["search", "anything", "--hybrid"]);
    assert_eq!(out.status.code(), Some(6));

    let stats = run_hash_json(&config, &["embed"]);
    assert_eq!(stats["scanned"], 9);
    assert_eq!(stats["embedded"], 9);
    assert_eq!(stats["model"], "hash-test-embedder");
    assert!(stats["chunks_total"].as_u64().unwrap() >= 9);

    // Incremental no-op.
    let stats = run_hash_json(&config, &["embed"]);
    assert_eq!(stats["embedded"], 0);
    assert_eq!(stats["unchanged"], 9);
    assert_eq!(stats["chunks_written"], 0);

    // Edit one note; only it gets re-embedded, and its new distinctive
    // tokens dominate similarity ranking under the hash embedder.
    let journal = vault.join("Journal/2026, June 04.md");
    let mut content = std::fs::read_to_string(&journal).unwrap();
    content.push_str("\nThe quixotic zeppelin hypothesis.\n");
    std::fs::write(&journal, content).unwrap();
    run_hash_json(&config, &["index"]);
    let stats = run_hash_json(&config, &["embed"]);
    assert_eq!(stats["embedded"], 1);
    assert_eq!(stats["unchanged"], 8);

    let hits = run_hash_json(&config, &["similar", "quixotic zeppelin", "--limit", "3"]);
    let top = &hits[0];
    assert_eq!(top["id"], "Journal/2026, June 04");
    assert!(top["score"].as_f64().unwrap() > 0.0);
    assert_eq!(top["type"], "journal");
    assert!(top["path"].as_str().unwrap().ends_with("June 04.md"));
    assert!(top["snippet"].as_str().unwrap().contains("retrieval quality"));
    assert!(top["chunk_seq"].as_i64().is_some());

    // Type filter applies to similarity results.
    let hits = run_hash_json(&config, &["similar", "context", "--type", "blog"]);
    assert!(!hits.as_array().unwrap().is_empty());
    for h in hits.as_array().unwrap() {
        assert_eq!(h["type"], "blog");
    }

    // Hybrid fuses both backends; the strong BM25+vector match wins and
    // component scores are exposed.
    let hits = run_hash_json(&config, &["search", "context engineering", "--hybrid", "--limit", "3"]);
    let top = &hits[0];
    assert_eq!(top["id"], "blogs/context-engineering");
    assert!(top["score"].as_f64().unwrap() > 0.0);
    assert!(top["bm25_score"].as_f64().unwrap() > 0.0);

    // Plain BM25 search is unchanged by the hybrid flag's existence.
    let hits = run_hash_json(&config, &["search", "context engineering", "--limit", "3"]);
    assert_eq!(hits[0]["id"], "blogs/context-engineering");

    // Deleting a note drops its embeddings on the next embed.
    std::fs::remove_file(vault.join("projects/prism.md")).unwrap();
    run_hash_json(&config, &["index"]);
    let stats = run_hash_json(&config, &["embed"]);
    assert_eq!(stats["removed"], 1);
    assert_eq!(stats["scanned"], 8);
}

/// Full pipeline against the real EmbeddingGemma model. Downloads
/// ~300MB into the test's temp dir on every run, so it is opt-in:
/// `cargo test -- --ignored`.
#[test]
#[ignore = "downloads the ~300MB embedding model"]
fn real_model_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");

    let real = |args: &[&str]| {
        let out = Command::cargo_bin("smolbren")
            .unwrap()
            .env_remove("SMOLBREN_EMBEDDER")
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).expect("stdout is JSON")
    };

    real(&["vault", "add", "real", fixture_vault().to_str().unwrap()]);
    real(&["index"]);
    let stats = real(&["embed"]);
    assert_eq!(stats["embedded"], 9);
    assert_eq!(stats["model"], "embeddinggemma-300m-onnx-q8");

    let hits = real(&["similar", "managing context windows for agents", "--limit", "3"]);
    assert!(!hits.as_array().unwrap().is_empty());
    assert!(hits[0]["score"].as_f64().unwrap() > 0.0);
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
