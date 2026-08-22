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
    pub fetched_at: u64, // unix seconds
    pub prices: std::collections::HashMap<String, BestPrice>,
}

impl BestPrice {
    fn cache_path() -> anyhow::Result<std::path::PathBuf> {
        let base = dirs::config_dir().context("no config dir on this platform")?;
        Ok(base.join("llm-leaders").join("best_prices.json"))
    }

    fn load() -> anyhow::Result<Option<std::collections::HashMap<String, BestPrice>>> {
        let Ok(content) = std::fs::read_to_string(Self::cache_path()?) else {
            return Ok(None);
        };
        let cache: BestPriceCache =
            serde_json::from_str(&content).context("parsing best-price cache")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        const TTL: u64 = 24 * 60 * 60;
        if now.saturating_sub(cache.fetched_at) < TTL {
            Ok(Some(cache.prices))
        } else {
            Ok(None)
        }
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
            fetched_at: now,
            prices: prices.clone(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&cache)?)?;
        Ok(())
    }
}

/// For each given model id, fetch its provider endpoints and pick the
/// cheapest (by input price — the OpenRouter website convention). Serves
/// from a 24h on-disk cache first; missing ids are fetched in parallel and
/// merged back into the cache. `refresh` bypasses the cache entirely.
pub fn fetch_best_prices(
    ids: &[String],
    refresh: bool,
) -> Result<std::collections::HashMap<String, BestPrice>> {
    use std::collections::HashMap;

    let mut cached: HashMap<String, BestPrice> = if refresh {
        HashMap::new()
    } else {
        BestPrice::load()?.unwrap_or_default()
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
        let fetched = fetch_best_prices_uncached(&missing)?;
        for (id, bp) in &fetched {
            cached.insert(id.clone(), bp.clone());
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
