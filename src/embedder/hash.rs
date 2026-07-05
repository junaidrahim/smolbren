//! Deterministic feature-hashing embedder for tests. Token overlap between
//! two texts raises their cosine similarity, which is enough to assert
//! ranking behavior end-to-end without downloading a model.

use super::{Embedder, EXPECTED_DIM};

pub const ID: &str = "hash-test-embedder";

pub struct HashEmbedder;

impl HashEmbedder {
    fn embed_one(text: &str) -> Vec<f32> {
        let mut v = vec![0f32; EXPECTED_DIM];
        for token in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            let digest = blake3::hash(token.as_bytes());
            let h = u64::from_le_bytes(digest.as_bytes()[..8].try_into().unwrap());
            let idx = (h % EXPECTED_DIM as u64) as usize;
            let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

impl Embedder for HashEmbedder {
    fn dim(&self) -> usize {
        EXPECTED_DIM
    }

    // Embeds raw text, not the model prompts: the fixed scaffolding
    // tokens ("task", "query", "title", …) would otherwise dominate the
    // similarity of short documents and drown out real term overlap.
    fn embed_docs(&mut self, pairs: &[(String, String)]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(pairs.iter().map(|(_title, text)| Self::embed_one(text)).collect())
    }

    fn embed_query(&mut self, query: &str) -> anyhow::Result<Vec<f32>> {
        Ok(Self::embed_one(query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn deterministic_and_normalized() {
        let a = HashEmbedder::embed_one("the quick brown fox");
        let b = HashEmbedder::embed_one("the quick brown fox");
        assert_eq!(a, b);
        let norm = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn token_overlap_raises_similarity() {
        let q = HashEmbedder::embed_one("quixotic zeppelin adventures");
        let hit = HashEmbedder::embed_one("a note about quixotic zeppelin travel");
        let miss = HashEmbedder::embed_one("groceries milk eggs bread");
        assert!(cosine(&q, &hit) > cosine(&q, &miss));
    }

    #[test]
    fn empty_text_is_zero_vector_without_panic() {
        let v = HashEmbedder::embed_one("");
        assert_eq!(v.len(), EXPECTED_DIM);
        assert!(v.iter().all(|x| *x == 0.0));
    }
}
