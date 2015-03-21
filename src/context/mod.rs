use gl;
use libc;

use std::env;
use std::mem;
use std::ptr;
use std::collections::hash_state::DefaultState;
use std::collections::HashMap;
use std::default::Default;
use std::cell::{RefCell, RefMut};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};

use GliumCreationError;
use backend::Backend;
use version;
use version::Api;

use fbo;
use sampler_object;
use uniforms;
use util;
use vertex_array_object;

pub use self::capabilities::Capabilities;
pub use self::extensions::ExtensionsList;
pub use self::state::GLState;
pub use version::Version as GlVersion;      // TODO: remove

mod capabilities;
mod extensions;
mod state;

pub struct Context {
    gl: gl::Gl,
    state: RefCell<GLState>,
    version: GlVersion,
    extensions: ExtensionsList,
    capabilities: Capabilities,
    shared_debug_output: Box<SharedDebugOutput>,

    backend: RefCell<Box<Backend>>,
    check_current_context: bool,

    // we maintain a list of FBOs
    // the option is here to destroy the container
    pub framebuffer_objects: Option<fbo::FramebuffersContainer>,

    pub vertex_array_objects: vertex_array_object::VertexAttributesSystem,

    // we maintain a list of samplers for each possible behavior
    pub samplers: RefCell<HashMap<uniforms::SamplerBehavior, sampler_object::SamplerObject, 
                          DefaultState<util::FnvHasher>>>,
}

pub struct CommandContext<'a, 'b> {
    pub gl: &'a gl::Gl,
    pub state: RefMut<'b, GLState>,
    pub version: &'a GlVersion,
    pub extensions: &'a ExtensionsList,
    pub capabilities: &'a Capabilities,
    pub shared_debug_output: &'a SharedDebugOutput,
}

/// Struct shared with the debug output callback.
pub struct SharedDebugOutput {
    /// Whether debug output should report errors
    pub report_errors: AtomicBool,
}

impl SharedDebugOutput {
    pub fn new() -> Box<SharedDebugOutput> {
        Box::new(SharedDebugOutput {
            report_errors: AtomicBool::new(true),
        })
    }
}

impl Context {
    pub fn new<B>(backend: B, check_current_context: bool)
                  -> Result<Context, GliumCreationError>
                  where B: Backend + 'static
    {
        unsafe { backend.make_current() };
        let gl = gl::Gl::load_with(|symbol| unsafe { backend.get_proc_address(symbol) });

        let gl_state = RefCell::new(Default::default());
        let version = version::get_gl_version(&gl);
        let extensions = extensions::get_extensions(&gl);
        let capabilities = capabilities::get_capabilities(&gl, &version, &extensions);

        let shared_debug = SharedDebugOutput::new();

        {
            let mut ctxt = CommandContext {
                gl: &gl,
                state: gl_state.borrow_mut(),
                version: &version,
                extensions: &extensions,
                capabilities: &capabilities,
                shared_debug_output: &shared_debug,
            };

            try!(check_gl_compatibility(&mut ctxt));
            init_debug_callback(&mut ctxt);
        }

        Ok(Context {
            gl: gl,
            state: gl_state,
            version: version,
            extensions: extensions,
            capabilities: capabilities,
            shared_debug_output: shared_debug,
            backend: RefCell::new(Box::new(backend)),
            check_current_context: check_current_context,
            framebuffer_objects: Some(fbo::FramebuffersContainer::new()),
            vertex_array_objects: vertex_array_object::VertexAttributesSystem::new(),
            samplers: RefCell::new(HashMap::with_hash_state(Default::default())),
        })
    }

    pub fn get_framebuffer_dimensions(&self) -> (u32, u32) {
        self.backend.borrow().get_framebuffer_dimensions()
    }

    pub fn make_current<'a>(&'a self) -> CommandContext<'a, 'a> {
        if self.check_current_context {
            let backend = self.backend.borrow();
            if !backend.is_current() {
                unsafe { backend.make_current() };
            }
        }

        CommandContext {
            gl: &self.gl,
            state: self.state.borrow_mut(),
            version: &self.version,
            extensions: &self.extensions,
            capabilities: &self.capabilities,
            shared_debug_output: &*self.shared_debug_output,
        }
    }

    pub fn rebuild<B>(&self, new_backend: B)
                      -> Result<(), GliumCreationError>
                      where B: Backend + 'static
    {
        unsafe { new_backend.make_current() };

        *self.state.borrow_mut() = Default::default();
        // FIXME: verify version, capabilities and extensions
        *self.backend.borrow_mut() = Box::new(new_backend);

        Ok(())
    }

    pub fn swap_buffers(&self) {
        let backend = self.backend.borrow();

        if self.check_current_context {
            if !backend.is_current() {
                unsafe { backend.make_current() };
            }
        }

        // this is necessary on Windows 8, or nothing is being displayed
        unsafe { self.gl.Flush(); }

        // swapping
        backend.swap_buffers();
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub fn get_version(&self) -> &GlVersion {
        &self.version
    }

    pub fn get_extensions(&self) -> &ExtensionsList {
        &self.extensions
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            // this is the code of make_current duplicated here because we can't borrow
            // `self` twice
            if self.check_current_context {
                let backend = self.backend.borrow();
                if !backend.is_current() {
                    backend.make_current();
                }
            }

            let mut ctxt = CommandContext {
                gl: &self.gl,
                state: self.state.borrow_mut(),
                version: &self.version,
                extensions: &self.extensions,
                capabilities: &self.capabilities,
                shared_debug_output: &*self.shared_debug_output,
            };

            let fbos = self.framebuffer_objects.take();
            fbos.unwrap().cleanup(&mut ctxt);

            self.vertex_array_objects.cleanup(&mut ctxt);

            let mut samplers = self.samplers.borrow_mut();
            for (_, s) in samplers.drain() {
                s.destroy(&mut ctxt);
            }

            // disabling callback
            if ctxt.state.enabled_debug_output != Some(false) {
                if ctxt.version >= &GlVersion(Api::Gl, 4,5) || ctxt.extensions.gl_khr_debug {
                    ctxt.gl.Disable(gl::DEBUG_OUTPUT);
                } else if ctxt.extensions.gl_arb_debug_output {
                    ctxt.gl.DebugMessageCallbackARB(mem::transmute(0usize),
                                                    ptr::null());
                }

                ctxt.state.enabled_debug_output = Some(false);
                ctxt.gl.Finish();
            }
        }
    }
}

fn check_gl_compatibility(ctxt: &mut CommandContext) -> Result<(), GliumCreationError> {
    let mut result = Vec::new();

    if !(ctxt.version >= &GlVersion(Api::Gl, 1, 5)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 2, 0)) &&
        (!ctxt.extensions.gl_arb_vertex_buffer_object || !ctxt.extensions.gl_arb_map_buffer_range)
    {
        result.push("OpenGL implementation doesn't support buffer objects");
    }

    if !(ctxt.version >= &GlVersion(Api::Gl, 2, 0)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 2, 0)) &&
        (!ctxt.extensions.gl_arb_shader_objects ||
            !ctxt.extensions.gl_arb_vertex_shader || !ctxt.extensions.gl_arb_fragment_shader)
    {
        result.push("OpenGL implementation doesn't support vertex/fragment shaders");
    }

    if !ctxt.extensions.gl_ext_framebuffer_object && !(ctxt.version >= &GlVersion(Api::Gl, 3, 0)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 2, 0))
    {
        result.push("OpenGL implementation doesn't support framebuffers");
    }

    if !ctxt.extensions.gl_ext_framebuffer_blit && !(ctxt.version >= &GlVersion(Api::Gl, 3, 0)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 2, 0))
    {
        result.push("OpenGL implementation doesn't support blitting framebuffers");
    }

    if !ctxt.extensions.gl_arb_vertex_array_object &&
        !ctxt.extensions.gl_apple_vertex_array_object &&
        !ctxt.extensions.gl_oes_vertex_array_object &&
        !(ctxt.version >= &GlVersion(Api::Gl, 3, 0)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 3, 0))
    {
        result.push("OpenGL implementation doesn't support vertex array objects");
    }

    if cfg!(feature = "gl_uniform_blocks") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 1)) &&
        !ctxt.extensions.gl_arb_uniform_buffer_object
    {
        result.push("OpenGL implementation doesn't support uniform blocks");
    }

    if cfg!(feature = "gl_sync") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 2)) &&
        !(ctxt.version >= &GlVersion(Api::GlEs, 3, 0)) && !ctxt.extensions.gl_arb_sync
    {
        result.push("OpenGL implementation doesn't support synchronization objects");
    }

    if cfg!(feature = "gl_persistent_mapping") && !(ctxt.version >= &GlVersion(Api::Gl, 4, 4)) &&
        !ctxt.extensions.gl_arb_buffer_storage
    {
        result.push("OpenGL implementation doesn't support persistent mapping");
    }

    if cfg!(feature = "gl_program_binary") && !(ctxt.version >= &GlVersion(Api::Gl, 4, 1)) &&
        !ctxt.extensions.gl_arb_get_programy_binary
    {
        result.push("OpenGL implementation doesn't support program binary");
    }

    if cfg!(feature = "gl_tessellation") && !(ctxt.version >= &GlVersion(Api::Gl, 4, 0)) &&
        !ctxt.extensions.gl_arb_tessellation_shader
    {
        result.push("OpenGL implementation doesn't support tessellation");
    }

    if cfg!(feature = "gl_instancing") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 3)) &&
        !ctxt.extensions.gl_arb_instanced_arrays
    {
        result.push("OpenGL implementation doesn't support instancing");
    }

    if cfg!(feature = "gl_integral_textures") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 0)) &&
        !ctxt.extensions.gl_ext_texture_integer
    {
        result.push("OpenGL implementation doesn't support integral textures");
    }

    if cfg!(feature = "gl_depth_textures") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 0)) &&
        (!ctxt.extensions.gl_arb_depth_texture || !ctxt.extensions.gl_ext_packed_depth_stencil)
    {
        result.push("OpenGL implementation doesn't support depth or depth-stencil textures");
    }

    if cfg!(feature = "gl_stencil_textures") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 0))
    {
        result.push("OpenGL implementation doesn't support stencil textures");
    }

    if cfg!(feature = "gl_texture_multisample") && !(ctxt.version >= &GlVersion(Api::Gl, 3, 2))
    {
        result.push("OpenGL implementation doesn't support multisample textures");
    }

    if cfg!(feature = "gl_texture_multisample_array") &&
        !(ctxt.version >= &GlVersion(Api::Gl, 3, 2))
    {
        result.push("OpenGL implementation doesn't support arrays of multisample textures");
    }

    if result.len() == 0 {
        Ok(())
    } else {
        Err(GliumCreationError::IncompatibleOpenGl(result.connect("\n")))
    }
}

fn init_debug_callback(mut ctxt: &mut CommandContext) {
    if cfg!(ndebug) {
        return;
    }

    if env::var("GLIUM_DISABLE_DEBUG_OUTPUT").is_ok() {
        return;
    }

    // this is the C callback
    extern "system" fn callback_wrapper(source: gl::types::GLenum, ty: gl::types::GLenum,
        id: gl::types::GLuint, severity: gl::types::GLenum, _length: gl::types::GLsizei,
        message: *const gl::types::GLchar, user_param: *mut libc::c_void)
    {
        let user_param = user_param as *const SharedDebugOutput;
        let user_param = unsafe { user_param.as_ref().unwrap() };

        if (severity == gl::DEBUG_SEVERITY_HIGH || severity == gl::DEBUG_SEVERITY_MEDIUM) && 
           (ty == gl::DEBUG_TYPE_ERROR || ty == gl::DEBUG_TYPE_UNDEFINED_BEHAVIOR ||
            ty == gl::DEBUG_TYPE_PORTABILITY || ty == gl::DEBUG_TYPE_DEPRECATED_BEHAVIOR)
        {
            if user_param.report_errors.load(Ordering::Relaxed) {
                let message = unsafe {
                    String::from_utf8(CStr::from_ptr(message).to_bytes().to_vec()).unwrap()
                };

                panic!("Debug message with high or medium severity: `{}`.\n\
                        Please report this error: https://github.com/tomaka/glium/issues",
                        message);
            }
        }
    }

    struct SharedDebugOutputPtr(*const SharedDebugOutput);
    unsafe impl Send for SharedDebugOutputPtr {}
    let shared_debug_output_ptr = SharedDebugOutputPtr(ctxt.shared_debug_output);

    unsafe {
        if ctxt.version >= &GlVersion(Api::Gl, 4,5) || ctxt.extensions.gl_khr_debug ||
            ctxt.extensions.gl_arb_debug_output
        {
            if ctxt.state.enabled_debug_output_synchronous != true {
                ctxt.gl.Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);
                ctxt.state.enabled_debug_output_synchronous = true;
            }

            if ctxt.version >= &GlVersion(Api::Gl, 4, 5) ||
                (ctxt.version >= &GlVersion(Api::Gl, 1, 0) && ctxt.extensions.gl_khr_debug)
            {
                ctxt.gl.DebugMessageCallback(callback_wrapper, shared_debug_output_ptr.0
                                                                 as *const libc::c_void);
                ctxt.gl.DebugMessageControl(gl::DONT_CARE, gl::DONT_CARE, gl::DONT_CARE, 0,
                                            ptr::null(), gl::TRUE);

                if ctxt.state.enabled_debug_output != Some(true) {
                    ctxt.gl.Enable(gl::DEBUG_OUTPUT);
                    ctxt.state.enabled_debug_output = Some(true);
                }

            } else if ctxt.version >= &GlVersion(Api::GlEs, 2, 0) &&
                ctxt.extensions.gl_khr_debug
            {
                ctxt.gl.DebugMessageCallbackKHR(callback_wrapper, shared_debug_output_ptr.0
                                                                 as *const libc::c_void);
                ctxt.gl.DebugMessageControlKHR(gl::DONT_CARE, gl::DONT_CARE, gl::DONT_CARE, 0,
                                               ptr::null(), gl::TRUE);

                if ctxt.state.enabled_debug_output != Some(true) {
                    ctxt.gl.Enable(gl::DEBUG_OUTPUT);
                    ctxt.state.enabled_debug_output = Some(true);
                }

            } else {
                ctxt.gl.DebugMessageCallbackARB(callback_wrapper, shared_debug_output_ptr.0
                                                                    as *const libc::c_void);
                ctxt.gl.DebugMessageControlARB(gl::DONT_CARE, gl::DONT_CARE, gl::DONT_CARE,
                                               0, ptr::null(), gl::TRUE);

                ctxt.state.enabled_debug_output = Some(true);
            }
        }
    }
}
