### Changed: Import job pages show the check result and error report while import is off

While bulk import is turned off, an import's page (`/imports/{id}`) still shows the notice that
the import cannot continue, and now also shows the job's check result read-only: the counts, the
row problems with their filters, and **Download error report**. A finished import keeps its result
and its error report too. No step action is offered: Import, Back to mapping, Upload a corrected
file, Check again and Stop stay hidden, and stopping or deleting the import remains under
**Imports**. Links inside notices are now underlined, so they can be told apart from the text
around them without relying on colour.
