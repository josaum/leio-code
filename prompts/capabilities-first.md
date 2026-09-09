# Capabilities First

Use this when the repository may be generic and you need to know what LEIO Code actually supports before asking for profile-specific facets.

## Goal

Establish the current `workspace_profile`, optional workspace facets, and which `find` / `explain` / `doctor` families are meaningful.

## Suggested Flow

1. Run `leio_code_capabilities`.
2. If the repo exposes deploy topology or cartridges, then use `leio_code_find` or `leio_code_explain` for those facets.
3. If the repo exposes doctors, then use `leio_code_doctor`.
4. If the repo is generic, stay on symbols, env vars, Redis keys, routes, graphs, and exports.

## Output Shape

Return:
- the capability summary
- the first safe next query
- the facets that are not modeled, if any
