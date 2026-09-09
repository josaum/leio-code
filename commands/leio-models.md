---
description: Refresh or inspect the live OpenRouter work-shape model table (volume / verifier / quality picks)
argument-hint: "[refresh] [--base-url url] | [show]"
---

# LEIO models — live OpenRouter work-shape routing

Arguments: $ARGUMENTS (default: `show`).

1. `refresh` — fetch OpenRouter's public `/models` list (no API key; filters
   to text-only output, ≥ 100k context, skipping `:free` / `preview` /
   zero-priced variants) and rank volume/quality (price) and verifier (nearest
   median) into `~/.config/leio-harness/openrouter-models.json`, valid one
   week. Run `leio-harness models refresh` and report the new picks.
2. `show` — run `leio-harness models show` and report the resolved slugs plus
   their source (`openrouter-api` = live, `builtin` = 2026-08-17 snapshot).

Always state the source and age of the table. If resolution fell back to
`builtin`, say so plainly and offer the refresh.
