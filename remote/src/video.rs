//! Draws the decoded picture: its YUV planes as textures, turned into RGB by a shader, fitted to the window
//! with black bars where the shapes differ.
use crate::session::Frame;
use egui_glow::glow::{self, HasContext};

const VERTEX: &str = r#"#version 330 core
uniform vec4 u_rect;
out vec2 v_uv;
void main() {
    vec2 p = vec2(gl_VertexID & 1, (gl_VertexID >> 1) & 1);
    v_uv = vec2(p.x, 1.0 - p.y);
    gl_Position = vec4(mix(u_rect.xy, u_rect.zw, p), 0.0, 1.0);
}"#;

const FRAGMENT: &str = r#"#version 330 core
uniform sampler2D u_y, u_u, u_v;
uniform int u_planes;
uniform mat3 u_matrix;
uniform vec3 u_offset;
in vec2 v_uv;
out vec4 color;
void main() {
    float y = texture(u_y, v_uv).r;
    vec2 uv = u_planes == 2 ? texture(u_u, v_uv).rg : vec2(texture(u_u, v_uv).r, texture(u_v, v_uv).r);
    color = vec4(u_matrix * (vec3(y, uv) - u_offset), 1.0);
}"#;

pub struct Video {
    program: glow::Program,
    vao: glow::VertexArray,
    tex: [glow::Texture; 3],
    /// What the textures were last sized for: width, height, planes.
    shape: (i32, i32, i32),
    pub serial: u64,
    pub width: i32,
    pub height: i32,
    bt601: bool,
    full_range: bool,
}

impl Video {
    pub fn new(gl: &glow::Context) -> Video {
        unsafe {
            let program = gl.create_program().expect("GL program");
            for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
                let shader = gl.create_shader(kind).expect("GL shader");
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                assert!(gl.get_shader_compile_status(shader), "video shader: {}", gl.get_shader_info_log(shader));
                gl.attach_shader(program, shader);
            }
            gl.link_program(program);
            assert!(gl.get_program_link_status(program), "video shader: {}", gl.get_program_info_log(program));
            let tex = [0; 3].map(|_| {
                let t = gl.create_texture().expect("GL texture");
                gl.bind_texture(glow::TEXTURE_2D, Some(t));
                for (name, value) in [
                    (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                    (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                    (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                    (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                ] {
                    gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
                }
                t
            });
            Video { program, vao: gl.create_vertex_array().expect("GL vertex array"), tex, shape: (0, 0, 0), serial: 0, width: 0, height: 0, bt601: false, full_range: false }
        }
    }

    /// Takes the picture in if it is newer than the one held. Returns whether it was.
    pub fn upload(&mut self, gl: &glow::Context, f: &Frame) -> bool {
        if f.serial == self.serial || f.nplanes == 0 {
            return false;
        }
        let fresh = self.shape != (f.width, f.height, f.nplanes);
        let (cw, ch) = ((f.width + 1) / 2, (f.height + 1) / 2);
        unsafe {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for i in 0..f.nplanes as usize {
                // Plane 0 is brightness at full size; the rest are colour at half: one two-channel plane (NV12) or two.
                let (w, h, format, internal, texel) = match (i, f.nplanes) {
                    (0, _) => (f.width, f.height, glow::RED, glow::R8, 1),
                    (_, 2) => (cw, ch, glow::RG, glow::RG8, 2),
                    _ => (cw, ch, glow::RED, glow::R8, 1),
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(self.tex[i]));
                gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, f.stride[i] / texel);
                let pixels = glow::PixelUnpackData::Slice(Some(&f.planes[i]));
                if fresh {
                    gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, w, h, 0, format, glow::UNSIGNED_BYTE, pixels);
                } else {
                    gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, w, h, format, glow::UNSIGNED_BYTE, pixels);
                }
            }
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
        }
        self.shape = (f.width, f.height, f.nplanes);
        (self.width, self.height, self.bt601, self.full_range, self.serial) = (f.width, f.height, f.bt601, f.full_range, f.serial);
        true
    }

    /// Where the picture goes in a window of that many pixels: [x, y, width, height], from the top left.
    pub fn fit(&self, win: (i32, i32)) -> [f32; 4] {
        if self.width == 0 || win.0 == 0 || win.1 == 0 {
            return [0.0; 4];
        }
        let k = (win.0 as f32 / self.width as f32).min(win.1 as f32 / self.height as f32);
        let (w, h) = (self.width as f32 * k, self.height as f32 * k);
        [(win.0 as f32 - w) / 2.0, (win.1 as f32 - h) / 2.0, w, h]
    }

    pub fn draw(&self, gl: &glow::Context, win: (i32, i32)) {
        if self.width == 0 {
            return;
        }
        let [x, y, w, h] = self.fit(win);
        let (sx, sy) = (2.0 / win.0 as f32, 2.0 / win.1 as f32);
        // Rows are Y, U, V contributions; GL wants the matrix by columns, which is how this is written.
        let (ky, matrix): (f32, [f32; 9]) = match (self.bt601, self.full_range) {
            (false, false) => (1.164, [1.164, 1.164, 1.164, 0.0, -0.213, 2.112, 1.793, -0.533, 0.0]),
            (false, true) => (1.0, [1.0, 1.0, 1.0, 0.0, -0.1873, 1.8556, 1.5748, -0.4681, 0.0]),
            (true, false) => (1.164, [1.164, 1.164, 1.164, 0.0, -0.392, 2.017, 1.596, -0.813, 0.0]),
            (true, true) => (1.0, [1.0, 1.0, 1.0, 0.0, -0.344, 1.772, 1.402, -0.714, 0.0]),
        };
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            for (i, name) in ["u_y", "u_u", "u_v"].into_iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.tex[i]));
                gl.uniform_1_i32(gl.get_uniform_location(self.program, name).as_ref(), i as i32);
            }
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "u_planes").as_ref(), self.shape.2);
            gl.uniform_matrix_3_f32_slice(gl.get_uniform_location(self.program, "u_matrix").as_ref(), false, &matrix);
            gl.uniform_3_f32(gl.get_uniform_location(self.program, "u_offset").as_ref(), if ky > 1.0 { 16.0 / 255.0 } else { 0.0 }, 0.5, 0.5);
            gl.uniform_4_f32(gl.get_uniform_location(self.program, "u_rect").as_ref(), x * sx - 1.0, 1.0 - (y + h) * sy, (x + w) * sx - 1.0, 1.0 - y * sy);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.active_texture(glow::TEXTURE0);
        }
    }

    /// Forget the picture (a session ended): the next one starts from black.
    pub fn clear(&mut self) {
        (self.width, self.height) = (0, 0);
    }
}
