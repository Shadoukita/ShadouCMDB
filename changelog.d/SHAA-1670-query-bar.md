### Added: Filter the inventory by typing `key:value` in the search box

The search box over the inventory is now a query bar. Plain words still search labels, idents and attribute values as before. A `key:value` token filters the list, using the same filters as the controls next to it:

- `class:server,vm`: classes, by their key.
- `<lookup list>:<value>`, for example `environment:prod,test`: lookup values. Values of one list match any of them, and several lists must all match.
- `criticality:high`, `validity:all` or `validity:inactive`, `deleted:include` or `deleted:only`, `ip:10.0.0.0/8`, `layout:own` or `layout:default`, `template:<key>`.

Keys and values are suggested as you type. The bar and the filter controls stay in step: choosing a filter adds its token, and deleting a token removes the filter. The URL holds the same parameters as before, so existing bookmarks and saved views keep working. A key or value that does not exist, or an excluding `-key:value`, is reported under the bar, and the list keeps its last valid filters. To search for text that contains a colon, put it in quotes.
