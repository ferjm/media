use std::boxed::Box;
use std::sync::Arc;

pub trait Frame {
    fn get_width(&self) -> i32;
    fn get_height(&self) -> i32;
    fn get_data(&self) -> &Arc<Vec<u8>>;
    fn get_texture_id(&self) -> Result<u32, ()>;
    fn get_stride(&self) -> i32;
    fn get_offset(&self) -> i32;
}

pub trait FrameRenderer: Send + 'static {
    fn render<Frame>(&self, frame: Box<dyn self::Frame>);
}
