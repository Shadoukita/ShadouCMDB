### Added: Business service members in the web UI, and "Part of business services" on every CI

A business service's detail page has a **Members** tab: search, Class and Kind filters, sorting and
paging are kept in the URL (`?tab=members&mq=&mclass=&mkind=&msort=&mpage=`), so a view can be
bookmarked and shared. Operators who may edit business services can select members and remove
them (the confirmation names the number; the member CIs are not deleted), and add members through
an **Add members** dialog that searches the inventory on the server, marks CIs that already are
members, keeps a selection across pages (up to 500 at a time) and shows the reason for each CI the
server refuses (a service cannot include itself or a service that already includes it, the nesting
depth and the member limit). **Export members (CSV)** downloads the listed members and is recorded
in the audit log.

Every CI's Overview shows the business services it is part of, directly or through nested services,
with their criticality and technical owners. The Impact tab's list view pins the affected business
services above the result, most critical first; when the result is truncated or stops at the
depth, the section says it is incomplete.

Users whose permission profile hides some classes see only the members they may view, with the note
"Members of classes you are not allowed to view are not listed."
