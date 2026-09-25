//! Simplified EGL + GLES2 renderer — single texture, fullscreen quad only.
//!
//! Extracted from nereid-compositor's render/mod.rs with these simplifications:
//! - Only ONE texture slot (no HashMap, no TextureId)
//! - No swap_rb uniform (compositor pre-composes RGBA)
//! - No alpha uniform (always fully opaque)
//! - upload() always targets the single texture
//! - draw() always draws it as a fullscreen quad

use anyhow::{Context, Result};
use std::ffi::{c_char, c_void};
use tracing::info;

use crate::hwc::HwcBackend;

// --- EGL type aliases (loaded at runtime via libEGL.so) ---

type EGLDisplay = *mut c_void;
type EGLSurface = *mut c_void;
type EGLContext = *mut c_void;
type EGLConfig = *mut c_void;
type EGLint = i32;
type EGLBoolean = u32;
type EGLNativeDisplayType = *mut c_void;
type EGLNativeWindowType = *mut c_void;

const EGL_DEFAULT_DISPLAY: EGLNativeDisplayType = std::ptr::null_mut();
const EGL_NO_CONTEXT: EGLContext = std::ptr::null_mut();
const EGL_NO_SURFACE: EGLSurface = std::ptr::null_mut();
const EGL_NONE: EGLint = 0x3038;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_GREEN_SIZE: EGLint = 0x3025;
const EGL_BLUE_SIZE: EGLint = 0x3026;
const EGL_ALPHA_SIZE: EGLint = 0x3028;
const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_WINDOW_BIT: EGLint = 0x0004;
const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
const EGL_TRUE: EGLBoolean = 1;

// --- GLES2 constants ---

const GL_COLOR_BUFFER_BIT: u32 = 0x00004000;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FLOAT: u32 = 0x1406;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_TRUE: i32 = 1;

const VERTEX_SHADER: &str = r#"
attribute vec2 a_pos;
attribute vec2 a_texcoord;
varying vec2 v_texcoord;
void main() {
    gl_Position = vec4(a_pos, 0.0, 1.0);
    v_texcoord = a_texcoord;
}
"#;

const FRAGMENT_SHADER: &str = r#"
precision mediump float;
varying vec2 v_texcoord;
uniform sampler2D u_texture;
void main() {
    vec4 c = texture2D(u_texture, v_texcoord);
    gl_FragColor = c;
}
"#;

/// GLES2 function pointers loaded at runtime.
struct GlFns {
    viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    clear: unsafe extern "C" fn(u32),
    gen_textures: unsafe extern "C" fn(i32, *mut u32),
    bind_texture: unsafe extern "C" fn(u32, u32),
    tex_parameteri: unsafe extern "C" fn(u32, u32, i32),
    tex_image_2d:
        unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    tex_sub_image_2d:
        unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    delete_textures: unsafe extern "C" fn(i32, *const u32),
    active_texture: unsafe extern "C" fn(u32),
    create_shader: unsafe extern "C" fn(u32) -> u32,
    shader_source:
        unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32),
    compile_shader: unsafe extern "C" fn(u32),
    get_shaderiv: unsafe extern "C" fn(u32, u32, *mut i32),
    get_shader_info_log:
        unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char),
    delete_shader: unsafe extern "C" fn(u32),
    create_program: unsafe extern "C" fn() -> u32,
    attach_shader: unsafe extern "C" fn(u32, u32),
    link_program: unsafe extern "C" fn(u32),
    get_programiv: unsafe extern "C" fn(u32, u32, *mut i32),
    get_program_info_log:
        unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char),
    delete_program: unsafe extern "C" fn(u32),
    use_program: unsafe extern "C" fn(u32),
    get_attrib_location: unsafe extern "C" fn(u32, *const c_char) -> i32,
    enable_vertex_attrib_array: unsafe extern "C" fn(u32),
    disable_vertex_attrib_array: unsafe extern "C" fn(u32),
    vertex_attrib_pointer:
        unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void),
    gen_buffers: unsafe extern "C" fn(i32, *mut u32),
    bind_buffer: unsafe extern "C" fn(u32, u32),
    buffer_data: unsafe extern "C" fn(u32, isize, *const c_void, u32),
    delete_buffers: unsafe extern "C" fn(i32, *const u32),
    draw_arrays: unsafe extern "C" fn(u32, i32, i32),
    get_string: unsafe extern "C" fn(u32) -> *const c_char,
}

/// EGL function pointers loaded at runtime.
struct EglFns {
    get_display: unsafe extern "C" fn(EGLNativeDisplayType) -> EGLDisplay,
    initialize: unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean,
    choose_config: unsafe extern "C" fn(
        EGLDisplay,
        *const EGLint,
        *mut EGLConfig,
        EGLint,
        *mut EGLint,
    ) -> EGLBoolean,
    create_window_surface: unsafe extern "C" fn(
        EGLDisplay,
        EGLConfig,
        EGLNativeWindowType,
        *const EGLint,
    ) -> EGLSurface,
    create_context: unsafe extern "C" fn(
        EGLDisplay,
        EGLConfig,
        EGLContext,
        *const EGLint,
    ) -> EGLContext,
    make_current:
        unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean,
    swap_buffers: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
    get_proc_address: unsafe extern "C" fn(*const c_char) -> *const c_void,
    destroy_surface: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
    destroy_context: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
    terminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
}

/// EGL + GLES2 renderer with a single texture slot.
pub struct Renderer {
    egl: EglFns,
    gl: GlFns,
    egl_display: EGLDisplay,
    egl_surface: EGLSurface,
    #[allow(dead_code)]
    egl_context: EGLContext,
    pub width: u32,
    pub height: u32,
    program: u32,
    a_pos: u32,
    a_texcoord: u32,
    texture: u32,
    quad_vbo: u32,
    tex_width: u32,
    tex_height: u32,
    _lib_egl: libloading::Library,
    _lib_gles: libloading::Library,
}

impl Renderer {
    pub fn new(hwc: &HwcBackend) -> Result<Self> {
        // Load libEGL
        let lib_egl = unsafe { libloading::Library::new("libEGL.so") }
            .or_else(|_| unsafe { libloading::Library::new("libEGL.so.1") })
            .context("Failed to load libEGL")?;

        let egl = unsafe {
            macro_rules! egl_fn {
                ($name:literal) => {{
                    let sym: libloading::Symbol<_> = lib_egl
                        .get($name.as_bytes())
                        .with_context(|| format!("EGL symbol {} not found", $name))?;
                    *sym
                }};
            }
            EglFns {
                get_display: egl_fn!("eglGetDisplay"),
                initialize: egl_fn!("eglInitialize"),
                choose_config: egl_fn!("eglChooseConfig"),
                create_window_surface: egl_fn!("eglCreateWindowSurface"),
                create_context: egl_fn!("eglCreateContext"),
                make_current: egl_fn!("eglMakeCurrent"),
                swap_buffers: egl_fn!("eglSwapBuffers"),
                get_proc_address: egl_fn!("eglGetProcAddress"),
                destroy_surface: egl_fn!("eglDestroySurface"),
                destroy_context: egl_fn!("eglDestroyContext"),
                terminate: egl_fn!("eglTerminate"),
            }
        };

        // Initialize EGL
        let egl_display = unsafe { (egl.get_display)(EGL_DEFAULT_DISPLAY) };
        if egl_display.is_null() {
            anyhow::bail!("eglGetDisplay failed");
        }

        let mut major: EGLint = 0;
        let mut minor: EGLint = 0;
        if unsafe { (egl.initialize)(egl_display, &mut major, &mut minor) } != EGL_TRUE {
            anyhow::bail!("eglInitialize failed");
        }
        info!(major, minor, "EGL initialized");

        // Choose config
        let config_attribs: [EGLint; 13] = [
            EGL_RED_SIZE, 8,
            EGL_GREEN_SIZE, 8,
            EGL_BLUE_SIZE, 8,
            EGL_ALPHA_SIZE, 8,
            EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
            EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
            EGL_NONE,
        ];
        let mut config: EGLConfig = std::ptr::null_mut();
        let mut num_configs: EGLint = 0;
        if unsafe {
            (egl.choose_config)(
                egl_display,
                config_attribs.as_ptr(),
                &mut config,
                1,
                &mut num_configs,
            )
        } != EGL_TRUE
            || num_configs == 0
        {
            anyhow::bail!("eglChooseConfig failed");
        }

        // Create window surface
        let native_window = hwc.native_window_handle();
        let egl_surface = unsafe {
            (egl.create_window_surface)(
                egl_display,
                config,
                native_window,
                std::ptr::null(),
            )
        };
        if egl_surface.is_null() {
            anyhow::bail!("eglCreateWindowSurface failed");
        }

        // Create GLES2 context
        let context_attribs: [EGLint; 3] = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        let egl_context = unsafe {
            (egl.create_context)(
                egl_display,
                config,
                EGL_NO_CONTEXT,
                context_attribs.as_ptr(),
            )
        };
        if egl_context.is_null() {
            anyhow::bail!("eglCreateContext failed");
        }

        // Make current
        if unsafe {
            (egl.make_current)(egl_display, egl_surface, egl_surface, egl_context)
        } != EGL_TRUE
        {
            anyhow::bail!("eglMakeCurrent failed");
        }
        info!("GLES2 context created and made current");

        // Load GLES2 functions
        let lib_gles = unsafe { libloading::Library::new("libGLESv2.so") }
            .or_else(|_| unsafe { libloading::Library::new("libGLESv2.so.2") })
            .context("Failed to load libGLESv2")?;

        // Helper: try lib first, fall back to eglGetProcAddress
        let resolve_gl = |name: &str| -> Result<*const c_void> {
            let name_cstr = std::ffi::CString::new(name).unwrap();
            // Try direct symbol first
            let ptr: *const c_void = unsafe {
                lib_gles
                    .get::<*const c_void>(name.as_bytes())
                    .map(|s| *s)
                    .unwrap_or_else(|_| {
                        (egl.get_proc_address)(name_cstr.as_ptr())
                    })
            };
            if ptr.is_null() {
                anyhow::bail!("GL symbol {} not found", name);
            }
            Ok(ptr)
        };

        macro_rules! gl_fn {
            ($name:literal) => {
                std::mem::transmute(resolve_gl($name)?)
            };
        }

        let gl = unsafe {
            GlFns {
                viewport: gl_fn!("glViewport"),
                clear_color: gl_fn!("glClearColor"),
                clear: gl_fn!("glClear"),
                gen_textures: gl_fn!("glGenTextures"),
                bind_texture: gl_fn!("glBindTexture"),
                tex_parameteri: gl_fn!("glTexParameteri"),
                tex_image_2d: gl_fn!("glTexImage2D"),
                tex_sub_image_2d: gl_fn!("glTexSubImage2D"),
                delete_textures: gl_fn!("glDeleteTextures"),
                active_texture: gl_fn!("glActiveTexture"),
                create_shader: gl_fn!("glCreateShader"),
                shader_source: gl_fn!("glShaderSource"),
                compile_shader: gl_fn!("glCompileShader"),
                get_shaderiv: gl_fn!("glGetShaderiv"),
                get_shader_info_log: gl_fn!("glGetShaderInfoLog"),
                delete_shader: gl_fn!("glDeleteShader"),
                create_program: gl_fn!("glCreateProgram"),
                attach_shader: gl_fn!("glAttachShader"),
                link_program: gl_fn!("glLinkProgram"),
                get_programiv: gl_fn!("glGetProgramiv"),
                get_program_info_log: gl_fn!("glGetProgramInfoLog"),
                delete_program: gl_fn!("glDeleteProgram"),
                use_program: gl_fn!("glUseProgram"),
                get_attrib_location: gl_fn!("glGetAttribLocation"),
                enable_vertex_attrib_array: gl_fn!("glEnableVertexAttribArray"),
                disable_vertex_attrib_array: gl_fn!("glDisableVertexAttribArray"),
                vertex_attrib_pointer: gl_fn!("glVertexAttribPointer"),
                gen_buffers: gl_fn!("glGenBuffers"),
                bind_buffer: gl_fn!("glBindBuffer"),
                buffer_data: gl_fn!("glBufferData"),
                delete_buffers: gl_fn!("glDeleteBuffers"),
                draw_arrays: gl_fn!("glDrawArrays"),
                get_string: gl_fn!("glGetString"),
            }
        };

        // Log GL info
        unsafe {
            let vendor = std::ffi::CStr::from_ptr((gl.get_string)(GL_VENDOR))
                .to_string_lossy();
            let renderer = std::ffi::CStr::from_ptr((gl.get_string)(GL_RENDERER))
                .to_string_lossy();
            let version = std::ffi::CStr::from_ptr((gl.get_string)(GL_VERSION))
                .to_string_lossy();
            info!(%vendor, %renderer, %version, "GLES2 info");
        }

        // Compile shader program
        let program = unsafe { Self::compile_program(&gl)? };

        let a_pos_name = std::ffi::CString::new("a_pos").unwrap();
        let a_texcoord_name = std::ffi::CString::new("a_texcoord").unwrap();
        let a_pos =
            unsafe { (gl.get_attrib_location)(program, a_pos_name.as_ptr()) };
        let a_texcoord =
            unsafe { (gl.get_attrib_location)(program, a_texcoord_name.as_ptr()) };
        if a_pos < 0 || a_texcoord < 0 {
            anyhow::bail!("Shader attribute not found (a_pos={}, a_texcoord={})", a_pos, a_texcoord);
        }
        let a_pos = a_pos as u32;
        let a_texcoord = a_texcoord as u32;

        info!(a_pos, a_texcoord, "Shader program compiled");

        // Create the single texture
        let mut texture: u32 = 0;
        unsafe {
            (gl.gen_textures)(1, &mut texture);
            (gl.bind_texture)(GL_TEXTURE_2D, texture);
            (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (gl.tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        }

        // Fullscreen quad in NDC: [-1, -1] to [1, 1]
        // Texture coords: [0, 1] at bottom-left to [1, 0] at top-right
        #[rustfmt::skip]
        let vertices: [f32; 16] = [
            -1.0, -1.0,   0.0, 1.0,
             1.0, -1.0,   1.0, 1.0,
            -1.0,  1.0,   0.0, 0.0,
             1.0,  1.0,   1.0, 0.0,
        ];

        let mut quad_vbo = 0;
        unsafe {
            (gl.gen_buffers)(1, &mut quad_vbo);
            (gl.bind_buffer)(GL_ARRAY_BUFFER, quad_vbo);
            (gl.buffer_data)(
                GL_ARRAY_BUFFER,
                std::mem::size_of_val(&vertices) as isize,
                vertices.as_ptr().cast(),
                GL_STATIC_DRAW,
            );
            (gl.bind_buffer)(GL_ARRAY_BUFFER, 0);
        }

        Ok(Self {
            egl,
            gl,
            egl_display,
            egl_surface,
            egl_context,
            width: hwc.info.width,
            height: hwc.info.height,
            program,
            a_pos,
            a_texcoord,
            texture,
            quad_vbo,
            tex_width: 0,
            tex_height: 0,
            _lib_egl: lib_egl,
            _lib_gles: lib_gles,
        })
    }

    unsafe fn compile_program(gl: &GlFns) -> Result<u32> {
        unsafe {
            let vs = (gl.create_shader)(GL_VERTEX_SHADER);
            let vs_src = std::ffi::CString::new(VERTEX_SHADER).unwrap();
            let vs_ptr = vs_src.as_ptr();
            let vs_len = VERTEX_SHADER.len() as i32;
            (gl.shader_source)(vs, 1, &vs_ptr as *const *const c_char, &vs_len);
            (gl.compile_shader)(vs);
            let mut status: i32 = 0;
            (gl.get_shaderiv)(vs, GL_COMPILE_STATUS, &mut status);
            if status != GL_TRUE {
                let mut buf = [0 as c_char; 512];
                let mut len: i32 = 0;
                (gl.get_shader_info_log)(vs, 512, &mut len, buf.as_mut_ptr());
                let log = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
                (gl.delete_shader)(vs);
                anyhow::bail!("Vertex shader compile failed: {}", log);
            }

            let fs = (gl.create_shader)(GL_FRAGMENT_SHADER);
            let fs_src = std::ffi::CString::new(FRAGMENT_SHADER).unwrap();
            let fs_ptr = fs_src.as_ptr();
            let fs_len = FRAGMENT_SHADER.len() as i32;
            (gl.shader_source)(fs, 1, &fs_ptr as *const *const c_char, &fs_len);
            (gl.compile_shader)(fs);
            (gl.get_shaderiv)(fs, GL_COMPILE_STATUS, &mut status);
            if status != GL_TRUE {
                let mut buf = [0 as c_char; 512];
                let mut len: i32 = 0;
                (gl.get_shader_info_log)(fs, 512, &mut len, buf.as_mut_ptr());
                let log = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
                (gl.delete_shader)(vs);
                (gl.delete_shader)(fs);
                anyhow::bail!("Fragment shader compile failed: {}", log);
            }

            let program = (gl.create_program)();
            (gl.attach_shader)(program, vs);
            (gl.attach_shader)(program, fs);
            (gl.link_program)(program);
            (gl.get_programiv)(program, GL_LINK_STATUS, &mut status);
            if status != GL_TRUE {
                let mut buf = [0 as c_char; 512];
                let mut len: i32 = 0;
                (gl.get_program_info_log)(program, 512, &mut len, buf.as_mut_ptr());
                let log = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
                (gl.delete_program)(program);
                (gl.delete_shader)(vs);
                (gl.delete_shader)(fs);
                anyhow::bail!("Shader link failed: {}", log);
            }

            (gl.delete_shader)(vs);
            (gl.delete_shader)(fs);

            Ok(program)
        }
    }

    /// Clear the screen with a solid color.
    pub fn clear(&self, r: f32, g: f32, b: f32, a: f32) {
        unsafe {
            (self.gl.viewport)(0, 0, self.width as i32, self.height as i32);
            (self.gl.clear_color)(r, g, b, a);
            (self.gl.clear)(GL_COLOR_BUFFER_BIT);
        }
    }

    /// Upload a complete RGBA frame, validating it before changing GL state.
    pub fn upload(&mut self, width: u32, height: u32, stride: u32, data: &[u8]) -> Result<()> {
        let row_bytes = validate_upload(width, height, stride, data.len())?;
        unsafe {
            (self.gl.bind_texture)(GL_TEXTURE_2D, self.texture);

            if stride as usize == row_bytes {
                // Tight packing — single upload
                if self.tex_width == width && self.tex_height == height {
                    // Same dimensions — use sub image (faster, no realloc)
                    (self.gl.tex_sub_image_2d)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        data.as_ptr() as *const c_void,
                    );
                } else {
                    (self.gl.tex_image_2d)(
                        GL_TEXTURE_2D,
                        0,
                        GL_RGBA as i32,
                        width as i32,
                        height as i32,
                        0,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        data.as_ptr() as *const c_void,
                    );
                    self.tex_width = width;
                    self.tex_height = height;
                }
            } else {
                // Non-tight packing — allocate then upload row by row
                if self.tex_width != width || self.tex_height != height {
                    (self.gl.tex_image_2d)(
                        GL_TEXTURE_2D,
                        0,
                        GL_RGBA as i32,
                        width as i32,
                        height as i32,
                        0,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        std::ptr::null(),
                    );
                    self.tex_width = width;
                    self.tex_height = height;
                }
                for row in 0..height {
                    let offset = row as usize * stride as usize;
                    (self.gl.tex_sub_image_2d)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        row as i32,
                        width as i32,
                        1,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        data[offset..].as_ptr() as *const c_void,
                    );
                }
            }
        }
        Ok(())
    }

    /// Draw the single texture as a fullscreen quad.
    pub fn draw(&self) {
        if self.tex_width == 0 || self.tex_height == 0 {
            return;
        }

        unsafe {
            (self.gl.use_program)(self.program);

            (self.gl.active_texture)(GL_TEXTURE0);
            (self.gl.bind_texture)(GL_TEXTURE_2D, self.texture);

            (self.gl.enable_vertex_attrib_array)(self.a_pos);
            (self.gl.enable_vertex_attrib_array)(self.a_texcoord);

            (self.gl.bind_buffer)(GL_ARRAY_BUFFER, self.quad_vbo);

            (self.gl.vertex_attrib_pointer)(self.a_pos, 2, GL_FLOAT, 0, 16, std::ptr::null());
            (self.gl.vertex_attrib_pointer)(
                self.a_texcoord,
                2,
                GL_FLOAT,
                0,
                16,
                8usize as *const c_void,
            );

            (self.gl.draw_arrays)(GL_TRIANGLE_STRIP, 0, 4);

            (self.gl.disable_vertex_attrib_array)(self.a_pos);
            (self.gl.disable_vertex_attrib_array)(self.a_texcoord);
            (self.gl.bind_buffer)(GL_ARRAY_BUFFER, 0);
            (self.gl.use_program)(0);
        }
    }

    /// Swap EGL buffers. Triggers present_callback synchronously.
    pub fn swap_buffers(&self) -> Result<()> {
        if unsafe { (self.egl.swap_buffers)(self.egl_display, self.egl_surface) } != EGL_TRUE {
            anyhow::bail!("eglSwapBuffers failed");
        }
        Ok(())
    }
}

fn validate_upload(width: u32, height: u32, stride: u32, length: usize) -> Result<usize> {
    anyhow::ensure!(width > 0 && height > 0 && width <= i32::MAX as u32
                    && height <= i32::MAX as u32, "invalid texture dimensions");
    let row_bytes = (width as usize).checked_mul(4).context("texture row overflow")?;
    anyhow::ensure!(stride as usize >= row_bytes, "texture stride shorter than row");
    let required = (height as usize - 1).checked_mul(stride as usize)
        .and_then(|offset| offset.checked_add(row_bytes)).context("texture size overflow")?;
    anyhow::ensure!(length >= required, "incomplete texture buffer");
    Ok(row_bytes)
}

#[cfg(test)]
mod upload_tests {
    use super::validate_upload;

    #[test]
    fn complete_rows_allow_padding_without_requiring_final_padding() {
        assert_eq!(validate_upload(2, 2, 8, 16).unwrap(), 8);
        assert_eq!(validate_upload(2, 2, 12, 20).unwrap(), 8);
        assert!(validate_upload(2, 2, 12, 19).is_err());
        assert!(validate_upload(2, 2, 8, 15).is_err());
    }

    #[test]
    fn invalid_dimensions_and_short_stride_are_rejected() {
        for (width, height, stride) in [(0, 1, 4), (1, 0, 4),
                (2, 2, 7), (u32::MAX, 1, 4), (1, u32::MAX, 4)] {
            assert!(validate_upload(width, height, stride, 32).is_err());
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            (self.gl.delete_buffers)(1, &self.quad_vbo);
            (self.gl.delete_textures)(1, &self.texture);
            (self.gl.delete_program)(self.program);
            (self.egl.make_current)(
                self.egl_display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                EGL_NO_CONTEXT,
            );
            (self.egl.destroy_surface)(self.egl_display, self.egl_surface);
            (self.egl.destroy_context)(self.egl_display, self.egl_context);
            (self.egl.terminate)(self.egl_display);
        }
    }
}
