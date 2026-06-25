use std::env;
use std::path::PathBuf;

use cityjson_convert::{tabulate_cityobjects, LogicalType, Value};
use cityjson_lib::json;

/// Locates the complete CityJSON 2.0 conformance fixture used by the public
/// table test.
///
/// The corpus checkout is intentionally external to this crate because the
/// fixture is shared across CityJSON implementations. `CITYJSON_CORPUS_DIR` can
/// override the default sibling-checkout location.
fn corpus_fixture() -> PathBuf {
    let corpus_root = env::var_os("CITYJSON_CORPUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("cityjson-corpus")
        });
    let fixture = corpus_root
        .join("cases")
        .join("conformance")
        .join("v2_0")
        .join("cityjson_fake_complete")
        .join("cityjson_fake_complete.city.json");

    assert!(
        fixture.is_file(),
        "cityjson-corpus not found at {}; set CITYJSON_CORPUS_DIR to the corpus checkout containing cases/conformance/v2_0/cityjson_fake_complete/cityjson_fake_complete.city.json",
        corpus_root.display()
    );
    fixture
}

/// Returns the schema index for a named dynamic column used by a test assertion.
fn column_index(table: &cityjson_convert::Table<'_>, name: &str) -> usize {
    table
        .schema()
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}

/// Traverses a value completely so the corpus test exercises lazy
/// lists and structs in addition to top-level scalar cells.
fn validate_value(cell: Value<'_, '_>) {
    match cell {
        Value::List(values) => {
            for value in values.iter() {
                validate_value(value.expect("list item should conform to the inferred schema"));
            }
        }
        Value::Struct(values) => {
            for field in values.fields() {
                let (_, value) = field.expect("struct field should conform to the inferred schema");
                validate_value(value);
            }
        }
        Value::Null
        | Value::Boolean(_)
        | Value::UInt64(_)
        | Value::Int64(_)
        | Value::Float64(_)
        | Value::Utf8(_)
        | Value::GeometryRef(_)
        | Value::Json(_) => {}
    }
}

/// Tabulates the complete CityJSON 2.0 conformance fixture and consumes every
/// row and value.
///
/// Input: `cityjson_fake_complete`, which contains four CityObjects and covers
/// fixed fields, attributes, custom members, nested values, lists, nulls,
/// addresses, and stored geographical extents.
///
/// Assertions: tabulation succeeds; row count, order, and identifiers match the
/// source model; every row has one resolvable value per schema column; all lazy
/// nested values can be traversed; and representative fixed and dynamic values
/// reach the expected row.
///
/// Invariants protected: one row per CityObject in model order, positional
/// schema/value alignment, and end-to-end convertibility of the complete fixture.
#[test]
fn tabulates_cityjson_fake_complete() {
    let model = json::from_file(corpus_fixture()).expect("load cityjson_fake_complete fixture");
    let table = tabulate_cityobjects(&model).expect("tabulate CityObjects");
    let rows = table.rows().collect::<Vec<_>>();

    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter().map(|row| row.cityobject_id).collect::<Vec<_>>(),
        ["id-1", "id-3", "a-tree", "my-neighbourhood"]
    );

    for row in &rows {
        let cells = row.values().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(cells.len(), table.schema().columns.len());
        for cell in cells {
            validate_value(cell);
        }
    }

    let height = column_index(&table, "attributes__measuredHeight");
    assert_eq!(
        rows[0].bbox,
        Some([84710.1, 446846.0, -5.3, 84757.1, 446944.0, 40.9])
    );
    assert!(matches!(
        rows[0].value(height).unwrap().unwrap(),
        Value::Float64(22.3)
    ));
}

/// Establishes the borrowed and lazy API contract with a minimal inline model.
///
/// Input: one Building with a string, a heterogeneous JSON-fallback list, and a
/// homogeneous list of structs.
///
/// Assertions: schema path segments and scalar/JSON cells point at source model
/// storage, while list and struct children are exposed through lazy iterators.
///
/// Invariants protected: table values borrow source data instead of cloning
/// it, and nested typed values can be consumed without owned child collections.
#[test]
fn borrows_source_values_and_traverses_nested_values_lazily() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "attributes":{
                        "name":"Library",
                        "mixed":[1,false],
                        "items":[
                            {"label":"first","count":1},
                            {"label":"second"}
                        ]
                    }
                }
            },
            "vertices":[]
        }"#,
    )
    .expect("parse inline CityJSON");
    let (_, object) = model.cityobjects().first().unwrap();
    let attributes = object.attributes().unwrap();
    let table = tabulate_cityobjects(&model).unwrap();

    let name_ix = column_index(&table, "attributes__name");
    let mixed_ix = column_index(&table, "attributes__mixed");
    let items_ix = column_index(&table, "attributes__items");
    assert!(table
        .schema()
        .columns
        .iter()
        .all(|column| !matches!(column.logical_type, LogicalType::Struct(_))));
    assert!(matches!(
        &table.schema().columns[items_ix].logical_type,
        LogicalType::List {
            item,
            ..
        } if matches!(item.as_ref(), LogicalType::Struct(_))
    ));
    let source_name_key = attributes
        .keys()
        .find(|name| name.as_str() == "name")
        .unwrap();
    assert!(std::ptr::eq(
        table.schema().columns[name_ix].path[0],
        source_name_key.as_str()
    ));

    let row = table.rows().next().unwrap();
    let Value::Utf8(name) = row.value(name_ix).unwrap().unwrap() else {
        panic!("name should be a borrowed UTF-8 value");
    };
    let cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue::String(source_name) =
        attributes.get("name").unwrap()
    else {
        unreachable!();
    };
    assert!(std::ptr::eq(name, source_name.as_str()));

    let Value::Json(mixed) = row.value(mixed_ix).unwrap().unwrap() else {
        panic!("mixed should be a borrowed JSON fallback value");
    };
    assert!(std::ptr::eq(mixed, attributes.get("mixed").unwrap()));

    let Value::List(items) = row.value(items_ix).unwrap().unwrap() else {
        panic!("items should be a lazy list");
    };
    let Value::Struct(first_item) = items.iter().next().unwrap().unwrap() else {
        panic!("list item should be a lazy struct");
    };
    let mut fields = first_item.fields();
    let (count_name, count) = fields.next().unwrap().unwrap();
    let (label_name, label) = fields.next().unwrap().unwrap();
    assert_eq!(count_name, "count");
    assert!(matches!(count, Value::UInt64(1)));
    assert_eq!(label_name, "label");
    assert!(matches!(label, Value::Utf8("first")));
    assert!(fields.next().is_none());
}
