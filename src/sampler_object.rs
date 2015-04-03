use DrawError;

use uniforms::SamplerBehavior;

use std::collections::HashMap;

use gl;
use context::CommandContext;
use version::Version;
use version::Api;
use GlObject;
use ToGlEnum;

/// An OpenGL sampler object.
pub struct SamplerObject {
    id: gl::types::GLuint,
    destroyed: bool,
}

impl SamplerObject {
    /// Builds a new sampler object.
    pub fn new(ctxt: &mut CommandContext, behavior: &SamplerBehavior) -> SamplerObject {
        // making sure that the backend supports samplers
        assert!(ctxt.version >= &Version(Api::Gl, 3, 2) ||
                ctxt.extensions.gl_arb_sampler_objects);

        let sampler = unsafe {
            use std::mem;
            let mut sampler: gl::types::GLuint = mem::uninitialized();
            ctxt.gl.GenSamplers(1, &mut sampler);
            sampler
        };

        unsafe {
            ctxt.gl.SamplerParameteri(sampler, gl::TEXTURE_WRAP_S,
                                      behavior.wrap_function.0.to_glenum() as gl::types::GLint);
            ctxt.gl.SamplerParameteri(sampler, gl::TEXTURE_WRAP_T,
                                      behavior.wrap_function.1.to_glenum() as gl::types::GLint);
            ctxt.gl.SamplerParameteri(sampler, gl::TEXTURE_WRAP_R,
                                      behavior.wrap_function.2.to_glenum() as gl::types::GLint);
            ctxt.gl.SamplerParameteri(sampler, gl::TEXTURE_MIN_FILTER,
                                      behavior.minify_filter.to_glenum() as gl::types::GLint);
            ctxt.gl.SamplerParameteri(sampler, gl::TEXTURE_MAG_FILTER,
                                      behavior.magnify_filter.to_glenum() as gl::types::GLint);

            if let Some(max_value) = ctxt.capabilities.max_texture_max_anisotropy {
                let value = if behavior.max_anisotropy as f32 > max_value {
                    max_value
                } else {
                    behavior.max_anisotropy as f32
                };

                ctxt.gl.SamplerParameterf(sampler, gl::TEXTURE_MAX_ANISOTROPY_EXT, value);
            }
        }

        SamplerObject {
            id: sampler,
            destroyed: false,
        }
    }

    /// 
    pub fn destroy(mut self, ctxt: &mut CommandContext) {
        self.destroyed = true;

        unsafe {
            ctxt.gl.DeleteSamplers(1, [self.id].as_ptr());
        }
    }
}

impl GlObject for SamplerObject {
    type Id = gl::types::GLuint;

    fn get_id(&self) -> gl::types::GLuint {
        self.id
    }
}

impl Drop for SamplerObject {
    fn drop(&mut self) {
        assert!(self.destroyed);
    }
}

/// Returns the sampler corresponding to the given behavior, or a draw error if
/// samplers are not supported.
pub fn get_sampler(ctxt: &mut CommandContext,
                   samplers: &mut HashMap<SamplerBehavior, SamplerObject>,
                   behavior: &SamplerBehavior)
                   -> Result<gl::types::GLuint, DrawError>
{
    // checking for compatibility
    if ctxt.version < &Version(Api::Gl, 3, 2) && !ctxt.extensions.gl_arb_sampler_objects {
        return Err(DrawError::SamplersNotSupported);
    }

    // looking for an existing sampler
    match samplers.get(behavior) {
        Some(obj) => return Ok(obj.get_id()),
        None => ()
    };

    // builds a new sampler
    let sampler = SamplerObject::new(ctxt, behavior);
    let id = sampler.get_id();
    samplers.insert(behavior.clone(), sampler);
    Ok(id)
}
