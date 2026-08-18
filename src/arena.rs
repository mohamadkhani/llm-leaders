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

/// Scrape the WebDev leaderboard by fetching the Next.js RSC payload
/// embedded in the server-rendered HTML and regex-extracting score rows.
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

    // RSC payload escapes JSON with backslashes: \"modelKey\":\"x\",\"modelDisplayName\":\"y\",\"rating\":123.4
    // Rank appears nearby as \"rank\":N. Capture each entry, then pair with its closest rank.
    let row_re = Regex::new(
        r#""modelKey\\":\\"([^\\]*)\\"[^}]*?"modelDisplayName\\":\\"([^\\]*)\\"[^}]*?"rating\\":([0-9.]+)"#,
    )
    .unwrap();
    let rank_re = Regex::new(r#""rank\\":([0-9]+)"#).unwrap();

    let mut out: Vec<Score> = Vec::new();
    for caps in row_re.captures_iter(&resp) {
        let pos = caps.get(0).unwrap().start();
        // Find the nearest preceding rank token.
        let rank = rank_re
            .captures_iter(&resp[..pos])
            .last()
            .and_then(|c| c[1].parse::<u64>().ok())
            .unwrap_or(0);
        out.push(Score {
            model_key: caps[1].to_string(),
            display_name: caps[2].to_string(),
            rating: caps[3].parse().unwrap_or(0.0),
            rank,
        });
    }
    if out.is_empty() {
        anyhow::bail!("arena scrape found 0 entries — page format may have changed");
    }
    // Sort by rating desc so lookup prefers best when duplicates exist.
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
