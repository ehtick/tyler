use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use cityjson_lib::CityModel;
use csv::Terminator;

use crate::{
    tabular::{geometry_ref_to_multipoint_wkb_hex, value_to_text_cell, TextCell},
    tabulate_addresses, tabulate_cityobjects, tabulate_model_metadata,
    tabulate_semantic_assignments, tabulate_semantics, AddressTable, CityObjectTable, IdList,
    MetadataRow, MetadataTable, SemanticAssignmentRow, SemanticAssignmentTable, SemanticTable,
    Value,
};

#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct TsvExportOptions {
    pub include_null_rows: bool,
    pub include_hierarchy: bool,
    pub include_cityjson_ordinal: bool,
    pub include_metadata: bool,
    pub split_semantics: bool,
    pub split_address: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TsvWriteOptions {
    pub include_null_rows: bool,
    pub include_hierarchy: bool,
    pub include_cityjson_ordinal: bool,
}

/// Converts a `CityJSON` model to TSV files in an output directory.
///
/// # Errors
///
/// Returns an error when output files cannot be created or tabular values
/// cannot be resolved or serialized.
pub fn convert_to_tsv<P: AsRef<Path>>(
    model: &CityModel,
    output_dir: P,
    options: &TsvExportOptions,
) -> Result<()> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;

    let write_options = TsvWriteOptions {
        include_null_rows: options.include_null_rows,
        include_hierarchy: options.include_hierarchy,
        include_cityjson_ordinal: options.include_cityjson_ordinal,
    };

    let cityobjects = tabulate_cityobjects(model)?;
    let mut file = File::create(output_dir.join("cityobjects.tsv"))?;
    write_cityobjects_tsv(&cityobjects, &write_options, &mut file)?;

    if options.include_metadata {
        let metadata = tabulate_model_metadata(model)?;
        let mut file = File::create(output_dir.join("metadata.tsv"))?;
        write_metadata_tsv(&metadata, &mut file)?;
    }

    if options.split_address {
        let addresses = tabulate_addresses(model)?;
        let mut file = File::create(output_dir.join("addresses.tsv"))?;
        write_addresses_tsv(&addresses, &write_options, &mut file)?;
    }

    if options.split_semantics {
        let semantics = tabulate_semantics(model)?;
        let assignments = tabulate_semantic_assignments(model)?;
        let mut file = File::create(output_dir.join("semantics.tsv"))?;
        write_split_semantics_tsv(&semantics, &assignments, &write_options, &mut file)?;
    }

    Ok(())
}

/// Writes `CityObject` rows as TSV.
///
/// # Errors
///
/// Returns an error when writing fails or a row value cannot be resolved.
pub fn write_cityobjects_tsv<W: Write>(
    table: &CityObjectTable<'_>,
    options: &TsvWriteOptions,
    writer: W,
) -> Result<()> {
    let mut tsv = tsv_writer(writer);
    let mut header = vec!["cityobject_id".to_string(), "cityobject_type".to_string()];
    if options.include_cityjson_ordinal {
        header.push("cityobject_ix".to_string());
    }
    if options.include_hierarchy {
        header.extend(["parents".to_string(), "children".to_string()]);
    }
    header.extend(
        table
            .schema()
            .columns
            .iter()
            .map(|column| column.name.clone()),
    );
    tsv.write_record(header)?;

    for row in table.rows() {
        let dynamic = dynamic_cells(table.model(), row.values()).with_context(|| {
            format!(
                "resolve dynamic values for CityObject {}",
                row.cityobject_id
            )
        })?;
        if !options.include_null_rows && all_null(&dynamic) {
            continue;
        }

        let mut record = vec![
            row.cityobject_id.to_string(),
            row.cityobject_type_name().to_string(),
        ];
        if options.include_cityjson_ordinal {
            record.push(row.cityobject_ix.to_string());
        }
        if options.include_hierarchy {
            record.push(id_list_cell(&row.parents()?)?);
            record.push(id_list_cell(&row.children()?)?);
        }
        record.extend(dynamic.into_iter().map(|cell| cell.text));
        tsv.write_record(record)?;
    }

    tsv.flush()?;
    Ok(())
}

/// Writes address rows as TSV.
///
/// # Errors
///
/// Returns an error when writing fails or address values cannot be resolved.
pub fn write_addresses_tsv<W: Write>(
    table: &AddressTable<'_>,
    options: &TsvWriteOptions,
    writer: W,
) -> Result<()> {
    let mut tsv = tsv_writer(writer);
    let mut header = vec!["cityobject_id".to_string(), "cityobject_type".to_string()];
    if options.include_cityjson_ordinal {
        header.push("cityobject_ix".to_string());
    }
    header.push("location_wkb".to_string());
    header.extend(
        table
            .schema()
            .columns
            .iter()
            .map(|column| column.name.clone()),
    );
    tsv.write_record(header)?;

    for row in table.rows() {
        let fixed = row.fixed();
        let mut record = vec![
            fixed.cityobject_id.to_string(),
            fixed.cityobject_type_name().to_string(),
        ];
        if options.include_cityjson_ordinal {
            record.push(fixed.cityobject_ix.to_string());
        }
        let location = fixed
            .location()?
            .map(|handle| geometry_ref_to_multipoint_wkb_hex(table.model(), handle))
            .transpose()?
            .unwrap_or_default();
        record.push(location);
        record.extend(
            dynamic_cells(table.model(), row.values())?
                .into_iter()
                .map(|cell| cell.text),
        );
        tsv.write_record(record)?;
    }

    tsv.flush()?;
    Ok(())
}

/// Writes metadata rows as TSV.
///
/// # Errors
///
/// Returns an error when writing fails or a row value cannot be resolved.
pub fn write_metadata_tsv<W: Write>(table: &MetadataTable<'_>, writer: W) -> Result<()> {
    let mut tsv = tsv_writer(writer);
    let mut header = metadata_fixed_header();
    header.extend(
        table
            .schema()
            .columns
            .iter()
            .map(|column| column.name.clone()),
    );
    tsv.write_record(header)?;

    for row in table.rows() {
        let mut record = metadata_fixed_cells(row.fixed())?;
        record.extend(
            dynamic_cells(table.model(), row.values())?
                .into_iter()
                .map(|cell| cell.text),
        );
        tsv.write_record(record)?;
    }

    tsv.flush()?;
    Ok(())
}

/// Writes semantic definitions as TSV.
///
/// # Errors
///
/// Returns an error when writing fails or a row value cannot be resolved.
pub fn write_semantic_definitions_tsv<W: Write>(
    table: &SemanticTable<'_>,
    options: &TsvWriteOptions,
    writer: W,
) -> Result<()> {
    let mut tsv = tsv_writer(writer);
    let mut header = vec!["semantic_id".to_string(), "semantic_type".to_string()];
    if options.include_hierarchy {
        header.extend(["parent".to_string(), "children".to_string()]);
    }
    header.extend(
        table
            .schema()
            .columns
            .iter()
            .map(|column| column.name.clone()),
    );
    tsv.write_record(header)?;

    for row in table.rows() {
        let dynamic = dynamic_cells(table.model(), row.values())?;
        if !options.include_null_rows && all_null(&dynamic) {
            continue;
        }

        let fixed = row.fixed();
        let mut record = vec![
            fixed.semantic_id.to_string(),
            fixed.semantic_type_name().to_string(),
        ];
        if options.include_hierarchy {
            record.push(optional_u64_cell(fixed.parent));
            record.push(serde_json::to_string(&fixed.children)?);
        }
        record.extend(dynamic.into_iter().map(|cell| cell.text));
        tsv.write_record(record)?;
    }

    tsv.flush()?;
    Ok(())
}

/// Writes semantic assignment rows as TSV.
///
/// # Errors
///
/// Returns an error when writing fails.
pub fn write_semantic_assignments_tsv<W: Write>(
    table: &SemanticAssignmentTable<'_>,
    options: &TsvWriteOptions,
    writer: W,
) -> Result<()> {
    let mut tsv = tsv_writer(writer);
    tsv.write_record(semantic_assignment_header(options.include_cityjson_ordinal))?;

    for row in table.rows() {
        if !options.include_null_rows && row.semantic_id.is_none() {
            continue;
        }
        tsv.write_record(semantic_assignment_cells(
            row,
            options.include_cityjson_ordinal,
        ))?;
    }

    tsv.flush()?;
    Ok(())
}

/// Writes semantic assignment rows joined to semantic definition attributes.
///
/// # Errors
///
/// Returns an error when writing fails or semantic values cannot be resolved.
pub fn write_split_semantics_tsv<W: Write>(
    semantics: &SemanticTable<'_>,
    assignments: &SemanticAssignmentTable<'_>,
    options: &TsvWriteOptions,
    writer: W,
) -> Result<()> {
    let semantic_rows = semantics.rows().collect::<Vec<_>>();
    let semantic_by_id = semantic_rows
        .iter()
        .copied()
        .map(|row| (row.fixed().semantic_id, row))
        .collect::<HashMap<_, _>>();

    let mut tsv = tsv_writer(writer);
    let mut header = semantic_assignment_header(options.include_cityjson_ordinal);
    header.push("semantic_type".to_string());
    if options.include_hierarchy {
        header.extend(["parent".to_string(), "children".to_string()]);
    }
    header.extend(
        semantics
            .schema()
            .columns
            .iter()
            .map(|column| column.name.clone()),
    );
    tsv.write_record(header)?;

    for assignment in assignments.rows() {
        let semantic = assignment
            .semantic_id
            .and_then(|semantic_id| semantic_by_id.get(&semantic_id).copied());
        let dynamic = match semantic {
            Some(row) => dynamic_cells(semantics.model(), row.values()).with_context(|| {
                format!(
                    "resolve dynamic values for semantic {}",
                    row.fixed().semantic_id
                )
            })?,
            None => null_cells(semantics.schema().columns.len()),
        };
        if !options.include_null_rows && all_null(&dynamic) {
            continue;
        }

        let mut record = semantic_assignment_cells(assignment, options.include_cityjson_ordinal);
        if let Some(row) = semantic {
            let fixed = row.fixed();
            record.push(fixed.semantic_type_name().to_string());
            if options.include_hierarchy {
                record.push(optional_u64_cell(fixed.parent));
                record.push(serde_json::to_string(&fixed.children)?);
            }
        } else {
            record.push(String::new());
            if options.include_hierarchy {
                record.extend([String::new(), String::new()]);
            }
        }
        record.extend(dynamic.into_iter().map(|cell| cell.text));
        tsv.write_record(record)?;
    }

    tsv.flush()?;
    Ok(())
}

fn tsv_writer<W: Write>(writer: W) -> csv::Writer<W> {
    csv::WriterBuilder::new()
        .delimiter(b'\t')
        .terminator(Terminator::Any(b'\n'))
        .from_writer(writer)
}

fn dynamic_cells<'value, 'model: 'value>(
    model: &'model CityModel,
    values: impl IntoIterator<Item = Result<Value<'value, 'model>>>,
) -> Result<Vec<TextCell>> {
    values
        .into_iter()
        .map(|value| value_to_text_cell(model, value?))
        .collect::<Result<Vec<_>>>()
}

fn null_cells(count: usize) -> Vec<TextCell> {
    (0..count)
        .map(|_| TextCell {
            text: String::new(),
            is_null: true,
        })
        .collect()
}

fn all_null(cells: &[TextCell]) -> bool {
    cells.iter().all(|cell| cell.is_null)
}

fn id_list_cell(ids: &IdList<'_>) -> Result<String> {
    Ok(serde_json::to_string(ids.ids())?)
}

fn metadata_fixed_header() -> Vec<String> {
    [
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
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn metadata_fixed_cells(row: &MetadataRow<'_>) -> Result<Vec<String>> {
    Ok(vec![
        option_string_cell(row.identifier.as_deref()),
        option_string_cell(row.reference_date.as_deref()),
        option_string_cell(row.reference_system.as_deref()),
        option_string_cell(row.title.as_deref()),
        row.geographical_extent
            .map(|bbox| serde_json::to_string(&bbox))
            .transpose()?
            .unwrap_or_default(),
        option_string_cell(row.geographical_extent_wkt.as_deref()),
        option_string_cell(row.contact_name.as_deref()),
        option_string_cell(row.contact_email_address.as_deref()),
        option_string_cell(row.contact_role.as_deref()),
        option_string_cell(row.contact_website.as_deref()),
        option_string_cell(row.contact_type.as_deref()),
        option_string_cell(row.contact_phone.as_deref()),
        option_string_cell(row.contact_organization.as_deref()),
    ])
}

fn semantic_assignment_header(include_cityobject_ix: bool) -> Vec<String> {
    let mut header = vec!["semantic_id".to_string(), "cityobject_id".to_string()];
    if include_cityobject_ix {
        header.push("cityobject_ix".to_string());
    }
    header.extend(
        [
            "geometry_ix",
            "geometry_type",
            "geometry_lod",
            "primitive_ix",
        ]
        .into_iter()
        .map(str::to_string),
    );
    header
}

fn semantic_assignment_cells(
    row: &SemanticAssignmentRow<'_>,
    include_cityobject_ix: bool,
) -> Vec<String> {
    let mut cells = vec![
        optional_u64_cell(row.semantic_id),
        row.cityobject_id.to_string(),
    ];
    if include_cityobject_ix {
        cells.push(row.cityobject_ix.to_string());
    }
    cells.extend([
        row.geometry_ix.to_string(),
        row.geometry_type.to_string(),
        option_string_cell(row.geometry_lod.as_deref()),
        row.primitive_ix.to_string(),
    ]);
    cells
}

fn option_string_cell(value: Option<&str>) -> String {
    value.unwrap_or_default().to_string()
}

fn optional_u64_cell(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}
