use decoder::{AudioDecoder, AudioDecoderCallbacks, AudioDecoderOptions};
use graph::{AudioGraph, InputPort, NodeId, OutputPort, PortId};
use node::{AudioNodeInit, AudioNodeMessage};
use render_thread::AudioRenderThread;
use render_thread::AudioRenderThreadMsg;
use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::mpsc::{self, Sender};
use std::thread::Builder;
use AudioBackend;

/// Describes the state of the audio context on the control thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProcessingState {
    /// The audio context is suspended (context time is not proceeding,
    /// audio hardware may be powered down/released).
    Suspended,
    /// Audio is being processed.
    Running,
    /// The audio context has been released, and can no longer be used
    /// to process audio.
    Closed,
}

pub type StateChangeResult = Result<(), ()>;

/// Identify the type of playback, which affects tradeoffs between audio output
/// and power consumption.
pub enum LatencyCategory {
    /// Balance audio output latency and power consumption.
    Balanced,
    /// Provide the lowest audio output latency possible without glitching.
    Interactive,
    /// Prioritize sustained playback without interruption over audio output latency.
    /// Lowest power consumption.
    Playback,
}

/// User-specified options for a real time audio context.
pub struct RealTimeAudioContextOptions {
    /// Number of samples that will play in one second, measured in Hz.
    pub sample_rate: f32,
    /// Type of playback.
    pub latency_hint: LatencyCategory,
}

impl Default for RealTimeAudioContextOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48000.,
            latency_hint: LatencyCategory::Interactive,
        }
    }
}

/// User-specified options for an offline audio context.
pub struct OfflineAudioContextOptions {
    /// The number of channels for this offline audio context.
    pub channels: u32,
    /// The length of the rendered audio buffer in sample-frames.
    pub length: u32,
    /// Number of samples that will be rendered in one second, measured in Hz.
    pub sample_rate: f32,
}

impl Default for OfflineAudioContextOptions {
    fn default() -> Self {
        Self {
            channels: 1,
            length: 0,
            sample_rate: 48000.,
        }
    }
}

impl From<RealTimeAudioContextOptions> for AudioContextOptions {
    fn from(options: RealTimeAudioContextOptions) -> Self {
        AudioContextOptions::RealTimeAudioContext(options)
    }
}

impl From<OfflineAudioContextOptions> for AudioContextOptions {
    fn from(options: OfflineAudioContextOptions) -> Self {
        AudioContextOptions::OfflineAudioContext(options)
    }
}

/// User-specified options for a real time or offline audio context.
pub enum AudioContextOptions {
    RealTimeAudioContext(RealTimeAudioContextOptions),
    OfflineAudioContext(OfflineAudioContextOptions),
}

impl Default for AudioContextOptions {
    fn default() -> Self {
        AudioContextOptions::RealTimeAudioContext(Default::default())
    }
}

/// Representation of an audio context on the control thread.
pub struct AudioContext<B> {
    /// Rendering thread communication channel.
    sender: Sender<AudioRenderThreadMsg>,
    /// State of the audio context on the control thread.
    state: Cell<ProcessingState>,
    /// Number of samples that will be played in one second.
    sample_rate: f32,
    /// The identifier of an AudioDestinationNode with a single input
    /// representing the final destination for all audio.
    dest_node: NodeId,
    backend: PhantomData<B>,
}

impl<B: AudioBackend> AudioContext<B> {
    /// Constructs a new audio context.
    pub fn new(options: AudioContextOptions) -> Self {
        let options = match options {
            AudioContextOptions::RealTimeAudioContext(options) => options,
            AudioContextOptions::OfflineAudioContext(_) => unimplemented!(),
        };
        let sample_rate = options.sample_rate;

        let (sender, receiver) = mpsc::channel();
        let sender_ = sender.clone();
        let graph = AudioGraph::new();
        let dest_node = graph.dest_id();
        Builder::new()
            .name("AudioRenderThread".to_owned())
            .spawn(move || {
                AudioRenderThread::<B>::start(receiver, sender_, options.sample_rate, graph)
                    .expect("Could not start AudioRenderThread");
            })
            .unwrap();
        Self {
            sender,
            state: Cell::new(ProcessingState::Suspended),
            sample_rate,
            dest_node,
            backend: PhantomData,
        }
    }

    pub fn state(&self) -> ProcessingState {
        self.state.get()
    }

    pub fn dest_node(&self) -> NodeId {
        self.dest_node
    }

    pub fn current_time(&self) -> f64 {
        let (tx, rx) = mpsc::channel();
        let _ = self.sender.send(AudioRenderThreadMsg::GetCurrentTime(tx));
        rx.recv().unwrap()
    }

    pub fn create_node(&self, node_type: AudioNodeInit) -> NodeId {
        let (tx, rx) = mpsc::channel();
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::CreateNode(node_type, tx));
        rx.recv().unwrap()
    }

    /// Resume audio processing.
    make_state_change!(resume, Running, Resume);

    /// Suspend audio processing.
    make_state_change!(suspend, Suspended, Suspend);

    /// Stop audio processing and close render thread.
    make_state_change!(close, Closed, Close);

    pub fn message_node(&self, id: NodeId, msg: AudioNodeMessage) {
        let _ = self.sender.send(AudioRenderThreadMsg::MessageNode(id, msg));
    }

    pub fn connect_ports(&self, from: PortId<OutputPort>, to: PortId<InputPort>) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::ConnectPorts(from, to));
    }

    pub fn disconnect_all_from(&self, node: NodeId) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::DisconnectAllFrom(node));
    }

    // /// Disconnect all outgoing connections from a node's output
    // ///
    // /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-output
    pub fn disconnect_output(&self, out: PortId<OutputPort>) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::DisconnectOutput(out));
    }

    /// Disconnect connections from a node to another node
    ///
    /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode
    pub fn disconnect_between(&self, from: NodeId, to: NodeId) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::DisconnectBetween(from, to));
    }

    /// Disconnect all outgoing connections from a node's output to another node
    ///
    /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode-output
    pub fn disconnect_output_between(&self, out: PortId<OutputPort>, to: NodeId) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::DisconnectOutputBetween(out, to));
    }

    // /// Disconnect all outgoing connections from a node's output to another node's input
    // ///
    // /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode-output-input
    pub fn disconnect_output_between_to(&self, out: PortId<OutputPort>, inp: PortId<InputPort>) {
        let _ = self
            .sender
            .send(AudioRenderThreadMsg::DisconnectOutputBetweenTo(out, inp));
    }

    /// Asynchronously decodes the audio file data contained in the given
    /// buffer.
    pub fn decode_audio_data(&self, data: Vec<u8>, callbacks: AudioDecoderCallbacks) {
        let mut options = AudioDecoderOptions::default();
        options.sample_rate = self.sample_rate;
        Builder::new()
            .name("AudioDecoder".to_owned())
            .spawn(move || {
                let audio_decoder = B::make_decoder();

                audio_decoder.decode(data, callbacks, Some(options));
            })
            .unwrap();
    }
}

impl<T> Drop for AudioContext<T> {
    fn drop(&mut self) {
        let (tx, _) = mpsc::channel();
        let _ = self.sender.send(AudioRenderThreadMsg::Close(tx));
    }
}
