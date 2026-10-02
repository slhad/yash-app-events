# Architecture and dependency direction

Routing collections keep existing profile boundaries. The profile store discovers
legacy manifests and atomically installs complete nested collections under
`bundles/<router-id>/`. Profile APIs locate standalone or managed member documents;
the daemon remains their sole writer. The GUI worker fetches bounded collection
pages alongside profile metadata, and its render thread groups exact IDs only.
The catalog treats one collection as one archive/entry and validates every contained
identity before admitting it. See [collection packaging](profile-bundles.md).

The daemon is the only state-owning process. The GUI and CLI are protocol clients.

Profile discovery uses bounded `profile.list_summaries` pages with validated metadata
and independent rejected-entry diagnostics. The GUI worker collects pages, then fetches
only the selected document through `profile.get`; the render thread keeps summaries
and the selected committed/draft document instead of every full profile.
The optional [Omarchy Quickshell widget](quickshell.md) is another read-only
protocol-1 client. It polls status and caches the active profile name over a persistent
Unix socket; it performs no capture, detector work, persistence, or route delivery.

Dependencies flow inward from binaries to narrow libraries:

```text
daemon -> capture-pw -> capture
daemon -> catalog -> profile, output
daemon -> engine -> capture, profile, vision
daemon -> output -> protocol
daemon -> profile, protocol, vision
profile -> output -> protocol
cli -> engine, profile, protocol
gui -> profile, protocol
```

`protocol`, `profile`, `catalog`, and `capture` do not depend on application binaries. The
catalog crate owns remote catalog schemas, validation, bounded downloads, cache integrity,
and publication tooling; only the daemon invokes its network/cache service. OpenCV,
portal, PipeWire, GUI, and transport-specific types remain behind their owning crate
boundaries. A latest-frame slot connects capture to analysis; the GUI never owns a
capture session and never performs capture, detection, or file writes on its render
thread.
