use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

#[derive(Debug, Clone, Deserialize)]
pub struct ModelsResponse {
    pub data: Vec<Model>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub canonical_slug: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub description: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub context_length: Option<u64>,
    pub pricing: Pricing,
    /// Modalities this model can take and produce, from the v1 API's
    /// `architecture` object. Absent on older captures, so callers must
    /// treat it as `None`, not an error. This is the authoritative signal
    /// for "suitable for coding": a coding model outputs text only, while
    /// image/video/audio/speech/transcription/embeddings/rerank outputs
    /// mark a non-coding model.
    #[serde(default)]
    pub architecture: Option<Architecture>,
}

/// Modalities from the v1 API's `architecture` object.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Architecture {
    /// E.g. "text+image->text+audio". Not used directly — callers want
    /// the parsed `input_modalities` / `output_modalities`.
    #[serde(default)]
    pub modality: Option<String>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pricing {
    /// USD per token (prompt). Multiply by 1_000_000 for $/M.
    #[serde(default)]
    pub prompt: String,
    /// USD per token (completion).
    #[serde(default)]
    pub completion: String,
    /// Provider discount fraction (0.35 = 35% off); endpoints API only.
    /// (The catalog-level pricing has no discount; read it via BestPrice.)
    #[serde(default)]
    #[allow(dead_code)]
    pub discount: Option<f64>,
}

/// Cheapest-provider pricing for one model, from the /endpoints API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BestPrice {
    pub input: Option<f64>,
    pub output: Option<f64>,
    /// Provider discount fraction (0.35 = 35% off) on the cheapest endpoint.
    pub discount: Option<f64>,
    /// Cheapest provider's display name.
    #[allow(dead_code)]
    pub provider: Option<String>,
    /// When this entry was fetched (unix seconds). Per-entry, because a
    /// partial refetch rewrites the whole file: a single file-level timestamp
    /// would silently renew the entries it didn't touch.
    #[serde(default)]
    pub fetched_at: Option<u64>,
}

impl BestPrice {
    /// Is this entry discounted? A discount is time-limited, so it shortens
    /// the entry's usable life.
    fn is_discounted(&self) -> bool {
        self.discount.is_some_and(|d| d > 0.0)
    }

    /// Read the cache without TTL filtering — for the discount-diff that
    /// decides which entries to invalidate. Returns the raw last-known
    /// values, including expired ones: an expired entry is stale, not
    /// "discount went away".
    pub fn peek() -> Result<Option<std::collections::HashMap<String, BestPrice>>> {
        let Ok(content) = std::fs::read_to_string(Self::cache_path()?) else {
            return Ok(None);
        };
        let Ok(cache) = serde_json::from_str::<BestPriceCache>(&content) else {
            return Ok(None);
        };
        if cache.schema_version != BEST_PRICE_SCHEMA {
            return Ok(None);
        }
        Ok(Some(cache.prices))
    }
}

/// 1h for a plain entry; 15min for a discounted one. A discount is
/// time-limited, and when it lapses the price jumps with no other visible
/// change — so a discounted row is the one we can least afford to serve
/// stale.
const TTL: u64 = 60 * 60;
const DISCOUNT_TTL: u64 = 15 * 60;

/// Is a cached entry still usable? Pure so the policy can be tested without
/// touching the filesystem or the clock.
///
/// `file_stamp` is the whole-file timestamp, used only for entries written
/// before per-entry stamps existed.
fn entry_is_fresh(p: &BestPrice, file_stamp: u64, now: u64) -> bool {
    let stamped = p.fetched_at.unwrap_or(file_stamp);
    let ttl = if p.is_discounted() { DISCOUNT_TTL } else { TTL };
    now.saturating_sub(stamped) < ttl
}

#[derive(Debug, Deserialize)]
struct EndpointsResponse {
    data: EndpointsData,
}

#[derive(Debug, Deserialize)]
struct EndpointsData {
    #[serde(default)]
    endpoints: Vec<Endpoint>,
}

#[derive(Debug, Deserialize)]
struct Endpoint {
    #[serde(default)]
    name: String,
    #[serde(default)]
    pricing: Pricing,
}

/// On-disk cache shape for cheapest-provider prices.
#[derive(Debug, Serialize, Deserialize)]
pub struct BestPriceCache {
    pub schema_version: u32,
    pub fetched_at: u64, // unix seconds
    pub prices: std::collections::HashMap<String, BestPrice>,
}

/// Bump when the cache layout changes; a mismatch is treated as a miss so a
/// stale file from an older schema is never misread.
const BEST_PRICE_SCHEMA: u32 = 2;

impl BestPrice {
    fn cache_path() -> anyhow::Result<std::path::PathBuf> {
        let base = dirs::config_dir().context("no config dir on this platform")?;
        Ok(base.join("llm-leaders").join("best_prices.json"))
    }

    fn load() -> anyhow::Result<Option<std::collections::HashMap<String, BestPrice>>> {
        let Ok(content) = std::fs::read_to_string(Self::cache_path()?) else {
            return Ok(None);
        };
        // Old or corrupt cache: a miss, not an error — refetch rather than fail.
        let Ok(cache) = serde_json::from_str::<BestPriceCache>(&content) else {
            return Ok(None);
        };
        if cache.schema_version != BEST_PRICE_SCHEMA {
            return Ok(None);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        // Expired entries are simply dropped, which sends them down the
        // per-id refetch path in `fetch_best_prices`; their still-fresh
        // neighbours keep serving from cache.
        let mut prices = cache.prices;
        prices.retain(|_, p| entry_is_fresh(p, cache.fetched_at, now));
        Ok(Some(prices))
    }

    fn save(prices: &std::collections::HashMap<String, BestPrice>) -> anyhow::Result<()> {
        let path = Self::cache_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let cache = BestPriceCache {
            schema_version: BEST_PRICE_SCHEMA,
            fetched_at: now,
            prices: prices.clone(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&cache)?)?;
        Ok(())
    }
}

/// For each given model id, fetch its provider endpoints and pick the
/// cheapest (by input price — the OpenRouter website convention). Serves
/// from a 1h on-disk cache first; missing ids are fetched in parallel and
/// merged back into the cache. `refresh` bypasses the cache entirely.
///
/// `discount_changed` lists ids whose discount moved since their cached entry
/// was written (detected via the 5-min frontend catalog by the caller): those
/// entries are dropped so exactly those models re-fetch, even while their
/// neighbours keep serving from cache. This is the targeted invalidation that
/// keeps the `Disc` column intraday-accurate without re-running the fan-out
/// every 5 minutes.
pub fn fetch_best_prices(
    ids: &[String],
    refresh: bool,
    discount_changed: &[String],
) -> Result<std::collections::HashMap<String, BestPrice>> {
    use std::collections::HashMap;

    let mut cached: HashMap<String, BestPrice> = if refresh {
        HashMap::new()
    } else {
        let mut m = BestPrice::load()?.unwrap_or_default();
        // Targeted invalidation: only the models whose discount actually moved.
        for id in discount_changed {
            m.remove(id);
        }
        m
    };
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !cached.contains_key(*id))
        .cloned()
        .collect();

    if !missing.is_empty() {
        eprintln!(
            "fetching cheapest prices for {} model(s)...",
            missing.len()
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        let fetched = fetch_best_prices_uncached(&missing)?;
        for (id, bp) in &fetched {
            let mut bp = bp.clone();
            bp.fetched_at = Some(now); // stamp so a partial refetch doesn't renew its neighbours
            cached.insert(id.clone(), bp);
        }
        // Prune entries for models we'll never ask about again is overkill;
        // just persist the merged map.
        let _ = BestPrice::save(&cached);
    }
    Ok(cached)
}

fn fetch_best_prices_uncached(
    ids: &[String],
) -> Result<std::collections::HashMap<String, BestPrice>> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .build()?;
    let out: Mutex<HashMap<String, BestPrice>> = Mutex::new(HashMap::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());

    const THREADS: usize = 12;
    let chunk = (ids.len() + THREADS - 1) / THREADS;
    std::thread::scope(|s| {
        for group in ids.chunks(chunk.max(1)) {
            let client = &client;
            let out = &out;
            let errors = &errors;
            s.spawn(move || {
                for id in group {
                    let url = format!("{MODELS_URL}/{id}/endpoints");
                    let res = client
                        .get(&url)
                        .send()
                        .and_then(|r| r.error_for_status())
                        .and_then(|r| r.json::<EndpointsResponse>());
                    let parsed = match res {
                        Ok(p) => p,
                        // 404 = model id not in the catalog (e.g. stale
                        // models.txt entry) — expected, not worth a warning.
                        Err(e) if e.status() == Some(reqwest::StatusCode::NOT_FOUND) => continue,
                        Err(e) => {
                            errors.lock().unwrap().push(format!("{id}: {e}"));
                            continue;
                        }
                    };
                    // Cheapest endpoint by input price — the same convention
                    // as the OpenRouter website's model cards. Ties break on
                    // output price.
                    let best = parsed
                        .data
                        .endpoints
                        .iter()
                        .min_by(|a, b| {
                            let key = |e: &Endpoint| {
                                (
                                    e.pricing.prompt.parse::<f64>().unwrap_or(f64::INFINITY),
                                    e.pricing.completion.parse::<f64>().unwrap_or(f64::INFINITY),
                                )
                            };
                            key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    let entry = match best {
                        Some(e) => BestPrice {
                            input: price_per_m(&e.pricing.prompt),
                            output: price_per_m(&e.pricing.completion),
                            discount: e.pricing.discount,
                            provider: Some(e.name.clone()),
                            fetched_at: None, // stamped by the caller on insert
                        },
                        None => BestPrice::default(),
                    };
                    out.lock().unwrap().insert(id.clone(), entry);
                }
            });
        }
    });

    let errs = errors.into_inner().unwrap();
    if !errs.is_empty() {
        eprintln!("warning: cheapest-price lookup failed for {} model(s)", errs.len());
    }
    Ok(out.into_inner().unwrap())
}

impl Model {
    /// Is this model suitable for coding? Uses OpenRouter's own
    /// `architecture.output_modalities` from the v1 API — the authoritative
    /// signal. A coding model outputs text only; image/video/audio/speech/
    /// transcription/embeddings/rerank outputs mark a non-coding model.
    /// Returns `false` when the field is absent (older captures, v1-only
    /// runs), so callers must pair it with a fallback of their own — there
    /// is no hardcoded token list here, because a token that matches a
    /// coding model's name would drop it.
    pub fn non_coding(&self) -> bool {
        match &self.architecture {
            Some(a) => !a.output_modalities.is_empty() && a.output_modalities != ["text"],
            None => false,
        }
    }

    /// Input price in USD per million tokens. `Some(0.0)` for free models,
    /// `None` only when the value is unset/unparseable.
    pub fn input_per_m(&self) -> Option<f64> {
        price_per_m(&self.pricing.prompt)
    }

    /// Output price in USD per million tokens.
    pub fn output_per_m(&self) -> Option<f64> {
        price_per_m(&self.pricing.completion)
    }
}

fn price_per_m(per_token: &str) -> Option<f64> {
    let v: f64 = per_token.parse().ok()?;
    // 0 (or negative) means free — treat as 0.0 so it passes a price filter.
    Some(v.max(0.0) * 1_000_000.0)
}

/// Fetch the full OpenRouter model catalog (blocking).
pub fn fetch_models() -> Result<Vec<Model>> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .build()?;
    let resp: ModelsResponse = client
        .get(MODELS_URL)
        .send()
        .context("OpenRouter models request failed")?
        .error_for_status()?
        .json()
        .context("parsing OpenRouter models JSON")?;
    Ok(resp.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 60 * 60;
    const NOW: u64 = 1_000_000;

    fn entry(discount: Option<f64>, fetched_at: Option<u64>) -> BestPrice {
        BestPrice {
            input: Some(1.0),
            output: Some(2.0),
            discount,
            provider: Some("p".into()),
            fetched_at,
        }
    }

    #[test]
    fn plain_entry_lives_an_hour() {
        let e = entry(None, Some(NOW - 59 * 60));
        assert!(entry_is_fresh(&e, 0, NOW));
        let e = entry(None, Some(NOW - HOUR));
        assert!(!entry_is_fresh(&e, 0, NOW));
    }

    #[test]
    fn discounted_entry_expires_in_fifteen_minutes() {
        // Same age, opposite verdicts: the discount is what shortens the life.
        let age = Some(NOW - 20 * 60);
        assert!(entry_is_fresh(&entry(None, age), 0, NOW));
        assert!(!entry_is_fresh(&entry(Some(0.35), age), 0, NOW));
    }

    #[test]
    fn zero_discount_is_not_a_discount() {
        // The API writes 0.0 rather than omitting the field; that must not
        // cut the entry's TTL to 15 minutes.
        let e = entry(Some(0.0), Some(NOW - 20 * 60));
        assert!(entry_is_fresh(&e, 0, NOW));
    }

    #[test]
    fn unstamped_entry_falls_back_to_file_timestamp() {
        // Written before per-entry stamps existed: judged by the file's own.
        let e = entry(None, None);
        assert!(entry_is_fresh(&e, NOW - 10 * 60, NOW));
        assert!(!entry_is_fresh(&e, NOW - 2 * HOUR, NOW));
    }

    #[test]
    fn future_timestamp_does_not_expire_early() {
        // Clock skew must not underflow into "infinitely stale".
        let e = entry(Some(0.5), Some(NOW + HOUR));
        assert!(entry_is_fresh(&e, 0, NOW));
    }

    fn model(out: &[&str]) -> Model {
        Model {
            id: "test/model".to_string(),
            name: "Test Model".to_string(),
            canonical_slug: None,
            description: None,
            context_length: None,
            architecture: Some(Architecture {
                output_modalities: out.iter().map(|s| s.to_string()).collect(),
                ..Architecture::default()
            }),
            pricing: Pricing::default(),
        }
    }

    /// A coding model outputs text only — every other output modality
    /// marks a non-coding model.
    #[test]
    fn non_text_output_is_non_coding() {
        let cases: &[&[&str]] = &[
            &["image"],
            &["image", "text"],
            &["video"],
            &["text", "audio"],
            &["speech"],
            &["transcription"],
            &["embeddings"],
            &["rerank"],
        ];
        for out in cases {
            assert!(model(*out).non_coding(), "out={:?} must be non-coding", out);
        }
    }

    /// Text-only output is coding, including with image input.
    #[test]
    fn text_output_is_coding() {
        assert!(!model(&["text"]).non_coding(), "text output is coding");
        let mut m = model(&["text"]);
        m.architecture.as_mut().unwrap().input_modalities =
            vec!["text".to_string(), "image".to_string()];
        assert!(
            !m.non_coding(),
            "image input + text output is coding"
        );
    }

    /// Absent on older captures — never an error, just None.
    #[test]
    fn absent_architecture_is_coding() {
        let mut m = model(&["text"]);
        m.architecture = None;
        assert!(!m.non_coding(), "absent architecture must be coding");
    }
}
