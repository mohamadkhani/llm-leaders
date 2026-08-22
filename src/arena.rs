use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const WEBDEV_URL: &str = "https://arena.ai/leaderboard/code/webdev";
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// On-disk cache shape for arena scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cache {
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

/// Load valid (TTL-fresh) cache, else None.
fn load_fresh() -> Result<Option<Cache>> {
    let path = cache_path()?;
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let cache: Cache = serde_json::from_str(&content).context("parsing arena cache")?;
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

/// Helper struct to deserialize raw JSON objects from Arena's embedded payload.
#[derive(Debug, Deserialize)]
struct RawArenaEntry {
    #[serde(rename = "modelKey", default)]
    model_key: String,
    #[serde(rename = "modelDisplayName", default)]
    display_name: String,
    #[serde(default)]
    rating: f64,
    #[serde(default)]
    rank: u64,
    #[serde(rename = "modelOrganization", default)]
    organization: Option<String>,
    #[serde(rename = "modelUrl", default)]
    model_url: Option<String>,
}

/// Scrape the WebDev leaderboard by fetching the Next.js RSC payload
/// embedded in the server-rendered HTML and deserializing full model objects.
fn scrape() -> Result<Vec<Score>> {
    let resp = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
        .build()?
        .get(WEBDEV_URL)
        .send()
        .context("arena.ai leaderboard request failed")?
        .error_for_status()?
        .text()?;

    let obj_re = Regex::new(r#"\{[^{}]*\\"modelKey\\":\\"[^\\"]+\\"[^{}]*\}"#).unwrap();

    let mut out: Vec<Score> = Vec::new();
    for mat in obj_re.find_iter(&resp) {
        let unescaped = mat.as_str().replace("\\\"", "\"").replace("\\\\", "\\");
        if let Ok(raw) = serde_json::from_str::<RawArenaEntry>(&unescaped) {
            if !raw.model_key.is_empty() && !raw.display_name.is_empty() && raw.rating > 0.0 {
                out.push(Score {
                    model_key: raw.model_key,
                    display_name: raw.display_name,
                    rating: raw.rating,
                    rank: raw.rank,
                    organization: raw.organization,
                    model_url: raw.model_url,
                });
            }
        }
    }

    if out.is_empty() {
        anyhow::bail!("arena scrape found 0 entries — page format may have changed");
    }

    // Sort by rating desc
    out.sort_by(|a, b| b.rating.partial_cmp(&a.rating).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Get arena scores: fresh cache, else scrape + cache.
pub fn get_scores(refresh: bool) -> Result<Vec<Score>> {
    if !refresh {
        if let Some(c) = load_fresh()? {
            return Ok(c.entries);
        }
    }
    let entries = scrape()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    save(&Cache { fetched_at: now, entries: entries.clone() })?;
    Ok(entries)
}
