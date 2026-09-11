use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MODELS_PAGE_URL: &str = "https://benchlm.ai/models";
const FALLBACK_BUILD_ID: &str = "gwr6IxmBXSB7ktg3I6Jip";
const TTL: Duration = Duration::from_secs(5 * 60 * 60);
const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchLm {
    by_slug: HashMap<String, BenchLmStanding>,
    by_name: HashMap<String, BenchLmStanding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BenchLmStanding {
    pub score: f64,
    pub rank: u64,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(rename = "pageProps")]
    page_props: PageProps,
}

#[derive(Debug, Deserialize)]
struct PageProps {
    #[serde(default)]
    models: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    model: String,
    #[serde(rename = "overallScore", default)]
    overall_score: Option<f64>,
    #[serde(rename = "overallRank", default)]
    overall_rank: Option<u64>,
    #[serde(default)]
    siblings: Vec<RawSibling>,
}

#[derive(Debug, Deserialize)]
struct RawSibling {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    model: String,
    #[serde(rename = "overallScore", default)]
    overall_score: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    schema_version: u32,
    fetched_at: u64,
    data: BenchLm,
}

impl BenchLm {
    pub fn standing(&self, openrouter_id: &str, model_name: &str) -> Option<BenchLmStanding> {
        let base = openrouter_id.split(':').next().unwrap_or(openrouter_id);
        let tail = base.rsplit('/').next().unwrap_or(base);
        if let Some(standing) = self.by_slug.get(&normalize_id(tail)) {
            return Some(*standing);
        }

        let names = [
            model_name,
            model_name
                .split_once(':')
                .map(|(_, model)| model)
                .unwrap_or(model_name),
        ];
        names
            .iter()
            .find_map(|name| self.by_name.get(&normalize_name(name)))
            .copied()
    }

    pub fn len(&self) -> usize {
        self.by_slug.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_slug.is_empty()
    }
}

fn normalize_id(id: &str) -> String {
    id.to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn normalize_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn build(models: Vec<RawModel>) -> BenchLm {
    let mut raw: Vec<(String, String, f64, Option<u64>)> = Vec::new();
    for model in models {
        if let Some(score) = valid_score(model.overall_score) {
            raw.push((model.slug, model.model, score, model.overall_rank));
        }
        for sibling in model.siblings {
            if let Some(score) = valid_score(sibling.overall_score) {
                raw.push((sibling.slug, sibling.model, score, None));
            }
        }
    }

    let ranked_scores: Vec<f64> = raw
        .iter()
        .filter_map(|(_, _, score, rank)| rank.map(|_| *score))
        .collect();
    for entry in &mut raw {
        if entry.3.is_none() {
            entry.3 = Some(derived_rank(entry.2, &ranked_scores));
        }
    }

    raw.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut by_slug: HashMap<String, BenchLmStanding> = HashMap::new();
    let mut by_name: HashMap<String, BenchLmStanding> = HashMap::new();
    for (slug, name, score, rank) in raw {
        let standing = BenchLmStanding {
            score,
            rank: rank.unwrap_or_default(),
        };
        if !slug.is_empty() {
            by_slug.entry(normalize_id(&slug)).or_insert(standing);
        }
        if !name.is_empty() {
            by_name.entry(normalize_name(&name)).or_insert(standing);
        }
    }

    BenchLm { by_slug, by_name }
}

fn derived_rank(score: f64, ranked_scores: &[f64]) -> u64 {
    (ranked_scores
        .iter()
        .filter(|ranked| **ranked > score)
        .count()
        + 1) as u64
}

fn valid_score(score: Option<f64>) -> Option<f64> {
    score.filter(|score| score.is_finite())
}

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("benchlm.json"))
}

fn load_fresh() -> Result<Option<BenchLm>> {
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

fn save(data: &BenchLm) -> Result<()> {
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

pub fn parse_json(raw: &str) -> Result<BenchLm> {
    let response: ModelsResponse = serde_json::from_str(raw).context("parsing BenchLM JSON")?;
    Ok(build(response.page_props.models))
}

fn data_url(client: &reqwest::blocking::Client) -> Result<String> {
    let html = client
        .get(MODELS_PAGE_URL)
        .send()
        .context("BenchLM models page request failed")?
        .error_for_status()?
        .text()?;
    let build_id = html
        .split(r#""buildId":"#)
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .filter(|build_id| !build_id.is_empty())
        .unwrap_or(FALLBACK_BUILD_ID);
    Ok(format!(
        "https://benchlm.ai/_next/data/{build_id}/models.json"
    ))
}

fn fetch() -> Result<BenchLm> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client
        .get(data_url(&client)?)
        .send()
        .context("BenchLM request failed")?
        .error_for_status()?
        .text()
        .context("reading BenchLM response")?;
    parse_json(&response)
}

pub fn get(refresh: bool) -> Option<BenchLm> {
    if !refresh {
        match load_fresh() {
            Ok(Some(data)) => return Some(data),
            Ok(None) => {}
            Err(e) => eprintln!("warning: reading BenchLM cache failed: {e}"),
        }
    }
    match fetch() {
        Ok(data) => {
            if let Err(e) = save(&data) {
                eprintln!("warning: writing BenchLM cache failed: {e}");
            }
            Some(data)
        }
        Err(e) => {
            eprintln!("warning: BenchLM rank data unavailable ({e})");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_primary_and_sibling_scores() {
        let list = parse_json(
            r#"{"pageProps":{"models":[
                {
                    "slug":"claude-5","model":"Claude 5","creator":"Anthropic",
                    "overallScore":84.5,"overallRank":1,
                    "siblings":[{"slug":"claude-4","model":"Claude 4","overallScore":70}]
                }
            ]}}"#,
        )
        .expect("fixture parses");

        let primary = list
            .standing("anthropic/claude-5", "Anthropic: Claude 5")
            .expect("primary standing");
        let sibling = list
            .standing("anthropic/claude-4", "Anthropic: Claude 4")
            .expect("sibling standing");
        assert_eq!(primary.score, 84.5);
        assert_eq!(primary.rank, 1);
        assert_eq!(sibling.score, 70.0);
        assert_eq!(sibling.rank, 2);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn derives_sibling_ranks_from_ranked_primary_scores() {
        let list = parse_json(
            r#"{"pageProps":{"models":[
                {
                    "slug":"first","model":"First","overallScore":90,"overallRank":1,
                    "siblings":[
                        {"slug":"between","model":"Between","overallScore":85},
                        {"slug":"tied","model":"Tied","overallScore":80}
                    ]
                },
                {"slug":"second","model":"Second","overallScore":80,"overallRank":2},
                {"slug":"below","model":"Below","overallScore":70,"overallRank":3},
                {"slug":"unranked-low","model":"Unranked Low","overallScore":60}
            ]}}"#,
        )
        .expect("fixture parses");

        let between = list
            .standing("acme/between", "Acme: Between")
            .expect("between standing");
        let tied = list
            .standing("acme/tied", "Acme: Tied")
            .expect("tied standing");
        let low = list
            .standing("acme/unranked-low", "Acme: Unranked Low")
            .expect("low standing");
        assert_eq!(between.rank, 2);
        assert_eq!(tied.rank, 2);
        assert_eq!(low.rank, 4);
    }

    #[test]
    fn matches_slugs_names_and_tier_variants() {
        let list = parse_json(
            r#"{"pageProps":{"models":[
                {"slug":"gemini-3-8-flash","model":"Gemini 3.8 Flash","overallScore":75.6,"overallRank":8}
            ]}}"#,
        )
        .expect("fixture parses");

        assert!(list
            .standing("google/gemini-3.8-flash", "Google: Gemini 3.8 Flash")
            .is_some());
        assert!(list
            .standing(
                "google/gemini-3.8-flash:batch",
                "Google: Gemini 3.8 Flash (batch)"
            )
            .is_some());
        assert!(list.standing("other/unknown", "Other: Unknown").is_none());
    }

    #[test]
    fn keeps_best_record_for_duplicate_slugs() {
        let list = parse_json(
            r#"{"pageProps":{"models":[
                {"slug":"claude-5","model":"Claude 5","overallScore":80,"overallRank":2},
                {"slug":"claude-5","model":"Claude 5","overallScore":84,"overallRank":1}
            ]}}"#,
        )
        .expect("fixture parses");

        let standing = list
            .standing("anthropic/claude-5", "Anthropic: Claude 5")
            .expect("standing");
        assert_eq!(standing.score, 84.0);
        assert_eq!(standing.rank, 1);
        assert_eq!(list.len(), 1);
    }
}
