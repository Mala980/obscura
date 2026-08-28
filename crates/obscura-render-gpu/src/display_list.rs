pub struct DisplayListBuilder {
    items: Vec<DisplayItem>,
}

#[derive(Debug, Clone)]
pub enum DisplayItem {
    Rect { x: f32, y: f32, width: f32, height: f32, color: [u8; 4] },
    Text { x: f32, y: f32, text: String, font_size: f32, color: [u8; 4] },
    Image { x: f32, y: f32, width: f32, height: f32, data: Vec<u8> },
    Clip { x: f32, y: f32, width: f32, height: f32, items: Vec<DisplayItem> },
    Transform { matrix: [f32; 6], items: Vec<DisplayItem> },
    Opacity { opacity: f32, items: Vec<DisplayItem> },
}

impl DisplayListBuilder {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [u8; 4]) {
        self.items.push(DisplayItem::Rect { x, y, width: w, height: h, color });
    }

    pub fn push_text(&mut self, x: f32, y: f32, text: &str, size: f32, color: [u8; 4]) {
        self.items.push(DisplayItem::Text { x, y, text: text.to_string(), font_size: size, color });
    }

    pub fn build(&self) -> Vec<DisplayItem> {
        self.items.clone()
    }
}
