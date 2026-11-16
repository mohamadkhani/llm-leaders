//! Arena WebDev leaderboard scores (rank + Elo).
//!
//! Source of record: Arena's official public dataset
//! [`lmarena-ai/leaderboard-dataset`](https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset)
//! on Hugging Face (CC BY 4.0) — the `webdev` subset's `latest` split, which
//! snapshots the [arena.ai WebDev leaderboard](https://arena.ai/leaderboard/code/webdev)
//! daily. The leaderboard page itself sits behind a Cloudflare bot challenge
//! that rejects non-browser clients with HTTP 403, so the dataset is the
//! reliable machine-readable channel for the same data.
//!
//! The dataset carries no arena `model_key`, so one is synthesized from the
//! display name (see [`normalize_model_key`]); `Score.display_name` keeps the
//! raw name for the matcher's proximity tie-break.

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hugging Face datasets-server rows endpoint for the WebDev leaderboard.
/// Paged with `offset`/`length` query parameters.
const ROWS_URL: &str = "https://datasets-server.huggingface.co/rows?dataset=lmarena-ai/leaderboard-dataset&config=webdev&split=latest";
/// The main WebDev leaderboard is the `overall` category. Other categories in
/// the subset (`webdev-html`, `webdev-react`, `image_to_webdev`, …) are
/// separate boards and must not mix in.
const CATEGORY: &str = "overall";
const PAGE_LEN: usize = 100;
// 5h, aligned with the benchmarks cache: both sides of the rank columns
// move on the same cadence.
const TTL: Duration = Duration::from_secs(5 * 60 * 60);
/// Bump when the cache layout changes; a mismatch is a cache miss.
const SCHEMA_VERSION: u32 = 2;

/// On-disk cache shape for arena scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cache {
    pub schema_version: u32,
    pub fetched_at: u64, // unix seconds
    pub entries: Vec<Score>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    pub model_key: String,
    pub display_name: String,
    pub rating: f64,
    pub rank: u64,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub model_url: Option<String>,
}

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("arena.json"))
}

/// Load valid (TTL-fresh) cache, else None. A parse failure or schema
/// mismatch is a miss (refetch), never a hard error.
fn load_fresh() -> Result<Option<Cache>> {
    let path = cache_path()?;
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(cache) = serde_json::from_str::<Cache>(&content) else {
        return Ok(None);
    };
    if cache.schema_version != SCHEMA_VERSION {
        return Ok(None);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now.saturating_sub(cache.fetched_at) < TTL.as_secs() {
        Ok(Some(cache))
    } else {
        Ok(None)
    }
}

fn save(cache: &Cache) -> Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cache)?)?;
    Ok(())
}

/// One page of the datasets-server `/rows` response.
#[derive(Debug, Deserialize)]
struct HfPage {
    #[serde(default)]
    rows: Vec<HfRowWrap>,
}

#[derive(Debug, Deserialize)]
struct HfRowWrap {
    row: HfRow,
}

/// One row of the dataset's `webdev` subset. Unknown columns (license,
/// confidence bounds, …) are ignored; a malformed row is skipped, never fatal.
#[derive(Debug, Deserialize)]
struct HfRow {
    model_name: String,
    organization: Option<String>,
    rating: Option<f64>,
    rank: Option<u64>,
    category: Option<String>,
}

/// Synthesize the row's `model_key` from its display name.
///
/// The dataset carries no arena key, so the key is the name with its
/// parenthetical *configuration* metadata — `(Max)`, `(xHigh, Codex Harness)`,
/// `(High, 32K, 20251101)` — reduced to the one token that is identity: a full
/// `YYYYMMDD` date. Config tiers must not leak into identity, or
/// `GPT-5.6 Sol (xHigh, Codex Harness)` would refuse to match
/// `openai/gpt-5.6-sol`. Dots survive (they carry versions: `qwen3.8`), and
/// the result is lowercased and hyphenated to the shape of an arena key.
fn normalize_model_key(name: &str) -> String {
    let paren = Regex::new(r"\(([^)]*)\)").unwrap();
    let mut out = String::new();
    let mut last = 0;
    for caps in paren.captures_iter(name) {
        let whole = caps.get(0).unwrap();
        out.push_str(&name[last..whole.start()]);
        for tok in caps[1].split(|c: char| !c.is_ascii_alphanumeric()) {
            if is_full_date(tok) {
                out.push(' ');
                out.push_str(tok);
            }
        }
        last = whole.end();
    }
    out.push_str(&name[last..]);

    let mut key = String::new();
    for c in out.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            key.push(c.to_ascii_lowercase());
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    key.trim_matches('.').trim_matches('-').to_string()
}

/// A full `YYYYMMDD` date token — the only parenthetical token that is
/// identity rather than configuration.
fn is_full_date(tok: &str) -> bool {
    tok.len() == 8 && tok.starts_with("202") && tok.chars().all(|c| c.is_ascii_digit())
}

/// Map one dataset row to a [`Score`]. `None` for other categories or
/// incomplete rows — a malformed row is skipped, never fatal.
fn to_score(raw: HfRow) -> Option<Score> {
    if raw.category.as_deref() != Some(CATEGORY) || raw.model_name.is_empty() {
        return None;
    }
    let (rank, rating) = (raw.rank?, raw.rating?);
    Some(Score {
        model_key: normalize_model_key(&raw.model_name),
        display_name: raw.model_name,
        rating,
        rank,
        organization: raw.organization,
        model_url: None,
    })
}

/// Fetch the whole `overall` leaderboard from the dataset, paging the
/// datasets-server rows endpoint.
fn fetch() -> Result<Vec<Score>> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .build()?;

    let mut out: Vec<Score> = Vec::new();
    let mut offset = 0usize;
    loop {
        let url = format!("{ROWS_URL}&offset={offset}&length={PAGE_LEN}");
        let page: HfPage = client
            .get(&url)
            .send()
            .context("arena dataset request failed")?
            .error_for_status()
            .context("arena dataset request failed")?
            .json()
            .context("arena dataset response did not parse")?;
        let fetched = page.rows.len();
        for wrap in page.rows {
            if let Some(score) = to_score(wrap.row) {
                out.push(score);
            }
        }
        if fetched < PAGE_LEN {
            break;
        }
        offset += fetched;
    }

    if out.is_empty() {
        anyhow::bail!("arena dataset returned 0 rows — response format may have changed");
    }

    // Sort by rating desc
    out.sort_by(|a, b| {
        b.rating
            .partial_cmp(&a.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// Get arena scores: fresh cache, else fetch + cache.
///
/// Returns `None` on failure after one stderr warning — arena is an optional
/// source, so ranks and Elo degrade to `—` rather than failing the run.
pub fn get_scores(refresh: bool) -> Option<Vec<Score>> {
    if !refresh {
        match load_fresh() {
            Ok(Some(c)) => return Some(c.entries),
            Ok(None) => {}
            Err(e) => eprintln!("warning: reading arena cache failed: {e}"),
        }
    }
    match fetch() {
        Ok(entries) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Err(e) = save(&Cache {
                schema_version: SCHEMA_VERSION,
                fetched_at: now,
                entries: entries.clone(),
            }) {
                eprintln!("warning: writing arena cache failed: {e}");
            }
            Some(entries)
        }
        Err(e) => {
            eprintln!(
                "warning: arena leaderboard (lmarena-ai/leaderboard-dataset) unavailable ({e}) — arena ranks and Elo hidden"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parenthetical configuration must not leak into the key; dates must
    /// survive. Each case is a real `model_name` from the dataset.
    #[test]
    fn model_key_normalization() {
        let cases = [
            ("Claude Fable 5.2 (xHigh)", "claude-fable-5.2"),
            ("Claude Opus 5.5 (Max)", "claude-opus-5.5"),
            ("GPT-5.6 Sol (xHigh, Codex Harness)", "gpt-5.6-sol"),
            (
                "DeepSeek-V4-Pro (High, 20260813)",
                "deepseek-v4-pro-20260813",
            ),
            (
                "Claude Opus 4.5 (High, 32K, 20251101)",
                "claude-opus-4.5-20251101",
            ),
            ("Qwen3.8-Max (0902)", "qwen3.8-max"),
            ("Kimi K3 (Max)", "kimi-k3"),
            (
                "DeepSeek-V4.1-Flash-20261017 (Max)",
                "deepseek-v4.1-flash-20261017",
            ),
            ("GLM-5.3-Flash", "glm-5.3-flash"),
        ];
        for (name, want) in cases {
            assert_eq!(normalize_model_key(name), want, "{name}");
        }
    }

    /// A dataset page parses to scores, non-`overall` rows are dropped, and
    /// rows missing a rank or rating are skipped rather than fatal.
    #[test]
    fn page_parses_and_filters_category() {
        let page = r#"{"rows":[
            {"row":{"model_name":"Claude Fable 5.2 (xHigh)","organization":"anthropic","license":"Proprietary","rating":1825.96,"rank":1,"category":"overall","leaderboard_publish_date":"2026-11-15"}},
            {"row":{"model_name":"Other Model (Max)","organization":"other","license":"Proprietary","rating":1500.0,"rank":7,"category":"image_to_webdev","leaderboard_publish_date":"2026-11-15"}},
            {"row":{"model_name":"Unranked Model","organization":"other","license":"Proprietary","rating":null,"rank":null,"category":"overall","leaderboard_publish_date":"2026-11-15"}}
        ]}"#;
        let parsed: HfPage = serde_json::from_str(page).expect("dataset page parses");

        let out: Vec<Score> = parsed
            .rows
            .into_iter()
            .filter_map(|w| to_score(w.row))
            .collect();

        assert_eq!(out.len(), 1, "only the complete overall row survives");
        assert_eq!(out[0].model_key, "claude-fable-5.2");
        assert_eq!(out[0].display_name, "Claude Fable 5.2 (xHigh)");
        assert_eq!(out[0].rank, 1);
        assert_eq!(out[0].organization.as_deref(), Some("anthropic"));
    }
}
