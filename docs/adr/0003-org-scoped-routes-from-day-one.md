# 0003 Org-scoped routes and `org_id` everywhere from the first release

**Context.** Multi-tenancy is a requirement. Adding it after the API and the metadata schema exist would break both.
**Decision.** Every route is `/api/v1/orgs/{org}/...`; the `default` org exists out of the box; every metadata row has `org_id`; builders take the org as a bound argument; the database is resolved from config/metadata, never from request text.
**Consequences.** Single-tenant installs pay one path segment. Isolation can be tested exhaustively (route × caller matrix).
