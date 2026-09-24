/** Machine-readable state for the bounded doctor run used by quick status. */
export function summarizeBaseline(execution, envelope) {
  const allRows = Array.isArray(envelope?.doctors)
    ? envelope.doctors
    : Array.isArray(envelope?.entities) ? envelope.entities.filter((row) => row?.doctor) : [];
  const rows = allRows.filter((row) => row.skipped !== true);
  const count = (value) => Number.isFinite(value) && value >= 0 ? value : 0;
  const warningCount = Math.max(
    rows.reduce((sum, row) => sum + count(row.warning_count), 0),
    Array.isArray(envelope?.warnings) ? envelope.warnings.length : 0,
  );
  const hasReport = typeof envelope?.summary === "string"
    && (Array.isArray(envelope?.doctors) || Array.isArray(envelope?.entities));
  const status = execution.code !== 0 ? "failed"
    : !hasReport ? "unavailable"
      : warningCount > 0 ? "warnings" : "passed";
  return {
    scope: "baseline",
    status,
    doctor_count: rows.length,
    skipped_count: allRows.length - rows.length,
    warning_count: warningCount,
    exit_code: execution.code,
    workspace_profile: envelope?.meta?.workspace_profile ?? null,
    doctors: rows.map((row) => ({ name: row.doctor ?? row.name, warning_count: count(row.warning_count) })),
    ...(status === "failed" || status === "unavailable"
      ? { error: (execution.stderr?.trim() || "Baseline returned no usable report").slice(-2000) }
      : {}),
  };
}

export function formatBaseline(summary) {
  const profile = summary.workspace_profile ? ` (${summary.workspace_profile})` : "";
  return `Baseline doctors: ${summary.status}; ${summary.doctor_count} checked, ${summary.skipped_count ?? 0} skipped, ${summary.warning_count} warnings${profile}`;
}

/** Copy only executable graph recommendations; never forward arbitrary tool args. */
export function contextNextCalls(envelope, { repoRoot, indexPath, graphKinds, session }) {
  const files = envelope?.entities?.[0]?.files_to_read;
  if (Array.isArray(files)) {
    const calls = [];
    const seen = new Set();
    for (const file of files) {
      if (!nonempty(file?.path) || seen.has(file.path)) continue;
      seen.add(file.path);
      const symbol = file.symbols?.find(row => callable(row) && Number.isInteger(row.line) && row.line > 0)
        ?? file.symbols?.find(row => Number.isInteger(row.line) && row.line > 0);
      const scope = { repo_root: repoRoot, ...(indexPath ? { index_path: indexPath } : {}) };
      if (symbol) calls.push({ tool:"leio_code_nav", arguments:{...scope, ...(navigationSession(session) ? {session:navigationSession(session)} : {}), kind:"goto", needle:`definition:${file.path}#${symbol.line}`}, reason:`Open the indexed ${symbol.name} definition with live source and freshness` });
      else if (!graphKinds || graphKinds.includes("symbols-in")) calls.push({tool:"leio_code_graph", arguments:{...scope,kind:"symbols-in",needle:file.path},reason:"Inspect exact definitions in this ranked file"});
      if (calls.length === 3) break;
    }
    return calls;
  }

  const queries = envelope?.entities?.[0]?.graph_queries;
  if (!Array.isArray(queries)) return [];
  const calls = [];
  const seen = new Set();
  for (const query of queries) {
    if (query?.tool !== "leio_code_graph"
      || typeof query.kind !== "string"
      || (graphKinds && !graphKinds.includes(query.kind))
      || typeof query.needle !== "string" || !query.needle.trim()) continue;
    const key = JSON.stringify([query.kind, query.needle]);
    if (seen.has(key)) continue;
    seen.add(key);
    calls.push({
      tool: "leio_code_graph",
      arguments: {
        repo_root: repoRoot,
        ...(indexPath ? { index_path: indexPath } : {}),
        kind: query.kind,
        needle: query.needle,
      },
      reason: typeof query.reason === "string" ? query.reason : "Inspect the suggested code relationship",
    });
    if (calls.length === 4) break;
  }
  return calls;
}

const CALL_GRAPH_KINDS = ["callers-of", "callees-of", "callsites-of"];
const callable = (row) => ["function", "method", "class", "interface"].includes(row?.kind);
const nonempty = (value) => typeof value === "string" && value.trim().length > 0;
const symbolId = (row) => row?.kind !== "file" && nonempty(row?.symbol) && row.symbol.startsWith("urn:") ? row.symbol : null;

function sameNavNode(left, right) {
  if (!left || !right) return false;
  if (left.graph_symbol && right.graph_symbol) return left.graph_symbol === right.graph_symbol;
  if (left.section && right.section) return left.section === right.section;
  return left.path === right.path && left.symbol === right.symbol && left.kind === right.kind;
}

/** Match sidecar::sanitize_session_slug so inherited host IDs remain callable. */
function navigationSession(raw) {
  if (typeof raw !== "string") return undefined;
  let slug = "";
  for (const char of raw) {
    if (slug.length >= 64) break;
    if (/^[A-Za-z0-9._-]$/.test(char)) slug += char;
    else if (slug && !slug.endsWith("-")) slug += "-";
  }
  return slug.replace(/^-+|-+$/g, "") || undefined;
}

function navigationCalls({ repoRoot, indexPath, session, graphKinds }) {
  session = navigationSession(session);
  const calls = [];
  const seen = new Set();
  const add = (tool, args, reason) => {
    if (calls.length >= 4) return;
    if (tool === "leio_code_graph" && Array.isArray(graphKinds) && !graphKinds.includes(args.kind)) return;
    const key = JSON.stringify([tool, args]);
    if (seen.has(key)) return;
    seen.add(key);
    calls.push({
      tool,
      arguments: {
        repo_root: repoRoot,
        ...(indexPath ? { index_path: indexPath } : {}),
        ...(tool === "leio_code_nav" && session ? { session } : {}),
        ...args,
      },
      reason,
    });
  };
  return { calls, add };
}

/** Ground graph follow-ups in returned identities, never in a guessed symbol name. */
export function graphNextCalls(envelope, scope) {
  const rows = envelope?.entities;
  if (!Array.isArray(rows) || rows.length === 0) return [];
  const { calls, add } = navigationCalls(scope);
  const resolved = rows[0]?.resolved_symbol;
  const file = rows[0]?.resolved_file;
  const graph = (kind, needle, reason) => {
    if (nonempty(needle)) add("leio_code_graph", { kind, needle }, reason);
  };
  const goto = (row) => {
    if (symbolId(row)) add("leio_code_nav", { kind: "goto", needle: row.symbol },
      `Start a cursor at ${row.qual_name || row.name || row.symbol} in ${row.path}; use a distinct session for each agent`);
  };

  if (symbolId(resolved)) {
    if (callable(resolved)) {
      for (const kind of CALL_GRAPH_KINDS.filter((kind) => kind !== scope.kind).reverse()) {
        graph(kind, resolved.symbol, `Inspect ${kind} for the resolved symbol in ${resolved.path}`);
      }
    }
    goto(resolved);
    graph("symbols-in", resolved.path, "Inspect nearby definitions in the resolved file");
  } else if (CALL_GRAPH_KINDS.includes(scope.kind)) {
    for (const row of rows) {
      if (symbolId(row)) graph(scope.kind, row.symbol, `Resolve this exact candidate: ${row.qual_name || row.name} in ${row.path}`);
    }
  } else if (file) {
    if (scope.kind === "symbols-in") {
      const symbols = rows.filter((row) => symbolId(row));
      // Keep both the cursor bridge and concrete call evidence in the bounded set.
      const first = symbols.find(callable) ?? symbols[0];
      if (first) {
        goto(first);
        if (callable(first)) graph("callsites-of", first.symbol, "Inspect concrete callsites before following semantic relationships");
      }
      graph("resolved-importers-of", file.path, "Inspect incoming dependencies of this file");
      graph("resolved-imports-in", file.path, "Inspect this file's resolved dependencies");
    } else {
      graph("symbols-in", file.path, "Inspect definitions in the resolved file");
      if (scope.kind !== "resolved-importers-of") graph("resolved-importers-of", file.path, "Inspect incoming dependencies of this file");
      for (const row of rows) {
        for (const candidate of Array.isArray(row?.candidate_paths) ? row.candidate_paths : []) {
          graph("symbols-in", candidate, "Inspect a returned dependency candidate; resolution may be ambiguous");
        }
      }
    }
  } else {
    // File ambiguity and reverse-import/dead-code rows can still give exact paths.
    for (const row of rows) {
      if (nonempty(row?.file) && row.file.startsWith("urn:") && scope.kind !== "dead-code") {
        graph(scope.kind, row.file, `Resolve this exact file candidate: ${row.path}`);
      } else if (nonempty(row?.path)) {
        graph("symbols-in", row.path, "Inspect definitions at this returned source path");
      }
    }
  }
  return calls;
}

/** Offer cursor moves only from the current node, result indices and session. */
export function navNextCalls(envelope, scope) {
  if (envelope?.kind && envelope.kind !== "nav") return [];
  const rows = envelope?.entities;
  if (!Array.isArray(rows)) return [];
  const meta = envelope?.meta ?? {};
  const reportedSession = meta.session?.id;
  const session = typeof reportedSession === "string" && /^[A-Za-z0-9._-]{1,64}$/.test(reportedSession)
    ? reportedSession : scope.session;
  const { calls, add } = navigationCalls({ ...scope, session });
  const current = rows.find((row) => row.role === "current");
  const nav = (kind, reason, extra = {}) => add("leio_code_nav", { kind, ...extra }, reason);
  const listKinds = ["goto", "callers", "callees", "neighbors", "related", "parent", "child", "peer", "align"];
  const page = meta.result_page;
  const activePage = page && listKinds.includes(page.query?.kind)
    && (meta.action === "here" || meta.action === page.query.kind);
  const action = activePage ? page.query.kind : meta.action;
  const listing = listKinds.includes(action);
  const latticeMode = ["parent", "child", "peer", "align"].includes(action)
    || meta.navigation_mode === "lattice" || current?.kind === "fca_concept";
  const headingMode = !latticeMode && (meta.navigation_mode === "heading" || current?.section || meta.current_section);
  if (latticeMode && meta.lattice?.rebuild_required === true) {
    add("leio_code_export", { kind: "formal-context" },
      `Explicitly rebuild the ${meta.lattice.state || "unverified"} lattice before relying on its concepts; export may report induction limits or a missing FCA backend`);
  }
  if (listing) {
    for (const row of rows.filter((row) => row.role === "result" && Number.isInteger(row.index) && row.index >= 0
      && !sameNavNode(row, current)).slice(0, 2)) {
      const label = row.concept_details?.label || row.symbol;
      if (nonempty(row.graph_symbol)) nav("goto", `Open ${label} with live source`, {needle:row.graph_symbol});
      else nav("select", `Move to result ${row.index}: ${label}${nonempty(row.path) ? ` (${row.path})` : ""} before continuing the walk`, { index: row.index });
    }
  }
  if (activePage && page.has_more === true
    && Number.isSafeInteger(page.offset) && page.offset >= 0
    && Number.isSafeInteger(page.next_offset) && page.next_offset > page.offset
    && Number.isSafeInteger(page.limit) && page.limit >= 1 && page.limit <= 100
    && (action !== "goto" || nonempty(page.query.needle))) {
    nav(action, `Continue ${action} results at offset ${page.next_offset} in the same cursor`, {
      ...(action === "goto" ? { needle: page.query.needle } : {}),
      offset: page.next_offset, limit: page.limit,
    });
  }
  // Keep the active investigation ahead of history and avoid immediately
  // suggesting the same query again after it returned no useful result.
  const moves = latticeMode ? ["parent", "child", "peer"]
    : headingMode ? ["related"] : callable(current) ? ["callers", "callees"] : [];
  const reasons = {
    parent: "List more general FCA concepts and inspect their defining attributes",
    child: "List more specific FCA concepts, then select a returned index",
    peer: "List concepts sharing a parent; shared attributes are not call evidence",
    callers: "List callers of the current code symbol",
    callees: "List callees of the current code symbol",
    related: "List neighboring headings and related concepts",
  };
  for (const kind of moves) {
    if (kind !== action) {
      nav(kind, reasons[kind], ["callers", "callees"].includes(kind) ? {follow:true} : {});
      break;
    }
  }
  if (Number.isSafeInteger(current?.source?.next_offset) && current.source.next_offset > (current.source.offset ?? 0)) {
    nav("here", "Read the next bounded source window without leaving navigation", {source_offset:current.source.next_offset});
  }
  if (current?.kind === "file" && nonempty(current.path)) {
    add("leio_code_graph", { kind: "symbols-in", needle: current.path }, "Inspect definitions in the current file");
    add("leio_code_graph", { kind: "resolved-imports-in", needle: current.path }, "Inspect this file's dependencies");
  }
  for (const kind of meta.action === "back" ? ["forward", "back"] : ["back", "forward"]) {
    if ((kind === "back" ? meta.history_len : meta.future_len) > 0) {
      nav(kind, kind === "back" ? "Return to the previous cursor position" : "Return to the next cursor position");
    }
  }
  for (const kind of moves) if (kind !== action) nav(kind, reasons[kind]);
  if (!latticeMode && (current?.concept || meta.current_concept)) {
    nav("parent", "Inspect the current node's broader FCA concept after checking graph evidence");
  }
  if (calls.length === 0) add("leio_code_guide", { topic: "navigation" }, "Choose a graph symbol, file, heading or FCA concept to start navigation");
  return calls;
}
