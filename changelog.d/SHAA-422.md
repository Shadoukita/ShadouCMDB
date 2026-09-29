### Fixed: concurrent password resets of users who created tokens for each other no longer fail

Two administrators resetting, at the same time, the passwords of two users who had each created an
API token for the other (`PUT /api/v1/admin/users/{id}/password`) could make one reset fail with
`500 INTERNAL_ERROR` and `deadlock detected` in the server log ([#166]). The failed reset changed
nothing, so that user's password and tokens stayed as they were until it was retried. A reset now
locks all the tokens it revokes in one step, in a fixed order, and both resets succeed.

[#166]: https://github.com/Shadoukita/ShadouCMDB/issues/166
