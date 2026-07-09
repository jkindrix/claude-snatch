---
name: session-debrief
description: Extracts durable, non-derivable knowledge from a Claude Code session — corrected assumptions, rejected alternatives, dead ends, constraints, standing instructions — gates each candidate hard, and files survivors into their strongest enforceable home (test, hook, lint rule, site comment, ADR, CLAUDE.md, or the snatch decision/notes registries). Use when the user asks to debrief a session, capture lessons or learnings, harvest knowledge, run a retrospective, or asks "what should we remember from this".
when_to_use: Trigger phrases like "debrief", "capture what we learned", "knowledge capture", "retrospective", "what's worth remembering", end-of-session wrap-ups. Requires the snatch CLI. For judging what happened (errors, claims vs reality), use session-audit instead; debrief extracts what to keep.
---

# Session Debrief

Role: knowledge-capture auditor, not a summarizer. Most chunks yield nothing;
**zero items from a session is a valid outcome**, not a failure. Work from
evidence, never from the agent's self-report alone.

A **chunk** = one prompt boundary (typed or queued steering prompt) plus
everything it produced. Retrieval is `snatch` CLI via Bash (the MCP server
mirrors it but may predate the installed binary).

## Procedure

Copy this checklist and track progress:

```
Debrief Progress:
- [ ] 1. Map the session
- [ ] 2. Prefilter chunks
- [ ] 3. Read selected chunks
- [ ] 4. Gate each candidate item
- [ ] 5. File survivors (update, don't duplicate)
- [ ] 6. Sweep the registries
- [ ] 7. Report
```

**1. Map** — `snatch chunks <session-id>`. If a session-audit report for this
session exists in context, use it as prefilter input: verified claim-vs-reality
gaps are gate-1 surprise with evidence already attached.

**2. Prefilter — read all prompts, ration agent output.** User and steering
prompts are a sliver of the log; read 100% of them:
`snatch messages <id> -D overview -l 0 --max-text-len 2000`.
From the full prompt text select chunks whose prompts state constraints, give
directives, make or approve irreversible decisions, or repeat earlier
instructions — these calm chunks carry no distress markers and are exactly
where quiet-but-durable knowledge lives. Then add the distress-marked chunks
(`⚠ errors`, `(queued)`, abandoned branches, unusually long spans); distress
decides where the expensive `-D` reads of agent output go. Report skipped
chunks by their prompt line, not count alone, so prefilter recall is
spot-checkable.

**3. Read** — `snatch messages <id> --chunk <N> -D conversation`; escalate to
`-D full` or `--errors-only` where the narrative surprises, and to *adjacent
chunks* when an arc crosses a boundary (queued steering splits chunks: the
abandonment often sits one chunk after the rationale). High-yield sites:
user corrections of the agent (capture the corrected belief, not the event),
rejected alternatives with reasons, dead ends and why, constraints stated in
passing, durable "always/never" directives, discoveries contradicting docs.

**4. Gate — all three must hold, in order:**

- **Signal fired.** Surprise (expectation violated, root cause found, the user
  corrected the agent's model), irreversibility, blast radius, or friction
  (re-litigation, repeated instruction). Observably present in the chunk text.
- **Non-derivable — verified, not asserted.** Check the artifacts before
  claiming it: the chunk's touched files (`-D full` shows paths), `git log`
  for the session's window, existing docs/CLAUDE.md. Name what you checked.
  If the diff or docs show it, drop it.
- **Executable resurface trigger.** Name the concrete future moment this must
  precede AND a home on the ladder that will actually be present at that
  moment. A trigger no mechanism watches is trivia. Drop it.

**5. File each survivor at the strongest home reachable for its trigger class:**

| Trigger class | Ladder (strongest first) |
|---|---|
| Code change | failing test/CI > type or lint rule > comment at the exact site > ADR/docs |
| Agent behavior / process | hook (Claude Code or git) > project CLAUDE.md |
| Human decision (architecture, scope) | ADR/docs > `snatch decisions add` |
| Not yet homeable | `manage_decisions` (decisions, rejected alternatives) / `manage_notes` (gotchas, dead ends, tactical constraints) — set `resurface_when`/`expires_when` as structured fields (CLI `--resurface-when`/`--expires-when`), never as prose inside the text |

An item that becomes a test or hook is done — do not also record it in prose.
**Before filing, check the target home for an existing entry: update or
supersede it; never write a drifting sibling.** Never leave two live items
that disagree.

**6. Sweep.** While the registries are open: retire notes/decisions whose
`expires_when` has passed or that current artifacts now contradict; when
this session re-confirmed an existing item, record the confirmation on it
(repetition is the promotion signal — recommend escalating its home one rung).

**7. Report.** Items filed (statement → home → resurface trigger), items
rejected per gate (one line each), registry ops performed, and skipped
chunks listed by prompt line.

## Item discipline

- One falsifiable sentence carrying the *because* when the reason is the
  payload: "session IDs are ULIDs because sort order is load-bearing for
  replay", not "IDs matter".
- The statement carries its validity scope ("in this repo", "while X holds").
- Attach the message UUID from the chunk as evidence, plus the artifact
  checked for Gate 2.
- A single occurrence caps at gotcha/dead-end/note. A standing "always/never"
  rule requires an explicit user directive or a recorded re-confirmation —
  one anecdote never becomes a rule.

## Refuse — these are drop rules, not style advice

- Progress narration, plans not yet load-bearing, restatements, praise —
  the commit log's job.
- Anything the diff, tests, or existing docs already show.
- Items where no kind fits: that is Gate 1 failing, not a filing problem.
- Speculated optional fields — omit `expires_when` and extra signals rather
  than guess; one signal honestly present beats three plausible ones.
- Claims inherited from confident assertion (the user's or the agent's)
  without evidence in the chunk — capture with attribution or drop.
- Chunk size and drama are not signals; gate each item individually.

Calibration: 0 items is normal, 1–2 per eventful chunk is busy, 3+ means you
are summarizing — re-gate each against all three gates.

## Skill feedback

If a step here didn't match reality (wrong flag, missing capability,
misleading guidance), say so to the user in one line at the end of the
debrief. Do not edit this skill unprompted.
