# Local-markdown tracker — wayfinding operations

This repo has no remote issue tracker, so the work-plan map lives here as markdown.

## Layout

- `workplan/MAP.md` — the map issue (label `workplan:map`). The canonical artifact.
- `workplan/tickets/NNN-slug.md` — child tickets. Frontmatter is the issue metadata.
- `workplan/research/<slug>.md` — findings files produced by research subagents, linked from their tickets.

## Ticket frontmatter

```yaml
id: 006                      # issue id; referenced as blocked-by targets
title: <the ticket's name>   # refer to tickets by this, never by bare id
type: workplan:research | workplan:prototype | workplan:grilling | workplan:task
status: open | closed
assignee:                    # non-empty = claimed; claim BEFORE any work
blocked-by: []               # list of ids; blocking convention (markdown has no native edges)
```

## Operations

- **Claim**: set `assignee:` to the dev/session working it, before any other edit.
- **Resolve**: append a `## Resolution` section to the ticket body, set `status: closed`,
  then add one line to MAP.md → *Decisions so far*: `- [title](tickets/NNN-slug.md) — gist`.
- **Frontier query**: tickets where `status: open`, `assignee` empty, and every id in
  `blocked-by` has `status: closed`. Example:
  `grep -l "status: open" workplan/tickets/*.md` then filter by empty assignee + closed blockers.
- **New tickets**: create with the next free id, then wire `blocked-by` in a second pass.
