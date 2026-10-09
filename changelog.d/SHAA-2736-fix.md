### Fixed: Notifications of a workflow that has not been published yet

Reading or saving the notifications of a workflow that has only a draft (no published version)
failed with an internal server error. It now works, and the lint warns that no current version has
the transition yet.
