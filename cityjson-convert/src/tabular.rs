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
use cityjson_lib::cityjson_types::resources::handles::GeometryHandle;
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::{CityObjectType, OwnedAttributeValue, OwnedAttributes};
use cityjson_lib::CityModel;

/// Borrowed CityObject table with one inferred schema.
///
/// The table owns schema containers and physical column names, but borrows all
/// source field names and values from the model. It does not store rows.
#[derive(Debug)]
pub struct Table<'model> {
    model: &'model CityModel,
    schema: TableSchema<'model>,
}

impl<'model> Table<'model> {
    /// Returns the shared flattened schema used by every row.
    #[must_use]
    pub fn schema(&self) -> &TableSchema<'model> {
        &self.schema
    }

    /// Iterates over CityObjects in model order without allocating rows.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_, 'model>> {
        self.model
            .cityobjects()
            .iter()
            .enumerate()
            .map(|(cityobject_ix, (_, object))| Row {
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
}

impl ColumnOrigin {
    /// Returns the physical source prefix used in flattened column names.
    fn as_str(self) -> &'static str {
        match self {
            Self::Attributes => "attributes",
            Self::Extra => "extra",
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
#[derive(Clone, Copy, Debug)]
pub struct Row<'table, 'model> {
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

impl<'table, 'model> Row<'table, 'model> {
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
        self.schema
            .columns
            .get(index)
            .map(|column| self.value_for_column(column))
    }

    /// Resolves values lazily in shared schema order.
    pub fn values(&self) -> impl Iterator<Item = Result<Value<'_, 'model>>> {
        self.schema
            .columns
            .iter()
            .map(|column| self.value_for_column(column))
    }

    /// Resolves one dynamic column against this row's matching source map.
    fn value_for_column<'row>(
        &'row self,
        column: &'row ColumnSchema<'model>,
    ) -> Result<Value<'row, 'model>> {
        let attributes = match column.origin {
            ColumnOrigin::Attributes => self.attributes,
            ColumnOrigin::Extra => self.extra,
        };
        let value = resolve_path(attributes, &column.path, &column.name)?;
        build_value(value, &column.logical_type, column.nullable, &column.name)
    }
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
pub fn tabulate_cityobjects(model: &CityModel) -> Result<Table<'_>> {
    let mut attributes = StructSchema::default();
    let mut extra = StructSchema::default();
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
    Ok(Table {
        model,
        schema: build_table_schema(attributes, extra),
    })
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

/// Flattens attribute and extra trees into one shared table schema.
fn build_table_schema<'model>(
    attributes: StructSchema<'model>,
    extra: StructSchema<'model>,
) -> TableSchema<'model> {
    let mut columns = Vec::new();
    let mut path = Vec::new();
    let mut name_buffer = String::new();
    flatten_struct_schema(
        ColumnOrigin::Attributes,
        attributes,
        &mut path,
        false,
        &mut name_buffer,
        &mut columns,
    );
    flatten_struct_schema(
        ColumnOrigin::Extra,
        extra,
        &mut path,
        false,
        &mut name_buffer,
        &mut columns,
    );
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
) {
    for field in schema.fields.into_values() {
        path.push(field.name);
        let nullable = inherited_nullable || field.nullable;
        match field.logical_type {
            LogicalType::Null => {}
            LogicalType::Struct(schema) => {
                flatten_struct_schema(origin, schema, path, nullable, name_buffer, columns);
            }
            logical_type => {
                build_column_name(name_buffer, origin, path);
                debug_assert!(!matches!(logical_type, LogicalType::Struct(_)));
                columns.push(ColumnSchema {
                    origin,
                    path: path.clone(),
                    name: unique_column_name(name_buffer, columns),
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
    buffer.push_str(origin.as_str());
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
