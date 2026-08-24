//! Two-tier arena↔catalog matcher.
//!
//! Replaces `match_score.rs`, whose additive similarity score let an absent
//! component pass for free and matched each catalog model independently. Two
//! consequences, both observed live: `z-ai/glm-4.5v` scored high enough on
//! arena's `glm-5.3` row to take rank #8, and 24 of 40 rows shared a
//! duplicate rank because nothing stopped many models claiming one row.
//!
//! The rebuild fixes both at the root:
//!
//! 1. **Oracle tier** — OpenRouter's own benchmark payload already maps its
//!    model ids to the benchmark/arena ids. When the oracle knows a row, we
//!    use it. No inference, no scoring.
//! 2. **Structural tier** — otherwise, both sides parse to a
//!    [`ModelIdentity`] and must agree component-wise ([`ModelIdentity::matches`]).
//!    A missing component is never a wildcard, so `glm-4.5v` cannot reach
//!    `glm-5.3`.
//!
//! Then a single **global one-to-one assignment** over all candidate pairs:
//! each arena row is claimed by at most one catalog model and vice versa, so
//! duplicate ranks are structurally impossible rather than merely unlikely.
//!
//! See ADR-0001 and ADR-0002.

use std::collections::{HashMap, HashSet};

use crate::arena::Score;
use crate::identity::{token_set, ModelIdentity};
use crate::openrouter::Model;
use crate::or_bench::Benchmarks;

/// How a pairing was established — the audit trail behind every rank shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provenance {
    /// OpenRouter's benchmark payload mapped this arena row to this model id.
    Oracle,
    /// Both names parsed to the same structural identity.
    Structural,
}

/// One resolved arena↔catalog pairing.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub score: &'a Score,
    pub provenance: Provenance,
}

/// A candidate pairing before assignment. `cost` orders the greedy pass:
/// lower is better.
#[derive(Debug)]
struct Candidate {
    model_idx: usize,
    score_idx: usize,
    provenance: Provenance,
    cost: u32,
}

/// Resolve arena rows against catalog models.
///
/// Returns `model.id` → [`Match`]. Every arena row appears at most once
/// across the whole map; a model with no defensible pairing is absent rather
/// than pointed at a plausible-looking row.
///
/// `bench` is optional: without it the oracle tier is skipped and matching
/// falls back to structure alone (see `or_bench::get`, which degrades to
/// `None` rather than failing the run).
pub fn resolve<'a>(
    models: &[Model],
    scores: &'a [Score],
    bench: Option<&Benchmarks>,
) -> HashMap<String, Match<'a>> {
    // Parse each side once. An arena row carries its identity in *either*
    // field, and which one is richer varies by row: `model_key` is usually
    // the better source, but a benchmark-suffixed key like
    // `deepseek-v4-ch3-thinking-webdev` loses the `flash` that its
    // `display_name` ("deepseek-v4-flash-high-preview") keeps. So we keep
    // both candidate identities and let a row match on either.
    let model_ids: Vec<ModelIdentity> = models
        .iter()
        .map(|m| {
            // Prefer the permaslug: it carries the date that the short id drops.
            let name = m.canonical_slug.as_deref().unwrap_or(&m.id);
            ModelIdentity::parse(name, None)
        })
        .collect();

    // Map each model to its canonical index — itself, unless it is a tier
    // variant (`:free`, `:batch`) whose canonical twin is also present. Tier
    // siblings are the *same model* (same permaslug), so they share one
    // identity and the canonical id claims any arena row on its behalf.
    let canonical_of: Vec<usize> = (0..models.len())
        .map(|i| match models[i].id.split_once(':') {
            Some((base, tier)) if matches!(tier, "free" | "batch") => {
                models.iter().position(|m| m.id == base).unwrap_or(i)
            }
            _ => i,
        })
        .collect();

    let score_ids: Vec<Vec<ModelIdentity>> = scores
        .iter()
        .map(|s| {
            let hint = s.organization.as_deref();
            let mut ids = vec![ModelIdentity::parse(&s.model_key, hint)];
            let from_display = ModelIdentity::parse(&s.display_name, hint);
            if !ids.contains(&from_display) {
                ids.push(from_display);
            }
            ids
        })
        .collect();

    // Oracle: arena row → OpenRouter id, straight from OpenRouter's own data.
    let oracle_target: Vec<Option<String>> = scores
        .iter()
        .map(|s| {
            let b = bench?;
            b.resolve(&s.model_key)
                .or_else(|| b.resolve(&s.display_name))
                .map(|e| e.openrouter_id.to_lowercase())
        })
        .collect();

    let model_key_by_id: HashMap<String, usize> = models
        .iter()
        .enumerate()
        // Index only canonical models: a tier variant never claims a row on
        // its own — the canonical id carries it for the whole family.
        .filter(|(i, _)| canonical_of[*i] == *i)
        .map(|(i, m)| (m.id.to_lowercase(), i))
        .collect();

    let mut candidates: Vec<Candidate> = Vec::new();

    for (si, score) in scores.iter().enumerate() {
        // Tier 1 — oracle. An exact id match is authoritative, so it costs 0
        // and no structural candidate can outrank it.
        if let Some(target) = &oracle_target[si] {
            if let Some(&mi) = model_key_by_id.get(target) {
                candidates.push(Candidate {
                    model_idx: mi,
                    score_idx: si,
                    provenance: Provenance::Oracle,
                    cost: 0,
                });
                // The oracle has spoken for this row; don't also offer it
                // structurally to some other model.
                continue;
            }
        }

        // Tier 2 — structure. Every model whose identity agrees component-wise
        // with *either* of the row's candidate identities. A row matching on
        // its display name is no less valid than one matching on its key, but
        // it is weaker evidence, so it costs slightly more.
        for (mi, mid) in model_ids.iter().enumerate() {
            if canonical_of[mi] != mi {
                continue; // tier variant — its canonical twin stands in
            }
            let Some((which, sid)) = score_ids[si]
                .iter()
                .enumerate()
                .find(|(_, sid)| mid.matches(sid))
            else {
                continue;
            };
            candidates.push(Candidate {
                model_idx: mi,
                score_idx: si,
                provenance: Provenance::Structural,
                cost: structural_cost(&models[mi], score, mid, sid) + which as u32,
            });
        }
    }

    // Global one-to-one assignment. Sorting by cost and taking greedily is
    // optimal enough here: identity matching leaves few genuine contests, and
    // the tie-break below is deterministic. The invariant that matters is
    // that each side is claimed once — enforced by the two `seen` sets.
    candidates.sort_by(|a, b| {
        a.cost
            .cmp(&b.cost)
            .then(a.provenance.cmp(&b.provenance))
            // Deterministic final tie-break so a run is reproducible.
            .then(a.score_idx.cmp(&b.score_idx))
            .then(a.model_idx.cmp(&b.model_idx))
    });

    let mut used_models: HashSet<usize> = HashSet::new();
    let mut used_scores: HashSet<usize> = HashSet::new();
    let mut out: HashMap<String, Match<'a>> = HashMap::new();

    for c in candidates {
        if used_models.contains(&c.model_idx) || used_scores.contains(&c.score_idx) {
            continue;
        }
        used_models.insert(c.model_idx);
        used_scores.insert(c.score_idx);
        out.insert(
            models[c.model_idx].id.clone(),
            Match {
                score: &scores[c.score_idx],
                provenance: c.provenance,
            },
        );
    }

    out
}

/// Cost of a structural pairing — only ever compares pairs that already agree
/// on every identity component, so this just picks the most specific among
/// equals. Lower is better; oracle matches sit below all of these at 0.
fn structural_cost(
    model: &Model,
    score: &Score,
    model_id: &ModelIdentity,
    score_id: &ModelIdentity,
) -> u32 {
    let mut cost: u32 = 10;

    // Agreeing dates is the strongest corroboration two names can offer.
    match (&model_id.date, &score_id.date) {
        (Some(a), Some(b)) if a == b => cost = cost.saturating_sub(4),
        // One side dated and the other not is normal (OpenRouter dates its
        // permaslugs, arena often doesn't) — neither reward nor punish.
        (None, None) => {}
        _ => cost += 1,
    }

    // Display-name proximity as a last discriminator: how much of the arena
    // row's wording the catalog name accounts for.
    let a = token_set(&model.name);
    let b = token_set(&score.display_name);
    if !b.is_empty() {
        let overlap = a.intersection(&b).count();
        let frac = (overlap * 4) / b.len().max(1);
        cost = cost.saturating_sub(frac as u32);
    }

    cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrouter::Pricing;

    fn model(id: &str, name: &str, slug: Option<&str>) -> Model {
        Model {
            id: id.to_string(),
            name: name.to_string(),
            canonical_slug: slug.map(String::from),
            description: None,
            context_length: None,
            pricing: Pricing::default(),
        }
    }

    fn score(key: &str, display: &str, rank: u64, rating: f64, org: Option<&str>) -> Score {
        Score {
            model_key: key.to_string(),
            display_name: display.to_string(),
            rating,
            rank,
            organization: org.map(String::from),
            model_url: None,
        }
    }

    /// The headline regression: `glm-4.5v` must not take `glm-5.3`'s rank.
    #[test]
    fn glm_45v_does_not_steal_53_rank() {
        let models = vec![
            model("z-ai/glm-4.5v", "Z.AI: GLM 4.5V", Some("z-ai/glm-4.5v")),
            model("z-ai/glm-5.3", "Z.AI: GLM 5.3", Some("z-ai/glm-5.3")),
        ];
        let scores = vec![score("glm-5.3", "GLM 5.3", 8, 1450.0, Some("z.ai"))];

        let got = resolve(&models, &scores, None);

        assert!(
            !got.contains_key("z-ai/glm-4.5v"),
            "glm-4.5v must not claim any arena row here"
        );
        assert_eq!(
            got.get("z-ai/glm-5.3").map(|m| m.score.rank),
            Some(8),
            "glm-5.3 should hold rank 8"
        );
    }

    /// The other headline regression: no arena row is ever shared.
    #[test]
    fn ranks_are_unique_across_all_rows() {
        let models = vec![
            model("z-ai/glm-5.3", "Z.AI: GLM 5.3", Some("z-ai/glm-5.3")),
            model("z-ai/glm-5.2", "Z.AI: GLM 5.2", Some("z-ai/glm-5.2")),
            model("z-ai/glm-4.5v", "Z.AI: GLM 4.5V", Some("z-ai/glm-4.5v")),
            model(
                "deepseek/deepseek-v4-pro",
                "DeepSeek: V4 Pro",
                Some("deepseek/deepseek-v4-pro"),
            ),
            model(
                "deepseek/deepseek-v4-flash",
                "DeepSeek: V4 Flash",
                Some("deepseek/deepseek-v4-flash"),
            ),
        ];
        let scores = vec![
            score("glm-5.3", "GLM 5.3", 8, 1450.0, Some("z.ai")),
            score("glm-5.2", "GLM 5.2", 12, 1420.0, Some("z.ai")),
            score(
                "deepseek-v4-pro-high",
                "DeepSeek V4 Pro High",
                3,
                1500.0,
                Some("deepseek"),
            ),
            score(
                "deepseek-v4-flash-high",
                "DeepSeek V4 Flash High",
                15,
                1400.0,
                Some("deepseek"),
            ),
        ];

        let got = resolve(&models, &scores, None);

        let ranks: Vec<u64> = got.values().map(|m| m.score.rank).collect();
        let unique: HashSet<u64> = ranks.iter().copied().collect();
        assert_eq!(ranks.len(), unique.len(), "duplicate ranks in {got:#?}");

        // And the pairings are the right ones, not merely distinct.
        assert_eq!(got["z-ai/glm-5.3"].score.rank, 8);
        assert_eq!(got["z-ai/glm-5.2"].score.rank, 12);
        assert_eq!(got["deepseek/deepseek-v4-pro"].score.rank, 3);
        assert_eq!(got["deepseek/deepseek-v4-flash"].score.rank, 15);
    }

    /// A `flash` row must never land on the `pro` model.
    #[test]
    fn variants_do_not_cross_match() {
        let models = vec![model(
            "deepseek/deepseek-v4-pro",
            "DeepSeek: V4 Pro",
            Some("deepseek/deepseek-v4-pro"),
        )];
        let scores = vec![score(
            "deepseek-v4-flash-high",
            "DeepSeek V4 Flash High",
            15,
            1400.0,
            Some("deepseek"),
        )];

        let got = resolve(&models, &scores, None);
        assert!(got.is_empty(), "flash row must not match the pro model");
    }

    /// No plausible pairing → no rank at all. Failing closed is the point:
    /// a wrong rank is worse than a missing one.
    #[test]
    fn unmatched_model_gets_no_rank() {
        let models = vec![model(
            "acme/unknown-9",
            "Acme: Unknown 9",
            Some("acme/unknown-9"),
        )];
        let scores = vec![score("glm-5.3", "GLM 5.3", 8, 1450.0, Some("z.ai"))];
        assert!(resolve(&models, &scores, None).is_empty());
    }

    /// Arena config tiers collapse onto the base model: `-high` and `-max`
    /// decorate a row without changing which model it is.
    #[test]
    fn config_tier_rows_match_base_model() {
        let models = vec![model(
            "anthropic/claude-opus-5",
            "Anthropic: Claude Opus 5",
            Some("anthropic/claude-opus-5"),
        )];
        for key in ["claude-opus-5", "claude-opus-5-high", "claude-opus-5-max"] {
            let scores = vec![score(key, "Claude Opus 5", 1, 1600.0, Some("anthropic"))];
            let got = resolve(&models, &scores, None);
            assert_eq!(
                got.get("anthropic/claude-opus-5").map(|m| m.score.rank),
                Some(1),
                "arena key {key} should match claude-opus-5"
            );
        }
    }

    /// Two arena rows for one model (a `-high` and a `-low` tier) must not
    /// both bind: one model takes one row.
    #[test]
    fn one_model_claims_only_one_row() {
        let models = vec![model(
            "anthropic/claude-opus-5",
            "Anthropic: Claude Opus 5",
            Some("anthropic/claude-opus-5"),
        )];
        let scores = vec![
            score("claude-opus-5-high", "Claude Opus 5 High", 1, 1600.0, Some("anthropic")),
            score("claude-opus-5-low", "Claude Opus 5 Low", 4, 1550.0, Some("anthropic")),
        ];

        let got = resolve(&models, &scores, None);
        assert_eq!(got.len(), 1, "exactly one pairing, got {got:#?}");
        // Deterministic: the first row wins the tie, not an arbitrary one.
        assert_eq!(got["anthropic/claude-opus-5"].score.rank, 1);
    }

    /// The oracle outranks structure. Here the arena row is an anonymized
    /// codename that no parser could resolve — only OpenRouter's own mapping
    /// knows it, and the resulting match is marked as such.
    #[test]
    fn oracle_resolves_codename_rows() {
        use crate::or_bench::{Benchmarks, OracleEntry};

        let models = vec![model("z-ai/glm-5.3", "Z.AI: GLM 5.3", Some("z-ai/glm-5.3"))];
        let scores = vec![score("stealth-raccoon", "Stealth Raccoon", 2, 1480.0, None)];

        let mut bench = Benchmarks::default();
        bench.by_da_id.insert(
            "stealth-raccoon".to_string(),
            OracleEntry {
                da_model_id: "stealth-raccoon".to_string(),
                openrouter_id: "z-ai/glm-5.3".to_string(),
                permaslug: "z-ai/glm-5.3".to_string(),
                display_name: "GLM 5.3".to_string(),
            },
        );

        let got = resolve(&models, &scores, Some(&bench));
        let m = got
            .get("z-ai/glm-5.3")
            .expect("oracle should bind the codename row");
        assert_eq!(m.score.rank, 2);
        assert_eq!(m.provenance, Provenance::Oracle);
    }

    /// Without benchmark data the matcher still works — it just loses the
    /// oracle tier. `or_bench::get` degrades to `None` by design.
    #[test]
    fn missing_oracle_degrades_to_structural() {
        let models = vec![model("z-ai/glm-5.3", "Z.AI: GLM 5.3", Some("z-ai/glm-5.3"))];
        let scores = vec![score("glm-5.3", "GLM 5.3", 8, 1450.0, Some("z.ai"))];

        let got = resolve(&models, &scores, None);
        assert_eq!(got["z-ai/glm-5.3"].provenance, Provenance::Structural);
    }

    /// Same inputs, same output — including which row wins a contest.
    #[test]
    fn assignment_is_deterministic() {
        let models = vec![
            model("z-ai/glm-5.3", "Z.AI: GLM 5.3", Some("z-ai/glm-5.3")),
            model("z-ai/glm-5.2", "Z.AI: GLM 5.2", Some("z-ai/glm-5.2")),
        ];
        let scores = vec![
            score("glm-5.3", "GLM 5.3", 8, 1450.0, Some("z.ai")),
            score("glm-5.2", "GLM 5.2", 12, 1420.0, Some("z.ai")),
        ];

        let first = resolve(&models, &scores, None);
        for _ in 0..8 {
            let again = resolve(&models, &scores, None);
            assert_eq!(first.len(), again.len());
            for (id, m) in &first {
                assert_eq!(again[id].score.rank, m.score.rank);
                assert_eq!(again[id].provenance, m.provenance);
            }
        }
    }
}
