//! Usage mix — the token-share weights behind the `Mix` price column.
//!
//! A workload's effective price per million tokens is neither the input nor
//! the output price alone: a coding agent that prompt-caches heavily pays
//! for mostly *cached-input* tokens. [`UsageMix`] carries the share of each
//! token kind and blends the matching per-million prices into one number.
//! The active mix is persisted (`--mix` writes it to `mix.json`), so later
//! runs reuse it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Percentage shares of a token budget. They need not sum to 100 — they are
/// normalized at use. `cache` is cached-input tokens (cache reads).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UsageMix {
    pub input: f64,
    pub output: f64,
    pub cache: f64,
}

impl Default for UsageMix {
    /// 3:1 input:output — the chat-ish blend the Model value heat has always
    /// used. No cache share until the user sets one.
    fn default() -> Self {
        UsageMix {
            input: 75.0,
            output: 25.0,
            cache: 0.0,
        }
    }
}

impl UsageMix {
    /// Parse a `--mix` spec in either form:
    /// - named, any subset: `input=8.1,output=0.4,cache=91.5`
    /// - positional `input,output,cache`: `8.1,0.4,91.5`
    ///
    /// Values are percentages; omitted components are 0. The total must be
    /// positive and every value finite and non-negative.
    pub fn parse(spec: &str) -> Result<UsageMix> {
        let mut m = UsageMix {
            input: 0.0,
            output: 0.0,
            cache: 0.0,
        };
        if spec.contains('=') {
            for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
                let (key, val) = part
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("mix part {part:?} is not key=value"))?;
                let val: f64 = val
                    .trim()
                    .parse()
                    .with_context(|| format!("mix value {val:?} is not a number"))?;
                match key.trim().to_ascii_lowercase().as_str() {
                    "input" | "in" => m.input = val,
                    "output" | "out" => m.output = val,
                    "cache" => m.cache = val,
                    other => bail!("unknown mix component {other:?} (use input, output, cache)"),
                }
            }
        } else {
            let vals: Vec<f64> = spec
                .split(',')
                .map(|s| s.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .context("mix values are not numbers (use input,output,cache)")?;
            if vals.len() > 3 {
                bail!("too many mix values (use input,output,cache)");
            }
            for (i, v) in vals.iter().enumerate() {
                match i {
                    0 => m.input = *v,
                    1 => m.output = *v,
                    _ => m.cache = *v,
                }
            }
        }
        if !m.valid() {
            bail!("mix values must be non-negative and sum to more than 0");
        }
        Ok(m)
    }

    /// A usable mix: every share finite and non-negative, some share > 0.
    fn valid(&self) -> bool {
        [self.input, self.output, self.cache]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
            && self.input + self.output + self.cache > 0.0
    }

    /// Weights normalized to sum to 1: `(input, output, cache)`.
    fn shares(&self) -> (f64, f64, f64) {
        let total = self.input + self.output + self.cache;
        (self.input / total, self.output / total, self.cache / total)
    }

    /// Blended USD per million tokens at this mix. `input` is the anchor
    /// price and required; a missing `output` counts as 0. `cache_read`
    /// falls back to `input` — a model without cache pricing charges full
    /// input for those tokens.
    pub fn blended(
        &self,
        input: Option<f64>,
        output: Option<f64>,
        cache_read: Option<f64>,
    ) -> Option<f64> {
        let input = input?;
        let (wi, wo, wc) = self.shares();
        Some(wi * input + wo * output.unwrap_or(0.0) + wc * cache_read.unwrap_or(input))
    }

    /// Load the persisted mix, if any. A missing or invalid file is a miss.
    pub fn load() -> Option<UsageMix> {
        let path = cache_path().ok()?;
        let content = std::fs::read_to_string(path).ok()?;
        let m = serde_json::from_str::<UsageMix>(&content).ok()?;
        m.valid().then_some(m)
    }

    /// Persist the mix for later runs. Failure is the caller's to warn
    /// about — a missing save must not break the run.
    pub fn save(&self) -> Result<()> {
        let path = cache_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

fn cache_path() -> Result<std::path::PathBuf> {
    let base = dirs::config_dir().context("no config dir on this platform")?;
    Ok(base.join("llm-leaders").join("mix.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec the user actually types: an agent profile with a heavy
    /// cache share.
    #[test]
    fn parses_named_spec() {
        let m = UsageMix::parse("input=8.1,output=0.4,cache=91.5").unwrap();
        assert_eq!(
            m,
            UsageMix {
                input: 8.1,
                output: 0.4,
                cache: 91.5
            }
        );
    }

    /// Positional shorthand in input,output,cache order; a partial spec
    /// leaves the rest at 0.
    #[test]
    fn parses_positional_spec() {
        let m = UsageMix::parse("8.1,0.4,91.5").unwrap();
        assert_eq!(
            m,
            UsageMix {
                input: 8.1,
                output: 0.4,
                cache: 91.5
            }
        );
        let m = UsageMix::parse("3,1").unwrap();
        assert_eq!(
            m,
            UsageMix {
                input: 3.0,
                output: 1.0,
                cache: 0.0
            }
        );
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(UsageMix::parse("speed=5").is_err(), "unknown component");
        assert!(UsageMix::parse("input=-1").is_err(), "negative share");
        assert!(
            UsageMix::parse("input=0,output=0,cache=0").is_err(),
            "all zero"
        );
        assert!(UsageMix::parse("1,2,3,4").is_err(), "too many values");
        assert!(UsageMix::parse("fast").is_err(), "not numbers");
    }

    /// The blend weights by normalized shares: 3:1 input:output is the
    /// documented default, and the cache share reads cache pricing.
    #[test]
    fn blends_by_normalized_shares() {
        let m = UsageMix::parse("8.1,0.4,91.5").unwrap();
        // 8.1/100 * 5 + 0.4/100 * 25 + 91.5/100 * 0.5
        let want = 0.081 * 5.0 + 0.004 * 25.0 + 0.915 * 0.5;
        let got = m.blended(Some(5.0), Some(25.0), Some(0.5)).unwrap();
        assert!((got - want).abs() < 1e-9, "{got} != {want}");

        let d = UsageMix::default();
        assert!((d.blended(Some(3.0), Some(1.0), None).unwrap() - 2.5).abs() < 1e-9);
    }

    /// No cache pricing → cached tokens are charged as full input. No input
    /// price at all → the blend is unknown.
    #[test]
    fn cache_falls_back_to_input() {
        let m = UsageMix::parse("0,0,100").unwrap();
        assert_eq!(m.blended(Some(2.0), Some(9.0), None), Some(2.0));
        assert_eq!(m.blended(None, Some(9.0), Some(2.0)), None);
    }
}
