# 3D Tiles Coordinate Frame Implementation Plan

## Goal

Implement the coordinate model from ADR 003:

- `boundingVolume.region` is the only bounding volume type for 3D Tiles output.
- Regions are absolute EPSG:4979 bounds.
- GLB mesh positions are local ENU meters.
- The root tile transform maps root ENU to ECEF.
- glTF Y-up handling is explicit and tested.

## Current Problems to Remove

1. `Tileset::from_quadtree()` creates one transformer to EPSG:4979 and passes
   it into `BoundingVolume::box_from_bbox()`, even though `box_from_bbox()`
   documents that its transformer must target EPSG:4978.
2. The GLB writer can reproject vertices to ECEF-relative coordinates and then
   pass those coordinates through `build_node_matrix()`, which applies a local
   Z-up to glTF Y-up axis conversion.
3. The transform contract is spread across `src/formats.rs`, `src/main.rs`,
   and `cityjson-convert/src/gltf_writer.rs` without a shared type describing
   the intended frame.

## Phase 1: Introduce Coordinate Frame Types

Add a small coordinate-frame module, either in `src/coordinates.rs` or inside
`src/formats.rs` until it deserves a separate module.

Implement:

```rust
pub struct RootEnuFrame {
    pub source_origin: [f64; 3],
    pub geodetic_origin: [f64; 3],
    pub ecef_origin: [f64; 3],
    pub east: [f64; 3],
    pub north: [f64; 3],
    pub up: [f64; 3],
}
```

Responsibilities:

- Compute the root source origin from the quadtree/world bbox center.
- Transform root source origin to EPSG:4979 for longitude, latitude, height.
- Transform root source origin to EPSG:4978 for ECEF translation.
- Build ENU basis vectors from longitude and latitude.
- Build the 3D Tiles root transform in column-major order:

```text
[
  east.x,  east.y,  east.z,  0,
  north.x, north.y, north.z, 0,
  up.x,    up.y,    up.z,    0,
  O.x,     O.y,     O.z,     1
]
```

Add unit tests for:

- basis vectors are unit length
- basis vectors are mutually orthogonal
- `east x north == up` within tolerance
- the transform translation equals the ECEF origin

## Phase 2: Convert GLB Export Options to Use ENU

Replace the current GLB placement options:

```rust
source_crs: Option<String>,
ecef_origin: Option<[f64; 3]>,
reproject_to_ecef: bool,
```

with a clearer placement enum in `cityjson-convert`:

```rust
pub enum GeometryPlacement {
    SourceCoordinates,
    EcefRelative {
        source_crs: String,
        origin: [f64; 3],
    },
    Enu {
        source_crs: String,
        ecef_origin: [f64; 3],
        east: [f64; 3],
        north: [f64; 3],
        up: [f64; 3],
    },
}
```

If changing the public API that much is too disruptive, add `enu_frame` first
and keep the old fields deprecated internally until the migration is complete.

GLB conversion for `GeometryPlacement::Enu`:

1. Read source vertex in source CRS.
2. Transform to EPSG:4978.
3. Compute `delta = ecef - ecef_origin`.
4. Store local Z-up ENU coordinates:

```text
x = dot(delta, east)
y = dot(delta, north)
z = dot(delta, up)
```

5. Center and quantize the local coordinates as the writer already does.
6. Use the standard glTF Z-up-to-Y-up root node matrix for local source data.

Add tests in `cityjson-convert/tests/gltf_writer_geometry.rs`:

- a synthetic fixture near the root origin maps to near-zero ENU coordinates
- moving the source point east increases local X
- moving the source point north increases local Y
- moving the source point up increases local Z
- the GLB node matrix uses the expected Z-up-to-Y-up convention and does not
  encode ECEF axis swapping

## Phase 3: Make Tileset Bounds Use Regions

Update `src/formats.rs`:

1. Replace leaf `BoundingVolume::box_from_bbox(...)` calls with
   `BoundingVolume::region_from_bbox(...)`.
2. Replace content `BoundingVolume::box_from_bbox(...)` calls with
   `BoundingVolume::region_from_bbox(...)`.
3. Keep internal/root tile regions as regions.
4. Remove or quarantine `box_from_bbox()` if no longer used by 3D Tiles output.

Improve `region_from_bbox()`:

1. Enumerate all eight corners of the source bbox.
2. Transform each corner to EPSG:4979.
3. Compute min/max longitude, latitude, and height.
4. Convert longitude and latitude to radians.
5. Return `BoundingVolume::Region([west, south, east, north, min_h, max_h])`.

Add tests:

- `region_from_bbox()` transforms all eight corners.
- output order is `[west, south, east, north, minHeight, maxHeight]`.
- longitude and latitude are radians.
- root/content/tile bounding volumes in generated tilesets are all `region`.

## Phase 4: Use Root ENU in 3D Tiles Export

Update `src/main.rs`:

1. Replace `compute_root_ecef_origin()` with `compute_root_enu_frame()`.
2. Pass the root ENU frame to `Tileset::from_quadtree()`.
3. Pass the same root ENU frame to `build_glb_export_options()`.
4. Remove ad hoc `ecef_origin` usage from the 3D Tiles export path.

Update `Tileset::from_quadtree()`:

1. Use the root ENU frame transform as the root tile transform.
2. Keep root/tile/content bounding volumes as absolute EPSG:4979 regions.
3. Ensure implicit tiling keeps one root transform and no per-tile transform.

Add integration coverage using `tests/output/debug`:

- replay debug data into a temporary output
- assert `tileset.json.root.transform` is not translation-only
- assert `tileset.json.root.boundingVolume.region` exists
- assert implicit root content URI remains `t/{level}/{x}/{y}.glb`
- assert generated GLBs exist and contain local-scale transforms, not global
  ECEF translations

## Phase 5: Validate and Clean Up

Run:

```shell
just fmt
just test
```

If available, also run a 3D Tiles validator against generated output.

Manual viewer checks:

1. Generate the explicit tileset from the repository fixture/debug data.
2. Generate the implicit tileset from the same data.
3. Load both in the target 3D Tiles viewer.
4. Confirm content appears inside its EPSG:4979 region.
5. Confirm switching `--3dtiles-content-add-bv` does not move content.
6. Confirm `--3dtiles-content-clip-to-tile-bounds` clips geometry without
   changing placement.

## Implementation Order

1. Add `RootEnuFrame` and tests.
2. Improve `region_from_bbox()` and switch 3D Tiles bounds to regions.
3. Add ENU placement to `cityjson-convert`.
4. Wire `RootEnuFrame` through Tyler's 3D Tiles export.
5. Add GLB and tileset regression tests.
6. Remove obsolete ECEF-relative code paths only after tests cover the new
   placement contract.

## Non-Goals

- Do not implement per-tile ENU frames for implicit tiling.
- Do not store GLB positions as EPSG:4979 longitude/latitude/height.
- Do not change input CRS detection or cjindex behavior.
- Do not solve vertical datum conversion beyond using the transformation that
  PROJ provides for the configured source CRS.
