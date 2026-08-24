//! Client for OpenRouter's undocumented frontend catalog endpoint.
//!
//! This is the only source of the **permaslug** (OpenRouter's dated model
//! identifier, e.g. `deepseek/deepseek-v4-flash-20260731`) and of a **fresh
//! discount** figure. The public `/api/v1/models` API exposes neither:
//! `canonical_slug` is null for every model, so dates are invisible there.
//!
//! The endpoint is undocumented and unversioned, so every failure here is
//! non-fatal: callers fall back to v1-only structural matching. The core
//! price + arena-rank table must survive this endpoint disappearing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CATALOG_URL: &str = "https://openrouter.ai/api/frontend/v1/catalog/models";
const TTL: Duration = Duration::from_secs(5 * 60);

/// Bump when the cache layout changes; a mismatch is treated as a miss so a
/// stale file from an older schema is never misread.
const SCHEMA_VERSION: u32 = 1;

/// What we keep for one catalog entry, keyed by the v1 model id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Dated identifier — the join key the ranking endpoints use.
    pub permaslug: String,
    /// Dateless slug (the `id` of the canonical model, without `:free`/`:batch`).
    pub slug: String,
    pub name: String,
    pub short_name: String,
    pub author_display_name: Option<String>,
    /// Provider discount fraction (0.35 = 35% off) on this entry's endpoint.
    pub discount: Option<f64>,
}

/// On-disk cache shape.
#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    schema_version: u32,
    fetched_at: u64, // unix seconds
    /// Keyed by v1 model id (`model_variant_slug`), e.g. `z-ai/glm-5.2:free`.
    entries: HashMap<String, CatalogEntry>,
}

// --- wire format -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    #[serde(default)]
    data: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    permaslug: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    short_name: String,
    #[serde(default)]
    author_display_name: Option<String>,
    #[serde(default)]
    endpoint: Option<RawEndpoint>,
}

#[derive(Debug, Deserialize)]
struct RawEndpoint {
    /// The variant-qualified slug (`z-ai/glm-5.2:free`). This — not the bare
    /// `slug` — is what matches a v1 model `id`, so it is our lookup key.
    #[serde(default)]
    model_variant_slug: Option<String>,
    #[serde(default)]
    pricing: RawPricing,
}

#[derive(Debug, Default, Deserialize)]
struct RawPricing {
    #[serde(default)]
    discount: Option<f64>,
}

// --- cache -----------------------------------------------------------------

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("catalog.json"))
}

/// Load a TTL-fresh, schema-matching cache, else None.
fn load_fresh() -> Result<Option<HashMap<String, CatalogEntry>>> {
    let path = cache_path()?;
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    // A parse failure here means an old or corrupt file: treat as a miss
    // rather than an error, so a bad cache can never break a run.
    let Ok(cache) = serde_json::from_str::<Cache>(&content) else {
        return Ok(None);
    };
    if cache.schema_version != SCHEMA_VERSION {
        return Ok(None);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if now.saturating_sub(cache.fetched_at) < TTL.as_secs() {
        Ok(Some(cache.entries))
    } else {
        Ok(None)
    }
}

fn save(entries: &HashMap<String, CatalogEntry>) -> Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let cache = Cache {
        schema_version: SCHEMA_VERSION,
        fetched_at: now,
        entries: entries.clone(),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&cache)?)?;
    Ok(())
}

// --- fetch -----------------------------------------------------------------

fn fetch() -> Result<HashMap<String, CatalogEntry>> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .timeout(Duration::from_secs(25))
        .build()?;
    let resp: CatalogResponse = client
        .get(CATALOG_URL)
        .send()
        .context("frontend catalog request failed")?
        .error_for_status()?
        .json()
        .context("parsing frontend catalog JSON")?;
    Ok(index(resp.data))
}

/// Index raw rows by v1 model id. One row per provider endpoint is possible,
/// so keep the first entry that carries a discount — the discount is the
/// field that matters downstream and a zero would otherwise mask it.
fn index(rows: Vec<RawModel>) -> HashMap<String, CatalogEntry> {
    let mut out: HashMap<String, CatalogEntry> = HashMap::new();
    for row in rows {
        let Some(ep) = row.endpoint else { continue };
        let Some(id) = ep.model_variant_slug.clone() else {
            continue;
        };
        if row.permaslug.is_empty() {
            continue;
        }
        let discount = ep.pricing.discount.filter(|d| *d > 0.0);
        let entry = CatalogEntry {
            permaslug: row.permaslug,
            slug: row.slug,
            name: row.name,
            short_name: row.short_name,
            author_display_name: row.author_display_name,
            discount,
        };
        match out.get(&id) {
            // Prefer a row that actually has a discount over one that doesn't.
            Some(prev) if prev.discount.is_some() || discount.is_none() => {}
            _ => {
                out.insert(id, entry);
            }
        }
    }
    out
}

/// Get the frontend catalog: fresh cache, else fetch + cache.
///
/// Returns `None` on any failure after emitting a single stderr warning —
/// callers must degrade to v1-only structural matching rather than fail.
pub fn get(refresh: bool) -> Option<HashMap<String, CatalogEntry>> {
    if !refresh {
        match load_fresh() {
            Ok(Some(entries)) => return Some(entries),
            Ok(None) => {}
            Err(e) => eprintln!("warning: reading catalog cache failed: {e}"),
        }
    }
    match fetch() {
        Ok(entries) => {
            if let Err(e) = save(&entries) {
                eprintln!("warning: writing catalog cache failed: {e}");
            }
            Some(entries)
        }
        Err(e) => {
            eprintln!("warning: OpenRouter catalog unavailable ({e}) — matching without permaslugs");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> HashMap<String, CatalogEntry> {
        let raw = include_str!("../tests/fixtures/catalog.json");
        let resp: CatalogResponse = serde_json::from_str(raw).expect("fixture parses");
        index(resp.data)
    }

    #[test]
    fn resolves_permaslug_for_known_slugs() {
        let cat = fixture();
        for (id, want) in [
            ("z-ai/glm-5.3", "z-ai/glm-5.3-20260816"),
            ("z-ai/glm-4.5v", "z-ai/glm-4.5v"),
            ("deepseek/deepseek-v4-flash-0731", "deepseek/deepseek-v4-flash-20260731"),
            ("deepseek/deepseek-v4-flash", "deepseek/deepseek-v4-flash-20260423"),
            ("deepseek/deepseek-v4-pro-0813", "deepseek/deepseek-v4-pro-20260813"),
            ("anthropic/claude-opus-5", "anthropic/claude-opus-5-20260723"),
            ("moonshotai/kimi-k2.7-code", "moonshotai/kimi-k2.7-code-20260612"),
        ] {
            let got = cat.get(id).unwrap_or_else(|| panic!("{id} missing from catalog"));
            assert_eq!(got.permaslug, want, "permaslug for {id}");
        }
    }

    /// Tier variants are keyed separately, and share the canonical permaslug.
    #[test]
    fn tier_variants_are_distinct_keys() {
        let cat = fixture();
        for id in ["z-ai/glm-5.2", "z-ai/glm-5.2:free", "z-ai/glm-5.2:batch"] {
            let e = cat.get(id).unwrap_or_else(|| panic!("{id} missing"));
            assert_eq!(e.permaslug, "z-ai/glm-5.2-20260616");
            assert_eq!(e.slug, "z-ai/glm-5.2");
        }
    }

    /// A zero discount is normalized to None so callers can treat
    /// "has a discount" as a simple Option check.
    #[test]
    fn zero_discount_is_none() {
        let cat = fixture();
        let e = cat.get("z-ai/glm-5.3").expect("glm-5.3");
        assert!(e.discount.is_none(), "expected no discount, got {:?}", e.discount);
    }
}
