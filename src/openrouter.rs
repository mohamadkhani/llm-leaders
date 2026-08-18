use anyhow::{Context, Result};
use serde::Deserialize;

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
    #[allow(dead_code)]
    pub context_length: Option<u64>,
    pub pricing: Pricing,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pricing {
    /// USD per token (prompt). Multiply by 1_000_000 for $/M.
    #[serde(default)]
    pub prompt: String,
    /// USD per token (completion).
    #[serde(default)]
    pub completion: String,
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
