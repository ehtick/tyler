# Use Geographic Implicit Tiling for Region Bounds

## Status

Accepted

## Related Commits

- 7ba9bef04805753d2e8ee3bb762c891d2431cf12
- ad853afd987089c6907e70d82055593440bf4b6c

## Context

Tyler builds 3D Tiles from a source-CRS grid and quadtree. This works well for
explicit tilesets because every explicit tile writes its own transformed
`boundingVolume.region`. The tile ID may come from a projected source CRS grid,
but the tile boundary in `tileset.json` is computed for that exact tile by
transforming its source bbox to EPSG:4979.

Implicit tiling is different. An implicit tileset writes one root bounding
volume and the viewer derives child tile bounds by subdivision. For
`boundingVolume.region`, the subdivision axes are geographic:

```text
x = longitude subdivision
y = latitude subdivision
height range is inherited by quadtree children
```

After moving implicit root bounds from `box` to `region`, Tyler still mapped
implicit content URIs from the source-CRS quadtree IDs:

```text
t/{level}/{x}/{y}.glb
```

This created a mismatch:

- GLB content was placed correctly on the globe using the shared root ENU frame
- implicit tile bounds were derived by longitude/latitude subdivision
- content tile IDs still represented source-CRS grid subdivision

The result in viewers such as Cesium was that GLB content and implicit tile
bounds did not line up.

There was also a separate refinement issue during the first geographic tiling
implementation. Content could appear on both parent and child implicit tiles.
With `refine: "REPLACE"`, the viewer may drop parent content when child content
is selected. If parent content is not a deliberate lower-detail replacement for
complete child content, this can make geometry disappear while zooming in. A
brief deepest-level-only fix avoided that, but it could create too many GLB
tiles for large outputs. An ancestor-merging fix was also rejected because it
could create overly large content tiles. Tyler now uses additive refinement for
implicit tiles instead.

## Decision

For implicit 3D Tiles that use `boundingVolume.region`, Tyler will assign
content to geographic implicit tile IDs.

Concretely:

1. Explicit tiling remains source-CRS based.
   - The source grid and quadtree remain the internal tiling model.
   - Each explicit tile writes its own transformed EPSG:4979 region.
   - This preserves the source-CRS tiling path for explicit 3D Tiles and future
     non-3D-Tiles output formats.

2. Implicit 3D Tiles use geographic content tile IDs.
   - The root implicit region is derived from the root source bbox transformed
     to EPSG:4979.
   - Feature bounds are transformed to geographic longitude/latitude bounds for
     implicit tile assignment.
   - A content URI such as `t/8/123/456.glb` means geographic implicit tile
     `(level=8, x=123, y=456)`, where `x` subdivides longitude and `y`
     subdivides latitude.

3. GLB placement remains unchanged.
   - Source geometry is read in the source CRS.
   - GLB vertices are written in the shared root ENU frame.
   - The root tile transform places that ENU frame in ECEF.

4. Implicit tiles use additive refinement.
   - Content is assigned at the corresponding source leaf level.
   - Parent and child content may both be available in the implicit hierarchy.
   - The implicit root tile template is serialized with `refine: "ADD"`.
   - `refine: "ADD"` prevents parent content from disappearing when child
     content is selected.

5. Building and BuildingPart unique assignment is preserved.
   - For `Building` and `BuildingPart`, each feature is assigned to one
     geographic tile at the source leaf level by transformed centroid.
   - Other feature types can still be assigned to all geographic tiles at the
     source leaf level intersecting their transformed bbox.

6. Geographic clipping is deferred.
   - `--3dtiles-content-clip-to-tile-bounds` still applies to explicit
     source-CRS tiles.
   - For geographic implicit tiling, Tyler logs a warning and does not clip.
   - Exact clipping would require clipping against geographic tile boundaries or
     projected tile polygons, not the current source-CRS axis-aligned bbox.

## Consequences

Good:

- implicit tile IDs now match the subdivision space of `boundingVolume.region`
- GLB content and implicit tile bounds line up in Cesium
- source-CRS tiling remains available for explicit output and future formats
- `region` avoids the large-area planar-box curvature problem
- additive refinement avoids parent content disappearing when child content is
  selected
- BuildingPart output does not explode into bbox-intersection duplication

Trade-offs:

- implicit 3D Tiles now have a separate assignment path from explicit tiles
- feature bboxes must be transformed to EPSG:4979 for geographic implicit
  assignment
- transformed bbox corners are an approximation for large or curved features
- quadtree region heights are still inherited by all implicit descendants
- missing vertical projection grids can still produce poor region height values
- geographic clipping remains a future task
- additive refinement can show duplicate geometry if a feature is assigned to
  both an ancestor and descendant content tile

## Rejected Alternatives

- Keep using source-CRS tile IDs for implicit `region` tiles.
  This keeps one tile ID system but is semantically wrong for implicit region
  subdivision. The viewer derives child bounds in longitude/latitude space, not
  source-CRS grid space.

- Switch implicit tiles back to `boundingVolume.box`.
  A box can be made to align better with source-CRS grid axes, but large-area
  boxes need curvature correction and are less natural for geographic viewers.
  The move to `region` is still the right model for global bounds.

- Force all implicit content to the deepest level.
  This avoids parent/child refinement conflicts with `REPLACE`, but it can
  create many more GLB tiles than the source-leaf hierarchy.

- Merge descendant content into ancestor content tiles.
  This also avoids parent/child refinement conflicts with `REPLACE`, but it can
  create very large content tiles when a high-level ancestor overlaps many
  descendants.

- Reproject all feature geometry before tiling.
  Tyler only needs geographic bounds and centroid-like placement for assignment.
  Reprojecting and storing all vertices would duplicate work already done by the
  GLB exporter and would make the indexing path heavier.

- Project geographic tile bounds back to the source CRS for assignment.
  Geographic tile rectangles do not generally become axis-aligned source-CRS
  rectangles. A projected source-CRS bbox would be conservative and can
  over-assign; exact projected polygon intersection is a larger clipping/indexing
  problem.

## Notes

The current geographic implicit assignment transforms bbox corners. This is a
reasonable first implementation for compact building features. For large linear
or area features, Tyler may need bbox-edge densification or direct selected
vertex transformation to avoid underestimating geographic bounds.

The implementation logs the number of source features, content tiles, and
feature-tile assignments for geographic implicit tiling. This is useful for
spotting accidental duplication or unexpectedly large implicit output.

Future work:

- add geographic clipping for implicit tile content
- improve geographic bounds for large projected bboxes
- add warnings for suspicious EPSG:4979 height ranges
- decide whether very large datasets need multi-subtree implicit output instead
  of a single root subtree
