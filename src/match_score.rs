use crate::arena::Score;
use crate::openrouter::Model;
use regex::Regex;
use std::collections::HashSet;

/// Vendor synonyms mapping between OpenRouter model ID prefix and Arena metadata (organization/display/key/url).
const VENDOR_SYNONYMS: &[(&str, &[&str])] = &[
    ("anthropic", &["anthropic"]),
    ("openai", &["openai"]),
    ("google", &["google"]),
    ("deepseek", &["deepseek"]),
    ("qwen", &["alibaba", "qwen"]),
    ("z-ai", &["z.ai", "z-ai", "glm"]),
    ("moonshotai", &["moonshot", "kimi"]),
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

/// Distinctive model tier / family keywords where a conflict is considered incompatible.
const DISTINCTIVE_KEYWORDS: &[&str] = &[
    "pro", "flash", "opus", "sonnet", "haiku", "fable", "luna", "sol", "terra", "codex", "coder",
    "r1", "lite", "turbo", "plus", "max", "instant", "thinking",
];

/// Pairs of keywords that cannot match across models (e.g. Pro vs Flash).
const INCOMPATIBLE_PAIRS: &[(&[&str], &[&str])] = &[
    (&["pro"], &["flash"]),
    (&["opus"], &["sonnet"]),
    (&["opus"], &["haiku"]),
    (&["sonnet"], &["haiku"]),
    (&["luna"], &["sol", "terra"]),
    (&["sol"], &["luna", "terra"]),
    (&["terra"], &["luna", "sol"]),
];

/// Extract 8-digit (YYYYMMDD) and 4-digit (MMDD) date snapshots from a text string.
fn extract_dates(s: &str) -> (HashSet<String>, HashSet<String>) {
    let s_lower = s.to_lowercase();
    let re_d8 = Regex::new(r"202[4-6][01][0-9][0-3][0-9]").unwrap();
    let re_d8_hyphen = Regex::new(r"202[4-6]-[01][0-9]-[0-3][0-9]").unwrap();
    let re_d4 = Regex::new(r"(?:^|[^a-z0-9])((?:0[1-9]|1[0-2])[0-3][0-9])(?:$|[^a-z0-9])").unwrap();

    let mut d8 = HashSet::new();
    for m in re_d8.find_iter(&s_lower) {
        d8.insert(m.as_str().to_string());
    }
    for m in re_d8_hyphen.find_iter(&s_lower) {
        d8.insert(m.as_str().replace('-', ""));
    }

    let mut d4 = HashSet::new();
    for cap in re_d4.captures_iter(&s_lower) {
        if let Some(m) = cap.get(1) {
            d4.insert(m.as_str().to_string());
        }
    }

    (d8, d4)
}

/// Extract clean alphanumeric tokens, dropping date numbers.
fn extract_tokens(s: &str) -> HashSet<String> {
    let re_clean = Regex::new(r"[^a-z0-9.]").unwrap();
    let re_date = Regex::new(r"\b202[4-6][0-9]{4}\b|\b[0-9]{4}\b").unwrap();
    let re_split_num = Regex::new(r"^([a-z]+)(\d+(?:\.\d+)?)$").unwrap();

    let s_lower = s.to_lowercase();
    let cleaned = re_clean.replace_all(&s_lower, " ");
    let without_dates = re_date.replace_all(&cleaned, " ");

    let mut tokens = HashSet::new();
    for raw in without_dates.split_whitespace() {
        let tok = raw.trim_matches('.');
        if !tok.is_empty() {
            tokens.insert(tok.to_string());
            if let Some(caps) = re_split_num.captures(tok) {
                tokens.insert(caps[1].to_string());
                tokens.insert(caps[2].to_string());
            }
        }
    }
    tokens
}

/// Dynamic heuristic scoring between an OpenRouter Model and an Arena Score entry.
fn calculate_match_score(model: &Model, arena: &Score) -> i32 {
    let m_id_clean = model.id.split(':').next().unwrap_or(&model.id).to_lowercase();
    let m_name = model.name.to_lowercase();
    let m_slug = model.canonical_slug.as_deref().unwrap_or("").to_lowercase();

    let a_disp = arena.display_name.to_lowercase();
    let a_key = arena.model_key.to_lowercase();
    let a_org = arena.organization.as_deref().unwrap_or("").to_lowercase();
    let a_url = arena.model_url.as_deref().unwrap_or("").to_lowercase();

    // 1. Vendor check
    let m_vendor: String = m_id_clean
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let vendor_matched = if !m_vendor.is_empty() {
        match VENDOR_SYNONYMS.iter().find(|(v, _)| *v == m_vendor.as_str()) {
            Some((_, orgs)) => orgs.iter().any(|org| {
                a_org.contains(org)
                    || a_disp.contains(org)
                    || a_key.contains(org)
                    || a_url.contains(org)
            }),
            None => a_org.contains(&m_vendor)
                || a_disp.contains(&m_vendor)
                || a_key.contains(&m_vendor)
                || a_url.contains(&m_vendor),
        }
    } else {
        false
    };

    if !vendor_matched {
        return -1000;
    }

    let mut score = 50;

    let m_tail: String = m_id_clean.split('/').last().unwrap_or("").to_string();
    let m_text = format!("{m_tail} {m_name} {m_slug}");
    let a_text = format!("{a_disp} {a_key}");

    let m_tokens = extract_tokens(&m_text);
    let a_tokens = extract_tokens(&a_text);

    // 2. Version number alignment
    let re_ver = Regex::new(r"^\d+(?:\.\d+)?$").unwrap();
    let m_nums: HashSet<String> = m_tokens.iter().filter(|t| re_ver.is_match(t)).cloned().collect();
    let a_nums: HashSet<String> = a_tokens.iter().filter(|t| re_ver.is_match(t)).cloned().collect();

    if !m_nums.is_empty() && !a_nums.is_empty() {
        let exact_overlap_count = m_nums.intersection(&a_nums).count() as i32;
        if exact_overlap_count > 0 {
            score += 60 * exact_overlap_count;
        } else {
            // Numbers conflict! (e.g. 5.2 vs 5.1, v3 vs v4)
            return -500;
        }
    }

    // 3. Distinctive keyword alignment
    let m_distinct: HashSet<String> = m_tokens
        .iter()
        .filter(|s| DISTINCTIVE_KEYWORDS.contains(&s.as_str()))
        .cloned()
        .collect();
    let a_distinct: HashSet<String> = a_tokens
        .iter()
        .filter(|s| DISTINCTIVE_KEYWORDS.contains(&s.as_str()))
        .cloned()
        .collect();

    if !m_distinct.is_empty() && !a_distinct.is_empty() {
        let overlap_count = m_distinct.intersection(&a_distinct).count() as i32;
        let diff_m = m_distinct.difference(&a_distinct).count() as i32;
        let diff_a = a_distinct.difference(&m_distinct).count() as i32;

        for &(s1, s2) in INCOMPATIBLE_PAIRS {
            let has_s1_m = s1.iter().any(|w| m_distinct.contains(*w));
            let has_s2_a = s2.iter().any(|w| a_distinct.contains(*w));
            let has_s2_m = s2.iter().any(|w| m_distinct.contains(*w));
            let has_s1_a = s1.iter().any(|w| a_distinct.contains(*w));

            if (has_s1_m && has_s2_a) || (has_s2_m && has_s1_a) {
                return -500;
            }
        }

        score += 30 * overlap_count;
        score -= 10 * (diff_m + diff_a);
    }

    // 4. Date Snapshot Matching
    let m_full_text = format!("{m_id_clean} {m_slug} {m_name}");
    let a_full_text = format!("{a_disp} {a_key} {a_url}");
    let (m_d8, m_d4) = extract_dates(&m_full_text);
    let (a_d8, a_d4) = extract_dates(&a_full_text);

    let has_m_date = !m_d8.is_empty() || !m_d4.is_empty();
    let has_a_date = !a_d8.is_empty() || !a_d4.is_empty();

    if has_m_date && has_a_date {
        let mut matched_date = false;
        if !m_d8.is_disjoint(&a_d8) {
            matched_date = true;
        } else {
            let m_all_4: HashSet<String> = m_d4.iter().cloned().chain(m_d8.iter().map(|d| d[4..].to_string())).collect();
            let a_all_4: HashSet<String> = a_d4.iter().cloned().chain(a_d8.iter().map(|d| d[4..].to_string())).collect();

            if !m_all_4.is_disjoint(&a_all_4) {
                matched_date = true;
            } else {
                // Check month/day proximity (e.g. within 5 days for rolling releases)
                for md in &m_all_4 {
                    for ad in &a_all_4 {
                        if let (Ok(m_val), Ok(a_val)) = (md.parse::<i32>(), ad.parse::<i32>()) {
                            if (m_val - a_val).abs() <= 5 {
                                matched_date = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        if matched_date {
            score += 100;
        } else {
            // Explicitly conflicting snapshot dates (different months/snapshots)
            score -= 150;
        }
    } else if !has_m_date && !has_a_date {
        score += 30;
    } else {
        // One is dated, one is undated (e.g. OpenRouter canonical slug has 20260520, but Arena lists base name)
        score += 10;
    }

    // 5. URL match
    if !a_url.is_empty() {
        let trimmed_url = a_url.trim_end_matches('/');
        if let Some(a_url_slug) = trimmed_url.split('/').last() {
            if !a_url_slug.is_empty() && (m_id_clean.contains(a_url_slug) || m_slug.contains(a_url_slug)) {
                score += 50;
            }
        }
    }

    score
}

/// Find the best arena score for an OpenRouter model using dynamic multi-signal heuristic scoring.
pub fn score_for<'a>(model: &'a Model, scores: &'a [Score]) -> Option<&'a Score> {
    const MIN_CONFIDENCE_SCORE: i32 = 80;

    let mut best: Option<(&'a Score, i32)> = None;

    for score_entry in scores {
        let s = calculate_match_score(model, score_entry);
        if s >= MIN_CONFIDENCE_SCORE {
            match best {
                Some((_, top_s)) if s > top_s => {
                    best = Some((score_entry, s));
                }
                // When confidence scores tie, prefer the higher ranked / higher rated entry
                Some((top_entry, top_s)) if s == top_s && score_entry.rating > top_entry.rating => {
                    best = Some((score_entry, s));
                }
                None => {
                    best = Some((score_entry, s));
                }
                _ => {}
            }
        }
    }

    best.map(|(entry, _)| entry)
}
