# Accessibility

ShadouCMDB's web UI targets **WCAG 2.1 level AA**. Every pull request checks the main screens automatically with
[axe-core](https://github.com/dequelabs/axe-core) in the Playwright end-to-end suite
([`frontend/e2e/a11y.spec.ts`](../frontend/e2e/a11y.spec.ts)).

## What is checked

axe runs with the rule tags `wcag2a`, `wcag2aa`, `wcag21a` and `wcag21aa` on the page as an operator sees it,
against a real API with the demo inventory:

| Screen | State checked |
| --- | --- |
| Sign-in | empty form; refused sign-in with its error |
| Inventory list | the CI table with filters, sorting and paging |
| CI detail page | grid layout with sections and the relationships panel; the delete confirmation dialog |
| CI edit form | the form generated from the class's attributes |
| Class editor | a class with its attributes; the *Add attribute* dialog |
| Users and permission profiles | the user list, the new-user form, the profile list and a profile's permission matrix |
| My account | password change and two-factor panels |
| Two-factor enrolment | the step with the QR code and setup key |

A **critical** or **serious** violation fails the test and the pull request. **Moderate** and **minor** findings are
printed in the test output and listed on the test in the HTML report, without failing it. Each checked state
attaches `axe-<screen>.json` to the report with the violations and the checks axe could not decide on its own
(for example text contrast over overlapping elements), which need a look by a person.

No rule is turned off on any screen. A rule may only be turned off for one screen, with a comment in the spec
saying why, and listed here.

## What is not checked automatically

Automated checks find roughly a third to a half of WCAG issues. They do not judge whether a label or error text
makes sense, whether the reading and focus order is logical across a whole task, or how the UI works with a screen
reader or at 400 % zoom. Those need a manual review; report what you find as a bug.

Screens outside the table above (for example the dashboard, search, the layout designer and the other
administration pages) share the same components and styles but are not checked one by one yet.

## Run it

```sh
API_PROXY_TARGET=http://<api-host>:3000 npx playwright test a11y     # in frontend/, as in its README
```
