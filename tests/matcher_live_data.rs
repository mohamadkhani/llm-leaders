//! Offline integration test: the real matcher against real captured
//! payloads. These fixtures were captured live on 2026-08-24 and trimmed to
//! the interesting families (deepseek v4, glm 4.5v/5.3, qwen3.8, claude
//! opus 5, kimi k2.7) — the same shape as production, minus the bulk.

use std::collections::HashSet;

use llm_leaders::arena::Score;
use llm_leaders::matcher::{resolve, Provenance};
use llm_leaders::openrouter::{Model, ModelsResponse};

fn catalog() -> Vec<Model> {
    let raw = include_str!("fixtures/v1_models.json");
    serde_json::from_str::<ModelsResponse>(raw)
        .expect("v1 fixture parses")
        .data
}

fn arena() -> Vec<Score> {
    let raw = include_str!("fixtures/arena.json");
    #[derive(serde::Deserialize)]
    struct ArenaCache {
        entries: Vec<Score>,
    }
    serde_json::from_str::<ArenaCache>(raw)
        .expect("arena fixture parses")
        .entries
}

fn bench() -> llm_leaders::or_bench::Benchmarks {
    llm_leaders::or_bench::parse_json(include_str!("fixtures/benchmarks.json"))
        .expect("benchmarks fixture parses")
}

/// The acceptance table from issue #3, against captured live payloads.
#[test]
fn acceptance_table_on_live_fixtures() {
    let models = catalog();
    let scores = arena();
    let b = bench();
    let got = resolve(&models, &scores, Some(&b));

    let rank_of = |id: &str| got.get(id).map(|m| m.score.rank);
    let provenance_of = |id: &str| got.get(id).map(|m| m.provenance);

    // The defining bug: glm-4.5v must not claim any arena row…
    assert_eq!(
        rank_of("z-ai/glm-4.5v"),
        None,
        "glm-4.5v must never claim an arena rank"
    );
    // …while glm-5.3 holds #8.
    assert_eq!(
        rank_of("z-ai/glm-5.3"),
        Some(8),
        "glm-5.3 should hold rank #8"
    );
    assert_eq!(provenance_of("z-ai/glm-5.3"), Some(Provenance::Structural));

    // DeepSeek pairs, matched by date against the dated permaslugs.
    assert_eq!(rank_of("deepseek/deepseek-v4-flash-0731"), Some(13));
    assert_eq!(rank_of("deepseek/deepseek-v4-pro-0813"), Some(12));
    // Two undated v4 models and two undated arena rows. `pro` is pinned by
    // the oracle (its `da_model_id` `deepseek-v4-pro` matches the row's
    // display name); `flash` resolves structurally via its display name —
    // and never crosses onto the pro row.
    assert_eq!(rank_of("deepseek/deepseek-v4-flash"), Some(58));
    assert_eq!(rank_of("deepseek/deepseek-v4-pro"), Some(49));
    assert_eq!(provenance_of("deepseek/deepseek-v4-pro"), Some(Provenance::Oracle));
    assert_eq!(provenance_of("deepseek/deepseek-v4-flash"), Some(Provenance::Structural));

    // Claude: `max` and `high` are config tiers of the same model. Arena
    // lists both (#1 and #4); one model claims one row — the better one.
    assert_eq!(rank_of("anthropic/claude-opus-5"), Some(1));
    // The fast sibling never crosses onto the base model's row.
    assert_eq!(rank_of("anthropic/claude-opus-5-fast"), None);

    // Kimi: oracle-resolved via the da_model_id `kimi-k2.7-code`.
    assert_eq!(rank_of("moonshotai/kimi-k2.7-code"), Some(41));
    assert_eq!(provenance_of("moonshotai/kimi-k2.7-code"), Some(Provenance::Oracle));

    // Tier siblings are the same model as their canonical twin: they inherit
    // its pairing rather than claiming a row of their own.
    let base_rank = rank_of("z-ai/glm-5.2");
    assert_eq!(rank_of("z-ai/glm-5.2:free"), base_rank);
    assert_eq!(rank_of("z-ai/glm-5.2:batch"), base_rank);
}

/// THE property check: no arena row is claimed by more than one model
/// family. Tier variants (`:free`) deliberately share their canonical
/// twin's pairing, so they are excluded before counting.
#[test]
fn no_arena_rank_is_claimed_twice() {
    let models = catalog();
    let scores = arena();
    let b = bench();
    let got = resolve(&models, &scores, Some(&b));

    let mut seen: HashSet<usize> = HashSet::new();
    for (id, m) in &got {
        if id.contains(":free") || id.contains(":batch") {
            continue; // inherits its canonical twin's pairing by design
        }
        assert!(
            seen.insert(m.score as *const Score as usize),
            "row #{}/{} claimed twice",
            m.score.rank,
            m.score.model_key,
        );
    }
    // And every variant that did get an entry agrees with its base.
    for (id, m) in &got {
        if let Some(base) = id.split(':').next() {
            if id != base {
                assert_eq!(
                    got.get(base).map(|b| b.score as *const Score as usize),
                    Some(m.score as *const Score as usize),
                    "{id} disagrees with its canonical twin {base}"
                );
            }
        }
    }
}

/// Without the oracle (frontend endpoint down), the same table renders with
/// the structural tier alone — glm-5.3 keeps its rank, no duplicates.
#[test]
fn structural_only_when_oracle_unavailable() {
    let models = catalog();
    let scores = arena();
    let got = resolve(&models, &scores, None);

    assert_eq!(
        got.get("z-ai/glm-5.3").map(|m| m.score.rank),
        Some(8),
        "glm-5.3 keeps rank 8 without the oracle"
    );
    assert!(got.get("z-ai/glm-4.5v").is_none());

    let mut seen: HashSet<usize> = HashSet::new();
    for (id, m) in &got {
        if id.contains(":free") || id.contains(":batch") {
            continue;
        }
        assert!(
            seen.insert(m.score as *const Score as usize),
            "row #{} claimed twice (no-oracle path)",
            m.score.rank
        );
    }
}
