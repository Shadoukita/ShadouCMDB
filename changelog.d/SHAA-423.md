### Fixed: a class list with a saved default sort is queried once, not twice

Opening the inventory of a class whose list view sets a default sort or page size (Customization ›
List views) now waits for that list view before loading the page, so the configuration-item query
and its total count run once with the saved sort instead of first with the label sort. This matters
on large inventories and for attribute sorts. ([GH#167](https://github.com/Shadoukita/ShadouCMDB/issues/167))
