# Optimize CityJSON-to-GLB Pre-Write Path

## Status

Proposed

## Related Commits

- none yet

## Context

Tyler spends most of its large-input time before `gltf_writer::write_city_model_glb`.
The hot path currently includes several layers of temporary reconstruction:

- `cityjson-index` decodes features from indexed storage
- `cityjson-json` / `cityjson-lib` materialize feature models
- Tyler indexes features into the dense grid and builds tile membership
- Tyler re-reads the selected tile features, prepares them, merges them, and then
  hands a `CityModel` to `cityjson_convert::convert_to_glb`

Earlier profiling and the current code audit showed repeated allocation churn in
that path:

- selected-geometry scans build and sort temporary vertex lists
- grid counting used a full bbox-expanded `CellCounts` path even when the feature
  class only needed a single owning cell
- tile preparation repeated type filtering, empty-geometry cleanup, and extents
  recomputation even when those steps could not change the GLB-relevant output
- `cityjson-index` feature reads were being reopened and re-resolved more often
  than necessary during tile export

The current work has already reduced several of those costs inside Tyler, but the
overall path is still split across index decode, tile preparation, and GLB write.
The next optimizations should build on the same model rather than reintroduce
intermediate JSON materialization.

## Decision

Tyler will treat the CityJSON-to-GLB pre-write path as a single optimization
surface with two phases.

### Implemented first-pass changes

Tyler now:

1. Tracks feature type membership during grid indexing so later tile preparation
   can skip no-op type filtering.
2. Uses a direct unique-assignment fast path for `Building` and `BuildingPart`
   rather than materializing a full bbox-expanded `CellCounts` just to pick the
   winning cell.
3. Pre-sizes selected-geometry scratch buffers conservatively to reduce repeated
   allocation in `selected_geometry_stats`.
4. Skips tile-preparation work when it cannot change GLB-relevant output:
   - type filtering is skipped when the tile is already known to contain only the
     selected types
   - empty-geometry cleanup is skipped when no prior step can introduce empty
     CityObjects
   - merged GLB models are no longer sent through `cleanup_and_update_extents`
     before `convert_to_glb`
5. Adds timing and count logging around the main grid-indexing and tile-export
   substeps so the remaining hot spots can be measured directly.

### Planned follow-up changes

Tyler will keep the same pre-write boundary and continue optimizing the path
before `convert_to_glb` by reducing feature reconstruction churn:

1. Favor row-ref or batch-read APIs in `cityjson-index` when scanning features
   for grid indexing and tile export.
2. Preserve a cached parsed base model/root so feature assembly does not need to
   serialize intermediate `serde_json::Value` data back into bytes.
3. Prefer direct import of assembled features into `OwnedCityModel` when that can
   be done without changing observable output.
4. Keep tile preparation focused on the GLB-relevant geometry path, and only add
   extra cleanup when debug CityJSON output or another explicit output mode needs
   it.

The `gltf_writer` boundary remains unchanged in this phase: it continues to
receive `&CityModel`.

## Consequences

Good:

- the current pass removes several redundant temporary containers in Tyler
- future work has a clear boundary: reduce pre-write reconstruction, not GLB
  encoding semantics
- timing logs make it easier to compare the remaining cost of feature decode,
  tile preparation, merge, and GLB conversion
- the tile preparation path is now easier to reason about because no-op work is
  skipped explicitly

Trade-offs:

- the optimization surface spans multiple crates, so the next pass will need
  coordinated API changes in `cityjson-index` and possibly `cityjson-json`
- some skipped work is now conditional on assumptions about what affects GLB
  output, so those assumptions need regression tests
- direct feature import and cached root reuse may require new public API surface
  that must stay stable enough for callers outside Tyler

## Rejected Alternatives

- Leave the current Tyler-local improvements as a one-off patch.
  That would fix some churn, but it would not address the repeated decode and
  materialization work that still sits in `cityjson-index` and `cityjson-json`.

- Move the optimization boundary into `gltf_writer`.
  The writer is already downstream of the expensive reconstruction. Optimizing
  there would not reduce the repeated feature assembly and tile-prep work that
  happens before it.

- Rewrite the pre-write path around a new ad hoc representation.
  The repo already has a CityJSON model boundary and a 3D Tiles writer boundary.
  Replacing both would be a larger design change than this pass needs.

- Skip all cleanup unconditionally.
  That risks changing output behavior for filtered or debug paths where cleanup
  still matters.

## Validation Plan

- keep the current parity tests for unique assignment and tile-preparation skips
- add regression tests for any future `cityjson-index` row-ref or batch-read API
- verify that cached-base and direct-import work preserves:
  - CityJSON
  - CityJSONSeq / NDJSON
  - feature-file cjindex layouts
- compare wall time before and after each follow-up phase using the existing
  collapsed-profile workflow
- keep `cargo test` and workspace tests green after each step

## Notes

This ADR is intentionally broader than the first patch set. It records the
current Tyler-side optimizations and the direction for the next pass so that the
implementation can continue without reopening the same scope discussion.
