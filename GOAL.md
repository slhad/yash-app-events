# Completed release goal

Status: complete since 2026-07-11.

The original goal was to deliver a usable Linux and Wayland application that captures a
game window through the desktop portal, detects configured HUD state, emits temporal events,
stores portable profiles, supports replay, and exposes the same controls through the GUI,
CLI, and local JSON-RPC protocol.

That release goal is complete. Later work added OCR, image classification, output routing,
the public profile catalog, scene-aware analysis, profile bundles, transition context,
capacity diagnostics, and the 2026-09-20/21 performance audits, including the final
one-shot, live-scheduling, output, and GUI repaint follow-up.

Current authority is deliberately small:

1. `SPECS.md` defines required behavior and records verification evidence.
2. `PLAN.md` lists current work and the gates for accepting a change.
3. `ROADMAP.md` lists ideas that have not been scheduled.
4. `README.md` describes behavior users can rely on.
5. `AGENTS.md` defines repository working rules.

Git history preserves the original implementation contract and detailed phase plans. Do not
reopen this completed goal to record ordinary maintenance work. Add a new specification and a
current plan entry when the product scope changes.
