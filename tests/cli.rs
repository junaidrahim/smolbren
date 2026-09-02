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

fn run_with_env(config: &Path, args: &[&str], key: &str, value: &str) -> Output {
    Command::cargo_bin("smolbren")
        .unwrap()
        .env(key, value)
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

fn final_stderr_json(out: &Output) -> serde_json::Value {
    let stderr = String::from_utf8_lossy(&out.stderr);
    serde_json::from_str(stderr.lines().last().expect("stderr has an error line"))
        .expect("last stderr line is JSON")
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

    let listed = run_json(&config, &["vault", "list"]);
    assert_eq!(listed[0]["embedding_status_error"], "index missing");

    // .obsidian/junk.json must be skipped: 9 markdown notes.
    let stats = run_json(&config, &["index"]);
    assert_eq!(stats["scanned"], 9);
    assert_eq!(stats["added"], 9);
    assert_eq!(stats["edges"], 15);
    assert_eq!(stats["unresolved_edges"], 0);

    let listed = run_json(&config, &["vault", "list"]);
    assert_eq!(listed[0]["indexed_notes"], 9);
    assert_eq!(listed[0]["embedded_notes"], 0);
    assert_eq!(listed[0]["embedding_lag_notes"], 9);
    assert_eq!(listed[0]["embeddings_stale"], true);

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
    let plain = run(
        &config,
        &["get", "blogs/context-engineering", "--body", "--format", "text"],
    );
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(plain.trim_start().starts_with("# Context engineering"));
    assert!(plain.contains("Draft thesis"));

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

    // Anonymous nodes use the same catch-all note dataset, both alone and at
    // either end of typed or anonymous relationships.
    let result = run_json(&config, &["query", "MATCH (n) RETURN count(n)"]);
    assert_eq!(result["rows"][0]["count(n)"], 9);
    let result = run_json(
        &config,
        &["query", "MATCH (a)-[:mentions]->(b) RETURN count(b)"],
    );
    assert_eq!(result["rows"][0]["count(b)"], 6);
    let result = run_json(
        &config,
        &["query", "MATCH (a)-[r]->(b) RETURN count(b)"],
    );
    assert_eq!(result["rows"][0]["count(b)"], 15);

    // Scalar frontmatter is queryable, including ISO date ranges.
    let result = run_json(
        &config,
        &[
            "query",
            "MATCH (b:blog) WHERE b.status = $status AND b.created >= $cutoff RETURN b.id, b.status, b.created",
            "--param",
            "status=draft",
            "--param",
            "cutoff=2026-05-01",
        ],
    );
    assert_eq!(result["rows"].as_array().unwrap().len(), 1);
    assert_eq!(result["rows"][0]["b.id"], "blogs/context-engineering");
    assert_eq!(result["rows"][0]["b.status"], "draft");
    assert_eq!(result["rows"][0]["b.created"], "2026-05-10");

    let hits = run_json(&config, &["search", "context engineering", "--type", "blog", "--limit", "3"]);
    assert_eq!(hits[0]["id"], "blogs/context-engineering");
    assert!(hits[0]["score"].as_f64().unwrap() > 0.0);
    assert!(hits[0]["snippet"].as_str().unwrap().contains("context engineering"));

    let scoped = run_json(&config, &["search", "context", "--path", "blogs/"]);
    assert!(!scoped.as_array().unwrap().is_empty());
    assert!(scoped
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["path"].as_str().unwrap().starts_with("blogs/")));
}

#[test]
fn index_and_embed_report_progress_on_stderr() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");

    run_json(&config, &["vault", "add", "progress", fixture_vault().to_str().unwrap()]);

    let indexed = run(&config, &["index"]);
    assert!(indexed.status.success());
    let index_stats: serde_json::Value =
        serde_json::from_slice(&indexed.stdout).expect("index stdout is JSON");
    assert_eq!(index_stats["scanned"], 9);
    let index_log = String::from_utf8_lossy(&indexed.stderr);
    assert!(index_log.contains("index: discovered 9 notes"));
    assert!(index_log.contains("index: parsed 9/9 files (100.0%)"));
    assert!(index_log.contains("index: refreshing 0/7 search indexes"));
    assert!(index_log.contains("index: refreshed 7/7 indexes (100.0%)"));
    assert!(index_log.contains("index: complete"));
    assert!(index_log.contains("ETA 0s"));

    let embedded = run_hash(&config, &["embed"]);
    assert!(embedded.status.success());
    let embed_stats: serde_json::Value =
        serde_json::from_slice(&embedded.stdout).expect("embed stdout is JSON");
    assert_eq!(embed_stats["embedded"], 9);
    let embed_log = String::from_utf8_lossy(&embedded.stderr);
    assert!(embed_log.contains("embed: scanned 9 notes"));
    assert!(embed_log.contains("embed: model ready"));
    assert!(embed_log.contains("embed: embedded"));
    assert!(embed_log.contains("chunks (100.0%)"));
    assert!(embed_log.contains("embed: complete"));
    assert!(embed_log.contains("ETA 0s"));
}

#[test]
fn agent_docs_are_emitted_from_the_canonical_skill() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");
    let out = run(&config, &["docs", "--agent"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        include_str!("../skills/smolbren/SKILL.md")
    );
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
    let unresolved = run_json(&config, &["unresolved"]);
    assert_eq!(unresolved.as_array().unwrap().len(), 3);
    assert!(unresolved
        .as_array()
        .unwrap()
        .iter()
        .all(|edge| edge["to_id"].as_str().unwrap().ends_with("prism")));
}

#[test]
fn failed_full_rebuild_preserves_last_good_index_and_repair_recovers() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");
    let vault = tmp.path().join("vault");
    copy_dir(&fixture_vault(), &vault);

    run_json(&config, &["vault", "add", "recovery", vault.to_str().unwrap()]);
    run_json(&config, &["index"]);

    let note_path = vault.join("blogs/context-engineering.md");
    let mut content = std::fs::read_to_string(&note_path).unwrap();
    content.push_str("\nTransactional rebuild marker.\n");
    std::fs::write(&note_path, content).unwrap();

    let failed = run_with_env(
        &config,
        &["index", "--full"],
        "SMOLBREN_TEST_FAIL_FULL_BEFORE_SWAP",
        "1",
    );
    assert_eq!(failed.status.code(), Some(1));
    let error = final_stderr_json(&failed);
    assert!(error["error"].as_str().unwrap().contains("before swapping"));

    // The live dataset is still the pre-edit version.
    let old = run_json(&config, &["get", "blogs/context-engineering", "--body"]);
    assert!(!old["body"].as_str().unwrap().contains("Transactional rebuild marker"));

    let repaired = run_json(&config, &["repair"]);
    assert_eq!(repaired["repaired"], true);
    let new = run_json(&config, &["get", "blogs/context-engineering", "--body"]);
    assert!(new["body"].as_str().unwrap().contains("Transactional rebuild marker"));
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
    let err = final_stderr_json(&out);
    assert_eq!(err["code"], "embeddings_missing");
    let out = run_hash(&config, &["search", "anything", "--hybrid"]);
    assert_eq!(out.status.code(), Some(6));

    let stats = run_hash_json(&config, &["embed"]);
    assert_eq!(stats["scanned"], 9);
    assert_eq!(stats["embedded"], 9);
    assert_eq!(stats["model"], "hash-test-embedder");
    assert!(stats["chunks_total"].as_u64().unwrap() >= 9);

    let listed = run_hash_json(&config, &["vault", "list"]);
    assert_eq!(listed[0]["embedding_lag_notes"], 0);
    assert_eq!(listed[0]["embeddings_stale"], false);

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

    let hits = run_hash_json(&config, &["similar", "context", "--path", "blogs/"]);
    assert!(hits
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["path"].as_str().unwrap().starts_with("blogs/")));

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

/// Full pipeline against the real EmbeddingGemma model. Downloads the
/// model into the test's temp dir on every run, so it is opt-in:
/// `cargo test -- --ignored`.
///
/// Uses a generated vault of 40 notes — more than one embedding batch —
/// because single-batch runs can't catch batching-mode bugs (the Q8
/// dynamic-quantization failure only appeared past 32 chunks).
#[test]
#[ignore = "downloads the embedding model (~hundreds of MB)"]
fn real_model_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let config = tmp.path().join("config.json");
    let vault = tmp.path().join("vault");
    std::fs::create_dir_all(vault.join("notes")).unwrap();
    for i in 0..40 {
        std::fs::write(
            vault.join(format!("notes/note-{i:02}.md")),
            format!("---\ntype: note\n---\n\n# Note {i}\n\nBody text for note number {i}.\n"),
        )
        .unwrap();
    }

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

    real(&["vault", "add", "real", vault.to_str().unwrap()]);
    real(&["index"]);
    let stats = real(&["embed"]);
    assert_eq!(stats["embedded"], 40);
    assert_eq!(stats["model"], "embeddinggemma-300m-onnx-q4");

    let hits = real(&["similar", "note number seventeen", "--limit", "3"]);
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
    let err = final_stderr_json(&out);
    assert_eq!(err["code"], "note_not_found");

    // Unknown vault name.
    let out = run(&config, &["--vault", "ghost", "types"]);
    assert_eq!(out.status.code(), Some(3));
}
