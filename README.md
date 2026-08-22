# llm-leaders

A CLI that lists coding LLMs with their **OpenRouter prices** and **arena.ai WebDev Elo** score, side by side.

Example — `llm-leaders --all --max-input 1.5 --max-rank 50` (cheapest-input price ≤ $1.50/M, arena rank ≤ 50, across the full OpenRouter catalog):

![llm-leaders --all --max-input 1.5 --max-rank 50](assets/example.png)

## Columns

| Arena # | Model | In $/M | Out $/M | Disc | Elo | ID |
| ---: | --- | ---: | ---: | ---: | ---: | --- |
| arena.ai WebDev rank (#1 best) | OpenRouter model name | input price per million tokens | output price per million tokens | provider discount | arena Elo | OpenRouter model ID (copy-paste to use the model) |

Prices come live from the [OpenRouter model catalog](https://openrouter.ai/api/v1/models), then refined per model with the cheapest provider from the [endpoints API](https://openrouter.ai/api/v1/models) — the same "lowest across providers" price the OpenRouter website shows. Cheapest prices are cached for 24h at `~/.config/llm-leaders/best_prices.json`; the first `--all` run takes ~20s to fetch all providers, subsequent runs are instant. `--refresh` bypasses the cache.
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

# keep only free models / only discounted models
llm-leaders --free
llm-leaders --discounted

# fuzzy search by name or ID (subsequence match, multi-word AND)
llm-leaders --search "glm"
llm-leaders --search "kimi k3"        # both words must match
llm-leaders --all --search "opus"     # search across the whole catalog

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

The curated list is loaded from:
1. `./models.txt` in the current working directory (if present), or
2. `~/.config/llm-leaders/models.txt` (global user config).

If neither file exists, `llm-leaders` automatically falls back to showing all models (`--all`).
Running `llm-leaders add` creates and updates `~/.config/llm-leaders/models.txt` automatically (or `./models.txt` if run inside the project repo). One OpenRouter model ID per line (`#` comments allowed).

## Build

```sh
cargo build --release
# binary at target/release/llm-leaders
```

## Packaging (Arch / AUR)

A tagged push (`git tag v0.1.0 && git push --tags`) triggers [.gitea/workflows/release.yml](.gitea/workflows/release.yml), which runs in an `archlinux:latest` container and:

1. builds the release binary and UPX-compresses it,
2. packs `llm-leaders-<ver>-x86_64-unknown-linux-gnu.tar.gz` (binary + LICENSE),
3. lints the package plan with `namcap`,
4. publishes a GitHub/Gitea Release, and
5. updates the `llm-leaders-bin` AUR package — [packaging/arch/PKGBUILD](packaging/arch/PKGBUILD) is the canonical source; [packaging/arch/publish-aur.sh](packaging/arch/publish-aur.sh) clones AUR, injects the tarball's real `b2sum`, regenerates `.SRCINFO`, and pushes over SSH.

The AUR push authenticates with an SSH key registered to your AUR account, stored as the `AUR_SSH_PRIVATE_KEY` action secret. That key is account-wide (AUR has no per-package deploy keys), so treat it as a credential.

Install from AUR: `yay -S llm-leaders-bin`.
