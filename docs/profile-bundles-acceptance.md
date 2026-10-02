# Profile collection acceptance

Date: 2026-10-02. Scope: `SPEC-PROFILE-013`, with shared protocol, archive,
profile-store, catalog, and native GUI compatibility checks.

## Automated verification

The canonical gates passed:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --no-deps
bash scripts/check-readme-claims.sh
```

The workspace reported 223 passed tests and two previously ignored tests, with no
failures. All 379 repository JSON artifacts parsed, affected/current Markdown links
resolved, and `git diff --check` passed.

Collection tests cover legacy/managed discovery and pagination, stale pins, missing
identities/assets, revision/game/layout/router-route mismatches, exact grouping and
search, round trips, tampering, traversal, outer/nested links, collisions, aggregate
limits, failed-install cleanup, retained-lineage rebasing, editable members,
trash/restore/history, and the 128-collection admission bound. Existing routing tests
retain the legacy router-as-fallback behavior; its document is packaged once.

A real Unix-socket test imports, lists, gets, exports, and rejects a repeated import.
It verifies inactive installation and limit validation. CLI parsing covers each new
shared operation. A local HTTP catalog test downloads an integrity-checked bundle,
installs one collection, verifies every reviewed ID/game/schema, and leaves capture
and activation unset. Synthetic catalog sources build one reproducible package/entry
and reject extra files.

The existing BlazBlue Stage Tracker source validates and rebuilds to its original
published SHA-256:

`07efe534ec49d723cd4ce06fa6ea0becc085ee4b71ba63288e97ec9f612c05b4`

Schema-1 portable bytes and catalog metadata remain unchanged by the app's schema-2
in-memory migration. A dedicated admission test rejects incorrect schema metadata.

## Native Wayland verification

An isolated 1440×1000 labwc/wayvnc desktop ran the release GUI against private XDG
roots copied from the installed library. Input used screenshot-based mouse/keyboard
commands. The user's physical desktop and original profile files were not edited.

The GUI showed one Queen Blade collection with its router and ten exact members.
BlazBlue remained independently visible and opened its editor. The 132 other valid
Queen Blade profiles stayed under a collapsed Other installed profiles group. The
existing invalid profile stayed under Unavailable profiles.

Expanding the collection exposed the router/member rows. Selecting Guild Battle
opened that exact document. Searching Server Error forced the collection open and
showed the matching member; searching portrait revealed the older portrait profile
in Other installed profiles. A temporary stale router pin in the private manifest
produced an unavailable header, a yellow mismatch diagnostic, and disabled export,
while BlazBlue remained usable. Restoring the manifest and refreshing cleared it.
The repeated-repaint test verifies sidebar width remains bounded with long labels.

Native GUI export and CLI export produced identical collection bytes. Status remained
responsive during packaging; a separate CLI status request completed in 3 ms during
the real export run. Bundle packaging uses blocking workers and longer client deadlines.

Private screenshot evidence was retained locally under the application's state
acceptance directory, including collapsed, expanded, selected-member, search,
older-profile, and stale-pin views. Screenshots contain local profile information and
are not committed or published. Test desktops and private copied libraries were
removed after verification.

## Real collection round trip and local installation

The existing Queen Blade bundle exported successfully with:

- 11 nested profiles and 13 outer ZIP entries, including the two metadata documents;
- 989 expanded payload files, totaling 148,045,623 bytes;
- 26,114,701 compressed bytes;
- SHA-256 `9ff9e1cd9944d9bf725137e87305effd5426dc5527ba64d9a1146af92c367cde`.

The measured 141.19 MiB payload exceeded the initial 128 MiB collection allowance.
The implemented aggregate collection limit is 256 MiB; each nested profile retains
128 MiB, each file 32 MiB, and the collection 2,048 expanded payload files.

A fresh private daemon installed one collection containing all 11 profiles. Every
one of its 989 payload files matched its source bytes. Discovery reported no reference
errors, capture stayed stopped, and no profile became active.

Release GUI/daemon/CLI binaries were installed atomically with local binary backups.
The stopped user daemon was restarted and its active profile remained unchanged.
Live discovery returned the Queen Blade collection with all 11 exact IDs and no errors.
The local package was retained as
`$XDG_DATA_HOME/yash-app-events/bundle-exports/queen-blade-local.hudbundle`, mode 0600.
The original profile tree's full file inventory and SHA-256 hashes were unchanged.

This change did not publish Queen Blade or remove older copies. Bundles retain
screenshot-analysis routing; live capture still activates individual profiles.
Changing an installed member revision makes its pins stale. Updating a manifest's
pins currently requires editing its JSON; automatic repinning is outside this change.
