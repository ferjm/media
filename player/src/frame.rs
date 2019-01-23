use std::sync::Arc;

#[derive(Clone)]
pub struct Frame {
    width: i32,
    height: i32,
    data: Arc<Vec<u8>>,
    stride: Option<i32>,
    offset: i32,
}

impl Frame {
    pub fn new(
        width: i32,
        height: i32,
        data: Arc<Vec<u8>>,
        stride: Option<i32>,
        offset: i32,
    ) -> Frame {
        Frame {
            width,
            height,
            data,
            stride,
            offset,
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

    pub fn get_stride(&self) -> Option<i32> {
        self.stride
    }

    pub fn get_offset(&self) -> i32 {
        self.offset
    }
}

pub trait FrameRenderer: Send + 'static {
    fn render(&mut self, frame: Frame);
}
