# SQLite Registry In One Process (v1)

The registry is one SQLite database (WAL mode) owned by one finitesitesd
process; the engine serializes access behind a mutex. Schema and record
shapes stay portable (TEXT ids, INTEGER unix seconds, no SQLite-isms in
the store API) so a Postgres port stays mechanical if multi-process ever
demands it.

SQLite-on-the-host also lines up with the production backup story:
Litestream replicates the registry the same way it will replicate tier-2
tenant databases.

**Considered Options**

- Postgres from day one (like finite Core): operationally heavier than the
  one-box v1 needs, and the prototype's schema ports either way.
- SQLite + WAL + Litestream seam: smallest honest footprint; chosen.
