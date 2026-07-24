//! Shared borrowed tabular representation of `CityJSON` `CityObjects`.
//!
//! This module exposes a [`CityModel`] through a format-neutral schema and a lazy
//! sequence of `CityObject` rows for flat writers such as CSV, TSV, and
//! `GeoPackage`. It defines logical fields and values only; delimiters, `SQLite`
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
//! actual `CityObject` data. For example, [`LogicalType`] describes a value while
//! [`Value`] exposes one value from a row.
//! Nested schema and value types remain under this module; the crate root exports
//! only the primary table vocabulary.
//!
//! # Data flow
//!
//! [`tabulate_cityobjects`] scans the model once to infer the shared dynamic
//! schema. The returned table owns that schema and borrows the model.
//! Iterating rows walks the model directly in `CityObject` order. Rows and values
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
//! `CityObject` `attributes` and custom `extra` members are inferred independently.
//! A field is nullable when it is explicitly null or absent from any `CityObject`.
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
//! A dynamic column is derived from a `CityObject`'s `attributes` or `extra` map
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
//! Conflicts are resolved case-insensitively and deterministically with `__2`,
//! `__3`, and later suffixes. Null-only fields do not produce columns.
//! Attribute columns precede extra columns, and fields are ordered
//! lexicographically at every map level.
//!
//! # Invariants
//!
//! A table has one row per `CityObject` in model order. Row ordinals are
//! zero-based, and every row uses the same ordered schema. Value index `n`
//! corresponds to dynamic-column index `n`. Missing nullable values become
//! null values. Non-null values either conform to the inferred logical type or
//! return a path-bearing error; they are never silently converted to an
//! unrelated type. The table and every value derived from it are bounded by
//! the lifetime of the source model.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Display;

use anyhow::{bail, Context, Result};
use cityjson_lib::cityjson_types::resources::handles::{
    CityObjectHandle, GeometryHandle, SemanticHandle,
};
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::boundary::Boundary;
use cityjson_lib::cityjson_types::v2_0::{
    CityObjectType, GeometryType, Metadata, OwnedAttributeValue, OwnedAttributes, Semantic,
    SemanticType, VertexIndex,
};
use cityjson_lib::CityModel;
use serde_json::{Map, Value as JsonValue};

#[derive(Debug)]
pub struct CityObjectTable<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

impl<'model> CityObjectTable<'model> {
    /// Returns the source model backing this borrowed table.
    #[must_use]
    pub fn model(&self) -> &'model CityModel {
        self.model
    }

    /// Returns the shared flattened schema used by every row.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates over `CityObjects` in model order without allocating rows.
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

/// Borrowed `CityObject` hierarchy table with one parent-to-child edge per row.
///
/// The table is empty when no `CityObject` declares parent or child handles.
/// Edges are de-duplicated because `CityJSON` can expose the same relation from
/// both sides through a parent's `children` and a child's `parents` member.
#[derive(Debug)]
pub struct CityObjectHierarchyTable<'model> {
    rows: Vec<HierarchyRow<'model>>,
}

impl<'model> CityObjectHierarchyTable<'model> {
    /// Iterates hierarchy edges ordered by parent id and then child id.
    pub fn rows(&self) -> impl Iterator<Item = &HierarchyRow<'model>> {
        self.rows.iter()
    }
}

/// One parent-to-child hierarchy edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyRow<'model> {
    pub parent_id: &'model str,
    pub child_id: &'model str,
}

/// Borrowed address table with one row per `CityObject.extra.address` object.
#[derive(Debug)]
pub struct AddressTable<'model> {
    model: &'model CityModel,
    rows: Vec<AddressRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> AddressTable<'model> {
    /// Returns the source model backing this borrowed table.
    #[must_use]
    pub fn model(&self) -> &'model CityModel {
        self.model
    }

    /// Returns the flattened schema for address fields except `location`.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates address rows in source `CityObject` order.
    pub fn rows(&self) -> impl Iterator<Item = AddressRowRef<'_, 'model>> {
        self.rows.iter().map(|row| AddressRowRef {
            row,
            columns: &self.schema.columns,
        })
    }
}

/// Fixed address row fields plus the borrowed nested address object.
#[derive(Clone, Debug)]
pub struct AddressRow<'model> {
    pub cityobject_id: &'model str,
    pub cityobject_ix: u64,
    cityobject_type: &'model CityObjectType<OwnedStringStorage>,
    address: &'model HashMap<String, OwnedAttributeValue>,
}

impl AddressRow<'_> {
    /// Returns the owning `CityObject` type using its `CityJSON` display spelling.
    #[must_use]
    pub fn cityobject_type_name(&self) -> impl Display + '_ {
        self.cityobject_type
    }

    /// Returns the optional `address.location` geometry value.
    ///
    /// # Errors
    ///
    /// Returns an error when the deserialized value cannot be represented as a
    /// geometry reference.
    pub fn location(&self) -> Result<Value<'_, '_>> {
        build_value(
            self.address.get("location"),
            &LogicalType::GeometryRef,
            true,
            "address.location",
        )
    }
}

/// Borrowed address row bound to shared dynamic address columns.
#[derive(Clone, Copy, Debug)]
pub struct AddressRowRef<'table, 'model> {
    row: &'table AddressRow<'model>,
    columns: &'table [ColumnSchema<'model>],
}

impl<'table, 'model> AddressRowRef<'table, 'model> {
    #[must_use]
    pub fn fixed(&self) -> &'table AddressRow<'model> {
        self.row
    }

    #[must_use]
    pub fn value(&self, index: usize) -> Option<Result<Value<'_, 'model>>> {
        self.columns
            .get(index)
            .map(|column| self.value_for_column(column))
    }

    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.columns
            .iter()
            .map(|column| self.value_for_column(column))
    }

    fn value_for_column<'row>(
        &'row self,
        column: &'row ColumnSchema<'model>,
    ) -> Result<Value<'row, 'model>> {
        let value = resolve_path_in_map(Some(self.row.address), &column.path, &column.name)?;
        build_value(value, &column.logical_type, column.nullable, &column.name)
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
    /// `CityObject` member from which the column originates.
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

/// `CityObject` member from which a dynamic column originates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnOrigin {
    /// Column inferred from the `CityObject` `attributes` member.
    Attributes,
    /// Column inferred from custom `CityObject` members.
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
            Self::SemanticAttributes => Some("attribute"),
            Self::Extra | Self::MetadataExtra => None,
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

/// One allocation-free row over a source `CityObject`.
#[derive(Clone, Debug)]
pub struct CityObjectRow<'table, 'model> {
    model: &'model CityModel,
    /// `CityJSON` object identifier borrowed from the model.
    pub cityobject_id: &'model str,
    /// Zero-based ordinal in model `CityObject` order.
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

impl<'model> CityObjectRow<'_, 'model> {
    /// Returns the `CityObject` type using its `CityJSON` display spelling.
    #[must_use]
    pub fn cityobject_type_name(&self) -> impl Display + '_ {
        self.cityobject_type
    }

    /// Resolves the value at `index`.
    ///
    /// Returns `None` when `index` is outside the shared schema. Otherwise the
    /// result contains a borrowed value or a path-bearing conversion error.
    #[must_use]
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

    /// Resolves one dynamic column from a separately inferred compatible schema.
    ///
    /// `GeoPackage` feature layers can infer dynamic schemas per `CityObject` type,
    /// while rows still borrow their fixed identity from the model-wide table.
    pub(crate) fn value_for_schema_column<'row>(
        &'row self,
        column: &'row ColumnSchema<'model>,
    ) -> Result<Value<'row, 'model>> {
        self.value_for_column(column)
    }

    /// Resolves parent `CityObject` handles into borrowed `CityObject` ids.
    ///
    /// # Errors
    ///
    /// Returns an error when the model contains dangling parent handles.
    pub fn parents(&self) -> Result<IdList<'model>> {
        cityobject_id_list(self.model, self.parents)
    }

    /// Resolves child `CityObject` handles into borrowed `CityObject` ids.
    ///
    /// # Errors
    ///
    /// Returns an error when the model contains dangling child handles.
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

/// Borrowed list of `CityObject` identifiers for hierarchy columns.
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

/// One-row logical metadata table for a `CityJSON` model.
#[derive(Debug)]
pub struct MetadataTable<'model> {
    model: &'model CityModel,
    rows: Vec<MetadataRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> MetadataTable<'model> {
    /// Returns the source model backing this borrowed table.
    #[must_use]
    pub fn model(&self) -> &'model CityModel {
        self.model
    }

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

    #[must_use]
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
    model: &'model CityModel,
    rows: Vec<SemanticRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> SemanticTable<'model> {
    /// Returns the source model backing this borrowed table.
    #[must_use]
    pub fn model(&self) -> &'model CityModel {
        self.model
    }

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

impl SemanticRow<'_> {
    /// Returns the semantic type using its `CityJSON` display spelling.
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

    #[must_use]
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

/// Borrowed semantic primitive table with one row per geometry primitive.
///
/// Rows join primitive semantic assignments to semantic definition attributes.
/// Geometry bytes are intentionally exposed through [`semantic_primitive_geometry`]
/// so text writers can avoid computing WKB while `GeoPackage` writers can serialize
/// the same projection with a `geom` column.
#[derive(Debug)]
pub struct SemanticPrimitiveTable<'model> {
    model: &'model CityModel,
    rows: Vec<SemanticPrimitiveRow<'model>>,
    schema: TableSchema<'model>,
}

impl<'model> SemanticPrimitiveTable<'model> {
    /// Returns the source model backing this borrowed table.
    #[must_use]
    pub fn model(&self) -> &'model CityModel {
        self.model
    }

    /// Returns the flattened schema for semantic attributes.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates semantic primitive rows in source `CityObject` and geometry order.
    pub fn rows(&self) -> impl Iterator<Item = SemanticPrimitiveRowRef<'_, 'model>> {
        self.rows.iter().map(|row| SemanticPrimitiveRowRef {
            row,
            columns: &self.schema.columns,
        })
    }
}

/// Fixed semantic primitive fields plus borrowed semantic attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticPrimitiveRow<'model> {
    pub cityobject_id: &'model str,
    pub geometry_id: u64,
    pub semantic_id: Option<u64>,
    pub primitive_ix: u64,
    pub geometry_type: GeometryType,
    pub geometry_lod: Option<String>,
    pub semantic_type: Option<&'model SemanticType<OwnedStringStorage>>,
    pub primitive_type: PrimitiveType,
    pub point_ix: Option<u64>,
    pub linestring_ix: Option<u64>,
    pub solid_ix: Option<u64>,
    pub shell_ix: Option<u64>,
    pub surface_ix: Option<u64>,
    geometry_handle: GeometryHandle,
    attributes: Option<&'model OwnedAttributes>,
}

impl SemanticPrimitiveRow<'_> {
    /// Returns the semantic type using its `CityJSON` display spelling, when the
    /// primitive has a semantic assignment.
    #[must_use]
    pub fn semantic_type_name(&self) -> Option<impl Display + '_> {
        self.semantic_type
    }
}

/// Borrowed semantic primitive row bound to shared dynamic semantic columns.
#[derive(Clone, Copy, Debug)]
pub struct SemanticPrimitiveRowRef<'table, 'model> {
    row: &'table SemanticPrimitiveRow<'model>,
    columns: &'table [ColumnSchema<'model>],
}

impl<'table, 'model> SemanticPrimitiveRowRef<'table, 'model> {
    /// Returns fixed semantic primitive fields.
    #[must_use]
    pub fn fixed(&self) -> &'table SemanticPrimitiveRow<'model> {
        self.row
    }

    /// Resolves the semantic attribute value at `index`.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<Result<Value<'_, 'model>>> {
        self.columns
            .get(index)
            .map(|column| resolve_dynamic_value(self.row.attributes, column))
    }

    /// Resolves semantic attribute values lazily in shared schema order.
    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.columns
            .iter()
            .map(|column| resolve_dynamic_value(self.row.attributes, column))
    }
}

/// WKB bytes and source-space extent for one semantic primitive.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticPrimitiveGeometry {
    pub wkb: Vec<u8>,
    pub bbox: Option<[f64; 6]>,
}

/// Borrowed semantic hierarchy table with one parent-to-child edge per row.
///
/// Semantic ids are zero-based indices from the source semantic object handles.
/// The table is empty when no semantic objects declare parent or child handles.
#[derive(Debug)]
pub struct SemanticHierarchyTable {
    rows: Vec<SemanticHierarchyRow>,
}

impl SemanticHierarchyTable {
    /// Iterates hierarchy edges ordered by parent id and then child id.
    pub fn rows(&self) -> impl Iterator<Item = &SemanticHierarchyRow> {
        self.rows.iter()
    }
}

/// One parent-to-child semantic hierarchy edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticHierarchyRow {
    pub parent_id: u64,
    pub child_id: u64,
}

/// Borrowed semantic-assignment table with one row per mapped geometry primitive.
#[derive(Debug)]
pub struct SemanticAssignmentTable<'model> {
    rows: Vec<SemanticAssignmentRow<'model>>,
}

impl<'model> SemanticAssignmentTable<'model> {
    /// Iterates semantic assignment rows in `CityObject` and geometry order.
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
    pub geometry_id: u64,
    pub geometry_handle: GeometryHandle,
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

/// Text cell produced from a tabular value for delimiter-based writers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextCell {
    pub text: String,
    pub is_null: bool,
}

/// Serializes a logical tabular value to the shared compact text-cell contract.
///
/// Scalar values are written directly. Nested values, heterogeneous JSON
/// fallbacks, and geometry references inside nested values are encoded as compact
/// JSON text. Geometry references are encoded as hex ISO WKB.
///
/// # Errors
///
/// Returns an error when a lazy nested value cannot be resolved, a geometry
/// reference cannot be encoded as WKB, or an unsupported source attribute variant
/// is encountered.
pub fn value_to_text_cell(model: &CityModel, value: Value<'_, '_>) -> Result<TextCell> {
    Ok(match value {
        Value::Null => TextCell {
            text: String::new(),
            is_null: true,
        },
        Value::Boolean(value) => TextCell {
            text: value.to_string(),
            is_null: false,
        },
        Value::UInt64(value) => TextCell {
            text: value.to_string(),
            is_null: false,
        },
        Value::Int64(value) => TextCell {
            text: value.to_string(),
            is_null: false,
        },
        Value::Float64(value) => TextCell {
            text: value.to_string(),
            is_null: false,
        },
        Value::Utf8(value) => TextCell {
            text: value.to_string(),
            is_null: false,
        },
        Value::GeometryRef(value) => TextCell {
            text: geometry_ref_to_wkb_hex(model, value)?,
            is_null: false,
        },
        nested => TextCell {
            text: serde_json::to_string(&value_to_json(model, nested)?)?,
            is_null: false,
        },
    })
}

/// Serializes a logical tabular value to compact JSON-compatible data.
///
/// This is intentionally a tabular serializer, not `CityJSON` document
/// serialization. In particular, geometry references are emitted as hex ISO WKB.
///
/// # Errors
///
/// Returns an error when a lazy nested value cannot be resolved, a geometry
/// reference cannot be encoded as WKB, or an unsupported source attribute variant
/// is encountered.
pub fn value_to_json(model: &CityModel, value: Value<'_, '_>) -> Result<JsonValue> {
    Ok(match value {
        Value::Null => JsonValue::Null,
        Value::Boolean(value) => JsonValue::Bool(value),
        Value::UInt64(value) => JsonValue::Number(value.into()),
        Value::Int64(value) => JsonValue::Number(value.into()),
        Value::Float64(value) => {
            serde_json::Number::from_f64(value).map_or(JsonValue::Null, JsonValue::Number)
        }
        Value::Utf8(value) => JsonValue::String(value.to_string()),
        Value::GeometryRef(value) => JsonValue::String(geometry_ref_to_wkb_hex(model, value)?),
        Value::List(values) => {
            let mut items = Vec::with_capacity(values.len());
            for item in values.iter() {
                items.push(value_to_json(model, item?)?);
            }
            JsonValue::Array(items)
        }
        Value::Struct(values) => {
            let mut fields = Map::new();
            for field in values.fields() {
                let (name, value) = field?;
                fields.insert(name.to_string(), value_to_json(model, value)?);
            }
            JsonValue::Object(fields)
        }
        Value::Json(value) => attribute_value_to_json(model, value)?,
    })
}

/// Serializes a source attribute value using the tabular JSON fallback contract.
///
/// Geometry-valued attributes become hex ISO WKB strings. This differs from
/// `cityjson-json`, which serializes those values for `CityJSON` documents.
///
/// # Errors
///
/// Returns an error when the attribute variant is unsupported by the tabular
/// representation or a geometry reference cannot be encoded as WKB.
pub fn attribute_value_to_json(
    model: &CityModel,
    value: &OwnedAttributeValue,
) -> Result<JsonValue> {
    Ok(match value {
        OwnedAttributeValue::Bool(value) => JsonValue::Bool(*value),
        OwnedAttributeValue::Unsigned(value) => JsonValue::Number((*value).into()),
        OwnedAttributeValue::Integer(value) => JsonValue::Number((*value).into()),
        OwnedAttributeValue::Float(value) => {
            serde_json::Number::from_f64(*value).map_or(JsonValue::Null, JsonValue::Number)
        }
        OwnedAttributeValue::String(value) => JsonValue::String(value.clone()),
        OwnedAttributeValue::Vec(values) => JsonValue::Array(
            values
                .iter()
                .map(|value| attribute_value_to_json(model, value))
                .collect::<Result<Vec<_>>>()?,
        ),
        OwnedAttributeValue::Map(values) => {
            let mut fields = Map::new();
            for (name, value) in values {
                fields.insert(name.clone(), attribute_value_to_json(model, value)?);
            }
            JsonValue::Object(fields)
        }
        OwnedAttributeValue::Geometry(value) => {
            JsonValue::String(geometry_ref_to_wkb_hex(model, *value)?)
        }
        OwnedAttributeValue::Null => JsonValue::Null,
        unsupported => bail!("unsupported attribute value variant {unsupported}"),
    })
}

/// Resolves a geometry attribute handle and returns raw ISO WKB bytes.
///
/// # Errors
///
/// Returns an error when the handle is dangling, resolves to a geometry without
/// boundaries, or WKB conversion fails.
pub fn geometry_ref_to_wkb(model: &CityModel, value: GeometryHandle) -> Result<Vec<u8>> {
    let geometry = model
        .resolve_geometry(value)
        .with_context(|| format!("resolve geometry attribute handle {value:?}"))?;
    let Some(boundary) = geometry.boundaries() else {
        bail!("geometry attribute handle {value:?} resolves to a geometry without boundaries");
    };
    boundary
        .to_wkb(model.vertices())
        .with_context(|| format!("encode geometry attribute handle {value:?} as WKB"))
}

/// Resolves an address location geometry handle and returns raw ISO WKB bytes.
///
/// # Errors
///
/// Returns an error when the handle is dangling, does not resolve to a
/// `MultiPoint`, has no boundaries, or WKB conversion fails.
pub fn geometry_ref_to_multipoint_wkb(model: &CityModel, value: GeometryHandle) -> Result<Vec<u8>> {
    let geometry = model
        .resolve_geometry(value)
        .with_context(|| format!("resolve address.location geometry handle {value:?}"))?;
    if !matches!(geometry.type_geometry(), GeometryType::MultiPoint) {
        bail!(
            "address.location geometry handle {value:?} must resolve to MultiPoint, found {}",
            geometry.type_geometry()
        );
    }
    let Some(boundary) = geometry.boundaries() else {
        bail!(
            "address.location geometry handle {value:?} resolves to a geometry without boundaries"
        );
    };
    boundary
        .to_wkb(model.vertices())
        .with_context(|| format!("encode address.location geometry handle {value:?} as WKB"))
}

/// Resolves an address location geometry handle and returns hex ISO WKB.
///
/// # Errors
///
/// Returns an error when [`geometry_ref_to_multipoint_wkb`] fails.
pub fn geometry_ref_to_multipoint_wkb_hex(
    model: &CityModel,
    value: GeometryHandle,
) -> Result<String> {
    Ok(bytes_to_hex(&geometry_ref_to_multipoint_wkb(model, value)?))
}

fn geometry_ref_to_wkb_hex(model: &CityModel, value: GeometryHandle) -> Result<String> {
    Ok(bytes_to_hex(&geometry_ref_to_wkb(model, value)?))
}

#[must_use]
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Serializes model metadata to compact JSON for metadata-extension payloads.
///
/// # Errors
///
/// Returns an error when metadata `extra` contains an unsupported attribute
/// variant.
pub fn metadata_to_compact_json(
    model: &CityModel,
    metadata: &Metadata<OwnedStringStorage>,
) -> Result<String> {
    let mut object = Map::new();
    if let Some(identifier) = metadata.identifier() {
        object.insert(
            "identifier".to_string(),
            JsonValue::String(identifier.to_string()),
        );
    }
    if let Some(reference_date) = metadata.reference_date() {
        object.insert(
            "referenceDate".to_string(),
            JsonValue::String(reference_date.to_string()),
        );
    }
    if let Some(reference_system) = metadata.reference_system() {
        object.insert(
            "referenceSystem".to_string(),
            JsonValue::String(reference_system.to_string()),
        );
    }
    if let Some(title) = metadata.title() {
        object.insert("title".to_string(), JsonValue::String(title.to_string()));
    }
    if let Some(extent) = metadata.geographical_extent() {
        object.insert(
            "geographicalExtent".to_string(),
            JsonValue::Array(
                extent
                    .as_slice()
                    .iter()
                    .copied()
                    .map(JsonValue::from)
                    .collect(),
            ),
        );
    }
    if let Some(contact) = metadata.point_of_contact() {
        let mut contact_object = Map::new();
        contact_object.insert(
            "contactName".to_string(),
            JsonValue::String(contact.contact_name().to_string()),
        );
        contact_object.insert(
            "emailAddress".to_string(),
            JsonValue::String(contact.email_address().to_string()),
        );
        if let Some(role) = contact.role() {
            contact_object.insert("role".to_string(), JsonValue::String(role.to_string()));
        }
        if let Some(website) = contact.website().as_ref() {
            contact_object.insert("website".to_string(), JsonValue::String(website.clone()));
        }
        if let Some(kind) = contact.contact_type() {
            contact_object.insert(
                "contactType".to_string(),
                JsonValue::String(kind.to_string()),
            );
        }
        if let Some(phone) = contact.phone().as_ref() {
            contact_object.insert("phone".to_string(), JsonValue::String(phone.clone()));
        }
        if let Some(organization) = contact.organization().as_ref() {
            contact_object.insert(
                "organization".to_string(),
                JsonValue::String(organization.clone()),
            );
        }
        object.insert(
            "pointOfContact".to_string(),
            JsonValue::Object(contact_object),
        );
    }
    if let Some(extra) = metadata.extra() {
        for (name, value) in extra.iter() {
            object.insert(format!("+{name}"), attribute_value_to_json(model, value)?);
        }
    }
    Ok(serde_json::to_string(&JsonValue::Object(object))?)
}

/// Lazy borrowed list value.
#[derive(Debug)]
pub struct ListValue<'schema, 'model> {
    values: &'model [OwnedAttributeValue],
    item_type: &'schema LogicalType<'model>,
    item_nullable: bool,
    path: &'schema str,
}

impl<'model> ListValue<'_, 'model> {
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

impl<'model> StructValue<'_, 'model> {
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

/// Infers the shared schema and returns a borrowed `CityObject` table.
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
    let extra = infer_extra_schema_without_addresses(model)?;
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

pub(crate) fn tabulate_cityobject_type_schema<'model>(
    model: &'model CityModel,
    cityobject_type: &str,
) -> Result<TableSchema<'model>> {
    let matching_cityobjects = model
        .cityobjects()
        .iter()
        .map(|(_, object)| object)
        .filter(|object| object.type_cityobject().to_string() == cityobject_type)
        .collect::<Vec<_>>();
    let attributes = infer_attribute_schema(
        matching_cityobjects
            .iter()
            .map(|object| object.attributes()),
    )?;
    let extra = infer_extra_schema_without_addresses_for_objects(
        matching_cityobjects.iter().map(|object| object.extra()),
    )?;
    Ok(build_dynamic_schema(
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
    ))
}

fn infer_extra_schema_without_addresses(model: &CityModel) -> Result<StructSchema<'_>> {
    infer_extra_schema_without_addresses_for_objects(
        model.cityobjects().iter().map(|(_, object)| object.extra()),
    )
}

fn infer_extra_schema_without_addresses_for_objects<'model>(
    sources: impl IntoIterator<Item = Option<&'model OwnedAttributes>>,
) -> Result<StructSchema<'model>> {
    let mut schema = StructSchema::default();
    for (row_ix, source) in sources.into_iter().enumerate() {
        match source {
            Some(extra) => merge_extra_map_without_addresses(&mut schema, extra, row_ix)?,
            None => mark_all_nullable(&mut schema),
        }
    }
    Ok(schema)
}

fn merge_extra_map_without_addresses<'model>(
    schema: &mut StructSchema<'model>,
    values: &'model OwnedAttributes,
    seen_rows: usize,
) -> Result<()> {
    for field in schema.fields.values_mut() {
        if !values.contains_key(field.name) || field.name == "address" {
            field.nullable = true;
        }
    }
    for (name, value) in values.iter() {
        if name == "address" {
            continue;
        }
        merge_field(schema, name, value, seen_rows)?;
    }
    Ok(())
}

/// Tabulates `CityObject` parent/child edges into a standalone hierarchy table.
///
/// # Errors
///
/// Returns an error when a parent or child handle references a missing
/// `CityObject`.
pub fn tabulate_cityobject_hierarchy(model: &CityModel) -> Result<CityObjectHierarchyTable<'_>> {
    let mut edges = BTreeMap::<(&str, &str), HierarchyRow<'_>>::new();
    for (_, object) in model.cityobjects().iter() {
        let cityobject_id = object.id();
        for parent in cityobject_id_list(model, object.parents())?.iter() {
            edges
                .entry((parent, cityobject_id))
                .or_insert(HierarchyRow {
                    parent_id: parent,
                    child_id: cityobject_id,
                });
        }
        for child in cityobject_id_list(model, object.children())?.iter() {
            edges.entry((cityobject_id, child)).or_insert(HierarchyRow {
                parent_id: cityobject_id,
                child_id: child,
            });
        }
    }
    Ok(CityObjectHierarchyTable {
        rows: edges.into_values().collect(),
    })
}

/// Infers the address schema and returns one borrowed row per `extra.address`.
///
/// # Errors
///
/// Returns an error when an `address` member is present but is not an object, or
/// when an address value variant cannot be represented by the table vocabulary.
pub fn tabulate_addresses(model: &CityModel) -> Result<AddressTable<'_>> {
    let mut rows = Vec::new();
    for (cityobject_ix, (_, object)) in model.cityobjects().iter().enumerate() {
        for address in cityobject_addresses(object)? {
            rows.push(AddressRow {
                cityobject_id: object.id(),
                cityobject_ix: cityobject_ix as u64,
                cityobject_type: object.type_cityobject(),
                address,
            });
        }
    }

    let mut schema = StructSchema::default();
    for (row_ix, row) in rows.iter().enumerate() {
        merge_address_values(&mut schema, row.address, row_ix)?;
    }

    Ok(AddressTable {
        model,
        rows,
        schema: build_dynamic_schema(
            [(ColumnOrigin::Extra, schema)],
            &["cityobject_id", "cityobject_ix", "cityobject_type", "geom"],
        ),
    })
}

/// Infers the metadata table for a `CityJSON` model.
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
        model,
        rows: vec![row],
        schema: build_dynamic_schema(
            [(ColumnOrigin::MetadataExtra, extra)],
            &[
                "identifier",
                "reference_date",
                "reference_system",
                "title",
                "geographical_extent_wkb",
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

/// Infers the semantic-definition table for a `CityJSON` model.
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
        model,
        rows,
        schema: build_dynamic_schema(
            [(ColumnOrigin::SemanticAttributes, attributes)],
            &["semantic_id", "semantic_type", "parent", "children"],
        ),
    })
}

/// Tabulates semantic parent/child edges into a standalone hierarchy table.
pub fn tabulate_semantic_hierarchy(model: &CityModel) -> SemanticHierarchyTable {
    let mut edges = BTreeMap::<(u64, u64), SemanticHierarchyRow>::new();
    for (handle, semantic) in model.iter_semantics() {
        let semantic_id = semantic_handle_id(handle);
        if let Some(parent) = semantic.parent().map(semantic_handle_id) {
            edges
                .entry((parent, semantic_id))
                .or_insert(SemanticHierarchyRow {
                    parent_id: parent,
                    child_id: semantic_id,
                });
        }
        for child in semantic.children().unwrap_or_default().iter().copied() {
            let child_id = semantic_handle_id(child);
            edges
                .entry((semantic_id, child_id))
                .or_insert(SemanticHierarchyRow {
                    parent_id: semantic_id,
                    child_id,
                });
        }
    }
    SemanticHierarchyTable {
        rows: edges.into_values().collect(),
    }
}

/// Tabulates semantic primitives joined to semantic definition attributes.
///
/// # Errors
///
/// Returns an error when geometry handles cannot be resolved, semantic assignment
/// counts do not match primitive counts, or semantic attributes cannot be
/// represented by the table vocabulary.
pub fn tabulate_semantic_primitives(model: &CityModel) -> Result<SemanticPrimitiveTable<'_>> {
    let attributes = infer_attribute_schema(
        model
            .iter_semantics()
            .map(|(_, semantic)| semantic.attributes()),
    )?;
    let semantics_by_id = model
        .iter_semantics()
        .map(|(handle, semantic)| (semantic_handle_id(handle), semantic))
        .collect::<BTreeMap<_, _>>();
    let assignments = tabulate_semantic_assignments(model)?;
    let rows = assignments
        .rows
        .into_iter()
        .filter(|assignment| assignment.semantic_id.is_some())
        .map(|assignment| {
            let semantic = assignment
                .semantic_id
                .and_then(|semantic_id| semantics_by_id.get(&semantic_id).copied());
            SemanticPrimitiveRow {
                cityobject_id: assignment.cityobject_id,
                geometry_id: assignment.geometry_id,
                semantic_id: assignment.semantic_id,
                primitive_ix: assignment.primitive_ix,
                geometry_type: assignment.geometry_type,
                geometry_lod: assignment.geometry_lod,
                semantic_type: semantic.map(Semantic::type_semantic),
                primitive_type: assignment.primitive_type,
                point_ix: assignment.point_ix,
                linestring_ix: assignment.linestring_ix,
                solid_ix: assignment.solid_ix,
                shell_ix: assignment.shell_ix,
                surface_ix: assignment.surface_ix,
                geometry_handle: assignment.geometry_handle,
                attributes: semantic.and_then(Semantic::attributes),
            }
        })
        .collect();

    Ok(SemanticPrimitiveTable {
        model,
        rows,
        schema: build_dynamic_schema(
            [(ColumnOrigin::SemanticAttributes, attributes)],
            &[
                "cityobject_id",
                "geometry_id",
                "semantic_id",
                "primitive_ix",
                "geometry_type",
                "geometry_lod",
                "semantic_type",
                "geom",
            ],
        ),
    })
}

/// Encodes one semantic primitive as ISO WKB plus its source-space extent.
///
/// Points are encoded as `PointZ`, linestrings as `LineStringZ`, and surfaces as
/// `PolygonZ`. Surface primitives retain all rings from the source boundary. The
/// function resolves geometry instances through the model, matching semantic
/// assignment tabulation.
///
/// # Errors
///
/// Returns an error when the row references an incompatible boundary shape, a
/// primitive index is out of range, or a vertex index is dangling.
pub fn semantic_primitive_geometry(
    model: &CityModel,
    row: &SemanticPrimitiveRow<'_>,
) -> Result<SemanticPrimitiveGeometry> {
    let geometry = model
        .resolve_geometry(row.geometry_handle)
        .with_context(|| {
            format!(
                "resolve geometry for semantic primitive on CityObject {}",
                row.cityobject_id
            )
        })?;
    let Some(boundary) = geometry.boundaries() else {
        bail!(
            "semantic primitive {} on geometry {} of CityObject {} has no boundaries",
            row.primitive_ix,
            row.geometry_id,
            row.cityobject_id
        );
    };

    match row.primitive_type {
        PrimitiveType::Point => {
            let point_ix = required_index(row.point_ix, "point_ix", row)?;
            let points = boundary.to_nested_multi_point()?;
            let vertex = *points.get(point_ix).ok_or_else(|| {
                anyhow::anyhow!(
                    "point primitive index {} is out of range for geometry {} of CityObject {}",
                    point_ix,
                    row.geometry_id,
                    row.cityobject_id
                )
            })?;
            encode_point_primitive(model, vertex)
        }
        PrimitiveType::LineString => {
            let linestring_ix = required_index(row.linestring_ix, "linestring_ix", row)?;
            let linestrings = boundary.to_nested_multi_linestring()?;
            let vertices = linestrings.get(linestring_ix).ok_or_else(|| {
                anyhow::anyhow!(
                    "linestring primitive index {} is out of range for geometry {} of CityObject {}",
                    linestring_ix,
                    row.geometry_id,
                    row.cityobject_id
                )
            })?;
            encode_linestring_primitive(model, vertices)
        }
        PrimitiveType::Surface => {
            let surface = semantic_surface(boundary, *geometry.type_geometry(), row)?;
            encode_surface_primitive(model, surface)
        }
    }
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
        for (source_geometry_ix, geometry_handle) in geometry_handles.iter().copied().enumerate() {
            let source_geometry = model
                .get_geometry(geometry_handle)
                .ok_or_else(|| anyhow::anyhow!("dangling geometry handle {geometry_handle:?}"))?;
            let geometry = model.resolve_geometry(geometry_handle)?;
            let Some(semantics) = geometry.semantics() else {
                continue;
            };
            let Some(boundary) = geometry.boundaries() else {
                bail!(
                    "semantic assignments on source geometry index {source_geometry_ix} of CityObject {} have no boundaries",
                    object.id()
                );
            };

            let context = SemanticAssignmentContext {
                cityobject_id: object.id(),
                cityobject_ix: cityobject_ix as u64,
                geometry_id: geometry_handle_id(geometry_handle),
                geometry_handle,
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

fn cityobject_addresses(
    object: &cityjson_lib::cityjson_types::v2_0::CityObject<OwnedStringStorage>,
) -> Result<Vec<&HashMap<String, OwnedAttributeValue>>> {
    let Some(value) = object.extra().and_then(|extra| extra.get("address")) else {
        return Ok(Vec::new());
    };
    match value {
        OwnedAttributeValue::Null => Ok(Vec::new()),
        OwnedAttributeValue::Vec(values) => values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| match value {
                OwnedAttributeValue::Null => None,
                OwnedAttributeValue::Map(values) => Some(Ok(values)),
                other => Some(Err(anyhow::anyhow!(
                    "address[{index}] member for CityObject {} must be an object, found {other}",
                    object.id()
                ))),
            })
            .collect(),
        other => bail!(
            "address member for CityObject {} must be an array, found {other}",
            object.id()
        ),
    }
}

fn merge_address_values<'model>(
    schema: &mut StructSchema<'model>,
    values: &'model HashMap<String, OwnedAttributeValue>,
    seen_rows: usize,
) -> Result<()> {
    for field in schema.fields.values_mut() {
        if !values.contains_key(field.name) || field.name == "location" {
            field.nullable = true;
        }
    }
    for (name, value) in values {
        if name == "location" {
            continue;
        }
        merge_field(schema, name, value, seen_rows)?;
    }
    Ok(())
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
                .map(cityjson_lib::cityjson_types::v2_0::CityObject::id)
                .ok_or_else(|| anyhow::anyhow!("dangling CityObject handle {handle:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(IdList { ids })
}

fn semantic_handle_id(handle: SemanticHandle) -> u64 {
    u64::from(handle.raw_parts().0)
}

fn geometry_handle_id(handle: GeometryHandle) -> u64 {
    u64::from(handle.raw_parts().0)
}

fn required_index(value: Option<u64>, name: &str, row: &SemanticPrimitiveRow<'_>) -> Result<usize> {
    let value = value.ok_or_else(|| {
        anyhow::anyhow!(
            "semantic primitive {} on geometry {} of CityObject {} is missing {name}",
            row.primitive_ix,
            row.geometry_id,
            row.cityobject_id
        )
    })?;
    usize::try_from(value).with_context(|| format!("{name} value {value} does not fit in usize"))
}

fn semantic_surface(
    boundary: &Boundary<u32>,
    geometry_type: GeometryType,
    row: &SemanticPrimitiveRow<'_>,
) -> Result<Vec<Vec<u32>>> {
    let surface_ix = required_index(row.surface_ix, "surface_ix", row)?;
    match geometry_type {
        GeometryType::MultiSurface | GeometryType::CompositeSurface => {
            let surfaces = boundary.to_nested_multi_or_composite_surface()?;
            surfaces.get(surface_ix).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "surface primitive index {} is out of range for geometry {} of CityObject {}",
                    surface_ix,
                    row.geometry_id,
                    row.cityobject_id
                )
            })
        }
        GeometryType::Solid => {
            let shell_ix = required_index(row.shell_ix, "shell_ix", row)?;
            let shells = boundary.to_nested_solid()?;
            shells
                .get(shell_ix)
                .and_then(|shell| shell.get(surface_ix))
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "surface primitive shell {} surface {} is out of range for geometry {} of CityObject {}",
                        shell_ix,
                        surface_ix,
                        row.geometry_id,
                        row.cityobject_id
                    )
                })
        }
        GeometryType::MultiSolid | GeometryType::CompositeSolid => {
            let solid_ix = required_index(row.solid_ix, "solid_ix", row)?;
            let shell_ix = required_index(row.shell_ix, "shell_ix", row)?;
            let solids = boundary.to_nested_multi_or_composite_solid()?;
            solids
                .get(solid_ix)
                .and_then(|solid| solid.get(shell_ix))
                .and_then(|shell| shell.get(surface_ix))
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "surface primitive solid {} shell {} surface {} is out of range for geometry {} of CityObject {}",
                        solid_ix,
                        shell_ix,
                        surface_ix,
                        row.geometry_id,
                        row.cityobject_id
                    )
                })
        }
        other => bail!(
            "surface semantic primitive on geometry {} of CityObject {} is incompatible with geometry type {other}",
            row.geometry_id,
            row.cityobject_id
        ),
    }
}

fn encode_point_primitive(model: &CityModel, vertex: u32) -> Result<SemanticPrimitiveGeometry> {
    let coordinate = semantic_vertex_coordinate(model, vertex)?;
    let mut wkb = Vec::with_capacity(1 + 4 + 24);
    push_wkb_header(&mut wkb, 1001);
    push_wkb_coordinate(&mut wkb, coordinate);
    Ok(SemanticPrimitiveGeometry {
        wkb,
        bbox: Some(coordinate_bbox(coordinate)),
    })
}

fn encode_linestring_primitive(
    model: &CityModel,
    vertices: &[u32],
) -> Result<SemanticPrimitiveGeometry> {
    let mut wkb = Vec::with_capacity(1 + 4 + 4 + vertices.len() * 24);
    push_wkb_header(&mut wkb, 1002);
    push_wkb_count(&mut wkb, vertices.len(), "linestring vertex count")?;
    let mut bbox = None;
    for vertex in vertices {
        let coordinate = semantic_vertex_coordinate(model, *vertex)?;
        update_bbox(&mut bbox, coordinate);
        push_wkb_coordinate(&mut wkb, coordinate);
    }
    Ok(SemanticPrimitiveGeometry { wkb, bbox })
}

fn encode_surface_primitive(
    model: &CityModel,
    rings: Vec<Vec<u32>>,
) -> Result<SemanticPrimitiveGeometry> {
    let vertex_count = rings.iter().map(Vec::len).sum::<usize>();
    let mut wkb = Vec::with_capacity(1 + 4 + 4 + rings.len() * 4 + vertex_count * 24);
    push_wkb_header(&mut wkb, 1003);
    push_wkb_count(&mut wkb, rings.len(), "surface ring count")?;
    let mut bbox = None;
    for ring in rings {
        push_wkb_count(&mut wkb, ring.len(), "surface ring vertex count")?;
        for vertex in ring {
            let coordinate = semantic_vertex_coordinate(model, vertex)?;
            update_bbox(&mut bbox, coordinate);
            push_wkb_coordinate(&mut wkb, coordinate);
        }
    }
    Ok(SemanticPrimitiveGeometry { wkb, bbox })
}

fn semantic_vertex_coordinate(model: &CityModel, vertex: u32) -> Result<[f64; 3]> {
    let vertex = model
        .get_vertex(VertexIndex::new(vertex))
        .with_context(|| format!("missing semantic primitive vertex {vertex}"))?;
    Ok(vertex.to_array())
}

fn push_wkb_header(out: &mut Vec<u8>, geometry_type: u32) {
    out.push(1);
    out.extend_from_slice(&geometry_type.to_le_bytes());
}

fn push_wkb_count(out: &mut Vec<u8>, count: usize, label: &str) -> Result<()> {
    let count = u32::try_from(count).with_context(|| format!("{label} exceeds u32"))?;
    out.extend_from_slice(&count.to_le_bytes());
    Ok(())
}

fn push_wkb_coordinate(out: &mut Vec<u8>, coordinate: [f64; 3]) {
    for value in coordinate {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn coordinate_bbox([x, y, z]: [f64; 3]) -> [f64; 6] {
    [x, y, z, x, y, z]
}

fn update_bbox(bbox: &mut Option<[f64; 6]>, coordinate: [f64; 3]) {
    match bbox {
        Some(existing) => {
            existing[0] = existing[0].min(coordinate[0]);
            existing[1] = existing[1].min(coordinate[1]);
            existing[2] = existing[2].min(coordinate[2]);
            existing[3] = existing[3].max(coordinate[0]);
            existing[4] = existing[4].max(coordinate[1]);
            existing[5] = existing[5].max(coordinate[2]);
        }
        None => *bbox = Some(coordinate_bbox(coordinate)),
    }
}

#[derive(Clone, Debug)]
struct SemanticAssignmentContext<'model> {
    cityobject_id: &'model str,
    cityobject_ix: u64,
    geometry_id: u64,
    geometry_handle: GeometryHandle,
    geometry_type: GeometryType,
    geometry_lod: Option<String>,
    geometry_is_instance: bool,
}

#[derive(Clone, Copy, Debug)]
struct SurfacePath {
    solid: Option<u64>,
    shell: Option<u64>,
    surface: u64,
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
            geometry_id: context.geometry_id,
            geometry_handle: context.geometry_handle,
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
            geometry_id: context.geometry_id,
            geometry_handle: context.geometry_handle,
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
            geometry_id: context.geometry_id,
            geometry_handle: context.geometry_handle,
            geometry_type: context.geometry_type,
            geometry_lod: context.geometry_lod.clone(),
            geometry_is_instance: context.geometry_is_instance,
            primitive_type: PrimitiveType::Surface,
            primitive_ix: primitive_ix as u64,
            point_ix: None,
            linestring_ix: None,
            solid_ix: path.solid,
            shell_ix: path.shell,
            surface_ix: Some(path.surface),
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
            context.geometry_id,
            context.cityobject_id
        );
    }
    Ok(())
}

fn surface_paths_for_multi_surface(surface_count: usize) -> Vec<SurfacePath> {
    (0..surface_count)
        .map(|surface_ix| SurfacePath {
            solid: None,
            shell: None,
            surface: surface_ix as u64,
        })
        .collect()
}

fn surface_paths_for_solid<Surface>(shells: &[Vec<Surface>]) -> Vec<SurfacePath> {
    let mut paths = Vec::new();
    for (shell_ix, surfaces) in shells.iter().enumerate() {
        for surface_ix in 0..surfaces.len() {
            paths.push(SurfacePath {
                solid: None,
                shell: Some(shell_ix as u64),
                surface: surface_ix as u64,
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
                    solid: Some(solid_ix as u64),
                    shell: Some(shell_ix as u64),
                    surface: surface_ix as u64,
                });
            }
        }
    }
    paths
}

/// Merges one `CityObject` attribute map into an ordered inferred tree.
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
fn infer_type(value: &OwnedAttributeValue) -> Result<LogicalType<'_>> {
    Ok(match value {
        OwnedAttributeValue::Bool(_) => LogicalType::Boolean,
        OwnedAttributeValue::Unsigned(_) => LogicalType::UInt64,
        OwnedAttributeValue::Integer(_) => LogicalType::Int64,
        OwnedAttributeValue::Float(_) => LogicalType::Float64,
        OwnedAttributeValue::String(_) => LogicalType::Utf8,
        OwnedAttributeValue::Null | OwnedAttributeValue::Geometry(_) => LogicalType::Null,
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
fn infer_list_type(values: &[OwnedAttributeValue]) -> Result<LogicalType<'_>> {
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
    if matches!(
        value,
        OwnedAttributeValue::Null | OwnedAttributeValue::Geometry(_)
    ) {
        return Ok(());
    }
    match (&mut *current, value) {
        (LogicalType::Null, _) => *current = infer_type(value)?,
        (LogicalType::Boolean, OwnedAttributeValue::Bool(_))
        | (LogicalType::UInt64, OwnedAttributeValue::Unsigned(_))
        | (LogicalType::Int64, OwnedAttributeValue::Integer(_))
        | (
            LogicalType::Float64,
            OwnedAttributeValue::Float(_)
            | OwnedAttributeValue::Unsigned(_)
            | OwnedAttributeValue::Integer(_),
        )
        | (LogicalType::Utf8, OwnedAttributeValue::String(_))
        | (LogicalType::Json, _) => {}
        (LogicalType::UInt64, OwnedAttributeValue::Integer(_))
        | (LogicalType::Int64, OwnedAttributeValue::Unsigned(_)) => {
            *current = LogicalType::Int64;
        }
        (LogicalType::UInt64 | LogicalType::Int64, OwnedAttributeValue::Float(_)) => {
            *current = LogicalType::Float64;
        }
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
    let mut used_names = reserved_name_set(reserved_names);
    for (origin, schema) in groups {
        flatten_struct_schema(
            origin,
            schema,
            &mut path,
            false,
            &mut name_buffer,
            &mut columns,
            &mut used_names,
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
    used_names: &mut HashSet<String>,
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
                    used_names,
                );
            }
            logical_type => {
                build_column_name(name_buffer, origin, path);
                debug_assert!(!matches!(logical_type, LogicalType::Struct(_)));
                columns.push(ColumnSchema {
                    origin,
                    path: path.clone(),
                    name: unique_column_name(name_buffer, used_names),
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

fn reserved_name_set(reserved_names: &[&str]) -> HashSet<String> {
    reserved_names
        .iter()
        .map(|name| column_name_key(name))
        .collect()
}

/// Returns the first column name not used by fixed or dynamic columns.
fn unique_column_name(base: &str, used_names: &mut HashSet<String>) -> String {
    if used_names.insert(column_name_key(base)) {
        return base.to_string();
    }

    let mut suffix = 2;
    loop {
        let candidate = format!("{base}__{suffix}");
        if used_names.insert(column_name_key(&candidate)) {
            return candidate;
        }
        suffix += 1;
    }
}

fn column_name_key(name: &str) -> String {
    name.to_ascii_lowercase()
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
    let Some(value) = attributes.and_then(|attributes| attributes.get(first)) else {
        return Ok(None);
    };
    resolve_remaining_path(value, remaining, column_name)
}

fn resolve_path_in_map<'model>(
    values: Option<&'model HashMap<String, OwnedAttributeValue>>,
    path: &[&str],
    column_name: &str,
) -> Result<Option<&'model OwnedAttributeValue>> {
    let Some((first, remaining)) = path.split_first() else {
        bail!("{column_name}: empty source path");
    };
    let Some(value) = values.and_then(|values| values.get(*first)) else {
        return Ok(None);
    };
    resolve_remaining_path(value, remaining, column_name)
}

fn resolve_remaining_path<'model>(
    mut value: &'model OwnedAttributeValue,
    remaining: &[&str],
    column_name: &str,
) -> Result<Option<&'model OwnedAttributeValue>> {
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
        (LogicalType::Float64, OwnedAttributeValue::Unsigned(value)) => Ok(Value::Float64(
            num_traits::ToPrimitive::to_f64(value).ok_or_else(|| {
                anyhow::anyhow!("{path}: unsigned integer {value} cannot be represented as Float64")
            })?,
        )),
        (LogicalType::Float64, OwnedAttributeValue::Integer(value)) => Ok(Value::Float64(
            num_traits::ToPrimitive::to_f64(value).ok_or_else(|| {
                anyhow::anyhow!("{path}: integer {value} cannot be represented as Float64")
            })?,
        )),
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
