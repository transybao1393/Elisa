//! Token budget, trimming, compaction, project store and the system prompt. Contract in DESIGN.md.
use crate::frontend::SessionInfo;
use crate::types::{Block, Message, Role};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod cards;
mod facts;
mod history;
mod notes;
mod prompt;
mod quarantine;
mod store;
mod tokens;

pub use cards::*;
pub use facts::*;
pub use history::*;
pub use notes::*;
pub use prompt::*;
pub use quarantine::*;
pub use store::*;
pub use tokens::*;

/// First `n` chars of `s` (char-boundary safe).
fn head(s: &str, n: usize) -> &str { s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i]) }

/// Last `n` chars of `s` (char-boundary safe).
fn tail(s: &str, n: usize) -> &str {
    let len = s.chars().count();
    if n >= len { return s; }
    s.char_indices().nth(len - n).map_or("", |(i, _)| &s[i..])
}

fn capped(s: &str, cap: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= cap { s.to_string() } else { format!("{}\n[... cut at {cap} chars]", head(s, cap)) }
}

/// lowercase words only (punctuation, quotes and symbols as spaces), for comparing notes with the instructions
fn norm_words(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' }).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Share of the smaller text's distinct words (alphanumeric, lowercased, >= 2 chars) that the other one also has.
pub fn word_overlap(a: &str, b: &str) -> f32 {
    let words = |s: &str| -> HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_lowercase).collect()
    };
    let (a, b) = (words(a), words(b));
    let n = a.len().min(b.len());
    if n == 0 { return 0.0; }
    a.intersection(&b).count() as f32 / n as f32
}

/// Whitespace collapsed to single spaces, cut at `n` chars with `…`.
pub fn one_line(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n { format!("{}…", head(&flat, n)) } else { flat }
}
