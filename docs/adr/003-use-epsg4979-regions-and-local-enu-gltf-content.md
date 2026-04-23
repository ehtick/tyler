# Use EPSG:4979 Regions and Local ENU glTF Content for 3D Tiles

## Status

Accepted

## Related Commits

- `f4503c4` - Implement root ENU coordinate frame for 3D Tiles export
- `13e5a79` - Fix GLB validation for 3D Tiles content

## Short Version

Tyler writes a 3D Tiles tileset and a set of GLB files. These two parts need
different coordinate systems.

The tileset JSON should describe where tiles are on Earth. For that, we use
`boundingVolume.region`, which is defined in EPSG:4979:

```text
[west, south, east, north, minHeight, maxHeight]
```

Here, west/south/east/north are longitude and latitude in radians, and the
heights are meters above the WGS 84 ellipsoid.

The GLB files should not store longitude and latitude. glTF mesh positions are
plain 3D coordinates in meters. Longitude and latitude are angles, not meters.
Instead, Tyler stores GLB vertices in a local East-North-Up coordinate frame,
usually shortened to ENU:

```text
x = meters east from the root origin
y = meters north from the root origin
z = meters up from the root origin
```

The root tile transform then places that local ENU model on Earth by converting
it to EPSG:4978 ECEF coordinates.

In other words:

```text
tileset bounding volumes: EPSG:4979 regions
GLB vertex coordinates:   local ENU meters
root tile transform:      local ENU -> EPSG:4978 ECEF
```

This is the coordinate contract Tyler should use.

## Problem

The original symptom was that GLB files were created, but did not show up in
the expected location in a 3D Tiles viewer.

There were two separate classes of problems that can produce that symptom:

1. The coordinates can be wrong.
2. The GLB files can be invalid, causing the viewer to reject the content.

Both matter. A correct tileset transform is not enough if the GLB is invalid.
Likewise, a valid GLB is not enough if it is placed in the wrong coordinate
frame.

## Why EPSG:4979 Belongs in Bounding Volumes

`boundingVolume.region` is already a global geographic bounding volume. It is
not a local model-space box.

Example region:

```json
{
  "boundingVolume": {
    "region": [
      0.0853901,
      0.9120123,
      0.0854127,
      0.9120279,
      4.2,
      38.7
    ]
  }
}
```

This means:

```text
west longitude  = 0.0853901 radians
south latitude  = 0.9120123 radians
east longitude  = 0.0854127 radians
north latitude  = 0.9120279 radians
minimum height  = 4.2 meters
maximum height  = 38.7 meters
```

The viewer can use this directly to decide whether the tile is visible. The
tile transform does not move this region. The region is already on Earth.

That is why `region_from_bbox()` should transform the source bounding box to
EPSG:4979 and write a region.

## Why EPSG:4979 Does Not Belong in GLB Vertices

glTF positions are linear coordinates. They are interpreted as distances in a
3D Cartesian space.

This is wrong:

```text
vertex = [longitude, latitude, height]
```

For example:

```text
vertex = [4.895, 52.370, 12.0]
```

That looks like Amsterdam in degrees, but glTF does not know that. A glTF
viewer reads it as:

```text
x = 4.895 meters
y = 52.370 meters
z = 12.0 meters
```

If radians are used instead, it is still wrong:

```text
vertex = [0.0854, 0.9140, 12.0]
```

The first two numbers are angles, but glTF still treats them as meters. The
building becomes a tiny object near the local origin, not a building at its
real position on Earth.

## Why Global ECEF Does Not Belong Directly in GLB Vertices

EPSG:4978 ECEF is a global Cartesian coordinate system centered at the center
of the Earth. A point near the Netherlands has coordinates with magnitudes of
millions of meters.

Example shape of an ECEF coordinate:

```text
vertex = [3899000.0, 348000.0, 5029000.0]
```

This is a valid global Earth coordinate, but it is a poor GLB vertex position:

- glTF positions are usually stored as 32-bit floats.
- 32-bit floats lose useful precision when the numbers are very large.
- Small building details become hard to represent accurately.
- glTF Y-up handling becomes easy to mix up with global ECEF axes.

The issue is not that ECEF is geospatially wrong. ECEF is useful for the final
placement transform. The issue is that ECEF is the wrong space for compact,
precise mesh vertices.

## The Local ENU Model

ENU means East-North-Up. It is a local coordinate frame tangent to the Earth at
one chosen origin.

Imagine putting a small coordinate tripod on the ground near the center of the
tileset:

```text
east  = local +X
north = local +Y
up    = local +Z
```

If a building corner is 23 meters east, 8 meters north, and 2 meters above the
root origin, Tyler stores it like this before glTF axis conversion:

```text
vertex = [23.0, 8.0, 2.0]
```

Those are small, local, metric numbers. That is what GLB mesh data is good at
storing.

The tileset then supplies a root transform that says:

```text
take this local ENU coordinate system and place it on Earth
```

So a vertex goes through this path:

```text
source CRS vertex
  -> EPSG:4978 ECEF
  -> root-local ENU meters
  -> glTF encoding
  -> 3D Tiles runtime placement
  -> final position on Earth
```

## Example: Correct Placement

Assume the root ENU origin is near the center of the dataset.

A building corner in the source dataset is transformed to ECEF. Tyler subtracts
the root ECEF origin and projects the difference onto the local ENU axes.

The result might be:

```text
ENU vertex = [12.4, -3.8, 6.1]
```

Meaning:

```text
12.4 meters east of the root origin
3.8 meters south of the root origin
6.1 meters above the root origin
```

The GLB stores this small local coordinate. The root tile transform maps the
local ENU axes back to global ECEF. The viewer renders the building at the
right location on Earth.

This is the desired model.

## Example: Wrong Placement by Double Georeferencing

A common mistake is to georeference the same geometry twice.

For example:

```text
GLB vertices are already ECEF-relative
and
the root tile transform also treats them as local ENU
```

That is wrong because ECEF-relative axes are not the same as local east, north,
and up axes.

ECEF axes point in fixed global directions from the center of the Earth. ENU
axes rotate depending on where the dataset is on Earth. Treating ECEF deltas as
if they were ENU vectors rotates the model incorrectly.

The symptom can be that the tile exists, the GLB exists, and the bounding
region looks reasonable, but the rendered content appears far away, underground,
or not visible in the expected camera view.

## Example: Wrong Placement by Using Degrees in GLB

Another mistake is to store source geographic coordinates directly in the GLB:

```text
vertex = [4.895, 52.370, 12.0]
```

The writer may intend:

```text
longitude = 4.895 degrees
latitude  = 52.370 degrees
height    = 12.0 meters
```

But glTF sees:

```text
x = 4.895 meters
y = 52.370 meters
z = 12.0 meters
```

The mesh is no longer geospatially meaningful. A tileset transform cannot
magically recover the fact that the first two values were angles.

## glTF Y-Up and 3D Tiles Z-Up

There is one more axis issue.

glTF uses a right-handed Y-up coordinate system. 3D Tiles uses glTF content but
places it into a geospatial Z-up runtime environment.

Tyler's local ENU frame is naturally Z-up:

```text
east  = X
north = Y
up    = Z
```

If Tyler keeps this Z-up layout in the GLB buffers, the GLB root node needs the
standard Z-up-to-Y-up matrix. Then the 3D Tiles runtime applies the matching
Y-up-to-Z-up handling for glTF content. The two transforms cancel each other
for the local model orientation.

The practical rule is:

```text
do not silently mix ECEF axes, ENU axes, and glTF Y-up axes
```

The code must make the axis conversion explicit.

## Why `region_from_bbox()` Must Use All 8 Corners

A source bounding box has two opposite corners:

```text
min = [min_x, min_y, min_z]
max = [max_x, max_y, max_z]
```

It is tempting to transform only those two corners to EPSG:4979 and use the
result as the region. That can underestimate the real bounds.

The safer approach is to transform all 8 corners:

```text
[min_x, min_y, min_z]
[min_x, min_y, max_z]
[min_x, max_y, min_z]
[min_x, max_y, max_z]
[max_x, min_y, min_z]
[max_x, min_y, max_z]
[max_x, max_y, min_z]
[max_x, max_y, max_z]
```

Then Tyler takes the minimum and maximum longitude, latitude, and height from
those transformed points.

This matters because CRS transformations are not guaranteed to behave like a
simple axis-aligned scale and translation over the whole bbox. Transforming all
8 corners avoids accidentally creating a region that is too small.

## Why Implicit Tiling Uses One Root ENU Frame

Explicit tiles can, in theory, each have their own tile transform.

Implicit tiling is different. It uses subtree availability and templated
content URIs. There is no simple per-content transform attached to each
available implicit tile in the same way.

So Tyler uses one ENU frame for the root of the implicit tileset:

```text
one root ENU origin
one root ENU-to-ECEF transform
all GLB content coordinates are relative to that root ENU frame
```

This is simple and coherent. It also keeps the GLB coordinates small for the
current Tyler output sizes.

For very large datasets, a single ENU tangent plane slowly becomes less exact
as distance from the root origin grows. If that becomes a real precision issue,
explicit tilesets could later support per-tile ENU origins. That is not the
initial design.

## The GLB Validity Issue Found During Debugging

While debugging the original viewer problem, the output files in
`tests/output` showed another important issue: the GLB files themselves were
invalid.

The tileset structure could be traversed, but `3d-tiles-validator` reported
content errors for every GLB.

The main problems were:

- Quantized `POSITION` accessors used `SHORT` components with
  `normalized: true`, but their accessor `min` and `max` values were written as
  normalized floating-point values.
- glTF requires accessor `min` and `max` to match the accessor component type.
  For `SHORT`, that means integer bounds, not normalized float bounds.
- Primitives used `EXT_mesh_features`, but the extension was not always listed
  in top-level `extensionsUsed`.
- Some primitives referenced `propertyTable: 0` even when there was no
  `EXT_structural_metadata` property table.

Example of the wrong kind of accessor bounds:

```json
{
  "componentType": 5122,
  "type": "VEC3",
  "normalized": true,
  "min": [-0.99, -0.87, -0.34],
  "max": [0.94, 0.91, 0.28]
}
```

`5122` means `SHORT`. Since the stored values are signed 16-bit integers, the
accessor bounds must also be integers:

```json
{
  "componentType": 5122,
  "type": "VEC3",
  "normalized": true,
  "min": [-32440, -28507, -11141],
  "max": [30801, 29818, 9175]
}
```

The `normalized: true` flag tells glTF how to interpret those integers at
runtime. It does not change the JSON type required for `min` and `max`.

This was important because a strict viewer may reject invalid GLB content
before it ever draws the model. From the outside, that can look like a
coordinate placement bug:

```text
tileset.json exists
GLB files exist
bounding regions look plausible
viewer shows no content
```

But the real failure can be:

```text
viewer refused to load invalid GLB
```

So GLB validity is part of the placement contract.

## Decision

Tyler will use this coordinate and validity model:

1. Tile and content bounding volumes use `boundingVolume.region`.
2. `region_from_bbox()` creates those regions in EPSG:4979.
3. `region_from_bbox()` transforms all 8 source bbox corners.
4. GLB mesh positions are not written in EPSG:4979.
5. GLB mesh positions are written in local ENU meters.
6. The root tile transform maps local ENU coordinates to EPSG:4978 ECEF.
7. Implicit tilesets use one root ENU frame.
8. Explicit tilesets initially use the same root ENU model for consistency.
9. glTF Y-up handling is explicit and tested.
10. Generated GLBs must be valid glTF 2.0 assets.
11. Quantized accessor `min` and `max` values must match the raw component
    type.
12. Every used glTF extension must be listed in `extensionsUsed`.
13. Mesh feature IDs may only reference a property table when that table
    exists.

The full intended path is:

```text
source CRS vertex
  -> EPSG:4978 ECEF
  -> root-local ENU meters
  -> glTF Y-up encoding
  -> 3D Tiles runtime glTF handling
  -> root tile.transform ENU-to-ECEF
  -> final Earth position
```

## Consequences

Good consequences:

- Bounding volumes are easy for geospatial viewers to understand.
- Regions remain absolute EPSG:4979 values and are not accidentally moved by
  tile transforms.
- GLB vertices are stored as small meter values instead of large global
  coordinates.
- 32-bit float precision and quantization behave better.
- The root transform has one clear meaning: local ENU to global ECEF.
- The same model works for implicit tiling.
- The code no longer has to pretend that ECEF-relative axes are local Z-up
  axes.
- Viewer debugging becomes clearer because coordinate problems and GLB validity
  problems can be checked separately.

Trade-offs:

- Tyler must compute and keep a root geodetic origin.
- Tyler must compute the ENU basis vectors for that origin.
- Source heights need care. EPSG:4979 heights are ellipsoidal heights. If the
  input data uses a different vertical datum, Tyler can still have a vertical
  offset unless the CRS transformation accounts for it.
- A single root ENU frame is an approximation over large areas. The farther
  away from the root origin a tile is, the less perfectly the tangent plane
  matches the curved Earth.
- glTF Y-up handling is subtle and needs regression tests.
- Generated output should be checked with `3d-tiles-validator` when debugging
  viewer placement issues.

## Rejected Alternatives

### Store GLB vertices directly in EPSG:4979

Rejected because longitude and latitude are angles, while glTF positions are
linear coordinates.

Example wrong GLB vertex:

```text
[longitude, latitude, height]
```

glTF will read that as:

```text
[meters_x, meters_y, meters_z]
```

### Store GLB vertices directly in global ECEF

Rejected because the coordinates are too large for good mesh precision and make
axis handling confusing.

ECEF belongs in the placement transform, not in every mesh vertex.

### Store ECEF-relative vertices and treat them as local Z-up

Rejected because ECEF-relative axes are not local ENU axes.

An ECEF delta vector is measured in global Earth-centered axes. A local ENU
vector is measured in east, north, and up directions at the dataset origin.
Those are different frames.

### Use `boundingVolume.box` instead of EPSG:4979 regions

Rejected for Tyler's 3D Tiles output because regions are the clearer
geospatial bounding volume. They directly express longitude, latitude, and
height.

### Use per-tile ENU frames for implicit tiling

Rejected for the initial implementation because implicit tiling does not give
us a simple per-content transform in the templated availability structure.

One root ENU frame is simpler, easier to validate, and matches the current
implicit tileset structure.

## Validation Checklist

When GLB files are created but do not show up in the viewer, check these in
order:

1. Run `3d-tiles-validator` on `tileset.json`.
2. Fix any GLB content validation errors first.
3. Check that tile and content bounding volumes are EPSG:4979 regions.
4. Check that the root tile transform is an ENU-to-ECEF transform.
5. Check that GLB vertex magnitudes are small local meter values, not
   longitude/latitude or global ECEF values.
6. Check that quantized accessor `min` and `max` values match the accessor
   component type.
7. Check that all used glTF extensions are declared in `extensionsUsed`.
8. Check that feature IDs only reference metadata property tables that exist.

## Relevant Specifications

- 3D Tiles specification, coordinate systems, regions, transforms, and glTF
  transforms:
  <https://github.com/CesiumGS/3d-tiles/blob/main/specification/README.adoc>
- glTF 2.0 coordinate system:
  <https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html>
