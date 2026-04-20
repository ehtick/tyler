use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cityjson_index::{CityIndex, IndexedFeatureRef};
use cityjson_lib::cityjson::v2_0::{GeometryType, VertexIndex};
use cityjson_lib::json::staged::from_feature_file_with_base;
use cityjson_lib::CityModel;
use earcutr::earcut;
use gltf::json as json;
use log::info;

use crate::parser::{FeatureReference, InputSource, World};
use crate::spatial_structs::{QuadTree, QuadTreeNodeId};

const GLTF_VERSION: &str = "2.0";

/// Parse hex color string (#RRGGBB) to RGBA f32 array [R, G, B, A]
fn hex_to_rgba(hex: &str) -> Result<[f32; 4], anyhow::Error> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return Err(anyhow::anyhow!("Invalid hex color format: expected #RRGGBB"));
    }
    let hex_digits = &hex[1..];
    let r = u8::from_str_radix(&hex_digits[0..2], 16)?;
    let g = u8::from_str_radix(&hex_digits[2..4], 16)?;
    let b = u8::from_str_radix(&hex_digits[4..6], 16)?;
    Ok([
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0, // Alpha, always opaque
    ])
}

/// Create default PBR material matching pg2b3dm's structure
fn create_default_material(base_color: &str) -> Result<json::Material, anyhow::Error> {
    let base_color_rgba = hex_to_rgba(base_color)?;
    
    // Metallic roughness factor from pg2b3dm default: #008000
    // Green channel = 128/255 = 0.501960... (roughness)
    // Red channel = 0/255 = 0.0 (metallic)
    let roughness_factor = 128.0 / 255.0;
    let metallic_factor = 0.0;
    
    Ok(json::Material {
        name: None,
        extensions: Default::default(),
        extras: Default::default(),
        pbr_metallic_roughness: json::material::PbrMetallicRoughness {
            base_color_factor: json::material::PbrBaseColorFactor(base_color_rgba),
            metallic_factor: json::material::StrengthFactor(metallic_factor),
            roughness_factor: json::material::StrengthFactor(roughness_factor),
            base_color_texture: None,
            metallic_roughness_texture: None,
            extensions: Default::default(),
            extras: Default::default(),
        },
        normal_texture: None,
        occlusion_texture: None,
        emissive_texture: None,
        emissive_factor: json::material::EmissiveFactor([0.0, 0.0, 0.0]),
        alpha_mode: json::validation::Checked::Valid(json::material::AlphaMode::Opaque),
        alpha_cutoff: None,
        double_sided: true,
    })
}

pub fn write_tile_glb<P: AsRef<Path>>(
    world: &World,
    quadtree: &QuadTree,
    qtree_node_id: QuadTreeNodeId,
    output_path: P,
    default_color: &str,
) -> Result<()> {
    info!("write_tile_glb called for node {:?}, output: {:?}", qtree_node_id, output_path.as_ref());
    let qtree_node = quadtree
        .node(&qtree_node_id)
        .context("Tile not present in quadtree")?;
    let feature_reader = FeatureReader::new(world)?;
    let mut builder = MeshBuilder::new();
    let mut feature_count = 0usize;

    for feature_id in collect_tile_feature_ids(world, qtree_node) {
        let feature = &world.features[feature_id];
        let model = feature_reader.load(&feature.reference)?;
        builder.add_model(&model)?;
        feature_count += 1;
    }

    info!(
        "Processed {} features for the output GLB, writing {} vertices and {} indices",
        feature_count,
        builder.positions.len(),
        builder.indices.len()
    );

    builder.write_glb(output_path, default_color)
}

enum FeatureReader {
    Legacy {
        features_root: PathBuf,
        feature_base_document: Vec<u8>,
    },
    CjIndex {
        city_index: CityIndex,
        refs_by_id: HashMap<String, IndexedFeatureRef>,
    },
}

impl FeatureReader {
    fn new(world: &World) -> Result<Self> {
        match &world.input_source {
            InputSource::LegacyFeatureFiles { features_root } => Ok(Self::Legacy {
                features_root: features_root.clone(),
                feature_base_document: world.feature_base_document.clone(),
            }),
            InputSource::CjIndexDataset { .. } => {
                let city_index = world
                    .input_source
                    .open_index()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                let mut refs_by_id = HashMap::new();
                for page_result in city_index.iter_all_feature_ref_pages(4096)? {
                    for feature_ref in page_result? {
                        refs_by_id.insert(feature_ref.feature_id.clone(), feature_ref);
                    }
                }
                Ok(Self::CjIndex {
                    city_index,
                    refs_by_id,
                })
            }
        }
    }

    fn load(&self, reference: &FeatureReference) -> Result<CityModel> {
        match (self, reference) {
            (
                Self::Legacy {
                    features_root,
                    feature_base_document,
                },
                FeatureReference::LegacyPath(relative_path),
            ) => {
                let feature_path = features_root.join(relative_path);
                from_feature_file_with_base(&feature_path, feature_base_document)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
            }
            (Self::CjIndex { city_index, refs_by_id }, FeatureReference::CjIndexId(feature_id)) => {
                let indexed = refs_by_id
                    .get(feature_id)
                    .ok_or_else(|| anyhow::anyhow!("missing cjindex feature reference {feature_id}"))?;
                city_index
                    .read_feature(indexed)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
            }
            (Self::Legacy { .. }, FeatureReference::CjIndexId(_)) => {
                Err(anyhow::anyhow!("legacy input unexpectedly referenced a cjindex feature"))
            }
            (Self::CjIndex { .. }, FeatureReference::LegacyPath(_)) => {
                Err(anyhow::anyhow!("cjindex input unexpectedly referenced a legacy feature path"))
            }
        }
    }
}

fn collect_tile_feature_ids(
    world: &World,
    qtree_node: &QuadTree,
) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut feature_ids = Vec::new();
    for cellid in qtree_node.cells() {
        let cell = world.grid.cell(cellid);
        for fid in &cell.feature_ids {
            if seen.insert(*fid) {
                feature_ids.push(*fid);
            }
        }
    }
    feature_ids
}

struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    fn new() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn add_model(&mut self, model: &CityModel) -> Result<()> {
        let mut vertex_cache: HashMap<u32, u32> = HashMap::new();

        for (_id, cityobject) in model.cityobjects().iter() {
            let Some(geometry_handles) = cityobject.geometry() else {
                continue;
            };
            for geometry_handle in geometry_handles {
                let geometry = model.resolve_geometry(*geometry_handle)?;
                match geometry.geometry().type_geometry() {
                    GeometryType::MultiSurface | GeometryType::CompositeSurface => {
                        let Some(boundary) = geometry.geometry().boundaries() else {
                            continue;
                        };
                        for surface in boundary.to_nested_multi_or_composite_surface()? {
                            self.add_surface(&surface, model, &mut vertex_cache)?;
                        }
                    }
                    GeometryType::Solid => {
                        let Some(boundary) = geometry.geometry().boundaries() else {
                            continue;
                        };
                        for shell in boundary.to_nested_solid()? {
                            for surface in shell {
                                self.add_surface(&surface, model, &mut vertex_cache)?;
                            }
                        }
                    }
                    GeometryType::MultiSolid | GeometryType::CompositeSolid => {
                        let Some(boundary) = geometry.geometry().boundaries() else {
                            continue;
                        };
                        for solid in boundary.to_nested_multi_or_composite_solid()? {
                            for shell in solid {
                                for surface in shell {
                                    self.add_surface(&surface, model, &mut vertex_cache)?;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn vertex_index(
        &mut self,
        idx: u32,
        position: [f32; 3],
        cache: &mut HashMap<u32, u32>,
    ) -> u32 {
        if let Some(&existing) = cache.get(&idx) {
            return existing;
        }
        self.positions.push(position);
        self.normals.push([0.0, 0.0, 0.0]);
        let index = (self.positions.len() - 1) as u32;
        cache.insert(idx, index);
        index
    }

    fn add_surface(
        &mut self,
        surface: &[Vec<u32>],
        model: &CityModel,
        cache: &mut HashMap<u32, u32>,
    ) -> Result<()> {
        if surface.is_empty() {
            return Ok(());
        }
        let exterior = &surface[0];
        if exterior.len() < 3 {
            return Ok(());
        }

        let mut local_positions: Vec<[f32; 3]> = Vec::new();
        let mut glb_indices: Vec<u32> = Vec::new();
        let mut flat_coords: Vec<f64> = Vec::new();
        let mut hole_indices: Vec<usize> = Vec::new();
        let mut vertex_count = 0usize;

        for (ring_idx, ring) in surface.iter().enumerate() {
            if ring.len() < 3 {
                continue;
            }
            if ring_idx > 0 {
                hole_indices.push(vertex_count);
            }

            for &vertex_id in ring {
                let position = self.compute_local_position(vertex_id, model)?;
                let glb_index = self.vertex_index(vertex_id, position, cache);
                local_positions.push(position);
                glb_indices.push(glb_index);
                vertex_count += 1;
            }
        }

        if glb_indices.len() < 3 {
            return Ok(());
        }

        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for pos in &local_positions {
            for axis in 0..3 {
                if pos[axis] < min[axis] {
                    min[axis] = pos[axis];
                }
                if pos[axis] > max[axis] {
                    max[axis] = pos[axis];
                }
            }
        }

        let mut ranges = [0.0f32; 3];
        for axis in 0..3 {
            ranges[axis] = max[axis] - min[axis];
        }
        let drop_axis = ranges
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(idx, _)| idx)
            .unwrap_or(2);

        for pos in &local_positions {
            match drop_axis {
                0 => {
                    flat_coords.push(pos[1] as f64);
                    flat_coords.push(pos[2] as f64);
                }
                1 => {
                    flat_coords.push(pos[0] as f64);
                    flat_coords.push(pos[2] as f64);
                }
                _ => {
                    flat_coords.push(pos[0] as f64);
                    flat_coords.push(pos[1] as f64);
                }
            }
        }

        let triangulated = earcut(&flat_coords, &hole_indices, 2);
        if triangulated.len() < 3 {
            return Ok(());
        }

        let mut face_indices = Vec::with_capacity(triangulated.len());
        for idx in triangulated {
            face_indices.push(glb_indices[idx]);
        }

        self.emit_triangles(face_indices);
        Ok(())
    }

    fn emit_triangles(&mut self, face_indices: Vec<u32>) {
        for tri in face_indices.chunks_exact(3) {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;

            let v0 = self.positions[i0];
            let v1 = self.positions[i1];
            let v2 = self.positions[i2];

            let u = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            let v = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
            let normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];

            for &i in tri {
                let n = &mut self.normals[i as usize];
                n[0] += normal[0];
                n[1] += normal[1];
                n[2] += normal[2];
            }

            self.indices.extend_from_slice(tri);
        }
    }

    fn compute_local_position(
        &self,
        idx: u32,
        model: &CityModel,
    ) -> Result<[f32; 3], anyhow::Error> {
        let vertex = model
            .get_vertex(VertexIndex::new(idx))
            .ok_or_else(|| anyhow::anyhow!("missing vertex {idx}"))?;
        let [x, y, z] = vertex.to_array();
        Ok([x as f32, y as f32, z as f32])
    }

    fn write_glb<P: AsRef<Path>>(&mut self, output_path: P, default_color: &str) -> Result<()> {
        self.normalize_normals();

        if self.positions.is_empty() {
            info!(
                "No geometry to write, creating empty GLB file at {:?}",
                output_path.as_ref()
            );
            if let Some(parent) = output_path.as_ref().parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| {
                        format!(
                            "Failed to create parent directory for {:?}",
                            output_path.as_ref()
                        )
                    })?;
            }
            File::create(output_path.as_ref()).context("Create empty GLB file")?;
            return Ok(());
        }
        info!(
            "Writing GLB file with {} vertices and {} indices to {:?}",
            self.positions.len(),
            self.indices.len(),
            output_path.as_ref()
        );

        let mut bin_buffer: Vec<u8> = Vec::new();

        let positions_offset = 0;
        if !self.positions.is_empty() {
            info!(
                "First position in self.positions: [{:.2}, {:.2}, {:.2}]",
                self.positions[0][0], self.positions[0][1], self.positions[0][2]
            );
            if self.positions.len() > 1 {
                info!(
                    "Second position in self.positions: [{:.2}, {:.2}, {:.2}]",
                    self.positions[1][0], self.positions[1][1], self.positions[1][2]
                );
            }
        }
        for p in &self.positions {
            for component in p {
                bin_buffer.extend_from_slice(&component.to_le_bytes());
            }
        }

        let normals_offset = bin_buffer.len();
        for n in &self.normals {
            for component in n {
                bin_buffer.extend_from_slice(&component.to_le_bytes());
            }
        }

        let indices_offset = bin_buffer.len();
        for index in &self.indices {
            bin_buffer.extend_from_slice(&index.to_le_bytes());
        }

        let accessor_positions = json::Accessor {
            buffer_view: Some(json::Index::new(0)),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(self.positions.len() as u64),
            component_type: json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::F32,
            )),
            normalized: false,
            min: Some(json::Value::Array(
                (0..3)
                    .map(|axis| {
                        let min = self.positions.iter().map(|v| v[axis]).fold(f32::INFINITY, f32::min);
                        json::Value::from(min)
                    })
                    .collect(),
            )),
            max: Some(json::Value::Array(
                (0..3)
                    .map(|axis| {
                        let max = self.positions.iter().map(|v| v[axis]).fold(f32::NEG_INFINITY, f32::max);
                        json::Value::from(max)
                    })
                    .collect(),
            )),
            type_: json::validation::Checked::Valid(json::accessor::Type::Vec3),
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
            sparse: None,
        };

        let accessor_normals = json::Accessor {
            buffer_view: Some(json::Index::new(1)),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(self.normals.len() as u64),
            component_type: json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::F32,
            )),
            normalized: false,
            type_: json::validation::Checked::Valid(json::accessor::Type::Vec3),
            extensions: Default::default(),
            extras: Default::default(),
            min: None,
            max: None,
            name: None,
            sparse: None,
        };

        let accessor_indices = json::Accessor {
            buffer_view: Some(json::Index::new(2)),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(self.indices.len() as u64),
            component_type: json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::U32,
            )),
            normalized: false,
            min: Some(json::Value::from(vec![0])),
            max: Some(json::Value::from(vec![self.indices.iter().copied().max().unwrap_or(0)])),
            type_: json::validation::Checked::Valid(json::accessor::Type::Scalar),
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
            sparse: None,
        };

        let buffer_views = vec![
            json::buffer::View {
                buffer: json::Index::new(0),
                byte_length: json::validation::USize64((self.positions.len() * 12) as u64),
                byte_offset: Some(json::validation::USize64(positions_offset as u64)),
                byte_stride: Some(json::buffer::Stride(12)),
                target: Some(json::validation::Checked::Valid(json::buffer::Target::ArrayBuffer)),
                extensions: Default::default(),
                extras: Default::default(),
                name: None,
            },
            json::buffer::View {
                buffer: json::Index::new(0),
                byte_length: json::validation::USize64((self.normals.len() * 12) as u64),
                byte_offset: Some(json::validation::USize64(normals_offset as u64)),
                byte_stride: Some(json::buffer::Stride(12)),
                target: Some(json::validation::Checked::Valid(json::buffer::Target::ArrayBuffer)),
                extensions: Default::default(),
                extras: Default::default(),
                name: None,
            },
            json::buffer::View {
                buffer: json::Index::new(0),
                byte_length: json::validation::USize64((self.indices.len() * 4) as u64),
                byte_offset: Some(json::validation::USize64(indices_offset as u64)),
                byte_stride: None,
                target: Some(json::validation::Checked::Valid(json::buffer::Target::ElementArrayBuffer)),
                extensions: Default::default(),
                extras: Default::default(),
                name: None,
            },
        ];

        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert(
            json::validation::Checked::Valid(json::mesh::Semantic::Positions),
            json::Index::new(0),
        );
        attributes.insert(
            json::validation::Checked::Valid(json::mesh::Semantic::Normals),
            json::Index::new(1),
        );

        let material = create_default_material(default_color)?;

        let primitive = json::mesh::Primitive {
            attributes,
            indices: Some(json::Index::new(2)),
            material: Some(json::Index::new(0)),
            mode: json::validation::Checked::Valid(json::mesh::Mode::Triangles),
            targets: None,
            extensions: Default::default(),
            extras: Default::default(),
        };

        let mesh = json::Mesh {
            primitives: vec![primitive],
            weights: None,
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
        };

        let node = json::Node {
            mesh: Some(json::Index::new(0)),
            camera: None,
            children: None,
            skin: None,
            matrix: Some([
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, //
                0.0, 0.0, 0.0, 1.0,
            ]),
            rotation: None,
            scale: None,
            translation: None,
            weights: None,
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
        };

        let scene = json::Scene {
            nodes: vec![json::Index::new(0)],
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
        };

        let root = json::Root {
            accessors: vec![accessor_positions, accessor_normals, accessor_indices],
            buffers: vec![json::Buffer {
                byte_length: json::validation::USize64(bin_buffer.len() as u64),
                uri: None,
                name: Some("buffer0".into()),
                extensions: Default::default(),
                extras: Default::default(),
            }],
            buffer_views,
            materials: vec![material],
            meshes: vec![mesh],
            nodes: vec![node],
            scenes: vec![scene],
            scene: Some(json::Index::new(0)),
            asset: json::Asset {
                version: GLTF_VERSION.into(),
                generator: Some("cityjson-export".into()),
                copyright: None,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut json_bytes = json::serialize::to_string(&root)?.into_bytes();
        let json_padding = (4 - (json_bytes.len() % 4)) % 4;
        json_bytes.extend(std::iter::repeat(b' ').take(json_padding));

        let bin_padding = (4 - (bin_buffer.len() % 4)) % 4;
        bin_buffer.extend(std::iter::repeat(0).take(bin_padding));

        let total_length = 12 + 8 + json_bytes.len() + 8 + bin_buffer.len();
        let mut glb_bytes = Vec::with_capacity(total_length);
        glb_bytes.extend_from_slice(b"glTF");
        glb_bytes.extend_from_slice(&2u32.to_le_bytes());
        glb_bytes.extend_from_slice(&(total_length as u32).to_le_bytes());

        glb_bytes.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb_bytes.extend_from_slice(b"JSON");
        glb_bytes.extend_from_slice(&json_bytes);

        glb_bytes.extend_from_slice(&(bin_buffer.len() as u32).to_le_bytes());
        glb_bytes.extend_from_slice(b"BIN\0");
        glb_bytes.extend_from_slice(&bin_buffer);

        // Create parent directories if they don't exist
        if let Some(parent) = output_path.as_ref().parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent directory for {:?}", output_path.as_ref()))?;
        }
        
        let output_path_str = output_path.as_ref().to_string_lossy().to_string();

        let mut file = File::create(output_path)?;
        file.write_all(&glb_bytes)?;

        if !self.positions.is_empty() {
            let min_x = self.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            let max_x = self.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
            let min_y = self.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
            let max_y = self.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
            let min_z = self.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
            let max_z = self.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);

            let center_x = (min_x + max_x) / 2.0;
            let center_y = (min_y + max_y) / 2.0;
            let center_z = (min_z + max_z) / 2.0;

            info!("GLB Summary: {}", output_path_str);
            info!("  Vertices: {}", self.positions.len());
            info!("  Coordinate range:");
            info!("    X: [{:.2}, {:.2}] (span: {:.2})", min_x, max_x, max_x - min_x);
            info!("    Y: [{:.2}, {:.2}] (span: {:.2})", min_y, max_y, max_y - min_y);
            info!("    Z: [{:.2}, {:.2}] (span: {:.2})", min_z, max_z, max_z - min_z);
            info!("  Coordinate center: [{:.2}, {:.2}, {:.2}]", center_x, center_y, center_z);
        }

        Ok(())
    }

    fn normalize_normals(&mut self) {
        for normal in self.normals.iter_mut() {
            let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            if length > f32::EPSILON {
                normal[0] /= length;
                normal[1] /= length;
                normal[2] /= length;
            } else {
                // Zero-length normal (degenerate triangles). Use default up vector [0, 1, 0]
                // which is appropriate for glTF Y-up coordinate system
                normal[0] = 0.0;
                normal[1] = 1.0;
                normal[2] = 0.0;
            }
        }
    }
}
