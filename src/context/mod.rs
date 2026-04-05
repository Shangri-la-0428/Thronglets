//! Context fingerprinting via SimHash.
//!
//! Converts arbitrary context strings into 128-bit (16-byte) fingerprints.
//! Similar contexts produce similar hashes — Hamming distance between
//! fingerprints approximates semantic distance.
//!
//! This is the machine-native replacement for human tag taxonomies.
//! AI agents describe their context in natural language; SimHash
//! makes it searchable without keyword matching.

use sha2::{Digest, Sha256};

/// 128-bit context fingerprint.
pub type ContextHash = [u8; 16];

/// Compute SimHash fingerprint from a context string.
///
/// Algorithm:
/// 1. Tokenize into shingles (character n-grams)
/// 2. Hash each shingle with SHA-256, take first 128 bits
/// 3. For each bit position: if bit=1 add weight, if bit=0 subtract weight
/// 4. Final fingerprint: bit=1 if sum > 0, bit=0 otherwise
pub fn simhash(text: &str) -> ContextHash {
    let text = text.to_lowercase();
    let shingles = extract_shingles(&text, 3);

    if shingles.is_empty() {
        return [0u8; 16];
    }

    // 128 bit positions, signed accumulators
    let mut v = [0i32; 128];

    for shingle in &shingles {
        let hash = hash_shingle(shingle);
        for (byte_idx, &byte) in hash.iter().enumerate() {
            for bit_idx in 0..8 {
                let pos = byte_idx * 8 + bit_idx;
                if byte & (1 << (7 - bit_idx)) != 0 {
                    v[pos] += 1;
                } else {
                    v[pos] -= 1;
                }
            }
        }
    }

    // Collapse to bits
    let mut result = [0u8; 16];
    for (pos, &val) in v.iter().enumerate() {
        if val > 0 {
            result[pos / 8] |= 1 << (7 - (pos % 8));
        }
    }
    result
}

/// Hamming distance between two context hashes.
/// Lower = more similar. Range: 0 (identical) to 128 (opposite).
pub fn hamming_distance(a: &ContextHash, b: &ContextHash) -> u32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x ^ y).count_ones())
        .sum()
}

/// Cosine-like similarity from Hamming distance. Range: 0.0 to 1.0.
pub fn similarity(a: &ContextHash, b: &ContextHash) -> f64 {
    1.0 - (hamming_distance(a, b) as f64 / 128.0)
}

/// Extract character n-gram shingles from text.
fn extract_shingles(text: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < n {
        if chars.is_empty() {
            return vec![];
        }
        return vec![chars.iter().collect()];
    }
    chars.windows(n).map(|w| w.iter().collect()).collect()
}

/// Format a context string to match `build_hook_context()` output format.
/// When `tool_name` is provided, wraps the raw context so that SimHash
/// proximity matching works between signal_post and prehook queries.
pub fn format_signal_context(tool_name: Option<&str>, raw_context: &str) -> String {
    match tool_name {
        Some(t) if t.eq_ignore_ascii_case("bash") => format!("bash: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("read") => format!("read file: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("write") => format!("write file: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("edit") => format!("edit file: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("grep") => {
            format!("search for '{raw_context}'")
        }
        Some(t) if t.eq_ignore_ascii_case("glob") => {
            format!("find files matching: {raw_context}")
        }
        Some(t) if t.eq_ignore_ascii_case("agent") => format!("agent: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("webfetch") => format!("fetch: {raw_context}"),
        Some(t) if t.eq_ignore_ascii_case("websearch") => format!("search: {raw_context}"),
        Some(other) => format!("{}: {raw_context}", other.to_lowercase()),
        None => raw_context.to_string(),
    }
}

/// Hash a shingle to 16 bytes (128 bits) using SHA-256 truncation.
fn hash_shingle(shingle: &str) -> [u8; 16] {
    let full = Sha256::digest(shingle.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_contexts_produce_identical_hashes() {
        let a = simhash("translate a technical document from Chinese to English");
        let b = simhash("translate a technical document from Chinese to English");
        assert_eq!(a, b);
        assert_eq!(hamming_distance(&a, &b), 0);
        assert!((similarity(&a, &b) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similar_contexts_are_close() {
        let a = simhash("translate a technical document from Chinese to English");
        let b = simhash("translate a legal document from Chinese to English");
        let dist = hamming_distance(&a, &b);
        // Similar sentences should have low hamming distance
        assert!(
            dist < 40,
            "similar contexts should be close, got distance {dist}"
        );
        assert!(similarity(&a, &b) > 0.6);
    }

    #[test]
    fn different_contexts_are_far() {
        let a = simhash("translate a technical document from Chinese to English");
        let b = simhash("deploy kubernetes cluster on AWS with terraform");
        let dist = hamming_distance(&a, &b);
        // Very different tasks should have higher distance
        assert!(
            dist > 20,
            "different contexts should be far, got distance {dist}"
        );
    }

    #[test]
    fn case_insensitive() {
        let a = simhash("Rust P2P Networking");
        let b = simhash("rust p2p networking");
        assert_eq!(a, b);
    }

    #[test]
    fn empty_string_produces_zero_hash() {
        let h = simhash("");
        assert_eq!(h, [0u8; 16]);
    }

    #[test]
    fn short_strings_still_work() {
        let a = simhash("AI");
        let b = simhash("ML");
        // Should not panic, should produce different hashes
        assert_ne!(a, b);
    }

    #[test]
    fn hamming_distance_symmetry() {
        let a = simhash("context alpha");
        let b = simhash("context beta");
        assert_eq!(hamming_distance(&a, &b), hamming_distance(&b, &a));
    }
}
