### Changed (breaking API change): control characters, line breaks and bidi controls are refused in single-line text

Text sent to the API now follows one character policy ([GH#289]). Characters that do not belong in
CMDB data are refused with `400 VALIDATION_ERROR`, detail code `invalid_character`, and a message that
names the character. Nothing is stripped or changed silently, so the audit trail records exactly what
was sent.

- **Refused everywhere** (values, object keys and search terms): the control characters U+0001–U+0008,
  U+000B, U+000C, U+000E–U+001F, DEL (U+007F) and U+0080–U+009F. They act as terminal escape codes
  in logs and exports.
- **Allowed only in multiline fields**: TAB, line breaks (CR, LF, U+2028, U+2029) and the
  bidirectional embedding, override and isolate controls U+202A–U+202E and U+2066–U+2069, which can
  make displayed text read differently from what is stored ("Trojan Source"). Multiline fields are
  descriptions, relationship notes, help texts, form notes, location addresses, CA certificates and
  text attributes marked `multiline`. The OpenAPI document marks them with `x-multiline: true`.
  Everything else, such as names, labels, keys and single-line text attributes, is one line.
- **Always allowed**: right-to-left text, the direction marks U+200E, U+200F and U+061C, and
  zero-width (non-)joiners and spaces, which right-to-left names and emoji need.
- Search terms may contain TAB, line breaks and bidi controls because they are not stored. Passwords,
  tokens and client secrets are taken as typed, apart from the characters refused everywhere.

Stored data is not changed, and the check applies to what is written: the web UI sends only changed
CI attributes, so a stored value does not block editing other attributes.

**Upgrade:** scripts or integrations that send tabs or line breaks in names, labels or single-line text
attributes now get a 400. Configuration files exported earlier fail to import if such a field contains
one. Remove the character, or mark the text attribute `multiline`, and send it again. An account
whose password (local or in the directory) contains one of the characters refused everywhere can no
longer sign in; an administrator resets the password.

[GH#289]: https://github.com/Shadoukita/ShadouCMDB/issues/289
