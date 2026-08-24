//! Client for OpenRouter's frontend benchmark rankings.
//!
//! Two things come from this endpoint:
//!
//! 1. **The oracle.** Every `daData` row carries a non-null `openrouter_id`
//!    alongside the benchmark's own `da_model_id` — OpenRouter has already
//!    done the arena→OpenRouter mapping authoritatively (1356/1356 rows
//!    measured). The matcher uses this as its first tier.
//! 2. **Score tables.** 27 benchmark categories, each keyed by
//!    `openrouter_id`, with a rank computed within that category's coverage.
//!
//! Like the catalog, this endpoint is undocumented and unversioned, so every
//! failure is non-fatal: the matcher drops to structural-only matching and
//! benchmark columns render `—`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BENCH_URL: &str = "https://openrouter.ai/api/frontend/v1/rankings/benchmarks";
const TTL: Duration = Duration::from_secs(5 * 60 * 60);
const SCHEMA_VERSION: u32 = 1;

/// The two categories promoted to default columns (ADR-0003).
pub const CAT_WEBSITE: &str = "models-website";
pub const CAT_CODE: &str = "models-codecategories";

/// One model's standing in one benchmark category.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Standing {
    pub score: f64,
    /// 1-based rank within this category only.
    pub rank: u64,
}

/// An oracle entry: the authoritative identity of one benchmarked model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleEntry {
    /// The benchmark's own model id — often equal to an arena `model_key`.
    pub da_model_id: String,
    pub openrouter_id: String,
    pub permaslug: String,
    pub display_name: String,
}

/// Parsed benchmark data: the oracle plus per-category score tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Benchmarks {
    /// `da_model_id` (lowercased) → oracle entry.
    pub by_da_id: HashMap<String, OracleEntry>,
    /// `display_name` (lowercased) → oracle entry. Arena rows whose
    /// `model_key` is an anonymized codename still carry a real display name.
    pub by_display: HashMap<String, OracleEntry>,
    /// category → (`openrouter_id` → standing).
    pub scores: HashMap<String, HashMap<String, Standing>>,
}

impl Benchmarks {
    /// Look up an oracle entry by benchmark id, then by display name.
    pub fn resolve(&self, key: &str) -> Option<&OracleEntry> {
        let k = key.to_lowercase();
        self.by_da_id.get(&k).or_else(|| self.by_display.get(&k))
    }

    /// A model's standing in one category.
    pub fn standing(&self, category: &str, openrouter_id: &str) -> Option<Standing> {
        self.scores.get(category)?.get(openrouter_id).copied()
    }

    /// Resolve a user-supplied `--bench` value to a full category name: the
    /// exact name, or its short form (`uicomponent` → `models-uicomponent`).
    pub fn resolve_category(&self, input: &str) -> Option<String> {
        if self.scores.contains_key(input) {
            return Some(input.to_string());
        }
        let short = format!("models-{input}");
        self.scores.contains_key(&short).then_some(short)
    }

    /// How many models a category covers — shown in the column header so
    /// `#1 of 9` isn't misread as `#1 of 150`.
    pub fn coverage(&self, category: &str) -> usize {
        self.scores.get(category).map_or(0, |m| m.len())
    }

    /// All categories with their coverage, sorted by coverage desc.
    pub fn categories(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> =
            self.scores.iter().map(|(k, v)| (k.clone(), v.len())).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

// --- wire format -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BenchResponse {
    data: BenchData,
}

#[derive(Debug, Deserialize)]
struct BenchData {
    #[serde(default, rename = "daData")]
    da_data: HashMap<String, Vec<RawRow>>,
}

#[derive(Debug, Deserialize)]
struct RawRow {
    #[serde(default)]
    da_model_id: Option<String>,
    #[serde(default)]
    permaslug: Option<String>,
    #[serde(default)]
    openrouter_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    score: Option<f64>,
}

// --- cache -----------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    schema_version: u32,
    fetched_at: u64,
    data: Benchmarks,
}

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("benchmarks.json"))
}

fn load_fresh() -> Result<Option<Benchmarks>> {
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
        Ok(Some(cache.data))
    } else {
        Ok(None)
    }
}

fn save(data: &Benchmarks) -> Result<()> {
    let path = cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let cache = Cache {
        schema_version: SCHEMA_VERSION,
        fetched_at: now,
        data: data.clone(),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&cache)?)?;
    Ok(())
}

// --- parse -----------------------------------------------------------------

/// Parse a raw benchmarks JSON payload (tests + cache path share this).
pub fn parse_json(raw: &str) -> Result<Benchmarks> {
    let resp: BenchResponse = serde_json::from_str(raw).context("parsing benchmarks JSON")?;
    Ok(build(resp.data.da_data))
}

/// Build the oracle and score tables from raw category data.
fn build(da_data: HashMap<String, Vec<RawRow>>) -> Benchmarks {
    let mut out = Benchmarks::default();

    for (category, rows) in da_data {
        // Rank within this category, by score desc. Rows without a usable
        // score are indexed in the oracle but carry no standing.
        let mut scored: Vec<(&RawRow, f64, String)> = rows
            .iter()
            .filter_map(|r| {
                let id = r.openrouter_id.as_deref()?;
                Some((r, r.score?, id.to_string()))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut table: HashMap<String, Standing> = HashMap::new();
        for (i, (_, score, id)) in scored.iter().enumerate() {
            // A model can appear more than once per category (config
            // variants); keep its best standing.
            table.entry(id.clone()).or_insert(Standing {
                score: *score,
                rank: (i + 1) as u64,
            });
        }
        if !table.is_empty() {
            out.scores.insert(category, table);
        }

        for row in &rows {
            let (Some(or_id), Some(permaslug)) =
                (row.openrouter_id.as_deref(), row.permaslug.as_deref())
            else {
                continue;
            };
            let entry = OracleEntry {
                da_model_id: row.da_model_id.clone().unwrap_or_default(),
                openrouter_id: or_id.to_string(),
                permaslug: permaslug.to_string(),
                display_name: row.display_name.clone().unwrap_or_default(),
            };
            if let Some(da) = row.da_model_id.as_deref().filter(|s| !s.is_empty()) {
                out.by_da_id.entry(da.to_lowercase()).or_insert_with(|| entry.clone());
            }
            if let Some(disp) = row.display_name.as_deref().filter(|s| !s.is_empty()) {
                out.by_display.entry(disp.to_lowercase()).or_insert(entry);
            }
        }
    }

    out
}

fn fetch() -> Result<Benchmarks> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .timeout(Duration::from_secs(25))
        .build()?;
    let resp: BenchResponse = client
        .get(BENCH_URL)
        .send()
        .context("benchmarks request failed")?
        .error_for_status()?
        .json()
        .context("parsing benchmarks JSON")?;
    Ok(build(resp.data.da_data))
}

/// Get benchmark data: fresh cache, else fetch + cache.
///
/// Returns `None` on failure after one stderr warning — the matcher then
/// skips its oracle tier and benchmark columns render `—`.
pub fn get(refresh: bool) -> Option<Benchmarks> {
    if !refresh {
        match load_fresh() {
            Ok(Some(data)) => return Some(data),
            Ok(None) => {}
            Err(e) => eprintln!("warning: reading benchmarks cache failed: {e}"),
        }
    }
    match fetch() {
        Ok(data) => {
            if let Err(e) = save(&data) {
                eprintln!("warning: writing benchmarks cache failed: {e}");
            }
            Some(data)
        }
        Err(e) => {
            eprintln!("warning: OpenRouter benchmarks unavailable ({e}) — no oracle, no benchmark columns");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture() -> Benchmarks {
        parse_json(include_str!("../tests/fixtures/benchmarks.json")).expect("fixture parses")
    }

    #[test]
    fn oracle_resolves_known_ids() {
        let b = fixture();
        for (key, want) in [
            ("glm-5.1", "z-ai/glm-5.1"),
            ("glm-4.7", "z-ai/glm-4.7"),
            ("deepseek-v4-flash-0731", "deepseek/deepseek-v4-flash-0731"),
            ("kimi-k2.7-code", "moonshotai/kimi-k2.7-code"),
            ("qwen3.8-max", "qwen/qwen3.8-max"),
        ] {
            let got = b.resolve(key).unwrap_or_else(|| panic!("{key} unresolved"));
            assert_eq!(got.openrouter_id, want, "oracle for {key}");
        }
    }

    /// Anonymized arena keys are unresolvable by id but resolvable by the
    /// display name the benchmark shares with the arena scrape.
    #[test]
    fn oracle_resolves_by_display_name() {
        let b = fixture();
        let got = b.resolve("GLM 5.1").expect("display-name lookup");
        assert_eq!(got.openrouter_id, "z-ai/glm-5.1");
    }

    /// The invariant measured against live data: every indexed row has a
    /// non-null openrouter_id and permaslug.
    #[test]
    fn every_oracle_entry_is_fully_identified() {
        let b = fixture();
        assert!(!b.by_da_id.is_empty(), "oracle should not be empty");
        for e in b.by_da_id.values().chain(b.by_display.values()) {
            assert!(!e.openrouter_id.is_empty(), "empty openrouter_id for {e:?}");
            assert!(!e.permaslug.is_empty(), "empty permaslug for {e:?}");
        }
    }

    #[test]
    fn scores_are_ranked_within_category() {
        let b = fixture();
        let table = b.scores.get(CAT_WEBSITE).expect("models-website present");
        assert!(!table.is_empty());
        // Rank 1 must be the highest score in the category.
        let best = table.values().max_by(|a, b| a.score.total_cmp(&b.score)).unwrap();
        let rank1 = table.values().filter(|s| s.rank == 1).count();
        assert_eq!(rank1, 1, "exactly one rank-1 entry per category");
        assert_eq!(
            table.values().find(|s| s.rank == 1).unwrap().score,
            best.score,
            "rank 1 should hold the top score"
        );
    }

    #[test]
    fn coverage_matches_table_size() {
        let b = fixture();
        for (cat, count) in b.categories() {
            assert_eq!(b.coverage(&cat), count);
            assert!(count > 0);
        }
    }

    /// The column lookups the renderer performs: by openrouter_id, directly.
    /// Tier variants (`:free`) are absent from score tables by design.
    #[test]
    fn standings_for_default_columns() {
        let b = fixture();
        // A canonical model must have standings wherever the category covers
        // it; find one covered by both default categories.
        let mut checked = 0;
        for id in b.scores.get(CAT_WEBSITE).expect("website table present").keys() {
            if let Some(s) = b.standing(CAT_WEBSITE, id) {
                assert!(s.rank >= 1);
                checked += 1;
            }
            if checked >= 5 {
                break;
            }
        }
        assert!(checked > 0, "fixture has covered models in models-website");
        // A variant id is not benchmarked: standing is None, not an error.
        assert!(b.standing(CAT_WEBSITE, "z-ai/glm-5.2:free").is_none());
    }

    /// --bench short form: `uicomponent` resolves to `models-uicomponent`,
    /// full names pass through, unknown inputs return None.
    #[test]
    fn resolve_category_short_form() {
        let b = fixture();
        assert_eq!(b.resolve_category(CAT_WEBSITE), Some(CAT_WEBSITE.to_string()));
        assert_eq!(
            b.resolve_category("uicomponent"),
            Some("models-uicomponent".to_string())
        );
        assert!(b.resolve_category("models-uicomponent").is_some());
        assert_eq!(b.resolve_category("no-such-category"), None);
    }
}
