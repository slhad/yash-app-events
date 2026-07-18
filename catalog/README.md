# Profile Catalog Sources

This directory contains reviewable source documents for packages published to the single
GitHub release tagged `profiles`. Generated `.hudprofile` archives and catalog indexes are
never committed.

Each immutable profile version lives below
`profiles/<game-slug>/<profile-slug>/v<version>/`. Publication validates the profile, inert
output recipes, declared compatibility, exact file inventory, and media-free claim before
building the archive. Merging a validated source change publishes missing immutable packages
first and a new append-only `catalog-v1-rNNNNNN.json` revision last.

Do not add gameplay captures, replay suites, thumbnails, capture bindings, restore tokens,
machine-local output routes, absolute paths, or generated packages here.
