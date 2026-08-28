/// A flattened GPU-ready quad for rendering.
#[derive(Debug, Clone)]
pub struct GpuQuad {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: [u8; 4],
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        font_size: f32,
        color: [u8; 4],
    },
    Image {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        data: Vec<u8>,
    },
    Clip {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        items: Vec<DisplayItem>,
    },
    Transform {
        matrix: [f32; 6],
        items: Vec<DisplayItem>,
    },
    Opacity {
        opacity: f32,
        items: Vec<DisplayItem>,
    },
}

pub struct DisplayListBuilder {
    items: Vec<DisplayItem>,
}

impl DisplayListBuilder {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4]) {
        self.items.push(DisplayItem::Rect {
            x,
            y,
            width: w,
            height: h,
            color,
        });
    }

    pub fn push_text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.items.push(DisplayItem::Text {
            x,
            y,
            text: text.to_string(),
            font_size: size,
            color,
        });
    }

    pub fn push_image(&mut self, x: f32, y: f32, w: f32, h: f32, data: Vec<u8>) {
        self.items.push(DisplayItem::Image {
            x,
            y,
            width: w,
            height: h,
            data,
        });
    }

    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32, items: Vec<DisplayItem>) {
        self.items.push(DisplayItem::Clip {
            x,
            y,
            width: w,
            height: h,
            items,
        });
    }

    pub fn push_transform(&mut self, matrix: [f32; 6], items: Vec<DisplayItem>) {
        self.items.push(DisplayItem::Transform { matrix, items });
    }

    pub fn push_opacity(&mut self, opacity: f32, items: Vec<DisplayItem>) {
        self.items.push(DisplayItem::Opacity { opacity, items });
    }

    pub fn build(&self) -> Vec<DisplayItem> {
        self.items.clone()
    }

    /// Flatten display items into GPU-ready quads.
    /// Handles opacity, clipping, and transforms (basic affine).
    pub fn build_gpu_quads(&self) -> Vec<GpuQuad> {
        let mut quads = Vec::new();
        for item in &self.items {
            Self::flatten_item(item, 1.0, &mut quads);
        }
        quads
    }

    fn flatten_item(item: &DisplayItem, parent_opacity: f32, quads: &mut Vec<GpuQuad>) {
        match item {
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => {
                let a = (color[3] as f32 / 255.0 * parent_opacity * 255.0) as u8;
                quads.push(GpuQuad {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    color: [color[0], color[1], color[2], a],
                });
            }
            DisplayItem::Text {
                x,
                y,
                text,
                font_size,
                color,
            } => {
                // Render each character as a solid-colored quad placeholder.
                // A real implementation would use glyph textures here.
                let char_w = font_size * 0.6;
                let char_h = font_size;
                let a = (color[3] as f32 / 255.0 * parent_opacity * 255.0) as u8;
                let mut cx = *x;
                for _ in text.chars() {
                    quads.push(GpuQuad {
                        x: cx,
                        y: *y,
                        width: char_w,
                        height: *char_h,
                        color: [color[0], color[1], color[2], a],
                    });
                    cx += char_w;
                }
            }
            DisplayItem::Image {
                x,
                y,
                width,
                height,
                data,
            } => {
                // Images are flattened as solid-color placeholder quads.
                // A real implementation would upload `data` as a GL texture.
                let _ = data;
                quads.push(GpuQuad {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    color: [255, 255, 255, (255.0 * parent_opacity) as u8],
                });
            }
            DisplayItem::Clip {
                x,
                y,
                width,
                height,
                items,
            } => {
                let clip_x = *x;
                let clip_y = *y;
                let clip_w = *width;
                let clip_h = *height;
                for child in items {
                    Self::flatten_item_clipped(child, parent_opacity, clip_x, clip_y, clip_w, clip_h, quads);
                }
            }
            DisplayItem::Transform { matrix, items } => {
                // matrix = [a, b, c, d, tx, ty]
                let a = matrix[0];
                let b = matrix[1];
                let c = matrix[2];
                let d = matrix[3];
                let tx = matrix[4];
                let ty = matrix[5];
                for child in items {
                    Self::flatten_item_transformed(child, parent_opacity, a, b, c, d, tx, ty, quads);
                }
            }
            DisplayItem::Opacity { opacity, items } => {
                let combined = parent_opacity * opacity;
                for child in items {
                    Self::flatten_item(child, combined, quads);
                }
            }
        }
    }

    fn flatten_item_clipped(
        item: &DisplayItem,
        parent_opacity: f32,
        cx: f32,
        cy: f32,
        cw: f32,
        ch: f32,
        quads: &mut Vec<GpuQuad>,
    ) {
        match item {
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => {
                let ix = x.max(cx);
                let iy = y.max(cy);
                let ix2 = (x + width).min(cx + cw);
                let iy2 = (y + height).min(cy + ch);
                if ix2 > ix && iy2 > iy {
                    let a = (color[3] as f32 / 255.0 * parent_opacity * 255.0) as u8;
                    quads.push(GpuQuad {
                        x: ix,
                        y: iy,
                        width: ix2 - ix,
                        height: iy2 - iy,
                        color: [color[0], color[1], color[2], a],
                    });
                }
            }
            DisplayItem::Opacity { opacity, items } => {
                let combined = parent_opacity * opacity;
                for child in items {
                    Self::flatten_item_clipped(child, combined, cx, cy, cw, ch, quads);
                }
            }
            _ => {
                // For non-rect children inside clips, flatten directly (no clip intersection yet).
                Self::flatten_item(item, parent_opacity, quads);
            }
        }
    }

    fn flatten_item_transformed(
        item: &DisplayItem,
        parent_opacity: f32,
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        tx: f32,
        ty: f32,
        quads: &mut Vec<GpuQuad>,
    ) {
        match item {
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => {
                // Transform all four corners and find AABB
                let corners = [
                    Self::transform_point(*x, *y, a, b, c, d, tx, ty),
                    Self::transform_point(*x + *width, *y, a, b, c, d, tx, ty),
                    Self::transform_point(*x, *y + *height, a, b, c, d, tx, ty),
                    Self::transform_point(*x + *width, *y + *height, a, b, c, d, tx, ty),
                ];

                let min_x = corners.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
                let min_y = corners.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
                let max_x = corners.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
                let max_y = corners.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);

                let a_val = (color[3] as f32 / 255.0 * parent_opacity * 255.0) as u8;
                quads.push(GpuQuad {
                    x: min_x,
                    y: min_y,
                    width: max_x - min_x,
                    height: max_y - min_y,
                    color: [color[0], color[1], color[2], a_val],
                });
            }
            DisplayItem::Opacity { opacity, items } => {
                let combined = parent_opacity * opacity;
                for child in items {
                    Self::flatten_item_transformed(child, combined, a, b, c, d, tx, ty, quads);
                }
            }
            _ => {
                Self::flatten_item(item, parent_opacity, quads);
            }
        }
    }

    fn transform_point(
        x: f32,
        y: f32,
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        tx: f32,
        ty: f32,
    ) -> (f32, f32) {
        (a * x + c * y + tx, b * x + d * y + ty)
    }
}

impl Default for DisplayListBuilder {
    fn default() -> Self {
        Self::new()
    }
}
