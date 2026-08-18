# llm-leaders

A CLI that lists coding LLMs with their **OpenRouter prices** and **arena.ai WebDev Elo** score, side by side.

## Columns

| Arena # | Model | In $/M | Out $/M | Elo | ID |
| ---: | --- | ---: | ---: | ---: | --- |
| arena.ai WebDev rank (#1 best) | OpenRouter model name | input price per million tokens | output price per million tokens | arena Elo | OpenRouter model ID (copy-paste to use the model) |

Prices come live from the [OpenRouter model catalog](https://openrouter.ai/api/v1/models).
Ranks come from the [arena.ai WebDev leaderboard](https://arena.ai/leaderboard/code/webdev), scraped from the page's embedded payload and cached for 24h at `~/.config/llm-leaders/arena.json`.

The terminal table uses heat scales, all computed over the rows actually displayed so they stay meaningful under any filter combination:

- **Model** — value-for-money heat: Elo odds (`10^(Elo/400)` — each +400 Elo counts as 10× quality) per dollar of blended price (input weighted 3 : output 1, log-scaled). Green = best quality-per-dollar in view, red = worst. Free models with a known Elo render **bold pure green** — unbeatable per dollar.
- **Arena # / Elo** — green = best rank / highest Elo in view, scaling through yellow to red = worst.
- **In $/M / Out $/M** — green = cheapest in view, scaling through yellow to red = priciest.

Columns with no spread (e.g. a single-row result) are left uncolored.

## Usage

```sh
# render the table (sorted by arena rank, asc — best on top)
llm-leaders

# markdown table for pasting into docs/PRs
llm-leaders --markdown

# sort by arena elo (desc), input price (asc), output price (asc), or name (asc)
llm-leaders --sort elo
llm-leaders --sort input
llm-leaders --sort output
llm-leaders --sort name

# keep only models cheaper than $1/M input (free models always pass;
# models with no known price are dropped)
llm-leaders --max-input 1

# keep only models with output price at most $1/M
llm-leaders --max-output 1

# keep only models ranked in the arena top 20 (models with no arena score
# are dropped when this filter is set)
llm-leaders --max-rank 20

# browse the entire OpenRouter catalog (~400 models) instead of your
# curated list — combines with the filters above
llm-leaders --all --max-rank 10 --max-input 1

# combine filters
llm-leaders --max-input 1 --max-rank 20

# force-refresh the arena cache
llm-leaders --refresh

# manage the curated model list
llm-leaders add                    # interactive picker over the OpenRouter catalog
llm-leaders add z-ai/glm-5.2       # add one or more IDs directly (validated against the catalog)
llm-leaders remove                 # interactive multi-select over your current list
llm-leaders remove z-ai/glm-5.2    # remove by ID
llm-leaders list                   # print the current list
```

## The curated list

[models.txt](models.txt) holds the OpenRouter IDs to display, one per line (`#` comments allowed). Seed it with the strongest coding LLMs and edit freely — the main command renders exactly those rows.

## Build

```sh
cargo build --release
# binary at target/release/llm-leaders
```
