use block::{Block, Chunk, Tick, FRAMES_PER_BLOCK};
use node::{AudioNodeEngine, AudioScheduledSourceNodeMessage, BlockInfo, OnEndedCallback};
use node::{AudioNodeType, ChannelInfo};
use param::{Param, ParamType};

/// Control messages directed to AudioBufferSourceNodes.
#[derive(Debug, Clone)]
pub enum AudioBufferSourceNodeMessage {
    /// Set the data block holding the audio sample data to be played.
    SetBuffer(Option<AudioBuffer>),
}

/// This specifies options for constructing an AudioBufferSourceNode.
#[derive(Debug, Clone)]
pub struct AudioBufferSourceNodeOptions {
    /// The audio asset to be played.
    pub buffer: Option<AudioBuffer>,
    /// The initial value for the detune AudioParam.
    pub detune: f32,
    /// The initial value for the loop_enabled attribute.
    pub loop_enabled: bool,
    /// The initial value for the loop_end attribute.
    pub loop_end: Option<f64>,
    /// The initial value for the loop_start attribute.
    pub loop_start: Option<f64>,
    /// The initial value for the playback_rate AudioParam.
    pub playback_rate: f32,
}

impl Default for AudioBufferSourceNodeOptions {
    fn default() -> Self {
        AudioBufferSourceNodeOptions {
            buffer: None,
            detune: 0.,
            loop_enabled: false,
            loop_end: None,
            loop_start: None,
            playback_rate: 1.,
        }
    }
}

/// AudioBufferSourceNode engine.
/// https://webaudio.github.io/web-audio-api/#AudioBufferSourceNode
/// XXX Implement looping
/// XXX Implement playbackRate and related bits
#[derive(AudioScheduledSourceNode, AudioNodeCommon)]
#[allow(dead_code)]
pub(crate) struct AudioBufferSourceNode {
    channel_info: ChannelInfo,
    /// A data block holding the audio sample data to be played.
    buffer: Option<AudioBuffer>,
    /// AudioParam to modulate the speed at which is rendered the audio stream.
    detune: Param,
    /// Indicates if the region of audio data designated by loopStart and loopEnd
    /// should be played continuously in a loop.
    loop_enabled: bool,
    /// An playhead position where looping should end if the loop_enabled
    /// attribute is true.
    loop_end: Option<f64>,
    /// An playhead position where looping should begin if the loop_enabled
    /// attribute is true.
    loop_start: Option<f64>,
    /// Playback offset.
    playback_offset: usize,
    /// The speed at which to render the audio stream.
    playback_rate: Param,
    /// Time at which the source should start playing.
    start_at: Option<Tick>,
    /// Time at which the source should stop playing.
    stop_at: Option<Tick>,
    /// The ended event callback.
    pub onended_callback: Option<OnEndedCallback>,
}

impl AudioBufferSourceNode {
    pub fn new(options: AudioBufferSourceNodeOptions) -> Self {
        Self {
            channel_info: Default::default(),
            buffer: options.buffer,
            detune: Param::new(options.detune),
            loop_enabled: options.loop_enabled,
            loop_end: options.loop_end,
            loop_start: options.loop_start,
            playback_offset: 0,
            playback_rate: Param::new(options.playback_rate),
            start_at: None,
            stop_at: None,
            onended_callback: None,
        }
    }

    pub fn handle_message(&mut self, message: AudioBufferSourceNodeMessage, _: f32) {
        match message {
            AudioBufferSourceNodeMessage::SetBuffer(buffer) => {
                self.buffer = buffer;
            }
        }
    }
}

impl AudioNodeEngine for AudioBufferSourceNode {
    fn node_type(&self) -> AudioNodeType {
        AudioNodeType::AudioBufferSourceNode
    }

    fn input_count(&self) -> u32 {
        0
    }

    fn process(&mut self, mut inputs: Chunk, info: &BlockInfo) -> Chunk {
        debug_assert!(inputs.len() == 0);

        if self.buffer.is_none() {
            inputs.blocks.push(Default::default());
            return inputs;
        }

        let len = { self.buffer.as_ref().unwrap().len() as usize };

        if self.playback_offset >= len || self.should_play_at(info.frame) == (false, true) {
            self.maybe_trigger_onended_callback();
            inputs.blocks.push(Default::default());
            return inputs;
        }

        let buffer = self.buffer.as_ref().unwrap();

        let samples_to_copy = match self.stop_at {
            Some(stop_at) => {
                let ticks_to_stop = stop_at - info.frame;
                if ticks_to_stop > FRAMES_PER_BLOCK {
                    FRAMES_PER_BLOCK.0 as usize
                } else {
                    ticks_to_stop.0 as usize
                }
            }
            None => FRAMES_PER_BLOCK.0 as usize,
        };

        let next_offset = self.playback_offset + samples_to_copy;

        if samples_to_copy == FRAMES_PER_BLOCK.0 as usize {
            // copy entire chan
            let mut block = Block::empty();
            for chan in 0..buffer.chans() {
                block.push_chan(&buffer.buffers[chan as usize][self.playback_offset..next_offset]);
            }
            inputs.blocks.push(block)
        } else {
            // silent fill and copy
            let mut block = Block::default();
            block.repeat(buffer.chans());
            block.explicit_repeat();
            for chan in 0..buffer.chans() {
                let data = block.data_chan_mut(chan);
                data.copy_from_slice(
                    &buffer.buffers[chan as usize][self.playback_offset..next_offset],
                );
            }
            inputs.blocks.push(block)
        }

        self.playback_offset = next_offset;
        inputs
    }

    fn get_param(&mut self, id: ParamType) -> &mut Param {
        match id {
            ParamType::PlaybackRate => &mut self.playback_rate,
            ParamType::Detune => &mut self.detune,
            _ => panic!("Unknown param {:?} for AudioBufferSourceNode", id),
        }
    }

    make_message_handler!(
        AudioBufferSourceNode: handle_message,
        AudioScheduledSourceNode: handle_source_node_message
    );
}

#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// Invariant: all buffers must be of the same length
    pub buffers: Vec<Vec<f32>>,
}

impl AudioBuffer {
    pub fn new(chan: u8, len: usize) -> Self {
        assert!(chan > 0);
        let mut buffers = Vec::with_capacity(chan as usize);
        let single = vec![0.; len];
        buffers.resize(chan as usize, single);
        AudioBuffer { buffers }
    }

    pub fn from_buffers(buffers: Vec<Vec<f32>>) -> Self {
        for buf in &buffers {
            assert!(buf.len() == buffers[0].len())
        }

        Self { buffers }
    }

    pub fn len(&self) -> usize {
        self.buffers[0].len()
    }

    pub fn chans(&self) -> u8 {
        self.buffers.len() as u8
    }
}

impl From<Vec<f32>> for AudioBuffer {
    fn from(vec: Vec<f32>) -> Self {
        Self { buffers: vec![vec] }
    }
}

impl From<Vec<Vec<f32>>> for AudioBuffer {
    fn from(vec: Vec<Vec<f32>>) -> Self {
        AudioBuffer::from_buffers(vec)
    }
}
