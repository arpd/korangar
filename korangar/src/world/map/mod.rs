#[cfg(feature = "debug")]
use std::collections::HashSet;

use cgmath::{Array, EuclideanSpace, Matrix4, Point3, SquareMatrix, Vector2, Vector3};
use derive_new::new;
use korangar_audio::AudioEngine;
#[cfg(feature = "debug")]
use korangar_debug::profiling::Profiler;
#[cfg(feature = "debug")]
use korangar_interface::windows::PrototypeWindow;
use korangar_util::collision::{Frustum, KDTree, AABB};
#[cfg(feature = "debug")]
use korangar_util::container::SimpleKey;
use korangar_util::container::SimpleSlab;
use korangar_util::create_simple_key;
#[cfg(feature = "debug")]
use option_ext::OptionExt;
#[cfg(feature = "debug")]
use ragnarok_formats::map::MapData;
use ragnarok_formats::map::{EffectSource, LightSettings, LightSource, SoundSource, Tile, TileFlags, WaterSettings};
#[cfg(feature = "debug")]
use ragnarok_formats::transform::Transform;
use ragnarok_packets::ClientTick;
use wgpu::RenderPass;

use super::{Entity, LightSourceExt, Object};
use crate::graphics::{Camera, DeferredRenderer, EntityRenderer, GeometryRenderer, Renderer};
#[cfg(feature = "debug")]
use crate::graphics::{MarkerRenderer, RenderSettings};
#[cfg(feature = "debug")]
use crate::interface::application::InterfaceSettings;
use crate::{
    Buffer, Color, GameFileLoader, IndicatorRenderer, ModelVertex, PickerRenderer, Texture, TextureGroup, TileVertex, WaterVertex,
};

create_simple_key!(ObjectKey, "Key to an object inside the map");

fn average_tile_height(tile: &Tile) -> f32 {
    (tile.upper_left_height + tile.upper_right_height + tile.lower_left_height + tile.lower_right_height) / 4.0
}

// MOVE
fn get_value(day_timer: f32, offset: f32, p: f32) -> f32 {
    let sin = (day_timer + offset).sin();
    sin.abs().powf(2.0 - p) / sin
}

fn get_channels(day_timer: f32, offset: f32, ps: [f32; 3]) -> Vector3<f32> {
    let red = get_value(day_timer, offset, ps[0]);
    let green = get_value(day_timer, offset, ps[1]);
    let blue = get_value(day_timer, offset, ps[2]);
    Vector3::new(red, green, blue)
}

fn color_from_channel(base_color: Color, channels: Vector3<f32>) -> Color {
    Color::rgb_u8(
        (base_color.red * channels.x) as u8,
        (base_color.green * channels.y) as u8,
        (base_color.blue * channels.z) as u8,
    )
}

fn get_ambient_light_color(ambient_color: Color, day_timer: f32) -> Color {
    let sun_offset = 0.0;
    let ambient_channels = (get_channels(day_timer, sun_offset, [0.3, 0.2, 0.2]) * 0.55 + Vector3::from_value(0.65)) * 255.0;
    color_from_channel(ambient_color, ambient_channels)
}

fn get_directional_light_color_intensity(directional_color: Color, intensity: f32, day_timer: f32) -> (Color, f32) {
    let sun_offset = 0.0;
    let moon_offset = std::f32::consts::PI;

    let directional_channels = get_channels(day_timer, sun_offset, [0.8, 0.0, 0.25]) * 255.0;

    if directional_channels.x.is_sign_positive() {
        let directional_color = color_from_channel(directional_color, directional_channels);
        return (directional_color, f32::min(intensity * 1.5, 1.0));
    }

    let directional_channels = get_channels(day_timer, moon_offset, [0.3; 3]) * 255.0;
    let directional_color = color_from_channel(Color::rgb_u8(150, 150, 255), directional_channels);

    (directional_color, f32::min(intensity * 1.5, 1.0))
}

pub fn get_light_direction(day_timer: f32) -> Vector3<f32> {
    let sun_offset = -std::f32::consts::FRAC_PI_2;
    let c = (day_timer + sun_offset).cos();
    let s = (day_timer + sun_offset).sin();

    match c.is_sign_positive() {
        true => Vector3::new(s, c, -0.5),
        false => Vector3::new(s, -c, -0.5),
    }
}

#[cfg(feature = "debug")]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MarkerIdentifier {
    Object(u32),
    LightSource(usize),
    SoundSource(usize),
    EffectSource(usize),
    Particle(usize, usize),
    Entity(usize),
}

#[cfg(feature = "debug")]
impl MarkerIdentifier {
    pub const SIZE: f32 = 1.5;
}

#[derive(new)]
pub struct Map {
    width: usize,
    height: usize,
    water_settings: Option<WaterSettings>,
    light_settings: LightSettings,
    tiles: Vec<Tile>,
    ground_vertex_buffer: Buffer<ModelVertex>,
    water_vertex_buffer: Option<Buffer<WaterVertex>>,
    ground_textures: TextureGroup,
    objects: SimpleSlab<ObjectKey, Object>,
    light_sources: Vec<LightSource>,
    sound_sources: Vec<SoundSource>,
    effect_sources: Vec<EffectSource>,
    tile_picker_vertex_buffer: Buffer<TileVertex>,
    tile_vertex_buffer: Buffer<ModelVertex>,
    object_kdtree: KDTree<ObjectKey, AABB>,
    background_music_track_name: Option<String>,
    #[cfg(feature = "debug")]
    map_data: MapData,
}

impl Map {
    pub fn x_in_bounds(&self, x: usize) -> bool {
        x <= self.width
    }

    pub fn y_in_bounds(&self, y: usize) -> bool {
        y <= self.height
    }

    pub fn get_world_position(&self, position: Vector2<usize>) -> Vector3<f32> {
        let height = average_tile_height(self.get_tile(position));
        Vector3::new(position.x as f32 * 5.0 + 2.5, height, position.y as f32 * 5.0 + 2.5)
    }

    // TODO: Make this private once path finding is properly implemented
    pub fn get_tile(&self, position: Vector2<usize>) -> &Tile {
        &self.tiles[position.x + position.y * self.width]
    }

    pub fn background_music_track_name(&self) -> Option<&str> {
        self.background_music_track_name.as_deref()
    }

    pub fn set_ambient_sound_sources(&self, audio_engine: &AudioEngine<GameFileLoader>) {
        // We increase the range of the ambient sound,
        // so that it can ease better into the world.
        const AMBIENT_SOUND_MULTIPLIER: f32 = 1.5;

        // This is the only correct place to clear the ambient sound.
        audio_engine.clear_ambient_sound();

        for sound in self.sound_sources.iter() {
            let sound_effect_key = audio_engine.load(&sound.sound_file);

            audio_engine.add_ambient_sound(
                sound_effect_key,
                Point3::from_vec(sound.position),
                sound.range * AMBIENT_SOUND_MULTIPLIER,
                sound.volume,
                sound.cycle,
            );
        }

        audio_engine.prepare_ambient_sound_world();
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_ground<T>(
        &self,
        render_target: &mut T::Target,
        render_pass: &mut RenderPass,
        renderer: &T,
        camera: &dyn Camera,
        time: f32,
    ) where
        T: Renderer + GeometryRenderer,
    {
        renderer.render_geometry(
            render_target,
            render_pass,
            camera,
            &self.ground_vertex_buffer,
            &self.ground_textures,
            Matrix4::identity(),
            time,
        );
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_objects<T>(
        &self,
        render_target: &mut T::Target,
        render_pass: &mut RenderPass,
        renderer: &T,
        camera: &dyn Camera,
        client_tick: ClientTick,
        time: f32,
        frustum_query_result: &mut Vec<ObjectKey>,
        #[cfg(feature = "debug")] frustum_culling: bool,
    ) where
        T: Renderer + GeometryRenderer,
    {
        #[cfg(feature = "debug")]
        let culling_measurement = Profiler::start_measurement("frustum culling");

        let (view_matrix, projection_matrix) = camera.view_projection_matrices();
        let frustum = Frustum::new(projection_matrix * view_matrix);

        frustum_query_result.clear();
        self.object_kdtree.query(&frustum, frustum_query_result);

        #[cfg(feature = "debug")]
        culling_measurement.stop();

        #[cfg(feature = "debug")]
        if !frustum_culling {
            self.objects.iter().for_each(|(_, object)| {
                object.render_geometry(render_target, render_pass, renderer, camera, client_tick, time);
            });

            return;
        }

        for object_key in frustum_query_result.iter().copied() {
            if let Some(object) = self.objects.get(object_key) {
                object.render_geometry(render_target, render_pass, renderer, camera, client_tick, time);
            }
        }
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_entities<T>(
        &self,
        entities: &[Entity],
        render_target: &mut T::Target,
        render_pass: &mut RenderPass,
        renderer: &T,
        camera: &dyn Camera,
        include_self: bool,
    ) where
        T: Renderer + EntityRenderer,
    {
        entities
            .iter()
            .skip(!include_self as usize)
            .for_each(|entity| entity.render(render_target, render_pass, renderer, camera));
    }

    #[cfg(feature = "debug")]
    #[korangar_debug::profile]
    pub fn render_bounding(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
        player_camera: &dyn Camera,
        frustum_culling: bool,
        frustum_query_result: &mut Vec<ObjectKey>,
    ) {
        let (view_matrix, projection_matrix) = player_camera.view_projection_matrices();
        let frustum = Frustum::new(projection_matrix * view_matrix);

        frustum_query_result.clear();
        self.object_kdtree.query(&frustum, frustum_query_result);

        let mut intersection_set = HashSet::new();
        frustum_query_result.iter().for_each(|&key| {
            intersection_set.insert(key);
        });

        self.objects.iter().for_each(|(object_key, object)| {
            let bounding_box_matrix = object.get_bounding_box_matrix();
            let bounding_box = AABB::from_transformation_matrix(bounding_box_matrix);
            let intersects = intersection_set.contains(&object_key);

            let color = match !frustum_culling || intersects {
                true => Color::rgb_u8(255, 255, 0),
                false => Color::rgb_u8(255, 0, 255),
            };

            let offset = bounding_box.size().y / 2.0;
            let position = bounding_box.center() - Vector3::new(0.0, offset, 0.0);
            let transform = Transform::position(position.to_vec());

            renderer.render_bounding_box(render_target, render_pass, camera, &transform, &bounding_box, color);
        });
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_tiles(
        &self,
        render_target: &mut <PickerRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &PickerRenderer,
        camera: &dyn Camera,
    ) {
        renderer.render_tiles(render_target, render_pass, camera, &self.tile_picker_vertex_buffer);
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_walk_indicator<T>(
        &self,
        render_target: &mut <T>::Target,
        render_pass: &mut RenderPass,
        renderer: &T,
        camera: &dyn Camera,
        color: Color,
        position: Vector2<usize>,
    ) where
        T: Renderer + IndicatorRenderer,
    {
        const OFFSET: f32 = 1.0;
        const TILE_SIZE: f32 = 5.0;

        let tile = self.get_tile(position);

        if tile.flags.contains(TileFlags::WALKABLE) {
            let base_x = position.x as f32 * TILE_SIZE;
            let base_y = position.y as f32 * TILE_SIZE;

            let upper_left = Vector3::new(base_x, tile.upper_left_height + OFFSET, base_y);
            let upper_right = Vector3::new(base_x + TILE_SIZE, tile.upper_right_height + OFFSET, base_y);
            let lower_left = Vector3::new(base_x, tile.lower_left_height + OFFSET, base_y + TILE_SIZE);
            let lower_right = Vector3::new(base_x + TILE_SIZE, tile.lower_right_height + OFFSET, base_y + TILE_SIZE);

            renderer.render_walk_indicator(
                render_target,
                render_pass,
                camera,
                color,
                upper_left,
                upper_right,
                lower_left,
                lower_right,
            );
        }
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn render_water(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
        day_timer: f32,
    ) {
        if let Some(water_vertex_buffer) = &self.water_vertex_buffer {
            renderer.render_water(render_target, render_pass, camera, water_vertex_buffer, day_timer);
        }
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn ambient_light(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        day_timer: f32,
    ) {
        let ambient_color = get_ambient_light_color(self.light_settings.ambient_color.to_owned().unwrap().into(), day_timer);
        renderer.ambient_light(render_target, render_pass, ambient_color);
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn directional_light(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
        light_texture: &Texture,
        light_matrix: Matrix4<f32>,
        day_timer: f32,
    ) {
        let light_direction = get_light_direction(day_timer);
        let (directional_color, intensity) = get_directional_light_color_intensity(
            self.light_settings.diffuse_color.to_owned().unwrap().into(),
            self.light_settings.light_intensity.unwrap(),
            day_timer,
        );

        renderer.directional_light(
            render_target,
            render_pass,
            camera,
            light_texture,
            light_matrix,
            light_direction,
            directional_color,
            intensity,
        );
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn point_lights(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
    ) {
        self.light_sources
            .iter()
            .for_each(|light_source| light_source.render_light(render_target, render_pass, renderer, camera));
    }

    #[cfg_attr(feature = "debug", korangar_debug::profile)]
    pub fn water_light(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
    ) {
        let water_level = self
            .water_settings
            .as_ref()
            .and_then(|settings| settings.water_level)
            .unwrap_or_default();

        renderer.water_light(render_target, render_pass, camera, water_level);
    }

    #[cfg(feature = "debug")]
    pub fn to_prototype_window(&self) -> &dyn PrototypeWindow<InterfaceSettings> {
        &self.map_data
    }

    #[cfg(feature = "debug")]
    #[korangar_debug::profile]
    pub fn render_overlay_tiles(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
    ) {
        renderer.render_overlay_tiles(render_target, render_pass, camera, &self.tile_vertex_buffer);
    }

    #[cfg(feature = "debug")]
    pub fn resolve_marker<'a>(
        &'a self,
        entities: &'a [Entity],
        marker_identifier: MarkerIdentifier,
    ) -> &dyn PrototypeWindow<InterfaceSettings> {
        match marker_identifier {
            MarkerIdentifier::Object(key) => self.objects.get(ObjectKey::new(key)).unwrap(),
            MarkerIdentifier::LightSource(index) => &self.light_sources[index],
            MarkerIdentifier::SoundSource(index) => &self.sound_sources[index],
            MarkerIdentifier::EffectSource(index) => &self.effect_sources[index],
            MarkerIdentifier::Particle(..) => panic!(),
            // TODO: implement properly
            MarkerIdentifier::Entity(index) => &entities[index],
        }
    }

    #[cfg(feature = "debug")]
    #[korangar_debug::profile]
    pub fn render_markers<T>(
        &self,
        render_target: &mut T::Target,
        render_pass: &mut RenderPass,
        renderer: &T,
        camera: &dyn Camera,
        render_settings: &RenderSettings,
        entities: &[Entity],
        hovered_marker_identifier: Option<MarkerIdentifier>,
    ) where
        T: Renderer + MarkerRenderer,
    {
        use super::SoundSourceExt;
        use crate::EffectSourceExt;

        if render_settings.show_object_markers {
            self.objects.iter().for_each(|(object_key, object)| {
                let marker_identifier = MarkerIdentifier::Object(object_key.key());

                object.render_marker(
                    render_target,
                    render_pass,
                    renderer,
                    camera,
                    marker_identifier,
                    hovered_marker_identifier.contains(&marker_identifier),
                )
            });
        }

        if render_settings.show_light_markers {
            self.light_sources.iter().enumerate().for_each(|(index, light_source)| {
                let marker_identifier = MarkerIdentifier::LightSource(index);

                light_source.render_marker(
                    render_target,
                    render_pass,
                    renderer,
                    camera,
                    marker_identifier,
                    hovered_marker_identifier.contains(&marker_identifier),
                )
            });
        }

        if render_settings.show_sound_markers {
            self.sound_sources.iter().enumerate().for_each(|(index, sound_source)| {
                let marker_identifier = MarkerIdentifier::SoundSource(index);

                sound_source.render_marker(
                    render_target,
                    render_pass,
                    renderer,
                    camera,
                    marker_identifier,
                    hovered_marker_identifier.contains(&marker_identifier),
                )
            });
        }

        if render_settings.show_effect_markers {
            self.effect_sources.iter().enumerate().for_each(|(index, effect_source)| {
                let marker_identifier = MarkerIdentifier::EffectSource(index);

                effect_source.render_marker(
                    render_target,
                    render_pass,
                    renderer,
                    camera,
                    marker_identifier,
                    hovered_marker_identifier.contains(&marker_identifier),
                )
            });
        }

        if render_settings.show_entity_markers {
            entities.iter().enumerate().for_each(|(index, entity)| {
                let marker_identifier = MarkerIdentifier::Entity(index);

                entity.render_marker(
                    render_target,
                    render_pass,
                    renderer,
                    camera,
                    marker_identifier,
                    hovered_marker_identifier.contains(&marker_identifier),
                )
            });
        }
    }

    #[cfg(feature = "debug")]
    #[korangar_debug::profile]
    pub fn render_marker_box(
        &self,
        render_target: &mut <DeferredRenderer as Renderer>::Target,
        render_pass: &mut RenderPass,
        renderer: &DeferredRenderer,
        camera: &dyn Camera,
        marker_identifier: MarkerIdentifier,
    ) {
        match marker_identifier {
            MarkerIdentifier::Object(key) => {
                self.objects
                    .get(ObjectKey::new(key))
                    .unwrap()
                    .render_bounding_box(render_target, render_pass, renderer, camera)
            }
            MarkerIdentifier::LightSource(_index) => {}
            MarkerIdentifier::SoundSource(_index) => {}
            MarkerIdentifier::EffectSource(_index) => {}
            MarkerIdentifier::Particle(_index, _particle_index) => {}
            MarkerIdentifier::Entity(_index) => {}
        }
    }
}
