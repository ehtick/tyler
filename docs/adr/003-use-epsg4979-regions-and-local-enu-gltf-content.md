# Use EPSG:4979 Regions and Local ENU glTF Content for 3D Tiles

## Status

Accepted

## Related Commits

- none yet

## Context

Tyler writes 3D Tiles with glTF/GLB tile content. The output has to satisfy
two different coordinate requirements:

- tile bounding volumes should be geospatially meaningful and viewer-friendly
- GLB geometry should be stored in a numerically stable metric coordinate frame

The 3D Tiles specification allows `boundingVolume.region` to be defined
directly in EPSG:4979. Regions use longitude and latitude in radians and
height in meters above or below the WGS 84 ellipsoid. A tile `transform` does
not apply to `region` bounding volumes because those regions are already
globally georeferenced.

glTF, on the other hand, stores mesh positions as linear coordinates in meters
and uses a right-handed Y-up coordinate system. EPSG:4979 longitude and
latitude are angular coordinates, so they are not suitable as glTF vertex
positions.

Tyler previously mixed these concerns. The tileset can carry an ECEF root
translation, while the GLB writer can also reproject geometry to ECEF-relative
coordinates and then apply a glTF root node matrix intended for local Z-up
source data. That makes the placement contract hard to reason about and can
place GLB content away from its intended 3D Tiles location.

## Decision

Tyler will use EPSG:4979 `region` bounding volumes and local ENU metric glTF
content.

Concretely:

1. Tile and content bounding volumes will use `boundingVolume.region` created
   from source-coordinate bounding boxes with `region_from_bbox()`.
2. `region_from_bbox()` will transform source CRS bounds to EPSG:4979 and
   write `[west, south, east, north, minHeight, maxHeight]`.
3. GLB mesh positions will not be written in EPSG:4979.
4. GLB mesh positions will be written in a local East-North-Up frame in meters.
5. The tileset or tile `transform` will map the local ENU frame to ECEF
   EPSG:4978.
6. For implicit tiling, Tyler will use one root ENU frame for the implicit
   tileset because implicit content URIs are templated and per-tile transforms
   are not available in subtree availability.
7. For explicit tiling, Tyler may use the same root ENU frame initially for
   consistency. Per-tile ENU origins can be introduced later if precision or
   file-level independence requires it.
8. The GLB writer will make the glTF Y-up requirement explicit. If Tyler keeps
   source mesh data in Z-up ENU coordinates inside the GLB buffers, the GLB
   root node must contain the standard Z-up-to-Y-up matrix so that the 3D Tiles
   runtime Y-up-to-Z-up transform cancels it.

The initial implementation target is root ENU for both explicit and implicit
tilesets. This gives one coherent placement model:

```text
source CRS vertex
  -> EPSG:4978 ECEF
  -> root-local ENU meters
  -> glTF Y-up encoding
  -> 3D Tiles runtime glTF Y-up-to-Z-up
  -> root tile.transform ENU-to-ECEF
```

## Consequences

Good:

- `boundingVolume.region` remains absolute EPSG:4979 and is not affected by
  tile transforms.
- GLB geometry is stored in meters, not angular coordinates.
- Local ENU coordinates keep vertex magnitudes small enough for f32 and
  quantization.
- The root transform has a clear geospatial meaning: local ENU to ECEF.
- The same design works for implicit tiling, where a single root transform is
  the natural placement anchor.
- The code can avoid using ECEF axes as if they were local Z-up axes.

Trade-offs:

- Tyler must compute and preserve a root geodetic origin and its ENU basis.
- Source heights must be handled deliberately. EPSG:4979 heights are
  ellipsoidal; source datasets with a different vertical datum may still carry
  a systematic vertical offset unless the source-to-EPSG:4979 transformation
  handles the vertical datum correctly.
- A single root ENU frame introduces small orientation error over very large
  extents because ENU is tangent at one origin. This is acceptable for Tyler's
  current tile extents and can be revisited with per-tile ENU transforms for
  explicit tilesets if needed.
- glTF Y-up handling remains subtle and must be tested. The transform pair must
  cancel in 3D Tiles runtimes.

## Rejected Alternatives

- Store GLB vertices directly in EPSG:4979.
  This is wrong for glTF because longitude and latitude are angular, while
  glTF positions are linear meters.

- Store GLB vertices directly in global ECEF coordinates.
  This creates large coordinate magnitudes, poor f32 precision, and a confusing
  interaction with glTF Y-up transforms.

- Store GLB vertices as ECEF-relative deltas and apply the existing Z-up to
  Y-up matrix unchanged.
  ECEF axes are global Cartesian axes, not a local East-North-Up frame. Treating
  ECEF deltas like local Z-up data rotates them incorrectly.

- Keep `boundingVolume.box` for tile bounds while using EPSG:4979 elsewhere.
  This is not aligned with the desired output. Regions are more appropriate for
  geospatial tile bounds and are explicitly supported by 3D Tiles.

- Use per-tile ENU frames for implicit tiling.
  Implicit tiling does not provide a simple per-content tile transform in the
  content URI template. A root ENU frame is simpler and matches the implicit
  tileset structure.

## Notes

The implementation should improve `region_from_bbox()` so it transforms all
eight source bbox corners, not just two opposite corners. The region should be
the min/max of all transformed longitudes, latitudes, and heights. This avoids
underestimating the region when CRS transformation is not perfectly monotonic
over the bbox.

Relevant specifications:

- 3D Tiles specification, coordinate systems, regions, transforms, and glTF
  transforms:
  <https://github.com/CesiumGS/3d-tiles/blob/main/specification/README.adoc>
- glTF 2.0 coordinate system:
  <https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html>
