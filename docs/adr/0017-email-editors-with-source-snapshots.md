# Email Editors With Source Snapshots, Not Git Hosting

Email-keyed multi-editor publishing is modeled as registry authorization plus
optional source snapshots attached to immutable versions. Editors prove control
of an email address with a local Email Key, and a publish may carry a separate
`tar.gz` Source Snapshot so another editor can pull editable project files
without Finite Sites becoming a git host.

**Considered Options**

- Minimal git hosting: familiar source-sharing semantics, but commits, refs,
  merge conflicts, access control, and repository storage become a second
  product beside publishing.
- Only published artifacts: keeps the current model, but editors cannot
  reliably get the project source that produced a site.
- Version-attached source snapshots: enough for handoff and replayable audit,
  deliberately easy to remove if a better source-sharing system replaces it.
