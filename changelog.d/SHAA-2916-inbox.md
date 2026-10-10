### Added: approvals inbox and approval delegations in the web UI

**Approvals** in the navigation shows how many requests wait for your decision. It lists them with the
earliest due first, so overdue requests lead, next to the requests you made and those you decided. Filters
for status and overdue stay in the URL. The count includes only requests on CI types you may view, so it
matches what you can open.

**My delegations** in the user menu lists the delegations of your approvals and those that let you decide for
someone. You can delegate your approvals for a time window of at most 90 days, and revoke a delegation.
Administrators with user management set up or end delegations for any user under **Administration ›
Approval delegations**. Choosing a delegate yourself needs the user lookup (edit on business services or user
management); without it, the dialog points you to an administrator. No API or database change.
