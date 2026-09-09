# Context bundles (`context` command)

The **`context`** command (and MCP tool `leio_code_context`) builds a **ranked, bounded working set** for a natural-language task: files, symbols, env vars, Redis keys, deploy targets when modeled, plus instruction/memory zones, verification anchors, suggested tests, graph follow-ups, and doctor hints.

This document describes **how files are ranked** and **what agents should read first**.

## CLI

```bash
cd /path/to/leio-code
cargo run -- context "your task description" --repo /path/to/target-repo --limit 8
```

`--limit` caps how many top files/symbols appear in the bundle (clamped internally). Global `--repo` points at the workspace root (same as other subcommands).

## Read order (agents)

1. **`retrieval_signals`** — whether sparse IDF, intent routes, and optional graph boosts applied; **`reading_order`** explains truncation strategy (top of `files_to_read` is strongest; mitigates “lost in the middle” when the consumer clips context).
2. **`context_zones`** — instructions → memory → anchors → ranked files → graph follow-ups → verification → risks.
3. **`files_to_read`** — each row includes **`score`**, **`reasons`**, **`modified_unix_ms`**, and structured symbol/env/redis snippets.

## Ranking pipeline (files)

Roughly:

| Stage | What it does |
|-------|----------------|
| **Token & IDF** | Task text is tokenized (camelCase split, stopwords dropped). Per-token **smoothed IDF** weights matches so rare terms (few paths/symbols hit) outweigh ubiquitous fragments like shared directory prefixes. |
| **Layers** | Each file gets three additive layers: **path** (path match + segment boost + intent routes + path-side identifier hits), **symbol** (matched definitions + symbol-side identifier hits), **config** (env var + Redis key matches). |
| **Intent routes** | If the task mentions tests/specs/CI, deploy/Docker/workflows, frontend/UI, or auth/secrets, paths matching those **patterns** get an extra path-layer boost (e.g. `tests/`, `.github/workflows`, `.tsx`). |
| **Identifier needles** | **PascalCase** and **`SCREAMING_SNAKE`** fragments are extracted from the **raw** task string and matched case-sensitively against paths, symbol names, and env names. |
| **Path segments** | Task tokens (length ≥ 3) that equal a directory name or file stem (one extension stripped) get a segment bonus. |
| **RRF** | **Reciprocal Rank Fusion** merges three ranked views (path layer, symbol layer, config layer) with **k ≈ 60** and a fixed scale on top of the lexical sum. **Tied layer scores share the same rank** so identical files tie on RRF and **mtime** breaks ties (newer first). |
| **Graph proximity (optional)** | If `.leio-code/exports/code-graph-v1/query-cache.json` exists and matches the cache schema, top seeds after RRF get **import/importer** neighbors and **caller/callee** neighbors from symbols in those files. Bonuses are modest and **skipped for seeds themselves**. Context **does not** rebuild the graph; it only reads the cache (fail-open if missing). |

Symbols, env vars, Redis keys, and deploy targets use the **same IDF-weighted lexical scorer** as file rows where applicable.

## Key JSON fields

| Field | Meaning |
|-------|---------|
| **`selection_policy`** | Strategy name, reranker id string, limits, **`intent_routes_considered`**. |
| **`retrieval_signals`** | **`sparse_idf_weighting`**, **`intent_routes_considered`**, **`identifier_needles_extracted`**, **`graph_cache_loaded`**, optional **`code_graph_refresh_hint`** when no cache, **`signal_stack`** one-line summary of signals. |
| **`files_to_read`** | Ordered strongest-first; use **`modified_unix_ms`** as a secondary freshness cue when scores tie in display-only tools. |

## Enabling graph proximity boosts

Graph boosts are **optional** and **read-only** at context time:

1. **`cargo run -- index --repo <root>`** (or your usual index refresh).
2. **`cargo run -- export code-graph --repo <root>`** — writes `graph.nq`, `manifest.json`, and **`query-cache.json`** under `.leio-code/exports/code-graph-v1/`.

Until that exists, bundles still work; **`retrieval_signals.code_graph_refresh_hint`** tells operators to export once. Full **`graph`** CLI queries may still refresh artifacts when invoked; **`context`** alone does not.

## Related

- Code-graph export layout and query cache: [technical guide § What stays on disk](CLI-GUIDE.md#what-stays-on-disk).
- Agent-oriented overview: [skills/leio-code/SKILL.md](../skills/leio-code/SKILL.md).
