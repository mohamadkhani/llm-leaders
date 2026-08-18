use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// The curated list of OpenRouter IDs, one per non-comment, non-empty line.
/// Preserves file order and comments on save.
pub fn read_ids(path: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading model list {}", path.display()))?;
    Ok(text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect())
}

/// Append IDs that aren't already present, preserving comments.
pub fn add_ids(path: &Path, to_add: &[String]) -> Result<Vec<String>> {
    let mut existing: Vec<String> = read_ids(path)?
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let mut appended = Vec::new();
    for id in to_add {
        if !existing.iter().any(|e| e == id) {
            existing.push(id.clone());
            appended.push(id.clone());
        }
    }
    if !appended.is_empty() {
        ensure_trailing_newline(path)?;
        let mut to_write = String::new();
        for id in &appended {
            to_write.push_str(id);
            to_write.push('\n');
        }
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .with_context(|| format!("opening {} for append", path.display()))?;
        use std::io::Write;
        f.write_all(to_write.as_bytes())?;
    }
    Ok(appended)
}

/// Remove the given IDs from the file, preserving comments.
pub fn remove_ids(path: &Path, to_remove: &[String]) -> Result<Vec<String>> {
    let remove_set: std::collections::HashSet<&str> =
        to_remove.iter().map(|s| s.as_str()).collect();
    let text = fs::read_to_string(path)?;
    let mut out = String::new();
    let mut removed = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') && remove_set.contains(trimmed) {
            removed.push(trimmed.to_string());
            continue; // drop the line
        }
        out.push_str(line);
        out.push('\n');
    }
    fs::write(path, out)?;
    Ok(removed)
}

fn ensure_trailing_newline(path: &Path) -> Result<()> {
    let text = fs::read_to_string(path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        let mut f = fs::OpenOptions::new().append(true).open(path)?;
        f.write_all(b"\n")?;
    }
    Ok(())
}

use std::io::Write as _;
