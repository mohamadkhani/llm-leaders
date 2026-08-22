use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use comfy_table::{ContentArrangement, Table};
use std::path::PathBuf;

mod arena;
mod match_score;
mod models_list;
mod openrouter;

#[derive(Parser)]
#[command(name = "llm-leaders", about = "List coding LLMs: OpenRouter prices + arena WebDev rank")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Output a GitHub-flavored markdown table instead of a styled terminal table.
    #[arg(long, global = true)]
    markdown: bool,

    /// Sort column: rank (default, asc), elo (desc), input (input $/M asc),
    /// output (output $/M asc), name (asc). "price" is an alias for "input".
    #[arg(long, global = true, default_value = "rank")]
    sort: String,

    /// Keep only models whose input price is at most this (USD per million tokens).
    /// Free models (price 0) always pass. Models with no known price are dropped.
    #[arg(long, global = true)]
    max_input: Option<f64>,

    /// Keep only models whose output price is at most this (USD per million tokens).
    /// Free models (price 0) always pass. Models with no known price are dropped.
    #[arg(long, global = true)]
    max_output: Option<f64>,

    /// Keep only models whose arena rank is at most this (1 = best). Models with no
    /// arena score are dropped when this is set.
    #[arg(long, global = true)]
    max_rank: Option<u64>,

    /// Keep only free models (input price 0).
    #[arg(long, global = true)]
    free: bool,

    /// Keep only models with an active provider discount.
    #[arg(long, global = true)]
    discounted: bool,

    /// Fuzzy-filter by model name or ID (e.g. "glm", "kimi-k3", "qwen coder").
    #[arg(long, global = true)]
    search: Option<String>,

    /// Show the full OpenRouter catalog instead of your curated models.txt list.
    #[arg(long, global = true)]
    all: bool,

    /// Force-refresh the arena score cache.
    #[arg(long, global = true)]
    refresh: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Add models interactively from the OpenRouter catalog, or by explicit IDs.
    Add {
        /// OpenRouter model IDs to add directly (e.g. "z-ai/glm-5.2"). If omitted, interactive picker.
        ids: Vec<String>,
    },
    /// Remove models interactively from your list, or by explicit IDs.
    Remove {
        /// OpenRouter model IDs to remove directly. If omitted, interactive picker.
        ids: Vec<String>,
    },
    /// Print the current curated list.
    List,
}

fn list_path() -> PathBuf {
    PathBuf::from("models.txt")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Add { ids }) => cmd_add(&ids)?,
        Some(Command::Remove { ids }) => cmd_remove(&ids)?,
        Some(Command::List) => {
            for id in models_list::read_ids(&list_path())? {
                println!("{id}");
            }
        }
        None => render_table(&TableOpts {
            markdown: cli.markdown,
            sort: cli.sort,
            max_input: cli.max_input,
            max_output: cli.max_output,
            max_rank: cli.max_rank,
            free: cli.free,
            discounted: cli.discounted,
            search: cli.search,
            all: cli.all,
            refresh: cli.refresh,
        })?,
    }
    Ok(())
}

struct TableOpts {
    markdown: bool,
    sort: String,
    max_input: Option<f64>,
    max_output: Option<f64>,
    max_rank: Option<u64>,
    free: bool,
    discounted: bool,
    search: Option<String>,
    all: bool,
    refresh: bool,
}

fn cmd_add(ids: &[String]) -> Result<()> {
    let path = list_path();
    let catalog = openrouter::fetch_models()?;
    let existing: std::collections::HashSet<String> =
        models_list::read_ids(&path)?.into_iter().collect();

    let to_add: Vec<String> = if ids.is_empty() {
        interactive_add(&catalog, &existing)?
    } else {
        validate_ids(ids, &catalog)?
    };

    if to_add.is_empty() {
        println!("Nothing to add.");
        return Ok(());
    }
    let added = models_list::add_ids(&path, &to_add)?;
    println!("Added {} model(s):", added.len());
    for id in &added {
        println!("  + {id}");
    }
    Ok(())
}

fn cmd_remove(ids: &[String]) -> Result<()> {
    let path = list_path();
    let current = models_list::read_ids(&path)?;
    let to_remove: Vec<String> = if ids.is_empty() {
        interactive_remove(&current)?
    } else {
        ids.iter().cloned().collect()
    };
    if to_remove.is_empty() {
        println!("Nothing to remove.");
        return Ok(());
    }
    let removed = models_list::remove_ids(&path, &to_remove)?;
    println!("Removed {} model(s):", removed.len());
    for id in &removed {
        println!("  - {id}");
    }
    Ok(())
}

fn validate_ids(ids: &[String], catalog: &[openrouter::Model]) -> Result<Vec<String>> {
    let mut ok = Vec::new();
    for id in ids {
        if catalog.iter().any(|m| m.id == *id) {
            ok.push(id.clone());
        } else {
            match closest_match(id, catalog) {
                Some(s) => bail!("unknown OpenRouter id {id:?} — did you mean {s:?}?"),
                None => bail!("unknown OpenRouter id {id:?}"),
            }
        }
    }
    Ok(ok)
}

fn closest_match(id: &str, catalog: &[openrouter::Model]) -> Option<String> {
    let target: Vec<&str> = id
        .rsplit('/')
        .next()
        .unwrap_or(id)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    catalog
        .iter()
        .map(|m| {
            let parts: Vec<&str> = m
                .id
                .rsplit('/')
                .next()
                .unwrap_or(&m.id)
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .collect();
            let shared = target.iter().filter(|t| parts.contains(t)).count();
            (shared, m.id.clone())
        })
        .max_by_key(|(s, _)| *s)
        .filter(|(s, _)| *s > 0)
        .map(|(_, id)| id)
}

fn interactive_add(
    catalog: &[openrouter::Model],
    existing: &std::collections::HashSet<String>,
) -> Result<Vec<String>> {
    use inquire::MultiSelect;

    let mut opts: Vec<&openrouter::Model> =
        catalog.iter().filter(|m| !existing.contains(&m.id)).collect();
    opts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let display: Vec<String> = opts
        .iter()
        .map(|m| {
            format!(
                "{}  [in {}  out {}]",
                m.name,
                fmt_price(m.input_per_m()),
                fmt_price(m.output_per_m())
            )
        })
        .collect();
    let ids: Vec<String> = opts.iter().map(|m| m.id.clone()).collect();

    let selected: Vec<String> = MultiSelect::new(
        "Select models to add (type to search, Enter to confirm):",
        display.clone(),
    )
    .with_page_size(15)
    .prompt()?
    .into_iter()
    .map(|d| {
        let idx = display.iter().position(|x| *x == d).unwrap();
        ids[idx].clone()
    })
    .collect();
    Ok(selected)
}

fn interactive_remove(current: &[String]) -> Result<Vec<String>> {
    use inquire::MultiSelect;
    if current.is_empty() {
        println!("Your list is empty.");
        return Ok(vec![]);
    }
    let ans = MultiSelect::new("Select models to remove:", current.to_vec())
        .with_page_size(15)
        .prompt()?;
    Ok(ans)
}

#[derive(Clone)]
struct Row {
    id: String,
    name: String,
    input: Option<f64>,
    output: Option<f64>,
    discount: Option<f64>,
    rank: Option<u64>,
    elo: Option<f64>,
}

fn render_table(opts: &TableOpts) -> Result<()> {
    let ids: Option<Vec<String>> = if opts.all {
        None
    } else {
        let ids = models_list::read_ids(&list_path())?;
        if ids.is_empty() {
            println!("models.txt is empty. Add models with `llm-leaders add`.");
            return Ok(());
        }
        Some(ids)
    };

    let catalog = openrouter::fetch_models()?;
    let scores = arena::get_scores(opts.refresh)?;

    // Cheapest-provider prices (matches what the OpenRouter site shows),
    // cached 24h on disk. In --all mode this covers the whole catalog; the
    // first uncached --all run takes ~20s to fetch ~400 endpoints.
    let id_list: Vec<String> = match &ids {
        Some(list) => list.clone(),
        None => catalog.iter().map(|m| m.id.clone()).collect(),
    };
    let best_prices = openrouter::fetch_best_prices(&id_list, opts.refresh)?;

    let mut rows: Vec<Row> = Vec::new();
    let build_row = |model: &openrouter::Model| {
        let arena = match_score::score_for(model, &scores).map(|s| (s.rank, s.rating));
        let (rank, elo) = arena
            .map(|(rk, e)| (Some(rk), Some(e)))
            .unwrap_or((None, None));
        // Prefer cheapest-provider pricing when we have it; fall back to the
        // catalog's default-endpoint price.
        let best = best_prices.get(&model.id);
        Row {
            id: model.id.clone(),
            name: model.name.clone(),
            input: best.and_then(|b| b.input).or_else(|| model.input_per_m()),
            output: best.and_then(|b| b.output).or_else(|| model.output_per_m()),
            discount: best.and_then(|b| b.discount),
            rank,
            elo,
        }
    };

    match &ids {
        // Curated list: catalog models where possible; for ids missing from
        // the catalog, synthesize a minimal Model so coding models that exist
        // in arena but not in OpenRouter still get a rank/elo.
        Some(ids) => {
            let by_id: std::collections::HashMap<String, &openrouter::Model> =
                catalog.iter().map(|m| (m.id.clone(), m)).collect();
            for id in ids {
                match by_id.get(id) {
                    Some(m) => rows.push(build_row(m)),
                    None => {
                        let synth = openrouter::Model {
                            id: id.clone(),
                            name: id.clone(),
                            canonical_slug: None,
                            description: None,
                            context_length: None,
                            pricing: openrouter::Pricing {
                                prompt: "0".to_string(),
                                completion: "0".to_string(),
                                discount: None,
                            },
                        };
                        rows.push(build_row(&synth));
                    }
                }
            }
        }
        // Full catalog: one row per OpenRouter model, stable catalog order.
        None => {
            for m in &catalog {
                rows.push(build_row(m));
            }
        }
    }

    // ---- Filters ---------------------------------------------------------
    let before = rows.len();
    if let Some(max_in) = opts.max_input {
        rows.retain(|r| r.input.map_or(false, |p| p <= max_in));
    }
    if let Some(max_out) = opts.max_output {
        rows.retain(|r| r.output.map_or(false, |p| p <= max_out));
    }
    if let Some(max_rk) = opts.max_rank {
        rows.retain(|r| r.rank.map_or(false, |rk| rk <= max_rk));
    }
    if opts.free {
        rows.retain(|r| r.input.map_or(false, |p| p <= 0.0));
    }
    if opts.discounted {
        rows.retain(|r| r.discount.map_or(false, |d| d > 0.0));
    }
    if let Some(q) = &opts.search {
        let q = q.to_lowercase();
        // Split into words; each word must fuzzy-match (subsequence) the
        // name or the id.
        let words: Vec<String> = q.split_whitespace().map(|w| w.to_string()).collect();
        rows.retain(|r| {
            let hay = format!("{} {}", r.name, r.id).to_lowercase();
            words.iter().all(|w| fuzzy_contains(&hay, w))
        });
    }
    let dropped = before - rows.len();

    // ---- Sort ------------------------------------------------------------
    // rank: asc, missing last. elo: desc, missing last.
    // price: input asc, missing last. output: output asc, missing last.
    // name: asc.
    match opts.sort.as_str() {
        "rank" => rows.sort_by(|a, b| {
            a.rank
                .unwrap_or(u64::MAX)
                .cmp(&b.rank.unwrap_or(u64::MAX))
        }),
        "elo" => rows.sort_by(|a, b| {
            b.elo
                .unwrap_or(f64::MIN)
                .partial_cmp(&a.elo.unwrap_or(f64::MIN))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "input" | "price" => rows.sort_by(|a, b| {
            // Input price first, output price as tiebreaker.
            let tie = a
                .input
                .unwrap_or(f64::INFINITY)
                .partial_cmp(&b.input.unwrap_or(f64::INFINITY))
                .unwrap_or(std::cmp::Ordering::Equal);
            tie.then_with(|| {
                a.output
                    .unwrap_or(f64::INFINITY)
                    .partial_cmp(&b.output.unwrap_or(f64::INFINITY))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        }),
        "output" => rows.sort_by(|a, b| {
            a.output
                .unwrap_or(f64::INFINITY)
                .partial_cmp(&b.output.unwrap_or(f64::INFINITY))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        "name" => rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        other => bail!("invalid --sort {other:?} (use rank|elo|input|output|name)"),
    }

    if opts.markdown {
        print_markdown(&rows);
    } else {
        print_table(&rows);
    }
    if dropped > 0 {
        eprintln!("({dropped} model(s) hidden by filters)");
    }
    Ok(())
}

fn fmt_price(v: Option<f64>) -> String {
    match v {
        Some(p) if p > 0.0 => format!("${:.2}", p),
        Some(_) => "free".to_string(),
        None => "—".to_string(),
    }
}

fn fmt_rank(r: Option<u64>) -> String {
    match r {
        Some(n) => format!("#{n}"),
        None => "—".to_string(),
    }
}

fn fmt_elo(r: Option<f64>) -> String {
    match r {
        Some(e) => format!("{:.0}", e),
        None => "—".to_string(),
    }
}

fn fmt_discount(d: Option<f64>) -> String {
    match d {
        Some(f) if f > 0.0 => format!("-{}%", (f * 100.0).round() as u64),
        _ => "—".to_string(),
    }
}

/// Case-insensitive subsequence match: every char of `needle` appears in
/// `hay` in order (skipping non-matching chars). "g52" matches "glm 5.2".
fn fuzzy_contains(hay: &str, needle: &str) -> bool {
    let mut it = hay.chars();
    'outer: for n in needle.chars() {
        if n.is_whitespace() {
            continue;
        }
        for h in it.by_ref() {
            if h == n {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Map t in [0,1] onto the green -> yellow -> red ramp (truecolor).
/// t = 0 green, t = 1 red.
fn heat_rgb(t: f64) -> comfy_table::Color {
    let t = t.clamp(0.0, 1.0);
    let (r, g, b) = if t <= 0.5 {
        let k = (t / 0.5) * 255.0;
        (k.round() as u8, 255, 0)
    } else {
        let k = ((t - 0.5) / 0.5) * 255.0;
        (255, (255.0 - k).round() as u8, 0)
    };
    comfy_table::Color::Rgb { r, g, b }
}

/// Price-heat color for a price cell, scaled to the min/max of the rows
/// actually displayed: cheapest = green, mid = yellow, priciest = red.
/// Returns None when the scale is degenerate (single row / all prices equal)
/// or the price is unknown, leaving the cell uncolored.
fn price_heat(price: Option<f64>, min: f64, max: f64) -> Option<comfy_table::Color> {
    let p = price?;
    if max <= min {
        return None; // no spread: color would be arbitrary
    }
    // 0 = cheapest, 1 = priciest
    let t = ((p - min) / (max - min)).clamp(0.0, 1.0);
    Some(heat_rgb(t))
}

/// Rank heat: best (lowest) rank in view = green, worst = red. Same
/// degenerate rules as the price scale.
fn rank_heat(rank: Option<u64>, min: u64, max: u64) -> comfy_table::Color {
    let t = match rank {
        Some(r) if max > min => ((r - min) as f64 / (max - min) as f64).clamp(0.0, 1.0),
        // No spread (single ranked row, or unranked): leave plain.
        _ => return comfy_table::Color::Reset,
    };
    heat_rgb(t)
}

/// Elo heat: highest Elo in view = green, lowest = red. Same degenerate
/// rules as the other scales.
fn elo_heat(elo: Option<f64>, min: f64, max: f64) -> comfy_table::Color {
    let e = match elo {
        Some(e) if max > min => e,
        _ => return comfy_table::Color::Reset,
    };
    // Higher Elo = greener; invert t so 0 -> green.
    let t = 1.0 - ((e - min) / (max - min)).clamp(0.0, 1.0);
    heat_rgb(t)
}

/// Value-for-money heat for the model name: how much arena quality a model
/// delivers per dollar, relative to the rows actually displayed. Best value
/// in view = green, worst = red. Free models (price 0) get an even deeper
/// green than the best paid value — free quality is unbeatable per dollar.
///
/// Value = odds(elo) / blended price, where odds(elo) = 10^(elo/400) is the
/// Elo win-probability multiplier (each +400 Elo = 10x the odds) — this
/// weights quality strongly enough that a big Elo gap isn't erased by a cheap
/// price. Blended price weights input 3 : output 1 (typical coding-agent
/// traffic mix). Models missing Elo or prices are left uncolored.
fn value_heat(
    elo: Option<f64>,
    input: Option<f64>,
    output: Option<f64>,
    min_val: f64, // min of ln(value) over paid+ranked rows in view
    max_val: f64, // max of ln(value) over paid+ranked rows in view
) -> (comfy_table::Color, bool) {
    let (elo, inp, out) = match (elo, input, output) {
        (Some(e), Some(i), Some(o)) => (e, i, o),
        _ => return (comfy_table::Color::Reset, false),
    };
    let blended = inp * 3.0 + out;
    if blended <= 0.0 {
        // Free model with a known Elo: bold pure green — one step stronger
        // than the plain pure green the best paid value gets.
        return if elo > 0.0 {
            (comfy_table::Color::Rgb { r: 0, g: 255, b: 0 }, true)
        } else {
            (comfy_table::Color::Reset, false)
        };
    }
    let value = (10f64.powf(elo / 400.0) / blended).ln();
    if max_val <= min_val {
        return (comfy_table::Color::Reset, false);
    }
    // Higher value = greener; invert t so 0 -> green.
    let t = 1.0 - ((value - min_val) / (max_val - min_val)).clamp(0.0, 1.0);
    (heat_rgb(t), false)
}

fn styled_cell(text: String, color: comfy_table::Color, bold: bool) -> comfy_table::Cell {
    use comfy_table::{Attribute, Cell};
    let mut cell = Cell::new(text).fg(color);
    if bold {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell
}

fn print_table(rows: &[Row]) {
    use comfy_table::presets::UTF8_FULL_CONDENSED;
    let mut t = Table::new();
    t.load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Disabled)
        // Emit ANSI styles even when stdout isn't a TTY (e.g. piped to less -R).
        .enforce_styling();
    t.set_header(vec![
        styled_cell("Arena #".to_string(), comfy_table::Color::Reset, true),
        styled_cell("Model".to_string(), comfy_table::Color::Reset, true),
        styled_cell("In $/M".to_string(), comfy_table::Color::Reset, true),
        styled_cell("Out $/M".to_string(), comfy_table::Color::Reset, true),
        styled_cell("Disc".to_string(), comfy_table::Color::Reset, true),
        styled_cell("Elo".to_string(), comfy_table::Color::Reset, true),
        styled_cell("ID".to_string(), comfy_table::Color::Reset, true),
    ]);
    // Price-heat scale over the visible rows (after filtering).
    let inputs: Vec<f64> = rows.iter().filter_map(|r| r.input).collect();
    let outputs: Vec<f64> = rows.iter().filter_map(|r| r.output).collect();
    let (in_min, in_max) = (
        inputs.iter().cloned().fold(f64::INFINITY, f64::min),
        inputs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    let (out_min, out_max) = (
        outputs.iter().cloned().fold(f64::INFINITY, f64::min),
        outputs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    // Rank-heat scale over the visible ranks.
    let ranks: Vec<u64> = rows.iter().filter_map(|r| r.rank).collect();
    let (rk_min, rk_max) = (
        ranks.iter().cloned().min().unwrap_or(0),
        ranks.iter().cloned().max().unwrap_or(0),
    );
    // Elo-heat scale over the visible elos.
    let elos: Vec<f64> = rows.iter().filter_map(|r| r.elo).collect();
    let (elo_min, elo_max) = (
        elos.iter().cloned().fold(f64::INFINITY, f64::min),
        elos.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    // Value-for-money scale over the visible rows. Blended price weights
    // input 3 : output 1 (typical coding-agent traffic); value = elo-odds /
    // price where odds = 10^(elo/400). Free models are excluded.
    let blended = |r: &Row| r.input.map(|i| i * 3.0 + r.output.unwrap_or(0.0));
    let values: Vec<f64> = rows
        .iter()
        .filter_map(|r| {
            let p = blended(r)?;
            if p <= 0.0 {
                return None; // free: excluded from the value scale
            }
            let elo = r.elo?;
            // Log-scale: value ratios span orders of magnitude; log keeps the
            // gradient readable.
            Some((10f64.powf(elo / 400.0) / p).ln())
        })
        .collect();
    let (val_min, val_max) = (
        values.iter().cloned().fold(f64::INFINITY, f64::min),
        values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    );
    for r in rows {
        let (val_color, val_bold) = value_heat(r.elo, r.input, r.output, val_min, val_max);
        t.add_row(vec![
            styled_cell(fmt_rank(r.rank), rank_heat(r.rank, rk_min, rk_max), false),
            styled_cell(r.name.clone(), val_color, val_bold),
            styled_cell(
                fmt_price(r.input),
                price_heat(r.input, in_min, in_max).unwrap_or(comfy_table::Color::Reset),
                false,
            ),
            styled_cell(
                fmt_price(r.output),
                price_heat(r.output, out_min, out_max).unwrap_or(comfy_table::Color::Reset),
                false,
            ),
            styled_cell(
                fmt_discount(r.discount),
                if r.discount.map_or(false, |d| d > 0.0) {
                    comfy_table::Color::Green
                } else {
                    comfy_table::Color::Reset
                },
                r.discount.map_or(false, |d| d >= 0.30),
            ),
            styled_cell(fmt_elo(r.elo), elo_heat(r.elo, elo_min, elo_max), false),
            styled_cell(r.id.clone(), comfy_table::Color::DarkGrey, false),
        ]);
    }
    // Color the table frame. comfy-table resets styling after each cell
    // (\x1b[0m), which would also wipe a surrounding frame color, and its
    // border glyphs are emitted with no color of their own. So we render the
    // table, then post-process: wrap every box-drawing border character in a
    // dim-grey foreground code. Cell text colors are left untouched.
    let rendered = format!("{t}");
    let frame_color = "\x1b[38;5;240m"; // dim grey (256-color)
    let reset = "\x1b[0m";
    let colored = color_borders(&rendered, frame_color, reset);
    println!("{colored}");
}

/// Wrap each box-drawing border character in `color`...`reset`, leaving cell
/// content (and its own ANSI codes) intact.
fn color_borders(table: &str, color: &str, reset: &str) -> String {
    let borders: &[char] = &[
        '─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼',
        '═', '║', '╞', '╡', '╪', '╪', '╪', '╕', '╛', '╘', '╒',
        '╓', '╖', '╙', '╜', '╟', '╢', '╤', '╧', '╨', '╩', '╦', '╠', '╣', '╬',
        '╌', '╍', '┄', '┅', '┆', '┇', '┈', '┉', '┊', '┋',
        '╭', '╮', '╰', '╯',
    ];
    let mut out = String::with_capacity(table.len() * 2);
    for ch in table.chars() {
        if borders.contains(&ch) {
            out.push_str(color);
            out.push(ch);
            out.push_str(reset);
        } else {
            out.push(ch);
        }
    }
    out
}

fn print_markdown(rows: &[Row]) {
    println!("| Arena # | Model | In $/M | Out $/M | Disc | Elo | ID |");
    println!("|---:|---|---:|---:|---:|---:|---|");
    for r in rows {
        println!(
            "| {} | {} | {} | {} | {} | {} | `{}` |",
            fmt_rank(r.rank),
            r.name,
            fmt_price(r.input),
            fmt_price(r.output),
            fmt_discount(r.discount),
            fmt_elo(r.elo),
            r.id
        );
    }
}
