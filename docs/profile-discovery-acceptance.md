# Profile discovery acceptance

Date: 2026-10-02. Scope: `SPEC-UI-002`, `SPEC-PROFILE-003/006`, and `SPEC-IPC-004`.

## Reproduction and native verification

An isolated labwc Wayland desktop, transported through loopback-only wayvnc at
1440×1000, ran the native egui GUI with a separate daemon socket and private XDG
data/config/state/cache roots. The test copied installed profile documents and revision
lineage, leaving the original profile store untouched. Machine capture bindings,
routes, game media, and automatic capture policies were not copied.

The library contained 146 profile directories: 145 canonical UUID entries and one
externally named backup. One canonical directory contained another profile's stable
ID. The installed GUI reproduced an empty profile sidebar because the strict legacy
`profile.list` failed. Direct get of the BlazBlue profile still succeeded.

The updated release GUI and daemon discovered 144 valid profiles and reported the
rejected identity as an unavailable profile. Metadata arrived in two pages: 127 valid
profiles plus one diagnostic, then 17 valid profiles. Compact CLI result bodies totaled
27,627 bytes. The metadata omitted full detector/scene/rule documents.

Native mouse/keyboard verification covered:

- BlazBlue Entropy Effect starter, revision 34, appears and opens with its detector
  hierarchy and normalized regions;
- searching for `blaz` leaves the BlazBlue entry visible;
- scrolling reaches profiles beyond the initially visible rows;
- expanding unavailable profiles discloses the rejected ID and identity-mismatch reason;
- refresh retains the selected profile and the warning;
- switching to another valid profile and back loads the matching editor document.

The malformed profile remained stored and rejected by direct get. No profile was
deleted, reimported, activated, or automatically repaired. This run did not test the
portal picker, capture, live detection, or output delivery; older capture evidence
retains its original date and scope.

The tested GUI, daemon, and CLI release binaries were then installed locally with
backups, and the user daemon was restarted while capture was stopped. The live store
returned the same 144 valid profiles and one diagnostic across two pages; BlazBlue
loaded at revision 34. Before/after SHA-256 checks confirmed all original committed
profile documents were unchanged, and the active profile ID was preserved.

## Automated verification

The workspace suite passed 208 tests, with two existing release benchmarks ignored.
Strict workspace Clippy, formatting, documentation generation, and README claims passed.

New regression coverage includes:

- store discovery isolates mismatched IDs, invalid JSON, and regressed revisions,
  advances over rejected entries, ignores external backup directories, and leaves
  rejected document bytes untouched;
- metadata count, text lengths, escaped serialized size, final cursor, and empty
  continuation remain bounded;
- real Unix-socket discovery returns metadata/diagnostics, validates page limits,
  preserves the strict legacy array API, and keeps direct invalid-profile get rejected;
- GUI discovery collects every page, retains diagnostics, filters names/game/UUIDs,
  loads only the selection, ignores stale selection replies, preserves unsaved drafts
  during refresh, retains the committed snapshot used by revert, and keeps sidebar
  width stable across 100 repaints with long profile labels;
- the CLI uses the same negotiated summary method.

Reproducible checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo doc --workspace --no-deps
bash scripts/check-readme-claims.sh
```

For native reproduction, use a private copy of a profile library with more than 128
canonical entries and one identity-mismatched document. Start the daemon with private
XDG roots and launch the GUI inside an isolated Wayland desktop with
`YASH_APP_EVENTS_SOCKET` pointing at that daemon. Compare discovery with
`yash-eventsctl --socket <private-socket> --json profile list-summaries`; follow
`next_after` with `--after` until null. See [GUI controls](gui.md) and
[protocol details](protocol-v1.md) for current behavior.
