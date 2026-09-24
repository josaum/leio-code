/** Compact successful editing-loop results; full=true retains the diagnostic packet. */
const pick = (value, keys) => Object.fromEntries(keys.filter(k => value?.[k] !== undefined).map(k => [k,value[k]]));
export function compactEditingResult(result, {full = false, scope = {}} = {}) {
  const original = result.structuredContent;
  if (full || !original?.ok || !["context","graph","nav"].includes(original.tool_family)) return result;
  const data = pick(original,["ok","exit_code","repo_root","tool_family","orientation","next_calls","kind_catalog_warning"]);
  const envelope = original.envelope;
  if (!envelope) return result;
  const lean = pick(envelope,["schema_version","query_id","kind","summary","timing_ms"]);
  lean.evidence = (envelope.evidence ?? []).slice(0,3);
  lean.warnings = envelope.warnings ?? [];
  if (original.tool_family === "context") {
    const context = envelope.entities?.[0] ?? {};
    lean.entities = [{...pick(context,["task","instruction_sources","memory_sources","verification_anchors","tests_to_run","risk_notes"]),
      files_to_read:(context.files_to_read ?? []).slice(0,5).map(row => ({...pick(row,["path","language","reasons"]),symbols:row.symbols?.slice(0,3)})),
      omitted_files:Math.max(0,(context.files_to_read?.length ?? 0)-5)}];
    lean.meta = pick(envelope.meta,["workspace_profile"]);
  } else if (original.tool_family === "nav") {
    const current = envelope.entities?.find(row => row.role === "current");
    const graphMode = envelope.meta?.navigation_mode === "graph";
    lean.entities = (envelope.entities ?? []).filter(row => !(row.role === "result" && current && row.graph_symbol && row.graph_symbol === current.graph_symbol))
      .map(row => graphMode ? pick(row,["role","index","path","line","symbol","kind","graph_symbol","source"]) : {...row});
    lean.meta = pick(envelope.meta,["action","history_len","future_len","session","navigation_mode","followed_edge","result_page"]);
    if (!graphMode) lean.meta.lattice = envelope.meta?.lattice;
    else lean.warnings = lean.warnings.filter(w => !(typeof w === "string" && w.startsWith("lattice ")));
    if (lean.meta.result_page) lean.meta.result_page = {...lean.meta.result_page,query:pick(lean.meta.result_page.query,["kind","needle"])};
  } else {
    lean.entities = (envelope.entities ?? []).slice(0,20).map(row => ({...row}));
    lean.meta = {...pick(envelope.meta,["kind","direction"]), returned:lean.entities.length,total:envelope.entities?.length ?? 0,truncated:(envelope.entities?.length ?? 0)>20};
  }
  data.envelope = lean;
  data.limitation = "Source and indexed relationships are engineering observations, not runtime proof.";
  data.next_calls = (data.next_calls ?? []).slice(0,3);
  for (const row of lean.entities ?? []) {
    const needle = row.graph_symbol ?? (typeof row.symbol === "string" && row.symbol.startsWith("urn:") ? row.symbol : undefined);
    if (needle) row.open = {tool:"leio_code_nav",arguments:{repo_root:original.repo_root,...pick(scope,["index_path","session"]),kind:"goto",needle}};
  }
  data.diagnostics = {tool:`leio_code_${original.tool_family}`,arguments:{...scope,repo_root:original.repo_root,full:true}};
  // Source text appears once, in structuredContent. The text block is a short finding.
  return {...result,content:[{type:"text",text:typeof lean.summary === "string" ? lean.summary : "LEIO result"}],structuredContent:data};
}
