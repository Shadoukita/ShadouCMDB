### Changed: CI form and detail page start with the General section

The CI form and detail page show the same core for every class ([CI core]): a **General** section
with the ident, valid from, valid until and the class's fields without a form section, then the class's
own sections. The former fixed fields and the "Other" section are gone; a field without a section sits
on General. A new CI's valid from is pre-filled with the current local time, and double-clicking any
date or date-and-time input fills in the current local date and time. Only administrators can edit the
ident; for everyone else it is read-only. Lists and the detail page say when an active CI deactivates
("deactivates on …"); lists show active CIs by default, with **Validity › Show inactive** for the rest.
The class page's **Title attribute** picks the field that labels the class's CIs. Saved layouts keep
working: fields no panel places now fall into General and their sections.

[CI core]: docs/data-model.md#the-ci-core-ident-validity-and-label
