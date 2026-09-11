use anyhow::{Context, Result};
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::{Deserialize as DeriveDeserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::or_bench::Standing;

const CAPABILITY_URL: &str = "https://benchmarklist.com/directory-data/models.json";
const TTL: Duration = Duration::from_secs(5 * 60 * 60);
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, DeriveDeserialize)]
pub struct BenchmarkList {
    by_id: HashMap<String, CapabilityStanding>,
    by_tail_id: HashMap<String, Vec<CapabilityStanding>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, DeriveDeserialize)]
pub struct CapabilityStanding {
    pub standing: Standing,
    pub coverage: Option<u64>,
    api_id: String,
    name: String,
}

#[derive(Debug, DeriveDeserialize)]
struct BenchmarkResponse {
    #[serde(default, deserialize_with = "deserialize_scored_rows")]
    rows: Vec<RawRow>,
}

#[derive(Debug, DeriveDeserialize)]
struct RawRow {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "capabilityScore", default)]
    capability_score: Option<f64>,
    #[serde(default)]
    count: Option<u64>,
}

#[derive(Debug, Serialize, DeriveDeserialize)]
struct Cache {
    schema_version: u32,
    fetched_at: u64,
    data: BenchmarkList,
}

impl BenchmarkList {
    pub fn standing(&self, openrouter_id: &str, model_name: &str) -> Option<CapabilityStanding> {
        let exact = normalize_id(openrouter_id);
        if let Some(standing) = self.by_id.get(&exact) {
            return Some(standing.clone());
        }

        let tail = openrouter_id
            .split(':')
            .next()
            .unwrap_or(openrouter_id)
            .rsplit('/')
            .next()
            .unwrap_or(openrouter_id);
        let candidates = self.by_tail_id.get(&normalize_id(tail))?;
        let compatible: Vec<&CapabilityStanding> = candidates
            .iter()
            .filter(|candidate| names_compatible(model_name, &candidate.name))
            .collect();
        if compatible.len() == 1 {
            return Some(compatible[0].clone());
        }
        None
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

impl BenchmarkResponse {
    fn into_list(self) -> BenchmarkList {
        build(self.rows)
    }
}

fn deserialize_scored_rows<'de, D>(deserializer: D) -> Result<Vec<RawRow>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ScoredRowsVisitor;

    impl<'de> Visitor<'de> for ScoredRowsVisitor {
        type Value = Vec<RawRow>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a sequence of model rows")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut rows = Vec::new();
            while let Some(row) = seq.next_element::<RawRow>()? {
                if row.id.is_empty() {
                    continue;
                }
                if let Some(score) = row.capability_score.filter(|score| score.is_finite()) {
                    rows.push(RawRow {
                        id: row.id,
                        name: row.name,
                        capability_score: Some(score),
                        count: row.count,
                    });
                }
            }
            Ok(rows)
        }
    }

    deserializer.deserialize_seq(ScoredRowsVisitor)
}

fn build(rows: Vec<RawRow>) -> BenchmarkList {
    let mut rows = rows;
    rows.sort_by(|a, b| {
        b.capability_score
            .unwrap_or(f64::MIN)
            .total_cmp(&a.capability_score.unwrap_or(f64::MIN))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut by_id = HashMap::new();
    let mut by_tail_id: HashMap<String, Vec<CapabilityStanding>> = HashMap::new();
    for (index, row) in rows.into_iter().enumerate() {
        let api_id = normalize_id(&row.id);
        let tail_id = normalize_id(row.id.rsplit('/').next().unwrap_or(&row.id));
        let standing = CapabilityStanding {
            standing: Standing {
                score: row.capability_score.unwrap_or(f64::MIN),
                rank: (index + 1) as u64,
            },
            coverage: row.count,
            api_id: api_id.clone(),
            name: row.name,
        };

        by_id.entry(api_id.clone()).or_insert(standing.clone());
        let candidates = by_tail_id.entry(tail_id).or_default();
        if !candidates
            .iter()
            .any(|candidate| candidate.api_id == standing.api_id)
        {
            candidates.push(standing);
        }
    }

    BenchmarkList { by_id, by_tail_id }
}

fn normalize_id(id: &str) -> String {
    id.split(':')
        .next()
        .unwrap_or(id)
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn name_tokens(name: &str) -> Vec<String> {
    let display = name
        .split_once(':')
        .map(|(_, model)| model)
        .unwrap_or(name)
        .to_ascii_lowercase();
    let mut tokens: Vec<String> = display
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter(|token| {
            !matches!(
                *token,
                "ai" | "model" | "latest" | "extended" | "preview" | "free" | "batch"
            )
        })
        .map(String::from)
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

fn names_compatible(openrouter_name: &str, benchmark_name: &str) -> bool {
    let left = name_tokens(openrouter_name);
    let right = name_tokens(benchmark_name);
    if left.is_empty() || right.is_empty() {
        return false;
    }

    let left_numeric: Vec<&String> = left
        .iter()
        .filter(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .collect();
    let right_numeric: Vec<&String> = right
        .iter()
        .filter(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .collect();
    if left_numeric != right_numeric {
        return false;
    }

    let (short, long) = if left.len() <= right.len() {
        (&left, &right)
    } else {
        (&right, &left)
    };
    short.iter().all(|token| long.contains(token))
}

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("capability.json"))
}

fn load_fresh() -> Result<Option<BenchmarkList>> {
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

fn save(data: &BenchmarkList) -> Result<()> {
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

pub fn parse_json(raw: &str) -> Result<BenchmarkList> {
    let response: BenchmarkResponse =
        serde_json::from_str(raw).context("parsing BenchmarkList JSON")?;
    Ok(response.into_list())
}

fn fetch() -> Result<BenchmarkList> {
    let client = reqwest::blocking::Client::builder()
        .gzip(true)
        .user_agent("llm-leaders")
        .timeout(Duration::from_secs(60))
        .build()?;
    let response = client
        .get(CAPABILITY_URL)
        .send()
        .context("BenchmarkList request failed")?
        .error_for_status()?;
    let response: BenchmarkResponse =
        serde_json::from_reader(response).context("parsing BenchmarkList JSON")?;
    Ok(response.into_list())
}

pub fn get(refresh: bool) -> Option<BenchmarkList> {
    if !refresh {
        match load_fresh() {
            Ok(Some(data)) => return Some(data),
            Ok(None) => {}
            Err(e) => eprintln!("warning: reading BenchmarkList cache failed: {e}"),
        }
    }
    match fetch() {
        Ok(data) => {
            if let Err(e) = save(&data) {
                eprintln!("warning: writing BenchmarkList cache failed: {e}");
            }
            Some(data)
        }
        Err(e) => {
            eprintln!("warning: BenchmarkList capability data unavailable ({e})");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scores_and_computes_ranks() {
        let list = parse_json(
            r#"{"rows":[
                {"id":"openai-gpt-5","name":"GPT-5","capabilityScore":150,"count":10},
                {"id":"anthropic-claude-5","name":"Claude 5","capabilityScore":160,"count":20},
                {"id":"unscored","name":"Unscored","count":3}
            ]}"#,
        )
        .expect("fixture parses");

        let gpt = list
            .standing("openai/gpt-5", "OpenAI: GPT-5")
            .expect("gpt standing");
        let claude = list
            .standing("anthropic/claude-5", "Anthropic: Claude 5")
            .expect("claude standing");
        assert_eq!(gpt.standing.score, 150.0);
        assert_eq!(gpt.standing.rank, 2);
        assert_eq!(gpt.coverage, Some(10));
        assert_eq!(claude.standing.score, 160.0);
        assert_eq!(claude.standing.rank, 1);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn matches_provider_qualified_ids_and_tier_variants() {
        let list = parse_json(
            r#"{"rows":[
                {"id":"openai-gpt-5","name":"GPT-5","capabilityScore":150,"count":10}
            ]}"#,
        )
        .expect("fixture parses");

        assert!(list.standing("openai/gpt-5", "OpenAI: GPT-5").is_some());
        assert!(list
            .standing("openai/gpt-5:free", "OpenAI: GPT-5 (free)")
            .is_some());
        assert!(list.standing("other/gpt-5", "Other: GPT-5").is_none());
    }

    #[test]
    fn allows_tail_id_only_when_names_agree() {
        let list = parse_json(
            r#"{"rows":[
                {"id":"granite-4.2-8b","name":"Granite 4.2 8B","capabilityScore":110,"count":8},
                {"id":"glm-4","name":"GLM-4","capabilityScore":127,"count":4}
            ]}"#,
        )
        .expect("fixture parses");

        assert!(list
            .standing("ibm-granite/granite-4.2-8b", "IBM: Granite 4.2 8B")
            .is_some());
        assert!(list.standing("z-ai/glm-4.7", "Z.ai: GLM 4.7").is_none());
    }

    #[test]
    fn keeps_best_record_when_ids_repeat() {
        let list = parse_json(
            r#"{"rows":[
                {"id":"openai-gpt-5","name":"GPT-5","capabilityScore":140,"count":4},
                {"id":"openai-gpt-5","name":"GPT-5","capabilityScore":150,"count":10}
            ]}"#,
        )
        .expect("fixture parses");

        let standing = list
            .standing("openai/gpt-5", "OpenAI: GPT-5")
            .expect("standing");
        assert_eq!(standing.standing.score, 150.0);
        assert_eq!(standing.coverage, Some(10));
    }
}
