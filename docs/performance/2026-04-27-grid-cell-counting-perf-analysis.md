# Grid Cell Counting Stage Perf Analysis

Date: 2026-04-27

Input profiles:

- `bvz_dh_2026-04-27-215648.collapsed`
- `ams_up_large_2026-04-27-221437.collapsed`
- `ams_up_large_2026-04-27-223426.collapsed`
- `ams_up_large_2026-04-27-224618.collapsed`

## Focus

This note analyzes the stage that starts when Tyler logs:

```text
Counting vertices in grid cells
```

The current test run spends about one minute in this stage. On the real input
of roughly 11 million features, this stage reportedly takes about one hour out
of a three-hour total runtime.

## Code Path

The log is emitted by `World::index_with_grid` in `src/parser.rs`.

The stage currently does the following:

1. Scans cjindex feature pages with `CJINDEX_PAGE_SIZE = 65_536`.
2. Processes each page in parallel with Rayon.
3. For each feature, calls `index_feature_model`.
4. Computes selected geometry stats:
   - selected vertex indices
   - bbox
   - centroid
   - selected object types
5. Counts selected vertices by grid cell with `count_vertices_in_grid`.
6. Merges vertex-cell counts with every cell intersecting the feature bbox.
7. Converts the result into feature-to-cell assignments.
8. Integrates those assignments into the dense square grid.

Important source locations:

- `src/parser.rs:286`: `World::index_with_grid`
- `src/parser.rs:315`: `selected_geometry_stats`
- `src/parser.rs:319`: `count_vertices_in_grid`
- `src/parser.rs:623`: large-feature parallel path threshold
- `src/parser.rs:633`: merge vertex counts with bbox cells
- `src/parser.rs:709`: `CellCounts::merge_vertex_counts_with_bbox`
- `src/parser.rs:747`: selected type filtering

## Profile Caveat

The collapsed profile is whole-process, not sliced to the log stage.

It contains 384,998 samples total, but only 44 samples are directly attributed
to `tyler::parser::World::index_with_grid`, and zero samples are named under
`count_vertices_in_grid` or `count_vertex_cells`.

That means this file is not sufficient to precisely prove where time is spent
inside the logged stage. The likely causes are:

- the profile is dominated by other stages,
- relevant functions were inlined into generic/library frames,
- debug symbols or frame pointers are not enough to preserve the exact Rust
  call path in the collapsed stacks.

The next run should add explicit timing around the substeps in this stage.

## Whole-Run Hotspots: Initial Mixed Profile

Broad aggregates from the profile:

```text
total samples                         384,998
proj-related                          104,148   27.05%
allocator-ish                          61,390   15.95%
BTreeMap::insert                       15,755    4.09%
all alloc::collections::btree frames    20,552    5.34%
lock/futex                             11,098    2.88%
cityjson_index::collect_vertex_indices  2,933    0.76%
selected_geometry_stats                   198    0.05%
```

Top leaf symbols include:

```text
libproj.so.25.9.4.0`[unknown]                         10.19%
alloc::collections::btree::map::BTreeMap::insert        4.03%
libc.so.6`_int_malloc                                   3.96%
libm.so.6`__ieee754_pow_fma                             3.46%
serde_json::value::de::::deserialize                    3.36%
libm.so.6`__sin_fma                                     3.35%
libm.so.6`__atan_fma                                    3.21%
serde_json::de::Deserializer::parse_integer             2.90%
libc.so.6`_int_free                                     2.72%
```

These numbers should not be read as stage-specific, but they do show that
allocator churn and `BTreeMap` mutation are meaningful costs in the full run.

## Buildings-Only Large-Area Profile

Second input profile:

```text
ams_up_large_2026-04-27-221437.collapsed
```

This run was filtered to `Building` and `BuildingPart`.

The file contains 49,113 folded stacks and 272,850 samples. The profile is
still a whole-process collapsed profile, so it is not a clean slice of the
`Counting vertices in grid cells` stage. However, compared with the initial
profile, `selected_geometry_stats` is now visible enough to inspect.

Broad aggregates:

```text
total samples                          272,850
selected_geometry_stats                  2,217    0.81%
World::index_with_grid                     115    0.04%
count_vertices_in_grid                       0    0.00%
count_vertex_cells                           0    0.00%
merge_vertex_counts_with_bbox                0    0.00%
feature_to_cells                             0    0.00%
BTreeMap::insert                         2,015    0.74%
all alloc::collections::btree frames      7,194    2.64%
allocator-ish                            51,815   18.99%
free/cfree                               87,807   32.18%
lock/futex/spin                          82,746   30.33%
proj-related                             26,161    9.59%
serde_json                               27,035    9.91%
cityjson_index                            1,140    0.42%
```

Top leaf symbols:

```text
[kernel.kallsyms]`native_queued_spin_lock_slowpath     23.79%
libc.so.6`_int_malloc                                   6.28%
libc.so.6`_int_free                                     4.15%
libproj.so.25.9.4.0`[unknown]                           3.61%
core::ptr::drop_in_place                                1.93%
libc.so.6`malloc                                        1.66%
libc.so.6`malloc_consolidate                            1.58%
core::hash::BuildHasher::hash_one                       1.53%
sqlite3VdbeExec                                         1.51%
::write                                                 1.48%
```

For stacks containing `selected_geometry_stats`, the immediate child frames are:

```text
libc.so.6`cfree@GLIBC_2.2.5        1,909   86.11%
<self>                               304   13.71%
small_sort_network                     2    0.09%
asm_sysvec_apic_timer_interrupt        1    0.05%
quicksort                              1    0.05%
```

Interpretation:

- The buildings-only run supports the earlier suspicion that allocation churn
  is important in this path.
- The visible `selected_geometry_stats` samples are dominated by freeing memory,
  not by obvious arithmetic in vertex-to-grid lookup.
- `count_vertices_in_grid`, `count_vertex_cells`,
  `merge_vertex_counts_with_bbox`, and `feature_to_cells` still do not appear as
  named frames, likely because they are inlined or lost in the folded stacks.
- This profile does not prove that bbox-cell materialization alone consumes the
  hour, but it makes an allocation-reduction optimization a better first target
  than micro-optimizing `SquareGrid::locate_point`.

Comparison with the initial mixed profile:

```text
metric                    mixed profile       buildings-only profile
total samples             384,998             272,850
selected_geometry_stats   198 / 0.05%         2,217 / 0.81%
BTreeMap::insert          15,755 / 4.09%      2,015 / 0.74%
btree frames total        20,552 / 5.34%      7,194 / 2.64%
allocator-ish             57,634 / 14.97%     51,815 / 18.99%
free/cfree                36,998 / 9.61%      87,807 / 32.18%
lock/futex/spin           11,683 / 3.03%      82,746 / 30.33%
proj-related              102,893 / 26.73%    26,161 / 9.59%
serde_json                67,759 / 17.60%     27,035 / 9.91%
cityjson_index            13,539 / 3.52%      1,140 / 0.42%
```

The buildings-only profile shifts away from projection and JSON parsing and
toward allocator/free/lock costs. That is consistent with a workload dominated
by many small feature-level temporary allocations.

## Buildings-Only Profile With Better Symbols

Third input profile:

```text
ams_up_large_2026-04-27-223426.collapsed
```

This profile has much better Rust stack detail. It contains 93,218 folded stacks
and 271,416 samples.

It still appears to include work outside the target grid-counting stage:

```text
cityjson_index / cityjson read path        46,825   17.25%
cityjson_convert / glTF / GLB write path   37,888   13.96%
```

So the percentages below are still whole-profile percentages, not exact
stage-only timings.

Broad aggregates:

```text
total samples                    271,416
selected_geometry_stats           21,782    8.03%
World::index_with_grid             7,809    2.88%
count_vertices_in_grid                 0    0.00%
count_vertex_cells                     0    0.00%
merge_vertex_counts_with_bbox          0    0.00%
feature_to_cells                       0    0.00%
SquareGrid::locate_point               0    0.00%
BTreeMap::insert                   2,688    0.99%
all btree frames                   7,623    2.81%
RawVec / Vec growth               38,567   14.21%
allocator-ish                     56,964   20.99%
free/cfree                        87,883   32.38%
lock/futex/spin                   78,628   28.97%
proj-related                      37,228   13.72%
serde_json                        46,042   16.96%
cityjson-related                  93,151   34.32%
```

The important new detail is inside `selected_geometry_stats`:

```text
selected_geometry_stats total      21,782    8.03% of total
RawVec reserve/growth inside it    19,159    7.06% of total, 87.96% of selected
malloc inside it                    5,914    2.18% of total, 27.15% of selected
free inside it                      1,853    0.68% of total,  8.51% of selected
sort inside it                        529    0.19% of total,  2.43% of selected
```

Immediate children below `selected_geometry_stats`:

```text
RawVecInner::reserve::do_reserve_and_handle   19,159   87.96%
cfree@GLIBC_2.2.5                              1,773    8.14%
quicksort                                        363    1.67%
<self>                                           240    1.10%
small_sort_network                               108    0.50%
```

Interpretation:

- `selected_geometry_stats` is now a clear hotspot.
- Almost all visible time inside it is `Vec` growth/reallocation, not sorting
  or bbox/centroid arithmetic.
- The path at `src/parser.rs:535-536` creates empty `selected_vertices` and
  `geometry_scratch` vectors for every feature, then
  `collect_selected_vertex_indices` grows them while extracting geometry vertex
  indices.
- For buildings-only runs, optimizing this allocation pattern is now the
  highest-confidence first step.
- The grid-counting functions are still not visible as named frames. That may be
  inlining, or it may mean they are cheaper than the allocation-heavy geometry
  stats path in this profile.

## Buildings-Only Profile With Half Rayon Threads

Fourth input profile:

```text
ams_up_large_2026-04-27-224618.collapsed
```

This run used half the previous Rayon thread count. It contains 42,813 folded
stacks and 176,327 samples.

The main result is that allocator lock contention drops sharply. In the previous
better-symbols run, stacks containing futex/spin-lock frames were 28.97% of
samples. With half the Rayon threads, that drops to 3.06%.

Comparison:

```text
metric                    full Rayon profile       half Rayon profile
total samples             271,416                  176,327
alloc/free/realloc        144,479 / 53.23%          63,757 / 36.16%
allocator locks/futex      78,628 / 28.97%           5,399 /  3.06%
rayon                      91,255 / 33.62%          15,598 /  8.85%
drop/destructors           81,179 / 29.91%          22,374 / 12.69%
cityjson read/parse        60,134 / 22.16%          56,487 / 32.04%
glb/gltf output            45,826 / 16.88%          42,028 / 23.84%
projection/proj            37,228 / 13.72%          31,394 / 17.80%
grid indexing/parser       29,591 / 10.90%          13,657 /  7.75%
selected_geometry_stats    21,782 /  8.03%           6,100 /  3.46%
sqlite                     23,121 /  8.52%          17,113 /  9.71%
hashing                    12,272 /  4.52%          11,565 /  6.56%
btree map                   7,623 /  2.81%           7,221 /  4.10%
```

Top leaf symbols in the half-thread profile:

```text
libc.so.6`_int_malloc                            7.97%
libc.so.6`_int_free                              5.51%
libproj.so.25.9.4.0`[unknown]                    5.20%
core::ptr::drop_in_place                         2.60%
libc.so.6`malloc                                 2.32%
serde_json::de::Deserializer::parse_integer      2.24%
core::hash::BuildHasher::hash_one                2.13%
::deserialize_any                                2.13%
::write                                          2.12%
libc.so.6`malloc_consolidate                     2.02%
```

Interpretation:

- The full-thread profile was heavily affected by glibc allocator contention.
- Reducing Rayon parallelism makes the workload characteristics clearer: the
  remaining major work is CityJSON parsing/reading, GLB/glTF output, PROJ work,
  and normal allocation/free cost.
- The workload is not scaling cleanly with the original thread count, because
  many threads contend on the allocator while processing allocation-heavy
  feature/model data.
- A different allocator (`jemalloc` or `mimalloc`) is now a strong whole-run
  experiment, because it may recover parallelism without forcing the Rayon
  thread count down.
- `selected_geometry_stats` still shows the same local pattern: 85.05% of its
  visible samples are `RawVec` growth/reserve and 83.21% contain malloc/realloc.
  The absolute share is lower because the profile is no longer dominated by
  allocator lock stalls.

## Most Likely Stage-Specific Problem

The current implementation always computes a full `CellCounts` object before
deciding whether the feature has unique cell assignment.

For `Building` and `BuildingPart`, `feature_to_cells` assigns the feature to
exactly one grid cell: the cell with the highest score.

However, before that max is taken, `count_vertices_in_grid` calls
`CellCounts::merge_vertex_counts_with_bbox`, which enumerates every grid cell
intersecting the feature bbox and materializes scores for all of them.

For building-like unique assignment, this can be wasteful:

- bbox-only cells are generated,
- `Vec` capacity is sized for the bbox intersection,
- all entries are iterated again to find the max,
- all non-max entries are then discarded.

This is especially suspicious for real input with millions of features, because
even a small per-feature bbox expansion cost scales directly with feature count.
Large or elongated feature bboxes can make it much worse.

The first buildings-only profile strengthens this recommendation. For
`Building`/`BuildingPart`, the code path currently builds and frees several
temporary containers per feature:

- `selected_vertices` inside `selected_geometry_stats`,
- a `BTreeMap<CellId, usize>` in `count_vertex_cells`,
- a `Vec<(CellId, usize)>` conversion before bbox merging,
- a `CellCounts` vector including bbox-only cells,
- a final `Vec<(CellId, Cell)>` in `feature_to_cells`.

For unique assignment, most of this data is only used to select one maximum
cell. The highest-value optimization is therefore to collapse the unique path
into a single pass that returns the best cell directly.

The third profile adds a more precise first target: the empty `Vec` allocations
inside `selected_geometry_stats` are repeatedly growing. Before changing
ownership semantics, reduce this churn by pre-sizing or reusing the temporary
vertex buffers used to collect unique vertex indices.

## Current Score Semantics

The counting code has a non-obvious `or_insert(1) += 1` pattern.

For a vertex in a previously unseen cell:

```rust
*vertex_counts.entry(cellid).or_insert(1) += 1;
```

The first vertex produces a score of `2`, not `1`.

Then bbox merging adds another point:

- vertex cell inside bbox: `count + 1`
- bbox-only cell: `2`

The tests currently encode this behavior. It should be preserved unless the
ownership scoring semantics are intentionally changed.

## Recommended Optimization

There are two related optimizations.

First, reduce `selected_geometry_stats` allocation churn:

1. Initialize `selected_vertices` with a capacity derived from
   `model.vertices().len()`.
2. Initialize `geometry_scratch` with a capacity derived from the same source or
   from a conservative per-geometry estimate.
3. Keep the existing `sort_unstable` and `dedup` behavior, because sorting is
   only a small part of the current visible cost and it preserves exact centroid
   and bbox semantics.

Then split the unique-assignment path before full bbox-cell materialization.

For `Building` and `BuildingPart`:

1. Compute selected geometry stats as today.
2. Count selected vertices by located grid cell.
3. Pick the max-score cell directly.
4. Preserve deterministic tie-breaking.
5. Return only that one `(CellId, Cell)` assignment.
6. Skip `CellCounts::merge_vertex_counts_with_bbox`.

More aggressive version:

1. Detect unique assignment before `count_vertices_in_grid`.
2. For unique assignment, do not create `CellCounts`.
3. If selected vertex cells exist, choose the best vertex cell directly.
4. Only consult bbox cells if the current semantics require a fallback for an
   otherwise empty vertex-cell map.

This preserves the current practical output for ordinary building features
where selected vertices exist, while avoiding materializing bbox-only cells that
are discarded immediately.

Expected benefit:

- reduces per-building allocations,
- reduces `BTreeMap` or replacement map mutations,
- removes bbox-range enumeration for unique assignment,
- should scale well on the 11 million feature input if buildings dominate.

Expected evidence after implementation:

- fewer `cfree`, `_int_free`, `malloc`, and `malloc_consolidate` samples,
- fewer `native_queued_spin_lock_slowpath` samples caused by allocator locks,
- lower time in the logged `Counting vertices in grid cells` stage,
- no meaningful change in projection or JSON parsing costs.

## Tie-Breaking

Current unique assignment uses:

```rust
cell_vtx_cnt.iter().max_by(|a, b| a.1.cmp(b.1))
```

Rust iterator `max_by` returns the last maximum for equal comparisons. Since
`CellCounts` entries are sorted by `CellId`, equal scores currently select the
greatest `CellId` among tied cells.

Any optimized path should preserve that behavior unless intentionally changed.

## Instrumentation Needed In Next Run

Add timing logs inside `World::index_with_grid` around:

- page scan / model loading,
- `selected_geometry_stats`,
- vertex-cell counting,
- bbox-cell merge,
- unique assignment max selection,
- `integrate_feature_in_cells`,
- page-level total.

Also log counters:

- number of features processed,
- selected vertex count histogram or buckets,
- bbox-intersecting cell count histogram or buckets,
- number of unique-assignment vs multi-cell-assignment features,
- number of large-feature parallel-path calls.

This will make the next buildings run actionable even if collapsed stacks still
lose some Rust call-path detail.

## Longer-Term Direction

`docs/adr/001-use-segment-grid-traversal-for-feature-cell-ownership.md` already
accepts replacing bbox fallback with segment-to-grid traversal.

That is the better semantic fix for ownership scoring. It avoids bbox inflation
and derives candidate cells from actual geometry coverage. However, it is a
larger implementation than the direct unique-assignment optimization.

Recommended order:

1. Add timing instrumentation.
2. Optimize unique-assignment buildings by avoiding full bbox-cell
   materialization.
3. Re-profile the buildings-only run.
4. Implement segment-to-grid traversal if ownership quality or bbox overhead
   still matters after step 2.

## Test Plan For The Optimization

Add tests for the unique-assignment fast path:

- one vertex cell,
- multiple vertex cells with different counts,
- equal-count tie cells,
- large bbox with many bbox-only cells,
- behavior parity with current unique assignment for representative building
  features.

Keep existing `count_vertices_in_grid_*` tests for the multi-cell path.

Run:

```bash
cargo test
```
