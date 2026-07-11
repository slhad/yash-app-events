# Architecture and dependency direction

The daemon is the only state-owning process. The GUI and CLI are protocol clients.

Dependencies flow inward from binaries to narrow libraries:

```text
daemon -> capture-pw -> capture
daemon -> engine -> capture, profile, vision
daemon -> output -> protocol
daemon -> profile, protocol, vision
cli/gui -> profile, protocol
```

`protocol`, `profile`, and `capture` do not depend on application binaries. OpenCV,
portal, PipeWire, GUI, and transport-specific types remain behind their owning crate
boundaries. A latest-frame slot connects capture to analysis; the GUI never owns a
capture session and never performs capture, detection, or file writes on its render
thread.

