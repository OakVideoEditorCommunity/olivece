// Oak Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Oak Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! The evaluation seam (C++ `RenderProcessor : NodeTraverser`,
//! flattened): turns node-graph evaluation into render jobs by
//! implementing oaknode's `RenderHooks`. Each C++ `process_*` virtual
//! is one hook method.
//!
//! This pass implements the graph hooks: frame generation runs fully;
//! plugin jobs dispatch through the executor slot oakplugin installs
//! ([`set_plugin_executor`]); footage jobs decode through the oakcodec
//! decoder bridge; shader jobs execute on the shared GPU context
//! ([`oak_core::backend::GpuContext`], falling back to an input pass-
//! through when no adapter is available); color transform jobs apply
//! their OCIO processor (CPU frames convert for real; the GPU
//! color-managed blit is deferred at the backend and passes through);
//! the disk frame-cache payload I/O remains deferred.

use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::shaderfx::{compile_effect, run_effect};
use oak_codec::decoder::{
    CodecStream, Decoder as _, RenderMode, RetrieveAudioStatus, RetrieveVideoParams,
    K_COLOR_RANGE_DEFAULT,
};
use oak_codec::ffmpeg::FFmpegDecoder;
use oak_core::color::ColorProcessor;
use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};
use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::jobs::{ColorTransformJobPayload, FootageJobPayload, ShaderJobPayload};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable};

/// Static mapping of OCIO-based node shaders to the OCIO function they
/// splice at their `%1` marker: (node type id, OCIO function name, source
/// space/role, destination space/role) — the C++ `GenerateProcessor`
/// `ColorTransform` pairs, resolved by the OCIO config at runtime.
///
/// Only the node's *function* (the GPU stub) is requested here; the
/// processor is built against the manager's reference color space
/// (C++ chromakey.cpp `GenerateProcessor` uses `GetReferenceColorSpace`,
/// which `ColorManager` sets to `OCIO::ROLE_SCENE_LINEAR`,
/// colormanager.cpp).
pub const OCIO_SHADER_STUBS: &[(&str, &str, &str, &str)] = &[(
    "org.olivevideoeditor.Olive.chromakey",
    "SceneLinearToCIEXYZ_d65",
    "scene_linear",
    "cie_xyz_d65_interchange",
)];

/// Static mapping of the OCIO grading nodes to the grading-primary style
/// whose dynamic GPU shader they splice at their `%1` marker (C++
/// `oakrender_color_processor_create_grading_primary` with the node's
/// `GRADING_LIN`/`GRADING_LOG` style; the processor is built against the
/// default config at render time).
pub const OCIO_GRADING_STUBS: &[(&str, oak_core::color::GradingStyle)] = &[
    (
        "org.olivevideoeditor.Olive.ociogradingtransformlinear",
        oak_core::color::GradingStyle::Lin,
    ),
    (
        "org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog",
        oak_core::color::GradingStyle::Log,
    ),
];

/// The OCIO GPU function shader for `type_id` (the `%1` stub), or `None`
/// when the node is not OCIO-based or the processor cannot be generated
/// (no default config, or a LUT processor the renderer cannot upload).
pub fn ocio_stub_for(type_id: &str) -> Option<String> {
    let (_, fn_name, from, to) = OCIO_SHADER_STUBS
        .iter()
        .find(|(id, ..)| *id == type_id)
        .copied()?;
    oak_core::color::ocio_function_shader(fn_name, from, to)
}

/// The OCIO grading GPU shader for `type_id` (the `%1` stub), or `None`
/// when the node is not a grading node or no default config is set up.
pub fn grading_stub_for(type_id: &str) -> Option<String> {
    let (_, style) = OCIO_GRADING_STUBS
        .iter()
        .find(|(id, _)| *id == type_id)
        .copied()?;
    oak_core::color::grading_primary_function_shader(style)
}

/// Job specification: the closed set of C++ `*Job` payloads
/// (AcceleratedJob family) as internal evaluation records — jobs no
/// longer travel inside values across module boundaries.
#[derive(Clone, Debug)]
pub enum JobSpec {
    /// Shader job (frag/vert source + params).
    Shader {
        /// Fragment source.
        frag: String,
        /// Vertex source.
        vert: String,
    },
    /// Color transform job.
    ColorTransform {
        /// Processor identity (color::ProcessorCache key).
        processor: u64,
    },
    /// Direct frame generation (CPU nodes).
    Generate,
    /// Disk cache read (C++ CacheJob).
    Cache {
        /// Cache file path.
        path: String,
    },
    /// Footage decode (C++ FootageJob; decode via bridge::codec).
    Footage {
        /// Decoder/stream id.
        decoder_id: String,
        /// Footage filename (M12 P0: the decode path).
        filename: String,
        /// Media stream index.
        stream_index: i32,
    },
    /// Sample generation (C++ SampleJob).
    Sample,
    /// OFX plugin job — executed through the registered plugin executor
    /// ([`set_plugin_executor`]; the oakplugin crate installs its
    /// render driver there, so oakrender never sees OFX types).
    Plugin {
        /// Plugin instance identity (oakplugin instance registry key).
        instance: u64,
        /// Request time in seconds (C++ `PluginJob` time).
        time: f64,
        /// Clip name the main source texture arrives on (C++
        /// `node->get_effect_input_id()`).
        effect_input_id: Option<String>,
        /// Clip input textures by clip name (multi-input plugins).
        inputs: Vec<(String, Texture)>,
        /// Param overrides: input id -> node value (the tagged values
        /// captured at evaluation time).
        values: Vec<(String, NodeValue)>,
    },
}

/// The hooks implementation handed to the oaknode traverser.
pub struct RenderEvalHooks {
    /// Cache usage toggle (C++ use_cache).
    pub use_cache: bool,
    /// Active ticket identity (for cancellation polling).
    pub ticket: Option<crate::ticket::TicketId>,
    /// Forced output size for resolved footage jobs; `None` decodes at
    /// the media's native size (C++ `RenderProcessor` requests the
    /// texture at the output resolution). The graph-sequence driver sets
    /// this to the sequence frame size.
    pub frame_size: Option<(i32, i32)>,
    /// The sequence's square-pixel resolution (C++ the `NodeGlobals`
    /// square resolution the shape/polygon generators insert as
    /// `resolution_in` at job-build time). Generator jobs anchor their
    /// pixel-size params to this — NOT to the render-target size — so a
    /// proxy-resolution playback render draws them at the same relative
    /// size as a paused full-res frame. `None` (non-sequence renders)
    /// falls back to the render-target size.
    pub sequence_size: Option<(i32, i32)>,

}

// ---------------------------------------------------------------------------
// Plugin job executor (dependency inversion seam)
// ---------------------------------------------------------------------------
//
// oakrender sits BELOW oakplugin in the dependency graph (oakplugin
// depends on oakrender for the texture value types), so the plugin job
// execution cannot be a direct call. The oakplugin crate installs its
// render driver here at init; `process_plugin_job` dispatches through
// the slot. Without an executor, plugin jobs fail explainably (the
// pre-wiring behavior).

/// Plugin job request handed to the registered executor (the C++
/// `process_plugin_job(texture, destination, node)` inputs flattened).
pub struct PluginJobRequest<'a> {
    /// The job spec ([`JobSpec::Plugin`] guaranteed by the caller).
    pub spec: &'a JobSpec,
    /// The input texture the job runs against.
    pub src: Texture,
}

/// Plugin executor: runs one plugin job and returns the output
/// texture. Implemented by the oakplugin crate on top of its render
/// driver.
pub type PluginExecutor = dyn Fn(&PluginJobRequest<'_>) -> Result<Texture> + Send + Sync;

static PLUGIN_EXECUTOR: std::sync::OnceLock<std::sync::Mutex<Option<Arc<PluginExecutor>>>> =
    std::sync::OnceLock::new();

fn executor_slot() -> &'static std::sync::Mutex<Option<Arc<PluginExecutor>>> {
    PLUGIN_EXECUTOR.get_or_init(|| std::sync::Mutex::new(None))
}

/// Install the plugin job executor (oakplugin registration point;
/// `None` clears it).
pub fn set_plugin_executor(executor: Option<Arc<PluginExecutor>>) {
    *executor_slot().lock().unwrap_or_else(|e| e.into_inner()) = executor;
}

/// The installed plugin executor, if any.
pub fn plugin_executor() -> Option<Arc<PluginExecutor>> {
    executor_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Plugin instance factory: resolves an OFX plugin identifier to a live
/// instance-registry id, creating (and caching) the instance lazily.
/// Implemented by the oakplugin crate — the montage effect path carries
/// plugin identifiers, not instance ids (the montage is resolved from
/// the timeline in the main process; the instance lives in whichever
/// process renders the frame).
pub type PluginInstanceFactory = dyn Fn(&str) -> Option<u64> + Send + Sync;

static PLUGIN_INSTANCE_FACTORY: std::sync::OnceLock<
    std::sync::Mutex<Option<Arc<PluginInstanceFactory>>>,
> = std::sync::OnceLock::new();

fn instance_factory_slot() -> &'static std::sync::Mutex<Option<Arc<PluginInstanceFactory>>> {
    PLUGIN_INSTANCE_FACTORY.get_or_init(|| std::sync::Mutex::new(None))
}

/// Install the plugin instance factory (oakplugin registration point;
/// `None` clears it).
pub fn set_plugin_instance_factory(factory: Option<Arc<PluginInstanceFactory>>) {
    *instance_factory_slot().lock().unwrap_or_else(|e| e.into_inner()) = factory;
}

/// The installed plugin instance factory, if any.
pub fn plugin_instance_factory() -> Option<Arc<PluginInstanceFactory>> {
    instance_factory_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Box a resolved texture into the table's texture channel (the render
/// seam's output is the one place a texture legitimately travels as a
/// refcounted handle — [`oak_node::handle::get_checked`] probes against
/// `Texture` stay type-checked).
fn texture_value(texture: Texture) -> NodeValue {
    NodeValue::Texture(oak_node::handle::make_owned(texture))
}

/// The failure marker frame: solid magenta (1, 0, 1, 1) F32 RGBA —
/// the C++ plugin renderer paints failed plugin output purple so a
/// broken plugin is visible instead of silently black.
fn purple_frame(time: Rational, size: (i32, i32)) -> Texture {
    let (w, h) = (size.0.max(1), size.1.max(1));
    let mut frame = match generate_frame(time, (w, h), PixelFormat::F32) {
        Ok(f) => f,
        Err(_) => return Texture::dummy(),
    };
    for pixel in frame.data.chunks_exact_mut(16) {
        for (i, v) in [1.0f32, 0.0, 1.0, 1.0].iter().enumerate() {
            pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    Texture::wrap_frame(frame)
}

impl RenderEvalHooks {
    pub fn new() -> Self {
        Self {
            use_cache: false,
            ticket: None,
            frame_size: None,
            sequence_size: None,
        }
    }

    /// C++ process_color_transform: apply the job's OCIO processor to the
    /// input texture. A CPU frame converts in place (the real OCIO
    /// `convert_frame`); a GPU input passes through with a one-time log —
    /// the color-managed GPU blit is deferred at the backend
    /// (`GpuContext::blit` rejects a processor). An invalid processor
    /// passes the input through unchanged (C++ creates processors
    /// non-fatally); a non-texture input is `Error::Invalid`.
    fn process_color_transform_job(
        &mut self,
        payload: &ColorTransformJobPayload,
    ) -> Result<Texture> {
        let NodeValue::Texture(handle) = &payload.input else {
            return Err(Error::Invalid);
        };
        if handle.ctx.is_null() {
            return Err(Error::Invalid);
        }
        let tex = (unsafe { oak_node::handle::get_checked::<Texture>(handle) })
            .cloned()
            .ok_or(Error::Invalid)?;
        if !payload.color_processor.is_valid() {
            // No valid processor (no default config, LUT load failure):
            // pass the input through, mirroring the C++ non-fatal
            // processor creation.
            return Ok(tex);
        }
        let mut tex = tex;
        if let Texture::Cpu(frame) = &mut tex {
            // The CPU leg converts in place (real OCIO `convert_frame`).
            payload.color_processor.convert_frame(frame)?;
            return Ok(tex);
        }
        // GPU leg: deferred at the backend — pass through, logged once
        // per processor.
        let key = format!(
            "colortransform:gpu:{}",
            payload.color_processor.cache_id()
        );
        if unsupported_warned().insert(key) {
            eprintln!(
                "color transform on a GPU texture passes through unchanged: \
                 color-managed GPU blit deferred at the backend"
            );
        }
        Ok(tex)
    }

    /// C++ process_frame_generation: fill the destination with a generated
    /// F32 frame (transparent black for now).
    fn process_frame_generation(
        &mut self,
        destination: &mut Texture,
        time: Rational,
    ) -> Result<()> {
        let Texture::Cpu(frame) = destination else {
            return Err(Error::Failed(
                "frame generation on GPU deferred: CPU path only this pass".into(),
            ));
        };
        let generated = generate_frame(time, (frame.width, frame.height), frame.format)?;
        frame.data = generated.data;
        frame.timestamp = time;
        Ok(())
    }

    /// C++ process_plugin_job: dispatch through the installed plugin
    /// executor (the oakplugin render driver; dependency inversion). A
    /// missing executor or a failed render yields a purple failure frame
    /// instead of aborting the graph, matching pluginjob.cpp's fallback.
    fn process_plugin_job(&mut self, src: Texture, spec: &JobSpec) -> Result<Texture> {
        let JobSpec::Plugin {
            instance,
            time,
            effect_input_id,
            inputs,
            values,
        } = spec
        else {
            return Err(Error::Invalid);
        };
        let size = src.size();
        let Some(executor) = plugin_executor() else {
            return Ok(purple_frame(Rational::from_double(*time), size));
        };
        let _ = (instance, effect_input_id, inputs, values);
        match executor(&PluginJobRequest { spec, src }) {
            Ok(texture) => Ok(texture),
            Err(err) => {
                eprintln!("plugin instance {instance} render at t={time}s failed: {err:#}");
                Ok(purple_frame(Rational::from_double(*time), size))
            }
        }
    }

    /// C++ process_video_cache_job.
    fn process_video_cache_job(&mut self, spec: &JobSpec) -> Result<Texture> {
        let JobSpec::Cache { path } = spec else {
            return Err(Error::Invalid);
        };
        let _ = path;
        Err(Error::Failed(
            "disk frame-cache load deferred: oakcodec EXR/JPEG decode pending".into(),
        ))
    }

    /// Executes the deferred plugin payloads a [`oak_node::nodes::plugin::PluginNode`]
    /// pushed into its output table (C++ JobEnginePlugin processing in
    /// jobmanager.cpp): unwraps each [`oak_node::nodes::plugin::PluginJobPayload`]
    /// box, splits it into input textures and tagged param values, and
    /// replaces the box with the rendered texture.
    fn resolve_plugin_jobs(&mut self, table: &mut NodeValueTable) {
        for (_, value, _) in table.rows_mut() {
            let NodeValue::Texture(handle) = value else {
                continue;
            };
            if handle.ctx.is_null() {
                continue;
            }
            let payload = unsafe {
                oak_node::handle::get_checked::<oak_node::nodes::plugin::PluginJobPayload>(handle)
            }
                .cloned();
            let Some(payload) = payload else {
                // A genuine texture box (e.g. a source node's frame):
                // not a plugin job, leave it alone.
                continue;
            };

            let mut inputs: Vec<(String, Texture)> = Vec::new();
            let mut values: Vec<(String, NodeValue)> = Vec::new();
            for (key, v) in payload.values.iter() {
                match v {
                    NodeValue::Texture(h) if !h.ctx.is_null() => {
                        match unsafe { oak_node::handle::get_checked::<Texture>(h) }.cloned() {
                            Some(texture) => inputs.push((key.clone(), texture)),
                            None => eprintln!("plugin job input '{key}' is not a texture box"),
                        }
                    }
                    NodeValue::Texture(_) | NodeValue::None => {}
                    other => values.push((key.clone(), other.clone())),
                }
            }

            // Fallback order mirrors pluginrenderer.cpp's effect input
            // resolution: the declared effect input, else the first
            // available clip texture.
            let effect_src = if payload.effect_input_id.is_empty() {
                None
            } else {
                inputs
                    .iter()
                    .find(|(key, _)| key == &payload.effect_input_id)
                    .map(|(_, t)| t.clone())
            };
            let src = effect_src
                .or_else(|| inputs.first().map(|(_, t)| t.clone()))
                .unwrap_or_else(Texture::dummy);

            let spec = JobSpec::Plugin {
                instance: payload.instance.0,
                time: payload.time.to_f64(),
                effect_input_id: if payload.effect_input_id.is_empty() {
                    None
                } else {
                    Some(payload.effect_input_id.clone())
                },
                inputs,
                values,
            };
            match self.process_plugin_job(src, &spec) {
                Ok(texture) => {
                    *value = NodeValue::Texture(oak_node::handle::make_owned(texture));
                }
                Err(err) => {
                    eprintln!("plugin job resolve failed: {err:#}");
                }
            }
        }
    }

    /// Resolve the footage payloads a footage node pushed into its output
    /// table (C++ FootageJob processing in jobmanager.cpp): decodes each
    /// boxed [`FootageJobPayload`] at its request time and replaces the
    /// box with the resulting texture. Genuine textures pass through.
    fn resolve_footage_jobs(&mut self, table: &mut NodeValueTable) {
        let size = self.frame_size.unwrap_or((0, 0));
        for (_, value, _) in table.rows_mut() {
            let NodeValue::Texture(handle) = value else {
                continue;
            };
            if handle.ctx.is_null() {
                continue;
            }
            let payload = unsafe {
                oak_node::handle::get_checked::<FootageJobPayload>(handle)
            }
                .cloned();
            let Some(payload) = payload else {
                continue;
            };
            match render_footage_frame(
                &payload.filename,
                payload.stream_index,
                payload.time,
                size,
                PixelFormat::F32,
            ) {
                Ok(texture) => {
                    *value = NodeValue::Texture(oak_node::handle::make_owned(texture));
                }
                Err(err) => {
                    eprintln!("footage job decode failed: {err:#}");
                }
            }
        }
    }

    /// Resolve the shader payloads an effect node pushed into its output
    /// table (C++ ShaderJob processing in jobmanager.cpp): execute each
    /// boxed [`ShaderJobPayload`] on the shared GPU context and replace
    /// the box with the result texture. Failed or un-runnable jobs fall
    /// back to the effect input texture from the params row (a pass-
    /// through — C++ leaves the failed shader's output as its input);
    /// a missing input resolves to `NodeValue::None`.
    fn resolve_shader_jobs(&mut self, table: &mut NodeValueTable) {
        // Collect the boxes up front: replacing a row while iterating
        // `rows_mut` would alias the table.
        let jobs: Vec<(usize, ShaderJobPayload)> = table
            .rows_mut()
            .iter_mut()
            .enumerate()
            .filter_map(|(i, (_, value, _))| {
                let NodeValue::Texture(handle) = value else {
                    return None;
                };
                if handle.ctx.is_null() {
                    return None;
                }
                let payload = unsafe {
                    oak_node::handle::get_checked::<ShaderJobPayload>(handle)
                }
                    .cloned();
                payload.map(|p| (i, p))
            })
            .collect();
        for (i, payload) in jobs {
            let resolved = match self.process_shader_job(&payload) {
                Some(texture) => NodeValue::Texture(oak_node::handle::make_owned(texture)),
                None => payload
                    .params
                    .get(&payload.effect_input)
                    .cloned()
                    .unwrap_or(NodeValue::None),
            };
            table.rows_mut()[i].1 = resolved;
        }
    }

    /// Resolve the color-transform payloads an OCIO node pushed into its
    /// output table (C++ ColorTransformJob processing in jobmanager.cpp):
    /// apply each boxed [`ColorTransformJobPayload`]'s processor to its
    /// input texture and replace the box with the result. Failures fall
    /// back to the job's input texture (a pass-through — the C++ renderer
    /// leaves the failed transform's output as its input); genuine
    /// textures pass through.
    fn resolve_color_transform_jobs(&mut self, table: &mut NodeValueTable) {
        // Collect the boxes up front: replacing a row while iterating
        // `rows_mut` would alias the table.
        let jobs: Vec<(usize, ColorTransformJobPayload)> = table
            .rows_mut()
            .iter_mut()
            .enumerate()
            .filter_map(|(i, (_, value, _))| {
                let NodeValue::Texture(handle) = value else {
                    return None;
                };
                if handle.ctx.is_null() {
                    return None;
                }
                let payload = unsafe {
                    oak_node::handle::get_checked::<ColorTransformJobPayload>(handle)
                }
                .cloned();
                payload.map(|p| (i, p))
            })
            .collect();
        for (i, payload) in jobs {
            let resolved = match self.process_color_transform_job(&payload) {
                Ok(texture) => NodeValue::Texture(oak_node::handle::make_owned(texture)),
                Err(err) => {
                    eprintln!("color transform job failed: {err:#}");
                    payload.input.clone()
                }
            };
            table.rows_mut()[i].1 = resolved;
        }
    }

    /// Execute one shader payload (C++ process_shader run by the render
    /// worker): compile the emitting behavior's fragment shader on the
    /// shared GPU context and run the requested iterations. **Every**
    /// texture-typed param is bound by its input id (C++ binds all
    /// sampler inputs — merge's base/blend, the keyers' garbage/core
    /// mattes, opacity's texture modulation); a param boxing a nested
    /// shader payload resolves recursively first (the C++
    /// AcceleratedJob chain — generator-over-base `mrg` jobs). CPU
    /// frames upload into scratch textures; the pass size comes from
    /// the effect input's texture (else the first bound texture, else
    /// the hook's frame size, else 1x1). `None` when the job cannot run
    /// (no GPU context, unknown node type, missing shader, or a
    /// compile/upload/run failure) — the caller then falls back to the
    /// effect input texture.
    fn process_shader_job(&self, payload: &ShaderJobPayload) -> Option<Texture> {
        self.process_shader_job_depth(payload, 0)
    }

    /// [`Self::process_shader_job`] with a recursion guard for nested
    /// payloads (generator-over-base chains nest at most 2 deep).
    fn process_shader_job_depth(&self, payload: &ShaderJobPayload, depth: u32) -> Option<Texture> {
        /// Nested-payload recursion ceiling (defensive; real graphs nest
        /// a generator job inside a merge job and stop there).
        const MAX_JOB_DEPTH: u32 = 8;

        // One log line per shader per process instead of one per frame.
        let warn = |reason: &str| {
            let key = format!("shader:{}:{}", payload.type_id, payload.shader_id);
            if unsupported_warned().insert(key) {
                eprintln!(
                    "shader job \"{}\" (shader \"{}\") failed: {reason}",
                    payload.type_id, payload.shader_id
                );
            }
        };

        let Some(ctx) = oak_core::backend::GpuContext::shared() else {
            warn("no GPU context");
            return None;
        };

        // The emitting node behavior: the type id selects the fragment
        // source (C++ `node->get_shader_code(shader_id)`).
        let Some((_, behavior)) =
            oak_node::factory::Factory::global().create_any(&payload.type_id)
        else {
            warn("unknown node type");
            return None;
        };

        // OCIO-based nodes splice the auto-generated OCIO function into
        // their `%1` marker (C++ `GetShaderCode({shader_id, stub})`, the
        // stub built in colormanagement.cpp `GetColorContext`). A stub
        // that cannot be generated — no default config, or a LUT
        // processor with no upload path — falls back to the effect input
        // pass-through.
        let ocio_entry = OCIO_SHADER_STUBS
            .iter()
            .find(|(id, ..)| *id == payload.type_id)
            .copied();
        let grading_entry = OCIO_GRADING_STUBS
            .iter()
            .find(|(id, _)| *id == payload.type_id)
            .copied();
        let glsl = match (grading_entry, ocio_entry) {
            (Some((_, style)), _) => {
                let Some(stub) = oak_core::color::grading_primary_function_shader(style) else {
                    return None;
                };
                match behavior.shader_code(&stub) {
                    Some(glsl) => glsl,
                    None => {
                        warn("shader not found");
                        return None;
                    }
                }
            }
            (None, Some((_, fn_name, from, to))) => {
                let Some(stub) = oak_core::color::ocio_function_shader(fn_name, from, to) else {
                    return None;
                };
                match behavior.shader_code(&stub) {
                    Some(glsl) => glsl,
                    None => {
                        warn("shader not found");
                        return None;
                    }
                }
            }
            (None, None) => match behavior.shader_code(&payload.shader_id) {
                Some(glsl) => glsl,
                None => {
                    warn("shader not found");
                    return None;
                }
            },
        };

        // Pipeline cache key: the type id plus the shader-variant id (the
        // OCIO stub text folds in too, so a config change recompiles
        // instead of reusing a stale variant).
        let spliced_ocio = grading_entry.is_some() || ocio_entry.is_some();
        let key = if spliced_ocio {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&glsl, &mut h);
            format!(
                "{}:{}:ocio:{}",
                payload.type_id,
                payload.shader_id,
                std::hash::Hasher::finish(&h)
            )
        } else {
            format!("{}:{}", payload.type_id, payload.shader_id)
        };
        let compiled = match compile_effect(&ctx, &key, &glsl, ctx.is_filterable()) {
            Ok(effect) => effect,
            Err(err) => {
                warn(&format!("compile failed: {err:#}"));
                return None;
            }
        };

        // Bind every texture-typed param by name: genuine texture boxes
        // bind directly (CPU frames upload into scratch first); nested
        // shader payloads (the generator layer of an `mrg` job) resolve
        // recursively. `scratch` holds the upload tokens created here;
        // `keepalive` holds the cloned `Texture`s — a `Texture::Gpu`
        // clone destroys its token on drop, so the clones must outlive
        // the pass. Both are released when the job finishes (input-token
        // destruction at job end matches the historical semantics).
        let mut inputs: Vec<(String, u64)> = Vec::new();
        let mut scratch: Vec<u64> = Vec::new();
        let mut keepalive: Vec<Texture> = Vec::new();
        let mut size: Option<(i32, i32)> = None;
        let bind = |key: &str,
                        value: &NodeValue,
                        inputs: &mut Vec<(String, u64)>,
                        scratch: &mut Vec<u64>,
                        keepalive: &mut Vec<Texture>,
                        size: &mut Option<(i32, i32)>|
         -> Option<()> {
            let NodeValue::Texture(handle) = value else {
                return None;
            };
            if handle.ctx.is_null() {
                return None;
            }
            let tex = (unsafe { oak_node::handle::get_checked::<Texture>(handle) })
                .cloned()
                .or_else(|| {
                    if depth >= MAX_JOB_DEPTH {
                        return None;
                    }
                    let nested = (unsafe {
                        oak_node::handle::get_checked::<ShaderJobPayload>(handle)
                    })
                    .cloned()?;
                    Some(self.process_shader_job_depth(&nested, depth + 1)?)
                });
            let tex = tex?;
            let (token, tex_size) = match &tex {
                Texture::Gpu {
                    token,
                    width,
                    height,
                    ..
                } => (*token, (*width, *height)),
                Texture::Cpu(frame) => {
                    let token = match ctx.create_texture(frame.width, frame.height) {
                        Ok(t) => t,
                        Err(err) => {
                            warn(&format!("input texture: {err:#}"));
                            return None;
                        }
                    };
                    if let Err(err) = ctx.upload(token, frame) {
                        ctx.destroy_texture(token);
                        warn(&format!("input upload failed: {err:#}"));
                        return None;
                    }
                    scratch.push(token);
                    (token, (frame.width, frame.height))
                }
            };
            // The pass size follows the effect input's texture (C++ the
            // job's video params = the main input size); any other bound
            // texture sets it only when no effect input was seen.
            if size.is_none() || key == payload.effect_input {
                *size = Some(tex_size);
            }
            inputs.push((key.to_string(), token));
            keepalive.push(tex);
            Some(())
        };

        // The effect input binds first: `run_effect` falls back to
        // `inputs.first()` for the shader's first declared sampler.
        let effect_value = payload.params.get(&payload.effect_input).cloned();
        if let Some(value) = &effect_value {
            bind(
                &payload.effect_input,
                value,
                &mut inputs,
                &mut scratch,
                &mut keepalive,
                &mut size,
            );
        }
        for (key, value) in &payload.params {
            if key == &payload.effect_input {
                continue;
            }
            bind(key, value, &mut inputs, &mut scratch, &mut keepalive, &mut size);
        }
        // Generators bind no texture: render at the requested frame size
        // (the graph driver sets it to the sequence size); 1x1 only when
        // nobody knows better.
        let size = size.or(self.frame_size).unwrap_or((1, 1));

        // `resolution_in` anchors to the sequence square resolution, not
        // the render target (C++ inserts the NodeGlobals square
        // resolution into the job at build time): the node params it
        // denormalizes (shape size/pos, transform offsets, corner pin
        // points, drop shadow distance) are all sequence-pixel values,
        // so a proxy-size playback render must resolve them against the
        // same resolution as a paused full-res frame, or the effect
        // visibly changes size whenever the transport stops. Pre-filling
        // the row wins over `run_effect`'s frame-size auto-fill; a node
        // that inserted its own `resolution_in` keeps it.
        let mut sequence_row;
        let params = if self.sequence_size.is_some()
            && !payload.params.contains_key("resolution_in")
            && compiled
                .translated
                .uniforms
                .iter()
                .any(|u| u.name == "resolution_in")
        {
            let (w, h) = self.sequence_size.unwrap();
            sequence_row = payload.params.clone();
            sequence_row.insert(
                "resolution_in".to_string(),
                NodeValue::Vec2([w as f64, h as f64]),
            );
            &sequence_row
        } else {
            &payload.params
        };

        let dst = match ctx.create_texture(size.0.max(1), size.1.max(1)) {
            Ok(t) => t,
            Err(err) => {
                for t in &scratch {
                    ctx.destroy_texture(*t);
                }
                warn(&format!("output texture: {err:#}"));
                return None;
            }
        };

        let result = run_effect(
            &ctx,
            &compiled,
            params,
            &inputs,
            dst,
            size,
            payload.iterations.max(1) as u32,
            if payload.iterative_input.is_empty() {
                None
            } else {
                Some(payload.iterative_input.as_str())
            },
        );
        for t in &scratch {
            ctx.destroy_texture(*t);
        }
        match result {
            Ok(()) => Some(Texture::Gpu {
                token: dst,
                backend: ctx.kind(),
                width: size.0.max(1),
                height: size.1.max(1),
                format: PixelFormat::F32,
                ctx: ctx.clone(),
            }),
            Err(err) => {
                ctx.destroy_texture(dst);
                warn(&format!("run failed: {err:#}"));
                None
            }
        }
    }
}
impl oak_node::traverser::RenderHooks for RenderEvalHooks {
    fn use_cache(&self) -> bool {
        self.use_cache
    }

    fn is_cancelled(&self) -> bool {
        // TODO(phase-6b): poll the ticket's cancellation flag here so
        // long plugin renders can be interrupted.
        false
    }

    fn resolve(
        &mut self,
        node: oak_node::id::NodeId,
        _row: &NodeValueRow,
        table: &mut NodeValueTable,
    ) {
        let _ = node;
        self.resolve_plugin_jobs(table);
        self.resolve_footage_jobs(table);
        self.resolve_shader_jobs(table);
        self.resolve_color_transform_jobs(table);
    }
}

impl Default for RenderEvalHooks {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate the pipeline's canonical frame: F32 RGBA, transparent black,
/// with the given timestamp (the CPU-backend producer for video tickets).
pub fn generate_frame(time: Rational, size: (i32, i32), format: PixelFormat) -> Result<Frame> {
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return Err(Error::Invalid);
    }
    let mut frame = Frame::new();
    let mut pod = VideoParamsPod::default();
    pod.width = w;
    pod.height = h;
    pod.format = format as i32;
    frame.set_video_params(pod);
    frame.timestamp = time;
    if !frame.allocate() {
        return Err(Error::NoMem);
    }
    Ok(frame)
}

/// The manager-installed ticket producer: render the frame the ticket
/// asks for (F32 pipeline frame). This is the CPU-backend render path.
///
/// M12 P0 routing: a sequence montage (list of clips) is composited
/// topmost-last; a single-footage ticket decodes one stream; otherwise
/// the pipeline frame is generated.
pub fn render_produced_frame(
    time: Rational,
    params: &crate::ticket::VideoTicketParams,
) -> Result<Texture> {
    let (w, h) = params.render_size();
    let format = params.force_format.unwrap_or(PixelFormat::F32);

    if !params.montage.is_empty() {
        let r = render_montage_frame(time, params, (w, h), format);
        return r;
    }
    if let Some((filename, stream_index)) = &params.footage {
        return render_footage_frame(filename, *stream_index, time, (w, h), format);
    }

    let frame = generate_frame(time, (w, h), format)?;
    Ok(Texture::wrap_frame(frame))
}

// ---------------------------------------------------------------------------
// Footage decode (M12 P0): the oakcodec bridge
// ---------------------------------------------------------------------------

/// Process-wide open decoder sessions, keyed by (filename, stream).
/// Sessions are mutex-serialized inside the oakcodec box, so sharing
/// one handle across worker threads is safe. The value carries an LRU
/// tick: the map is capped ([`MAX_CACHED_DECODERS`]) because every
/// session pins an FFmpeg context plus up to two native decoded frames
/// (~50 MB at 4K) — before the cap, scrubbing a footage bin grew the
/// map without bound.
static DECODERS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<(String, i32), (Arc<dyn oak_codec::decoder::Decoder>, u64)>,
    >,
> = std::sync::OnceLock::new();

/// Cap on cached decoder sessions per process (LRU beyond this). 16
/// covers heavy multi-clip montages without reopen thrash; eviction only
/// drops the map entry — an in-flight render keeps its Arc alive and the
/// session dies with the last reference (Drop releases FFmpeg).
///
/// Eviction is hardware-first: hardware sessions pin GPU memory (each
/// NVDEC decoder holds a surface pool — ~100 MB at 4K), so a full cache
/// with live hardware sessions can exhaust the GPU's video memory and
/// make the NEXT decoder open fail with `cuvidCreateDecoder` OOM (the
/// "4K 切换后大量 CUDA_ERROR_OUT_OF_MEMORY 报错" log flood). Software
/// sessions (system RAM only) are evicted only when nothing else is
/// available.
const MAX_CACHED_DECODERS: usize = 6;

/// LRU tick source for [`DECODERS`].
static DECODER_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Cap on decoded FRAMES cached per process (release builds of the graph
/// renderer re-decode every frame the pre-render window pulls — each is a
/// seek+decode on the shared decoder session, which serializes the graph
/// path behind the montage baseline. A small frame LRU lets playback pull
/// the forward window from cache instead of thrashing the decoder: the
/// window is sequential, so a short FIFO of recently decoded frames hits
/// on every pre-render restart and on repeat plays).
const MAX_CACHED_FRAMES: usize = 24;

/// Decoded-frame LRU: `(filename, stream, time, w, h)` -> F32 CPU frame.
/// Frame data is the expensive part (a 1080p frame ≈ 31 MB); the decoder
/// session cache alone still re-decodes every `render_footage_frame`.
/// The size is part of the key: the same media at a different target
/// resolution is a different frame (an interleaved source-monitor/proxy
/// request must not reuse a wrongly-sized pixel buffer).
static DECODED_FRAMES: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<(String, i32, (i64, i64), i32, i32), (Frame, u64)>,
    >,
> = std::sync::OnceLock::new();

fn decoded_frames() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<(String, i32, (i64, i64), i32, i32), (Frame, u64)>,
> {
    DECODED_FRAMES
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Time-key rational (the frame-LRU's deterministic key half).
fn time_key(t: &Rational) -> (i64, i64) {
    (t.numerator(), t.denominator())
}

fn decoders() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<(String, i32), (Arc<dyn oak_codec::decoder::Decoder>, u64)>,
> {
    DECODERS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Pick the victim to evict under the LRU cap: the least-recently-used
/// HARDWARE session when one exists (freeing GPU video memory first,
/// which is the scarce resource), otherwise the least-recently-used
/// session of any kind.
fn eviction_victim(
    cache: &std::collections::HashMap<
        (String, i32),
        (Arc<dyn oak_codec::decoder::Decoder>, u64),
    >,
) -> Option<(String, i32)> {
    cache
        .iter()
        .filter(|(_, (decoder, _))| decoder.hardware_decoding())
        .min_by_key(|(_, (_, t))| *t)
        .map(|(k, _)| k.clone())
        .or_else(|| {
            cache
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
        })
}

/// Open (or reuse) the decoder session for `(filename, stream_index)`.
fn open_decoder(filename: &str, stream_index: i32) -> Result<Arc<dyn oak_codec::decoder::Decoder>> {
    let key = (filename.to_string(), stream_index);
    let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if std::env::var_os("OAK_PERF").is_some() {
        eprintln!("[open] {:?} s{} (request)", filename, stream_index);
    }
    {
        let mut cache = decoders();
        if let Some((d, t)) = cache.get_mut(&key) {
            *t = tick;
            if std::env::var_os("OAK_PERF").is_some() {
                eprintln!("[open] {:?} s{} CACHED", filename, stream_index);
            }
            return Ok(d.clone());
        }
    }
    // Hardware-session budget is now handled by the LRU cap below (and the
    // GPU-vram worker-count policy): the "evict the ONLY hardware session
    // before every new open" guard below was eviction-on-every-frame — a
    // live session was dropped before the NEXT request for the same file
    // could hit it, so every frame re-opened the decoder (~0.5 s each) and
    // playback could never keep up (the [open] (request) log flood without
    // a single CACHED hit).
    let decoder: Arc<dyn oak_codec::decoder::Decoder> = Arc::new(FFmpegDecoder::new());
    let stream = CodecStream::with_block(filename.to_string(), stream_index, None);
    decoder
        .open(&stream)
        .map_err(|e| Error::Failed(format!("footage decode open: {e:?}")))?;
    let mut cache = decoders();
    // LRU eviction: hardware-first (free the GPU memory a full cache of
    // hardware sessions pins — the scarcity that breaks the next open);
    // the victim is dropped here, but an in-flight render holds its own
    // Arc — the FFmpeg context (and its GPU surface pool) dies with the
    // last reference.
    while cache.len() >= MAX_CACHED_DECODERS {
        let Some(victim) = eviction_victim(&cache) else {
            break;
        };
        cache.remove(&victim);
    }
    cache.insert(key, (decoder.clone(), tick));
    Ok(decoder)
}

/// Decode the footage frame at `time` and copy/scale it into an
/// oakrender F32 frame of `(w, h)`.
pub fn render_footage_frame(
    filename: &str,
    stream_index: i32,
    time: Rational,
    size: (i32, i32),
    format: PixelFormat,
) -> Result<Texture> {
    let started = std::time::Instant::now();
    let result = render_footage_frame_inner(filename, stream_index, time, size, format);
    if std::env::var_os("OAK_PERF").is_some() {
        eprintln!(
            "[decode] {:?} s{} time {}/{} ({:.3}s) size {:?} -> {:.3}s {:?}",
            filename,
            stream_index,
            time.numerator(),
            time.denominator(),
            time.to_f64(),
            size,
            started.elapsed().as_secs_f64(),
            result.is_ok()
        );
    }
    result
}

fn render_footage_frame_inner(
    filename: &str,
    stream_index: i32,
    time: Rational,
    size: (i32, i32),
    format: PixelFormat,
) -> Result<Texture> {
    // (file, stream, time) repeatedly (pre-render restarts, repeated
    // scale-up at the same time, graph + montage interleaving); the
    // decode itself is the expensive part and must not re-run per
    // request. The target size is part of the key (see the cache
    // type's doc).
    let (w, h) = size;
    let cache_size = if w > 0 && h > 0 { (w, h) } else { (-1, -1) };
    let cache_key = (filename.to_string(), stream_index, time_key(&time), cache_size.0, cache_size.1);
    {
        let mut cache = decoded_frames();
        let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let frame = cache.get(&cache_key).map(|(f, _)| f.clone());
        if let Some(frame) = frame {
            cache.insert(cache_key.clone(), (frame.clone(), tick));
            return Ok(Texture::wrap_frame(frame));
        }
    }
    let decoder = open_decoder(filename, stream_index)?;
    let params = RetrieveVideoParams {
        stream: CodecStream::with_block(filename.to_string(), stream_index, None),
        time,
        length: TimeRange::default(),
        force_range: K_COLOR_RANGE_DEFAULT,
        is_image_sequence: false,
        image_sequence_digits: 0,
        image_sequence_number: 0,
        mode: RenderMode::Offline,
        alpha_is_premultiplied: false,
        // 直接按目标尺寸出帧：swscale 一次完成格式转换 + 缩放，不再
        // 产生全分辨率 F32 中间帧（4K 预览每帧省 ~260MB 瞬时拷贝）。
        target_size: if w > 0 && h > 0 {
            Some((w as u32, h as u32))
        } else {
            None
        },
    };
    let decoded = decoder
        .retrieve_video_frame(&params)
        .map_err(|e| Error::Failed(format!("footage decode at {time:?}: {e:?}")))?;

    let src_w = decoded.width();
    let src_h = decoded.height();
    let src_linesize = decoded.linesize_bytes();
    if src_w <= 0 || src_h <= 0 || src_linesize <= 0 || !decoded.is_allocated() {
        return Err(Error::Failed("footage decode: bad decoded frame".into()));
    }

    // `(0, 0)` means "native size": decode without scaling and produce a
    // frame matching the decoded dimensions (M12: the graph sequence
    // path requests native frames and scales at composite time).
    let (dw, dh) = if w > 0 && h > 0 { (w, h) } else { (src_w, src_h) };
    let mut dst = generate_frame(time, (dw, dh), format)?;
    let dst_linesize = dst.linesize_bytes() as i32;
    let src_data = match decoded.data() {
        Some(d) => d,
        None => return Err(Error::Failed("footage decode: no frame data".into())),
    };

    if src_w == dw && src_h == dh && src_linesize == dst_linesize {
        let bytes = (src_h as usize)
            .checked_mul(src_linesize as usize)
            .ok_or(Error::NoMem)?;
        dst.data[..bytes].copy_from_slice(&src_data[..bytes]);
    } else {
        scale_rgba_f32(
            src_data.as_ptr(),
            src_linesize,
            src_w,
            src_h,
            &mut dst.data,
            dst_linesize,
            dw,
            dh,
        );
    }
    // Input node: source colorspace → the pipeline working space (ACEScg
    // by default; the legacy sRGB working space keeps the pass-through).
    convert_decoded_to_working(&mut dst, &decoded);
    // Memoize the finished working-space frame (LRU-capped).
    {
        let mut cache = decoded_frames();
        let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !cache.contains_key(&cache_key) {
            if cache.len() >= MAX_CACHED_FRAMES {
                if let Some(victim) = cache
                    .iter()
                    .filter(|(_, (_, t))| *t > 0)
                    .min_by_key(|(_, (_, t))| *t)
                    .map(|(k, _)| k.clone())
                {
                    cache.remove(&victim);
                }
            }
            cache.insert(cache_key.clone(), (dst.clone(), tick));
        }
    }
    Ok(Texture::wrap_frame(dst))
}

/// Convert a decoded footage frame (display-referred RGB in the source's
/// own colorspace) into the pipeline working space, driven by the frame's
/// colorimetry metadata (carried on the codec frame's params). A no-op in
/// the legacy sRGB working space or when the frame has no pixel data.
fn convert_decoded_to_working(dst: &mut Frame, decoded: &oak_codec::frame::Frame) {
    use oak_core::colormath::{source_primaries_from_av, source_transfer_from_av,
                              WorkingColorSpace,
    };
    if oak_core::color::pipeline_working_space() == WorkingColorSpace::SrgbLegacy {
        return;
    }
    // Frames without colorimetry metadata get the generic fallback (sRGB
    // primaries, sRGB transfer) instead of passing through unconverted;
    // the missing tag is warned once per process.
    let (primaries, transfer) = match decoded.params() {
        Some(params) => (
            source_primaries_from_av(params.color_primaries()),
            source_transfer_from_av(params.color_transfer()),
        ),
        None => {
            warn_missing_colorimetry_once();
            (source_primaries_from_av(2), source_transfer_from_av(2))
        }
    };
    let w = dst.width.max(0) as usize;
    let h = dst.height.max(0) as usize;
    if w == 0 || h == 0 {
        return;
    }
    let row_bytes = w * 16; // F32 RGBA
    let linesize = dst.linesize_bytes();
    for y in 0..h {
        let start = y * linesize;
        if start + row_bytes > dst.data.len() {
            break;
        }
        oak_core::colormath::decode_to_acescg_bytes(
            &mut dst.data[start..start + row_bytes],
            w,
            primaries,
            transfer,
        );
    }
}

// ---------------------------------------------------------------------------
// Graph-driven sequence rendering
// ---------------------------------------------------------------------------

/// WGSL fragment for the graph compositor's alpha-over pass (the C++
/// viewer shader is `:/shaders/alphaover.frag`; same premultiplied-over
/// math on raw texture loads). Bindings: 1 = destination (accumulator),
/// 3 = source (the clip frame) — the layout [`GpuContext::compile_shader_pass`]
/// assigns to texture pairs.
const COMP_WGSL: &str = r#"
@group(0) @binding(1) var dst_tex: texture_2d<f32>;
@group(0) @binding(3) var src_tex: texture_2d<f32>;

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(dst_tex);
    let coord = clamp(vec2<u32>(u32(i32(frag.x)), u32(i32(frag.y))), vec2<u32>(0u, 0u), dims - vec2<u32>(1u, 1u));
    let s = textureLoad(src_tex, coord, 0);
    let d = textureLoad(dst_tex, coord, 0);
    let a = clamp(s.a, 0.0, 1.0);
    // RGB keeps the working-space values unclamped (HDR/WCG can exceed
    // 1.0); only the alpha of the result is clamped to the valid range.
    return vec4<f32>(s.rgb * a + d.rgb * (1.0 - a), clamp(a + d.a * (1.0 - a), 0.0, 1.0));
}
"#;

/// GPU composite of `frames` into one `(w, h)` frame: bottom (last) to
/// top (first), alpha-over into a ping-pong accumulator pair. Frames that
/// do not match `(w, h)` are skipped (the caller scales at decode time;
/// mismatches are defensive).
fn composite_tracks_gpu(
    ctx: &oak_core::backend::GpuContext,
    frames: &[Frame],
    size: (i32, i32),
) -> Result<Frame> {
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return Err(Error::Invalid);
    }
    let program = ctx.compile_shader_pass("oak/builtin/alpha-over", COMP_WGSL, 2, false, false)?;
    let mut acc = ctx.create_texture(w, h)?;
    let mut out = ctx.create_texture(w, h)?;
    let src = ctx.create_texture(w, h)?;

    let result = (|| {
        // The accumulator starts fully transparent.
        let clear = generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32)?;
        ctx.upload(acc, &clear)?;
        for frame in frames.iter().rev().filter(|f| f.width == w && f.height == h) {
            ctx.upload(src, frame)?;
            ctx.run_shader_pass(&program, &[], &[acc, src], out)?;
            std::mem::swap(&mut acc, &mut out);
        }
        ctx.download(acc)
    })();

    ctx.destroy_texture(acc);
    ctx.destroy_texture(out);
    ctx.destroy_texture(src);
    result
}

/// CPU composite of `frames` into one `size` frame — the fallback when no
/// GPU device is available (or the pass fails): bottom (last) to top
/// (first) via [`composite_over`].
///
/// GPU failures are remembered: a device whose wgpu pipeline fails
/// validation (e.g. an adapter that advertises ComputePipeline yet lacks
/// the required features) fails EVERY frame otherwise — each attempt
/// recompiles the shader, surfaces a validation error and stalls the
/// playback tick (the "picture barely updates on NVIDIA" report). After
/// one failure the composite stays on the CPU path for the process.
static GPU_COMPOSITE_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn composite_tracks(frames: Vec<Frame>, size: (i32, i32)) -> Frame {
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return Frame::dummy();
    }
    if !GPU_COMPOSITE_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        // Single-frame composites (the common preview case) stay on the
        // CPU path: the GPU route costs an upload+download round trip per
        // frame for no benefit until two or more tracks overlap.
        if frames.len() > 1 {
            if let Some(ctx) = oak_core::backend::GpuContext::shared() {
                match composite_tracks_gpu(&ctx, &frames, (w, h)) {
                    Ok(frame) => return frame,
                    Err(err) => {
                        eprintln!(
                            "GPU track composite failed, using CPU (and staying there): {err:#}"
                        );
                        GPU_COMPOSITE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }
    let Ok(mut acc) = generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32) else {
        return Frame::dummy();
    };
    let acc_stride = acc.linesize_bytes() as i32;
    for frame in frames.iter().rev().filter(|f| f.width == w && f.height == h) {
        composite_over(
            &mut acc.data,
            acc_stride,
            w,
            h,
            &frame.data,
            frame.linesize_bytes() as i32,
            1.0,
        );
    }
    acc
}

/// Render one frame of `viewer` (a sequence) at `time`: evaluate every
/// enabled clip overlapping `time` through the node graph (one traverser
/// pass per clip; the hooks' decoder cache is shared across clips) and
/// composite the resulting frames bottommost-first — in the video track
/// list the LAST track (the highest-numbered one) is the topmost stack
/// element (NLE stacking, matching the timeline UI).
///
/// `size` is the decode target for every clip, so all frames composite
/// without per-frame scaling. Errors: `Invalid` for a non-F32 format or a
/// non-positive size, `NotFound` for a missing viewer or non-sequence.
pub fn render_graph_frame(
    project: &Mutex<oak_node::project::Project>,
    viewer: oak_node::id::NodeId,
    time: Rational,
    size: (i32, i32),
    format: PixelFormat,
) -> Result<Texture> {
    if format != PixelFormat::F32 {
        return Err(Error::Invalid);
    }
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return Err(Error::Invalid);
    }
    let graph = &project.lock().unwrap().graph;
    let entry = graph.get(viewer).ok_or(Error::NotFound)?;
    let sequence = entry
        .behavior
        .as_any()
        .and_then(|a| a.downcast_ref::<oak_node::sequence::SequenceBehavior>())
        .ok_or(Error::NotFound)?;

    // Collect the clips covering `time`: the sequence's track lists (video
    // then audio — C++ `Sequence` keeps them in the `k_track_input_format`
    // array order), the video list's tracks topmost-first (the list's last
    // track is the top of the stack; `composite_tracks` walks the frames in
    // reverse and draws the first frame last), then each track's blocks.
    let mut clips: Vec<oak_node::id::NodeId> = Vec::new();
    for tl_id in &sequence.track_lists {
        let Some(tl) = graph.get(*tl_id) else {
            continue;
        };
        let Some(tl) = tl
            .behavior
            .as_any()
            .and_then(|a| a.downcast_ref::<oak_node::track::TrackListBehavior>())
        else {
            continue;
        };
        if tl.kind != oak_node::track::TrackType::Video {
            continue;
        }
        for track_id in tl.tracks.iter().rev() {
            let Some(track) = graph.get(*track_id) else {
                continue;
            };
            let Some(track) = track
                .behavior
                .as_any()
                .and_then(|a| a.downcast_ref::<oak_node::track::TrackBehavior>())
            else {
                continue;
            };
            for block_id in &track.blocks {
                let Some(block) = graph.get(*block_id) else {
                    continue;
                };
                let Some(clip) = block
                    .behavior
                    .as_any()
                    .and_then(|a| a.downcast_ref::<oak_node::block::ClipBlockBehavior>())
                else {
                    continue;
                };
                if clip.core.enabled && time >= clip.core.in_() && time < clip.core.out() {
                    clips.push(*block_id);
                }
            }
        }
    }

    let mut traverser = oak_node::traverser::Traverser::new();
    let mut hooks = RenderEvalHooks::new();
    hooks.frame_size = Some(size);
    // The generators' `resolution_in` anchor (C++ NodeGlobals square
    // resolution): the sequence's native size, so proxy-size playback and
    // full-res paused frames draw generated layers identically.
    hooks.sequence_size = sequence
        .video_params
        .first()
        .map(|p| (p.width.max(1), p.height.max(1)));
    let perf = std::env::var_os("OAK_PERF").is_some();
    let mut perf_collect = perf.then(std::time::Instant::now);
    let mut perf_collect_ms = 0.0f64;
    let mut frames: Vec<Frame> = Vec::new();
    let mut perf_clip_hist: Vec<(oak_node::id::NodeId, f64)> = Vec::new();
    for clip in clips.clone() {
        let clip_sw = perf.then(std::time::Instant::now);
        let request = oak_node::traverser::EvalRequest::new(clip, time);
        let table = traverser.evaluate(graph, &request, &mut hooks).map_err(|e| {
            Error::Failed(format!("graph evaluation of clip {clip:?} failed: {e:?}"))
        })?;
        if perf {
            perf_clip_hist.push((
                clip,
                clip_sw
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0),
            ));
        }
        if let Some(t0) = &perf_collect {
            perf_collect_ms += t0.elapsed().as_secs_f64() * 1000.0;
            perf_collect = Some(std::time::Instant::now());
        }
        let Some(NodeValue::Texture(handle)) = table.get(oak_node::value::ValueType::Texture)
        else {
            continue;
        };
        if handle.ctx.is_null() {
            continue;
        }
        let Some(texture) = (unsafe { oak_node::handle::get_checked::<Texture>(handle) }).cloned()
        else {
            continue;
        };
        match texture.to_frame() {
            Ok(frame) => frames.push(frame),
            Err(err) => eprintln!("graph sequence: texture read-back failed: {err:#}"),
        }
    }

    let composite_started = perf.then(std::time::Instant::now);
    let mut frame = composite_tracks(frames, size);
    frame.timestamp = time;
    if perf {
        let composite_ms = composite_started
            .map(|t| t.elapsed().as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        for (clip, ms) in &perf_clip_hist {
            eprintln!("[perf]   clip {clip:?} evaluate {ms:.1}ms");
        }
        eprintln!(
            "[perf] graph frame time {time:?} size {size:?}: evaluate {perf_collect_ms:.1}ms composite {composite_ms:.1}ms total {}ms",
            perf_collect_ms + composite_ms
        );
    }
    Ok(Texture::wrap_frame(frame))
}

/// Render the audio montage over `params.range` (M12 P1): every clip
/// overlapping the range is decoded (interleaved f32 at the output rate
/// and layout) and mixed with its gain; uncovered parts stay silent. The
/// mixed output is clamped to [-1, 1].
pub fn render_audio_samples(
    params: &crate::ticket::AudioTicketParams,
) -> Result<crate::ticket::TicketPayload> {
    let (rate, layout, channels, total_frames) = audio_layout(params)?;
    let mut acc = vec![0.0f32; total_frames.saturating_mul(channels as usize)];
    mix_audio_montage(params, rate, channels, total_frames, &mut acc)?;
    Ok(crate::ticket::TicketPayload::Audio(crate::ticket::AudioSamples {
        samples: acc,
        sample_rate: rate,
        channel_layout: layout,
        channel_count: channels,
    }))
}

/// The output layout an audio render produces: `(sample_rate,
/// channel_layout, channel_count, total_sample_frames)`.
fn audio_layout(params: &crate::ticket::AudioTicketParams) -> Result<(i32, u64, i32, usize)> {
    let rate = params.sample_rate.max(1);
    let channels = params.channel_layout.count_ones().max(1) as i32;
    let duration = params.range.out() - params.range.in_();
    let seconds = if duration.denominator() == 0 {
        0.0
    } else {
        duration.numerator() as f64 / duration.denominator() as f64
    };
    if seconds <= 0.0 || seconds > 3600.0 {
        return Err(Error::Invalid);
    }

    // Anchor each chunk to the absolute sample grid (`round(out·rate) -
    // round(in·rate)`) instead of rounding the duration: at fractional
    // frame rates (29.97 fps → 1601.6 samples/frame) a duration-round
    // would emit 1602 samples for every chunk and accumulate ~12 extra
    // samples per second, slowly desyncing audio from video. Per-chunk
    // anchoring keeps the total exact and matches the decode side
    // (`FFmpegDecoder::retrieve_audio_to` fills `round(out·rate) -
    // round(in·rate)` samples).
    let total_frames = ((params.range.out().to_f64() * rate as f64).round()
        - (params.range.in_().to_f64() * rate as f64).round())
        .max(0.0) as usize;
    Ok((rate, params.channel_layout, channels, total_frames))
}

/// The byte length (interleaved f32, little-endian) an audio render of
/// `params` writes into a shm slot — the worker's slot-geometry check
/// (M15 S3). Mirrors [`render_audio_samples_into`]'s layout math.
pub fn audio_samples_byte_len(params: &crate::ticket::AudioTicketParams) -> Result<usize> {
    let (_rate, _layout, channels, total_frames) = audio_layout(params)?;
    Ok(total_frames
        .saturating_mul(channels as usize)
        .saturating_mul(4))
}

/// Mix the audio montage into `acc` (`total_frames * channels` samples,
/// zero-initialized by the caller). Shared by the heap
/// [`render_audio_samples`] and the shm-slot [`render_audio_samples_into`]
/// paths so the decode/mix logic exists once.
fn mix_audio_montage(
    params: &crate::ticket::AudioTicketParams,
    rate: i32,
    channels: i32,
    total_frames: usize,
    acc: &mut [f32],
) -> Result<()> {
    for clip in &params.montage {
        // Overlap of the clip with the requested range.
        let in_time = params.range.in_().max(clip.in_time);
        let out_time = params.range.out().min(clip.out_time);
        if out_time <= in_time {
            continue;
        }
        // Round to the absolute sample grid like the decode side (which
        // anchors at `round(media_start·rate)`): truncation would shift a
        // clip's mix by up to one sample per chunk.
        let start_frame = ((in_time - params.range.in_()).to_f64() * rate as f64).round() as usize;
        let end_frame = ((out_time - params.range.in_()).to_f64() * rate as f64).round() as usize;
        if start_frame >= total_frames {
            continue;
        }
        let frames = (end_frame - start_frame).min(total_frames - start_frame);
        if frames == 0 {
            continue;
        }

        // Media time of the overlap start; the media-out is
        // media_start + (overlap duration).
        let media_start = clip.media_in + (in_time - clip.in_time);
        let media_end = media_start + (out_time - in_time);
        let mut buf = vec![0.0f32; frames * channels as usize];
        let decoder = open_decoder(&clip.filename, clip.stream_index)?;
        let range = TimeRange::new(media_start, media_end);
        let status = decoder
            .retrieve_audio(&mut buf, &range, rate, params.channel_layout)
            .map_err(|e| Error::Failed(format!("footage audio decode: {e:?}")))?;
        let written = match status {
            RetrieveAudioStatus::Success => frames,
            _ => 0,
        };
        // Mix into the accumulator (per-channel gain).
        for i in 0..written * channels as usize {
            acc[start_frame * channels as usize + i] += buf[i] * clip.gain;
        }
    }
    // Clamp to [-1, 1]: overlapping clips sum linearly and can exceed
    // full scale, and the playback sink (cpal) forwards samples without
    // any clamping of its own.
    for sample in acc.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
    Ok(())
}

/// Render the audio montage over `params.range` directly into `dst` as
/// little-endian f32 bytes (M15 S3 worker seam): the render worker passes
/// a shared-memory slot slice as `dst`, so the samples land in the slot
/// with no staging allocation. `dst.len()` must hold
/// `frame_count * channels * 4` bytes.
pub fn render_audio_samples_into(
    params: &crate::ticket::AudioTicketParams,
    dst: &mut [u8],
) -> Result<()> {
    let (rate, _layout, channels, total_frames) = audio_layout(params)?;
    let need = total_frames
        .saturating_mul(channels as usize)
        .saturating_mul(4);
    if dst.len() < need {
        return Err(Error::NoMem);
    }
    let mut acc = vec![0.0f32; total_frames.saturating_mul(channels as usize)];
    mix_audio_montage(params, rate, channels, total_frames, &mut acc)?;
    // Interleaved f32 -> little-endian bytes in the slot.
    for (out, sample) in dst[..need].chunks_exact_mut(4).zip(&acc) {
        out.copy_from_slice(&sample.to_le_bytes());
    }
    Ok(())
}

/// Bilinear scale an F32-RGBA image (row-major with per-row strides).
fn scale_rgba_f32(
    src: *const u8,
    src_stride: i32,
    src_w: i32,
    src_h: i32,
    dst: &mut [u8],
    dst_stride: i32,
    dst_w: i32,
    dst_h: i32,
) {
    if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 {
        return;
    }
    // 1:1 copy (no scaling): the caller already handled stride equality;
    // here we handle the general case with a fast path for integer 1:1.
    let sample = |x: f64, y: f64| -> [f32; 4] {
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = (x - x0 as f64) as f32;
        let fy = (y - y0 as f64) as f32;
        let x1 = (x0 + 1).clamp(0, src_w - 1);
        let y1 = (y0 + 1).clamp(0, src_h - 1);
        let x0 = x0.clamp(0, src_w - 1);
        let y0 = y0.clamp(0, src_h - 1);
        let px = |xx: i32, yy: i32| -> [f32; 4] {
            let off = (yy as usize) * (src_stride as usize) + (xx as usize) * 16;
            // SAFETY: coordinates are clamped to the source size.
            let b = unsafe { std::slice::from_raw_parts(src.add(off), 16) };
            let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
            [f(0), f(1), f(2), f(3)]
        };
        let c00 = px(x0, y0);
        let c10 = px(x1, y0);
        let c01 = px(x0, y1);
        let c11 = px(x1, y1);
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let mut out = [0f32; 4];
        for i in 0..4 {
            let top = lerp(c00[i], c10[i], fx);
            let bottom = lerp(c01[i], c11[i], fx);
            out[i] = lerp(top, bottom, fy);
        }
        out
    };
    for y in 0..dst_h {
        let sy = (y as f64 + 0.5) * src_h as f64 / dst_h as f64 - 0.5;
        let sy = sy.max(0.0);
        for x in 0..dst_w {
            let sx = (x as f64 + 0.5) * src_w as f64 / dst_w as f64 - 0.5;
            let sx = sx.max(0.0);
            let px = sample(sx, sy);
            let off = (y as usize) * (dst_stride as usize) + (x as usize) * 16;
            // RGB is not clamped: bilinear lerp is a convex combination,
            // so values cannot overshoot the source range, and HDR/WCG
            // working-space pixels may legitimately exceed 1.0. Only the
            // alpha channel is clamped to its valid range.
            for i in 0..4 {
                let v = if i == 3 { px[i].clamp(0.0, 1.0) } else { px[i] };
                dst[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// Composite the montage at `time`: decode each covering clip and
/// alpha-composite topmost-last (NLE track order: the highest-numbered
/// track is topmost).
fn render_montage_frame(
    time: Rational,
    params: &crate::ticket::VideoTicketParams,
    size: (i32, i32),
    format: PixelFormat,
) -> Result<Texture> {
    let mut acc = generate_frame(time, size, format)?;
    let stride = acc.linesize_bytes();
    let acc_data = &mut acc.data;
    render_montage_frame_into(time, params, size, acc_data, stride as i32)?;
    Ok(Texture::wrap_frame(acc))
}

/// Composite the montage at `time` directly into `dst` (F32 RGBA rows of
/// `dst_stride` bytes) — the M15 worker seam: the render worker passes a
/// shared-memory slot slice as `dst`, so the composited frame lands in
/// the slot with no staging copy. `dst` is zeroed first (transparent
/// black base).
pub fn render_montage_frame_into(
    time: Rational,
    params: &crate::ticket::VideoTicketParams,
    size: (i32, i32),
    dst: &mut [u8],
    dst_stride: i32,
) -> Result<()> {
    let (w, h) = size;
    let need = (h as usize).saturating_mul(dst_stride as usize);
    if w <= 0 || h <= 0 || dst.len() < need {
        return Err(Error::Invalid);
    }
    // Transparent-black base.
    dst[..need].fill(0);
    // Decode from the bottom clip first, composite topmost-last.
    for clip in &params.montage {
        if time < clip.in_time || time >= clip.out_time {
            continue;
        }
        let media_time = clip.media_in + (time - clip.in_time);
        let decoded = render_footage_frame(
            &clip.filename,
            clip.stream_index,
            media_time,
            (w, h),
            PixelFormat::F32,
        )?;
        // The clip's effect stack runs between decode and compositing
        // (C++ semantics: the clip texture passes through the chain
        // bottom-up, the chain top feeds the track composite).
        let effected = apply_clip_effects(decoded, clip, time);
        let (src_data, src_stride) = match &effected {
            Texture::Cpu(src) => (&src.data, src.linesize_bytes() as i32),
            _ => continue,
        };
        composite_over(dst, dst_stride, w, h, src_data, src_stride, clip.gain);
    }
    Ok(())
}

/// `src` over `dst` (premultiplied-ish alpha compositing; F32 RGBA).
/// `gain` scales the source RGB (audio-style volume applied to video
/// transparency is ignored here; gain scales color). Exposed for the M15
/// render worker, which composites montage frames directly into
/// shared-memory slots.
pub fn composite_over(
    dst: &mut [u8],
    dst_stride: i32,
    w: i32,
    h: i32,
    src: &[u8],
    src_stride: i32,
    gain: f32,
) {
    let read = |buf: &[u8], stride: i32, x: i32, y: i32| -> [f32; 4] {
        let off = (y as usize) * (stride as usize) + (x as usize) * 16;
        let mut out = [0f32; 4];
        for i in 0..4 {
            out[i] = f32::from_le_bytes(buf[off + i * 4..off + i * 4 + 4].try_into().unwrap());
        }
        out
    };
    for y in 0..h {
        for x in 0..w {
            let s = read(src, src_stride, x, y);
            let d = read(dst, dst_stride, x, y);
            let a = (s[3] * gain).clamp(0.0, 1.0);
            let out = [
                (s[0] * gain) * a + d[0] * (1.0 - a),
                (s[1] * gain) * a + d[1] * (1.0 - a),
                (s[2] * gain) * a + d[2] * (1.0 - a),
                a + d[3] * (1.0 - a),
            ];
            let off = (y as usize) * (dst_stride as usize) + (x as usize) * 16;
            // RGB keeps the working-space values unclamped (HDR/WCG can
            // exceed 1.0); only alpha is clamped to its valid range.
            for i in 0..4 {
                let v = if i == 3 { out[i].clamp(0.0, 1.0) } else { out[i] };
                dst[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Montage clip effect stacks
// ---------------------------------------------------------------------------

/// The built-in Opacity effect's type id (oaknode `OpacityEffect`) — the
/// one built-in video effect with a CPU evaluator on the montage path.
const OPACITY_EFFECT_TYPE_ID: &str = "org.olivevideoeditor.Olive.opacity";

/// The Opacity effect's value input id (oaknode `opacity_in`).
const OPACITY_VALUE_INPUT: &str = "opacity_in";

/// The effect type ids the montage path already warned about (one log
/// line per type per process instead of one per frame).
fn unsupported_warned() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    WARNED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Log an unsupported-effect passthrough once per type id.
fn warn_unsupported_once(type_id: &str, reason: &str) {
    if unsupported_warned().insert(type_id.to_string()) {
        eprintln!("montage effect \"{type_id}\" passes through unchanged: {reason}");
    }
}

/// Warn once (per process) when a decoded frame carries no colorimetry
/// metadata and falls back to the generic sRGB assumptions.
fn warn_missing_colorimetry_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        eprintln!("decoded frame has no colorimetry metadata; assuming sRGB");
    });
}

/// Run a clip's effect stack over its decoded frame (source-first order;
/// disabled effects are bypassed — the C++ traverser's bypass pushes the
/// effect input through unchanged). Effects the montage path cannot
/// evaluate log a warning once and pass the frame through.
fn apply_clip_effects(
    src: Texture,
    clip: &crate::ticket::MontageClip,
    time: Rational,
) -> Texture {
    let mut tex = src;
    for effect in &clip.effects {
        if !effect.enabled {
            continue;
        }
        tex = apply_montage_effect(tex, effect, time);
    }
    tex
}

/// Apply one effect to `src` (an F32 RGBA CPU frame of the montage
/// pipeline). `time` is the sequence time the frame is rendered at (the
/// C++ node evaluation time).
fn apply_montage_effect(
    src: Texture,
    effect: &crate::ticket::MontageEffect,
    time: Rational,
) -> Texture {
    // Built-in Opacity: multiply every channel by the opacity factor
    // (C++ `:/shaders/opacity.frag`: `frag_color = texture(tex_in, …) *
    // opacity_in` — the shader scales the whole vec4, alpha included).
    if effect.type_id == OPACITY_EFFECT_TYPE_ID {
        let factor = effect
            .params
            .iter()
            .find(|(id, _)| id == OPACITY_VALUE_INPUT)
            .map(|(_, v)| v.to_double())
            .unwrap_or(1.0);
        // Unity is a pass-through (C++ `qFuzzyCompare(opacity, 1.0)`).
        if (factor - 1.0).abs() * 1e12 <= factor.abs().min(1.0) {
            return src;
        }
        // Texture implements Drop, so scale in place through a mutable
        // borrow instead of moving the frame out.
        let mut tex = src;
        match &mut tex {
            Texture::Cpu(frame) => {
                let stride = frame.linesize_bytes();
                for row in frame.data.chunks_exact_mut(stride).take(frame.height.max(0) as usize) {
                    for px in row[..(frame.width.max(0) as usize) * 16].chunks_exact_mut(16) {
                        for c in px.chunks_exact_mut(4) {
                            let v = f32::from_le_bytes(c.try_into().unwrap());
                            c.copy_from_slice(&(v * factor as f32).to_le_bytes());
                        }
                    }
                }
            }
            _ => {
                warn_unsupported_once(
                    &effect.type_id,
                    "opacity on a non-CPU texture is not supported by the montage path",
                );
            }
        }
        return tex;
    }

    // Everything else: an OFX plugin effect. The montage carries the
    // plugin identifier; the rendering process resolves it to a live
    // instance through the oakplugin-installed factory (lazily created
    // and cached per identifier), then dispatches through the plugin
    // executor exactly like the graph path's plugin jobs.
    let Some(factory) = plugin_instance_factory() else {
        warn_unsupported_once(
            &effect.type_id,
            "no plugin instance factory installed (oakplugin init missing in this process)",
        );
        return src;
    };
    let Some(instance) = factory(&effect.type_id) else {
        warn_unsupported_once(
            &effect.type_id,
            "no evaluator: unknown built-in effect or OFX plugin unavailable in this process",
        );
        return src;
    };
    let spec = JobSpec::Plugin {
        instance,
        time: time.to_f64(),
        effect_input_id: effect.effect_input_id.clone(),
        inputs: Vec::new(),
        values: effect.params.clone(),
    };
    let size = src.size();
    match RenderEvalHooks::new().process_plugin_job(src, &spec) {
        Ok(texture) => texture,
        Err(err) => {
            // Unreachable for a Plugin spec (the executor failure path
            // yields a purple frame); stay loud rather than silent.
            eprintln!("montage plugin job failed to dispatch: {err:#}");
            purple_frame(time, size)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_frame_is_f32_transparent_black() {
        let f = generate_frame(Rational::new(5, 1), (64, 48), PixelFormat::F32).unwrap();
        assert_eq!(f.width, 64);
        assert_eq!(f.height, 48);
        assert_eq!(f.format, PixelFormat::F32);
        assert_eq!(f.timestamp, Rational::new(5, 1));
        assert!(f.data.iter().all(|&b| b == 0), "transparent black");
        assert_eq!(f.data.len(), 64 * 48 * 4 * 4);
    }

    #[test]
    fn generated_frame_rejects_bad_size() {
        assert!(generate_frame(Rational::new(0, 1), (0, 10), PixelFormat::F32).is_err());
        assert!(generate_frame(Rational::new(0, 1), (-1, 10), PixelFormat::F32).is_err());
    }

    #[test]
    fn produced_frame_honors_ticket_params() {
        let params = crate::ticket::VideoTicketParams {
            viewer: 1,
            project: String::new(),
            time: Rational::new(2, 1),
            force_size: Some((16, 9)),
            force_format: Some(PixelFormat::F32),
            cache: None,
            cache_dir: None,
            cache_id: None,
            cache_timebase: None,
            footage: None,
            montage: Vec::new(),
        };
        let tex = render_produced_frame(params.time, &params).unwrap();
        assert_eq!(tex.size(), (16, 9));
        assert_eq!(tex.format(), PixelFormat::F32);
    }

    #[test]
    fn hooks_fail_explainably_for_deferred_jobs() {
        let mut hooks = RenderEvalHooks::new();

        assert!(hooks
            .process_video_cache_job(&JobSpec::Cache { path: "p".into() })
            .is_err());
    }

    #[test]
    fn generation_fills_cpu_texture() {
        let mut hooks = RenderEvalHooks::new();
        let mut tex = Texture::wrap_frame(
            generate_frame(Rational::new(1, 1), (8, 8), PixelFormat::F32).unwrap(),
        );
        hooks
            .process_frame_generation(&mut tex, Rational::new(3, 1))
            .unwrap();
        let Texture::Cpu(f) = &tex else {
            unreachable!()
        };
        assert_eq!(f.timestamp, Rational::new(3, 1));
        assert!(f.data.iter().all(|&b| b == 0));
        // GPU destination rejected.
        let mut gpu = Texture::Gpu {
            token: 0,
            backend: oak_core::backend::BackendKind::Cpu,
            width: 8,
            height: 8,
            format: PixelFormat::F32,
            ctx: Arc::new(UnusedCtx),
        };
        assert!(hooks
            .process_frame_generation(&mut gpu, Rational::new(1, 1))
            .is_err());
    }

    // The plugin executor lives in a process-wide slot; the tests below
    // mutate it and therefore serialize against each other.
    static PLUGIN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn plugin_spec() -> JobSpec {
        JobSpec::Plugin {
            instance: 7,
            time: 0.5,
            effect_input_id: Some("Source".into()),
            inputs: Vec::new(),
            values: Vec::new(),
        }
    }

    fn first_pixel(texture: &Texture) -> [f32; 4] {
        let Texture::Cpu(frame) = texture else {
            unreachable!()
        };
        let mut out = [0f32; 4];
        for i in 0..4 {
            out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
        }
        out
    }

    #[test]
    fn plugin_job_without_executor_yields_purple_frame() {
        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_executor(None);
        let mut hooks = RenderEvalHooks::new();
        let src = Texture::wrap_frame(
            generate_frame(Rational::new(0, 1), (4, 2), PixelFormat::F32).unwrap(),
        );
        let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
        assert_eq!(out.size(), (4, 2));
        assert_eq!(first_pixel(&out), [1.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn plugin_job_executor_error_falls_back_to_purple() {
        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_executor(Some(Arc::new(|_req: &PluginJobRequest<'_>| {
            Err(Error::Failed("boom".into()))
        })));
        let mut hooks = RenderEvalHooks::new();
        let src = Texture::wrap_frame(
            generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap(),
        );
        let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
        assert_eq!(first_pixel(&out), [1.0, 0.0, 1.0, 1.0]);
        set_plugin_executor(None);
    }

    #[test]
    fn plugin_job_dispatches_through_installed_executor() {
        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
            // Echo: paint the source size with the instance id.
            let JobSpec::Plugin { instance, .. } = req.spec else {
                return Err(Error::Invalid);
            };
            let v = (*instance as f32) / 10.0;
            let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
            for pixel in frame.data.chunks_exact_mut(16) {
                for c in 0..4 {
                    pixel[c * 4..c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            Ok(Texture::wrap_frame(frame))
        })));
        let mut hooks = RenderEvalHooks::new();
        let src = Texture::wrap_frame(
            generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap(),
        );
        let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
        assert_eq!(first_pixel(&out), [0.7, 0.7, 0.7, 0.7]);
        set_plugin_executor(None);
    }

    #[test]
    fn resolve_executes_payload_box_and_keeps_plain_textures() {
        use oak_node::nodes::plugin::{PluginInstanceHandle, PluginJobPayload};

        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
            let JobSpec::Plugin {
                instance,
                values,
                inputs,
                effect_input_id,
                ..
            } = req.spec
            else {
                return Err(Error::Invalid);
            };
            // The resolve seam must deliver the tagged param values and
            // the clip texture to the executor.
            assert_eq!(*instance, 7);
            assert_eq!(effect_input_id.as_deref(), Some("Source"));
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].0, "Source");
            assert!(values.iter().any(|(k, v)| {
                k == "gain" && matches!(v, NodeValue::Float(f) if (*f - 0.25).abs() < 1e-6)
            }));
            let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
            for pixel in frame.data.chunks_exact_mut(16) {
                for (i, v) in [0.25f32, 0.5, 0.75, 1.0].iter().enumerate() {
                    pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            Ok(Texture::wrap_frame(frame))
        })));

        // A real source texture box plus a payload box referencing it.
        let src_frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
        let src_box = oak_node::handle::make_owned(Texture::wrap_frame(src_frame));
        let mut values = NodeValueRow::new();
        values.insert("Source".into(), NodeValue::Texture(src_box));
        values.insert("gain".into(), NodeValue::Float(0.25));
        let payload = PluginJobPayload {
            instance: PluginInstanceHandle(7),
            time: Rational::new(1, 2),
            effect_input_id: "Source".into(),
            values,
        };

        let mut table = NodeValueTable::default();
        table.push(
            oak_node::value::ValueType::Texture,
            NodeValue::Texture(oak_node::handle::make_owned(payload)),
            None,
        );

        let mut hooks = RenderEvalHooks::new();
        hooks.resolve_plugin_jobs(&mut table);

        let NodeValue::Texture(handle) = table.get(oak_node::value::ValueType::Texture).unwrap()
        else {
            unreachable!()
        };
        let rendered = unsafe { oak_node::handle::get_checked::<Texture>(handle) }
            .expect("payload box must be replaced by the rendered texture");
        assert_eq!(first_pixel(rendered), [0.25, 0.5, 0.75, 1.0]);
        set_plugin_executor(None);
    }

    /// Stand-in context for the "GPU destination" test (never used for
    /// real GPU work).
    struct UnusedCtx;
    impl oak_core::backend::GpuContextLike for UnusedCtx {
        fn kind(&self) -> oak_core::backend::BackendKind {
            oak_core::backend::BackendKind::Cpu
        }
        fn destroy_texture(&self, _token: u64) {}
        fn upload(&self, _token: u64, _frame: &Frame) -> Result<()> {
            Err(Error::Failed("unused".into()))
        }
        fn download(&self, _token: u64) -> Result<Frame> {
            Err(Error::Failed("unused".into()))
        }
        fn blit(
            &self,
            _src: u64,
            _dst: u64,
            _processor: Option<&oak_core::color::ColorProcessor>,
        ) -> Result<()> {
            Err(Error::Failed("unused".into()))
        }
    }
    use std::sync::Arc;

    // ---- Audio (M12 P1 / M15 S3) -----------------------------------------

    fn audio_params(range: TimeRange) -> crate::ticket::AudioTicketParams {
        crate::ticket::AudioTicketParams {
            viewer: 1,
            range,
            sample_rate: 48000,
            channel_layout: 0x3,
            montage: Vec::new(),
        }
    }

    #[test]
    fn render_audio_samples_produces_silence_for_empty_montage() {
        // M12 P1: an empty montage renders total silence at the requested
        // layout.
        let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 24)));
        match render_audio_samples(&params).unwrap() {
            crate::ticket::TicketPayload::Audio(samples) => {
                // 1/24 s at 48 kHz = 2000 sample frames, stereo.
                assert_eq!(samples.sample_rate, 48000);
                assert_eq!(samples.channel_count, 2);
                assert_eq!(samples.samples.len(), 2000 * 2);
                assert!(samples.samples.iter().all(|&v| v == 0.0), "silence");
            }
            other => panic!("expected Audio payload, got {other:?}"),
        }
    }

    #[test]
    fn render_audio_samples_into_matches_heap_path_byte_for_byte() {
        // M15 S3: the shm-slot writer must produce exactly the same
        // little-endian f32 bytes as the heap path, so a worker's slot and
        // the in-process fallback agree for the same montage.
        let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 48)));
        let heap = match render_audio_samples(&params).unwrap() {
            crate::ticket::TicketPayload::Audio(samples) => samples,
            other => panic!("expected Audio payload, got {other:?}"),
        };
        let mut dst = vec![0u8; heap.samples.len() * 4];
        render_audio_samples_into(&params, &mut dst).unwrap();
        let expected: Vec<u8> = heap
            .samples
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(dst, expected);
        // And the into-path output parses back into the same samples.
        let parsed: Vec<f32> = dst
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(parsed, heap.samples);
    }

    #[test]
    fn render_audio_samples_into_rejects_small_buffer() {
        let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 24)));
        let mut dst = [0u8; 8]; // far too small for 2000x2 f32 samples
        assert!(render_audio_samples_into(&params, &mut dst).is_err());
    }

    // ---- Montage clip effect stacks -------------------------------------

    /// A 2x1 F32 texture filled with a known color.
    fn solid_texture(r: f32, g: f32, b: f32, a: f32) -> Texture {
        let mut frame = generate_frame(Rational::new(0, 1), (2, 1), PixelFormat::F32).unwrap();
        for px in frame.data.chunks_exact_mut(16) {
            for (c, v) in px.chunks_exact_mut(4).zip([r, g, b, a]) {
                c.copy_from_slice(&v.to_le_bytes());
            }
        }
        Texture::Cpu(frame)
    }

    fn opacity_effect(enabled: bool, value: f64) -> crate::ticket::MontageEffect {
        crate::ticket::MontageEffect {
            type_id: OPACITY_EFFECT_TYPE_ID.to_string(),
            enabled,
            effect_input_id: Some("tex_in".to_string()),
            params: vec![(OPACITY_VALUE_INPUT.to_string(), NodeValue::Float(value))],
        }
    }

    fn clip_with_effects(effects: Vec<crate::ticket::MontageEffect>) -> crate::ticket::MontageClip {
        crate::ticket::MontageClip {
            filename: String::new(),
            stream_index: 0,
            in_time: Rational::new(0, 1),
            out_time: Rational::new(1, 1),
            media_in: Rational::new(0, 1),
            gain: 1.0,
            effects,
        }
    }

    /// The built-in Opacity effect really scales the decoded pixels
    /// (every channel, C++ `opacity.frag` parity).
    #[test]
    fn montage_opacity_effect_scales_pixels() {
        let clip = clip_with_effects(vec![opacity_effect(true, 0.5)]);
        let out = apply_clip_effects(solid_texture(0.8, 0.4, 0.2, 1.0), &clip, Rational::new(0, 1));
        assert_eq!(first_pixel(&out), [0.4, 0.2, 0.1, 0.5]);
    }

    /// Disabled effects are bypassed (C++ traverser parity), and unity
    /// opacity is a pass-through.
    #[test]
    fn montage_disabled_or_unity_effects_pass_through() {
        let disabled = clip_with_effects(vec![opacity_effect(false, 0.5)]);
        let out = apply_clip_effects(solid_texture(0.8, 0.4, 0.2, 1.0), &disabled, Rational::new(0, 1));
        assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);

        let unity = clip_with_effects(vec![opacity_effect(true, 1.0)]);
        let out = apply_clip_effects(solid_texture(0.8, 0.4, 0.2, 1.0), &unity, Rational::new(0, 1));
        assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);
    }

    /// An OFX effect (any non-built-in type id) resolves its instance
    /// through the installed factory and dispatches through the executor,
    /// carrying the montage's parameter values.
    #[test]
    fn montage_plugin_effect_dispatches_with_params() {
        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_instance_factory(Some(Arc::new(|identifier: &str| {
            (identifier == "com.example.darken").then_some(7)
        })));
        set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
            let JobSpec::Plugin { instance, values, .. } = req.spec else {
                return Err(Error::Invalid);
            };
            assert_eq!(*instance, 7);
            // The injected parameter drives the output: paint the frame
            // with the "gain" value so the test observes the param path.
            let gain = values
                .iter()
                .find(|(k, _)| k == "gain")
                .map(|(_, v)| v.to_double() as f32)
                .unwrap_or(1.0);
            let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
            for px in frame.data.chunks_exact_mut(16) {
                for (c, v) in px.chunks_exact_mut(4).zip([gain, gain, gain, 1.0]) {
                    c.copy_from_slice(&v.to_le_bytes());
                }
            }
            Ok(Texture::Cpu(frame))
        })));
        let clip = clip_with_effects(vec![crate::ticket::MontageEffect {
            type_id: "com.example.darken".to_string(),
            enabled: true,
            effect_input_id: Some("Source".to_string()),
            params: vec![("gain".to_string(), NodeValue::Float(0.25))],
        }]);
        let out = apply_clip_effects(solid_texture(0.8, 0.4, 0.2, 1.0), &clip, Rational::new(0, 1));
        assert_eq!(first_pixel(&out), [0.25, 0.25, 0.25, 1.0]);
        set_plugin_executor(None);
        set_plugin_instance_factory(None);
    }

    /// An effect nobody can evaluate (no factory / unknown type) passes
    /// the frame through unchanged — loudly (the warn-once log), never a
    /// silent no-op.
    #[test]
    fn montage_unknown_effect_passes_through() {
        let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
        set_plugin_instance_factory(None);
        let clip = clip_with_effects(vec![crate::ticket::MontageEffect {
            type_id: "com.example.missing".to_string(),
            enabled: true,
            effect_input_id: Some("Source".to_string()),
            params: Vec::new(),
        }]);
        let out = apply_clip_effects(solid_texture(0.8, 0.4, 0.2, 1.0), &clip, Rational::new(0, 1));
        assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);
    }

    // ---- Graph-driven sequence (M12 phase 2) ----------------------------

    /// Native-size decode: a `(0, 0)` request produces the footage's
    /// intrinsic dimensions (the graph sequence path decodes native and
    /// scales at composite time).
    #[test]
    fn footage_native_size_decode() {
        let path = std::env::temp_dir().join(format!("oakrender_graph_native_{}.mp4", std::process::id()));
        oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip generation");
        let tex = render_footage_frame(&path.to_string_lossy(), 0, Rational::new(0, 1), (0, 0), PixelFormat::F32)
            .expect("native-size decode");
        assert_eq!(tex.size(), (64, 64));
        let _ = std::fs::remove_file(&path);
    }

    /// The resolve seam decodes each boxed [`FootageJobPayload`] into a
    /// texture and leaves genuine texture boxes untouched (in-place row
    /// replacement, C++ FootageJob processing).
    #[test]
    fn resolve_footage_jobs_decodes_payload_box() {
        let path = std::env::temp_dir().join(format!("oakrender_graph_resolve_{}.mp4", std::process::id()));
        oak_codec::testmedia::write_test_clip(&path, 32, 32, 10, 10).expect("test clip generation");

        let mut table = NodeValueTable::default();
        let payload = FootageJobPayload {
            filename: path.to_string_lossy().into_owned(),
            stream_index: 0,
            time: Rational::new(0, 1),
        };
        table.push(
            oak_node::value::ValueType::Texture,
            NodeValue::Texture(oak_node::handle::make_owned(payload)),
            None,
        );
        let genuine = Texture::wrap_frame(generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32).unwrap());
        table.push(
            oak_node::value::ValueType::Texture,
            NodeValue::Texture(oak_node::handle::make_owned(genuine)),
            None,
        );

        let mut hooks = RenderEvalHooks::new();
        hooks.frame_size = Some((32, 32));
        hooks.resolve_footage_jobs(&mut table);
        assert_eq!(table.count(), 2, "both rows stay, only the payload is replaced");

        let rows = table.rows();
        let NodeValue::Texture(decoded_handle) = &rows[0].1 else {
            unreachable!()
        };
        let decoded = unsafe { oak_node::handle::get_checked::<Texture>(decoded_handle) }
            .expect("payload box replaced by the decoded texture");
        let Texture::Cpu(frame) = decoded else {
            unreachable!()
        };
        assert_eq!((frame.width, frame.height), (32, 32));
        assert!(
            frame.data.iter().any(|&b| b != 0),
            "decoded frame must contain non-black pixels"
        );

        let NodeValue::Texture(genuine_handle) = &rows[1].1 else {
            unreachable!()
        };
        let genuine = unsafe { oak_node::handle::get_checked::<Texture>(genuine_handle) }
            .expect("genuine texture box stays untouched");
        assert_eq!(genuine.size(), (4, 4));

        let _ = std::fs::remove_file(&path);
    }

    /// The resolve seam applies a ColorTransformJob's OCIO processor for
    /// real (C++ ColorTransformJob processing): a CPU frame converts
    /// through the LUT in place.
    #[test]
    fn resolve_color_transform_job_applies_lut_on_cpu() {
        if oak_core::color::set_up_default_config().is_err() {
            eprintln!("bundled OCIO missing; skipping");
            return;
        }
        // 1D LUT doubling the red channel (linear ramp 0→0, 1→2).
        let path = std::env::temp_dir()
            .join(format!("oakrender_lut_double_{}.cube", std::process::id()));
        std::fs::write(&path, "LUT_1D_SIZE 2\n0.0 0.0 0.0\n2.0 1.0 1.0\n").unwrap();

        let Some(processor) = oak_core::color::ColorProcessor::create_lut(
            path.to_str().unwrap(),
            oak_core::color::Direction::Normal,
        )
        .filter(|p| p.is_valid()) else {
            eprintln!("LUT processor unavailable; skipping");
            let _ = std::fs::remove_file(&path);
            return;
        };

        // Input: a 0.25-grey CPU frame.
        let mut frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
        for px in frame.data.chunks_exact_mut(16) {
            for (c, v) in px.chunks_exact_mut(4).zip([0.25f32, 0.25, 0.25, 1.0]) {
                c.copy_from_slice(&v.to_le_bytes());
            }
        }
        let payload = ColorTransformJobPayload {
            color_processor: std::sync::Arc::new(processor),
            input: NodeValue::Texture(oak_node::handle::make_owned(Texture::wrap_frame(frame))),
            time: Rational::new(0, 1),
        };
        let mut table = NodeValueTable::default();
        table.push(
            oak_node::value::ValueType::Texture,
            NodeValue::Texture(oak_node::handle::make_owned(payload)),
            None,
        );

        use oak_node::traverser::RenderHooks;
        let mut hooks = RenderEvalHooks::new();
        hooks.resolve(
            oak_node::id::NodeId::INVALID,
            &NodeValueRow::new(),
            &mut table,
        );

        let rows = table.rows();
        let NodeValue::Texture(handle) = &rows[0].1 else {
            unreachable!()
        };
        let out = unsafe { oak_node::handle::get_checked::<Texture>(handle) }
            .expect("the job box is replaced by the converted texture");
        let px = first_pixel(out);
        assert!(
            (px[0] - 0.5).abs() < 1e-3,
            "red channel doubles through the LUT: {px:?}"
        );
        assert!((px[1] - 0.25).abs() < 1e-4, "green unchanged: {px:?}");
        assert_eq!(px[3], 1.0, "alpha preserved");
        let _ = std::fs::remove_file(&path);
    }

    /// An invalid processor passes the input texture through unchanged
    /// (C++ creates processors non-fatally).
    #[test]
    fn resolve_color_transform_job_passes_through_when_processor_invalid() {
        let mut frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
        for px in frame.data.chunks_exact_mut(16) {
            for (c, v) in px.chunks_exact_mut(4).zip([0.4f32, 0.3, 0.2, 1.0]) {
                c.copy_from_slice(&v.to_le_bytes());
            }
        }
        let payload = ColorTransformJobPayload {
            color_processor: std::sync::Arc::new(ColorProcessor::pass_through()),
            input: NodeValue::Texture(oak_node::handle::make_owned(Texture::wrap_frame(frame))),
            time: Rational::new(0, 1),
        };
        let mut table = NodeValueTable::default();
        table.push(
            oak_node::value::ValueType::Texture,
            NodeValue::Texture(oak_node::handle::make_owned(payload)),
            None,
        );

        use oak_node::traverser::RenderHooks;
        let mut hooks = RenderEvalHooks::new();
        hooks.resolve(
            oak_node::id::NodeId::INVALID,
            &NodeValueRow::new(),
            &mut table,
        );

        let rows = table.rows();
        let NodeValue::Texture(handle) = &rows[0].1 else {
            unreachable!()
        };
        let out = unsafe { oak_node::handle::get_checked::<Texture>(handle) }
            .expect("the job box is replaced by the input texture");
        assert_eq!(first_pixel(out), [0.4, 0.3, 0.2, 1.0], "untouched");
    }
    /// into transparent, then top (first) over it — `out = src*a +
    /// dst*(1-a)`, `out_a = a + dst_a*(1-a)` (premultiplied source).
    #[test]
    fn composite_tracks_matches_alpha_over_math() {
        let Texture::Cpu(top) = &solid_texture(0.5, 0.25, 0.125, 0.5) else {
            unreachable!()
        };
        let Texture::Cpu(bottom) = &solid_texture(1.0, 1.0, 1.0, 0.75) else {
            unreachable!()
        };
        let frames = vec![top.clone(), bottom.clone()];
        let expected = [0.625f32, 0.5, 0.4375, 0.875];

        // bottom over transparent: (0.75, 0.75, 0.75, 0.75), then top over:
        // r = 0.5*0.5 + 0.75*0.5, g = 0.25*0.5 + 0.75*0.5,
        // b = 0.125*0.5 + 0.75*0.5, a = 0.5 + 0.75*0.5.
        let out = composite_tracks(frames.clone(), (2, 1));
        let pixel = first_pixel(&Texture::wrap_frame(out));
        for (got, want) in pixel.iter().zip(expected) {
            assert!((got - want).abs() < 1e-4, "CPU composite: expected {want}, got {got}");
        }

        // Same math through the GPU pass when a device is available.
        if let Some(ctx) = oak_core::backend::GpuContext::shared() {
            let gpu_out = composite_tracks_gpu(&ctx, &frames, (2, 1)).expect("GPU composite");
            let pixel = first_pixel(&Texture::wrap_frame(gpu_out));
            for (got, want) in pixel.iter().zip(expected) {
                assert!((got - want).abs() < 1e-3, "GPU composite: expected {want}, got {got}");
            }
        }
    }

    /// End-to-end chromakey through the real GPU path: a solid opaque
    /// green frame keyed on the node's default green key leaves every
    /// pixel at zero (the C++ `ColorTransformJob` + `chromakey.frag`
    /// math: `colorclose` returns 0 at the key color, so the
    /// shadows/highlights transform yields `mask = 0` and `col *= mask`
    /// blanks the frame). Exercises the OCIO splice in
    /// [`super::process_shader_job`]: the shader's `%1` marker is filled
    /// with the real `SceneLinearToCIEXYZ_d65` GLSL generated from the
    /// default OCIO config, and the tolerance uniforms reach the shader
    /// under their (fixed) input-id spelling.
    #[test]
    fn gpu_chromakey_keys_green_with_ocio_stub() {
        let Some(ctx) = oak_core::backend::GpuContext::shared() else {
            eprintln!("no adapter; skipping");
            return;
        };
        // Install the process-wide default config (the C++
        // `ColorManager::SetUpDefaultConfig` startup step; color.rs tests
        // do the same). Without it the OCIO stub cannot be generated and
        // the job falls back to the input pass-through.
        if oak_core::color::set_up_default_config().is_err() {
            eprintln!("bundled OCIO missing; skipping");
            return;
        }
        if oak_core::color::ocio_function_shader(
            "SceneLinearToCIEXYZ_d65",
            "scene_linear",
            "cie_xyz_d65_interchange",
        )
            .is_none()
        {
            eprintln!("no OCIO config; skipping");
            return;
        }

        let size = (16, 16);
        let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
        for px in frame.data.chunks_exact_mut(16) {
            for (c, v) in px.chunks_exact_mut(4).zip([0.0f32, 1.0, 0.0, 1.0]) {
                c.copy_from_slice(&v.to_le_bytes());
            }
        }

        let src = ctx.create_texture(size.0, size.1).unwrap();
        ctx.upload(src, &frame).unwrap();
        let input = Texture::Gpu {
            token: src,
            backend: ctx.kind(),
            width: size.0,
            height: size.1,
            format: PixelFormat::F32,
            ctx: ctx.clone(),
        };

        let mut params = NodeValueRow::new();
        params.insert("tex_in".into(), NodeValue::Texture(oak_node::handle::make_owned(input)));
        params.insert("color_key".into(), NodeValue::Color([0.0, 1.0, 0.0, 1.0]));
        params.insert("lower_tolerance_in".into(), NodeValue::Float(5.0));
        params.insert("upper_tolerance_in".into(), NodeValue::Float(25.0));
        params.insert("mask_only_in".into(), NodeValue::Boolean(false));
        params.insert("invert_in".into(), NodeValue::Boolean(false));
        params.insert("shadows_in".into(), NodeValue::Float(100.0));
        params.insert("highlights_in".into(), NodeValue::Float(100.0));

        let payload = ShaderJobPayload {
            node_id: oak_node::id::NodeId::from_identity(1).unwrap(),
            time: Rational::new(0, 1),
            iterations: 1,
            type_id: "org.olivevideoeditor.Olive.chromakey".into(),
            shader_id: String::new(),
            effect_input: "tex_in".into(),
            params,
            iterative_input: String::new(),
        };
        let rendered = RenderEvalHooks::new()
            .process_shader_job(&payload)
            .expect("chromakey renders on the GPU");

        let Texture::Gpu { token: dst, .. } = rendered else {
            panic!("expected a GPU texture");
        };
        let out = ctx.download(dst).unwrap();
        for px in out.data.chunks_exact(16) {
            for (c, v) in px.chunks_exact(4).enumerate() {
                let got = f32::from_le_bytes(v.try_into().unwrap());
                assert!(
                    got.abs() < 1e-4,
                    "channel {c}: keyed green must be fully transparent (got {got})"
                );
            }
        }
        ctx.destroy_texture(dst);
        ctx.destroy_texture(src);
    }

    /// Pixel readback helper for the GPU verification tests.
    fn pixel_at(frame: &Frame, x: usize, y: usize) -> [f32; 4] {
        let at = (y * frame.width as usize + x) * 16;
        let mut out = [0f32; 4];
        for c in 0..4 {
            out[c] = f32::from_le_bytes(frame.data[at + c * 4..at + c * 4 + 4].try_into().unwrap());
        }
        out
    }

    /// Build a solid-color F32 CPU frame.
    fn filled_frame(size: (i32, i32), rgba: [f32; 4]) -> Texture {
        let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
        for px in frame.data.chunks_exact_mut(16) {
            for (c, v) in px.chunks_exact_mut(4).zip(rgba) {
                c.copy_from_slice(&v.to_le_bytes());
            }
        }
        Texture::wrap_frame(frame)
    }

    /// Evaluate one node's `value()` against `inputs` and resolve the
    /// resulting table through the hooks (the full value -> job -> GPU
    /// run -> texture path), reading the frame back while the table —
    /// which owns the texture's GPU token — is still alive.
    fn eval_node_row(
        type_id: &str,
        inputs: NodeValueRow,
        frame_size: Option<(i32, i32)>,
    ) -> Frame {
        use oak_node::traverser::RenderHooks;
        let (core, behavior) = oak_node::factory::Factory::global()
            .create_any(type_id)
            .expect("node type registered");
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let mut hooks = RenderEvalHooks::new();
        hooks.frame_size = frame_size;
        hooks.resolve(oak_node::id::NodeId::INVALID, &inputs, &mut table);
        let Some(NodeValue::Texture(handle)) =
            table.get(oak_node::value::ValueType::Texture)
        else {
            panic!("{type_id}: no texture produced");
        };
        if handle.ctx.is_null() {
            panic!("{type_id}: null texture produced");
        }
        let tex = (unsafe { oak_node::handle::get_checked::<Texture>(handle) })
            .expect("resolved texture");
        assert!(
            matches!(tex, Texture::Gpu { .. }),
            "{type_id}: the job must render on the GPU"
        );
        tex.to_frame().expect("readback")
    }

    /// Merge over the real GPU path: the merge node declares no effect
    /// input, so base and blend must bind by name for the alpha-over to
    /// run at all. Red base + half-alpha green blend -> (0.5, 1, 0, 1).
    #[test]
    fn gpu_merge_alpha_over_binds_base_and_blend() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert(
            "base_in".into(),
            texture_value(filled_frame((16, 16), [1.0, 0.0, 0.0, 1.0])),
        );
        inputs.insert(
            "blend_in".into(),
            texture_value(filled_frame((16, 16), [0.0, 1.0, 0.0, 0.5])),
        );
        let frame = eval_node_row("org.olivevideoeditor.Olive.merge", inputs, None);
        assert_eq!(frame.width, 16, "the pass size follows the base");
        for (x, y) in [(0, 0), (8, 8), (15, 15)] {
            let px = pixel_at(&frame, x, y);
            let want = [0.5, 1.0, 0.0, 1.0];
            for (c, (got, w)) in px.iter().zip(want).enumerate() {
                assert!(
                    (got - w).abs() < 1e-4,
                    "merge ({x},{y}) ch{c}: got {got}, want {w}"
                );
            }
        }
    }

    /// A bare generator (no input connected) renders at the hook's frame
    /// size instead of a 1x1 the composite step would drop.
    #[test]
    fn gpu_generator_without_input_renders_at_frame_size() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert("color_in".into(), NodeValue::Color([0.2, 0.4, 0.6, 1.0]));
        let frame = eval_node_row(
            "org.olivevideoeditor.Olive.solidgenerator",
            inputs,
            Some((8, 4)),
        );
        assert_eq!((frame.width, frame.height), (8, 4));
        let px = pixel_at(&frame, 3, 2);
        for (c, w) in [0.2f32, 0.4, 0.6, 1.0].iter().enumerate() {
            assert!(
                (px[c] - w).abs() < 1e-4,
                "solid ch{c}: got {}, want {w}",
                px[c]
            );
        }
    }

    /// Generator-over-base ("mrg"): the nested generator job resolves
    /// recursively and alpha-overs onto the base — the pentagon is green
    /// (the generated layer), the corners stay red (the base).
    #[test]
    fn gpu_generator_over_base_composites_nested_job() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert(
            "base_in".into(),
            texture_value(filled_frame((512, 512), [1.0, 0.0, 0.0, 1.0])),
        );
        inputs.insert("color_in".into(), NodeValue::Color([0.0, 1.0, 0.0, 1.0]));
        let frame = eval_node_row("org.olivevideoeditor.Olive.polygon", inputs, Some((512, 512)));
        assert_eq!((frame.width, frame.height), (512, 512));
        let center = pixel_at(&frame, 256, 256);
        assert!(
            center[1] > 0.9 && center[0] < 0.1,
            "pentagon center is the generated green: {center:?}"
        );
        let corner = pixel_at(&frame, 5, 5);
        assert!(
            corner[0] > 0.9 && corner[1] < 0.1,
            "corner keeps the red base: {corner:?}"
        );
    }

    /// Drop shadow with non-zero softness: three iterations feed back
    /// through `previous_iteration_in`; the blurred shadow lands offset
    /// from the source, widening the non-transparent area.
    #[test]
    fn gpu_dropshadow_softness_blurs_and_offsets() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        // 16x16 transparent frame with an opaque 4x4 square at (4,4).
        let mut frame = generate_frame(Rational::new(0, 1), (16, 16), PixelFormat::F32).unwrap();
        for y in 4..8usize {
            for x in 4..8usize {
                let at = (y * 16 + x) * 16;
                for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
                    frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
        inputs.insert("color_in".into(), NodeValue::Color([0.0, 0.0, 0.0, 1.0]));
        inputs.insert("distance_in".into(), NodeValue::Float(4.0));
        inputs.insert("angle_in".into(), NodeValue::Float(45.0));
        inputs.insert("radius_in".into(), NodeValue::Float(2.0));
        inputs.insert("opacity_in".into(), NodeValue::Float(1.0));
        inputs.insert("fast_in".into(), NodeValue::Boolean(false));

        let out_frame = eval_node_row("org.olivevideoeditor.Olive.dropshadow", inputs, None);
        assert_eq!((out_frame.width, out_frame.height), (16, 16));
        let covered = out_frame
            .data
            .chunks_exact(16)
            .filter(|px| f32::from_le_bytes(px[12..16].try_into().unwrap()) > 0.01)
            .count();
        assert!(
            covered > 16,
            "the offset blurred shadow must widen the covered area beyond the 4x4 source square: {covered}"
        );
    }
    /// Transform over the real GPU path: the fragment-side inverse
    /// sampling applies the node's matrix for real — a +3px x
    /// translation moves the white pixel from (2, 3) to (5, 3).
    #[test]
    fn gpu_transform_translates_pixels() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        // 8x8 black frame with one white pixel at (2, 3).
        let mut frame = generate_frame(Rational::new(0, 1), (8, 8), PixelFormat::F32).unwrap();
        let at = (3 * 8 + 2) * 16;
        for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
            frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
        inputs.insert("pos_in".into(), NodeValue::Vec2([3.0, 0.0]));

        let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
        assert_eq!((out.width, out.height), (8, 8));
        assert_eq!(pixel_at(&out, 2, 3), [0.0, 0.0, 0.0, 0.0], "source spot vacated");
        assert_eq!(pixel_at(&out, 5, 3), [1.0, 1.0, 1.0, 1.0], "pixel moved +3 in x");
        assert_eq!(pixel_at(&out, 0, 0), [0.0, 0.0, 0.0, 0.0]);
    }

    /// Shape generator over the real GPU path: a centered 8x8 rectangle
    /// on a 16x16 frame fills exactly the middle block (pixel centers
    /// with texcoord in [0.25, 0.75)), everything outside stays
    /// transparent.
    #[test]
    fn gpu_shape_rectangle_draws_centered_block() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert("pos_in".into(), NodeValue::Vec2([0.0, 0.0]));
        inputs.insert("size_in".into(), NodeValue::Vec2([8.0, 8.0]));
        inputs.insert("color_in".into(), NodeValue::Color([1.0, 0.0, 0.0, 1.0]));
        inputs.insert("type_in".into(), NodeValue::Combo(0));
        inputs.insert("radius_in".into(), NodeValue::Float(20.0));

        let frame = eval_node_row("org.olivevideoeditor.Olive.shape", inputs, Some((16, 16)));
        assert_eq!((frame.width, frame.height), (16, 16));
        for (x, y, inside) in [(8, 8, true), (4, 4, true), (11, 11, true), (0, 0, false), (3, 8, false), (12, 8, false), (15, 15, false)] {
            let px = pixel_at(&frame, x, y);
            if inside {
                assert_eq!(px, [1.0, 0.0, 0.0, 1.0], "({x},{y}) inside the rect");
            } else {
                assert_eq!(px, [0.0, 0.0, 0.0, 0.0], "({x},{y}) outside the rect");
            }
        }
    }

    /// Shape generator, the ellipse and rounded-rectangle dispatches:
    /// the ellipse fills the center and fades out before the corners;
    /// the rounded rect fills the middle but cuts the corner at (4,4)
    /// (radius 20 clamps to half the 8px size).
    #[test]
    fn gpu_shape_ellipse_and_rounded_rect() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let base_inputs = || {
            let mut inputs = NodeValueRow::new();
            inputs.insert("pos_in".into(), NodeValue::Vec2([0.0, 0.0]));
            inputs.insert("size_in".into(), NodeValue::Vec2([8.0, 8.0]));
            inputs.insert("color_in".into(), NodeValue::Color([1.0, 0.0, 0.0, 1.0]));
            inputs.insert("radius_in".into(), NodeValue::Float(20.0));
            inputs
        };

        let mut ellipse = base_inputs();
        ellipse.insert("type_in".into(), NodeValue::Combo(1));
        let frame = eval_node_row("org.olivevideoeditor.Olive.shape", ellipse, Some((16, 16)));
        assert_eq!(pixel_at(&frame, 8, 8), [1.0, 0.0, 0.0, 1.0], "ellipse center");
        assert_eq!(pixel_at(&frame, 0, 0), [0.0, 0.0, 0.0, 0.0], "ellipse corner faded out");

        let mut rounded = base_inputs();
        rounded.insert("type_in".into(), NodeValue::Combo(2));
        let frame = eval_node_row("org.olivevideoeditor.Olive.shape", rounded, Some((16, 16)));
        assert_eq!(pixel_at(&frame, 8, 8), [1.0, 0.0, 0.0, 1.0], "rounded rect middle");
        assert_eq!(pixel_at(&frame, 6, 6), [1.0, 0.0, 0.0, 1.0], "rounded rect inside the corner arc");
        assert_eq!(pixel_at(&frame, 0, 0), [0.0, 0.0, 0.0, 0.0], "rounded rect far corner");
        assert!(
            pixel_at(&frame, 4, 4)[3] < 0.1,
            "rounded rect corner (4,4) is cut by the arc: {:?}",
            pixel_at(&frame, 4, 4)
        );
    }

    /// Despill over the real GPU path: green-screen AVERAGE caps the
    /// green channel at the red/blue average (the shader's method
    /// dispatch must survive translation).
    #[test]
    fn gpu_despill_average_caps_green() {
        if oak_core::backend::GpuContext::shared().is_none() {
            eprintln!("no adapter; skipping");
            return;
        }
        let mut inputs = NodeValueRow::new();
        inputs.insert(
            "tex_in".into(),
            texture_value(filled_frame((4, 4), [0.2, 0.9, 0.3, 1.0])),
        );
        inputs.insert("color_in".into(), NodeValue::Combo(0));
        inputs.insert("method_in".into(), NodeValue::Combo(0));
        inputs.insert("preserve_luminance_input".into(), NodeValue::Boolean(false));

        let frame = eval_node_row("org.olivevideoeditor.Olive.despill", inputs, None);
        assert_eq!((frame.width, frame.height), (4, 4));
        let px = pixel_at(&frame, 2, 2);
        let want = [0.2f32, 0.25, 0.3, 1.0];
        for (c, w) in want.iter().enumerate() {
            assert!(
                (px[c] - w).abs() < 1e-4,
                "despill ch{c}: got {}, want {w}",
                px[c]
            );
        }
    }

}

