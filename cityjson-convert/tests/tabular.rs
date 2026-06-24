use std::env;
use std::path::PathBuf;

use cityjson_convert::{build_cityobject_table, Cell, CityObjectTable, DataType};
use cityjson_lib::json;

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

fn column_index(table: &CityObjectTable, name: &str) -> usize {
    table
        .schema
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}

/// Exercises the public CityObject-to-table boundary with the corpus's complete
/// CityJSON 2.0 fixture.
///
/// The row count and identifiers assert the one-row-per-CityObject invariant
/// and preservation of model order. The selected columns sample the three
/// important schema shapes present in the fixture: a typed scalar, a nested
/// list value, and a homogeneous string list. The row assertions verify that a
/// source bounding box and representative attribute values reach the correct
/// cells, while a CityObject without attributes receives null cells.
///
/// This intentionally avoids asserting every field in the fixture. Detailed
/// widening, naming, nullability, and invalid-value rules are covered by the
/// table-driven unit tests; this test protects their integration through the
/// single public [`build_cityobject_table`] operation.
#[test]
fn builds_table_from_cityjson_fake_complete() {
    let fixture = corpus_fixture();
    let model = json::from_file(&fixture).expect("load cityjson_fake_complete fixture");
    let table = build_cityobject_table(&model).expect("build CityObject table");

    assert_eq!(table.rows.len(), 4);
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| row.cityobject_id.as_str())
            .collect::<Vec<_>>(),
        ["id-1", "id-3", "a-tree", "my-neighbourhood"]
    );

    let height = column_index(&table, "attributes__measuredHeight");
    let address = column_index(&table, "extra__address");
    let roles = column_index(&table, "extra__children_roles");
    assert_eq!(table.schema.columns[height].data_type, DataType::Float64);
    assert!(matches!(
        table.schema.columns[address].data_type,
        DataType::List { .. }
    ));
    assert_eq!(
        table.schema.columns[roles].data_type,
        DataType::List {
            item_nullable: false,
            item: Box::new(DataType::Utf8),
        }
    );

    assert_eq!(
        table.rows[0].bbox,
        Some([84710.1, 446846.0, -5.3, 84757.1, 446944.0, 40.9])
    );
    assert_eq!(table.rows[0].cells[height], Cell::Float64(22.3));
    assert!(matches!(table.rows[0].cells[address], Cell::List(_)));
    assert!(table.rows[2].cells.iter().all(|cell| *cell == Cell::Null));
}
