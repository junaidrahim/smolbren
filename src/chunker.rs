//! Split note bodies into embedding-sized chunks along markdown boundaries.
//!
//! Blocks (paragraphs, headings, atomic code fences) are packed greedily up
//! to `MAX_CHUNK_BYTES`, and consecutive chunks share a small tail overlap so
//! sentences straddling a cut remain searchable in at least one chunk.

/// ~800 tokens at the usual ~4 bytes/token — comfortably inside the
/// embedding model's context window (see `embedder::MAX_TOKENS`).
pub const MAX_CHUNK_BYTES: usize = 3200;
/// Tail of the previous chunk repeated at the start of the next (qmd uses 15%).
pub const OVERLAP_BYTES: usize = MAX_CHUNK_BYTES * 15 / 100;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub seq: i32,
    pub text: String,
}

/// Chunk a markdown body. Whitespace-only input yields no chunks — the
/// caller decides what to embed for empty notes.
pub fn chunk_markdown(body: &str) -> Vec<Chunk> {
    if body.trim().is_empty() {
        return Vec::new();
    }
    let blocks: Vec<String> = split_blocks(body)
        .into_iter()
        .flat_map(|b| {
            if b.len() > MAX_CHUNK_BYTES {
                hard_split(&b, MAX_CHUNK_BYTES)
            } else {
                vec![b]
            }
        })
        .collect();

    let mut texts: Vec<String> = Vec::new();
    let mut cur = String::new();
    // Bytes of `cur` that are only overlap carried from the previous chunk;
    // a chunk is flushed only once it holds content beyond its seed.
    let mut seed_len = 0usize;
    for block in blocks {
        if !cur.is_empty() && cur.len() + 2 + block.len() > MAX_CHUNK_BYTES {
            if cur.len() > seed_len {
                let overlap = tail_lines(&cur, OVERLAP_BYTES);
                texts.push(std::mem::take(&mut cur));
                cur = overlap;
            } else {
                cur.clear();
            }
            seed_len = cur.len();
        }
        if cur.is_empty() {
            cur = block;
        } else {
            cur.push_str("\n\n");
            cur.push_str(&block);
        }
    }
    if cur.len() > seed_len {
        texts.push(cur);
    }
    texts
        .into_iter()
        .enumerate()
        .map(|(i, text)| Chunk { seq: i as i32, text })
        .collect()
}

/// Split into markdown blocks: blank-line-separated paragraphs, with
/// headings starting a fresh block and fenced code kept atomic.
fn split_blocks(body: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut fence: Option<&str> = None;
    let flush = |cur: &mut Vec<&str>, blocks: &mut Vec<String>| {
        if !cur.is_empty() {
            blocks.push(cur.join("\n"));
            cur.clear();
        }
    };
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            cur.push(line);
            if trimmed.starts_with(marker) {
                fence = None;
                flush(&mut cur, &mut blocks);
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut cur, &mut blocks);
            fence = Some(if trimmed.starts_with("```") { "```" } else { "~~~" });
            cur.push(line);
            continue;
        }
        if is_heading(line) {
            flush(&mut cur, &mut blocks);
            cur.push(line);
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut cur, &mut blocks);
            continue;
        }
        cur.push(line);
    }
    flush(&mut cur, &mut blocks);
    blocks
}

fn is_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ')
}

/// Split an oversized block at line boundaries, falling back to
/// char-boundary cuts for single lines longer than `max`.
fn hard_split(block: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in block.lines() {
        if line.len() > max {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let mut start = 0;
            while start < line.len() {
                let mut end = (start + max).min(line.len());
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                out.push(line[start..end].to_string());
                start = end;
            }
            continue;
        }
        if !cur.is_empty() && cur.len() + 1 + line.len() > max {
            out.push(std::mem::take(&mut cur));
        }
        if cur.is_empty() {
            cur.push_str(line);
        } else {
            cur.push('\n');
            cur.push_str(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Trailing whole lines of `s` totalling at most `want` bytes.
fn tail_lines(s: &str, want: usize) -> String {
    let mut taken: Vec<&str> = Vec::new();
    let mut total = 0usize;
    for line in s.lines().rev() {
        let add = line.len() + usize::from(!taken.is_empty());
        if total + add > want {
            break;
        }
        taken.push(line);
        total += add;
    }
    taken.reverse();
    let overlap = taken.join("\n");
    if overlap.trim().is_empty() { String::new() } else { overlap }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_yield_nothing() {
        assert!(chunk_markdown("").is_empty());
        assert!(chunk_markdown("  \n\n\t\n").is_empty());
    }

    #[test]
    fn short_body_is_single_chunk() {
        let chunks = chunk_markdown("# Title\n\nA paragraph.\n\nAnother one.");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].seq, 0);
        assert!(chunks[0].text.contains("A paragraph."));
        assert!(chunks[0].text.contains("Another one."));
    }

    #[test]
    fn long_body_splits_with_overlap() {
        // ~2.5KB paragraph of ten ~250-byte lines, so a line-boundary
        // overlap can be carried between chunks.
        let line = format!("{}end", "word ".repeat(49));
        let para = vec![line; 10].join("\n");
        let body = format!("{para}\n\n{para}\n\n{para}");
        let chunks = chunk_markdown(&body);
        assert!(chunks.len() >= 2, "expected multiple chunks, got {}", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.seq, i as i32);
            assert!(c.text.len() <= MAX_CHUNK_BYTES + OVERLAP_BYTES + 2);
        }
        // Each later chunk is seeded with the tail of its predecessor.
        let tail = tail_lines(&chunks[0].text, OVERLAP_BYTES);
        assert!(!tail.is_empty());
        assert!(chunks[1].text.starts_with(&tail));
    }

    #[test]
    fn code_fence_stays_atomic() {
        let filler = "x".repeat(3000);
        let fence = "```rust\nfn main() {}\nlet a = 1;\n```";
        let body = format!("{filler}\n\n{fence}");
        let chunks = chunk_markdown(&body);
        let holder: Vec<_> = chunks.iter().filter(|c| c.text.contains("fn main()")).collect();
        assert!(!holder.is_empty());
        for c in holder {
            assert!(c.text.contains("```rust"), "fence opening split away");
            assert!(c.text.matches("```").count() >= 2, "fence closing split away");
        }
    }

    #[test]
    fn heading_starts_new_block() {
        let s1 = "a".repeat(3190); // heading block no longer fits alongside
        let body = format!("{s1}\n## Section Two\ncontent here");
        let chunks = chunk_markdown(&body);
        assert!(chunks.len() >= 2);
        // The heading was cut away from the filler despite no blank line.
        assert!(!chunks[0].text.contains("Section Two"));
        let with_heading = chunks.iter().find(|c| c.text.contains("## Section Two")).unwrap();
        assert!(with_heading.text.contains("content here"));
    }

    #[test]
    fn oversized_single_line_is_hard_split() {
        let line = "z".repeat(10_000);
        let chunks = chunk_markdown(&line);
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| !c.text.is_empty()));
        assert!(chunks.iter().all(|c| c.text.len() <= MAX_CHUNK_BYTES + OVERLAP_BYTES + 2));
    }

    #[test]
    fn multibyte_input_never_panics() {
        let body = "héllo wörld émoji 🦀 ".repeat(400); // multibyte, > MAX_CHUNK_BYTES
        let chunks = chunk_markdown(&body);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.text.is_char_boundary(0) && c.text.is_char_boundary(c.text.len()));
        }
    }
}
