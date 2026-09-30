### Changed: Configuration files carry saved import mappings (format version 4)

`GET /api/v1/admin/config/export` now writes `formatVersion: 4` with a new optional section
`importMappings: [{name, description, classKey, definition}]`, the saved import mappings.

- **Rights:** the section is exported only to callers who also hold `cis.import`, and only with
  the mappings of classes they can view. Importing a file whose `importMappings` section is not
  empty also needs `cis.import` (`403` otherwise, dry run included).
- **Merge:** mappings are matched by class key and name, ignoring case. A missing one is created;
  an existing one gets the file's description and definition and keeps its name. Nothing is
  deleted. Class, field and relationship-type keys the target lacks are reported as warnings,
  and the mapping is still saved. A mapping of a class the importing user cannot view is skipped
  with a warning. The 500-mapping limit applies.
- **Audit:** each created or changed mapping is audited as an `import_mappings` change with
  actor type `import`.
- **Not exported:** the import switch, import jobs, uploaded files and their problems.
- **Compatibility:** files of versions 1 to 3 still import. Scripts that check
  `formatVersion === 3` on an export must accept 4. A version-4 file is refused by earlier
  releases with the usual "unsupported format version" error.
