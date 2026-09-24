# LEIO Code Prompt Pack

Use these prompts when you want `LEIO Code` to stay narrow, evidence-first, and fast.

## Available Prompts

- [capabilities-first.md](capabilities-first.md)
  - discover what the current repository profile actually supports before asking for deploy targets, cartridges, or doctors
- [deploy-debug.md](deploy-debug.md)
  - deploy lineage, readiness, smoke, rollback, secret-set, and target drift
- [drift-hunt.md](drift-hunt.md)
  - doctor-first contract drift investigation
- [graph-investigation.md](graph-investigation.md)
  - call flow, import topology, and structural ownership

## Rule

Prefer a narrow prompt that explicitly names:

- the entity or target
- the kind of evidence required
- the desired output shape
- the repository capability assumption, if any

That keeps the agent from drifting into raw grep and commentary.
