# Scene-Aware Game Understanding and Screenshot Analysis Plan

## 1. Motivation

`yash-app-events` currently evaluates every enabled detector in one flat active profile. This works for a small, stable HUD, but it does not efficiently or safely represent games with many visually distinct screens, menus, dialogs, overlays, and gameplay states.

The system must be able to answer two separate questions for every analyzed frame:

1. **Where is the player?** — gameplay, home menu, inventory, summon screen, results screen, loading screen, modal dialog, and so on.
2. **What is visible and actionable there?** — observed values, labels, buttons, cards, close controls, and other named UI elements with their geometry.

This is a generic game-understanding capability, not a game-specific exception. It should improve efficiency, reduce false positives, make profiles maintainable, and give external computer-use systems enough structured spatial information to reason about interaction without making `yash-app-events` itself generate input.

## 2. Product outcome

Given a live frame, replay frame, or explicit PNG screenshot, the daemon shall:

- identify one active base scene or report `unknown`/`ambiguous`;
- identify zero or more active overlays;
- run only global detectors, scene-recognition anchors, and detectors belonging to the active scene/overlays;
- return typed observations with stable IDs and names;
- return visible named UI elements with normalized and actual-frame pixel geometry;
- return explicitly configured safe interaction points for actionable elements;
- preserve deterministic replay behavior and bounded processing;
- expose the same result through daemon state, JSON-RPC, CLI, and GUI;
- never click, type, or otherwise automate game input.

The initial consumer is the `wayland-virtual-desktop` skill, which will capture an isolated desktop screenshot and ask Yash to analyze it before the AI decides whether to issue a separate virtual mouse or keyboard action.

## 3. Required JSON contract

Every screenshot-analysis result must identify the coordinate space before describing elements. Pixel coordinates use the analyzed frame after decoding, with origin `(0, 0)` at the top-left, positive X to the right, and positive Y downward. Rectangles use inclusive origin and width/height extents; consumers must treat the right and bottom bounds as exclusive.

Implementation note: the verified response keeps detector-backed observation elements in `elements` and declarative action geometry in `interaction_targets`. Both arrays use the same `id`, `name`, `role`, `visibility`, `region.normalized`, and `region.pixels` vocabulary. Keeping them separate prevents an OCR/template crop from being mistaken for a safe click target and avoids duplicating geometry in the JSON-first payload.

Example response:

```json
{
  "schema": 1,
  "profile_id": "8f9b7bd6-6f55-48f5-9aa5-f2d72b541d83",
  "profile_revision": 12,
  "frame": {
    "width": 558,
    "height": 992,
    "pixel_format": "rgba8",
    "coordinate_system": "top_left_origin"
  },
  "scene": {
    "id": "52aca753-64d9-464d-8bc9-dd2f4db15569",
    "name": "limited_summon",
    "status": "recognized",
    "confidence": 0.98,
    "candidates": [
      {
        "id": "52aca753-64d9-464d-8bc9-dd2f4db15569",
        "name": "limited_summon",
        "confidence": 0.98
      }
    ]
  },
  "overlays": [
    {
      "id": "27ae9ce4-b7a6-4fe7-b6fb-dde54a979a31",
      "name": "summon_confirmation",
      "confidence": 0.96
    }
  ],
  "elements": [
    {
      "id": "42ba6fdd-5912-4014-8450-4ce4b5c02db5",
      "name": "daily_remaining",
      "role": "observation",
      "scene_id": "52aca753-64d9-464d-8bc9-dd2f4db15569",
      "visibility": "visible",
      "confidence": 0.95,
      "region": {
        "normalized": {
          "x": 0.68,
          "y": 0.77,
          "width": 0.24,
          "height": 0.06
        },
        "pixels": {
          "x": 379,
          "y": 763,
          "width": 134,
          "height": 60
        }
      },
      "observation": {
        "status": "valid",
        "value": 10,
        "confidence": 0.95,
        "diagnostic": "recognized fixed-layout counter"
      }
    },
    {
      "id": "a2ca3871-8454-4011-8ed2-48bf8360dc98",
      "name": "confirm_summon",
      "role": "action",
      "overlay_id": "27ae9ce4-b7a6-4fe7-b6fb-dde54a979a31",
      "visibility": "visible",
      "confidence": 0.96,
      "region": {
        "normalized": {
          "x": 0.55,
          "y": 0.78,
          "width": 0.34,
          "height": 0.08
        },
        "pixels": {
          "x": 307,
          "y": 774,
          "width": 190,
          "height": 79
        }
      },
      "interaction": {
        "kind": "click",
        "point_normalized": {
          "x": 0.72,
          "y": 0.82
        },
        "point_pixels": {
          "x": 402,
          "y": 813
        },
        "configured": true
      }
    }
  ],
  "diagnostics": []
}
```

Contract rules:

- Every returned element has a stable ID, non-empty name, role, visibility state, confidence when meaningful, and geometry.
- Geometry is returned both as normalized coordinates and as pixel coordinates resolved against the exact analyzed frame.
- Pixel rectangles are validated, clipped only according to an explicit documented policy, and never silently overflow the frame.
- An actionable element may declare a profile-authored interaction point. The default, when explicitly enabled by the author, may be the rectangle center; Yash must distinguish a configured point from a derived center.
- Detection regions and interaction targets are separate concepts. A small OCR crop must not automatically become a clickable target.
- Elements with `unknown` or ambiguous visibility must not expose a supposedly safe interaction point.
- Unknown or ambiguous scene classification returns no scene-scoped action targets unless their visibility is independently proven.
- The response contains no screenshot bytes and does not persist the source image.

## 3.1 JSON-first AI consumption and visual escalation

The primary operational goal is to minimize image transfer to an AI. A recognized, sufficiently covered frame should be consumable entirely from structured JSON. Full screenshots are a fallback for discovery and recovery, not the normal control loop.

Add a bounded decision block to every analysis result:

```json
{
  "ai_handoff": {
    "json_sufficient": true,
    "visual_review_recommended": false,
    "reason": null,
    "coverage": 0.97,
    "missing_required_elements": [],
    "suggested_crops": []
  }
}
```

`json_sufficient` may be true only when:

- the base scene is recognized above its configured confidence threshold;
- the ambiguity margin is satisfied;
- required scene anchors and required observations are valid;
- every returned actionable element has independently valid visibility evidence;
- its geometry and optional interaction point resolve inside the exact frame;
- no unrecognized overlay materially obscures a required target;
- the profile revision and frame dimensions are included in the response.

Visual review should be recommended when the scene is unknown or ambiguous, confidence is below policy, a required element is missing, an overlay is uncertain, geometry is invalid, the profile layout is incompatible, or the current frame is novel relative to reviewed evidence.

When escalation is needed, return metadata for the smallest useful visual regions:

```json
{
  "ai_handoff": {
    "json_sufficient": false,
    "visual_review_recommended": true,
    "reason": "unknown_overlay",
    "coverage": 0.61,
    "missing_required_elements": ["confirm_summon"],
    "suggested_crops": [
      {
        "name": "dialog_area",
        "pixels": {"x": 72, "y": 260, "width": 414, "height": 520},
        "purpose": "classify the unrecognized modal overlay"
      }
    ]
  }
}
```

The JSON response describes crops but does not contain or automatically persist their bytes. A separate explicit command may materialize a suggested crop for AI review. The escalation order is:

1. JSON only.
2. One explicitly requested minimal crop.
3. Several bounded crops only when they answer different unresolved questions.
4. Full screenshot only when scene discovery, layout failure, or broad occlusion makes crops insufficient.

After visual review, corrected evidence should enter the existing review/regression workflow so the same situation becomes JSON-only in future runs.

## 4. Profile schema version 2

Introduce profile schema version 2. Schema 1 is already a persisted compatibility-sensitive contract, so the implementation must provide a deterministic v1-to-v2 migration and retain golden fixtures.

Add stable opaque IDs for scenes, overlays, and interaction targets. Keep detector elements as stable entities and avoid duplicating detector configuration inside scenes.

Recommended model:

```text
Profile
├── global_elements[]
├── scenes[]
│   ├── recognition expression
│   ├── detector element references
│   └── interaction targets
├── overlays[]
│   ├── recognition expression
│   ├── detector element references
│   └── interaction targets
├── elements[]
├── derived_observations[]
└── rules[]
```

### 4.1 Scenes

A scene is one mutually exclusive base screen such as gameplay, home, inventory, map, settings, or results. It contains:

- stable ID and stable machine name;
- user-facing label and optional description;
- enabled state and priority;
- bounded recognition expression referencing anchor detector observations;
- minimum recognition confidence;
- entry and exit hysteresis settings for live capture;
- references to detector elements evaluated only while active;
- named interaction targets;
- optional transition hints to likely next scenes.

Transition hints improve ranking but must never make an otherwise valid scene impossible to recognize. Deep links, reconnects, and unexpected dialogs must remain recoverable.

### 4.2 Overlays

Overlays are independently recognizable layers such as dialogs, tutorials, rewards, network errors, tooltips, and confirmation panels. Multiple overlays may coexist with a base scene. They use the same bounded recognition and element-reference primitives as scenes.

### 4.3 Global and anchor elements

Global elements are evaluated regardless of the active scene. Recognition anchors are a bounded subset evaluated during stage one. Expensive OCR and classifier detectors must not become anchors unless measured evidence justifies them.

### 4.4 Interaction targets

An interaction target is declarative metadata, not an input action. It contains:

- stable ID and name;
- role such as `action`, `navigation`, `dismiss`, or `selection`;
- normalized rectangle;
- optional normalized safe point;
- optional visibility expression referencing observations;
- owning scene or overlay;
- optional human-readable caution metadata.

Import, duplication, revision history, archive validation, and migration must preserve or correctly rekey all references.

## 5. Two-stage bounded analysis

### Stage 1 — Scene and overlay recognition

Run only:

- global detector elements;
- enabled scene anchors;
- enabled overlay anchors.

Resolve a ranked list of scene candidates from bounded non-recursive boolean expressions over typed observations. Select a base scene only when the winner meets its confidence threshold and exceeds the runner-up by a configured ambiguity margin. Otherwise report `unknown` or `ambiguous`.

Resolve overlays independently because they are not mutually exclusive.

Live capture applies N-of-M evidence and entry/exit hysteresis. One-shot screenshot analysis reports the evidence available in that exact frame and must not fabricate temporal certainty.

### Stage 2 — Contextual extraction

Run only:

- global non-anchor elements;
- elements referenced by the active base scene;
- elements referenced by recognized overlays.

Then compute derived observations, applicable event rules, visible interaction targets, and structured geometry. Scene changes must reset detector/rule history whose semantics do not cross scene boundaries.

Add explicit resource limits for:

- maximum scenes and overlays per profile;
- maximum anchors per scene/overlay;
- maximum recognition-condition leaves;
- maximum contextual detector elements per frame;
- maximum interaction targets per scene/overlay;
- maximum candidate and diagnostic entries returned;
- maximum total analysis time for an explicit screenshot request.

## 6. One-shot screenshot analysis

Add a pure daemon-owned analysis path for a caller-provided PNG:

```bash
yash-eventsctl --json analyze image \
  --profile-id <profile-uuid> \
  /absolute/path/to/screenshot.png
```

Add an additive protocol-v1 method such as `analysis.evaluate_image`. The CLI canonicalizes the requested path and the daemon applies the same image limits already used by replay suites. The method must:

- decode bounded grayscale/RGB/RGBA PNG input;
- use the same scene resolver and detector implementations as live processing;
- return the JSON contract in section 3;
- perform no capture, event publication, route delivery, current-state mutation, or image persistence by default;
- expose an explicit future-compatible option if publication is ever required;
- return `unknown` for detectors needing temporal baseline when only one frame is supplied;
- execute outside the GUI render thread;
- use a bounded request timeout and cancellation path.

Factor existing replay/suite PNG decoding into one validated decoder rather than adding a third decoder.

## 7. Live state and event integration

Extend daemon state with:

- active scene ID/name/status/confidence;
- ranked scene candidates, bounded to a small maximum;
- active overlays;
- scene transition sequence and timestamp;
- contextual observations;
- visible named element geometry.

Add scene transition events such as `entered`, `left`, and `changed`, while preserving the existing rule/event separation. Scene recognition produces scene observations; a temporal scene resolver produces scene transitions.

Existing protocol-v1 status fields remain compatible. New fields are additive. Persisted state schemas must be versioned and golden-tested.

## 8. GUI authoring workflow

Add a scene/layer editor without moving processing onto the render thread:

1. Create, rename, duplicate, reorder, enable, and remove scenes and overlays.
2. Mark existing detector elements as global, anchor, or contextual.
3. Build bounded recognition expressions from typed observations.
4. Configure thresholds, ambiguity margin, and live hysteresis.
5. Assign detector elements to scenes/overlays.
6. Draw and label interaction target rectangles.
7. Configure an optional safe interaction point visually.
8. Show normalized and actual-frame pixel geometry.
9. Preview ranked scene candidates and explain why a scene matched or failed.
10. Show which detectors ran and which were skipped on the current frame.
11. Test scenes, overlays, element extraction, and geometry against frozen frames and replay suites.

The GUI must clearly state that interaction targets are descriptive and that Yash never sends input.

## 9. Wayland virtual desktop integration

After the generic Yash command is verified, extend the global `wayland-virtual-desktop` skill with:

```bash
wayland-desktop analyze \
  --name queen-blade \
  --yash-profile <profile-uuid>
```

The integration shall:

1. capture a fresh screenshot through the existing RFB screenshot path;
2. store it in a user-private temporary file;
3. invoke `yash-eventsctl --json analyze image`;
4. return the structured scene, observations, element rectangles, and safe interaction points;
5. remove the temporary screenshot on success or failure unless `--keep-screenshot` is explicit;
6. never automatically execute a returned action;
7. preserve the screenshot dimensions used by the returned pixel coordinates;
8. document that a fresh screenshot and analysis are required after every UI transition.

The default adapter output is JSON only. It must not expose the temporary screenshot to the AI when `ai_handoff.json_sufficient` is true. When visual review is recommended, it should report the reason and suggested crop geometry, then require an explicit crop/full-frame materialization action. Add commands equivalent to:

```bash
wayland-desktop analyze --name queen-blade --yash-profile <profile-uuid>
wayland-desktop analyze-crop --name queen-blade --crop dialog_area
wayland-desktop analyze-frame --name queen-blade --keep-screenshot
```

The crop command must bind the requested crop to the same analyzed frame identity; if the desktop has changed, it must capture and analyze again instead of applying stale coordinates.

Add Bash completion for `analyze`, `--yash-profile`, and `--keep-screenshot`. Keep Yash optional: ordinary virtual-desktop lifecycle and input commands must continue to work when Yash is not installed.

## 10. Implementation sequence

### Phase A — Specification and impact baseline

1. Add normative `SPEC-SCENE-*` requirements and acceptance criteria to `SPECS.md`.
2. Add a post-release scene-awareness milestone to `ROADMAP.md` and reference this plan.
3. Record compatibility decisions for profile schema 2, additive protocol-v1 methods, and state response schemas.
4. Run repository impact analysis on profile validation/migration, duplication, daemon pipeline construction, replay/suite evaluation, state publication, CLI routing, and GUI profile editing.
5. Record baseline formatting, Clippy, tests, README claims, and documentation results.

Exit gate: specifications and compatibility rules are internally consistent before implementation begins.

### Phase B — Schema 2 and migration

1. Add scene, overlay, recognition-expression, element-scope, and interaction-target types.
2. Add strict validation and resource bounds.
3. Implement v1-to-v2 migration as one implicit default scene preserving existing flat behavior.
4. Update duplication/rekey, archive import/export, revision comparison, and catalog validation.
5. Add golden schema, migration, malicious-input, and round-trip tests.

Exit gate: every schema-1 profile loads with identical detector/event behavior, and schema-2 references cannot dangle or cycle.

### Phase C — Scene resolver and gated engine

1. Implement typed recognition-expression evaluation.
2. Implement ranked mutually exclusive base-scene resolution.
3. Implement independent overlay resolution.
4. Add ambiguity handling, N-of-M evidence, hysteresis, and scene transitions.
5. Split pipeline evaluation into anchor and contextual stages.
6. Reset scoped runtime state safely when context changes.
7. Add execution metrics for evaluated and skipped detectors.

Exit gate: replay tests prove correct scene/overlay transitions, unknown/ambiguous behavior, deterministic results, and reduced detector execution.

### Phase D — Screenshot analysis and geometry contract

1. Factor the bounded PNG decoder.
2. Implement pure one-shot scene and element evaluation.
3. Resolve normalized rectangles into validated pixel rectangles.
4. Resolve configured safe points into pixel coordinates.
5. Add `analysis.evaluate_image`, CLI command routing, JSON output, stable error mapping, and golden responses.
6. Compute the JSON-sufficiency decision, coverage, missing required elements, and minimal suggested crops.
7. Ensure no output/event/state side effects.

Exit gate: a `558x992` fixture returns its scene, overlays, observations, element names, rectangles, safe points, and a deterministic JSON-sufficiency decision exactly, while malformed/oversized inputs and unsafe geometry are rejected.

### Phase E — Live daemon and GUI integration

1. Publish scene/layer state and transitions through the existing bounded paths.
2. Add scene/layer/interaction-target editing to the GUI.
3. Add live/frozen diagnostics and detector execution explanations.
4. Update profile history comparisons and conflict handling for scene edits.

Exit gate: a user can author two base scenes plus one overlay, verify recognition, draw a named action target, and inspect its returned pixel geometry without editing profile JSON manually.

### Phase F — Virtual desktop adapter

1. Add the optional `wayland-desktop analyze` action.
2. Add private temporary-file lifecycle and cleanup tests.
3. Add completion and documentation.
4. Implement explicit minimal-crop and full-frame escalation commands bound to a fresh frame identity.
5. Run an end-to-end isolated desktop smoke test without enabling any automatic input.

Exit gate: one command captures and analyzes an isolated desktop and returns geometry matching the exact screenshot dimensions without exposing image bytes when JSON is sufficient; an explicit escalation returns only the requested fresh crop unless a full frame is justified.

### Phase G — Performance, compatibility, and release evidence

1. Benchmark flat versus gated profiles with representative counts of scenes, anchors, OCR regions, and interaction targets.
2. Verify bounded memory and cancellation under slow detectors and clients.
3. Run all schema/protocol golden and migration tests.
4. Run full workspace formatting, strict Clippy, tests, README claim checks, and docs.
5. Update `SPECS.md`, `ROADMAP.md`, `PLAN.md`, and `README.md` only with verified evidence.
6. Document known limitations and recovery behavior.

Exit gate: scene-aware analysis is measurably more efficient than evaluating every detector, compatibility tests pass, and all user-facing claims have evidence.

## 11. Verification matrix

At minimum, add tests for:

- schema-1 to schema-2 migration and golden round trips;
- duplicate/rekey behavior for every new stable ID and reference;
- malformed, excessive, cyclic, and dangling recognition expressions;
- base-scene winner, unknown, tie/ambiguity, priority, and hysteresis;
- simultaneous independent overlays;
- transition hints that rank but never prohibit recovery;
- global/anchor/contextual detector scheduling counts;
- detector-state reset on scene changes;
- OCR/classifier suppression outside their owning scenes;
- normalized-to-pixel conversion at `558x992`, `900x1600`, and a landscape resolution;
- out-of-frame and overflow geometry rejection;
- configured safe point, explicitly derived center, and absent unsafe point;
- omission of action targets for unknown/ambiguous visibility;
- deterministic JSON-sufficiency, coverage, missing-element, and escalation-reason decisions;
- minimal suggested-crop geometry and stale-frame rejection;
- pure screenshot analysis with no durable state/event/route side effects;
- live and replay scene-transition equivalence;
- CLI and protocol golden JSON;
- GUI render-thread isolation;
- archive/import/catalog limits and security boundaries;
- optional Wayland adapter behavior when Yash is installed and absent;
- temporary screenshot cleanup on success, daemon error, timeout, and interruption;
- regression performance showing bounded stage-one cost and fewer stage-two evaluations.

## 12. Compatibility and safety constraints

- Preserve `SPEC-PROD-003`: Yash describes UI geometry but never generates input.
- Preserve the daemon as the sole owner of active analysis state and profile writes.
- Preserve bounded latest-frame, subscription, detector, OCR, classifier, and output queues.
- Treat profile assets and caller-provided screenshots as untrusted and resource-limit all parsing.
- Do not persist screenshots without explicit consent.
- Do not expose image bytes through JSON-RPC.
- Do not infer click safety from an OCR/template crop; interaction geometry must be profile-authored.
- Do not report a scene, overlay, element visibility, or interaction point with fabricated certainty.
- Preserve existing schema-1 behavior through tested migration.
- Preserve existing protocol-v1 methods and fields; use additive methods/fields unless a deliberate versioned protocol migration is justified and documented.
- Keep game-specific profiles and copyrighted screenshots outside the generic repository unless their redistribution rights are explicit.

## 13. Completion definition

This enhancement is complete only when:

1. schema-2 scene/overlay/interaction contracts and v1 migration are verified;
2. live, replay, frozen, and explicit screenshot paths share the same resolver and detector behavior;
3. gated analysis demonstrably avoids irrelevant expensive detector work;
4. unknown and ambiguous states fail safely;
5. JSON results include exact frame dimensions, element names, normalized rectangles, pixel rectangles, and explicit safe points where configured;
6. recognized and sufficiently covered frames require JSON-only AI handoff, while uncertain frames provide bounded crop-first escalation metadata;
7. the GUI can author and diagnose the complete model;
8. the optional Wayland virtual-desktop adapter passes end-to-end without automatic input;
9. security, privacy, compatibility, and bounded-resource tests pass;
10. canonical formatting, strict Clippy, workspace tests, README claim checks, and documentation generation pass;
11. normative documents and verified behavior agree.
