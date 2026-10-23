# LEIO Research Papers & Reproducible Evaluation Suite

This directory contains two complete, publication-ready academic manuscripts—in both self-contained LaTeX (`main.tex`) and interactive Markdown (`PAPER.md`)—along with reproducible evaluation and ablation harnesses grounded in this repository (`v2.6.5`).

---

## 1. Papers Included

### Paper 1 (Software Engineering / Agentic Coding — ICSE / FSE / ASE)
- **Title:** *Beyond the Call Graph: Infrastructure-Aware Code Graphs, Formal Concept Lattices, and Executable Drift Contracts for Coding Agents*
- **Compiled PDF:** [`papers/leio-code-icse/leio-code-icse.pdf`](leio-code-icse/leio-code-icse.pdf)
- **LaTeX Manuscript:** [`papers/leio-code-icse/main.tex`](leio-code-icse/main.tex)
- **Markdown Edition:** [`papers/leio-code-icse/PAPER.md`](leio-code-icse/PAPER.md)
- **Core Contributions:**
  1. Heterogeneous operational code graph spanning 11 vertex kinds and 12 cross-layer edge types across polyglot source files, Docker/Compose/systemd manifests, env contracts, Redis key lifecycles, and subprocess boundaries.
  2. Pure-Rust bitset `In-Close` Galois concept lattice ($2,474$ closed concepts over $8,343$ objects, $4,061$ attributes, and $67,642$ incidences induced in $593\,\text{ms}$), category-theoretic functor coherence ($1.000$), and fail-closed Oxigraph SPARQL grounding (`leio-knowledge-core`).
  3. Diagnosis and formal resolution of the *Single-Token Multi-Channel RRF Amplification Pathology* via Coverage-Calibrated Multi-Faceted RRF and the ACE Git co-edit playbook:
     - **25-Task Multi-Category Benchmark:** $\text{MRR} = 0.9800$, $\text{Hit@1} = 96.0\%$, $\text{Hit@3} = 100.0\%$, **$29.4\times$ token reduction** vs. `ripgrep` ($17,261 \to 587$ tokens), and **$100.0\%$ coupled co-edit partner recall** (vs. $16.7\%$ without playbook).
     - **7-Task Golden Retrieval Suite:** $\text{MRR} = 1.0000$, $\text{Hit@1} = 100.0\%$ ($7/7$).

### Paper 2 (Multi-Agent Systems / Machine Learning — ICLR / ICML / NeurIPS)
- **Title:** *Zero-Chatter Multi-Agent Code Synthesis via Arrow Flight State Embeddings, Isotropy-Guided Evolution, and Fail-Closed Integration Gates*
- **Compiled PDF:** [`papers/leio-harness-iclr/leio-harness-iclr.pdf`](leio-harness-iclr/leio-harness-iclr.pdf)
- **LaTeX Manuscript:** [`papers/leio-harness-iclr/main.tex`](leio-harness-iclr/main.tex)
- **Markdown Edition:** [`papers/leio-harness-iclr/PAPER.md`](leio-harness-iclr/PAPER.md)
- **Core Contributions:**
  1. Zero-chatter multi-agent coordination over Apache Arrow Flight (`do_put`, `do_get`, and bidirectional `do_exchange` cosine top-$k$ matching), eliminating $\mathcal{O}(K^2 \cdot \bar{L})$ conversational token overhead.
  2. Spherical evolutionary state search (`EmbeddingGEPA`) using geodesic `slerp`, tangent-space Gaussian perturbations clipped to a hard angular trust region $\theta_{\max}$, and semantic-anchor regularization over a Pareto archive.
  3. Closed-form sliced Epps–Pulley isotropy collapse detector (`SIGReg`), cleanly separating collapsed 8-lane populations ($I_{\text{EP}} = 3.5042 > 2.0$) from isotropic populations ($I_{\text{EP}} = 0.3432 \le 2.0$, $10.21\times$ separation).
  4. Fail-closed integration and fast-forward promotion gate (`run_gate` / `evaluate_improvement`) with SHA-256 cryptographic evidence digests, evaluated across 88 parallel multi-model lanes (8 frontier and flash models).

---

## 2. Reproducing All Tables and Ablations

```bash
# 1. Build and install release binaries (leio-code + leio-harness)
make install

# 2. Run the 25-task 6-mode retrieval ablation, coupled co-edit recall, and SIGReg isotropy verification
python3 scripts/benchmark_paper_ablations.py --binary ~/.cargo/bin/leio-code

# 3. Run the 7-task golden retrieval evaluator
python3 scripts/evaluate_retrieval.py \
  --binary ~/.cargo/bin/leio-code \
  --repo . \
  --tasks benchmarks/retrieval-agent-tasks.json \
  --output benchmarks/retrieval-agent-results.json

# 4. Export the Formal Concept Lattice and verify functor coherence
~/.cargo/bin/leio-code --json --repo . export formal-context
```
