use std::boxed::Box;
use std::sync::Arc;

pub trait FrameData {
    fn to_vec(&self) -> Vec<u8>;
}

pub trait Frame {
    fn get_width(&self) -> i32;
    fn get_height(&self) -> i32;
    fn get_data(&self) -> Arc<FrameData>;
    fn get_texture_id(&self) -> Result<u32, ()>;
    fn get_stride(&self) -> i32;
    fn get_offset(&self) -> i32;
}

pub trait FrameRenderer: Send + Sync + 'static {
    fn render(&self, frame: Box<Frame>);
}
