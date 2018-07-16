use AudioBackend;
use block::{Chunk, Tick, FRAMES_PER_BLOCK};
use buffer_source_node::AudioBufferSourceNode;
use channel_node::{ChannelMergerNode, ChannelSplitterNode};
use context::{ProcessingState, StateChangeResult};
use destination_node::DestinationNode;
use gain_node::GainNode;
use graph::{AudioGraph, InputPort, NodeId, OutputPort, PortId};
use node::BlockInfo;
use node::{AudioNodeEngine, AudioNodeInit, AudioNodeMessage};
use oscillator_node::OscillatorNode;
use sink::AudioSink;
use std::sync::mpsc::{Receiver, Sender};

pub enum AudioRenderThreadMsg {
    CreateNode(AudioNodeInit, Sender<NodeId>),
    ConnectPorts(PortId<OutputPort>, PortId<InputPort>),
    MessageNode(NodeId, AudioNodeMessage),
    Resume(Sender<StateChangeResult>),
    Suspend(Sender<StateChangeResult>),
    Close(Sender<StateChangeResult>),
    SinkNeedData,
    GetCurrentTime(Sender<f64>),

    DisconnectAllFrom(NodeId),
    DisconnectOutput(PortId<OutputPort>),
    DisconnectBetween(NodeId, NodeId),
    DisconnectOutputBetween(PortId<OutputPort>, NodeId),
    DisconnectOutputBetweenTo(PortId<OutputPort>, PortId<InputPort>),
}

pub struct AudioRenderThread<B: AudioBackend> {
    pub graph: AudioGraph,
    pub sink: B::Sink,
    pub state: ProcessingState,
    pub sample_rate: f32,
    pub current_time: f64,
    pub current_frame: Tick,
}

impl<B: AudioBackend> AudioRenderThread<B> {
    /// Start the audio render thread
    pub fn start(
        event_queue: Receiver<AudioRenderThreadMsg>,
        sender: Sender<AudioRenderThreadMsg>,
        sample_rate: f32,
        graph: AudioGraph,
    ) -> Result<(), ()> {

        let sink = B::make_sink()?;

        let mut graph = Self {
            graph,
            sink,
            state: ProcessingState::Suspended,
            sample_rate,
            current_time: 0.,
            current_frame: Tick(0),
        };

        graph.sink.init(sample_rate, sender)?;
        graph.event_loop(event_queue);

        Ok(())
    }

    make_render_thread_state_change!(resume, Running, play);

    make_render_thread_state_change!(suspend, Suspended, stop);

    fn create_node(&mut self, node_type: AudioNodeInit) -> NodeId {
        let node: Box<AudioNodeEngine> = match node_type {
            AudioNodeInit::AudioBufferSourceNode(options) => {
                Box::new(AudioBufferSourceNode::new(options))
            }
            AudioNodeInit::DestinationNode => Box::new(DestinationNode::new()),
            AudioNodeInit::GainNode(options) => Box::new(GainNode::new(options)),
            AudioNodeInit::OscillatorNode(options) => Box::new(OscillatorNode::new(options)),
            AudioNodeInit::ChannelMergerNode(options) => Box::new(ChannelMergerNode::new(options)),
            AudioNodeInit::ChannelSplitterNode(options) => {
                Box::new(ChannelSplitterNode::new(options))
            }
            _ => unimplemented!(),
        };
        self.graph.add_node(node)
    }

    fn connect_ports(&mut self, output: PortId<OutputPort>, input: PortId<InputPort>) {
        self.graph.add_edge(output, input)
    }

    fn process(&mut self) -> Chunk {
        let info = BlockInfo {
            sample_rate: self.sample_rate,
            frame: self.current_frame,
            time: self.current_time,
        };
        self.graph.process(&info)
    }

    fn event_loop(&mut self, event_queue: Receiver<AudioRenderThreadMsg>) {
        let sample_rate = self.sample_rate;
        let handle_msg = move |context: &mut Self, msg: AudioRenderThreadMsg| -> bool {
            let mut break_loop = false;
            match msg {
                AudioRenderThreadMsg::CreateNode(node_type, tx) => {
                    let _ = tx.send(context.create_node(node_type));
                }
                AudioRenderThreadMsg::ConnectPorts(output, input) => {
                    context.connect_ports(output, input);
                }
                AudioRenderThreadMsg::Resume(tx) => {
                    let _ = tx.send(context.resume());
                }
                AudioRenderThreadMsg::Suspend(tx) => {
                    let _ = tx.send(context.suspend());
                }
                AudioRenderThreadMsg::Close(tx) => {
                    let _ = tx.send(context.suspend());
                    break_loop = true;
                }
                AudioRenderThreadMsg::GetCurrentTime(response) => {
                    response.send(context.current_time).unwrap()
                }
                AudioRenderThreadMsg::MessageNode(id, msg) => {
                    context.graph.node_mut(id).message(msg, sample_rate)
                }
                AudioRenderThreadMsg::SinkNeedData => {
                    // Do nothing. This will simply unblock the thread so we
                    // can restart the non-blocking event loop.
                }

                AudioRenderThreadMsg::DisconnectAllFrom(id) => {
                    context.graph.disconnect_all_from(id)
                }
                AudioRenderThreadMsg::DisconnectOutput(out) => context.graph.disconnect_output(out),
                AudioRenderThreadMsg::DisconnectBetween(from, to) => {
                    context.graph.disconnect_between(from, to)
                }
                AudioRenderThreadMsg::DisconnectOutputBetween(from, to) => {
                    context.graph.disconnect_output_between(from, to)
                }
                AudioRenderThreadMsg::DisconnectOutputBetweenTo(from, to) => {
                    context.graph.disconnect_output_between_to(from, to)
                }
            };

            break_loop
        };

        loop {
            if self.sink.has_enough_data() || self.state == ProcessingState::Suspended {
                // If we are not processing audio or
                // if we have already pushed enough data into the audio sink
                // we wait for messages coming from the control thread or
                // the audio sink. The audio sink will notify whenever it
                // needs more data.
                if let Ok(msg) = event_queue.recv() {
                    if handle_msg(self, msg) {
                        break;
                    }
                }
            } else {
                // If we have not pushed enough data into the audio sink yet,
                // we process the control message queue
                if let Ok(msg) = event_queue.try_recv() {
                    if handle_msg(self, msg) {
                        break;
                    }
                }

                if self.state == ProcessingState::Suspended {
                    // Bail out if we just suspended processing.
                    continue;
                }

                // push into the audio sink the result of processing a
                // render quantum.
                let data = self.process();
                if self.sink.push_data(data).is_ok() {
                    // increment current frame by the render quantum size.
                    self.current_frame += FRAMES_PER_BLOCK;
                    self.current_time = self.current_frame / self.sample_rate as f64;
                } else {
                    eprintln!("Could not push data to audio sink");
                }
            }
        }
    }
}
