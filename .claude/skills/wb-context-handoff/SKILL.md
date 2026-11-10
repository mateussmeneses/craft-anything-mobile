---
name: wb-context-handoff
description: Hand work to another agent, model or session with a self-contained plan, a recorded revision and explicit gaps
version: 1.1.0
---
# wb-context-handoff

## When to use
Changing agents or models, ending a session, or splitting work between a planning model and a cheaper executing model.

## Procedure
1. **Record the revision.** Note branch and commit. A handoff without a revision cannot be checked for drift. `aiwh resume .` records both.
2. **Write the plan as the product.** One task, exact paths, the verification command with its expected result, and stop conditions. It must work for a reader with no earlier conversation. `aiwh handoff plan .` assembles one from the documented queue.
3. **Split by cost.** Plan and review with the strongest model available; execute with a cheaper one. Only do this when the plan passes step 2.
4. **Claim before starting.** One owner per task: `aiwh handoff claim TASK --by NAME`; release it when done or abandoned.
5. **Check drift before executing.** `aiwh handoff check --plan FILE`. If the revision moved, stop and regenerate instead of adapting silently.
6. **State what is unknown.** List unverified decisions, missing test commands, truncated scans and open questions. A gap written down is cheaper than one rediscovered.
7. **Record rejections.** A finding judged wrong or out of scope goes in the state `rejections` list with the reason, so the next session does not raise it again.

## Rules
- Reference local files instead of copying transcripts. Never include secrets, credentials or personal browsing data.
- Decisions carry a source and a `last_verified` date; retract a decision with a reason instead of deleting it.

Verify with repository evidence, and record incomplete checks explicitly.
