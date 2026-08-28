use std::sync::Arc;

use glow::HasContext;

use crate::display_list::GpuQuad;

pub struct GpuRenderer {
    width: u32,
    height: u32,
    device_pixel_ratio: f32,
    egl: Option<Arc<crate::egl_context::EglHeadlessContext>>,
    gl: Option<glow::Context>,
    shader_program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    vbo: Option<glow::Buffer>,
    fbo: Option<glow::Framebuffer>,
    color_tex: Option<glow::Texture>,
    depth_rbo: Option<glow::Renderbuffer>,
    quads: Vec<GpuQuad>,
}

unsafe impl Send for GpuRenderer {}
unsafe impl Sync for GpuRenderer {}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("GPU initialization failed: {0}")]
    InitError(String),
    #[error("Display list build failed: {0}")]
    BuildError(String),
    #[error("Render failed: {0}")]
    RenderError(String),
    #[error("Surface lost: {0}")]
    SurfaceLost(String),
}

const VERTEX_SHADER: &str = r#"#version 330 core
layout(location = 0) in vec2 aPos;
layout(location = 1) in vec4 aColor;

uniform mat4 uProjection;

out vec4 vColor;

void main() {
    gl_Position = uProjection * vec4(aPos, 0.0, 1.0);
    vColor = aColor;
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 330 core
in vec4 vColor;
out vec4 FragColor;

void main() {
    FragColor = vColor;
}
"#;

impl GpuRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            device_pixel_ratio: 1.0,
            egl: None,
            gl: None,
            shader_program: None,
            vao: None,
            vbo: None,
            fbo: None,
            color_tex: None,
            depth_rbo: None,
            quads: Vec::new(),
        }
    }

    pub async fn initialize(&mut self) -> Result<(), RenderError> {
        let ctx = crate::egl_context::EglHeadlessContext::new(self.width, self.height)
            .map_err(|e| RenderError::InitError(e))?;

        let gl = ctx.load_gl();

        let shader_program = unsafe {
            Self::compile_shaders(&gl).map_err(|e| RenderError::InitError(e))?
        };

        let (vao, vbo) = unsafe {
            Self::create_buffers(&gl).map_err(|e| RenderError::InitError(e))?
        };

        let (fbo, color_tex, depth_rbo) = unsafe {
            self.create_framebuffer(&gl)
                .map_err(|e| RenderError::InitError(e))?
        };

        self.egl = Some(Arc::new(ctx));
        self.gl = Some(gl);
        self.shader_program = Some(shader_program);
        self.vao = Some(vao);
        self.vbo = Some(vbo);
        self.fbo = Some(fbo);
        self.color_tex = Some(color_tex);
        self.depth_rbo = Some(depth_rbo);

        Ok(())
    }

    unsafe fn compile_shaders(gl: &glow::Context) -> Result<glow::Program, String> {
        let program = gl.create_program().map_err(|e| e.to_string())?;

        let vert = gl.create_shader(glow::VERTEX_SHADER).map_err(|e| e.to_string())?;
        gl.shader_source(vert, VERTEX_SHADER);
        gl.compile_shader(vert);
        if !gl.get_shader_compile_status(vert) {
            let log = gl.get_shader_info_log(vert);
            gl.delete_shader(vert);
            return Err(format!("Vertex shader compilation failed: {log}"));
        }

        let frag = gl.create_shader(glow::FRAGMENT_SHADER).map_err(|e| e.to_string())?;
        gl.shader_source(frag, FRAGMENT_SHADER);
        gl.compile_shader(frag);
        if !gl.get_shader_compile_status(frag) {
            let log = gl.get_shader_info_log(frag);
            gl.delete_shader(vert);
            gl.delete_shader(frag);
            return Err(format!("Fragment shader compilation failed: {log}"));
        }

        gl.attach_shader(program, vert);
        gl.attach_shader(program, frag);
        gl.link_program(program);

        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            gl.delete_shader(vert);
            gl.delete_shader(frag);
            return Err(format!("Shader program linking failed: {log}"));
        }

        gl.detach_shader(program, vert);
        gl.detach_shader(program, frag);
        gl.delete_shader(vert);
        gl.delete_shader(frag);

        Ok(program)
    }

    unsafe fn create_buffers(gl: &glow::Context) -> Result<(glow::VertexArray, glow::Buffer), String> {
        let vao = gl.create_vertex_array().map_err(|e| e.to_string())?;
        let vbo = gl.create_buffer().map_err(|e| e.to_string())?;

        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

        let stride = std::mem::size_of::<f32>() * 6; // 2 pos + 4 color
        // Position attribute: 2 floats
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride as i32, 0);
        gl.enable_vertex_attrib_array(0);
        // Color attribute: 4 floats
        gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, stride as i32, (2 * std::mem::size_of::<f32>()) as i32);
        gl.enable_vertex_attrib_array(1);

        gl.bind_vertex_array(None);

        Ok((vao, vbo))
    }

    unsafe fn create_framebuffer(
        &self,
        gl: &glow::Context,
    ) -> Result<(glow::Framebuffer, glow::Texture, glow::Renderbuffer), String> {
        let fbo = gl.create_framebuffer().map_err(|e| e.to_string())?;
        let tex = gl.create_texture().map_err(|e| e.to_string())?;
        let rbo = gl.create_renderbuffer().map_err(|e| e.to_string())?;

        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));

        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            self.width as i32,
            self.height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            None,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );

        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(rbo));
        gl.renderbuffer_storage(
            glow::RENDERBUFFER,
            glow::DEPTH24_STENCIL8,
            self.width as i32,
            self.height as i32,
        );
        gl.framebuffer_renderbuffer(
            glow::FRAMEBUFFER,
            glow::DEPTH_STENCIL_ATTACHMENT,
            glow::RENDERBUFFER,
            Some(rbo),
        );

        if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            return Err("Framebuffer is not complete".to_string());
        }

        gl.bind_framebuffer(glow::FRAMEBUFFER, None);

        Ok((fbo, tex, rbo))
    }

    pub async fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if width == self.width && height == self.height {
            return Ok(());
        }

        self.destroy_gl_resources();
        self.width = width;
        self.height = height;

        if self.egl.is_some() {
            self.initialize().await?;
        }

        Ok(())
    }

    pub async fn set_device_pixel_ratio(&mut self, ratio: f32) {
        self.device_pixel_ratio = ratio;
    }

    pub async fn set_display_list(&mut self, quads: Vec<GpuQuad>) {
        self.quads = quads;
    }

    pub async fn render(&self) -> Result<Vec<u8>, RenderError> {
        let gl = self
            .gl
            .as_ref()
            .ok_or_else(|| RenderError::RenderError("GL context not initialized".into()))?;
        let shader = self
            .shader_program
            .ok_or_else(|| RenderError::RenderError("Shader program not initialized".into()))?;
        let vao = self
            .vao
            .ok_or_else(|| RenderError::RenderError("VAO not initialized".into()))?;
        let vbo = self
            .vbo
            .ok_or_else(|| RenderError::RenderError("VBO not initialized".into()))?;
        let fbo = self
            .fbo
            .ok_or_else(|| RenderError::RenderError("FBO not initialized".into()))?;

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, self.width as i32, self.height as i32);
            gl.clear_color(1.0, 1.0, 1.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);

            if !self.quads.is_empty() {
                let vertices = Self::quads_to_vertices(&self.quads, self.width, self.height);
                let vertex_count = vertices.len() / 6;

                gl.use_program(Some(shader));

                let proj = Self::orthographic_projection(self.width as f32, self.height as f32);
                let loc = gl.get_uniform_location(shader, "uProjection");
                if let Some(loc) = loc {
                    gl.uniform_matrix_4_f32_slice(Some(&loc), false, &proj);
                }

                gl.bind_vertex_array(Some(vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                let byte_len = (vertices.len() * std::mem::size_of::<f32>()) as isize;
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    std::slice::from_raw_parts(vertices.as_ptr() as *const u8, byte_len as usize),
                    glow::DYNAMIC_DRAW,
                );

                gl.draw_arrays(glow::TRIANGLES, 0, vertex_count as i32);

                gl.bind_vertex_array(None);
                gl.use_program(None);
            }

            let mut pixels = vec![0u8; (self.width * self.height * 4) as usize];
            gl.read_pixels(
                0,
                0,
                self.width as i32,
                self.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut pixels),
            );

            gl.bind_framebuffer(glow::FRAMEBUFFER, None);

            Ok(pixels)
        }
    }

    fn quads_to_vertices(quads: &[GpuQuad], width: u32, height: u32) -> Vec<f32> {
        let mut verts = Vec::with_capacity(quads.len() * 24); // 6 verts * 6 floats

        for quad in quads {
            let x1 = quad.x;
            let y1 = quad.y;
            let x2 = quad.x + quad.width;
            let y2 = quad.y + quad.height;

            let (r, g, b, a) = (
                quad.color[0] as f32 / 255.0,
                quad.color[1] as f32 / 255.0,
                quad.color[2] as f32 / 255.0,
                quad.color[3] as f32 / 255.0,
            );

            // Two triangles per quad: (x1,y1), (x2,y1), (x1,y2), (x1,y2), (x2,y1), (x2,y2)
            verts.extend_from_slice(&[
                x1, y1, r, g, b, a,
                x2, y1, r, g, b, a,
                x1, y2, r, g, b, a,
                x1, y2, r, g, b, a,
                x2, y1, r, g, b, a,
                x2, y2, r, g, b, a,
            ]);
        }

        verts
    }

    fn orthographic_projection(width: f32, height: f32) -> [f32; 16] {
        // Maps (0,0)-(width,height) to clip space (-1,-1)-(1,1)
        let sx = 2.0 / width;
        let sy = -2.0 / height;
        let tx = -1.0;
        let ty = 1.0;

        [
            sx,  0.0, 0.0, 0.0,
            0.0, sy,  0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            tx,  ty,  0.0, 1.0,
        ]
    }

    pub async fn get_state(&self) -> RendererState {
        if self.gl.is_some() {
            RendererState::Ready
        } else {
            RendererState::Uninitialized
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn destroy_gl_resources(&mut self) {
        if let Some(gl) = self.gl.as_ref() {
            unsafe {
                if let Some(t) = self.color_tex.take() {
                    gl.delete_texture(t);
                }
                if let Some(r) = self.depth_rbo.take() {
                    gl.delete_renderbuffer(r);
                }
                if let Some(f) = self.fbo.take() {
                    gl.delete_framebuffer(f);
                }
                if let Some(v) = self.vbo.take() {
                    gl.delete_buffer(v);
                }
                if let Some(v) = self.vao.take() {
                    gl.delete_vertex_array(v);
                }
                if let Some(p) = self.shader_program.take() {
                    gl.delete_program(p);
                }
            }
        }
        self.gl = None;
        self.egl = None;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RendererState {
    Uninitialized,
    Ready,
    Error(String),
}

impl Drop for GpuRenderer {
    fn drop(&mut self) {
        self.destroy_gl_resources();
    }
}
