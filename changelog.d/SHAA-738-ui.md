### Added: Claimed client address in the audit log

In Administration › Audit log, hovering over the record of a sign-in, MFA or API token entry now
shows the `claimedIpAddress` from [GH#282] as **Claimed address (unverified)**. This is the first
`X-Forwarded-For` address, which the client may have forged, and it appears only when it differs
from the verified address. MFA and API token entries now also show their verified address, their
peer address and the browser, as sign-in entries already did.

[GH#282]: https://github.com/Shadoukita/ShadouCMDB/issues/282
