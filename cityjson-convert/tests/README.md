# Test ownership

The converter test suite is deliberately split by observable contract:

- `cjconvert_gpkg.rs` is CLI acceptance coverage. It uses the immutable
  `cityjson-corpus` fixtures and checks GeoPackage options and sidecars.
- `gpkg_writer.rs` covers writer edge cases with small inline CityJSON fixtures:
  layer reuse, schema isolation, NULL attributes, identifier collisions, and
  source CRS rejection.
- `tabular.rs`, `tsv_writer.rs`, and `gltf_writer_geometry.rs` cover their
  corresponding tabular, TSV, and glTF contracts.
- `src/gpkg_writer.rs` unit tests cover GeoPackage binary headers, type mapping,
  extents, identifiers, and EPSG parsing.
- `tools/gis-integration` is the GDAL-only interoperability suite. GEOS and
  PostGIS codec interoperability belongs to `cityjson-types`.

Do not edit corpus fixtures to make an exporter test pass. Add a small inline
fixture only when an edge case cannot be represented by the corpus. Every new
test needs a doc comment stating its purpose, input, and assertions.

Useful commands:

```shell
cargo test -p cityjson-convert --test cjconvert_gpkg
cargo test -p cityjson-convert --test gpkg_writer
cargo test -p cityjson-convert gpkg_writer::tests
just gpkg-gis-test
```
