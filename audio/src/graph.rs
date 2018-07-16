use block::{Block, Chunk};
use destination_node::DestinationNode;
use node::{AudioNodeEngine, BlockInfo, ChannelCountMode};
use petgraph::graph::DefaultIx;
use petgraph::stable_graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::visit::{DfsPostOrder, EdgeRef, Reversed};
use petgraph::Direction;
use smallvec::SmallVec;
use std::cell::{RefCell, RefMut};
use std::cmp;

#[derive(Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Debug)]
/// A unique identifier for nodes in the graph. Stable
/// under graph mutation.
pub struct NodeId(NodeIndex<DefaultIx>);

impl NodeId {
    pub fn input(self, port: u32) -> PortId<InputPort> {
        PortId(self, PortIndex(port, InputPort))
    }
    pub fn output(self, port: u32) -> PortId<OutputPort> {
        PortId(self, PortIndex(port, OutputPort))
    }
}

/// A zero-indexed "port" for a node. Most nodes have one
/// input and one output port, but some may have more.
/// For example, a channel splitter node will have one output
/// port for each channel.
///
/// These are essentially indices into the Chunks
///
/// Kind is a zero sized type and is useful for distinguishing
/// between input and output ports (which may otherwise share indices)
#[derive(Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Debug)]
pub struct PortIndex<Kind>(pub u32, pub Kind);

impl<Kind> PortId<Kind> {
    pub fn node(&self) -> NodeId {
        self.0
    }
}

/// An identifier for a port.
#[derive(Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Debug)]
pub struct PortId<Kind>(NodeId, PortIndex<Kind>);

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
/// Marker type for denoting that the port is an input port
/// of the node it is connected to
pub struct InputPort;
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
/// Marker type for denoting that the port is an output port
/// of the node it is connected to
pub struct OutputPort;

pub struct AudioGraph {
    graph: StableGraph<Node, Edge>,
    dest_id: NodeId,
}

pub(crate) struct Node {
    node: RefCell<Box<AudioNodeEngine>>,
}

/// An edge in the graph
///
/// This connects one or more pair of ports between two
/// nodes, each connection represented by a `Connection`.
/// WebAudio allows for multiple connections to/from the same port
/// however it does not allow for duplicate connections between pairs
/// of ports
pub(crate) struct Edge {
    connections: SmallVec<[Connection; 1]>,
}

impl Edge {
    /// Find if there are connections between two given ports, return the index
    fn has_between(
        &self,
        output_idx: PortIndex<OutputPort>,
        input_idx: PortIndex<InputPort>,
    ) -> bool {
        self.connections
            .iter()
            .find(|e| e.input_idx == input_idx && e.output_idx == output_idx)
            .is_some()
    }

    fn remove_by_output(&mut self, output_idx: PortIndex<OutputPort>) {
        self.connections.retain(|i| i.output_idx != output_idx)
    }

    fn remove_by_pair(
        &mut self,
        output_idx: PortIndex<OutputPort>,
        input_idx: PortIndex<InputPort>,
    ) {
        self.connections
            .retain(|i| i.output_idx != output_idx || i.input_idx != input_idx)
    }
}

/// A single connection between ports
struct Connection {
    /// The index of the port on the input node
    /// This is actually the /output/ of this edge
    input_idx: PortIndex<InputPort>,
    /// The index of the port on the output node
    /// This is actually the /input/ of this edge
    output_idx: PortIndex<OutputPort>,
    /// When the from node finishes processing, it will push
    /// its data into this cache for the input node to read
    cache: RefCell<Option<Block>>,
}

impl AudioGraph {
    pub fn new() -> Self {
        let mut graph = StableGraph::new();
        let dest_id = NodeId(graph.add_node(Node::new(Box::new(DestinationNode::new()))));
        AudioGraph { graph, dest_id }
    }

    /// Create a node, obtain its id
    pub(crate) fn add_node(&mut self, node: Box<AudioNodeEngine>) -> NodeId {
        NodeId(self.graph.add_node(Node::new(node)))
    }

    /// Connect an output port to an input port
    ///
    /// The edge goes *from* the output port *to* the input port, connecting two nodes
    pub fn add_edge(&mut self, out: PortId<OutputPort>, inp: PortId<InputPort>) {
        let edge = self
            .graph
            .edges(out.node().0)
            .find(|e| e.target() == inp.node().0)
            .map(|e| e.id());
        if let Some(e) = edge {
            // .find(|e| e.weight().has_between(out.1, inp.1));
            let w = self
                .graph
                .edge_weight_mut(e)
                .expect("This edge is known to exist");
            if w.has_between(out.1, inp.1) {
                return;
            }
            w.connections.push(Connection::new(inp.1, out.1))
        } else {
            // add a new edge
            self.graph
                .add_edge(out.node().0, inp.node().0, Edge::new(inp.1, out.1));
        }
    }

    /// Disconnect all outgoing connections from a node
    ///
    /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect
    pub fn disconnect_all_from(&mut self, node: NodeId) {
        let edges = self.graph.edges(node.0).map(|e| e.id()).collect::<Vec<_>>();
        for edge in edges {
            self.graph.remove_edge(edge);
        }
    }

    // /// Disconnect all outgoing connections from a node's output
    // ///
    // /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-output
    pub fn disconnect_output(&mut self, out: PortId<OutputPort>) {
        let candidates: Vec<_> = self
            .graph
            .edges(out.node().0)
            .map(|e| (e.id(), e.target()))
            .collect();
        for (edge, to) in candidates {
            let mut e = self
                .graph
                .remove_edge(edge)
                .expect("Edge index is known to exist");
            e.remove_by_output(out.1);
            if !e.connections.is_empty() {
                self.graph.add_edge(out.node().0, to, e);
            }
        }
    }

    /// Disconnect connections from a node to another node
    ///
    /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode
    pub fn disconnect_between(&mut self, from: NodeId, to: NodeId) {
        let edge = self
            .graph
            .edges(from.0)
            .find(|e| e.target() == to.0)
            .map(|e| e.id());
        if let Some(i) = edge {
            self.graph.remove_edge(i);
        }
    }

    /// Disconnect all outgoing connections from a node's output to another node
    ///
    /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode-output
    pub fn disconnect_output_between(&mut self, out: PortId<OutputPort>, to: NodeId) {
        let edge = self
            .graph
            .edges(out.node().0)
            .find(|e| e.target() == to.0)
            .map(|e| e.id());
        if let Some(edge) = edge {
            let mut e = self
                .graph
                .remove_edge(edge)
                .expect("Edge index is known to exist");
            e.remove_by_output(out.1);
            if !e.connections.is_empty() {
                self.graph.add_edge(out.node().0, to.0, e);
            }
        }
    }

    // /// Disconnect all outgoing connections from a node's output to another node's input
    // ///
    // /// https://webaudio.github.io/web-audio-api/#dom-audionode-disconnect-destinationnode-output-input
    pub fn disconnect_output_between_to(
        &mut self,
        out: PortId<OutputPort>,
        inp: PortId<InputPort>,
    ) {
        let edge = self
            .graph
            .edges(out.node().0)
            .find(|e| e.target() == inp.node().0)
            .map(|e| e.id());
        if let Some(edge) = edge {
            let mut e = self
                .graph
                .remove_edge(edge)
                .expect("Edge index is known to exist");
            e.remove_by_pair(out.1, inp.1);
            if !e.connections.is_empty() {
                self.graph.add_edge(out.node().0, inp.node().0, e);
            }
        }
    }

    /// Get the id of the destination node in this graph
    ///
    /// All graphs have a destination node, with one input port
    pub fn dest_id(&self) -> NodeId {
        self.dest_id
    }

    /// For a given block, process all the data on this graph
    pub fn process(&mut self, info: &BlockInfo) -> Chunk {
        // DFS post order: Children are processed before their parent,
        // which is exactly what we need since the parent depends on the
        // children's output
        //
        // This will only visit each node once
        let reversed = Reversed(&self.graph);
        let mut visit = DfsPostOrder::new(reversed, self.dest_id.0);

        let mut blocks: SmallVec<[SmallVec<[Block; 1]>; 1]> = SmallVec::new();
        let mut output_counts: SmallVec<[u32; 1]> = SmallVec::new();

        while let Some(ix) = visit.next(reversed) {
            let mut curr = self.graph[ix].node.borrow_mut();

            let mut chunk = Chunk::default();
            chunk
                .blocks
                .resize(curr.input_count() as usize, Default::default());
            // if we have inputs, collect all the computed blocks
            // and construct a Chunk
            if curr.input_count() > 0 {
                // set up scratch space to store all the blocks
                blocks.clear();
                blocks.resize(curr.input_count() as usize, Default::default());

                let mode = curr.channel_count_mode();
                let count = curr.channel_count();
                let interpretation = curr.channel_interpretation();

                // all edges to this node are from its dependencies
                for edge in self.graph.edges_directed(ix, Direction::Incoming) {
                    let edge = edge.weight();
                    for connection in &edge.connections {
                        let block = connection
                            .cache
                            .borrow_mut()
                            .take()
                            .expect("Cache should have been filled from traversal");
                        blocks[connection.input_idx.0 as usize].push(block);
                    }
                }

                for (i, mut blocks) in blocks.drain().enumerate() {
                    if blocks.len() == 0 {
                        if mode == ChannelCountMode::Explicit {
                            // It's silence, but mix it anyway
                            chunk.blocks[i].mix(count, interpretation);
                        }
                    } else if blocks.len() == 1 {
                        chunk.blocks[i] = blocks.pop().expect("`blocks` had length 1");
                        match mode {
                            ChannelCountMode::Explicit => {
                                chunk.blocks[i].mix(count, interpretation);
                            }
                            ChannelCountMode::ClampedMax => {
                                if chunk.blocks[i].chan_count() > count {
                                    chunk.blocks[i].mix(count, interpretation);
                                }
                            }
                            // It's one channel, it maxes itself
                            ChannelCountMode::Max => (),
                        }
                    } else {
                        let mix_count = match mode {
                            ChannelCountMode::Explicit => count,
                            _ => {
                                let mut max = 0; // max channel count
                                for block in &blocks {
                                    max = cmp::max(max, block.chan_count());
                                }
                                if mode == ChannelCountMode::ClampedMax {
                                    max = cmp::min(max, count);
                                }
                                max
                            }
                        };
                        let block = blocks.into_iter().fold(Block::default(), |acc, mut block| {
                            block.mix(mix_count, interpretation);
                            acc.sum(block)
                        });
                        chunk.blocks[i] = block;
                    }
                }
            }

            // actually run the node engine
            let mut out = curr.process(chunk, info);

            assert_eq!(out.len(), curr.output_count() as usize);
            if curr.output_count() == 0 {
                continue;
            }

            // Count how many output connections fan out from each port
            // This is so that we don't have to needlessly clone audio buffers
            //
            // If this is inefficient, we can instead maintain this data
            // cached on the node
            output_counts.clear();
            output_counts.resize(curr.output_count() as usize, 0);
            for edge in self.graph.edges(ix) {
                let edge = edge.weight();
                for conn in &edge.connections {
                    output_counts[conn.output_idx.0 as usize] += 1;
                }
            }

            // all the edges from this node go to nodes which depend on it,
            // i.e. the nodes it outputs to. Store the blocks for retrieval.
            for edge in self.graph.edges(ix) {
                let edge = edge.weight();
                for conn in &edge.connections {
                    output_counts[conn.output_idx.0 as usize] -= 1;
                    // if there are no consumers left after this, take the data
                    let block = if output_counts[conn.output_idx.0 as usize] == 0 {
                        out[conn.output_idx].take()
                    } else {
                        out[conn.output_idx].clone()
                    };
                    *conn.cache.borrow_mut() = Some(block);
                }
            }
        }

        // The destination node stores its output on itself, extract it.
        self.graph[self.dest_id.0]
            .node
            .borrow_mut()
            .destination_data()
            .expect("Destination node should have data cached")
    }

    /// Obtain a mutable reference to a node
    pub(crate) fn node_mut(&self, ix: NodeId) -> RefMut<Box<AudioNodeEngine>> {
        self.graph[ix.0].node.borrow_mut()
    }
}

impl Node {
    pub fn new(node: Box<AudioNodeEngine>) -> Self {
        Node {
            node: RefCell::new(node),
        }
    }
}

impl Edge {
    pub fn new(input_idx: PortIndex<InputPort>, output_idx: PortIndex<OutputPort>) -> Self {
        Edge {
            connections: SmallVec::from_buf([Connection::new(input_idx, output_idx)]),
        }
    }
}

impl Connection {
    pub fn new(input_idx: PortIndex<InputPort>, output_idx: PortIndex<OutputPort>) -> Self {
        Connection {
            input_idx,
            output_idx,
            cache: RefCell::new(None),
        }
    }
}
