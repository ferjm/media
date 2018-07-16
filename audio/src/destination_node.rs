use block::Chunk;
use node::{AudioNodeEngine, BlockInfo};
use node::{AudioNodeType, ChannelCountMode, ChannelInfo};

#[derive(AudioNodeCommon)]
pub(crate) struct DestinationNode {
    channel_info: ChannelInfo,
    chunk: Option<Chunk>,
}

impl DestinationNode {
    pub fn new() -> Self {
        DestinationNode {
            channel_info: ChannelInfo {
                mode: ChannelCountMode::Explicit,
                ..Default::default()
            },
            chunk: None,
        }
    }
}

impl AudioNodeEngine for DestinationNode {
    fn node_type(&self) -> AudioNodeType {
        AudioNodeType::DestinationNode
    }

    fn process(&mut self, inputs: Chunk, _: &BlockInfo) -> Chunk {
        self.chunk = Some(inputs);
        Chunk::default()
    }

    fn destination_data(&mut self) -> Option<Chunk> {
        self.chunk.take()
    }

    fn output_count(&self) -> u32 {
        0
    }

    fn set_channel_count_mode(&mut self, _: ChannelCountMode) {
        panic!("destination nodes cannot have their mode changed");
    }
}
