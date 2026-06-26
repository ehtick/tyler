I would implement this by separating “logical tabular adapters” from “physical writers”. The current [tabular.rs](/home/balazs/Development/tyler/cityjson-convert/src/tabular.rs:1) already has the
right low-level
pieces: `TableSchema`, `ColumnSchema`, `LogicalType`, `Row`, `Value`, and borrowed/lazy value resolution. I would generalize those pieces just enough to support multiple row kinds, not create a
generic framework
for every future output.

The key split:

```text
tabular/
  mod.rs              shared schema/value machinery
  cityobjects.rs      CityObject logical table
  hierarchy.rs        reusable parent/child list projection
  metadata.rs         one-row metadata logical table
  semantics.rs        semantic definition table
  semantic_rows.rs    optional semantic assignment/surface table later
```

## 1. Shared format-agnostic table contract

I would keep the current dynamic attribute inference, but make it reusable for any source that exposes `attributes` / `extra` maps.

Conceptually:

```rust
pub trait LogicalTable<'model> {
    type Row<'table>
    where
        Self: 'table,
        'model: 'table;

    fn schema(&self) -> &TableSchema<'model>;

    fn rows(&self) -> Box<dyn Iterator<Item=Self::Row<'_>> + '_>;
}
```

But I would probably avoid boxing/generic traits at first unless needed. Simpler: each table type has the same shape:

```rust
pub struct CityObjectTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

pub struct SemanticTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

pub struct MetadataTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}
```

The shared part should be lower-level helpers:

```rust
fn infer_attribute_schema<'model>(
    sources: impl IntoIterator<Item=Option<&'model OwnedAttributes>>,
    origin: ColumnOrigin,
) -> Result<StructSchema<'model>>;

fn build_dynamic_schema<'model>(
    groups: impl IntoIterator<Item=(ColumnOrigin, StructSchema<'model>)>,
    reserved_names: &[&str],
) -> TableSchema<'model>;

fn resolve_dynamic_value<'row, 'model>(
    source: Option<&'model OwnedAttributes>,
    column: &'row ColumnSchema<'model>,
) -> Result<Value<'row, 'model>>;
```

That keeps format-agnostic behavior in one place:

- flatten maps with `__`
- infer nullable columns
- compact lists / JSON later in writers
- deterministic column names
- no writer-specific inference

## 2. Hierarchy representation

I would not model hierarchy as a separate relation for TSV. I would make hierarchy a reusable fixed-column projection that can be attached to row adapters.

For CityObjects:

```rust
pub struct HierarchyColumns<'model> {
    pub parents: IdList<'model>,
    pub children: IdList<'model>,
}

pub struct IdList<'model> {
    ids: Vec<&'model str>,
}
```

Resolution:

```rust
fn cityobject_id_list<'model>(
    model: &'model CityModel,
    handles: Option<&[CityObjectHandle]>,
) -> Result<IdList<'model>> {
    let ids = handles
        .unwrap_or_default()
        .iter()
        .map(|handle| {
            model
                .cityobjects()
                .get(*handle)
                .map(|object| object.id())
                .ok_or_else(|| anyhow!("dangling CityObject handle {handle:?}"))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(IdList { ids })
}
```

Then the CityObject row can expose hierarchy only when requested:

```rust
pub struct CityObjectRow<'table, 'model> {
    pub cityobject_id: &'model str,
    pub cityobject_ix: u64,
    pub cityobject_type: &'model CityObjectType<OwnedStringStorage>,
    pub hierarchy: Option<HierarchyColumns<'model>>,
    // dynamic values:
    attributes: Option<&'model OwnedAttributes>,
    extra: Option<&'model OwnedAttributes>,
    schema: &'table TableSchema<'model>,
}
```

For TSV, `parents` and `children` are encoded as compact JSON arrays:

```text
parents              children
["building-1"]       ["room-1","room-2"]
```

Important: this is still format-agnostic as a logical value. TSV decides the string encoding. A future GPKG writer could store the same list as JSON text or split it later if we decide that is better.

## 3. Metadata representation

Metadata should be its own logical table, not bolted onto CityObject rows. For `cjconvert`, it has one row. For Tyler, one row per tile. Same schema, different producer.

I would define:

```rust
pub struct MetadataTable<'model> {
    rows: Vec<MetadataRow<'model>>,
    schema: MetadataSchema,
}

pub struct MetadataRow<'model> {
    pub identifier: Option<String>,
    pub reference_date: Option<String>,
    pub reference_system: Option<String>,
    pub title: Option<String>,

    pub geographical_extent: Option<BBox>,

    pub point_of_contact: ContactFields<'model>,
    pub extra: Option<&'model OwnedAttributes>,
}
```

For `cjconvert`:

```rust
pub fn tabulate_model_metadata(model: &CityModel) -> Result<MetadataTable<'_>> {
    let row = MetadataRow::from_model_metadata(model.metadata());
    Ok(MetadataTable::one(row))
}
```

For Tyler later:

```rust
pub fn tabulate_tile_metadata<'model>(
    tile_metadata: impl IntoIterator<Item=TileMetadata<'model>>,
) -> Result<MetadataTable<'model>> {
    // one row per TSV tile
}
```

The WKT extent should be derived from `[minx, miny, minz, maxx, maxy, maxz]` as a 2D polygon:

```text
POLYGON((minx miny, maxx miny, maxx maxy, minx maxy, minx miny))
```

No metadata geometry goes into the main CityObject TSV.

For flattened metadata `extra`, I would reuse the same dynamic schema machinery, but with a metadata-specific origin:

```rust
pub enum ColumnOrigin {
    Attributes,
    Extra,
    MetadataExtra,
    SemanticAttributes,
}
```

or, if we want less churn, introduce a separate `DynamicSource` enum internal to tabular inference and keep public names stable.

## 4. Semantic representation

I would split semantics into two concepts.

First: semantic definitions, one row per semantic object in the model’s semantic pool.

```rust
pub struct SemanticTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

pub struct SemanticRow<'table, 'model> {
    pub semantic_id: u64,
    pub semantic_type: &'model SemanticType<OwnedStringStorage>,
    pub parent: Option<u64>,
    pub children: Vec<u64>,
    attributes: Option<&'model OwnedAttributes>,
    schema: &'table TableSchema<'model>,
}
```

This table is format-agnostic and directly maps:

- `semantic_id`
- `semantic_type`
- `parent`
- `children`
- flattened semantic attributes

Implementation sketch:

```rust
pub fn tabulate_semantics(model: &CityModel) -> Result<SemanticTable<'_>> {
    let mut attributes = StructSchema::default();

    for (row_ix, (_, semantic)) in model.iter_semantics().enumerate() {
        match semantic.attributes() {
            Some(values) => merge_attribute_map(&mut attributes, values, row_ix)?,
            None => mark_all_nullable(&mut attributes),
        }
    }

    Ok(SemanticTable {
        model,
        schema: build_semantic_schema(attributes),
    })
}
```

Second: semantic assignments, one row per geometry primitive assignment. I would not implement this until the `--tsv-split-semantics` milestone, because it needs geometry traversal choices.

Logical row shape:

```rust
pub struct SemanticAssignmentRow<'model> {
    pub cityobject_id: &'model str,
    pub cityobject_ix: u64,
    pub geometry_ix: u64,
    pub primitive_type: PrimitiveType,
    pub primitive_ix: u64,
    pub semantic_id: Option<u64>,
}
```

For current TSV needs, this can become `semantics.tsv` only behind an explicit flag. It should contain no geometry and no bbox.

## 5. Writer usage

The TSV writer should consume logical tables only:

```rust
pub fn write_cityobjects_tsv<W: Write>(
    table: &CityObjectTable<'_>,
    options: &TsvOptions,
    writer: W,
) -> Result<()>;

pub fn write_metadata_tsv<W: Write>(
    table: &MetadataTable<'_>,
    writer: W,
) -> Result<()>;

pub fn write_semantics_tsv<W: Write>(
    table: &SemanticTable<'_>,
    writer: W,
) -> Result<()>;
```

The writer owns only physical encoding:

- delimiter: tab
- null: empty field
- scalar formatting
- list/json: compact JSON
- hierarchy list: compact JSON array
- no geometry/bbox in main TSV

## Main design rule

The tabular module should answer: “what rows and logical values exist?”

The writer should answer: “how are those values serialized for TSV, CSV, GPKG, etc.?”

That keeps CityObjects, semantics, hierarchy, and metadata reusable across TSV now and GeoPackage later without reintroducing owned materialized rows or writer-specific inference.

## Implementation status

Implemented in `cityjson-convert`:

- Added explicit `CityObjectTable` / `CityObjectRow` names while keeping `Table` / `Row` as backwards-compatible aliases.
- Reused one dynamic schema/value path for CityObject `attributes`, CityObject `extra`, metadata `extra`, and semantic attributes via `infer_attribute_schema`, `build_dynamic_schema`, and `resolve_dynamic_value`.
- Added `ColumnOrigin::MetadataExtra` and `ColumnOrigin::SemanticAttributes`.
- Added hierarchy resolution on `CityObjectRow` through `parents()` and `children()`, returning `IdList` values that borrow resolved CityObject ids.
- Added `tabulate_model_metadata`, `MetadataTable`, `MetadataRow`, and `MetadataRowRef`, including fixed metadata fields, point-of-contact fields, flattened metadata `extra`, and 2D WKT extent projection.
- Added `tabulate_semantics`, `SemanticTable`, `SemanticRow`, and `SemanticRowRef`, including semantic type, parent, children, and flattened semantic attributes.
- Added focused tests for hierarchy, metadata, semantic definitions, and retained existing CityObject table tests.

Implementation notes:

- Semantic ids are derived from the public `SemanticHandle::raw_parts().0` slot so parent/child references can be represented without a separate remap layer.
- `MetadataTable` currently stores one row for `cjconvert`; tile metadata remains a future Tyler producer using the same logical shape.
- The module was kept in a single `tabular.rs` file for this pass to avoid unnecessary file churn. The logical split now exists in the public types and helpers, so physical file splitting can happen later if it becomes useful.

Not implemented in this pass:

- TSV writer functions for city objects, metadata, and semantics. No TSV writer exists in `cityjson-convert` yet; the current change exposes logical tables for future writers.
- Semantic assignment rows / `--tsv-split-semantics`, per the original plan's milestone note.
- GeoPackage-specific physical encoding.

