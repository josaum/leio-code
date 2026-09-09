# Conversation evidence and guidance

The `conversation` command prepares a local review of selected chat files, with
source hashes, message excerpts and temporal context. It helps you trace a
statement back to its source and distinguish prior context from later replies.
Semantic interpretation and the proposed statistical methods remain separate
steps; preparing a packet does not execute them.

## Run

Start with the readable terminal view:

```sh
leio-code --repo /absolute/private-folder conversation \
  --source 'Chat A.zip' --source 'Chat B.zip' --date-order dmy --account 'Alex'
```

The view shows why the target was selected, the context limit and parser warnings.
Each message includes its evidence ID, source filename and line range or JSON
pointer; ZIP records also name the transcript member. Duplicate transcripts point
to the first selected container, with every container retained in the source list.
Truncated excerpts are marked. Terminal and bidirectional formatting controls
are escaped for display; original text remains intact in the evidence packet.
Add `--json` to obtain the complete bounded evidence packet for an MCP client,
Reference adapter or analysis script. Both views use the same selection limits.

Stdio MCP: `leio_code_conversation` takes `repo_root`, `sources`, `date_order`,
optional `account`, optional `target`, and `limit` (1–20, default 8). Inspect the
inventory first, then select a canonical message ID from `evidence_records`, or
an unambiguous imported JSON `id`. Omitting a target selects the latest matching
message; selection is not an anomaly score. The guide is available on both
transports as topic `conversation`. Hosted Apps SDK does not expose local file
access. Existing installed binaries/plugin caches need a normal release refresh
before advertising this new source capability.

No recursive directory scan, index, network request, model inference, ZIP
extraction, or LEIO event journal is performed by this command. A host can still
persist tool responses; choose a host appropriate for the source data. Sources
are confined to the explicitly supplied folder after symlink resolution.
Only synthetic content belongs in repository tests.

### Try a small example

Save this synthetic conversation as `messages.json` in your chosen folder:

```json
[
  {"id":"earlier","timestamp":"2026-09-01T09:00:00","sender":"Alex","text":"We usually meet in the morning."},
  {"id":"question","timestamp":"2026-09-02T10:00:00","sender":"Sam","text":"What time works tomorrow?"},
  {"id":"reply","timestamp":"2026-09-02T10:01:00","sender":"Alex","text":"Nine works for me."},
  {"id":"later","timestamp":"2026-09-02T10:02:00","sender":"Sam","text":"Confirmed."}
]
```

Select the reply explicitly:

```sh
leio-code --repo /absolute/private-folder conversation \
  --source messages.json --account Alex --target reply
```

The earlier messages appear as prior context. “Confirmed.” appears only under
later context. To process the same selection programmatically:

```sh
leio-code --repo /absolute/private-folder --json conversation \
  --source messages.json --account Alex --target reply
```

### Read the context sections

| Packet field | Meaning |
| --- | --- |
| `contexts.target` | The selected message. |
| `contexts.antecedents` | Same-chat messages preceding the target in both time and source order. |
| `contexts.prior_account_history` | Eligible messages from the same account and chat, strictly prior and outside the target session. |
| `contexts.prediction_input` | The union of antecedents and prior-account history; the terminal view shows the two component sections. |
| `contexts.later_retrospective_only` | Messages with later timestamps, for retrospective interpretation only. |
| `contexts.same_timestamp_unordered` | Messages sharing the target timestamp; no prior/later order is inferred. |
| `source_order_conflicts_retrospective_only` | A separate record map where timestamp order disagrees with source order; never predictive input. |

Section counts describe selected records, not every matching message in the
source. Use `--limit 20` to expand each context section from its default of 8;
`--json` alone does not expand it. Sections can overlap; repeated evidence IDs
refer to the same message. An empty section means no qualifying record was
selected within the source and context limits.

Parser errors identify the selected file; invalid normalized
messages also identify their JSON pointer, such as `/messages/1` for the second
record in a wrapped export.

## Evidence contract

The existing QueryEnvelope contains one
`urn:leio-code:conversation-evidence:v1` packet. Its assurance is
`unsigned-conversation-evidence`; the transport retains the shared unsigned
engineering evidence digest. Neither digest authenticates the origin of a chat.

- Each container and parsed transcript has SHA-256 provenance. Canonical IDs
  combine transcript-content digest and starting line or JSON record ordinal.
  TXT/ZIP evidence uses line ranges; JSON uses exact JSON pointers.
- Context records include export account label, local timestamp, session,
  excerpt, full-text digest, and an explicit truncation flag (1,200 characters).
  Account labels, including identical labels across chats, do not resolve
  physical authors. Context limits are explicit; the packet is not the whole chat.
- Antecedents and prior-account examples must precede the target strictly in
  both timestamp and source-record order (`source_ordinal`, starting at one).
  Prior-account examples exclude its session. Later records are retrospective;
  equal timestamps are unordered. Cross-chat clocks are not predictive evidence.
  Records whose timestamp and source order disagree are also exposed in the
  bounded top-level `source_order_conflicts_retrospective_only` map. This map is
  separate from the six validated context sets and is never predictive input;
  existing Reference consumers preserve it without validating its additional content.
- Thirty-minute gaps define exploratory sessions; timestamp reversals split
  them and produce warnings. Session/day overlap must not be assumed to nest.
  Alternative gaps and chronology must be checked before statistical analysis.
- Category objects are finite evidence-ID subsets. Arrows are inclusions;
  record restriction is contravariant. Implemented checks cover identity,
  restriction composition, compatible gluing and inclusion–exclusion counts.
  These laws protect data consistency; similarity and causation are not arrows
  in this category, and model transformations are not claimed to be functors.

## Formats and limits

WhatsApp input supports bracketed iOS and `date, time -` Android headers with
24-hour time, optional seconds, multiline bodies and leading bidi/BOM marks.
`date_order=dmy` is the declared default; `mdy` is explicit. Two-digit years mean
2000–2099. Unsupported timestamp-like lines, invalid dates and non-UTF-8 text
fail clearly. No timezone is inferred.

Plain text cannot reliably distinguish a pasted transcript header from an outer
message. Counts are parsed header candidates; unfamiliar labels and clock
reversals need review. For curated boundaries, pass normalized JSON: either an
array or `{"messages": [...]}`. Every row requires `timestamp` in local
`YYYY-MM-DDTHH:MM:SS` form and `text`; optional fields are `sender` or `account`,
`id`, and `chat`. Imported eligibility, duplicate, kind, quote-reason and line
annotations are retained separately; explicitly excluded/quoted/duplicate records
are withheld from prior-account examples. These annotations are not certified by
this parser, and original text is preserved. Model scores are not imported.
Different JSON chat labels maintain separate sessions and histories.
Account labels, chat labels and imported IDs must be strings or null, with at
most 1,024 UTF-8 bytes per label/ID. At most 1,024 distinct account labels are
accepted across the selected sources; larger inventories fail explicitly.
Media and deletion markers match export placeholders, not words embedded in
ordinary prose.

ZIP input must contain exactly one transcript TXT outside `__MACOSX/`. Ambiguous
ZIPs require selecting the intended TXT first. Limits: 8 sources, 2 GiB/container,
32 MiB/transcript, 20,000 ZIP entries and 100,000 total parsed records. Media is
only recognized through text markers; recordings are not transcribed. Exact
duplicate transcript content is skipped, not treated as independent evidence.

## Host review and method plan

The host should answer semantic questions using cited IDs: speech act, referent,
premise, quotation/reported speech, topic continuity and relationship history.
State plausible alternative readings and evidence against each hypothesis.
An unknown source of a premise does not prove someone invented it. Similarity
retrieval does not establish copying; an account-style mismatch does not identify
another writer. Preserve source text, ASR output and user corrections separately.

The packet marks every advanced method `proposed-not-run`, including:

| Method | Required design |
| --- | --- |
| Conditional token likelihood | Pinned model/tokenizer, identical target tokens, generic vs strictly prior context vs account examples, no target-session leakage, token losses and truncation |
| Style/change scans | Semantic/length/genre controls, chronological validation, session-block null, multiple-scan correction, segmentation and rank-scaling sensitivity |
| Zipf / Zipf–Mandelbrot | Token-weighted finite-vocabulary likelihood, frozen training ranks, unknown-token mass, held-out empirical alternative, session bootstrap |
| Pareto / EVT | Raw positive support, threshold and tail-size sensitivity, discrete/continuous likelihood, refitted-threshold goodness-of-fit, same-support alternatives, POT-GPD/Hill diagnostics, dependence and declustering |
| Tail fragility | Top-share and remove-largest sensitivity; no Gaussian conversion of bounded percentiles, unsupported extrapolation or infinite-population-moment claims |
| Reply timing | Preserve export cutoff versus separately reported observation time; ongoing waits are censored, not completed durations or evidence of motive |

The inference plan follows likelihood and fit validation rather than straight-line
log-log regressions; see [Clauset, Shalizi and Newman](https://arxiv.org/abs/0706.1062).
Finite-sample tail fragility and uncertainty are motivated by
[Taleb's Statistical Consequences of Fat Tails](https://arxiv.org/abs/2001.10488).
For the formal vocabulary, see [Spivak's Category Theory for Scientists](https://arxiv.org/abs/1302.6946).
These references inform the guidance; they do not validate an authorship finding.

## Reference Provider integration

Reference owns its adapter and validation in `app/reference_demo/conversation_evidence.py`
and `LeioBridge.conversation`. It accepts local CLI transport only, preserves the
packet/source references and enforces context validity and unsigned status.
The configured local Reference dispatcher recognizes
`prepare conversation review <relative file>` and returns the packet without
putting it in governed claims or logging private excerpts in its event payload.
Programmatic callers can select multiple sources, an account and target.

This is a local source integration, not a Central production registration,
ontology promotion, deployed capability, or replacement for the separate
model-backed analysis pipeline. A normal paired release is needed to install it.
