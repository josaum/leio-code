import { z } from "zod";

const UiBadgeSchema = z
  .object({
    key: z.string().optional(),
    label: z.string().optional(),
    value: z.string().optional(),
    tone: z.string().optional(),
  })
  .passthrough();

const UiHintsSchema = z
  .object({
    badges: z.array(UiBadgeSchema).optional(),
    states: z.record(z.unknown()).optional(),
  })
  .passthrough();

const EnvelopeSummarySchema = z
  .object({
    summary: z.unknown().optional(),
    kind: z.string().nullable().optional(),
    confidence: z.number().nullable().optional(),
    entity_count: z.number().optional(),
    evidence_count: z.number().optional(),
    warning_count: z.number().optional(),
    workspace_profile: z.string().nullable().optional(),
  })
  .passthrough();

const QueryEnvelopeSchema = z
  .object({
    schema_version: z.string().optional(),
    kind: z.string().optional(),
    summary: z.unknown().optional(),
    confidence: z.number().optional(),
    entities: z.array(z.record(z.unknown())).optional(),
    evidence: z.array(z.record(z.unknown())).optional(),
    warnings: z.array(z.unknown()).optional(),
    meta: z.record(z.unknown()).optional(),
    timing_ms: z.number().optional(),
  })
  .passthrough();

const ActionPaletteSchema = z
  .object({
    show_tools: z.array(z.string()).optional(),
    hide_tools: z.array(z.string()).optional(),
    recommended_start_tool: z.string().optional(),
    recommended_next_tools: z.array(z.string()).optional(),
    query_starters: z.array(z.string()).optional(),
    source_contract: z.string().optional(),
  })
  .passthrough();

const SourceControlSchema = z
  .object({
    supports_repo_url: z.boolean().optional(),
    supports_server_repo_root: z.boolean().optional(),
    selected_repository: z.record(z.unknown()).nullable().optional(),
    github: z.record(z.unknown()).optional(),
  })
  .passthrough();

const EvidenceContractSchema = z
  .object({
    schema: z.literal("urn:leio-code:unsigned-engineering-evidence:v1"),
    version: z.literal("1.0"),
    assurance: z.literal("unsigned-engineering-evidence"),
    effect: z.literal("read-only"),
    transport: z.enum(["stdio", "streamable-http"]),
    producer: z
      .object({
        name: z.literal("leio-code"),
        version: z.string().nullable(),
      })
      .passthrough(),
    payload_sha256: z.string().regex(/^[a-f0-9]{64}$/),
  })
  .passthrough();

/**
 * JSON Schema (2020-12 default) for CallToolResult.structuredContent.
 * Root MUST be type: object. Extra keys are allowed so CLI envelopes can grow.
 */
export const LeioToolOutputSchema = z
  .object({
    ok: z.boolean().optional(),
    error: z.string().optional(),
    exit_code: z.number().optional(),
    summary: z.string().optional(),
    repo_root: z.string().optional(),
    repo_url: z.string().nullable().optional(),
    git_ref: z.string().nullable().optional(),
    revision: z.string().nullable().optional(),
    workspace_profile: z.string().nullable().optional(),
    sections: z.array(z.string()).optional(),
    envelope: QueryEnvelopeSchema.nullable().optional(),
    envelope_summary: EnvelopeSummarySchema.nullable().optional(),
    ui_hints: UiHintsSchema.optional(),
    workspace_capability_hints: z.record(z.unknown()).nullable().optional(),
    workspace_capabilities: z.record(z.unknown()).nullable().optional(),
    action_palette: ActionPaletteSchema.nullable().optional(),
    source_control: SourceControlSchema.nullable().optional(),
    specialist_bridge: z.record(z.unknown()).nullable().optional(),
    specialist_review: z.record(z.unknown()).nullable().optional(),
    auth: z.record(z.unknown()).optional(),
    mcp_session_id: z.string().nullable().optional(),
    evidence_contract: EvidenceContractSchema.optional(),
  })
  .passthrough();

const HarnessStageSchema = z.object({
  status: z.enum(["passed", "failed", "timed_out", "canceled", "infra_error"]),
  exitCode: z.number().int().nullable(),
  stdoutPath: z.string(), stderrPath: z.string(),
}).passthrough();
const HarnessTaskSchema = z.object({
  runId: z.string(), repoRoot: z.string(), task: z.string(), worker: z.string(),
  state: z.enum(["queued", "running", "blocked", "failed", "cancelled", "succeeded"]),
  phase: z.string(), supervisorPid: z.number().int().nullable(), workerPid: z.number().int().nullable(),
  supervisorAlive: z.boolean(), elapsedMs: z.number(), silenceMs: z.number(),
  sequence: z.number().int(), lastActivityAtMs: z.number(), lastOutputAtMs: z.number().nullable(),
  lastActivity: z.string(), error: z.string().nullable(), runDir: z.string(),
  workerResult: HarnessStageSchema.nullable(), verificationResult: HarnessStageSchema.nullable(),
  changedPaths: z.array(z.string()), diffPath: z.string().nullable(), diffTruncated: z.boolean(),
  untrackedArtifacts: z.array(z.string()), untrackedTruncated: z.boolean(),
  eventsPath: z.string(), specPath: z.string(), supervisorLogPath: z.string(),
  cancellationRequested: z.boolean().optional(),
}).passthrough();
const HarnessArtifactPreviewSchema = z.object({path:z.string().nullable(),text:z.string(),available:z.boolean(),truncated:z.boolean(),error:z.string().nullable()});
export const LeioTaskToolOutputSchema = LeioToolOutputSchema.extend({
  review_source: z.object({version:z.literal(1),source_repo:z.string(),repo_root:z.string(),captured_at:z.string(),files:z.array(z.object({path:z.string(),sha256:z.string(),bytes:z.number().int().nonnegative()})),scope:z.string()}).optional(),
  review_runs: z.array(z.object({repo_root:z.string(),run_id:z.string(),captured_at:z.string(),source_files:z.number().int(),recovered:z.boolean().optional(),next_call:z.object({tool:z.literal('leio_code_task'),arguments:z.record(z.unknown()),reason:z.string()})})).optional(),
  review_runs_truncated: z.boolean().optional(),
  review_catalog_incomplete: z.boolean().optional(),
  review_warnings: z.array(z.object({kind:z.string(),path:z.string(),message:z.string()})).optional(),
  preparation: z.object({source_repo:z.string(),repo_root:z.string(),baseline_revision:z.string(),source_edits_excluded:z.boolean(),detached:z.literal(true),clean:z.boolean()}).optional(),
  next_calls: z.array(z.object({tool:z.literal("leio_code_task"),arguments:z.record(z.unknown()),reason:z.string()})).optional(),
  review: z.object({version:z.literal(1),outcome:z.string(),phase:z.string(),error:z.string().nullable(),next_action:z.enum(["observe_or_cancel","review_changes","review_findings","inspect_partial_changes","inspect_failure"]),
    verification:z.object({status:z.string(),exit_code:z.number().nullable()}).nullable(),changed_files:z.array(z.string()),changed_files_truncated:z.boolean(),
    artifacts:z.object({patch:HarnessArtifactPreviewSchema,worker_stdout:HarnessArtifactPreviewSchema,worker_stderr:HarnessArtifactPreviewSchema,verification_stdout:HarnessArtifactPreviewSchema,verification_stderr:HarnessArtifactPreviewSchema}),
    new_files:z.array(z.object({file:z.string(),preview:HarnessArtifactPreviewSchema})),new_files_incomplete:z.boolean(),
    artifact_capture_truncated:z.boolean(),assurance:z.string()}).optional(),
  task: z.union([
    HarnessTaskSchema,
    z.object({repoRoot: z.string(), runs: z.array(HarnessTaskSchema)}),
    z.object({runId: z.string(), events: z.array(z.object({seq:z.number().int(),timestampMs:z.number(),kind:z.string(),phase:z.string(),state:z.string(),workerPid:z.number().int().nullable(),detail:z.string()})),nextAfter:z.number().int(),hasMore:z.boolean()}),
  ]).optional(),
});

export const LeioStatusToolOutputSchema = LeioToolOutputSchema.extend({
  doctor_summary: z.object({
    scope: z.literal("baseline"),
    status: z.enum(["passed", "warnings", "failed", "unavailable"]),
    doctor_count: z.number(),
    skipped_count: z.number().optional(),
    warning_count: z.number(),
    exit_code: z.number().nullable(),
    error: z.string().optional(),
  }).passthrough().optional(),
});

export const LeioNavigationToolOutputSchema = LeioToolOutputSchema.extend({
  next_tools: z.array(z.string()).optional(),
  next_calls: z.array(z.object({
    tool: z.string(),
    arguments: z.record(z.unknown()),
    reason: z.string(),
  })).optional(),
});

export const LeioContextToolOutputSchema = LeioNavigationToolOutputSchema.extend({
  orientation: z.object({
    provider: z.object({ transport: z.literal("stdio"), entrypoint: z.string(), pid: z.number(),
      binary: z.string(), binary_version: z.string().nullable() }),
    index: z.object({ state: z.enum(["available", "repository_mismatch", "missing", "unreadable"]),
      path: z.string(), source_freshness: z.string(), excluded_files: z.null(),
      limitations: z.array(z.string()) }).passthrough(),
    retrieval: z.object({ state: z.enum(["ranked_candidates", "no_matches"]), selected_files: z.number(),
      calibrated_confidence: z.literal(false), limitations: z.array(z.string()), next_action: z.string() }),
    available_graph_kinds: z.array(z.string()), baseline: z.string(),
  }).optional(),
});

export const LeioNavToolOutputSchema = LeioNavigationToolOutputSchema.extend({
  envelope: QueryEnvelopeSchema.extend({
    meta: z.object({
      navigation_mode: z.enum(["graph", "lattice", "heading"]).nullable().optional(),
      lattice: z.object({
        state: z.enum(["missing", "current", "stale", "unverified", "invalid", "unchecked"]),
        artifact_path: z.string(),
        reason: z.string(),
        rebuild_required: z.boolean(),
      }).passthrough().optional(),
      result_page: z.object({
        offset: z.number().int().nonnegative(),
        limit: z.number().int().min(1).max(100),
        returned: z.number().int().nonnegative(),
        total: z.number().int().nonnegative().nullable(),
        has_more: z.boolean(),
        next_offset: z.number().int().nonnegative().nullable(),
        query: z.object({ kind: z.string(), needle: z.string().nullable().optional() }).passthrough(),
      }).passthrough().nullable().optional(),
    }).passthrough().optional(),
  }).nullable().optional(),
});

export const LeioGuideToolOutputSchema = LeioToolOutputSchema.extend({
  topic: z.string().optional(),
  next_tools: z.array(z.string()).optional(),
  routing_doc: z.string().optional(),
  routing_note: z.string().optional(),
  recommended_tools: z.array(z.string()).optional(),
});

export const LeioSessionTargetOutputSchema = LeioToolOutputSchema.extend({
  selected_repository: z.record(z.unknown()).nullable().optional(),
});

export const LeioSpecialistOutputSchema = LeioToolOutputSchema.extend({
  question: z.string().optional(),
  depth: z.string().optional(),
  answer: z.string().optional(),
  answer_id: z.string().optional(),
  citations: z.array(z.unknown()).optional(),
  research_dossier: z.record(z.unknown()).nullable().optional(),
});

export const LeioWatchToolOutputSchema = LeioToolOutputSchema.extend({
  action: z.string().optional(),
  running: z.boolean().optional(),
  pid: z.number().nullable().optional(),
  pid_path: z.string().optional(),
  log_path: z.string().optional(),
});
