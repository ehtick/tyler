//! Shared borrowed tabular projection of CityJSON CityObjects.
//!
//! This module projects a [`CityModel`] into a format-neutral schema and a lazy
//! sequence of CityObject rows for flat writers such as CSV, TSV, and
//! GeoPackage. It defines logical fields and values only; delimiters, SQLite
//! types, and text encoding belong to the writers.
//!
//! # API vocabulary
//!
//! The tabular API is organized into three groups:
//!
//! - Table views: [`TableView`], [`TableSchema`],
//!   [`RowView`], [`ColumnSchema`], and [`ColumnSource`].
//! - Projected schemas: [`ProjectedType`], [`StructSchema`], and
//!   [`StructFieldSchema`].
//! - Projected values: [`ProjectedValue`], [`ListValueView`], and
//!   [`StructValueView`].
//!
//! The table types describe the flattened row layout. Schema types describe the
//! logical type accepted by a column or a nested container item. Value types are
//! borrowed views of actual CityObject data. For example, [`ProjectedType`]
//! describes a value while [`ProjectedValue`] exposes one value from a row.
//! Nested schema and value types remain under this module; the crate root exports
//! only the primary table vocabulary.
//!
//! # Data flow
//!
//! [`project_cityobjects`] scans the model once to infer the shared dynamic
//! schema. The returned table view owns that schema and borrows the model.
//! Iterating rows walks the model directly in CityObject order. Rows and values
//! are views: source identifiers, strings, attribute names, JSON fallback values,
//! lists, and structs are borrowed rather than copied into a second table.
//!
//! Each row exposes fixed identity fields and one logical value for every
//! projected column. Values are resolved on demand. Lists and structs retained
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
//! Inference and the final projected-column schema are separate layers. During
//! inference, maps form a private tree used to discover paths and merge field
//! types across rows. That private tree is flattened before the public table
//! schema is produced. [`ProjectedType::Struct`] is reserved for structs that
//! remain nested inside a retained container, primarily lists. A top-level
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
//! produces these projected columns:
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
//! Struct schemas and struct value views are still needed when a struct occurs
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
//! produces one projected column:
//!
//! ```text
//! attributes__items  List<Struct{name: Utf8, count: UInt64}>
//! ```
//!
//! Here the `items` list remains a single column, so its item structs require a
//! nested [`StructSchema`] and are read through a lazy [`StructValueView`].
//!
//! Physical names start with the column source and join path segments with `__`.
//! Within a path segment, `%` is escaped as `%25` and literal `__` as `%5F%5F`.
//! Conflicts are resolved deterministically with `__2`, `__3`, and later
//! suffixes. Null-only fields do not produce columns. Attribute columns precede
//! extra columns, and fields are ordered lexicographically at every map level.
//!
//! # Invariants
//!
//! A table view has one row per CityObject in model order. Row ordinals are
//! zero-based, and every row uses the same ordered schema. Value index `n`
//! corresponds to projected-column index `n`. Missing nullable values become
//! null values. Non-null values either conform to the inferred logical type or
//! return a path-bearing error; they are never silently converted to an
//! unrelated type. The table view and every view derived from it are bounded by
//! the lifetime of the source model.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;

use anyhow::{bail, Result};
use cityjson_lib::cityjson_types::resources::handles::GeometryHandle;
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::{CityObjectType, OwnedAttributeValue, OwnedAttributes};
use cityjson_lib::CityModel;

/// Borrowed CityObject table with one inferred schema.
///
/// The view owns schema containers and physical column names, but borrows all
/// source field names and values from the model. It does not store rows.
#[derive(Debug)]
pub struct TableView<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

impl<'model> TableView<'model> {
    /// Returns the shared flattened schema used by every projected row.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates over CityObjects in model order without allocating rows.
    pub fn rows(&self) -> impl Iterator<Item = RowView<'_, 'model>> {
        self.model
            .cityobjects()
            .iter()
            .enumerate()
            .map(|(cityobject_ix, (_, object))| RowView {
                cityobject_id: object.id(),
                cityobject_ix: cityobject_ix as u64,
                cityobject_type: object.type_cityobject(),
                bbox: object.geographical_extent().map(|bbox| (*bbox).into()),
                attributes: object.attributes(),
                extra: object.extra(),
                schema: &self.schema,
            })
    }
}

/// Ordered dynamic-column schema shared by every projected row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableSchema<'model> {
    /// Columns in the same positional order returned by row value iterators.
    pub columns: Vec<ColumnSchema<'model>>,
}

/// Definition of one flattened dynamic table column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSchema<'model> {
    /// CityObject member from which the column originates.
    pub source: ColumnSource,
    /// Unescaped nested source path below the source namespace.
    pub path: Vec<&'model str>,
    /// Escaped and unique physical column name.
    pub name: String,
    /// Logical type shared by every non-null value in the column.
    pub projected_type: ProjectedType<'model>,
    /// Whether the column accepts missing or explicit-null values.
    pub nullable: bool,
}

/// Source CityObject member from which a projected column originates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnSource {
    /// Column inferred from the CityObject `attributes` member.
    Attributes,
    /// Column inferred from custom CityObject members.
    Extra,
}

impl ColumnSource {
    /// Returns the physical source prefix used in flattened column names.
    fn as_str(self) -> &'static str {
        match self {
            Self::Attributes => "attributes",
            Self::Extra => "extra",
        }
    }
}

/// Logical type of a projected column or nested container item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectedType<'model> {
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
    /// Original source value used when no stable typed projection exists.
    Json,
    /// Reference to geometry owned by the source model.
    GeometryRef,
    /// Homogeneous ordered values.
    List {
        /// Whether an item in the list may be null.
        item_nullable: bool,
        /// Logical type shared by non-null list items.
        item: Box<ProjectedType<'model>>,
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
    /// Projected logical type of the field.
    pub projected_type: ProjectedType<'model>,
    /// Whether the field may be missing or explicitly null.
    pub nullable: bool,
}

/// One allocation-free row view over a source CityObject.
#[derive(Clone, Copy, Debug)]
pub struct RowView<'table, 'model> {
    /// CityJSON object identifier borrowed from the model.
    pub cityobject_id: &'model str,
    /// Zero-based ordinal in model CityObject order.
    pub cityobject_ix: u64,
    /// Stored source `geographicalExtent`, when present.
    pub bbox: Option<[f64; 6]>,
    cityobject_type: &'model CityObjectType<OwnedStringStorage>,
    attributes: Option<&'model OwnedAttributes>,
    extra: Option<&'model OwnedAttributes>,
    schema: &'table TableSchema<'model>,
}

impl<'table, 'model> RowView<'table, 'model> {
    /// Returns the CityObject type using its CityJSON display spelling.
    #[must_use]
    pub fn cityobject_type_name(&self) -> impl Display + '_ {
        self.cityobject_type
    }

    /// Resolves the projected value at `index`.
    ///
    /// Returns `None` when `index` is outside the shared schema. Otherwise the
    /// result contains a borrowed value or a path-bearing conversion error.
    pub fn value(&self, index: usize) -> Option<Result<ProjectedValue<'_, 'model>>> {
        self.schema
            .columns
            .get(index)
            .map(|column| self.value_for_column(column))
    }

    /// Resolves projected values lazily in shared schema order.
    pub fn values(&self) -> impl Iterator<Item = Result<ProjectedValue<'_, 'model>>> {
        self.schema
            .columns
            .iter()
            .map(|column| self.value_for_column(column))
    }

    /// Resolves one projected column against this row's matching source map.
    fn value_for_column<'row>(
        &'row self,
        column: &'row ColumnSchema<'model>,
    ) -> Result<ProjectedValue<'row, 'model>> {
        let attributes = match column.source {
            ColumnSource::Attributes => self.attributes,
            ColumnSource::Extra => self.extra,
        };
        let value = resolve_path(attributes, &column.path, &column.name)?;
        build_projected_value(value, &column.projected_type, column.nullable, &column.name)
    }
}

/// Borrowed logical value from one row and projected column.
#[derive(Debug)]
pub enum ProjectedValue<'schema, 'model> {
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
    /// List traversed lazily through a borrowed view.
    List(ListValueView<'schema, 'model>),
    /// Struct traversed lazily through a borrowed view.
    Struct(StructValueView<'schema, 'model>),
    /// Original source value borrowed after heterogeneous fallback.
    Json(&'model OwnedAttributeValue),
}

/// Lazy borrowed view of a list-valued projected value.
#[derive(Debug)]
pub struct ListValueView<'schema, 'model> {
    values: &'model [OwnedAttributeValue],
    item_type: &'schema ProjectedType<'model>,
    item_nullable: bool,
    path: &'schema str,
}

impl<'schema, 'model> ListValueView<'schema, 'model> {
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
    pub fn iter(&self) -> impl Iterator<Item = Result<ProjectedValue<'_, 'model>>> {
        self.values.iter().map(|value| {
            build_projected_value(Some(value), self.item_type, self.item_nullable, self.path)
        })
    }
}

/// Lazy borrowed view of a struct retained inside a container value.
#[derive(Debug)]
pub struct StructValueView<'schema, 'model> {
    values: &'model HashMap<String, OwnedAttributeValue>,
    schema: &'schema StructSchema<'model>,
    path: &'schema str,
}

impl<'schema, 'model> StructValueView<'schema, 'model> {
    /// Resolves struct fields lazily in lexical schema order.
    ///
    /// Each item contains the borrowed field name and its projected value.
    pub fn fields(
        &self,
    ) -> impl Iterator<Item = Result<(&'model str, ProjectedValue<'_, 'model>)>> {
        self.schema.fields.values().map(|field| {
            let value = build_projected_value(
                self.values.get(field.name),
                &field.projected_type,
                field.nullable,
                self.path,
            )?;
            Ok((field.name, value))
        })
    }
}

/// Private schema node used while merging and flattening source maps.
#[derive(Debug)]
enum InferredType<'model> {
    Null,
    Boolean,
    UInt64,
    Int64,
    Float64,
    Utf8,
    Json,
    GeometryRef,
    List {
        item_nullable: bool,
        item: Box<InferredType<'model>>,
    },
    Struct(InferredStruct<'model>),
}

/// Private field in an inferred map tree.
#[derive(Debug)]
struct InferredField<'model> {
    name: &'model str,
    value: InferredType<'model>,
    nullable: bool,
}

/// Private ordered map tree consumed during column flattening.
#[derive(Debug, Default)]
struct InferredStruct<'model> {
    fields: BTreeMap<&'model str, InferredField<'model>>,
}

/// Infers the shared schema and returns a borrowed CityObject table view.
///
/// # Errors
///
/// Returns an error when an attribute value variant cannot be represented by the
/// projection's logical type vocabulary.
pub fn project_cityobjects(model: &CityModel) -> Result<TableView<'_>> {
    let mut attributes = InferredStruct::default();
    let mut extra = InferredStruct::default();
    for (row_ix, (_, object)) in model.cityobjects().iter().enumerate() {
        match object.attributes() {
            Some(values) => merge_attribute_map(&mut attributes, values, row_ix)?,
            None => mark_all_nullable(&mut attributes),
        }
        match object.extra() {
            Some(values) => merge_attribute_map(&mut extra, values, row_ix)?,
            None => mark_all_nullable(&mut extra),
        }
    }
    Ok(TableView {
        model,
        schema: build_table_schema(attributes, extra),
    })
}

/// Merges one CityObject attribute map into an ordered inferred tree.
fn merge_attribute_map<'model>(
    spec: &mut InferredStruct<'model>,
    attributes: &'model OwnedAttributes,
    seen_rows: usize,
) -> Result<()> {
    for field in spec.fields.values_mut() {
        if !attributes.contains_key(field.name) {
            field.nullable = true;
        }
    }
    for (name, value) in attributes.iter() {
        merge_field(spec, name, value, seen_rows)?;
    }
    Ok(())
}

/// Merges one nested map into an ordered inferred tree.
fn merge_attribute_values<'model>(
    spec: &mut InferredStruct<'model>,
    values: &'model HashMap<String, OwnedAttributeValue>,
    seen_rows: usize,
) -> Result<()> {
    for field in spec.fields.values_mut() {
        if !values.contains_key(field.name) {
            field.nullable = true;
        }
    }
    for (name, value) in values {
        merge_field(spec, name, value, seen_rows)?;
    }
    Ok(())
}

/// Adds a field or merges its value into an existing inferred field.
fn merge_field<'model>(
    spec: &mut InferredStruct<'model>,
    name: &'model str,
    value: &'model OwnedAttributeValue,
    seen_rows: usize,
) -> Result<()> {
    if let Some(field) = spec.fields.get_mut(name) {
        if matches!(value, OwnedAttributeValue::Null) {
            field.nullable = true;
        } else {
            merge_inferred_type_with_value(&mut field.value, value)?;
        }
    } else {
        spec.fields.insert(
            name,
            InferredField {
                name,
                value: infer_type(value)?,
                nullable: seen_rows > 0 || matches!(value, OwnedAttributeValue::Null),
            },
        );
    }
    Ok(())
}

/// Marks every currently known field nullable for a missing source map.
fn mark_all_nullable(spec: &mut InferredStruct<'_>) {
    for field in spec.fields.values_mut() {
        field.nullable = true;
    }
}

/// Infers the private logical type of one borrowed source value recursively.
fn infer_type<'model>(value: &'model OwnedAttributeValue) -> Result<InferredType<'model>> {
    Ok(match value {
        OwnedAttributeValue::Null => InferredType::Null,
        OwnedAttributeValue::Bool(_) => InferredType::Boolean,
        OwnedAttributeValue::Unsigned(_) => InferredType::UInt64,
        OwnedAttributeValue::Integer(_) => InferredType::Int64,
        OwnedAttributeValue::Float(_) => InferredType::Float64,
        OwnedAttributeValue::String(_) => InferredType::Utf8,
        OwnedAttributeValue::Geometry(_) => InferredType::GeometryRef,
        OwnedAttributeValue::Vec(values) => infer_list_type(values)?,
        OwnedAttributeValue::Map(values) => {
            let mut spec = InferredStruct::default();
            merge_attribute_values(&mut spec, values, 0)?;
            InferredType::Struct(spec)
        }
        unsupported => bail!("unsupported attribute value variant {unsupported}"),
    })
}

/// Infers a homogeneous list item type or falls back to JSON.
fn infer_list_type<'model>(values: &'model [OwnedAttributeValue]) -> Result<InferredType<'model>> {
    let mut item_nullable = false;
    let mut item_type = InferredType::Null;
    let mut saw_item = false;
    for value in values {
        if matches!(value, OwnedAttributeValue::Null) {
            item_nullable = true;
        } else if saw_item {
            merge_inferred_type_with_value(&mut item_type, value)?;
        } else {
            item_type = infer_type(value)?;
            saw_item = true;
        }
    }
    if matches!(item_type, InferredType::Json) {
        Ok(InferredType::Json)
    } else {
        Ok(InferredType::List {
            item_nullable,
            item: Box::new(item_type),
        })
    }
}

/// Merges a source value into an existing inferred type using widening rules.
fn merge_inferred_type_with_value<'model>(
    current: &mut InferredType<'model>,
    value: &'model OwnedAttributeValue,
) -> Result<()> {
    if matches!(value, OwnedAttributeValue::Null) {
        return Ok(());
    }
    match (&mut *current, value) {
        (InferredType::Null, _) => *current = infer_type(value)?,
        (InferredType::Boolean, OwnedAttributeValue::Bool(_))
        | (InferredType::UInt64, OwnedAttributeValue::Unsigned(_))
        | (InferredType::Int64, OwnedAttributeValue::Integer(_))
        | (InferredType::Float64, OwnedAttributeValue::Float(_))
        | (InferredType::Utf8, OwnedAttributeValue::String(_))
        | (InferredType::GeometryRef, OwnedAttributeValue::Geometry(_))
        | (InferredType::Json, _) => {}
        (InferredType::UInt64, OwnedAttributeValue::Integer(_))
        | (InferredType::Int64, OwnedAttributeValue::Unsigned(_)) => {
            *current = InferredType::Int64;
        }
        (InferredType::UInt64 | InferredType::Int64, OwnedAttributeValue::Float(_)) => {
            *current = InferredType::Float64;
        }
        (
            InferredType::Float64,
            OwnedAttributeValue::Unsigned(_) | OwnedAttributeValue::Integer(_),
        ) => {}
        (
            InferredType::List {
                item_nullable,
                item,
            },
            OwnedAttributeValue::Vec(values),
        ) => {
            for value in values {
                if matches!(value, OwnedAttributeValue::Null) {
                    *item_nullable = true;
                } else {
                    merge_inferred_type_with_value(item, value)?;
                    if matches!(item.as_ref(), InferredType::Json) {
                        *current = InferredType::Json;
                        break;
                    }
                }
            }
        }
        (InferredType::Struct(spec), OwnedAttributeValue::Map(values)) => {
            merge_attribute_values(spec, values, 1)?;
        }
        _ => *current = InferredType::Json,
    }
    Ok(())
}

/// Flattens inferred attribute and extra trees into one shared table schema.
fn build_table_schema<'model>(
    attributes: InferredStruct<'model>,
    extra: InferredStruct<'model>,
) -> TableSchema<'model> {
    let mut columns = Vec::new();
    let mut path = Vec::new();
    let mut name_buffer = String::new();
    flatten_inferred_struct(
        ColumnSource::Attributes,
        attributes,
        &mut path,
        false,
        &mut name_buffer,
        &mut columns,
    );
    flatten_inferred_struct(
        ColumnSource::Extra,
        extra,
        &mut path,
        false,
        &mut name_buffer,
        &mut columns,
    );
    TableSchema { columns }
}

/// Recursively emits inferred non-struct leaves as projected columns.
fn flatten_inferred_struct<'model>(
    source: ColumnSource,
    spec: InferredStruct<'model>,
    path: &mut Vec<&'model str>,
    inherited_nullable: bool,
    name_buffer: &mut String,
    columns: &mut Vec<ColumnSchema<'model>>,
) {
    for field in spec.fields.into_values() {
        path.push(field.name);
        let nullable = inherited_nullable || field.nullable;
        match field.value {
            InferredType::Null => {}
            InferredType::Struct(spec) => {
                flatten_inferred_struct(source, spec, path, nullable, name_buffer, columns);
            }
            inferred_type => {
                build_column_name(name_buffer, source, path);
                let projected_type = into_projected_type(inferred_type);
                debug_assert!(!matches!(projected_type, ProjectedType::Struct(_)));
                columns.push(ColumnSchema {
                    source,
                    path: path.clone(),
                    name: unique_column_name(name_buffer, columns),
                    projected_type,
                    nullable,
                });
            }
        }
        path.pop();
    }
}

/// Converts a retained inferred type into its public projected schema.
fn into_projected_type<'model>(inferred: InferredType<'model>) -> ProjectedType<'model> {
    match inferred {
        InferredType::Null => ProjectedType::Null,
        InferredType::Boolean => ProjectedType::Boolean,
        InferredType::UInt64 => ProjectedType::UInt64,
        InferredType::Int64 => ProjectedType::Int64,
        InferredType::Float64 => ProjectedType::Float64,
        InferredType::Utf8 => ProjectedType::Utf8,
        InferredType::Json => ProjectedType::Json,
        InferredType::GeometryRef => ProjectedType::GeometryRef,
        InferredType::List {
            item_nullable,
            item,
        } => ProjectedType::List {
            item_nullable,
            item: Box::new(into_projected_type(*item)),
        },
        InferredType::Struct(spec) => ProjectedType::Struct(StructSchema {
            fields: spec
                .fields
                .into_values()
                .map(|field| {
                    (
                        field.name,
                        StructFieldSchema {
                            name: field.name,
                            projected_type: into_projected_type(field.value),
                            nullable: field.nullable,
                        },
                    )
                })
                .collect(),
        }),
    }
}

/// Builds an escaped physical name in a reusable string buffer.
fn build_column_name(buffer: &mut String, source: ColumnSource, path: &[&str]) {
    buffer.clear();
    buffer.push_str(source.as_str());
    for segment in path {
        buffer.push_str("__");
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
fn unique_column_name(base: &str, columns: &[ColumnSchema<'_>]) -> String {
    if !column_name_exists(base, columns) {
        return base.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}__{suffix}");
        if !column_name_exists(&candidate, columns) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Reports whether a physical name is reserved or already emitted.
fn column_name_exists(name: &str, columns: &[ColumnSchema<'_>]) -> bool {
    const FIXED_COLUMNS: [&str; 4] = ["cityobject_id", "cityobject_ix", "cityobject_type", "bbox"];
    FIXED_COLUMNS.contains(&name) || columns.iter().any(|column| column.name == name)
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

/// Validates and exposes one source value as a borrowed projected value.
fn build_projected_value<'schema, 'model>(
    value: Option<&'model OwnedAttributeValue>,
    projected_type: &'schema ProjectedType<'model>,
    nullable: bool,
    path: &'schema str,
) -> Result<ProjectedValue<'schema, 'model>> {
    let Some(value) = value else {
        if nullable {
            return Ok(ProjectedValue::Null);
        }
        bail!("{path}: missing non-nullable value");
    };
    if matches!(value, OwnedAttributeValue::Null) {
        if nullable {
            return Ok(ProjectedValue::Null);
        }
        bail!("{path}: null in non-nullable value");
    }
    match (projected_type, value) {
        (ProjectedType::Boolean, OwnedAttributeValue::Bool(value)) => {
            Ok(ProjectedValue::Boolean(*value))
        }
        (ProjectedType::UInt64, OwnedAttributeValue::Unsigned(value)) => {
            Ok(ProjectedValue::UInt64(*value))
        }
        (ProjectedType::Int64, OwnedAttributeValue::Integer(value)) => {
            Ok(ProjectedValue::Int64(*value))
        }
        (ProjectedType::Int64, OwnedAttributeValue::Unsigned(value)) => {
            Ok(ProjectedValue::Int64(i64::try_from(*value).map_err(
                |_| anyhow::anyhow!("{path}: unsigned integer {value} does not fit in Int64"),
            )?))
        }
        (ProjectedType::Float64, OwnedAttributeValue::Float(value)) => {
            Ok(ProjectedValue::Float64(*value))
        }
        (ProjectedType::Float64, OwnedAttributeValue::Unsigned(value)) => {
            Ok(ProjectedValue::Float64(*value as f64))
        }
        (ProjectedType::Float64, OwnedAttributeValue::Integer(value)) => {
            Ok(ProjectedValue::Float64(*value as f64))
        }
        (ProjectedType::Utf8, OwnedAttributeValue::String(value)) => {
            Ok(ProjectedValue::Utf8(value))
        }
        (ProjectedType::GeometryRef, OwnedAttributeValue::Geometry(value)) => {
            Ok(ProjectedValue::GeometryRef(*value))
        }
        (ProjectedType::Json, value) => Ok(ProjectedValue::Json(value)),
        (
            ProjectedType::List {
                item_nullable,
                item,
            },
            OwnedAttributeValue::Vec(values),
        ) => Ok(ProjectedValue::List(ListValueView {
            values,
            item_type: item,
            item_nullable: *item_nullable,
            path,
        })),
        (ProjectedType::Struct(schema), OwnedAttributeValue::Map(values)) => {
            Ok(ProjectedValue::Struct(StructValueView {
                values,
                schema,
                path,
            }))
        }
        (expected, actual) => bail!("{path}: expected {expected:?}, found {actual}"),
    }
}
