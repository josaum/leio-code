# Measured, with the receipts

LEIO adds ranked context, structural queries and a navigation session to your
agent's workflow. These local measurements show what that costs and how well
retrieval performs on the checked-in development tasks.

Measured **9 September 2026**, Apple M1, macOS 26.5.2, release binary
`2.6.2 (377ee12faf61, clean)`, against source revision
[`377ee12`](https://github.com/josaum/leio-code/tree/377ee12faf616acb9f84688de867097579c74df2).
[Machine-readable results](../benchmarks/public-results.json).

## Multi-step investigations through stdio MCP

The primary product benchmark exercises three guided investigations in the real
public repository: **420 indexed files**, source `377ee12faf61`, Node.js 24.13.0.
It runs actual MCP calls against the release CLI, with default provenance events
enabled. The index and graph are prepared before timing starts.

![Full navigation sequence medians](../assets/benchmark-navigation.svg)

| Scenario | Full-sequence median | Scripted checks passed |
| --- | ---: | ---: |
| Trace context ranking | 1,494.2 ms | 5 / 5 |
| Follow workflow policy dispatch | 1,463.4 ms | 5 / 5 |
| Investigate MCP binary selection | 1,416.0 ms | 5 / 5 |

Each sequence uses **11 tool calls**:

1. `context` returns at most five files and includes the labeled implementation.
2. `graph symbols-in` supplies the caller's exact stable identity.
3. `nav goto` sets the cursor, then `here` records it.
4. `nav callees` lists results; another `here` checks that listing did not move it.
5. `nav select` chooses the labeled callee; `here` checks the selected symbol.
6. `nav back` returns; `here` checks the restored cursor.
7. Close and reconnect the MCP provider; `here` verifies persisted position.

The measured interval includes all calls, assertions, and one provider
close/reconnect. Initial index creation, graph warm-up and initial connection
are excluded. All 15 runs passed these assertions. Times vary with machine load: the
slowest single run was **1,714.4 ms**, in the binary-selection scenario, whose
median is 1,416.0 ms.

These are **guided development scenarios**, chosen to exercise a known path.
The script receives the expected file and edge; it does not independently
reason about a bug. Only the selected relationship is checked. Other candidates,
call-edge completeness and architectural correctness are not validated. There
is no comparison arm, token-saving estimate or end-to-end productivity claim.

[Every timed run and assertion result](../benchmarks/navigation-results.json) ·
[Runnable benchmark](../scripts/benchmark_navigation.mjs)

### Reproduce the guided benchmark

Run the current benchmark script against a separate checkout of the measured
source. Build that checkout's binary so source identity is explicit:

```bash
git worktree add --detach /tmp/leio-benchmark-source 377ee12faf616acb9f84688de867097579c74df2
(cd /tmp/leio-benchmark-source && cargo build --locked --release -p leio-code)
npm ci --prefix mcp
BIN=/tmp/leio-benchmark-source/target/release/leio-code
"$BIN" --repo /tmp/leio-benchmark-source index
"$BIN" --repo /tmp/leio-benchmark-source export code-graph
LEIO_CODE_BIN="$BIN" \
  node scripts/benchmark_navigation.mjs --repo /tmp/leio-benchmark-source \
  --repeats 5 --output /tmp/navigation-results.json
```

The script rebuilds the target index and creates named navigation sessions there.
It makes no LLM calls. Use a disposable checkout, and keep the JSON output for
comparison. A failing assertion stops the run instead of reporting success.

## Additional query and retrieval measurements

The following small-query benchmarks are separate from the multi-step study.

![Local CLI latency comparison with different output types](../assets/benchmark-latency.svg)

## Query latency

Median elapsed wall time, **10 CLI subprocess invocations per task**, existing
index. Includes process startup and output production; it is not MCP transport
latency. These are warm local runs, not cold-index measurements.

| Task | LEIO | ripgrep textual lookup |
| --- | ---: | ---: |
| Find `query_dead_code` | 90.0 ms | 10.3 ms |
| Find `export_code_graph` | 92.2 ms | 10.4 ms |
| Context for RDF namespace configuration | 151.4 ms | 12.5 ms |

All invocations returned exit code 0. ripgrep is faster for these literal searches.
The outputs are different: textual matches versus LEIO's indexed symbol/context
results. This comparison does not establish equal relevance, an agent speedup,
token savings, or time to a correct answer. Machine load and cache state affect
results. The existing script's ratio is `rg_ms / leio_ms`, not a product speedup.

## Retrieval quality

Two measurements, and they disagree by a factor of three. Both are published
because the second one is the honest figure and the first is easy to over-read.

### Held-out corpus: 219 tasks, 11,573 files

A private polyglot monorepo (Python, TypeScript, Rust; name withheld) indexed at
a base revision, with tasks drawn from the 619 commits that landed **after** it,
so the change being sought is not yet in the tree. The task text is the commit
subject. The labels are the source files that commit changed and that already
existed at the base. Merge commits, commits touching more than four source
files, and commits whose files did not yet exist were dropped, leaving 219
tasks with a mean of 1.6 labels each. No model calls; labels never came from
LEIO output. Each task asks `context` for five files.

"Prefixed" keeps the conventional-commit `type(scope):` prefix a developer
would also have. "Stripped" removes it, because the scope often names a
directory and path matching hits it for free.

| Ranker state | Phrasing | Mean reciprocal rank | Top 1 | Top 3 | Top 5 |
| --- | --- | ---: | ---: | ---: | ---: |
| Shipped (graph proximity as tie-breaker) | prefixed | 0.157 | 9.1% | 21.0% | 28.3% |
| Shipped | stripped | 0.083 | 3.7% | 12.3% | 14.6% |
| Code graph removed entirely | prefixed | 0.158 | 9.1% | 21.5% | 28.3% |
| Code graph removed entirely | stripped | 0.083 | 3.7% | 12.3% | 14.6% |
| Previous additive graph bonus | prefixed | 0.157 | 9.1% | 21.0% | 28.3% |
| Previous additive graph bonus | stripped | 0.083 | 3.7% | 12.3% | 15.1% |
| Shipped + local Arrow node store, no encoder | prefixed | 0.159 | 9.1% | 22.4% | 27.9% |

Read across the rows: the code-graph channel contributes nothing measurable at
this scale, in either direction. Read down the phrasing: roughly half of the
signal is the scope token matching a directory name.

Looking past the five-file cut on the shipped ranker (prefixed): the labeled
file is in the top 10 for 31.1% of tasks, the top 20 for 41.1%, the top 40 for
48.4%, and **never appears in the top 40 for 51.6%**. Widening the bundle
recovers some tasks and then stalls. This is a retrieval gap, not a
re-ranking gap.

Not measured: the BGE-M3 cosine channel over `semantic_vec`, because no
encoder endpoint (`LEIO_CODE_EMBED_URL`) was configured. The Arrow row above is
that store's lexical and FCA-relation half only.

Aggregate receipt without task text or paths:
[`heldout-retrieval.json`](../benchmarks/heldout-retrieval.json). The task
file itself is not published; it contains the corpus's paths and commit
subjects.

Limits. Files a commit changed approximate the files a developer must read;
they are not the same set. Commit subjects are written with hindsight and
sometimes name the solution, which makes this easier than a live bug report.
Creation-only tasks are excluded, biasing toward modifications. One corpus, so
no cross-repository claim.

### In-repository development suite: 7 tasks, 420 files

Seven labeled tasks in this repository, each asking for at most five context
files. Labels are development annotations, not exhaustive relevance judgments.
This suite is small, used during development, and not held out. It is kept for
reproducibility and regression checks; **it is not evidence of retrieval
quality**, and the held-out figure above is the one to quote.

| Measure | Result |
| --- | ---: |
| Expected file in first position | 2 / 7 |
| Expected file in top three | 4 / 7 |
| Mean reciprocal rank | 0.493 |

Every task and returned path is included in the JSON report, including misses.
This does not measure architecture correctness, call-edge completeness, code
quality, or application runtime behavior. No before/after or competitor
quality claim is made.

## Reproduce

From a clean checkout of the measured revision, build the release binary and
create its index. Use an absolute binary path; `--version` should identify the
revision you intend to measure. Run the two scripts sequentially:

```bash
cargo build --locked --release -p leio-code
./target/release/leio-code --repo . index
python3 scripts/benchmark_retrieval.py --binary "$PWD/target/release/leio-code" --repeats 10
python3 scripts/evaluate_retrieval.py --binary "$PWD/target/release/leio-code" \
  --repo . --tasks benchmarks/retrieval-agent-tasks.json --output /tmp/retrieval.json
```

Timing tasks: [benchmark_retrieval.py](../scripts/benchmark_retrieval.py).
Relevance labels: [retrieval-agent-tasks.json](../benchmarks/retrieval-agent-tasks.json).
The scripts make no LLM calls. Results will vary across revisions and machines.
