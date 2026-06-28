//! Shared borrowed tabular representation of CityJSON CityObjects.
//!
//! This module exposes a [`CityModel`] through a format-neutral schema and a lazy
//! sequence of CityObject rows for flat writers such as CSV, TSV, and
//! GeoPackage. It defines logical fields and values only; delimiters, SQLite
//! types, and text encoding belong to the writers.
//!
//! # API vocabulary
//!
//! The tabular API is organized into two groups:
//!
//! - Data access: [`Table`], [`Row`], [`Value`], [`ListValue`], and
//!   [`StructValue`].
//! - Schema: [`TableSchema`], [`ColumnSchema`], [`LogicalType`],
//!   [`StructSchema`], and [`StructFieldSchema`].
//!
//! The table types describe the flattened row layout. Schema types describe the
//! logical type accepted by a column or a nested container item. Values borrow
//! actual CityObject data. For example, [`LogicalType`] describes a value while
//! [`Value`] exposes one value from a row.
//! Nested schema and value types remain under this module; the crate root exports
//! only the primary table vocabulary.
//!
//! # Data flow
//!
//! [`tabulate_cityobjects`] scans the model once to infer the shared dynamic
//! schema. The returned table owns that schema and borrows the model.
//! Iterating rows walks the model directly in CityObject order. Rows and values
//! borrow source identifiers, strings, attribute names, JSON fallback values,
//! lists, and structs are borrowed rather than copied into a second table.
//!
//! Each row exposes fixed identity fields and one logical value for every
//! dynamic column. Values are resolved on demand. Lists and structs retained
//! inside lists remain lazy, allowing a writer to encode nested values without
//! first materializing owned child collections.
//!
//! # Schema inference
//!
//! CityObject `attributes` and custom `extra` members are inferred independently.
//! A field is nullable when it is explicitly null or absent from any CityObject.
//! Scalar types are retained; unsigned/signed integer combinations widen to
//! signed integers, and integer/float combinations widen to floats. Homogeneous
//! lists retain their item type and item nullability. Mixed shapes and
//! heterogeneous lists fall back to a JSON value.
//!
//! During inference, struct schemas form a tree used to discover
//! paths and merge field types across rows. The top-level trees are flattened
//! before the table schema is produced. [`LogicalType::Struct`] is retained
//! only for structs nested inside a container, primarily lists. A top-level
//! [`ColumnSchema`] is never emitted with a direct struct type.
//!
//! # Column flattening
//!
//! A dynamic column is derived from a CityObject's `attributes` or `extra` map
//! instead of a fixed field such as `cityobject_id`. Flattening means that maps
//! directly beneath those sources, including recursively nested maps, disappear
//! from the final table shape and their leaf paths become columns. Flattening
//! does not mean every value becomes scalar: lists remain single typed columns,
//! and structs inside those lists remain nested.
//!
//! For example, this input:
//!
//! ```json
//! {
//!   "attributes": {
//!     "metrics": {
//!       "height": 12,
//!       "slope": 0.25
//!     },
//!     "tags": ["public", "office"]
//!   }
//! }
//! ```
//!
//! produces these columns:
//!
//! ```text
//! attributes__metrics__height  Float64
//! attributes__metrics__slope   Float64
//! attributes__tags             List<Utf8>
//! ```
//!
//! The direct `metrics` map is an inference node only and does not become a
//! struct-valued column. The `tags` vector remains one list-valued column.
//!
//! Struct schemas and struct values are still needed when a struct occurs
//! inside a retained container. For example:
//!
//! ```json
//! {
//!   "attributes": {
//!     "items": [
//!       {"name": "door", "count": 2},
//!       {"name": "window", "count": 5}
//!     ]
//!   }
//! }
//! ```
//!
//! produces one column:
//!
//! ```text
//! attributes__items  List<Struct{name: Utf8, count: UInt64}>
//! ```
//!
//! Here the `items` list remains a single column, so its item structs require a
//! nested [`StructSchema`] and are read through a lazy [`StructValue`].
//!
//! Physical names start with the column origin and join path segments with `__`.
//! Within a path segment, `%` is escaped as `%25` and literal `__` as `%5F%5F`.
//! Conflicts are resolved deterministically with `__2`, `__3`, and later
//! suffixes. Null-only fields do not produce columns. Attribute columns precede
//! extra columns, and fields are ordered lexicographically at every map level.
//!
//! # Invariants
//!
//! A table has one row per CityObject in model order. Row ordinals are
//! zero-based, and every row uses the same ordered schema. Value index `n`
//! corresponds to dynamic-column index `n`. Missing nullable values become
//! null values. Non-null values either conform to the inferred logical type or
//! return a path-bearing error; they are never silently converted to an
//! unrelated type. The table and every value derived from it are bounded by
//! the lifetime of the source model.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;

use anyhow::{bail, Result};
use cityjson_lib::cityjson_types::resources::handles::{
    CityObjectHandle, GeometryHandle, SemanticHandle,
};
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::{
    CityObjectType, GeometryType, Metadata, OwnedAttributeValue, OwnedAttributes, Semantic,
    SemanticType,
};
use cityjson_lib::CityModel;

#[derive(Debug)]
pub struct CityObjectTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

impl<'model> CityObjectTable<'model> {
    /// Returns the shared flattened schema used by every row.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates over CityObjects in model order without allocating rows.
    pub fn rows(&self) -> impl Iterator<Item = CityObjectRow<'_, 'model>> {
        self.model
            .cityobjects()
            .iter()
            .enumerate()
            .map(|(cityobject_ix, (_, object))| CityObjectRow {
                model: self.model,
                cityobject_id: object.id(),
                cityobject_ix: cityobject_ix as u64,
                cityobject_type: object.type_cityobject(),
                bbox: object.geographical_extent().map(|bbox| (*bbox).into()),
                parents: object.parents(),
                children: object.children(),
                attributes: object.attributes(),
                extra: object.extra(),
                columns: &self.schema.columns,
            })
    }
}

/// Ordered dynamic-column schema shared by every row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableSchema<'model> {
    /// Columns in the same positional order returned by row value iterators.
    pub columns: Vec<ColumnSchema<'model>>,
}

/// Definition of one flattened dynamic table column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSchema<'model> {
    /// CityObject member from which the column originates.
    pub origin: ColumnOrigin,
    /// Unescaped nested path below the column origin.
    pub path: Vec<&'model str>,
    /// Escaped and unique physical column name.
    pub name: String,
    /// Logical type shared by every non-null value in the column.
    pub logical_type: LogicalType<'model>,
    /// Whether the column accepts missing or explicit-null values.
    pub nullable: bool,
}

/// CityObject member from which a dynamic column originates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnOrigin {
    /// Column inferred from the CityObject `attributes` member.
    Attributes,
    /// Column inferred from custom CityObject members.
    Extra,
    /// Column inferred from custom metadata members.
    MetadataExtra,
    /// Column inferred from semantic-object attributes.
    SemanticAttributes,
}

impl ColumnOrigin {
    /// Returns the physical source prefix used in flattened column names.
    fn physical_prefix(self) -> Option<&'static str> {
        match self {
            Self::Attributes => Some("attributes"),
            Self::Extra => None,
            Self::MetadataExtra => Some("metadata_extra"),
            Self::SemanticAttributes => Some("attributes"),
        }
    }
}

/// Logical type of a dynamic column or nested container item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicalType<'model> {
    /// Null-only value used for empty or all-null retained containers.
    Null,
    /// Boolean value.
    Boolean,
    /// Unsigned 64-bit integer.
    UInt64,
    /// Signed 64-bit integer.
    Int64,
    /// 64-bit floating-point value.
    Float64,
    /// UTF-8 string.
    Utf8,
    /// Original source value used when no stable logical type exists.
    Json,
    /// Reference to geometry owned by the source model.
    GeometryRef,
    /// Homogeneous ordered values.
    List {
        /// Whether an item in the list may be null.
        item_nullable: bool,
        /// Logical type shared by non-null list items.
        item: Box<LogicalType<'model>>,
    },
    /// Struct retained inside a container value.
    ///
    /// Direct map paths are flattened before columns are emitted, so this
    /// variant appears only recursively inside retained container types.
    Struct(StructSchema<'model>),
}

/// Lexicographically ordered schema for a struct retained inside a container.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructSchema<'model> {
    /// Fields keyed by their borrowed source names.
    pub fields: BTreeMap<&'model str, StructFieldSchema<'model>>,
}

/// One field in a retained nested struct schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructFieldSchema<'model> {
    /// Source field name borrowed from the model.
    pub name: &'model str,
    /// Logical type of the field.
    pub logical_type: LogicalType<'model>,
    /// Whether the field may be missing or explicitly null.
    pub nullable: bool,
}

/// One allocation-free row over a source CityObject.
#[derive(Clone, Debug)]
pub struct CityObjectRow<'table, 'model> {
    model: &'model CityModel,
    /// CityJSON object identifier borrowed from the model.
    pub cityobject_id: &'model str,
    /// Zero-based ordinal in model CityObject order.
    pub cityobject_ix: u64,
    /// Stored source `geographicalExtent`, when present.
    pub bbox: Option<[f64; 6]>,
    cityobject_type: &'model CityObjectType<OwnedStringStorage>,
    parents: Option<&'model [CityObjectHandle]>,
    children: Option<&'model [CityObjectHandle]>,
    attributes: Option<&'model OwnedAttributes>,
    extra: Option<&'model OwnedAttributes>,
    columns: &'table [ColumnSchema<'model>],
}

impl<'table, 'model> CityObjectRow<'table, 'model> {
    /// Returns the CityObject type using its CityJSON display spelling.
    #[must_use]
    pub fn cityobject_type_name(&self) -> impl Display + '_ {
        self.cityobject_type
    }

    /// Resolves the value at `index`.
    ///
    /// Returns `None` when `index` is outside the shared schema. Otherwise the
    /// result contains a borrowed value or a path-bearing conversion error.
    pub fn value(&self, index: usize) -> Option<Result<Value<'_, 'model>>> {
        self.columns
            .get(index)
            .map(|column| self.value_for_column(column))
    }

    /// Resolves values lazily in shared schema order.
    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.columns
            .iter()
            .map(|column| self.value_for_column(column))
    }

    /// Resolves parent CityObject handles into borrowed CityObject ids.
    pub fn parents(&self) -> Result<IdList<'model>> {
        cityobject_id_list(self.model, self.parents)
    }

    /// Resolves child CityObject handles into borrowed CityObject ids.
    pub fn children(&self) -> Result<IdList<'model>> {
        cityobject_id_list(self.model, self.children)
    }

    /// Resolves one dynamic column against this row's matching source map.
    fn value_for_column<'row>(
        &'row self,
        column: &'row ColumnSchema<'model>,
    ) -> Result<Value<'row, 'model>> {
        let attributes = match column.origin {
            ColumnOrigin::Attributes => self.attributes,
            ColumnOrigin::Extra => self.extra,
            ColumnOrigin::MetadataExtra | ColumnOrigin::SemanticAttributes => None,
        };
        let value = resolve_path(attributes, &column.path, &column.name)?;
        build_value(value, &column.logical_type, column.nullable, &column.name)
    }
}

/// Borrowed list of CityObject identifiers for hierarchy columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdList<'model> {
    ids: Vec<&'model str>,
}

impl<'model> IdList<'model> {
    /// Returns the resolved ids in source handle order.
    #[must_use]
    pub fn ids(&self) -> &[&'model str] {
        &self.ids
    }

    /// Iterates resolved ids in source handle order.
    pub fn iter(&self) -> impl Iterator<Item = &'model str> + '_ {
        self.ids.iter().copied()
    }
}

/// One-row logical metadata table for a CityJSON model.
#[derive(Debug)]
pub struct MetadataTable<'model> {
    rows: Vec<MetadataRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> MetadataTable<'model> {
    /// Returns the flattened schema for metadata `extra` fields.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates metadata rows.
    pub fn rows(&self) -> impl Iterator<Item = MetadataRowRef<'_, 'model>> {
        self.rows.iter().map(|row| MetadataRowRef {
            row,
            columns: &self.schema.columns,
        })
    }
}

/// Fixed metadata fields plus a borrowed dynamic `extra` source.
#[derive(Clone, Debug, Default)]
pub struct MetadataRow<'model> {
    pub identifier: Option<String>,
    pub reference_date: Option<String>,
    pub reference_system: Option<String>,
    pub title: Option<String>,
    pub geographical_extent: Option<[f64; 6]>,
    pub geographical_extent_wkt: Option<String>,
    pub contact_name: Option<String>,
    pub contact_email_address: Option<String>,
    pub contact_role: Option<String>,
    pub contact_website: Option<String>,
    pub contact_type: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_organization: Option<String>,
    extra: Option<&'model OwnedAttributes>,
}

/// Borrowed metadata row bound to shared dynamic columns.
#[derive(Clone, Copy, Debug)]
pub struct MetadataRowRef<'table, 'model> {
    row: &'table MetadataRow<'model>,
    columns: &'table [ColumnSchema<'model>],
}

impl<'table, 'model> MetadataRowRef<'table, 'model> {
    #[must_use]
    pub fn fixed(&self) -> &'table MetadataRow<'model> {
        self.row
    }

    pub fn value(&self, index: usize) -> Option<Result<Value<'_, 'model>>> {
        self.columns
            .get(index)
            .map(|column| resolve_dynamic_value(self.row.extra, column))
    }

    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.columns
            .iter()
            .map(|column| resolve_dynamic_value(self.row.extra, column))
    }
}

/// Borrowed semantic-definition table with one row per semantic object.
#[derive(Debug)]
pub struct SemanticTable<'model> {
    rows: Vec<SemanticRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> SemanticTable<'model> {
    /// Returns the flattened schema for semantic attributes.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates semantic definition rows.
    pub fn rows(&self) -> impl Iterator<Item = SemanticRowRef<'_, 'model>> {
        self.rows.iter().map(|row| SemanticRowRef {
            row,
            columns: &self.schema.columns,
        })
    }
}

/// Fixed semantic fields plus a borrowed dynamic attribute source.
#[derive(Clone, Debug)]
pub struct SemanticRow<'model> {
    pub semantic_id: u64,
    pub semantic_type: &'model SemanticType<OwnedStringStorage>,
    pub parent: Option<u64>,
    pub children: Vec<u64>,
    attributes: Option<&'model OwnedAttributes>,
}

impl<'model> SemanticRow<'model> {
    /// Returns the semantic type using its CityJSON display spelling.
    #[must_use]
    pub fn semantic_type_name(&self) -> impl Display + '_ {
        self.semantic_type
    }
}

/// Borrowed semantic row bound to shared dynamic columns.
#[derive(Clone, Copy, Debug)]
pub struct SemanticRowRef<'table, 'model> {
    row: &'table SemanticRow<'model>,
    columns: &'table [ColumnSchema<'model>],
}

impl<'table, 'model> SemanticRowRef<'table, 'model> {
    #[must_use]
    pub fn fixed(&self) -> &'table SemanticRow<'model> {
        self.row
    }

    pub fn value(&self, index: usize) -> Option<Result<Value<'_, 'model>>> {
        self.columns
            .get(index)
            .map(|column| resolve_dynamic_value(self.row.attributes, column))
    }

    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.columns
            .iter()
            .map(|column| resolve_dynamic_value(self.row.attributes, column))
    }
}

/// Borrowed semantic-assignment table with one row per mapped geometry primitive.
#[derive(Debug)]
pub struct SemanticAssignmentTable<'model> {
    rows: Vec<SemanticAssignmentRow<'model>>,
}

impl<'model> SemanticAssignmentTable<'model> {
    /// Iterates semantic assignment rows in CityObject and geometry order.
    pub fn rows(&self) -> impl Iterator<Item = &SemanticAssignmentRow<'model>> {
        self.rows.iter()
    }
}

/// Geometry primitive kind addressed by a semantic assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveType {
    Point,
    LineString,
    Surface,
}

impl Display for PrimitiveType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Point => f.write_str("point"),
            Self::LineString => f.write_str("linestring"),
            Self::Surface => f.write_str("surface"),
        }
    }
}

/// Fixed semantic assignment fields for one geometry primitive.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticAssignmentRow<'model> {
    pub cityobject_id: &'model str,
    pub cityobject_ix: u64,
    pub geometry_ix: u64,
    pub geometry_type: GeometryType,
    pub geometry_lod: Option<String>,
    pub geometry_is_instance: bool,
    pub primitive_type: PrimitiveType,
    pub primitive_ix: u64,
    pub point_ix: Option<u64>,
    pub linestring_ix: Option<u64>,
    pub solid_ix: Option<u64>,
    pub shell_ix: Option<u64>,
    pub surface_ix: Option<u64>,
    pub semantic_id: Option<u64>,
}

/// Borrowed logical value from one row and dynamic column.
#[derive(Debug)]
pub enum Value<'schema, 'model> {
    /// Missing or explicit-null value.
    Null,
    /// Boolean value copied from the source scalar.
    Boolean(bool),
    /// Unsigned integer copied from the source scalar.
    UInt64(u64),
    /// Signed integer copied or widened from the source scalar.
    Int64(i64),
    /// Floating-point value copied or widened from the source scalar.
    Float64(f64),
    /// UTF-8 string borrowed from the source model.
    Utf8(&'model str),
    /// Geometry handle copied from the source value.
    GeometryRef(GeometryHandle),
    /// List traversed lazily without materializing its items.
    List(ListValue<'schema, 'model>),
    /// Struct traversed lazily without materializing its fields.
    Struct(StructValue<'schema, 'model>),
    /// Original source value borrowed after heterogeneous fallback.
    Json(&'model OwnedAttributeValue),
}

/// Lazy borrowed list value.
#[derive(Debug)]
pub struct ListValue<'schema, 'model> {
    values: &'model [OwnedAttributeValue],
    item_type: &'schema LogicalType<'model>,
    item_nullable: bool,
    path: &'schema str,
}

impl<'schema, 'model> ListValue<'schema, 'model> {
    /// Returns the number of source list items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether the source list has no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Resolves list items lazily in source order.
    pub fn iter(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.values
            .iter()
            .map(|value| build_value(Some(value), self.item_type, self.item_nullable, self.path))
    }
}

/// Lazy borrowed struct retained inside a container value.
#[derive(Debug)]
pub struct StructValue<'schema, 'model> {
    values: &'model HashMap<String, OwnedAttributeValue>,
    schema: &'schema StructSchema<'model>,
    path: &'schema str,
}

impl<'schema, 'model> StructValue<'schema, 'model> {
    /// Resolves struct fields lazily in lexical schema order.
    ///
    /// Each item contains the borrowed field name and its value.
    pub fn fields(&self) -> impl Iterator<Item = Result<(&'model str, Value<'_, 'model>)>> {
        self.schema.fields.values().map(|field| {
            let value = build_value(
                self.values.get(field.name),
                &field.logical_type,
                field.nullable,
                self.path,
            )?;
            Ok((field.name, value))
        })
    }
}

/// Infers the shared schema and returns a borrowed CityObject table.
///
/// # Errors
///
/// Returns an error when an attribute value variant cannot be represented by the
/// table's logical type vocabulary.
pub fn tabulate_cityobjects(model: &CityModel) -> Result<CityObjectTable<'_>> {
    let attributes = infer_attribute_schema(
        model
            .cityobjects()
            .iter()
            .map(|(_, object)| object.attributes()),
    )?;
    let extra =
        infer_attribute_schema(model.cityobjects().iter().map(|(_, object)| object.extra()))?;
    Ok(CityObjectTable {
        model,
        schema: build_dynamic_schema(
            [
                (ColumnOrigin::Attributes, attributes),
                (ColumnOrigin::Extra, extra),
            ],
            &[
                "cityobject_id",
                "cityobject_ix",
                "cityobject_type",
                "bbox",
                "parents",
                "children",
            ],
        ),
    })
}

/// Infers the metadata table for a CityJSON model.
///
/// # Errors
///
/// Returns an error when metadata `extra` values cannot be represented by the
/// table's logical type vocabulary.
pub fn tabulate_model_metadata(model: &CityModel) -> Result<MetadataTable<'_>> {
    let row = model
        .metadata()
        .map(MetadataRow::from_model_metadata)
        .unwrap_or_default();
    let extra = infer_attribute_schema([row.extra])?;
    Ok(MetadataTable {
        rows: vec![row],
        schema: build_dynamic_schema(
            [(ColumnOrigin::MetadataExtra, extra)],
            &[
                "identifier",
                "reference_date",
                "reference_system",
                "title",
                "geographical_extent",
                "geographical_extent_wkt",
                "contact_name",
                "contact_email_address",
                "contact_role",
                "contact_website",
                "contact_type",
                "contact_phone",
                "contact_organization",
            ],
        ),
    })
}

/// Infers the semantic-definition table for a CityJSON model.
///
/// # Errors
///
/// Returns an error when semantic attributes cannot be represented by the
/// table's logical type vocabulary.
pub fn tabulate_semantics(model: &CityModel) -> Result<SemanticTable<'_>> {
    let attributes = infer_attribute_schema(
        model
            .iter_semantics()
            .map(|(_, semantic)| semantic.attributes()),
    )?;
    let rows = model
        .iter_semantics()
        .map(|(handle, semantic)| SemanticRow::from_semantic(handle, semantic))
        .collect();
    Ok(SemanticTable {
        rows,
        schema: build_dynamic_schema(
            [(ColumnOrigin::SemanticAttributes, attributes)],
            &["semantic_id", "semantic_type", "parent", "children"],
        ),
    })
}

/// Tabulates geometry primitive to semantic definition assignments.
///
/// # Errors
///
/// Returns an error when geometry handles cannot be resolved, boundary nesting
/// is invalid for the effective geometry type, or semantic assignment counts do
/// not match primitive counts.
pub fn tabulate_semantic_assignments(model: &CityModel) -> Result<SemanticAssignmentTable<'_>> {
    let mut rows = Vec::new();
    for (cityobject_ix, (_, object)) in model.cityobjects().iter().enumerate() {
        let Some(geometry_handles) = object.geometry() else {
            continue;
        };
        for (geometry_ix, geometry_handle) in geometry_handles.iter().copied().enumerate() {
            let source_geometry = model
                .get_geometry(geometry_handle)
                .ok_or_else(|| anyhow::anyhow!("dangling geometry handle {geometry_handle:?}"))?;
            let geometry = model.resolve_geometry(geometry_handle)?;
            let Some(semantics) = geometry.semantics() else {
                continue;
            };
            let Some(boundary) = geometry.boundaries() else {
                bail!(
                    "semantic assignments on geometry {geometry_ix} of CityObject {} have no boundaries",
                    object.id()
                );
            };

            let context = SemanticAssignmentContext {
                cityobject_id: object.id(),
                cityobject_ix: cityobject_ix as u64,
                geometry_ix: geometry_ix as u64,
                geometry_type: *geometry.type_geometry(),
                geometry_lod: geometry.lod().map(ToString::to_string),
                geometry_is_instance: source_geometry.instance().is_some(),
            };

            match geometry.type_geometry() {
                GeometryType::MultiPoint => {
                    let points = boundary.to_nested_multi_point()?;
                    push_point_assignments(&mut rows, &context, semantics.points(), points.len())?;
                }
                GeometryType::MultiLineString => {
                    let linestrings = boundary.to_nested_multi_linestring()?;
                    push_linestring_assignments(
                        &mut rows,
                        &context,
                        semantics.linestrings(),
                        linestrings.len(),
                    )?;
                }
                GeometryType::MultiSurface | GeometryType::CompositeSurface => {
                    let surfaces = boundary.to_nested_multi_or_composite_surface()?;
                    push_surface_assignments(
                        &mut rows,
                        &context,
                        semantics.surfaces(),
                        surface_paths_for_multi_surface(surfaces.len()),
                    )?;
                }
                GeometryType::Solid => {
                    let shells = boundary.to_nested_solid()?;
                    push_surface_assignments(
                        &mut rows,
                        &context,
                        semantics.surfaces(),
                        surface_paths_for_solid(&shells),
                    )?;
                }
                GeometryType::MultiSolid | GeometryType::CompositeSolid => {
                    let solids = boundary.to_nested_multi_or_composite_solid()?;
                    push_surface_assignments(
                        &mut rows,
                        &context,
                        semantics.surfaces(),
                        surface_paths_for_multi_solid(&solids),
                    )?;
                }
                GeometryType::GeometryInstance => {
                    bail!(
                        "GeometryInstance for CityObject {} was not resolved to an effective geometry",
                        object.id()
                    );
                }
                geometry_type => {
                    bail!("unsupported geometry type {geometry_type} in semantic assignments");
                }
            }
        }
    }
    Ok(SemanticAssignmentTable { rows })
}

impl<'model> MetadataRow<'model> {
    fn from_model_metadata(metadata: &'model Metadata<OwnedStringStorage>) -> Self {
        let contact = metadata.point_of_contact();
        Self {
            identifier: metadata.identifier().map(ToString::to_string),
            reference_date: metadata.reference_date().map(ToString::to_string),
            reference_system: metadata.reference_system().map(ToString::to_string),
            title: metadata.title().map(ToString::to_string),
            geographical_extent: metadata.geographical_extent().map(|bbox| (*bbox).into()),
            geographical_extent_wkt: metadata.geographical_extent().map(bbox_wkt_2d),
            contact_name: contact.map(|contact| contact.contact_name().to_string()),
            contact_email_address: contact.map(|contact| contact.email_address().to_string()),
            contact_role: contact.and_then(|contact| contact.role().map(|role| role.to_string())),
            contact_website: contact
                .and_then(|contact| contact.website().as_ref().map(ToString::to_string)),
            contact_type: contact
                .and_then(|contact| contact.contact_type().map(|kind| kind.to_string())),
            contact_phone: contact
                .and_then(|contact| contact.phone().as_ref().map(ToString::to_string)),
            contact_organization: contact
                .and_then(|contact| contact.organization().as_ref().map(ToString::to_string)),
            extra: metadata.extra(),
        }
    }
}

impl<'model> SemanticRow<'model> {
    fn from_semantic(
        handle: SemanticHandle,
        semantic: &'model Semantic<OwnedStringStorage>,
    ) -> Self {
        Self {
            semantic_id: semantic_handle_id(handle),
            semantic_type: semantic.type_semantic(),
            parent: semantic.parent().map(semantic_handle_id),
            children: semantic
                .children()
                .unwrap_or_default()
                .iter()
                .copied()
                .map(semantic_handle_id)
                .collect(),
            attributes: semantic.attributes(),
        }
    }
}

fn infer_attribute_schema<'model>(
    sources: impl IntoIterator<Item = Option<&'model OwnedAttributes>>,
) -> Result<StructSchema<'model>> {
    let mut schema = StructSchema::default();
    for (row_ix, source) in sources.into_iter().enumerate() {
        match source {
            Some(values) => merge_attribute_map(&mut schema, values, row_ix)?,
            None => mark_all_nullable(&mut schema),
        }
    }
    Ok(schema)
}

fn resolve_dynamic_value<'row, 'model>(
    source: Option<&'model OwnedAttributes>,
    column: &'row ColumnSchema<'model>,
) -> Result<Value<'row, 'model>> {
    let value = resolve_path(source, &column.path, &column.name)?;
    build_value(value, &column.logical_type, column.nullable, &column.name)
}

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
                .ok_or_else(|| anyhow::anyhow!("dangling CityObject handle {handle:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(IdList { ids })
}

fn semantic_handle_id(handle: SemanticHandle) -> u64 {
    u64::from(handle.raw_parts().0)
}

#[derive(Clone, Debug)]
struct SemanticAssignmentContext<'model> {
    cityobject_id: &'model str,
    cityobject_ix: u64,
    geometry_ix: u64,
    geometry_type: GeometryType,
    geometry_lod: Option<String>,
    geometry_is_instance: bool,
}

#[derive(Clone, Copy, Debug)]
struct SurfacePath {
    solid_ix: Option<u64>,
    shell_ix: Option<u64>,
    surface_ix: u64,
}

fn push_point_assignments<'model>(
    rows: &mut Vec<SemanticAssignmentRow<'model>>,
    context: &SemanticAssignmentContext<'model>,
    assignments: cityjson_lib::cityjson_types::v2_0::geometry::HandleOptionSlice<
        '_,
        SemanticHandle,
    >,
    point_count: usize,
) -> Result<()> {
    validate_assignment_count(
        context,
        PrimitiveType::Point,
        assignments.len(),
        point_count,
    )?;
    for point_ix in 0..point_count {
        rows.push(SemanticAssignmentRow {
            cityobject_id: context.cityobject_id,
            cityobject_ix: context.cityobject_ix,
            geometry_ix: context.geometry_ix,
            geometry_type: context.geometry_type,
            geometry_lod: context.geometry_lod.clone(),
            geometry_is_instance: context.geometry_is_instance,
            primitive_type: PrimitiveType::Point,
            primitive_ix: point_ix as u64,
            point_ix: Some(point_ix as u64),
            linestring_ix: None,
            solid_ix: None,
            shell_ix: None,
            surface_ix: None,
            semantic_id: assignments[point_ix].map(semantic_handle_id),
        });
    }
    Ok(())
}

fn push_linestring_assignments<'model>(
    rows: &mut Vec<SemanticAssignmentRow<'model>>,
    context: &SemanticAssignmentContext<'model>,
    assignments: cityjson_lib::cityjson_types::v2_0::geometry::HandleOptionSlice<
        '_,
        SemanticHandle,
    >,
    linestring_count: usize,
) -> Result<()> {
    validate_assignment_count(
        context,
        PrimitiveType::LineString,
        assignments.len(),
        linestring_count,
    )?;
    for linestring_ix in 0..linestring_count {
        rows.push(SemanticAssignmentRow {
            cityobject_id: context.cityobject_id,
            cityobject_ix: context.cityobject_ix,
            geometry_ix: context.geometry_ix,
            geometry_type: context.geometry_type,
            geometry_lod: context.geometry_lod.clone(),
            geometry_is_instance: context.geometry_is_instance,
            primitive_type: PrimitiveType::LineString,
            primitive_ix: linestring_ix as u64,
            point_ix: None,
            linestring_ix: Some(linestring_ix as u64),
            solid_ix: None,
            shell_ix: None,
            surface_ix: None,
            semantic_id: assignments[linestring_ix].map(semantic_handle_id),
        });
    }
    Ok(())
}

fn push_surface_assignments<'model>(
    rows: &mut Vec<SemanticAssignmentRow<'model>>,
    context: &SemanticAssignmentContext<'model>,
    assignments: cityjson_lib::cityjson_types::v2_0::geometry::HandleOptionSlice<
        '_,
        SemanticHandle,
    >,
    paths: Vec<SurfacePath>,
) -> Result<()> {
    validate_assignment_count(
        context,
        PrimitiveType::Surface,
        assignments.len(),
        paths.len(),
    )?;
    for (primitive_ix, path) in paths.into_iter().enumerate() {
        rows.push(SemanticAssignmentRow {
            cityobject_id: context.cityobject_id,
            cityobject_ix: context.cityobject_ix,
            geometry_ix: context.geometry_ix,
            geometry_type: context.geometry_type,
            geometry_lod: context.geometry_lod.clone(),
            geometry_is_instance: context.geometry_is_instance,
            primitive_type: PrimitiveType::Surface,
            primitive_ix: primitive_ix as u64,
            point_ix: None,
            linestring_ix: None,
            solid_ix: path.solid_ix,
            shell_ix: path.shell_ix,
            surface_ix: Some(path.surface_ix),
            semantic_id: assignments[primitive_ix].map(semantic_handle_id),
        });
    }
    Ok(())
}

fn validate_assignment_count(
    context: &SemanticAssignmentContext<'_>,
    primitive_type: PrimitiveType,
    assignment_count: usize,
    primitive_count: usize,
) -> Result<()> {
    if assignment_count != primitive_count {
        bail!(
            "semantic {} assignment count {} does not match primitive count {} on geometry {} of CityObject {}",
            primitive_type,
            assignment_count,
            primitive_count,
            context.geometry_ix,
            context.cityobject_id
        );
    }
    Ok(())
}

fn surface_paths_for_multi_surface(surface_count: usize) -> Vec<SurfacePath> {
    (0..surface_count)
        .map(|surface_ix| SurfacePath {
            solid_ix: None,
            shell_ix: None,
            surface_ix: surface_ix as u64,
        })
        .collect()
}

fn surface_paths_for_solid<Surface>(shells: &[Vec<Surface>]) -> Vec<SurfacePath> {
    let mut paths = Vec::new();
    for (shell_ix, surfaces) in shells.iter().enumerate() {
        for surface_ix in 0..surfaces.len() {
            paths.push(SurfacePath {
                solid_ix: None,
                shell_ix: Some(shell_ix as u64),
                surface_ix: surface_ix as u64,
            });
        }
    }
    paths
}

fn surface_paths_for_multi_solid<Surface>(solids: &[Vec<Vec<Surface>>]) -> Vec<SurfacePath> {
    let mut paths = Vec::new();
    for (solid_ix, shells) in solids.iter().enumerate() {
        for (shell_ix, surfaces) in shells.iter().enumerate() {
            for surface_ix in 0..surfaces.len() {
                paths.push(SurfacePath {
                    solid_ix: Some(solid_ix as u64),
                    shell_ix: Some(shell_ix as u64),
                    surface_ix: surface_ix as u64,
                });
            }
        }
    }
    paths
}

fn bbox_wkt_2d(bbox: &cityjson_lib::cityjson_types::v2_0::BBox) -> String {
    format!(
        "POLYGON(({} {}, {} {}, {} {}, {} {}, {} {}))",
        bbox.min_x(),
        bbox.min_y(),
        bbox.max_x(),
        bbox.min_y(),
        bbox.max_x(),
        bbox.max_y(),
        bbox.min_x(),
        bbox.max_y(),
        bbox.min_x(),
        bbox.min_y()
    )
}

/// Merges one CityObject attribute map into an ordered inferred tree.
fn merge_attribute_map<'model>(
    schema: &mut StructSchema<'model>,
    attributes: &'model OwnedAttributes,
    seen_rows: usize,
) -> Result<()> {
    for field in schema.fields.values_mut() {
        if !attributes.contains_key(field.name) {
            field.nullable = true;
        }
    }
    for (name, value) in attributes.iter() {
        merge_field(schema, name, value, seen_rows)?;
    }
    Ok(())
}

/// Merges one nested map into an ordered inferred tree.
fn merge_attribute_values<'model>(
    schema: &mut StructSchema<'model>,
    values: &'model HashMap<String, OwnedAttributeValue>,
    seen_rows: usize,
) -> Result<()> {
    for field in schema.fields.values_mut() {
        if !values.contains_key(field.name) {
            field.nullable = true;
        }
    }
    for (name, value) in values {
        merge_field(schema, name, value, seen_rows)?;
    }
    Ok(())
}

/// Adds a field or merges its value into an existing inferred field.
fn merge_field<'model>(
    schema: &mut StructSchema<'model>,
    name: &'model str,
    value: &'model OwnedAttributeValue,
    seen_rows: usize,
) -> Result<()> {
    if let Some(field) = schema.fields.get_mut(name) {
        if matches!(value, OwnedAttributeValue::Null) {
            field.nullable = true;
        } else {
            merge_logical_type_with_value(&mut field.logical_type, value)?;
        }
    } else {
        schema.fields.insert(
            name,
            StructFieldSchema {
                name,
                logical_type: infer_type(value)?,
                nullable: seen_rows > 0 || matches!(value, OwnedAttributeValue::Null),
            },
        );
    }
    Ok(())
}

/// Marks every currently known field nullable for a missing source map.
fn mark_all_nullable(schema: &mut StructSchema<'_>) {
    for field in schema.fields.values_mut() {
        field.nullable = true;
    }
}

/// Infers the logical type of one borrowed source value recursively.
fn infer_type<'model>(value: &'model OwnedAttributeValue) -> Result<LogicalType<'model>> {
    Ok(match value {
        OwnedAttributeValue::Null => LogicalType::Null,
        OwnedAttributeValue::Bool(_) => LogicalType::Boolean,
        OwnedAttributeValue::Unsigned(_) => LogicalType::UInt64,
        OwnedAttributeValue::Integer(_) => LogicalType::Int64,
        OwnedAttributeValue::Float(_) => LogicalType::Float64,
        OwnedAttributeValue::String(_) => LogicalType::Utf8,
        OwnedAttributeValue::Geometry(_) => LogicalType::GeometryRef,
        OwnedAttributeValue::Vec(values) => infer_list_type(values)?,
        OwnedAttributeValue::Map(values) => {
            let mut schema = StructSchema::default();
            merge_attribute_values(&mut schema, values, 0)?;
            LogicalType::Struct(schema)
        }
        unsupported => bail!("unsupported attribute value variant {unsupported}"),
    })
}

/// Infers a homogeneous list item type or falls back to JSON.
fn infer_list_type<'model>(values: &'model [OwnedAttributeValue]) -> Result<LogicalType<'model>> {
    let mut item_nullable = false;
    let mut item_type = LogicalType::Null;
    let mut saw_item = false;
    for value in values {
        if matches!(value, OwnedAttributeValue::Null) {
            item_nullable = true;
        } else if saw_item {
            merge_logical_type_with_value(&mut item_type, value)?;
        } else {
            item_type = infer_type(value)?;
            saw_item = true;
        }
    }
    if matches!(item_type, LogicalType::Json) {
        Ok(LogicalType::Json)
    } else {
        Ok(LogicalType::List {
            item_nullable,
            item: Box::new(item_type),
        })
    }
}

/// Merges a source value into an existing inferred type using widening rules.
fn merge_logical_type_with_value<'model>(
    current: &mut LogicalType<'model>,
    value: &'model OwnedAttributeValue,
) -> Result<()> {
    if matches!(value, OwnedAttributeValue::Null) {
        return Ok(());
    }
    match (&mut *current, value) {
        (LogicalType::Null, _) => *current = infer_type(value)?,
        (LogicalType::Boolean, OwnedAttributeValue::Bool(_))
        | (LogicalType::UInt64, OwnedAttributeValue::Unsigned(_))
        | (LogicalType::Int64, OwnedAttributeValue::Integer(_))
        | (LogicalType::Float64, OwnedAttributeValue::Float(_))
        | (LogicalType::Utf8, OwnedAttributeValue::String(_))
        | (LogicalType::GeometryRef, OwnedAttributeValue::Geometry(_))
        | (LogicalType::Json, _) => {}
        (LogicalType::UInt64, OwnedAttributeValue::Integer(_))
        | (LogicalType::Int64, OwnedAttributeValue::Unsigned(_)) => {
            *current = LogicalType::Int64;
        }
        (LogicalType::UInt64 | LogicalType::Int64, OwnedAttributeValue::Float(_)) => {
            *current = LogicalType::Float64;
        }
        (
            LogicalType::Float64,
            OwnedAttributeValue::Unsigned(_) | OwnedAttributeValue::Integer(_),
        ) => {}
        (
            LogicalType::List {
                item_nullable,
                item,
            },
            OwnedAttributeValue::Vec(values),
        ) => {
            for value in values {
                if matches!(value, OwnedAttributeValue::Null) {
                    *item_nullable = true;
                } else {
                    merge_logical_type_with_value(item, value)?;
                    if matches!(item.as_ref(), LogicalType::Json) {
                        *current = LogicalType::Json;
                        break;
                    }
                }
            }
        }
        (LogicalType::Struct(schema), OwnedAttributeValue::Map(values)) => {
            merge_attribute_values(schema, values, 1)?;
        }
        _ => *current = LogicalType::Json,
    }
    Ok(())
}

/// Flattens dynamic source trees into one shared table schema.
fn build_dynamic_schema<'model>(
    groups: impl IntoIterator<Item = (ColumnOrigin, StructSchema<'model>)>,
    reserved_names: &[&str],
) -> TableSchema<'model> {
    let mut columns = Vec::new();
    let mut path = Vec::new();
    let mut name_buffer = String::new();
    for (origin, schema) in groups {
        flatten_struct_schema(
            origin,
            schema,
            &mut path,
            false,
            &mut name_buffer,
            &mut columns,
            reserved_names,
        );
    }
    TableSchema { columns }
}

/// Recursively emits non-struct leaves as dynamic columns.
fn flatten_struct_schema<'model>(
    origin: ColumnOrigin,
    schema: StructSchema<'model>,
    path: &mut Vec<&'model str>,
    inherited_nullable: bool,
    name_buffer: &mut String,
    columns: &mut Vec<ColumnSchema<'model>>,
    reserved_names: &[&str],
) {
    for field in schema.fields.into_values() {
        path.push(field.name);
        let nullable = inherited_nullable || field.nullable;
        match field.logical_type {
            LogicalType::Null => {}
            LogicalType::Struct(schema) => {
                flatten_struct_schema(
                    origin,
                    schema,
                    path,
                    nullable,
                    name_buffer,
                    columns,
                    reserved_names,
                );
            }
            logical_type => {
                build_column_name(name_buffer, origin, path);
                debug_assert!(!matches!(logical_type, LogicalType::Struct(_)));
                columns.push(ColumnSchema {
                    origin,
                    path: path.clone(),
                    name: unique_column_name(name_buffer, columns, reserved_names),
                    logical_type,
                    nullable,
                });
            }
        }
        path.pop();
    }
}

/// Builds an escaped physical name in a reusable string buffer.
fn build_column_name(buffer: &mut String, origin: ColumnOrigin, path: &[&str]) {
    buffer.clear();
    if let Some(prefix) = origin.physical_prefix() {
        buffer.push_str(prefix);
    }
    for segment in path {
        if !buffer.is_empty() {
            buffer.push_str("__");
        }
        push_escaped_path_segment(buffer, segment);
    }
}

/// Appends one path segment using the tabular escaping rules.
fn push_escaped_path_segment(output: &mut String, segment: &str) {
    let mut remainder = segment;
    while let Some(index) = remainder.find(['%', '_']) {
        output.push_str(&remainder[..index]);
        if remainder[index..].starts_with('%') {
            output.push_str("%25");
            remainder = &remainder[index + 1..];
        } else if remainder[index..].starts_with("__") {
            output.push_str("%5F%5F");
            remainder = &remainder[index + 2..];
        } else {
            output.push('_');
            remainder = &remainder[index + 1..];
        }
    }
    output.push_str(remainder);
}

/// Returns the first column name not used by fixed or dynamic columns.
fn unique_column_name(base: &str, columns: &[ColumnSchema<'_>], reserved_names: &[&str]) -> String {
    if !column_name_exists(base, columns, reserved_names) {
        return base.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}__{suffix}");
        if !column_name_exists(&candidate, columns, reserved_names) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Reports whether a physical name is reserved or already emitted.
fn column_name_exists(name: &str, columns: &[ColumnSchema<'_>], reserved_names: &[&str]) -> bool {
    reserved_names.contains(&name) || columns.iter().any(|column| column.name == name)
}

/// Follows a flattened column path through one optional source map.
fn resolve_path<'model>(
    attributes: Option<&'model OwnedAttributes>,
    path: &[&str],
    column_name: &str,
) -> Result<Option<&'model OwnedAttributeValue>> {
    let Some((first, remaining)) = path.split_first() else {
        bail!("{column_name}: empty source path");
    };
    let Some(mut value) = attributes.and_then(|attributes| attributes.get(first)) else {
        return Ok(None);
    };
    for segment in remaining {
        match value {
            OwnedAttributeValue::Null => return Ok(None),
            OwnedAttributeValue::Map(values) => {
                let Some(next) = values.get(*segment) else {
                    return Ok(None);
                };
                value = next;
            }
            other => {
                bail!("{column_name}: expected map while resolving source path, found {other}");
            }
        }
    }
    Ok(Some(value))
}

/// Validates and exposes one source value as a borrowed table value.
fn build_value<'schema, 'model>(
    value: Option<&'model OwnedAttributeValue>,
    logical_type: &'schema LogicalType<'model>,
    nullable: bool,
    path: &'schema str,
) -> Result<Value<'schema, 'model>> {
    let Some(value) = value else {
        if nullable {
            return Ok(Value::Null);
        }
        bail!("{path}: missing non-nullable value");
    };
    if matches!(value, OwnedAttributeValue::Null) {
        if nullable {
            return Ok(Value::Null);
        }
        bail!("{path}: null in non-nullable value");
    }
    match (logical_type, value) {
        (LogicalType::Boolean, OwnedAttributeValue::Bool(value)) => Ok(Value::Boolean(*value)),
        (LogicalType::UInt64, OwnedAttributeValue::Unsigned(value)) => Ok(Value::UInt64(*value)),
        (LogicalType::Int64, OwnedAttributeValue::Integer(value)) => Ok(Value::Int64(*value)),
        (LogicalType::Int64, OwnedAttributeValue::Unsigned(value)) => {
            Ok(Value::Int64(i64::try_from(*value).map_err(|_| {
                anyhow::anyhow!("{path}: unsigned integer {value} does not fit in Int64")
            })?))
        }
        (LogicalType::Float64, OwnedAttributeValue::Float(value)) => Ok(Value::Float64(*value)),
        (LogicalType::Float64, OwnedAttributeValue::Unsigned(value)) => {
            Ok(Value::Float64(*value as f64))
        }
        (LogicalType::Float64, OwnedAttributeValue::Integer(value)) => {
            Ok(Value::Float64(*value as f64))
        }
        (LogicalType::Utf8, OwnedAttributeValue::String(value)) => Ok(Value::Utf8(value)),
        (LogicalType::GeometryRef, OwnedAttributeValue::Geometry(value)) => {
            Ok(Value::GeometryRef(*value))
        }
        (LogicalType::Json, value) => Ok(Value::Json(value)),
        (
            LogicalType::List {
                item_nullable,
                item,
            },
            OwnedAttributeValue::Vec(values),
        ) => Ok(Value::List(ListValue {
            values,
            item_type: item,
            item_nullable: *item_nullable,
            path,
        })),
        (LogicalType::Struct(schema), OwnedAttributeValue::Map(values)) => {
            Ok(Value::Struct(StructValue {
                values,
                schema,
                path,
            }))
        }
        (expected, actual) => bail!("{path}: expected {expected:?}, found {actual}"),
    }
}
