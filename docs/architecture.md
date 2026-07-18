# Architecture and dependency direction

The daemon is the only state-owning process. The GUI and CLI are protocol clients.

Dependencies flow inward from binaries to narrow libraries:

```text
daemon -> capture-pw -> capture
daemon -> catalog -> profile
daemon -> engine -> capture, profile, vision
daemon -> output -> protocol
daemon -> profile, protocol, vision
cli/gui -> profile, protocol
```

`protocol`, `profile`, `catalog`, and `capture` do not depend on application binaries. The
catalog crate owns remote catalog schemas, validation, bounded downloads, cache integrity,
and publication tooling; only the daemon invokes its network/cache service. OpenCV,
portal, PipeWire, GUI, and transport-specific types remain behind their owning crate
boundaries. A latest-frame slot connects capture to analysis; the GUI never owns a
capture session and never performs capture, detection, or file writes on its render
thread.
