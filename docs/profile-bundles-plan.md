# Profile bundle implementation plan

Status: completed and verified, 2026-10-02.

This is the dated implementation sequence. Current format/commands are in
[collection packaging](profile-bundles.md), and verification results are in
[collection acceptance](profile-bundles-acceptance.md).

The existing routing manifest names one router and up to 32 specialized profiles.
It currently serves screenshot analysis only. This change makes that collection
discoverable and portable as one library/catalog entry, without combining detector
documents or introducing bundle-wide live capture.

1. Add `SPEC-PROFILE-013` for discovery, reference validation, portable packaging,
   atomic collection installation, and backward compatibility.
2. Discover existing `*.profile-bundle.json` manifests and daemon-installed bundles
   through bounded shared RPC/CLI methods. Keep invalid-bundle diagnostics visible.
3. Group the router and exact manifest members beneath one expandable GUI entry.
   Keep unrelated copies for that game under a collapsed Other installed profiles
   section. Searching can reveal them; grouping never deletes or rewrites profiles.
4. Export one integrity-checked archive containing the manifest and existing hardened
   portable profile archives. Stage and validate every profile before publishing an
   installed collection with one directory rename. Preserve revision high-water marks
   and update manifest pins when import rebases a retained identity.
5. Extend catalog source validation/build/download/install with additive bundle
   metadata while retaining single-profile catalog and archive compatibility. One
   bundle produces one catalog package and one entry; installation stays inactive.
6. Verify store, archive safety, protocol/CLI, catalog packaging/install, and GUI
   grouping/search/selection tests, then test the current Queen Blade bundle in an
   isolated native Wayland desktop. Build a local Queen Blade bundle package and
   install verified binaries locally. Publishing gameplay-derived assets or catalog
   sources is outside this change.

Acceptance requires canonical formatting, strict workspace Clippy, all-feature tests,
documentation generation, valid JSON/relative links/README claims, archive failure
tests, and native GUI evidence. Current guides and evidence will be updated with the
implementation; older acceptance reports retain their dates and scope.

Real-data verification found 141.19 MiB of declared Queen Blade payloads. Collection
expansion therefore uses a 256 MiB aggregate allowance, preserving the 128 MiB
per-profile limit. Export/import use blocking workers and longer client deadlines;
ordinary control clients remain responsive during packaging.
