### Added: Approvals inbox, request history and the awaiting-approval filter

- `GET /api/v1/workflow-approval-requests` lists approval requests, paginated. `view` selects which
  ones:
  - `actionable` (the default) is the approvals inbox: the pending requests whose active step you
    may decide now. The rules are the same as for a decision: you are an approver of the step, you
    are not the requester or the creator of the requesting token, you have not decided it yet, and
    the step's separation-of-duties and API token rules allow you.
  - `requested`: the requests you made.
  - `decided`: the requests you approved or rejected a step of.
  - `all`: every request.
- The list filters by `status`, `definitionKey`, `ciId`, `requestedBy` and `overdue`, and sorts by
  `dueAt` (the default, earliest first), `requestedAt` or `closedAt`. Requests on CIs of types you
  may not view are left out of the page and of the total.
- Incident runbook: to find the pending requests of an account that was disabled or compromised,
  list `view=all&status=pending&requestedBy=<user id>`, then cancel each one with `POST
  /api/v1/workflow-approval-requests/{id}/cancel`.
- `GET /api/v1/workflow-instances/{id}/approval-requests` lists the approval requests of one
  instance, newest first, whatever became of them.
- `POST /api/v1/workflow-approval-requests/{id}/refresh` (`workflows.manage`) works out again who
  may decide the active step of a pending request. It uses the workflow's current approver
  assignments and the CI's current values. Decisions already cast stand.
- `GET /api/v1/workflow-instances` takes `awaitingApproval=true|false`. The per-state summary
  (`GET /api/v1/workflow-instances/summary`) gains `awaitingApproval`: how many of the instances
  in each state are waiting for approval.
- With `excludeActorsOf`, everyone who approved a request for one of the listed transitions is
  now refused, not only the approver whose vote completed it (GH-635).
- **Upgrade:** migration 0053 adds two indexes on `workflow_approval_decisions`. The table is new
  in this release, so the migration is quick.
