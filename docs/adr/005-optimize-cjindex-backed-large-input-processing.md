# Optimize cjindex-Backed Large Input Processing

## Status

Proposed

## Related Commits

- `3eb784b` Optimize cityjson-index full scans
- `2b6c380` Optimize CityJSON index processing

## Context

Tyler now supports `cjindex` datasets as an input abstraction. This lets Tyler
work with `feature-files`, `ndjson`, and monolithic `cityjson` storage layouts
through one interface, but the first implementation kept several expensive
per-feature access patterns.

On a 10,771,547 feature sidecar, an older full run showed these phases:

- rebuilding the `cjindex` sidecar: about 7 minutes
- computing the unfiltered dataset extent: about 5 hours
- counting vertices into grid cells: about 5.5 hours
- converting and optimizing 23,356 GLB tiles: about 66 minutes

The unfiltered extent phase was caused by iterating every feature bbox page.
The underlying `cityjson-index` page query also used a nullable predicate:

```sql
WHERE (?1 IS NULL OR f.id > ?1)
```

For large sidecars, SQLite planned this as an RTree scan plus a temporary
sort, rather than a direct ordered range scan on `features.id`.

That issue has been fixed in `cityjson-index` by:

- splitting first-page and later-page SQL paths
- using `WHERE f.id > ?` only for later pages
- adding an aggregate bounds/count API for unfiltered extent computation

Tyler now uses the aggregate bounds API for unfiltered `cjindex` extent
computation. That should remove the old hours-scale extent scan. The next
large costs are the remaining phases that still need to read, decode, prepare,
and merge many features.

The current grid-counting path pages feature references and then reads each
feature individually. During tile conversion, Tyler stores only the public
`cjindex` feature id, reopens or resolves through `CityIndex`, and then reads
each selected tile feature by string id. This discards row-locality information
that Tyler already sees while scanning the index.

Model preparation also appears to do duplicated work. Each tile feature is
filtered, pruned, cleaned, and has extents recomputed before merge. The merged
tile model is then cleaned and has extents recomputed again.

## Decision

Tyler will optimize large `cjindex` inputs around rowid-backed feature
references and batched reads while preserving current grid assignment
semantics.

Completed `cityjson-index` changes are part of this decision:

1. `cityjson-index` uses separate first-page and later-page SQL queries for
   full scans.
2. Later pages use a direct rowid range predicate:

   ```sql
   WHERE f.id > ?
   ORDER BY f.id
   LIMIT ?
   ```

3. `cityjson-index` exposes an aggregate bounds/count helper that returns
   `None` for empty indexes.
4. Tyler uses that aggregate helper for unfiltered `cjindex` extent
   computation, then applies `--grid-minz` and `--grid-maxz` clamping as
   before.

The planned follow-up work is:

1. Add a public row-reference type or extend the existing feature reference in
   `cityjson-index` so callers can keep both:
   - the public feature id
   - the SQLite feature rowid needed for direct reads
2. Add row-ref or batch-read APIs to `cityjson-index`, such as:
   - read one feature by row ref
   - read many features by row ref
   - scan pages that yield feature refs with decoded models
3. Store a Tyler-owned serializable copy of the `cjindex` row ref in
   `FeatureReference` instead of storing only `CjIndexId(String)`.
4. Use the new row-ref or scan API in grid counting so Tyler can consume
   rowid-ordered decoded features without resolving each feature through its
   public string id.
5. Use thread-local `CityIndex` handles in tile conversion, matching the grid
   indexing strategy.
6. Use row-ref or batch reads during tile conversion. Tile refs may be sorted
   by rowid before reading when that does not change output semantics.
7. Audit tile model preparation and, if behavior is preserved, defer
   `cleanup_and_update_extents` until after feature models are merged.

No Tyler CLI changes are required for this work.

## Required Semantics

The optimization must preserve Tyler's observable tiling behavior.

In particular:

- selected-vertex counting remains part of the grid ownership score
- bbox-intersected cells remain candidates as they are today
- unique ownership for feature classes that require it remains unchanged
- tile contents remain selected by the same quadtree/grid feature membership
- feature type filtering, LoD pruning, and optional parent attribute
  inheritance keep their current behavior
- aggregate extent computation must produce the same unfiltered extent as the
  previous iterative bbox scan, except for existing min/max z clamping options

## Consequences

Good:

- unfiltered extent computation no longer requires reading every bbox page
- full scans can use ordered rowid range queries instead of nullable predicates
- Tyler can avoid resolving public feature ids repeatedly during later phases
- grid counting can be driven by row-local scan or batch APIs
- tile conversion can reduce repeated SQLite setup and point lookups
- rowid-sorted batch reads should improve locality for large sidecars
- deferring cleanup and extent recomputation may remove duplicated per-feature
  work during tile export

Trade-offs:

- Tyler's serialized world/debug state needs a richer `cjindex` feature
  reference
- `cityjson-index` must expose row-reference APIs carefully, without making
  storage details unstable for callers
- batch APIs add more public surface that needs tests for ordering, missing
  rows, and page boundaries
- deferring cleanup requires focused regression tests because filtering and
  parent/root handling are subtle

## Rejected Alternatives

- Keep using only public string feature ids.
  This is simple, but it forces repeated id resolution and prevents direct
  rowid-based reads in the hot paths.

- Only increase `CJINDEX_PAGE_SIZE`.
  Larger pages reduce some overhead, but they do not address per-feature string
  lookups or repeated point reads during tile conversion.

- Cache every decoded feature model in Tyler.
  This would avoid repeated decoding, but 10M+ features make a full decoded
  cache too memory-heavy for the expected datasets.

- Change grid ownership semantics while optimizing.
  That would make performance results hard to compare and could alter tile
  assignment. Ownership changes belong in a separate decision.

- Remove per-feature cleanup without tests.
  The duplicated cleanup is a good optimization candidate, but it must be
  proven against filtering, LoD pruning, inherited attributes, empty geometry
  removal, and final tile extents.

## Validation Plan

For `cityjson-index`:

- verify later page scans use `SEARCH f USING INTEGER PRIMARY KEY (rowid>?)`
  in `EXPLAIN QUERY PLAN`
- test first page, later pages, page boundaries, and no skipped or duplicated
  refs
- test aggregate bounds/count against iterative bounds on small fixtures
- test row-ref and batch-read APIs against `get(feature_id)`

For Tyler:

- test row-ref serialization and round-trip through world/debug data
- test optimized grid counting against the current behavior, including cells
  overlapped only by bbox
- test tile conversion reads the same features through row refs as through
  public ids
- test deferred cleanup, if implemented, against filtering, LoD pruning,
  parent attributes, empty geometry removal, and final extents

For performance:

- benchmark after the aggregate extent fix to establish the new baseline
- measure grid counting separately from feature read/decode time
- measure tile conversion subphases:
  - collect tile feature ids
  - read/decode features
  - prepare feature models
  - merge
  - cleanup and extent update
  - GLB encode/write
- compare wall time on the large sidecar using the same input path, feature
  count, tile count, output path location, and release build settings
