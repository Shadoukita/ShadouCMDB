### Changed: Dashboard layout

The **Dashboard** has a new layout.

- **Stat tiles** at the top show the number of configuration items and how many classes hold them, the changes in the last 7 days (with the number created and the number of status changes), and the number of business services. You only see the changes tile with the **audit.view** permission and the business services tile when you may view business services. A figure that cannot be loaded is left out instead of being shown as 0.
- The built-in dashboard and a dashboard set up under **Administration › Customization › Dashboard** now use the same widget grid. The built-in count widgets are titled **CIs by class** and **CIs by** followed by the status list's name, the same as the widget defaults in Customization.
- **Count widgets** now have column headers (class or value, CIs and share). Each row shows its share of all CIs as a bar and as a percentage. The **New** link on each class row is now a **+** button labelled "New ‹class›".
- A saved-search widget shows its number of matches as a count badge next to its title.
- When a count widget refers to a lookup list that no longer exists, the message now appears inside the widget.
- When you switch to custom widgets in Customization, the starting **By status** widget no longer stores an English title, so its title follows the user's language.
