# Scene view editing

The portable editor supports ordered multi-selection in the hierarchy and scene
viewport. Click replaces selection; Ctrl/Command toggles and Shift adds. Drag
empty space to select visible geometry in a rectangle. The last selected entity
is active. Invisible/non-rendering entities remain selectable in the hierarchy.

Use W, E and R while hovering the viewport to choose translation, rotation or
scale, or use the toolbar. Drag the center for camera-plane translation or
uniform scale, an axis for constrained movement/scale, a colored plane for
plane translation, or a ring for rotation. World/local space and center/active
pivot are configurable. Frame selection centers the view on the active entity.

A held gesture previews transforms only in the transient runtime. Release
submits one revision-checked operation (a batch for multiple roots), so one undo
restores the gesture. Escape, loss of application focus or pointer, or starting
camera navigation cancels the preview. A stale release is rejected and reconstructs the
preview from the current authored document. Selected children follow selected
ancestors once; their local transforms are preserved. A singular parent disables
manipulation. Rotation uses the parent's rotation basis; axis scaling edits the
closest local scale axis. General world-space shear is not representable by the
scene Transform component.

Snap enables translation steps, rotation degrees and scale-factor increments.
Grid spacing and visibility affect the view only. View settings includes named
camera bookmarks with replace, restore and delete operations. Save view settings
writes the validated, versioned `editor.json` beside `project.json`; view changes
are separate from scene history and scene Save. Missing settings use defaults.
Invalid existing settings produce a diagnostic and disable settings writes until
the file is repaired and the editor reopened. Settings writes use independent
temporary files, then replace the settings file atomically; concurrent saves use
last-writer-wins semantics. Settings validation is shared in
`engine-editor-settings`, including numeric, document-size and bookmark limits.

Picking uses a CPU ID raster built from extracted triangle geometry at 640x360
when the scene preview changes. Mouse movement and selection read that cached
buffer. Meshes use camera clipping and depth tests, with renderer opaque ID
ordering and LessEqual depth ties. Transparent 3D draws follow renderer back-to-front
ordering without depth writes. 2D draws use their own camera, layer/ID ordering
and per-draw depth-test/write flags, then overlay the mesh pass. The ID maps
back to the authored entity, including sprite/mesh draw prefixes. Transparent texture holes
and vertex/material alpha at or below 1/255 are unselectable; texture alpha uses
nearest clamped UV sampling. Marquee collects IDs with visible pixels, excluding
fully occluded geometry. Non-rendered helpers are selected through the hierarchy.

The buffer is geometric picking, not a GPU shader replay: custom WGSL vertex
movement/discard, filtered alpha edges and blended transparency can differ from
the displayed image. UI-space runtime draws are excluded. Picking is bounded to
200,000 triangles and 32,000,000 candidate samples per rebuild; exceeding either
limit disables picking with a diagnostic, retaining the preview image. A failed
preview rebuild also clears picking so a stale image cannot select old geometry.

Validation uses numeric 2D/3D projection and gizmo tests, CPU picking tests for
clipping/depth/alpha and work limits, and real egui input sequences for gesture
preview, multi-object translate/rotate/scale, snapping, cancellation, stale
releases, parent/child selection and single-entry undo. These tests use a headless
preview renderer; they do not establish interactive GPU/platform visual parity.
