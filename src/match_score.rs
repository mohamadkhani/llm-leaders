use crate::arena::Score;
use crate::openrouter::Model;

/// Known mismatches between arena display names and OpenRouter IDs.
/// Arena sometimes obfuscates model keys (e.g. "kinsley-5jmg" for qwen);
/// the display name is usually clean. This table handles the rest.
fn alias_map() -> Vec<(&'static str, &'static str)> {
    vec![
        // (arena display-name fragment, openrouter id fragment)
        ("claude-opus-5-max", "anthropic/claude-opus-5"),
        ("claude-opus-5-high", "anthropic/claude-opus-5"),
        ("claude-fable-5", "anthropic/claude-fable-5"),
        ("claude-sonnet-5", "anthropic/claude-sonnet-5"),
        ("gemini-3-pro-preview", "google/gemini-3-pro-preview"),
    ]
}

/// Normalize a name for comparison: lowercase, drop vendor prefix, drop
/// version separators, drop qualifiers like -webdev/-high/-max.
fn norm(s: &str) -> String {
    let base = s.rsplit('/').next().unwrap_or(s); // strip vendor/
    let lower = base.to_lowercase();
    // Drop arena harness/variant suffixes.
    let lower = lower
        .replace("-webdev", "")
        .replace("-high", "")
        .replace("-max", "")
        .replace("-low", "")
        .replace(" (codex-harness)", "");
    lower.replace(['.', '-', '_'], "")
}

/// Find the best arena score for an OpenRouter model.
/// Strategy: exact normalized match, then alias table, then substring.
pub fn score_for<'a>(model: &'a Model, scores: &'a [Score]) -> Option<&'a Score> {
    let target = norm(&model.id);

    // 1. Alias table — overrides everything.
    for (arena_frag, or_frag) in alias_map() {
        if model.id.contains(or_frag) {
            if let Some(s) = scores.iter().find(|s| s.display_name.contains(arena_frag)) {
                return Some(s);
            }
        }
    }

    // 2. Exact normalized match on display name or model key.
    if let Some(s) = scores
        .iter()
        .find(|s| norm(&s.display_name) == target || norm(&s.model_key) == target)
    {
        return Some(s);
    }

    // 3. Substring: arena display name contains the model's bare id tail, or vice versa.
    let bare = model.id.rsplit('/').next().unwrap_or(&model.id).to_lowercase();
    if let Some(s) = scores.iter().find(|s| {
        let dn = s.display_name.to_lowercase();
        dn.contains(&bare) || bare.contains(&dn)
    }) {
        return Some(s);
    }

    None
}
