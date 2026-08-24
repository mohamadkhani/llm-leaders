//! Structural model identity — the shared abstraction that both sides
//! (OpenRouter permaslug, arena `model_key` / `display_name`) parse into.
//!
//! Matching is component-wise agreement on `{ vendor, family, version,
//! variants, size }`, not string similarity. See ADR-0001.
//!
//! The parser is pure: no I/O, no network, deterministic. All of its
//! behaviour is pinned by table-driven unit tests.

use std::collections::{BTreeSet, HashSet};

/// Parsed identity of a model name. All fields are lowercased; absent
/// components are `None` / empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelIdentity {
    pub vendor: Option<String>,
    pub family: Option<String>,
    /// Dotted version, including a vision-style alpha tail: `4`, `5.3`,
    /// `4.5v`. The tail matters — `glm-5v-turbo` is not `glm-5-turbo`.
    pub version: Option<String>,
    /// Genuine identity variants — `flash`, `pro`, `code`, `opus`, `fast` —
    /// all of which must agree. A *set*, because names stack them
    /// (`claude-opus-5-fast`, `deepseek-v4-flash-vision-exp`) and a single
    /// slot would silently collapse distinct models onto each other.
    pub variants: BTreeSet<String>,
    /// Parameter-count marker: `27b`, `32b`, `4t`. Separates
    /// `qwen3.8-27b` from `qwen3.8-2.4t-a95b` and from `qwen3.8-max`.
    pub size: Option<String>,
    /// 8-digit `YYYYMMDD` if present, else None. A tie-break, never a gate.
    pub date: Option<String>,
}

/// Vendor synonyms — maps arena-side vendor tokens to the OpenRouter vendor
/// prefix. Mirrors the table in `match_score.rs` (preserved on import).
const VENDOR_SYNONYMS: &[(&str, &[&str])] = &[
    ("anthropic", &["anthropic", "claude"]),
    ("openai", &["openai", "gpt", "o1", "o3", "o4"]),
    ("google", &["google", "gemini"]),
    ("deepseek", &["deepseek"]),
    ("qwen", &["alibaba", "qwen"]),
    ("z-ai", &["z.ai", "z-ai", "glm"]),
    ("moonshotai", &["moonshot", "kimi", "moonshotai"]),
    ("xiaomi", &["xiaomi", "mimo"]),
    ("tencent", &["tencent", "hy3", "hunyuan"]),
    ("minimax", &["minimax"]),
    ("meta", &["meta", "muse"]),
    ("x-ai", &["spacexai", "x.ai", "x-ai", "grok", "xai"]),
    ("mistralai", &["mistral", "mistralai", "devstral", "codestral"]),
    ("nvidia", &["nvidia", "nemotron"]),
    ("poolside", &["poolside", "laguna"]),
    ("stepfun", &["stepfun", "step"]),
];

/// Normalize any vendor token to its canonical OpenRouter prefix.
fn canonical_vendor(token: &str) -> Option<String> {
    let t = token.to_lowercase();
    for (canonical, aliases) in VENDOR_SYNONYMS {
        if *canonical == t || aliases.iter().any(|a| *a == t) {
            return Some((*canonical).to_string());
        }
    }
    None
}

/// Config suffixes that decorate a name but are *not* model identity.
/// Stripped only when the remainder still parses (self-guarding — never
/// strip into an unparseable residue).
///
/// `max` lives here, not in the variants: it reads as a config tier far more
/// often than as identity (arena's `glm-5.3-max` *is* `z-ai/glm-5.3`). The
/// genuinely distinct `qwen/qwen3.8-max` is separated by its lack of a size
/// marker and, failing that, resolved by the oracle tier.
const CONFIG_SUFFIXES: &[&str] = &[
    "high",
    "low",
    "medium",
    "thinking",
    "nothinking",
    "ch1",
    "ch2",
    "ch3",
    "preview",
    "webdev",
    "latest",
    "free",
    "batch",
    "max",
    "xhigh",
];

/// Genuine identity variants — never stripped.
const IDENTITY_VARIANTS: &[&str] = &[
    "flash",
    "pro",
    "code",
    "coder",
    "codex",
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "luna",
    "sol",
    "terra",
    "air",
    "turbo",
    "lite",
    "plus",
    "instant",
    "reasoner",
    "reasoning",
    "chat",
    "vision",
    "fast",
    "mini",
    "nano",
    "spark",
    "exp",
    "instruct",
];

fn is_date(s: &str) -> bool {
    s.len() == 8 && s.starts_with("202") && s.chars().all(|c| c.is_ascii_digit())
}

/// Pull the first 8-digit YYYYMMDD date out of a token list.
fn extract_date(tokens: &[String]) -> Option<String> {
    tokens.iter().find(|t| is_date(t)).cloned()
}

/// Split a token into `(leading alpha, dotted numeric, trailing alpha)`.
///
/// `qwen3.8` → `("qwen", "3.8", "")`, `k2.7` → `("k", "2.7", "")`,
/// `4.5v` → `("", "4.5", "v")`, `v4` → `("v", "4", "")`, `glm` → `("glm", "", "")`.
///
/// This is what lets a family and its version share one token: the arena and
/// OpenRouter both write `qwen3.8` and `k2.7` that way.
fn split_alpha_num(tok: &str) -> (&str, &str, &str) {
    let b = tok.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let mut j = i;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
        j += 1;
    }
    // Don't leave a trailing '.' on the numeric run.
    let mut num_end = j;
    while num_end > i && b[num_end - 1] == b'.' {
        num_end -= 1;
    }
    (&tok[..i], &tok[i..num_end], &tok[j..])
}

/// A parameter-count token: a number with a `b`/`t` unit — `27b`, `32b`,
/// `4t`, `a95b`. These are size markers, never versions.
fn is_size_token(tok: &str) -> bool {
    let (_, num, tail) = split_alpha_num(tok);
    !num.is_empty() && (tail == "b" || tail == "t")
}

/// Split a string into lowercase alphanumeric tokens, then rejoin dotted
/// versions that the split broke apart.
fn tokenize(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let tokens: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    merge_dotted_versions(tokens)
}

/// Rejoin a version that tokenization split on its dot: `["qwen3", "8"]` →
/// `["qwen3.8"]`, `["k2", "7"]` → `["k2.7"]`, `["4", "5v"]` → `["4.5v"]`.
///
/// Three guards keep this from over-merging:
/// - a token already holding a dot is never extended, so
///   `glm-5.2-20260616` can't become `5.2.20260616`;
/// - an 8-digit date is never absorbed into a version;
/// - a size token is never absorbed, so `glm-4-32b` keeps version `4` and
///   size `32b` instead of collapsing to `4.32b`.
fn merge_dotted_versions(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let cur = &tokens[i];
        if i + 1 < tokens.len() && !cur.contains('.') {
            let next = &tokens[i + 1];
            let mergeable = cur.chars().last().is_some_and(|c| c.is_ascii_digit())
                && next.chars().next().is_some_and(|c| c.is_ascii_digit())
                && !is_date(next)
                && !is_size_token(next);
            if mergeable {
                out.push(format!("{cur}.{next}"));
                i += 2;
                continue;
            }
        }
        out.push(cur.clone());
        i += 1;
    }
    out
}

impl ModelIdentity {
    /// Parse any model name into an identity.
    ///
    /// Sources, in priority order:
    /// - OpenRouter permaslug/slug: `deepseek/deepseek-v4-flash-20260731`
    /// - arena `model_key`: `deepseek-v4-flash-high-20260730-webdev`
    /// - arena `display_name`: `DeepSeek: DeepSeek V4 Flash High`
    ///
    /// `hint_vendor` lets the caller pass a known vendor (e.g. from the
    /// catalog) when the name itself doesn't carry one — arena display names
    /// like `GLM 5.1` have none.
    pub fn parse(name: &str, hint_vendor: Option<&str>) -> ModelIdentity {
        // Drop an OpenRouter tier suffix (`:free`, `:batch`) — a row variant,
        // not identity. Only for slug-shaped tails: a display name like
        // `DeepSeek: DeepSeek V4` also has a colon but means nothing by it.
        let base = match name.rsplit_once(':') {
            Some((head, tail)) if matches!(tail, "free" | "batch" | "extended") => head,
            _ => name,
        };

        // Vendor: the `/`-prefix if present, else a synonym anywhere in the
        // name, else the caller's hint.
        let (vendor, body) = match base.split_once('/') {
            Some((pre, post)) => {
                let v =
                    canonical_vendor(pre.trim()).or_else(|| hint_vendor.and_then(canonical_vendor));
                (v, post.to_string())
            }
            None => {
                let v = tokenize(base)
                    .iter()
                    .find_map(|t| canonical_vendor(t))
                    .or_else(|| hint_vendor.and_then(canonical_vendor));
                (v, base.to_string())
            }
        };

        let body_tokens = tokenize(&body);
        let stripped = strip_config_suffixes(&body_tokens);
        let date = extract_date(&stripped).or_else(|| extract_date(&body_tokens));

        let mut version: Option<String> = None;
        let mut variants: BTreeSet<String> = BTreeSet::new();
        let mut family: Option<String> = None;
        let mut size: Option<String> = None;

        for t in &stripped {
            if Some(t.as_str()) == date.as_deref() {
                continue;
            }

            // A parameter-count marker, e.g. `27b`. Keep the first one: for
            // `2.4t-a95b` that's `4t`, which is enough to separate it from
            // `27b` (size is a matching component, never displayed).
            if is_size_token(t) {
                if size.is_none() {
                    size = Some(t.clone());
                }
                continue;
            }

            let (alpha, num, tail) = split_alpha_num(t);

            // `v4` / `v3.1` — a bare version marker, never a family.
            if alpha == "v" && !num.is_empty() {
                if version.is_none() {
                    version = Some(num.to_string());
                }
                continue;
            }

            if IDENTITY_VARIANTS.contains(&t.as_str()) {
                variants.insert(t.clone());
                continue;
            }

            // A family token, optionally carrying its own version:
            // `qwen3.8` → family `qwen` + version `3.8`; `glm` → family only.
            if !alpha.is_empty() && family.is_none() && !is_config_suffix(alpha) {
                family = Some(alpha.to_string());
            }
            if !num.is_empty() && version.is_none() {
                // Keep a vision-style tail on the version: `4.5v`, `5v`.
                version = Some(format!("{num}{tail}"));
            }
        }

        ModelIdentity {
            vendor,
            family,
            version,
            variants,
            size,
            date,
        }
    }

    /// Two identities are structurally compatible iff vendor, family,
    /// version, variants and size all agree.
    ///
    /// `None` matches only `None` — a missing component is never a wildcard.
    /// That is the flaw this replaces: the old scorer let an absent field
    /// pass for free, which is how `glm-4.5v` reached arena's `glm-5.3`.
    /// `date` is deliberately excluded: it disambiguates between otherwise
    /// equal candidates rather than gating them.
    pub fn matches(&self, other: &ModelIdentity) -> bool {
        self.vendor == other.vendor
            && self.family == other.family
            && self.version == other.version
            && self.variants == other.variants
            && self.size == other.size
    }
}

fn is_config_suffix(s: &str) -> bool {
    CONFIG_SUFFIXES.contains(&s)
}

/// Strip config suffixes from a token list, self-guarded: the result must
/// still carry a family or a version. If stripping would leave an
/// unparseable residue, keep the tokens as they were.
fn strip_config_suffixes(tokens: &[String]) -> Vec<String> {
    let date = extract_date(tokens);

    let candidate: Vec<String> = tokens
        .iter()
        .filter(|t| Some(t.as_str()) == date.as_deref() || !is_config_suffix(t))
        .cloned()
        .collect();

    let has_family_or_version = candidate.iter().any(|t| {
        if Some(t.as_str()) == date.as_deref() {
            return false;
        }
        let (alpha, num, _) = split_alpha_num(t);
        !num.is_empty() || (!alpha.is_empty() && !is_config_suffix(alpha))
    });

    if has_family_or_version {
        candidate
    } else {
        tokens.to_vec()
    }
}

/// Exposed for the matcher's display-name proximity tie-break.
pub fn token_set(s: &str) -> HashSet<String> {
    tokenize(s).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(
        vendor: Option<&str>,
        family: Option<&str>,
        version: Option<&str>,
        variants: &[&str],
        size: Option<&str>,
        date: Option<&str>,
    ) -> ModelIdentity {
        ModelIdentity {
            vendor: vendor.map(String::from),
            family: family.map(String::from),
            version: version.map(String::from),
            variants: variants.iter().map(|s| s.to_string()).collect(),
            size: size.map(String::from),
            date: date.map(String::from),
        }
    }

    /// The acceptance table: one row per real name shape we must handle.
    #[test]
    fn acceptance_table() {
        let cases: &[(&str, Option<&str>, ModelIdentity)] = &[
            // OpenRouter permaslug: vendor prefix + date.
            (
                "deepseek/deepseek-v4-flash-20260731",
                None,
                ident(
                    Some("deepseek"),
                    Some("deepseek"),
                    Some("4"),
                    &["flash"],
                    None,
                    Some("20260731"),
                ),
            ),
            // Arena key: `high` and `webdev` are config, the date survives.
            (
                "deepseek-v4-flash-high-20260730-webdev",
                Some("deepseek"),
                ident(
                    Some("deepseek"),
                    Some("deepseek"),
                    Some("4"),
                    &["flash"],
                    None,
                    Some("20260730"),
                ),
            ),
            (
                "z-ai/glm-5.3",
                None,
                ident(Some("z-ai"), Some("glm"), Some("5.3"), &[], None, None),
            ),
            // The vision tail rides on the version, keeping 4.5v ≠ 4.5.
            (
                "z-ai/glm-4.5v",
                None,
                ident(Some("z-ai"), Some("glm"), Some("4.5v"), &[], None, None),
            ),
            (
                "moonshotai/kimi-k2.7-code",
                None,
                ident(
                    Some("moonshotai"),
                    Some("kimi"),
                    Some("2.7"),
                    &["code"],
                    None,
                    None,
                ),
            ),
            // Size marker separates the 27b from the 2.4t and from -max.
            (
                "qwen/qwen3.8-27b",
                None,
                ident(
                    Some("qwen"),
                    Some("qwen"),
                    Some("3.8"),
                    &[],
                    Some("27b"),
                    None,
                ),
            ),
            (
                "qwen/qwen3.8-max",
                None,
                ident(Some("qwen"), Some("qwen"), Some("3.8"), &[], None, None),
            ),
            (
                "anthropic/claude-opus-5",
                None,
                ident(
                    Some("anthropic"),
                    Some("claude"),
                    Some("5"),
                    &["opus"],
                    None,
                    None,
                ),
            ),
            // `high` is config, so the arena tier collapses onto the model.
            (
                "anthropic/claude-opus-5-high",
                None,
                ident(
                    Some("anthropic"),
                    Some("claude"),
                    Some("5"),
                    &["opus"],
                    None,
                    None,
                ),
            ),
            // Stacked variants: `fast` must not be swallowed by `opus`.
            (
                "anthropic/claude-opus-5-fast",
                None,
                ident(
                    Some("anthropic"),
                    Some("claude"),
                    Some("5"),
                    &["fast", "opus"],
                    None,
                    None,
                ),
            ),
            // ch1 + thinking + webdev are all config; `pro` is identity.
            (
                "deepseek-v4-pro-ch1-thinking-webdev",
                Some("deepseek"),
                ident(
                    Some("deepseek"),
                    Some("deepseek"),
                    Some("4"),
                    &["pro"],
                    None,
                    None,
                ),
            ),
            // `4` is the version and `32b` the size — not a merged `4.32b`.
            (
                "z-ai/glm-4-32b",
                None,
                ident(Some("z-ai"), Some("glm"), Some("4"), &[], Some("32b"), None),
            ),
        ];
        for (input, hint, want) in cases {
            let got = ModelIdentity::parse(input, *hint);
            assert_eq!(got, *want, "parse({input:?})");
        }
    }

    /// The defining bug: `glm-4.5v` must never reach arena's `glm-5.3`.
    #[test]
    fn glm_45v_does_not_match_53() {
        let catalog = ModelIdentity::parse("z-ai/glm-4.5v", None);
        let arena = ModelIdentity::parse("glm-5.3", Some("glm"));
        assert!(!catalog.matches(&arena), "glm-4.5v must not match glm-5.3");
    }

    /// Arena writes `glm-5.3-max` for the model OpenRouter calls `glm-5.3`.
    #[test]
    fn arena_max_tier_collapses_onto_base_model() {
        let catalog = ModelIdentity::parse("z-ai/glm-5.3", None);
        for arena_name in ["glm-5.3", "glm-5.3-max"] {
            let arena = ModelIdentity::parse(arena_name, Some("glm"));
            assert!(
                catalog.matches(&arena),
                "{arena_name} should match z-ai/glm-5.3"
            );
        }
    }

    /// Same family and version, different variant → not a match.
    #[test]
    fn flash_does_not_match_pro() {
        let flash = ModelIdentity::parse("deepseek/deepseek-v4-flash-20260731", None);
        let pro = ModelIdentity::parse("deepseek/deepseek-v4-pro", None);
        assert_eq!(flash.family, pro.family);
        assert_eq!(flash.version, pro.version);
        assert!(!flash.matches(&pro), "flash ≠ pro via the variant gate");
    }

    /// Distinct models that a single-slot variant field would have merged.
    #[test]
    fn stacked_variants_stay_distinct() {
        let pairs = [
            ("anthropic/claude-opus-5", "anthropic/claude-opus-5-fast"),
            ("z-ai/glm-5-turbo", "z-ai/glm-5v-turbo"),
            (
                "deepseek/deepseek-v4-flash",
                "deepseek/deepseek-v4-flash-vision-exp",
            ),
            ("qwen/qwen3.8-27b", "qwen/qwen3.8-2.4t-a95b"),
            ("qwen/qwen3.8-27b", "qwen/qwen3.8-max"),
        ];
        for (a, b) in pairs {
            let ia = ModelIdentity::parse(a, None);
            let ib = ModelIdentity::parse(b, None);
            assert!(!ia.matches(&ib), "{a} must not match {b}\n  {ia:?}\n  {ib:?}");
        }
    }

    /// A date is a tie-break, not a gate: an undated name still matches its
    /// dated permaslug when every identity component agrees.
    #[test]
    fn date_does_not_gate_a_match() {
        let undated = ModelIdentity::parse("deepseek/deepseek-v4-pro", None);
        let dated = ModelIdentity::parse("deepseek/deepseek-v4-pro-20260423", None);
        assert!(undated.matches(&dated));
    }

    #[test]
    fn vendor_synonyms_normalize() {
        let a = ModelIdentity::parse("z.ai/glm-5.1", None);
        let b = ModelIdentity::parse("z-ai/glm-5.1", None);
        assert_eq!(a.vendor.as_deref(), Some("z-ai"));
        assert!(a.matches(&b));
    }

    /// Arena display names carry no vendor prefix; the caller supplies one.
    #[test]
    fn display_name_with_hint_vendor() {
        let a = ModelIdentity::parse("DeepSeek: DeepSeek V4 Flash High", Some("deepseek"));
        assert_eq!(a.family.as_deref(), Some("deepseek"));
        assert_eq!(a.version.as_deref(), Some("4"));
        assert!(a.variants.contains("flash"));
    }

    /// Stripping config suffixes must never erase a real identity core.
    #[test]
    fn stripping_preserves_the_core() {
        // Every config suffix here decorates a real model; all of them must
        // strip down to the same identity as the bare name.
        let bare = ModelIdentity::parse("z-ai/glm-5.2", None);
        for decorated in [
            "glm-5.2-high",
            "glm-5.2-thinking",
            "glm-5.2-webdev",
            "glm-5.2-high-thinking-webdev",
            "glm-5.2-ch1",
        ] {
            let got = ModelIdentity::parse(decorated, Some("glm"));
            assert!(
                bare.matches(&got),
                "{decorated} should strip to z-ai/glm-5.2, got {got:?}"
            );
        }
    }

    /// A name made only of config suffixes has no identity, and an empty
    /// identity must not match a real model — the matcher fails closed
    /// rather than pairing an unparseable name with an arbitrary row.
    #[test]
    fn all_config_name_matches_nothing() {
        let empty = ModelIdentity::parse("thinking-webdev", None);
        assert_eq!(empty.family, None);
        assert_eq!(empty.version, None);
        for real in ["z-ai/glm-5.2", "anthropic/claude-opus-5"] {
            let id = ModelIdentity::parse(real, None);
            assert!(!empty.matches(&id), "empty identity must not match {real}");
        }
    }
}
