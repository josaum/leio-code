# Deploy Debug Prompt

Use `LEIO Code` only.

1. Run the minimum `doctor` or `explain deploy-target` queries needed to understand the target.
2. Report only:
   - verdict
   - readiness/health/smoke/rollback/secret-set lineage
   - concrete files involved
   - the next single command if the target is still ambiguous
3. Do not expand into generic repo crawling unless LEIO coverage is insufficient.
