---
name: wb-ai-pr-review
description: Run evidence-based AI pull request reviews locally or in CI, ranking verified defects over style comments
---

# AI Pull Request Review

## Principles
- A finding needs a concrete failure scenario: input/state -> wrong output, crash, data loss or security impact.
- Verify each candidate finding against the code before reporting it; drop what cannot be reproduced or reasoned through.
- Rank by severity; keep style and preference notes separate and short.
- Never auto-merge, auto-approve or push fixes without explicit human approval.

## Procedure
1. Collect context: PR description, linked issue, diff (`gh pr diff <n>`), touched tests and project instructions (`AGENTS.md`, `CLAUDE.md`).
2. Map the change: entry points, changed contracts, migrations, configuration and concurrency boundaries.
3. Review passes: correctness, security (input validation, authz, secrets, injection), data handling, error paths, performance, test coverage.
4. Verify: run the narrowest relevant tests or reason step by step through the failure path; record which findings were verified and how.
5. Report: one entry per finding with file:line, summary, failure scenario and suggested fix; mark unverified items as such.

## CI integration (opt-in)
`templates/ai-pr-review.yml` is a starting GitHub Actions workflow. It is not installed by `aiwh init` copying alone: review the action version, inputs, permissions and secrets against the action's current documentation before committing it to `.github/workflows/`. Keep `permissions` minimal and never expose secrets to workflows triggered by forks.

Related open-source reviewers to evaluate: opencodereview and alibaba/open-code-review (see docs/REFERENCES.md).

## Deliverables
A ranked findings list, the verification evidence for each, and explicit notes on what was not reviewed.
