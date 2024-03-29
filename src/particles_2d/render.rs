use super::{update::Particle, ParticleStore};
use bevy::{
    core_pipeline::core_2d::Transparent2d,
    ecs::{
        entity::EntityHashMap,
        system::{
            lifetimeless::{Read, SRes},
            SystemParamItem,
        },
    },
    prelude::*,
    render::{
        mesh::{GpuBufferInfo, MeshVertexBufferLayout, PrimitiveTopology},
        render_asset::RenderAssets,
        render_phase::{
            AddRenderCommand, DrawFunctions, PhaseItem, RenderCommand, RenderCommandResult,
            RenderPhase, SetItemPipeline, TrackedRenderPass,
        },
        render_resource::{
            binding_types::uniform_buffer, AsBindGroup, BindGroup, BindGroupEntries,
            BindGroupLayout, BindGroupLayoutEntries, BlendState, Buffer, BufferInitDescriptor,
            BufferSlice, BufferUsages, BufferVec, ColorTargetState, ColorWrites, FrontFace,
            IndexFormat, OwnedBindingResource, PipelineCache, PolygonMode, PrimitiveState,
            RenderPipelineDescriptor, ShaderRef, ShaderStages, SpecializedMeshPipeline,
            SpecializedMeshPipelineError, SpecializedMeshPipelines, SpecializedRenderPipeline,
            SpecializedRenderPipelines, TextureFormat, VertexAttribute, VertexBufferLayout,
            VertexFormat, VertexStepMode,
        },
        renderer::{RenderDevice, RenderQueue},
        texture::{BevyDefault, FallbackImage},
        view::{
            ExtractedView, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms,
            VisibleEntities,
        },
        Extract, Render, RenderApp, RenderSet,
    },
    sprite::{
        Mesh2dPipeline, Mesh2dPipelineKey, RenderMesh2dInstances, SetMesh2dBindGroup,
        SetMesh2dViewBindGroup,
    },
    utils::{FloatOrd, HashMap, HashSet},
};
use bytemuck::{Pod, Zeroable};
use std::hash::Hash;

/// Particle Material Trait
/// bind custom fragment shader to material
pub trait Particle2dMaterial: AsBindGroup + Asset + Clone + Sized {
    fn fragment_shader() -> ShaderRef {
        super::PARTICLE_DEFAULT_FRAG.into()
    }
}

pub struct Particle2dMaterialPlugin<M: Particle2dMaterial> {
    _m: std::marker::PhantomData<M>,
}

impl<M: Particle2dMaterial> Default for Particle2dMaterialPlugin<M> {
    fn default() -> Self {
        Self {
            _m: std::marker::PhantomData::<M>::default(),
        }
    }
}

impl<M: Particle2dMaterial> Plugin for Particle2dMaterialPlugin<M> {
    fn build(&self, app: &mut App) {
        app.init_asset::<M>();
        app.sub_app_mut(RenderApp)
            .add_render_command::<Transparent2d, DrawParticle2d<M>>()
            .init_resource::<SpecializedRenderPipelines<Particle2dPipeline<M>>>()
            .init_resource::<ExtractedParticleBatches<M>>()
            .init_resource::<ExtractedParticleMaterials<M>>()
            .init_resource::<PreparedParticleMaterials<M>>()
            .init_resource::<RenderParticleMaterials<M>>()
            .init_resource::<ParticleMeta<M>>()
            .add_systems(
                ExtractSchedule,
                (extract_particles::<M>, extract_materials::<M>),
            )
            .add_systems(
                Render,
                (
                    queue_particles::<M>.in_set(RenderSet::Queue),
                    prepare_particle_materials::<M>.in_set(RenderSet::PrepareBindGroups),
                    prepare_particles_instance_buffers::<M>.in_set(RenderSet::PrepareResources),
                ),
            );
    }

    fn finish(&self, app: &mut App) {
        let render_app = app.sub_app_mut(RenderApp);
        render_app.init_resource::<Particle2dPipeline<M>>();
        let render_device = render_app.world.resource::<RenderDevice>();
        let mut buffer = BufferVec::<u32>::new(BufferUsages::INDEX);
        // // quad order
        buffer.push(2);
        buffer.push(0);
        buffer.push(1);
        buffer.push(1);
        buffer.push(3);
        buffer.push(2);
        buffer.write_buffer(render_device, render_app.world.resource::<RenderQueue>());
        render_app.insert_resource(ParticleMeta::<M> {
            index_buffer: buffer,
            view_bind_group: None,
            _m: Default::default(),
        });
    }
}

#[derive(Resource)]
pub struct ParticleMeta<M: Particle2dMaterial> {
    view_bind_group: Option<BindGroup>,
    index_buffer: BufferVec<u32>,
    _m: std::marker::PhantomData<M>,
}

impl<M: Particle2dMaterial> Default for ParticleMeta<M> {
    fn default() -> Self {
        Self {
            view_bind_group: None,
            index_buffer: BufferVec::<u32>::new(BufferUsages::INDEX),
            _m: Default::default(),
        }
    }
}

// ----------------------------------------------
// extract
#[derive(Resource)]
pub struct ExtractedParticleMaterials<M: Particle2dMaterial> {
    materials: Vec<(AssetId<M>, M)>,
}

impl<M: Particle2dMaterial> Default for ExtractedParticleMaterials<M> {
    fn default() -> Self {
        Self {
            materials: Vec::default(),
        }
    }
}

#[derive(Resource)]
pub struct ExtractedParticleBatches<M: Particle2dMaterial> {
    particles: EntityHashMap<Vec<InstanceData>>,
    _m: std::marker::PhantomData<M>,
}
impl<M: Particle2dMaterial> Default for ExtractedParticleBatches<M> {
    fn default() -> Self {
        Self {
            particles: Default::default(),
            _m: Default::default(),
        }
    }
}

fn extract_materials<M: Particle2dMaterial>(
    mut events: Extract<EventReader<AssetEvent<M>>>,
    mut materials: ResMut<ExtractedParticleMaterials<M>>,
    assets: Extract<Res<Assets<M>>>,
) {
    for event in events.read() {
        match event {
            AssetEvent::Added { id } | AssetEvent::Modified { id } => {
                if let Some(asset) = assets.get(*id) {
                    materials.materials.push((*id, asset.clone()));
                }
            }
            AssetEvent::Removed { id } => {
                materials.materials.retain(|(i, _)| i != id);
            }
            _ => (),
        }
    }
}

fn extract_particles<M: Particle2dMaterial>(
    mut cmd: Commands,
    mut extraced_batches: ResMut<ExtractedParticleBatches<M>>,
    mut render_material_instances: ResMut<RenderParticleMaterials<M>>,
    query: Extract<
        Query<(
            Entity,
            &ParticleStore,
            &GlobalTransform,
            &Handle<M>,
            &ViewVisibility,
        )>,
    >,
) {
    extraced_batches.particles.clear();
    query.iter().for_each(|emitter| {
        let (entity, particle_store, global, material_handle, visbility) = emitter;
        if !visbility.get() {
            return;
        }

        let particles = particle_store
            .iter()
            .map(|particle| InstanceData::from(particle))
            .collect::<Vec<_>>();

        extraced_batches.particles.insert(entity, particles);
        render_material_instances.insert(entity, material_handle.id());

        cmd.get_or_spawn(entity)
            .insert(ZOrder(FloatOrd(global.translation().z)));
    });
}

#[derive(Component, Deref)]
pub struct ZOrder(FloatOrd);

// ----------------------------------------------
// queue

fn queue_particles<M: Particle2dMaterial>(
    transparent_2d_draw_functions: Res<DrawFunctions<Transparent2d>>,
    custom_pipeline: Res<Particle2dPipeline<M>>,
    msaa: Res<Msaa>,
    mut pipelines: ResMut<SpecializedRenderPipelines<Particle2dPipeline<M>>>,
    pipeline_cache: Res<PipelineCache>,
    extract_particles: Res<ExtractedParticleBatches<M>>,
    z_orders: Query<&ZOrder>,

    mut views: Query<(
        &ExtractedView,
        &VisibleEntities,
        &mut RenderPhase<Transparent2d>,
    )>,
) {
    let draw_particles = transparent_2d_draw_functions
        .read()
        .id::<DrawParticle2d<M>>();
    // let msaa_key = Mesh2dPipelineKey::from_msaa_samples(msaa.samples());

    for (view, visible_entities, mut transparent_phase) in &mut views {
        // let view_key = msaa_key | Mesh2dPipelineKey::from_hdr(view.hdr);

        for (entity, _) in extract_particles.particles.iter() {
            if !visible_entities.entities.contains(&entity) {
                continue;
            }

            let mesh_key = Mesh2dPipelineKey::from_msaa_samples(msaa.samples())
                | Mesh2dPipelineKey::from_hdr(view.hdr);

            let key = Particle2dPipelineKey { mesh_key };

            let pipeline = pipelines.specialize(&pipeline_cache, &custom_pipeline, key);

            let Ok(order) = z_orders.get(*entity) else {
                return;
            };

            transparent_phase.add(Transparent2d {
                sort_key: **order,
                entity: *entity,
                pipeline,
                draw_function: draw_particles,
                batch_range: 0..1,
                dynamic_offset: None,
            });
        }
    }
}

// ----------------------------------------------
//

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct InstanceData {
    transform: [Vec4; 3],
    color: [f32; 4],
    custom: Vec4,
}
impl From<&Particle> for InstanceData {
    #[inline(always)]
    fn from(value: &Particle) -> Self {
        let transpose_model_3x3 = value.transform.compute_affine().matrix3.transpose();
        Self {
            transform: [
                transpose_model_3x3
                    .x_axis
                    .extend(value.transform.translation.x),
                transpose_model_3x3
                    .y_axis
                    .extend(value.transform.translation.y),
                transpose_model_3x3
                    .z_axis
                    .extend(value.transform.translation.z),
            ],
            color: value.color.as_linear_rgba_f32(),
            custom: Vec4::new(
                value.lifetime.fraction(),
                value.lifetime.duration().as_secs_f32(),
                0.,
                0.,
            ),
        }
    }
}

#[derive(Component, Deref)]
pub struct InstanceMaterialData(Vec<InstanceData>);

#[derive(Component)]
struct InstanceBuffer {
    buffer: Buffer,
    length: usize,
}

#[derive(Resource)]
pub struct PreparedParticleMaterial<M: Particle2dMaterial> {
    pub bindings: Vec<(u32, OwnedBindingResource)>,
    pub bind_group: BindGroup,
    pub key: M::Data,
}

#[derive(Resource, Deref, DerefMut)]
pub struct PreparedParticleMaterials<M: Particle2dMaterial>(
    HashMap<AssetId<M>, PreparedParticleMaterial<M>>,
);

#[derive(Deref, DerefMut)]
pub struct PrepareNextFrameParticleMaterials<M: Particle2dMaterial>(Vec<(AssetId<M>, M)>);

impl<M: Particle2dMaterial> Default for PrepareNextFrameParticleMaterials<M> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<M: Particle2dMaterial> Default for PreparedParticleMaterials<M> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

fn prepare_particle_materials<M: Particle2dMaterial>(
    mut extraced_materials: ResMut<ExtractedParticleMaterials<M>>,
    mut render_particle_materials: ResMut<PreparedParticleMaterials<M>>,
    mut prepare_next_frame: Local<PrepareNextFrameParticleMaterials<M>>,

    render_device: Res<RenderDevice>,
    images: Res<RenderAssets<Image>>,
    fallback_image: Res<FallbackImage>,
    pipeline: Res<Particle2dPipeline<M>>,
) {
    for (asset_id, material) in std::mem::take(&mut prepare_next_frame.0) {
        match material.as_bind_group(
            &pipeline.uniform_layout,
            &render_device,
            &images,
            &fallback_image,
        ) {
            Ok(prepared) => {
                render_particle_materials.insert(
                    asset_id,
                    PreparedParticleMaterial {
                        bindings: prepared.bindings,
                        bind_group: prepared.bind_group,
                        key: prepared.data,
                    },
                );
            }
            Err(_) => {
                warn!("failed prep retry");
                prepare_next_frame.push((asset_id, material));
            }
        }
    }

    for (asset_id, material) in std::mem::take(&mut extraced_materials.materials) {
        match material.as_bind_group(
            &pipeline.uniform_layout,
            &render_device,
            &images,
            &fallback_image,
        ) {
            Ok(prepared) => {
                render_particle_materials.insert(
                    asset_id,
                    PreparedParticleMaterial {
                        bindings: prepared.bindings,
                        bind_group: prepared.bind_group,
                        key: prepared.data,
                    },
                );
            }
            Err(_) => {
                warn!("failed prep");
                prepare_next_frame.push((asset_id, material));
            }
        }
    }
}

#[derive(Resource, DerefMut, Deref)]
pub struct RenderParticleMaterials<M: Particle2dMaterial>(EntityHashMap<AssetId<M>>);

impl<M: Particle2dMaterial> Default for RenderParticleMaterials<M> {
    fn default() -> Self {
        Self(EntityHashMap::default())
    }
}

fn prepare_particles_instance_buffers<M: Particle2dMaterial>(
    mut cmd: Commands,
    extraced_batches: Res<ExtractedParticleBatches<M>>,
    render_device: Res<RenderDevice>,
    view_uniforms: Res<ViewUniforms>,
    particle_pipeline: Res<Particle2dPipeline<M>>,
    mut particle_meta: ResMut<ParticleMeta<M>>,
) {
    if let Some(view_binding) = view_uniforms.uniforms.binding() {
        particle_meta.view_bind_group = Some(render_device.create_bind_group(
            "particle_view_bind_group",
            &particle_pipeline.view_layout,
            &BindGroupEntries::single(view_binding),
        ));
    }

    // @todo: merge together
    for (entity, particle_instances) in extraced_batches.particles.iter() {
        let buffer = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("instance data buffer"),
            contents: bytemuck::cast_slice(particle_instances.as_slice()),
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
        });

        cmd.entity(*entity).insert(InstanceBuffer {
            buffer,
            length: particle_instances.len(),
        });
    }
}

// ----------------------------------------------
// pipeline

#[derive(Resource)]
struct Particle2dPipeline<M: Particle2dMaterial> {
    vertex_shader: Handle<Shader>,
    fragment_shader: Handle<Shader>,
    uniform_layout: BindGroupLayout,
    view_layout: BindGroupLayout,
    _m: std::marker::PhantomData<M>,
}

#[derive(PartialEq, Eq, Hash, Clone)]
struct Particle2dPipelineKey {
    mesh_key: Mesh2dPipelineKey,
}

impl<M: Particle2dMaterial> FromWorld for Particle2dPipeline<M> {
    fn from_world(world: &mut World) -> Self {
        let server = world.resource::<AssetServer>();

        let fragment_shader = match M::fragment_shader() {
            ShaderRef::Default => super::PARTICLE_DEFAULT_FRAG,
            ShaderRef::Handle(handle) => handle,
            ShaderRef::Path(path) => server.load(path),
        };

        let vertex_shader = super::PARTICLE_VERTEX;
        let render_device = world.resource::<RenderDevice>();

        let view_layout = render_device.create_bind_group_layout(
            "particle_view_layout",
            &BindGroupLayoutEntries::single(
                ShaderStages::VERTEX_FRAGMENT,
                uniform_buffer::<ViewUniform>(true),
            ),
        );

        Particle2dPipeline {
            view_layout,
            uniform_layout: M::bind_group_layout(render_device), //world.resource::<ParticleUniformLayout>().0.clone(),
            vertex_shader,
            fragment_shader,
            _m: std::marker::PhantomData::<M>::default(),
        }
    }
}

impl<M: Particle2dMaterial> SpecializedRenderPipeline for Particle2dPipeline<M> {
    type Key = Particle2dPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let layout = vec![self.view_layout.clone(), self.uniform_layout.clone()];

        let format = if key.mesh_key.contains(Mesh2dPipelineKey::HDR) {
            ViewTarget::TEXTURE_FORMAT_HDR
        } else {
            TextureFormat::bevy_default()
        };

        RenderPipelineDescriptor {
            vertex: bevy::render::render_resource::VertexState {
                shader: self.vertex_shader.clone(),
                shader_defs: vec![],
                entry_point: "vertex".into(),
                buffers: vec![VertexBufferLayout {
                    array_stride: VertexFormat::Float32x4.size() * 5,
                    step_mode: VertexStepMode::Instance,
                    attributes: create_vertex_attributes(),
                }],
            },
            fragment: Some(bevy::render::render_resource::FragmentState {
                shader: self.fragment_shader.clone(),
                shader_defs: vec![],
                entry_point: "fragment".into(),
                targets: vec![Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            label: Some("particle 2d pipeline".into()),
            layout,
            push_constant_ranges: vec![],
            primitive: PrimitiveState {
                front_face: FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: PolygonMode::Fill,
                conservative: false,
                topology: PrimitiveTopology::TriangleList,
                strip_index_format: None,
            },
            depth_stencil: None,
            multisample: bevy::render::render_resource::MultisampleState {
                count: key.mesh_key.msaa_samples(),
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
        }
    }
}

fn create_vertex_attributes() -> Vec<VertexAttribute> {
    let mut attributes = Vec::new();
    let mut offset = 0;

    // translation
    attributes.push(VertexAttribute {
        format: VertexFormat::Float32x4,
        offset,
        shader_location: 0,
    });
    offset += VertexFormat::Float32x4.size();

    // rotation
    attributes.push(VertexAttribute {
        format: VertexFormat::Float32x4,
        offset,
        shader_location: 1,
    });
    offset += VertexFormat::Float32x4.size();

    // scale
    attributes.push(VertexAttribute {
        format: VertexFormat::Float32x4,
        offset,
        shader_location: 2,
    });
    offset += VertexFormat::Float32x4.size();

    // color
    attributes.push(VertexAttribute {
        format: VertexFormat::Float32x4,
        offset,
        shader_location: 3,
    });
    offset += VertexFormat::Float32x4.size();

    // custom
    attributes.push(VertexAttribute {
        format: VertexFormat::Float32x4,
        offset,
        shader_location: 4,
    });

    attributes
}

// ----------------------------------------------
// rendering

type DrawParticle2d<M> = (
    SetItemPipeline,
    SetParticleViewBindGroup<0, M>,
    SetParticle2dBindGroup<1, M>,
    DrawParticleInstanced<M>,
);

pub struct SetParticleViewBindGroup<const I: usize, M: Particle2dMaterial>(
    std::marker::PhantomData<M>,
);
impl<P: PhaseItem, M: Particle2dMaterial, const I: usize> RenderCommand<P>
    for SetParticleViewBindGroup<I, M>
{
    type Param = SRes<ParticleMeta<M>>;
    type ViewQuery = Read<ViewUniformOffset>;
    type ItemQuery = ();

    fn render<'w>(
        _item: &P,
        view_uniform: &'_ ViewUniformOffset,
        _entity: Option<()>,
        particle_meta: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        if let Some(bind_group) = &particle_meta.into_inner().view_bind_group.as_ref() {
            pass.set_bind_group(I, bind_group, &[view_uniform.offset]);
            return RenderCommandResult::Success;
        }
        RenderCommandResult::Failure
    }
}

struct SetParticle2dBindGroup<const I: usize, M: Particle2dMaterial>(std::marker::PhantomData<M>);
impl<const I: usize, M: Particle2dMaterial, P: PhaseItem> RenderCommand<P>
    for SetParticle2dBindGroup<I, M>
{
    type Param = (
        SRes<PreparedParticleMaterials<M>>,
        SRes<RenderParticleMaterials<M>>,
    );
    type ViewQuery = ();
    type ItemQuery = ();

    #[inline]
    fn render<'w>(
        item: &P,
        _view: bevy::ecs::query::ROQueryItem<'w, Self::ViewQuery>,
        _item_query: Option<bevy::ecs::query::ROQueryItem<'w, Self::ItemQuery>>,
        params: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let (prep_mats, prep_particles) = params;

        let Some(asset_id) = prep_particles.into_inner().get(&item.entity()) else {
            return RenderCommandResult::Failure;
        };

        let Some(prepared_material) = prep_mats.into_inner().get(asset_id) else {
            return RenderCommandResult::Failure;
        };

        pass.set_bind_group(I, &prepared_material.bind_group, &[]);

        RenderCommandResult::Success
    }
}

struct DrawParticleInstanced<M: Particle2dMaterial>(std::marker::PhantomData<M>);
impl<P: PhaseItem, M: Particle2dMaterial> RenderCommand<P> for DrawParticleInstanced<M> {
    type Param = SRes<ParticleMeta<M>>;
    type ViewQuery = ();
    type ItemQuery = Read<InstanceBuffer>;

    #[inline]
    fn render<'w>(
        _item: &P,
        _view: (),
        instance_buffer: Option<&'w InstanceBuffer>,
        meta: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some(instance_buffer) = instance_buffer else {
            return RenderCommandResult::Failure;
        };

        let particle_meta = meta.into_inner();
        pass.set_index_buffer(
            particle_meta.index_buffer.buffer().unwrap().slice(..),
            0,
            IndexFormat::Uint32,
        );
        pass.set_vertex_buffer(0, instance_buffer.buffer.slice(..));
        pass.draw_indexed(0..6, 0, 0..instance_buffer.length as u32);

        RenderCommandResult::Success
    }
}
