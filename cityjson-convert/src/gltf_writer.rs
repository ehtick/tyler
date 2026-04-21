#![allow(clippy::too_many_lines)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use cityjson_lib::cityjson::v2_0::{GeometryType, VertexIndex};
use cityjson_lib::CityModel;
use earcutr::earcut;
use gltf::json;
use log::info;
use meshopt::{
    generate_vertex_remap, optimize_overdraw_in_place_decoder, optimize_vertex_cache,
    optimize_vertex_fetch, quantize_snorm, remap_index_buffer, remap_vertex_buffer,
    DecodePosition,
};

const GLTF_VERSION: &str = "2.0";
const OVERDRAW_THRESHOLD: f32 = 1.05;
const QUANTIZATION_EXTENSION: &str = "KHR_mesh_quantization";
const QUANTIZED_POSITION_STRIDE: usize = std::mem::size_of::<QuantizedPosition>();
const QUANTIZED_NORMAL_STRIDE: usize = std::mem::size_of::<QuantizedNormal>();

/// Parse hex color string (#RRGGBB) to RGBA f32 array [R, G, B, A]
fn hex_to_rgba(hex: &str) -> Result<[f32; 4], anyhow::Error> {
    if hex.len() != 7 || !hex.starts_with('#') {
        return Err(anyhow::anyhow!(
            "Invalid hex color format: expected #RRGGBB"
        ));
    }
    let hex_digits = &hex[1..];
    let r = u8::from_str_radix(&hex_digits[0..2], 16)?;
    let g = u8::from_str_radix(&hex_digits[2..4], 16)?;
    let b = u8::from_str_radix(&hex_digits[4..6], 16)?;
    Ok([
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        1.0,
    ])
}

/// Create default PBR material matching pg2b3dm's structure
fn create_default_material(base_color: &str) -> Result<json::Material, anyhow::Error> {
    let base_color_rgba = hex_to_rgba(base_color)?;

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

/// Writes a `CityJSON` model as a binary glTF file.
///
/// # Errors
///
/// Returns an error when the model geometry cannot be read or triangulated, or
/// when the output GLB cannot be created.
pub fn write_city_model_glb<P: AsRef<Path>>(
    model: &CityModel,
    output_path: P,
    default_color: &str,
) -> Result<()> {
    let mut collector = MeshCollector::new();
    collector.add_model(model)?;
    let processed = collector.finish()?;
    info!(
        "Processed {} vertices and {} indices for the output GLB",
        processed.vertex_count(),
        processed.index_count()
    );
    processed.write_glb(output_path, default_color)
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

impl DecodePosition for Vertex {
    fn decode_position(&self) -> [f32; 3] {
        self.position
    }
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }

    fn add_point(&mut self, point: [f32; 3]) {
        for axis in 0..3 {
            self.min[axis] = self.min[axis].min(point[axis]);
            self.max[axis] = self.max[axis].max(point[axis]);
        }
    }

    fn from_vertices(vertices: &[Vertex]) -> Option<Self> {
        let mut bounds = Self::empty();
        let mut has_vertices = false;
        for vertex in vertices {
            bounds.add_point(vertex.position);
            has_vertices = true;
        }
        has_vertices.then_some(bounds)
    }

    fn center(&self) -> [f32; 3] {
        [
            f32::midpoint(self.min[0], self.max[0]),
            f32::midpoint(self.min[1], self.max[1]),
            f32::midpoint(self.min[2], self.max[2]),
        ]
    }
}

enum IndexBuffer {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl IndexBuffer {
    fn component_type(
        &self,
    ) -> json::validation::Checked<json::accessor::GenericComponentType> {
        match self {
            Self::U16(_) => json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::U16,
            )),
            Self::U32(_) => json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::U32,
            )),
        }
    }

    fn byte_length(&self) -> usize {
        match self {
            Self::U16(indices) => indices.len() * std::mem::size_of::<u16>(),
            Self::U32(indices) => indices.len() * std::mem::size_of::<u32>(),
        }
    }

    fn count(&self) -> usize {
        match self {
            Self::U16(indices) => indices.len(),
            Self::U32(indices) => indices.len(),
        }
    }

    fn max_value(&self) -> u32 {
        match self {
            Self::U16(indices) => indices.iter().copied().max().map_or(0, u32::from),
            Self::U32(indices) => indices.iter().copied().max().unwrap_or(0),
        }
    }

    fn write_bytes(&self, buffer: &mut Vec<u8>) {
        match self {
            Self::U16(indices) => {
                for index in indices {
                    buffer.extend_from_slice(&index.to_le_bytes());
                }
            }
            Self::U32(indices) => {
                for index in indices {
                    buffer.extend_from_slice(&index.to_le_bytes());
                }
            }
        }
    }
}

#[derive(Default)]
struct RawMesh {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

struct ProcessedMesh {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    center: [f32; 3],
    bounds: Option<Bounds>,
}

struct MeshCollector {
    mesh: RawMesh,
}

struct VertexAccessors {
    positions: json::Index<json::Accessor>,
    normals: json::Index<json::Accessor>,
}

#[derive(Default)]
struct BufferBuilder {
    bytes: Vec<u8>,
    buffer_views: Vec<json::buffer::View>,
    accessors: Vec<json::Accessor>,
}

struct EncodedGlb {
    root: json::Root,
    bin_buffer: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct QuantizedPosition {
    position: [i16; 4],
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct QuantizedNormal {
    normal: [i8; 4],
}

struct QuantizedMesh {
    positions: Vec<QuantizedPosition>,
    normals: Vec<QuantizedNormal>,
    indices: IndexBuffer,
    normalized_bounds: Bounds,
    position_scale: f32,
    center: [f32; 3],
}

impl MeshCollector {
    fn new() -> Self {
        Self {
            mesh: RawMesh::default(),
        }
    }

    fn add_model(&mut self, model: &CityModel) -> Result<()> {
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
                            self.add_surface(&surface, model)?;
                        }
                    }
                    GeometryType::Solid => {
                        let Some(boundary) = geometry.geometry().boundaries() else {
                            continue;
                        };
                        for shell in boundary.to_nested_solid()? {
                            for surface in shell {
                                self.add_surface(&surface, model)?;
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
                                    self.add_surface(&surface, model)?;
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

    fn finish(self) -> Result<ProcessedMesh> {
        self.mesh.into_processed()
    }

    fn add_surface(&mut self, surface: &[Vec<u32>], model: &CityModel) -> Result<()> {
        if surface.is_empty() {
            return Ok(());
        }
        let exterior = &surface[0];
        if exterior.len() < 3 {
            return Ok(());
        }

        let mut local_positions: Vec<[f32; 3]> = Vec::new();
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
                let position = Self::compute_position(vertex_id, model)?;
                local_positions.push(position);
                vertex_count += 1;
            }
        }

        if local_positions.len() < 3 {
            return Ok(());
        }

        let drop_axis = Self::find_projection_axis(&local_positions);
        for pos in &local_positions {
            match drop_axis {
                0 => {
                    flat_coords.push(f64::from(pos[1]));
                    flat_coords.push(f64::from(pos[2]));
                }
                1 => {
                    flat_coords.push(f64::from(pos[0]));
                    flat_coords.push(f64::from(pos[2]));
                }
                _ => {
                    flat_coords.push(f64::from(pos[0]));
                    flat_coords.push(f64::from(pos[1]));
                }
            }
        }

        let triangulated =
            earcut(&flat_coords, &hole_indices, 2).context("Failed to triangulate surface")?;
        if triangulated.len() < 3 {
            return Ok(());
        }

        self.mesh.emit_triangles(&local_positions, &triangulated, Self::compute_face_normal)
    }

    fn compute_position(idx: u32, model: &CityModel) -> Result<[f32; 3], anyhow::Error> {
        let vertex = model
            .get_vertex(VertexIndex::new(idx))
            .ok_or_else(|| anyhow::anyhow!("missing vertex {idx}"))?;
        let [x, y, z] = vertex.to_array();
        Ok([
            Self::f64_to_f32(x, "x", idx)?,
            Self::f64_to_f32(y, "y", idx)?,
            Self::f64_to_f32(z, "z", idx)?,
        ])
    }

    fn find_projection_axis(positions: &[[f32; 3]]) -> usize {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for pos in positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(pos[axis]);
                max[axis] = max[axis].max(pos[axis]);
            }
        }

        (0..3)
            .min_by(|&lhs, &rhs| {
                (max[lhs] - min[lhs])
                    .partial_cmp(&(max[rhs] - min[rhs]))
                    .unwrap()
            })
            .unwrap_or(2)
    }

    fn compute_face_normal(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3]) -> [f32; 3] {
        let u = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let v = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let normal = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let length =
            (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if length > f32::EPSILON {
            [normal[0] / length, normal[1] / length, normal[2] / length]
        } else {
            [0.0, 0.0, 1.0]
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn f64_to_f32(value: f64, axis: &str, vertex_id: u32) -> Result<f32> {
        if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
            anyhow::bail!("vertex {vertex_id} {axis} coordinate is outside the f32 range");
        }
        Ok(value as f32)
    }
}

impl RawMesh {
    fn emit_triangles<F>(
        &mut self,
        positions: &[[f32; 3]],
        triangulated: &[usize],
        compute_face_normal: F,
    ) -> Result<()>
    where
        F: Fn([f32; 3], [f32; 3], [f32; 3]) -> [f32; 3],
    {
        for tri in triangulated.chunks_exact(3) {
            let p0 = positions[tri[0]];
            let p1 = positions[tri[1]];
            let p2 = positions[tri[2]];
            let normal = compute_face_normal(p0, p1, p2);

            let base_index =
                u32::try_from(self.vertices.len()).context("GLB vertex count exceeds u32 range")?;
            self.vertices.push(Vertex {
                position: p0,
                normal,
            });
            self.vertices.push(Vertex {
                position: p1,
                normal,
            });
            self.vertices.push(Vertex {
                position: p2,
                normal,
            });
            self.indices
                .extend_from_slice(&[base_index, base_index + 1, base_index + 2]);
        }

        Ok(())
    }

    fn into_processed(mut self) -> Result<ProcessedMesh> {
        if self.vertices.is_empty() {
            return Ok(ProcessedMesh {
                vertices: self.vertices,
                indices: self.indices,
                center: [0.0; 3],
                bounds: None,
            });
        }

        let initial_bounds = Bounds::from_vertices(&self.vertices)
            .ok_or_else(|| anyhow::anyhow!("raw mesh bounds missing for non-empty mesh"))?;
        let center = initial_bounds.center();
        for vertex in &mut self.vertices {
            vertex.position[0] -= center[0];
            vertex.position[1] -= center[1];
            vertex.position[2] -= center[2];
        }

        self.optimize()?;
        let bounds = Bounds::from_vertices(&self.vertices);

        Ok(ProcessedMesh {
            vertices: self.vertices,
            indices: self.indices,
            center,
            bounds,
        })
    }

    fn optimize(&mut self) -> Result<()> {
        if self.vertices.is_empty() || self.indices.is_empty() {
            return Ok(());
        }

        let (vertex_count, remap) = generate_vertex_remap(&self.vertices, Some(&self.indices));
        let remapped_indices = remap_index_buffer(Some(&self.indices), vertex_count, &remap);
        let remapped_vertices = remap_vertex_buffer(&self.vertices, vertex_count, &remap);

        let mut optimized_indices =
            optimize_vertex_cache(&remapped_indices, remapped_vertices.len());
        optimize_overdraw_in_place_decoder(
            &mut optimized_indices,
            &remapped_vertices,
            OVERDRAW_THRESHOLD,
        );
        let optimized_vertices = optimize_vertex_fetch(&mut optimized_indices, &remapped_vertices);

        self.vertices = optimized_vertices;
        self.indices = optimized_indices;

        if self.vertices.len() > u32::MAX as usize {
            anyhow::bail!("GLB vertex count exceeds u32 index range");
        }

        Ok(())
    }
}

impl ProcessedMesh {
    fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    fn index_count(&self) -> usize {
        self.indices.len()
    }

    fn select_index_buffer(&self) -> Result<IndexBuffer> {
        if self.vertices.len() <= (u16::MAX as usize) + 1 {
            let mut indices = Vec::with_capacity(self.indices.len());
            for index in &self.indices {
                indices.push(
                    u16::try_from(*index)
                        .context("GLB index exceeds u16 range after mesh optimization")?,
                );
            }
            Ok(IndexBuffer::U16(indices))
        } else {
            Ok(IndexBuffer::U32(self.indices.clone()))
        }
    }

    fn write_glb<P: AsRef<Path>>(&self, output_path: P, default_color: &str) -> Result<()> {
        if self.vertices.is_empty() {
            info!(
                "No geometry to write, creating empty GLB file at {}",
                output_path.as_ref().display()
            );
            if let Some(parent) = output_path.as_ref().parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "Failed to create parent directory for {}",
                        output_path.as_ref().display()
                    )
                })?;
            }
            File::create(output_path.as_ref()).context("Create empty GLB file")?;
            return Ok(());
        }

        info!(
            "Writing GLB file with {} vertices and {} indices to {}",
            self.vertices.len(),
            self.indices.len(),
            output_path.as_ref().display()
        );

        let quantized = self.quantize()?;
        let encoded = quantized.encode_glb(default_color)?;
        let bounds = self
            .bounds
            .ok_or_else(|| anyhow::anyhow!("geometry bounds missing for non-empty mesh"))?;
        let index_buffer = self.select_index_buffer()?;
        encoded.write(output_path.as_ref())?;

        info!("GLB Summary: {}", output_path.as_ref().display());
        info!("  Vertices: {}", self.vertices.len());
        info!("  Indices: {}", index_buffer.count());
        info!(
            "  Local coordinate range: X [{:.2}, {:.2}], Y [{:.2}, {:.2}], Z [{:.2}, {:.2}]",
            bounds.min[0], bounds.max[0], bounds.min[1], bounds.max[1], bounds.min[2], bounds.max[2]
        );
        info!(
            "  World-space center: [{:.2}, {:.2}, {:.2}]",
            self.center[0], self.center[1], self.center[2]
        );

        Ok(())
    }

    fn quantize(&self) -> Result<QuantizedMesh> {
        let bounds = self
            .bounds
            .ok_or_else(|| anyhow::anyhow!("geometry bounds missing for non-empty mesh"))?;
        let index_buffer = self.select_index_buffer()?;

        let position_scale = [
            bounds.min[0].abs(),
            bounds.max[0].abs(),
            bounds.min[1].abs(),
            bounds.max[1].abs(),
            bounds.min[2].abs(),
            bounds.max[2].abs(),
        ]
        .into_iter()
        .fold(0.0_f32, f32::max)
        .max(f32::EPSILON);

        let normalized_bounds = Bounds {
            min: [
                (bounds.min[0] / position_scale).clamp(-1.0, 1.0),
                (bounds.min[1] / position_scale).clamp(-1.0, 1.0),
                (bounds.min[2] / position_scale).clamp(-1.0, 1.0),
            ],
            max: [
                (bounds.max[0] / position_scale).clamp(-1.0, 1.0),
                (bounds.max[1] / position_scale).clamp(-1.0, 1.0),
                (bounds.max[2] / position_scale).clamp(-1.0, 1.0),
            ],
        };

        let positions = self
            .vertices
            .iter()
            .map(|vertex| QuantizedPosition {
                position: [
                    quantize_snorm(vertex.position[0] / position_scale, 16) as i16,
                    quantize_snorm(vertex.position[1] / position_scale, 16) as i16,
                    quantize_snorm(vertex.position[2] / position_scale, 16) as i16,
                    0,
                ],
            })
            .collect();

        let normals = self
            .vertices
            .iter()
            .map(|vertex| QuantizedNormal {
                normal: [
                    quantize_snorm(vertex.normal[0], 8) as i8,
                    quantize_snorm(vertex.normal[1], 8) as i8,
                    quantize_snorm(vertex.normal[2], 8) as i8,
                    0,
                ],
            })
            .collect();

        Ok(QuantizedMesh {
            positions,
            normals,
            indices: index_buffer,
            normalized_bounds,
            position_scale,
            center: self.center,
        })
    }
}

impl QuantizedMesh {
    fn encode_glb(&self, default_color: &str) -> Result<EncodedGlb> {

        let mut buffer_builder = BufferBuilder::default();
        let vertex_accessors =
            buffer_builder.push_quantized_vertices(&self.positions, &self.normals, &self.normalized_bounds);
        let index_accessor = buffer_builder.push_indices(&self.indices);

        let mut attributes = std::collections::BTreeMap::new();
        attributes.insert(
            json::validation::Checked::Valid(json::mesh::Semantic::Positions),
            vertex_accessors.positions,
        );
        attributes.insert(
            json::validation::Checked::Valid(json::mesh::Semantic::Normals),
            vertex_accessors.normals,
        );

        let material = create_default_material(default_color)?;

        let primitive = json::mesh::Primitive {
            attributes,
            indices: Some(index_accessor),
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
                self.position_scale,
                0.0,
                0.0,
                0.0, //
                0.0,
                0.0,
                -self.position_scale,
                0.0, //
                0.0,
                self.position_scale,
                0.0,
                0.0, //
                self.center[0],
                self.center[2],
                -self.center[1],
                1.0,
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
            accessors: buffer_builder.accessors,
            buffers: vec![json::Buffer {
                byte_length: json::validation::USize64(buffer_builder.bytes.len() as u64),
                uri: None,
                name: Some("buffer0".into()),
                extensions: Default::default(),
                extras: Default::default(),
            }],
            buffer_views: buffer_builder.buffer_views,
            materials: vec![material],
            meshes: vec![mesh],
            nodes: vec![node],
            scenes: vec![scene],
            scene: Some(json::Index::new(0)),
            extensions_used: vec![QUANTIZATION_EXTENSION.to_string()],
            extensions_required: vec![QUANTIZATION_EXTENSION.to_string()],
            asset: json::Asset {
                version: GLTF_VERSION.into(),
                generator: Some("cityjson-convert".into()),
                copyright: None,
                ..Default::default()
            },
            ..Default::default()
        };

        Ok(EncodedGlb {
            root,
            bin_buffer: buffer_builder.bytes,
        })
    }
}

impl BufferBuilder {
    fn push_quantized_vertices(
        &mut self,
        positions: &[QuantizedPosition],
        normals: &[QuantizedNormal],
        bounds: &Bounds,
    ) -> VertexAccessors {
        let mut position_bytes = Vec::with_capacity(positions.len() * QUANTIZED_POSITION_STRIDE);
        for position in positions {
            for component in position.position {
                position_bytes.extend_from_slice(&component.to_le_bytes());
            }
        }

        let position_view = self.push_buffer_view(
            position_bytes,
            Some(QUANTIZED_POSITION_STRIDE),
            json::buffer::Target::ArrayBuffer,
        );
        let positions = self.push_accessor(json::Accessor {
            buffer_view: Some(position_view),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(positions.len() as u64),
            component_type: json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::I16,
            )),
            normalized: true,
            min: Some(json::Value::Array(
                bounds.min.iter().copied().map(json::Value::from).collect(),
            )),
            max: Some(json::Value::Array(
                bounds.max.iter().copied().map(json::Value::from).collect(),
            )),
            type_: json::validation::Checked::Valid(json::accessor::Type::Vec3),
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
            sparse: None,
        });

        let mut normal_bytes = Vec::with_capacity(normals.len() * QUANTIZED_NORMAL_STRIDE);
        for normal in normals {
            for component in normal.normal {
                normal_bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        let normal_view = self.push_buffer_view(
            normal_bytes,
            Some(QUANTIZED_NORMAL_STRIDE),
            json::buffer::Target::ArrayBuffer,
        );
        let normals = self.push_accessor(json::Accessor {
            buffer_view: Some(normal_view),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(normals.len() as u64),
            component_type: json::validation::Checked::Valid(json::accessor::GenericComponentType(
                json::accessor::ComponentType::I8,
            )),
            normalized: true,
            type_: json::validation::Checked::Valid(json::accessor::Type::Vec3),
            extensions: Default::default(),
            extras: Default::default(),
            min: None,
            max: None,
            name: None,
            sparse: None,
        });

        VertexAccessors { positions, normals }
    }

    fn push_indices(&mut self, index_buffer: &IndexBuffer) -> json::Index<json::Accessor> {
        let mut index_bytes = Vec::with_capacity(index_buffer.byte_length());
        index_buffer.write_bytes(&mut index_bytes);
        let view =
            self.push_buffer_view(index_bytes, None, json::buffer::Target::ElementArrayBuffer);
        self.push_accessor(json::Accessor {
            buffer_view: Some(view),
            byte_offset: Some(json::validation::USize64(0)),
            count: json::validation::USize64(index_buffer.count() as u64),
            component_type: index_buffer.component_type(),
            normalized: false,
            min: Some(json::Value::from(vec![0])),
            max: Some(json::Value::from(vec![index_buffer.max_value()])),
            type_: json::validation::Checked::Valid(json::accessor::Type::Scalar),
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
            sparse: None,
        })
    }

    fn push_buffer_view(
        &mut self,
        data: Vec<u8>,
        byte_stride: Option<usize>,
        target: json::buffer::Target,
    ) -> json::Index<json::buffer::View> {
        let byte_offset = self.bytes.len();
        let byte_length = data.len();
        self.bytes.extend_from_slice(&data);

        let index = self.buffer_views.len();
        self.buffer_views.push(json::buffer::View {
            buffer: json::Index::new(0),
            byte_length: json::validation::USize64(byte_length as u64),
            byte_offset: Some(json::validation::USize64(byte_offset as u64)),
            byte_stride: byte_stride.map(json::buffer::Stride),
            target: Some(json::validation::Checked::Valid(target)),
            extensions: Default::default(),
            extras: Default::default(),
            name: None,
        });

        json::Index::new(index as u32)
    }

    fn push_accessor(&mut self, accessor: json::Accessor) -> json::Index<json::Accessor> {
        let index = self.accessors.len();
        self.accessors.push(accessor);
        json::Index::new(index as u32)
    }
}

impl EncodedGlb {
    fn write<P: AsRef<Path>>(self, output_path: P) -> Result<()> {
        let mut json_bytes = json::serialize::to_string(&self.root)?.into_bytes();
        let json_padding = (4 - (json_bytes.len() % 4)) % 4;
        json_bytes.extend(std::iter::repeat_n(b' ', json_padding));

        let mut bin_buffer = self.bin_buffer;
        let bin_padding = (4 - (bin_buffer.len() % 4)) % 4;
        bin_buffer.extend(std::iter::repeat_n(0u8, bin_padding));

        let total_length = 12 + 8 + json_bytes.len() + 8 + bin_buffer.len();
        let total_length_u32 =
            u32::try_from(total_length).context("GLB total length exceeds u32 range")?;
        let json_length_u32 =
            u32::try_from(json_bytes.len()).context("GLB JSON chunk exceeds u32 range")?;
        let bin_length_u32 =
            u32::try_from(bin_buffer.len()).context("GLB BIN chunk exceeds u32 range")?;

        let mut glb_bytes = Vec::with_capacity(total_length);
        glb_bytes.extend_from_slice(b"glTF");
        glb_bytes.extend_from_slice(&2u32.to_le_bytes());
        glb_bytes.extend_from_slice(&total_length_u32.to_le_bytes());

        glb_bytes.extend_from_slice(&json_length_u32.to_le_bytes());
        glb_bytes.extend_from_slice(b"JSON");
        glb_bytes.extend_from_slice(&json_bytes);

        glb_bytes.extend_from_slice(&bin_length_u32.to_le_bytes());
        glb_bytes.extend_from_slice(b"BIN\0");
        glb_bytes.extend_from_slice(&bin_buffer);

        if let Some(parent) = output_path.as_ref().parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create parent directory for {}",
                    output_path.as_ref().display()
                )
            })?;
        }

        let mut file = File::create(output_path.as_ref())?;
        file.write_all(&glb_bytes)?;

        Ok(())
    }
}
