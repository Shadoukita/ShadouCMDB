### Changed (breaking API change): usage counts can be `null` (withheld)

`GET …/{id}/usage` no longer counts CIs of every CI type for every caller, as [GH#123] documented. A count over
CIs (CIs of a type, stored values of a field or lookup value, relationships) is told only when the caller may view
every CI type it can include. Otherwise the usage entry has `count: null` and the new field `withheld: true`
(`false` otherwise), and the entry is left out of the `details[]` of a `409 IN_USE` refusal, whose message then
says the row is still in use with the details withheld. A refused retire of a lookup value names the blocking
values without their counts. `inUse` and every refusal are still decided on all counts ([GH#265]).

**Upgrade:** API clients that read `count` from a usage report must accept `null`, and should not parse counts
out of `409 IN_USE` messages.

[GH#123]: https://github.com/Shadoukita/ShadouCMDB/issues/123
[GH#265]: https://github.com/Shadoukita/ShadouCMDB/issues/265
