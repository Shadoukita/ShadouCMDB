### Added: Business services

A *business service* groups the CIs that together deliver one service to the business, for example
web servers, application and database for "Online shop". It records a technical owner and a
business owner (users or user groups) and a criticality, using the same *Criticality* field as
every other CI. Open *Business services* in the main menu to list services by criticality and
owner, including "My services" and services without an owner or with a disabled owner. A
service's *Members* tab adds and removes CIs, and a service can include other services. Impact
analysis follows membership: when a member is affected, its services are affected, and the
*Impact* tab of every CI lists the affected business services first. Every CI shows which business
services it is part of.

Business services are CIs of the new built-in class *Business service*. The class cannot be
deleted, but you can rename it and add your own attributes. Who may see and edit services is
controlled like any class, in *Administration → Access → Profiles*. The new *Administration →
Access → Groups* page (right *Manage users*) manages user groups for ownership. Membership changes
and owner changes appear in the service's history. Member CSV exports are recorded in the audit
log. Users only see members of classes they are allowed to view.

**Upgrade:** No data is changed. If your installation has the starter class *Service* (key
`service`), it becomes the *Business service* class: your existing services appear under
*Business services*, with no members and no owners yet. Their *depends on* relationships are
unchanged and still count as dependencies in impact analysis. Otherwise a new, empty *Business
service* class is created. **Check your permission profiles:** profiles that grant *all classes*
can see business services straight away. Profiles with individual class grants need a grant for
*Business service* before their users can see services. The starter attribute *Service tier* is
not converted. To use *Criticality* instead, set it with bulk import, then remove the attribute if
you no longer need it. The configuration export format is now version 5; older exports still
import. New settings `BUSINESS_SERVICE_MAX_MEMBERS` (default 5 000) and
`BUSINESS_SERVICE_MAX_NESTING` (default 5) are described in the deployment guide.

**API:** `GET /api/v1/business-services` (list with member counts and owners), `GET
/api/v1/business-services/{id}`, `GET|POST /api/v1/business-services/{id}/members`, `POST
…/members/remove`, `DELETE …/members/{ciId}`, `GET …/members/export` (CSV), `PUT …/owners`, `GET
/api/v1/configuration-items/{id}/business-services`, `GET /api/v1/principals` (owner picker) and
`GET /api/v1/settings/business-services`. CI classes and relationship types report `systemRole`.
Membership is written only through the business service endpoints: `POST`, `PATCH` and `DELETE
/api/v1/relationships` refuse the member type with 400 `VALIDATION_ERROR`
(`system_relationship_type`). Deleting, archiving, purging, making abstract or re-parenting the
business service class answers 409 `IN_USE` (`system_class`); creating a subclass of it answers 400
`VALIDATION_ERROR` on `parentId`. The member type's key, direction, impact direction and active flag
answer 400, and deleting it 409 `IN_USE` (`system_relationship_type`).
