use std::sync::Arc;

#[derive(Clone)]
pub struct Frame {
    width: i32,
    height: i32,
    data: Arc<Vec<u8>>,
}

impl Frame {
    pub fn new(width: i32, height: i32, data: Arc<Vec<u8>>) -> Frame {
        Frame {
            width,
            height,
            data,
        }
    }

    pub fn get_width(&self) -> i32 {
        self.width
    }

    pub fn get_height(&self) -> i32 {
        self.height
    }

    pub fn get_data(&self) -> &Arc<Vec<u8>> {
        &self.data
    }
}

pub trait FrameRenderer: Send + Sync + 'static {
    fn render(&self, frame: Frame);
}
