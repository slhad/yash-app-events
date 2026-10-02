# Profile collections

A schema-1 routing manifest names a router and up to 32 members. Its pinned IDs,
revisions, game, and router scene/overlay routes remain the source of membership.
The router may itself be a fallback member in older manifests; its document appears
once in discovery and packaging.

The daemon discovers `profiles/*.profile-bundle.json` and
`bundles/<router-uuid>/bundle.json` under its XDG data root. The GUI groups exact IDs,
keeps other profiles for that game separately, and forces matching groups open during
search. Missing documents, stale pins, and invalid manifests are visible diagnostics.
Discovery never repairs, removes, or activates a profile.

## Shared commands

```bash
yash-eventsctl --json profile bundle list --limit 16
yash-eventsctl --json profile bundle get <router-uuid>
yash-eventsctl --json profile bundle export <router-uuid> /path/to/collection.hudbundle
yash-eventsctl --json profile bundle import /path/to/collection.hudbundle
yash-eventsctl --json profile bundle validate /path/to/collection.profile-bundle.json
```

Validate checks only the routing document offline. Export validates all referenced
profiles and declared assets through the daemon. Import installs inactive and rejects
existing live profile or collection IDs. Retained lineage from trashed profiles rebases
the imported documents and routing pins to at least the next known revisions.

## Portable format

The `.hudbundle` ZIP contains `manifest.json`, `bundle.json`, and one
`profiles/<uuid>.hudprofile` for each unique router/member ID. The outer manifest
has schema 1, `router_profile_id`, and a `files` inventory with relative path, byte
size, and SHA-256. Each nested archive uses the existing profile format and includes
declared detector assets and inert output recipes. Tokens, machine capture bindings,
output routes, drafts, and revision history are excluded.

Outer and expanded collection inventories have a 256 MiB aggregate limit.
Each nested profile retains its 128 MiB expansion limit. Individual outer
files and nested files are limited to 32 MiB. The expanded profiles have a combined
2,048-file limit. Outer metadata is bounded to 256 KiB and at most 35 entries.
The importer rejects unsafe paths, links, duplicate/undeclared/missing entries,
unsupported schemas, integrity errors, identity/pin/game/layout mismatches, and
unknown router route IDs before publication.

Installation stages under `bundles/.import-<uuid>`, validates every nested profile,
checks collisions and lineage, then renames the complete directory to
`bundles/<router-uuid>`. On failure it removes staging. Revision high-water marks
may advance after a write failure and never regress. Members remain individually
accessible through the same profile API. Editing or trashing a member makes its
pinned collection invalid; the GUI displays that error instead of silently routing
to another copy. Updating pins currently requires editing the routing manifest.

## Catalog sources

Catalog metadata defaults to `kind: profile`. A collection source sets `kind: bundle`
and stores `bundle.json` plus `profiles/<uuid>/profile.json`, declared JSON detector
assets, and optional `output-recipes/*.json` under its version directory. Validation
audits every file, checks all references, and compares the union of detector types and
recipe names to the declarations. `media_free: true` prohibits detector assets.
Non-media-free sources can include explicitly declared JSON templates/masks, audited
for secrets and machine paths. Binary media and model sources are not admitted by
this catalog-source workflow. Local portable bundles support the existing profile
asset formats independently.

One source yields one immutable
`bundle--<game-slug>--<profile-slug>--v<version>.hudbundle` package and one catalog entry.
The entry adds `kind: bundle` and `bundle_profile_ids`, ordered router first, with all
unique contained IDs. The installer verifies the downloaded archive's actual IDs,
game, and maximum portable document schema against reviewed metadata before installation.
Schema-1 archives retain their original metadata and bytes; in-memory migration to
schema 2 does not change the declared package schema.
Existing single-profile source layouts, filenames, and serialized metadata remain
compatible. Catalog sources and publication still require their normal human review;
local collection export does not publish anything.
