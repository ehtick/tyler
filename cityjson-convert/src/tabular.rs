//! Shared tabular representation of CityJSON CityObjects.
//!
//! This module converts a [`CityModel`] into a format-neutral table for flat
//! writers such as CSV, TSV, and GeoPackage. It defines the logical table only;
//! delimiters, SQLite types, and JSON text encoding belong to the writers.
//!
//! # Table layout
//!
//! Each CityObject becomes one [`CityObjectRow`] in model order. Fixed row
//! fields store its identifier, zero-based ordinal, type, source
//! `geographicalExtent`, and dynamic [`Cell`] values. Cell index `n` always
//! corresponds to [`TableSchema::columns`] index `n`.
//!
//! Dynamic columns are inferred from all CityObjects before rows are built.
//! `attributes` and custom `extra` members use separate [`ColumnNamespace`]s.
//! A field is nullable when explicitly null or absent from any row. A missing
//! map therefore contributes null values for every column in that namespace.
//!
//! # Data types
//!
//! Scalars retain logical types. Unsigned/signed integer combinations widen to
//! signed integers; integer/float combinations widen to floats. Homogeneous
//! lists retain typed items and item nullability. Consistently map-valued fields
//! become [`DataType::Struct`]. Fields with mixed shapes and heterogeneous lists
//! fall back to [`DataType::Json`].
//!
//! Cells conform to their inferred type. Missing and explicit-null nullable
//! values become [`Cell::Null`]. Lists and structs remain typed. JSON fallback
//! values retain their original [`OwnedAttributeValue`] until a writer encodes
//! them as text. Address locations remain opaque geometry references.
//!
//! # Column flattening
//!
//! Nested structs are flattened recursively. Lists and JSON values remain leaf
//! columns. Names start with the namespace and join path segments with `__`,
//! for example `attributes__metrics__height`. Before joining, `%` becomes
//! `%25` and literal `__` becomes `%5F%5F`. Conflicts receive deterministic
//! `__2`, `__3`, and later suffixes. Null-only fields do not produce flat
//! columns. Attribute columns precede extra columns, and fields are sorted
//! lexicographically at each level.
//!
//! # Invariants
//!
//! [`build_cityobject_table`] guarantees one row per CityObject, one cell per
//! schema column, deterministic column order and names, and either lossless
//! typed conversion or an error instead of silent incompatible coercion.

use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use cityjson_lib::cityjson_types::resources::handles::GeometryHandle;
use cityjson_lib::cityjson_types::v2_0::{OwnedAttributeValue, OwnedAttributes};
use cityjson_lib::CityModel;

#[cfg(test)]
use cityjson_arrow::schema::ProjectedFieldSpec as FieldSpec;
use cityjson_arrow::schema::ProjectedStructSpec as StructSpec;
/// Logical data type inferred for a tabular column or nested value.
pub use cityjson_arrow::schema::ProjectedValueSpec as DataType;

/// Source namespace of a dynamic CityObject column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnNamespace {
    /// Column inferred from the CityObject `attributes` member.
    Attributes,
    /// Column inferred from custom CityObject members.
    Extra,
}

/// Definition of one flattened dynamic table column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// CityObject member from which the column originates.
    pub namespace: ColumnNamespace,
    /// Unescaped nested source path below the namespace.
    pub path: Vec<String>,
    /// Escaped, unique physical column name.
    pub name: String,
    /// Logical type shared by every non-null cell in the column.
    pub data_type: DataType,
    /// Whether the column accepts missing or explicit-null values.
    pub nullable: bool,
}

/// Ordered dynamic-column schema shared by every row in a table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableSchema {
    /// Columns in the same positional order as each row's cells.
    pub columns: Vec<Column>,
}

/// Typed value stored at one row and dynamic-column intersection.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// Missing or explicit-null value.
    Null,
    /// Boolean value.
    Boolean(bool),
    /// Unsigned integer value.
    UInt64(u64),
    /// Signed integer value.
    Int64(i64),
    /// Floating-point value.
    Float64(f64),
    /// UTF-8 string value.
    Utf8(String),
    /// Reference to geometry owned by the source model.
    GeometryRef(GeometryHandle),
    /// Ordered values conforming to one list item type.
    List(Vec<Cell>),
    /// Named values conforming to a nested struct schema.
    Struct(BTreeMap<String, Cell>),
    /// Original value retained after heterogeneous type fallback.
    Json(OwnedAttributeValue),
}

/// One tabular row derived from a CityJSON CityObject.
#[derive(Clone, Debug, PartialEq)]
pub struct CityObjectRow {
    /// CityJSON object identifier.
    pub cityobject_id: String,
    /// Zero-based ordinal in model CityObject order.
    pub cityobject_ix: u64,
    /// CityJSON CityObject type name.
    pub cityobject_type: String,
    /// Source `geographicalExtent`, when present.
    pub bbox: Option<[f64; 6]>,
    /// Values aligned positionally with `TableSchema::columns`.
    pub cells: Vec<Cell>,
}

/// Format-neutral tabular representation of all CityObjects in a model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CityObjectTable {
    /// Dynamic schema shared by all rows.
    pub schema: TableSchema,
    /// Rows in model CityObject order.
    pub rows: Vec<CityObjectRow>,
}

/// Builds the shared CityObject table for a model.
///
/// # Errors
///
/// Returns an error when values cannot be represented by the inferred schema.
pub fn build_cityobject_table(_model: &CityModel) -> Result<CityObjectTable> {
    todo!("implement CityObject table construction")
}

fn infer_data_type(_value: &OwnedAttributeValue) -> Result<DataType> {
    todo!("implement tabular data type inference")
}

fn merge_data_types(_left: DataType, _right: DataType) -> Result<DataType> {
    todo!("implement tabular data type merging")
}

fn infer_attribute_schema(_rows: &[Option<&OwnedAttributes>]) -> Result<StructSpec> {
    todo!("implement attribute schema inference")
}

fn escape_path_segment(_segment: &str) -> String {
    todo!("implement path escaping")
}

#[derive(Default)]
struct ColumnNamer {
    used: HashSet<String>,
}

impl ColumnNamer {
    fn unique(&mut self, _base: &str) -> String {
        todo!("implement column-name conflict resolution")
    }
}

fn flatten_struct_schema(_namespace: ColumnNamespace, _spec: &StructSpec) -> Vec<Column> {
    todo!("implement nested schema flattening")
}

fn build_cell(
    _value: Option<&OwnedAttributeValue>,
    _data_type: &DataType,
    _nullable: bool,
    _path: &str,
) -> Result<Cell> {
    todo!("implement typed cell construction")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue as Value;

    fn list(item_nullable: bool, item: DataType) -> DataType {
        DataType::List {
            item_nullable,
            item: Box::new(item),
        }
    }

    fn map(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<HashMap<_, _>>(),
        )
    }

    /// Verifies type inference rules not exercised by the corpus acceptance
    /// test.
    ///
    /// Assertions cover numeric widening, list item nullability,
    /// heterogeneous fallback to `Json`, recursive struct merging,
    /// deterministic field order, and nullable fields omitted by some rows.
    #[test]
    fn infers_and_merges_data_types() {
        let inference_cases = vec![
            (
                "mixed numeric list",
                Value::Vec(vec![Value::Unsigned(1), Value::Float(2.5)]),
                list(false, DataType::Float64),
            ),
            (
                "nullable string list",
                Value::Vec(vec![Value::Null, Value::String("value".to_string())]),
                list(true, DataType::Utf8),
            ),
            (
                "heterogeneous list",
                Value::Vec(vec![Value::Unsigned(1), Value::Bool(false)]),
                DataType::Json,
            ),
        ];

        for (description, input, expected) in inference_cases {
            assert_eq!(infer_data_type(&input).unwrap(), expected, "{description}");
        }

        let merge_cases = vec![
            (
                "unsigned and signed",
                DataType::UInt64,
                DataType::Int64,
                DataType::Int64,
            ),
            (
                "unsigned and float",
                DataType::UInt64,
                DataType::Float64,
                DataType::Float64,
            ),
            (
                "signed and float",
                DataType::Int64,
                DataType::Float64,
                DataType::Float64,
            ),
            (
                "null and string",
                DataType::Null,
                DataType::Utf8,
                DataType::Utf8,
            ),
            (
                "boolean and string",
                DataType::Boolean,
                DataType::Utf8,
                DataType::Json,
            ),
            (
                "incompatible lists",
                list(false, DataType::UInt64),
                list(false, DataType::Utf8),
                DataType::Json,
            ),
        ];

        for (description, left, right, expected) in merge_cases {
            assert_eq!(
                merge_data_types(left, right).unwrap(),
                expected,
                "{description}"
            );
        }

        let first = OwnedAttributes::from(HashMap::from([(
            "metrics".to_string(),
            map([
                ("height", Value::Unsigned(12)),
                ("slope", Value::Float(0.25)),
            ]),
        )]));
        let second = OwnedAttributes::from(HashMap::from([(
            "metrics".to_string(),
            map([("height", Value::Float(14.5))]),
        )]));
        let actual = infer_attribute_schema(&[Some(&first), Some(&second)]).unwrap();
        let expected = StructSpec::new(vec![FieldSpec::new(
            "metrics",
            DataType::Struct(StructSpec::new(vec![
                FieldSpec::new("height", DataType::Float64, false),
                FieldSpec::new("slope", DataType::Float64, true),
            ])),
            false,
        )]);
        assert_eq!(actual, expected);
    }

    /// Verifies stable and reversible mapping from logical paths to columns.
    ///
    /// Assertions cover escaping, deterministic conflict suffixes, recursive
    /// struct flattening, preservation of source paths, omission of null-only
    /// fields, and retention of lists as single leaf columns.
    #[test]
    fn generates_column_names() {
        let escape_cases = [
            ("height", "height"),
            ("a__b", "a%5F%5Fb"),
            ("a%b", "a%25b"),
            ("%__", "%25%5F%5F"),
        ];
        for (input, expected) in escape_cases {
            assert_eq!(escape_path_segment(input), expected);
        }

        let mut namer = ColumnNamer::default();
        assert_eq!(namer.unique("attributes__name"), "attributes__name");
        assert_eq!(namer.unique("attributes__name"), "attributes__name__2");
        assert_eq!(namer.unique("attributes__name"), "attributes__name__3");

        let spec = StructSpec::new(vec![
            FieldSpec::new("always_null", DataType::Null, true),
            FieldSpec::new(
                "metrics__v2",
                DataType::Struct(StructSpec::new(vec![FieldSpec::new(
                    "height%",
                    DataType::Float64,
                    false,
                )])),
                false,
            ),
            FieldSpec::new("tags", list(false, DataType::Utf8), true),
        ]);

        assert_eq!(
            flatten_struct_schema(ColumnNamespace::Attributes, &spec),
            vec![
                Column {
                    namespace: ColumnNamespace::Attributes,
                    path: vec!["metrics__v2".to_string(), "height%".to_string()],
                    name: "attributes__metrics%5F%5Fv2__height%25".to_string(),
                    data_type: DataType::Float64,
                    nullable: false,
                },
                Column {
                    namespace: ColumnNamespace::Attributes,
                    path: vec!["tags".to_string()],
                    name: "attributes__tags".to_string(),
                    data_type: list(false, DataType::Utf8),
                    nullable: true,
                },
            ]
        );
    }

    /// Verifies that values become cells conforming to their column types.
    ///
    /// Successful assertions cover numeric widening, null normalization, and
    /// lossless JSON fallback. Failure assertions require incompatible values,
    /// numeric overflow, and invalid nulls to return path-bearing errors rather
    /// than being silently coerced.
    #[test]
    fn builds_typed_cells_and_rejects_invalid_values() {
        let success_cases = vec![
            (
                "unsigned to signed",
                Some(Value::Unsigned(7)),
                DataType::Int64,
                false,
                Cell::Int64(7),
            ),
            (
                "unsigned to float",
                Some(Value::Unsigned(7)),
                DataType::Float64,
                false,
                Cell::Float64(7.0),
            ),
            (
                "signed to float",
                Some(Value::Integer(-7)),
                DataType::Float64,
                false,
                Cell::Float64(-7.0),
            ),
            (
                "missing nullable value",
                None,
                DataType::Utf8,
                true,
                Cell::Null,
            ),
            (
                "explicit nullable value",
                Some(Value::Null),
                DataType::Utf8,
                true,
                Cell::Null,
            ),
            (
                "JSON fallback",
                Some(Value::Vec(vec![
                    Value::Unsigned(607),
                    Value::Bool(false),
                    Value::Float(28.47),
                ])),
                DataType::Json,
                true,
                Cell::Json(Value::Vec(vec![
                    Value::Unsigned(607),
                    Value::Bool(false),
                    Value::Float(28.47),
                ])),
            ),
        ];

        for (description, value, data_type, nullable, expected) in success_cases {
            assert_eq!(
                build_cell(value.as_ref(), &data_type, nullable, "attributes.test").unwrap(),
                expected,
                "{description}"
            );
        }

        let failure_cases = vec![
            (
                "wrong type",
                Some(Value::String("not-a-boolean".to_string())),
                DataType::Boolean,
                false,
            ),
            (
                "negative unsigned",
                Some(Value::Integer(-1)),
                DataType::UInt64,
                false,
            ),
            (
                "unsigned overflow",
                Some(Value::Unsigned(u64::MAX)),
                DataType::Int64,
                false,
            ),
            (
                "non-nullable null",
                Some(Value::Null),
                DataType::Utf8,
                false,
            ),
        ];

        for (description, value, data_type, nullable) in failure_cases {
            let error = build_cell(value.as_ref(), &data_type, nullable, "attributes.test")
                .expect_err(description);
            assert!(
                error.to_string().contains("attributes.test"),
                "{description}: {error}"
            );
        }
    }
}
