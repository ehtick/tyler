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

use anyhow::{bail, Result};
use cityjson_lib::cityjson_types::resources::handles::GeometryHandle;
use cityjson_lib::cityjson_types::v2_0::{OwnedAttributeValue, OwnedAttributes};
use cityjson_lib::CityModel;

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

/// Borrowed source data collected from a `CityModel` before table materialization.
struct CityObjectSourceRow<'a> {
    /// CityJSON object identifier.
    cityobject_id: &'a str,
    /// Zero-based ordinal in model CityObject order.
    cityobject_ix: u64,
    /// CityJSON CityObject type name.
    cityobject_type: String,
    /// Source `geographicalExtent`, when present.
    bbox: Option<[f64; 6]>,
    /// Borrowed CityObject `attributes`, if present.
    attributes: Option<&'a OwnedAttributes>,
    /// Borrowed CityObject `extra`, if present.
    extra: Option<&'a OwnedAttributes>,
}

/// Builds the shared CityObject table for a model.
///
/// # Errors
///
/// Returns an error when values cannot be represented by the inferred schema.
pub fn build_cityobject_table(model: &CityModel) -> Result<CityObjectTable> {
    // 1. Walk CityObjects in model order and collect borrowed source rows.
    // 2. Infer nested schemas independently for `attributes` and `extra`.
    // 3. Flatten both schemas into one stable output column list.
    // 4. Materialize each CityObject row against the shared schema.
    // 5. Return the assembled table or a path-bearing error on invalid data.
    let _ = model;
    todo!("implement CityObject table construction")
}

fn infer_cityobject_rows<'a>(model: &'a CityModel) -> Result<Vec<CityObjectSourceRow<'a>>> {
    // Walk the model's CityObjects in iteration order.
    // Borrow each object's identifier, type, bbox, attributes, and extra fields.
    // Keep the source data borrowed here so schema inference can happen before cloning.
    let _ = model;
    todo!("collect source rows from the model")
}

fn infer_attribute_schema_from_model<'a>(
    rows: &[CityObjectSourceRow<'a>],
    namespace: ColumnNamespace,
) -> Result<StructSpec> {
    // Filter the borrowed source rows down to the requested namespace.
    // Treat missing maps as participating rows so nullable columns are inferred correctly.
    // Reuse the existing nested schema inference helpers for the actual merge logic.
    let _ = (rows, namespace);
    todo!("infer the nested schema for one namespace")
}

fn build_column_schema(attributes: &StructSpec, extra: &StructSpec) -> Result<TableSchema> {
    // Flatten `attributes` first, then `extra`.
    // Seed name conflict resolution with reserved fixed columns before flattening dynamic fields.
    // Combine both flattened schemas into the final ordered tabular schema.
    let _ = (attributes, extra);
    todo!("flatten nested schemas into one ordered column list")
}

fn build_cityobject_row<'a>(
    source: &CityObjectSourceRow<'a>,
    schema: &TableSchema,
) -> Result<CityObjectRow> {
    // Copy the fixed row fields directly from the borrowed source row.
    // Then resolve each schema column from the relevant namespace and path in order.
    // Build one cell per column, preserving nullability and path-bearing errors.
    let _ = (source, schema);
    todo!("materialize one tabular row from one CityObject")
}

fn infer_data_type(value: &OwnedAttributeValue) -> Result<DataType> {
    Ok(match value {
        OwnedAttributeValue::Null => DataType::Null,
        OwnedAttributeValue::Bool(_) => DataType::Boolean,
        OwnedAttributeValue::Unsigned(_) => DataType::UInt64,
        OwnedAttributeValue::Integer(_) => DataType::Int64,
        OwnedAttributeValue::Float(_) => DataType::Float64,
        OwnedAttributeValue::String(_) => DataType::Utf8,
        OwnedAttributeValue::Geometry(_) => DataType::GeometryRef,
        OwnedAttributeValue::Vec(values) => {
            let mut item_nullable = false;
            let mut item_type = DataType::Null;

            for item in values {
                if matches!(item, OwnedAttributeValue::Null) {
                    item_nullable = true;
                    continue;
                }
                item_type = merge_data_types(item_type, infer_data_type(item)?)?;
            }

            if matches!(item_type, DataType::Json) {
                DataType::Json
            } else {
                DataType::List {
                    item_nullable,
                    item: Box::new(item_type),
                }
            }
        }
        OwnedAttributeValue::Map(values) => {
            let mut fields = values
                .iter()
                .map(|(name, value)| {
                    Ok(cityjson_arrow::schema::ProjectedFieldSpec::new(
                        name.clone(),
                        infer_data_type(value)?,
                        matches!(value, OwnedAttributeValue::Null),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            fields.sort_by(|left, right| left.name.cmp(&right.name));
            DataType::Struct(StructSpec::new(fields))
        }
        unsupported => bail!("unsupported attribute value variant {unsupported}"),
    })
}

fn merge_data_types(left: DataType, right: DataType) -> Result<DataType> {
    Ok(match (left, right) {
        (DataType::Null, other) | (other, DataType::Null) => other,
        (DataType::Boolean, DataType::Boolean) => DataType::Boolean,
        (DataType::UInt64, DataType::UInt64) => DataType::UInt64,
        (DataType::Int64 | DataType::UInt64, DataType::Int64)
        | (DataType::Int64, DataType::UInt64) => DataType::Int64,
        (DataType::Float64 | DataType::UInt64 | DataType::Int64, DataType::Float64)
        | (DataType::Float64, DataType::UInt64 | DataType::Int64) => DataType::Float64,
        (DataType::Utf8, DataType::Utf8) => DataType::Utf8,
        (DataType::GeometryRef, DataType::GeometryRef) => DataType::GeometryRef,
        (DataType::Json, _) | (_, DataType::Json) => DataType::Json,
        (
            DataType::List {
                item_nullable: left_nullable,
                item: left_item,
            },
            DataType::List {
                item_nullable: right_nullable,
                item: right_item,
            },
        ) => {
            let item = merge_data_types(*left_item, *right_item)?;
            if matches!(item, DataType::Json) {
                DataType::Json
            } else {
                DataType::List {
                    item_nullable: left_nullable || right_nullable,
                    item: Box::new(item),
                }
            }
        }
        (DataType::Struct(left), DataType::Struct(right)) => {
            DataType::Struct(merge_struct_data_types(left, right)?)
        }
        _ => DataType::Json,
    })
}

fn merge_struct_data_types(left: StructSpec, right: StructSpec) -> Result<StructSpec> {
    let mut fields = left
        .fields
        .into_iter()
        .map(|field| (field.name.clone(), field))
        .collect::<BTreeMap<_, _>>();
    let right_names = right
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<HashSet<_>>();

    for (name, field) in &mut fields {
        if !right_names.contains(name.as_str()) {
            field.nullable = true;
        }
    }

    for mut incoming in right.fields {
        if let Some(existing) = fields.get_mut(&incoming.name) {
            existing.nullable |= incoming.nullable;
            existing.value = merge_data_types(existing.value.clone(), incoming.value)?;
        } else {
            incoming.nullable = true;
            fields.insert(incoming.name.clone(), incoming);
        }
    }

    Ok(StructSpec::new(fields.into_values().collect()))
}

/// Infers the shared struct schema for a sequence of attribute rows.
///
/// Missing rows make already-seen fields nullable, and null values make the
/// corresponding field nullable at the field level.
fn infer_attribute_schema(rows: &[Option<&OwnedAttributes>]) -> Result<StructSpec> {
    let mut schema: Option<StructSpec> = None;
    let mut saw_missing_attributes = false;

    for attributes in rows {
        let Some(attributes) = attributes else {
            saw_missing_attributes = true;
            if let Some(schema) = &mut schema {
                for field in &mut schema.fields {
                    field.nullable = true;
                }
            }
            continue;
        };

        let mut fields = attributes
            .iter()
            .map(|(name, value)| {
                Ok(FieldSpec::new(
                    name.clone(),
                    infer_data_type(value)?,
                    matches!(value, OwnedAttributeValue::Null)
                        || (schema.is_none() && saw_missing_attributes),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        fields.sort_by(|left, right| left.name.cmp(&right.name));
        let row_schema = StructSpec::new(fields);

        schema = Some(match schema {
            Some(schema) => merge_struct_data_types(schema, row_schema)?,
            None => row_schema,
        });
    }

    Ok(schema.unwrap_or_else(|| StructSpec::new(Vec::new())))
}

fn escape_path_segment(_segment: &str) -> String {
    _segment.replace('%', "%25").replace("__", "%5F%5F")
}

#[derive(Default)]
struct ColumnNamer {
    used: HashSet<String>,
}

impl ColumnNamer {
    fn unique(&mut self, base: &str) -> String {
        let mut candidate = base.to_string();
        let mut suffix = 2;

        while !self.used.insert(candidate.clone()) {
            candidate = format!("{base}__{suffix}");
            suffix += 1;
        }

        candidate
    }
}

fn flatten_struct_schema(namespace: ColumnNamespace, spec: &StructSpec) -> Vec<Column> {
    fn namespace_name(namespace: ColumnNamespace) -> &'static str {
        match namespace {
            ColumnNamespace::Attributes => "attributes",
            ColumnNamespace::Extra => "extra",
        }
    }

    fn visit(
        namespace: ColumnNamespace,
        fields: &[FieldSpec],
        path: &mut Vec<String>,
        inherited_nullable: bool,
        namer: &mut ColumnNamer,
        columns: &mut Vec<Column>,
    ) {
        let mut ordered_fields: Vec<&FieldSpec> = fields.iter().collect();
        ordered_fields.sort_by(|left, right| left.name.cmp(&right.name));

        for field in ordered_fields {
            path.push(field.name.clone());
            let nullable = inherited_nullable || field.nullable;

            match &field.value {
                DataType::Null => {}
                DataType::Struct(struct_spec) => {
                    visit(
                        namespace,
                        &struct_spec.fields,
                        path,
                        nullable,
                        namer,
                        columns,
                    );
                }
                data_type => {
                    let mut name_parts = Vec::with_capacity(path.len() + 1);
                    name_parts.push(namespace_name(namespace).to_string());
                    name_parts.extend(path.iter().map(|segment| escape_path_segment(segment)));

                    columns.push(Column {
                        namespace,
                        path: path.clone(),
                        name: namer.unique(&name_parts.join("__")),
                        data_type: data_type.clone(),
                        nullable,
                    });
                }
            }

            path.pop();
        }
    }

    let mut columns = Vec::new();
    let mut path = Vec::new();
    let mut namer = ColumnNamer::default();

    visit(
        namespace,
        &spec.fields,
        &mut path,
        false,
        &mut namer,
        &mut columns,
    );

    columns
}

fn build_cell(
    value: Option<&OwnedAttributeValue>,
    data_type: &DataType,
    nullable: bool,
    path: &str,
) -> Result<Cell> {
    // Convert a borrowed CityObject value into the schema's logical cell type.
    // Use checked numeric widening and recursive list/struct conversion.
    // Preserve `Json` fallback values by cloning the original owned attribute value.
    // Return a path-bearing error when the value is incompatible or non-nullable null.
    let _ = (value, data_type, nullable, path);
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

    /// Verifies value type inference and type merging rules.
    ///
    /// Assertions cover numeric widening, list item nullability and
    /// heterogeneous fallback to `Json`.
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
    }

    /// Verifies attribute schema inference across multiple rows.
    ///
    /// Assertions cover recursive struct merging, deterministic field order,
    /// and nullable fields omitted by some rows.
    #[test]
    fn infers_attribute_schema() {
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
        let actual = infer_attribute_schema(&[Some(&first), Some(&second), None]).unwrap();
        let expected = StructSpec::new(vec![FieldSpec::new(
            "metrics",
            DataType::Struct(StructSpec::new(vec![
                FieldSpec::new("height", DataType::Float64, false),
                FieldSpec::new("slope", DataType::Float64, true),
            ])),
            true,
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
