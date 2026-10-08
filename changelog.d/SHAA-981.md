### Fixed: Stopping a bulk import reports the rows it actually imported

Stopping a running import commit set the job to *cancelled* at once, while the batch of up to 500
rows in progress was still being written. The import wizard stopped refreshing at *cancelled* and
showed up to one batch fewer created or updated rows than the import wrote, until the page was
reloaded. The data and the audit log were correct. ([GH#359])

A stop of a running commit is now a request: `POST /api/v1/imports/{id}/cancel` returns the job
still `committing`, with the new field `cancelRequestedAt` set, and the job ends as `cancelled`
with its final counts once the current batch is written. A queued commit, an analysis and a dry
run still stop at once. API clients that expected `cancelled` in the response to a stop of a
running commit should poll the job until it ends.

**Upgrade:** migration `0035` adds a column to the import job table; to apply it, run `shadoucmdb
migrate` before starting the new release (`serve` does not migrate).

[GH#359]: https://github.com/Shadoukita/ShadouCMDB/issues/359
