# Profile Catalog Sources

This directory contains reviewable source documents for packages published to the single
GitHub release tagged `profiles`. Generated `.hudprofile` and `.hudbundle` archives and catalog indexes are
never committed.

Each immutable profile version lives below
`profiles/<game-slug>/<profile-slug>/v<version>/`. Publication validates the profile, inert
output recipes, declared compatibility, exact file inventory, and media-free claim before
building the archive. Merging a validated source change publishes missing immutable packages
first and a new append-only `catalog-v1-rNNNNNN.json` revision last.

Do not add gameplay captures, replay suites, thumbnails, capture bindings, restore tokens,
machine-local output routes, absolute paths, or generated packages here.

Collection sources set `kind: bundle` and include `bundle.json` plus a `profiles/<uuid>/`
subtree for the router and each unique member. One collection builds one `.hudbundle`
and one catalog entry with all contained IDs. Validation audits the entire inventory,
references, detector/recipe declarations, and media-free claim. Only explicitly declared
JSON detector assets are accepted in non-media-free sources; binary images/models
remain outside this source workflow. See [collection format and commands](../docs/profile-bundles.md).
