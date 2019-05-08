use super::BACKEND_BASE_TIME;
use boxfnonce::SendBoxFnOnce;
use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_sdp;
use gst_webrtc;
use media_stream::GStreamerMediaStream;
use servo_media_streams::registry::{get_stream, MediaStreamId};
use servo_media_streams::MediaStreamType;
use servo_media_webrtc::thread::InternalEvent;
use servo_media_webrtc::WebRtcController as WebRtcThread;
use servo_media_webrtc::*;
use std::sync::{Arc, Mutex};
use std::{cmp, mem};

// TODO:
// - figure out purpose of glib loop

#[derive(Debug, Clone)]
pub struct MLineInfo {
    /// The caps for the given m-line
    caps: gst::Caps,
    /// Whether or not this sink pad has already been connected
    is_used: bool,
    /// The payload value of the given m-line
    payload: i32,
}

pub struct GStreamerWebRtcController {
    webrtc: gst::Element,
    pipeline: gst::Pipeline,
    /// We can't trigger a negotiation-needed event until we have streams, or otherwise
    /// a createOffer() call will lead to bad SDP. Instead, we delay negotiation.
    delayed_negotiation: bool,
    /// A handle to the event loop abstraction surrounding the webrtc implementations,
    /// which lets gstreamer callbacks send events back to the event loop to run on this object
    thread: WebRtcThread,
    signaller: Box<WebRtcSignaller>,
    /// All the streams that are actually connected to the webrtcbin (i.e., their presence has already
    /// been negotiated)
    streams: Vec<MediaStreamId>,
    /// Disconnected streams that are waiting to be linked. Streams are
    /// only linked when:
    ///
    /// - An offer is made (all pending streams are flushed)
    /// - An offer is received (all matching pending streams are flushed)
    /// - A stream is added when there is a so-far-disconnected remote-m-line
    ///
    /// In other words, these are all yet to be negotiated
    ///
    /// See link_stream
    pending_streams: Vec<MediaStreamId>,
    /// Each new webrtc stream should have a new payload/pt value, starting at 96
    ///
    /// This is maintained as a known yet-unused payload number, being incremented whenever
    /// we use it, and set to (remote_pt + 1) if the remote sends us a stream with a higher pt
    pt_counter: i32,
    /// We keep track of how many request pads have been created on webrtcbin
    /// so that we can request more to fill in the gaps and acquire a specific pad if necessary
    request_pad_counter: usize,
    /// Streams need to be connected to the relevant sink pad, and we figure this out
    /// by keeping track of the caps of each m-line in the SDP.
    remote_mline_info: Vec<MLineInfo>,
    /// Temporary storage for remote_mline_info until the remote description is applied
    ///
    /// Without this, a unluckily timed call to link_stream() may happen before the webrtcbin
    /// knows the remote description, but while we _think_ it does
    pending_remote_mline_info: Vec<MLineInfo>,
    /// In case we get multiple remote offers, this lets us keep track of which is the newest
    remote_offer_generation: u32,
    //send_msg_tx: mpsc::Sender<OwnedMessage>,
    //peer_id: String,
    _main_loop: glib::MainLoop,
    //bus: gst::Bus,
}

impl WebRtcControllerBackend for GStreamerWebRtcController {
    fn add_ice_candidate(&mut self, candidate: IceCandidate) -> WebrtcResult {
        self.webrtc.emit(
            "add-ice-candidate",
            &[&candidate.sdp_mline_index, &candidate.candidate],
        )?;
        Ok(())
    }

    fn set_remote_description(
        &mut self,
        desc: SessionDescription,
        cb: SendBoxFnOnce<'static, ()>,
    ) -> WebrtcResult {
        self.set_description(desc, DescriptionType::Remote, cb)
    }

    fn set_local_description(
        &mut self,
        desc: SessionDescription,
        cb: SendBoxFnOnce<'static, ()>,
    ) -> WebrtcResult {
        self.set_description(desc, DescriptionType::Local, cb)
    }

    fn create_offer(&mut self, cb: SendBoxFnOnce<'static, (SessionDescription,)>) -> WebrtcResult {
        self.flush_pending_streams(true)?;
        self.pipeline.set_state(gst::State::Playing)?;
        let promise = gst::Promise::new_with_change_func(move |promise| {
            on_offer_or_answer_created(SdpType::Offer, promise, cb);
        });

        self.webrtc
            .emit("create-offer", &[&None::<gst::Structure>, &promise])?;
        Ok(())
    }

    fn create_answer(&mut self, cb: SendBoxFnOnce<'static, (SessionDescription,)>) -> WebrtcResult {
        let promise = gst::Promise::new_with_change_func(move |promise| {
            on_offer_or_answer_created(SdpType::Answer, promise, cb);
        });

        self.webrtc
            .emit("create-answer", &[&None::<gst::Structure>, &promise])?;
        Ok(())
    }

    fn add_stream(&mut self, stream_id: &MediaStreamId) -> WebrtcResult {
        let stream =
            get_stream(stream_id).expect("Media streams registry does not contain such ID");
        let mut stream = stream.lock().unwrap();
        let mut stream = stream
            .as_mut_any()
            .downcast_mut::<GStreamerMediaStream>()
            .ok_or("Does not currently support non-gstreamer streams")?;
        stream.insert_capsfilter();
        self.link_stream(stream_id, &mut stream, false)?;
        if self.delayed_negotiation && (self.streams.len() > 1 || self.pending_streams.len() > 1) {
            self.delayed_negotiation = false;
            self.signaller.on_negotiation_needed(&self.thread);
        }
        Ok(())
    }

    fn configure(&mut self, stun_server: &str, policy: BundlePolicy) -> WebrtcResult {
        self.webrtc
            .set_property_from_str("stun-server", stun_server);
        self.webrtc
            .set_property_from_str("bundle-policy", policy.as_str());
        Ok(())
    }

    fn internal_event(&mut self, e: thread::InternalEvent) -> WebrtcResult {
        match e {
            InternalEvent::OnNegotiationNeeded => {
                if self.streams.is_empty() && self.pending_streams.is_empty() {
                    // we have no streams

                    // If the pipeline starts playing and on-negotiation-needed is present before there are any
                    // media streams, an invalid SDP offer will be created. Therefore, delay emitting the signal
                    self.delayed_negotiation = true;
                } else {
                    self.signaller.on_negotiation_needed(&self.thread);
                }
            }
            InternalEvent::OnIceCandidate(candidate) => {
                self.signaller.on_ice_candidate(&self.thread, candidate);
            }
            InternalEvent::OnAddStream(stream, ty) => {
                self.pipeline.set_state(gst::State::Playing)?;
                self.signaller.on_add_stream(&stream, ty);
            }
            InternalEvent::DescriptionAdded(cb, description_type, ty, remote_offer_generation) => {
                if description_type == DescriptionType::Remote
                    && ty == SdpType::Offer
                    && remote_offer_generation == self.remote_offer_generation
                {
                    mem::swap(
                        &mut self.pending_remote_mline_info,
                        &mut self.remote_mline_info,
                    );
                    self.pending_remote_mline_info.clear();
                    self.flush_pending_streams(false)?;
                }
                self.pipeline.set_state(gst::State::Playing)?;
                cb.call();
            }
            InternalEvent::UpdateSignalingState => {
                use gst_webrtc::WebRTCSignalingState::*;
                let prop = self.webrtc.get_property("signaling-state")?;
                let val = prop
                    .downcast::<gst_webrtc::WebRTCSignalingState>()
                    .map_err(|_| "unable to downcast signaling state")?
                    .get_some();
                let state = match val {
                    Stable => SignalingState::Stable,
                    HaveLocalOffer => SignalingState::HaveLocalOffer,
                    HaveRemoteOffer => SignalingState::HaveRemoteOffer,
                    HaveLocalPranswer => SignalingState::HaveLocalPranswer,
                    HaveRemotePranswer => SignalingState::HaveRemotePranswer,
                    Closed => SignalingState::Closed,
                    i => {
                        return Err(WebrtcError::Backend(format!(
                            "unknown signaling state: {:?}",
                            i
                        )))
                    }
                };
                self.signaller.update_signaling_state(state);
            }
            InternalEvent::UpdateGatheringState => {
                use gst_webrtc::WebRTCICEGatheringState::*;
                let prop = self.webrtc.get_property("ice-gathering-state")?;
                let val = prop
                    .downcast::<gst_webrtc::WebRTCICEGatheringState>()
                    .map_err(|_| "unable to downcast gathering state")?
                    .get_some();
                let state = match val {
                    New => GatheringState::New,
                    Gathering => GatheringState::Gathering,
                    Complete => GatheringState::Complete,
                    i => {
                        return Err(WebrtcError::Backend(format!(
                            "unknown gathering state: {:?}",
                            i
                        )))
                    }
                };
                self.signaller.update_gathering_state(state);
            }
            InternalEvent::UpdateIceConnectionState => {
                use gst_webrtc::WebRTCICEConnectionState::*;
                let prop = self.webrtc.get_property("ice-connection-state")?;
                let val = prop
                    .downcast::<gst_webrtc::WebRTCICEConnectionState>()
                    .map_err(|_| "unable to downcast ICE connection state")?
                    .get_some();
                let state = match val {
                    New => IceConnectionState::New,
                    Checking => IceConnectionState::Checking,
                    Connected => IceConnectionState::Connected,
                    Completed => IceConnectionState::Completed,
                    Disconnected => IceConnectionState::Disconnected,
                    Failed => IceConnectionState::Failed,
                    Closed => IceConnectionState::Closed,
                    i => {
                        return Err(WebrtcError::Backend(format!(
                            "unknown ICE connection state: {:?}",
                            i
                        )))
                    }
                };
                self.signaller.update_ice_connection_state(state);
            }
        }
        Ok(())
    }

    fn quit(&mut self) {
        self.signaller.close();

        self.pipeline.set_state(gst::State::Null).unwrap();

        //main_loop.quit();
    }
}

impl GStreamerWebRtcController {
    fn set_description(
        &mut self,
        desc: SessionDescription,
        description_type: DescriptionType,
        cb: SendBoxFnOnce<'static, ()>,
    ) -> WebrtcResult {
        let ty = match desc.type_ {
            SdpType::Answer => gst_webrtc::WebRTCSDPType::Answer,
            SdpType::Offer => gst_webrtc::WebRTCSDPType::Offer,
            SdpType::Pranswer => gst_webrtc::WebRTCSDPType::Pranswer,
            SdpType::Rollback => gst_webrtc::WebRTCSDPType::Rollback,
        };

        let kind = match description_type {
            DescriptionType::Local => "set-local-description",
            DescriptionType::Remote => "set-remote-description",
        };

        let sdp = gst_sdp::SDPMessage::parse_buffer(desc.sdp.as_bytes()).unwrap();
        if description_type == DescriptionType::Remote {
            self.remote_offer_generation += 1;
            self.store_remote_mline_info(&sdp);
        }
        let answer = gst_webrtc::WebRTCSessionDescription::new(ty, sdp);
        let thread = self.thread.clone();
        let remote_offer_generation = self.remote_offer_generation;
        let promise = gst::Promise::new_with_change_func(move |_promise| {
            // remote_offer_generation here ensures that DescriptionAdded doesn't
            // flush pending_remote_mline_info for stale remote offer callbacks
            thread.internal_event(InternalEvent::DescriptionAdded(
                cb,
                description_type,
                desc.type_,
                remote_offer_generation,
            ));
        });
        self.webrtc.emit(kind, &[&answer, &promise])?;
        Ok(())
    }

    fn store_remote_mline_info(&mut self, sdp: &gst_sdp::SDPMessage) {
        // remove after https://gitlab.freedesktop.org/gstreamer/gstreamer-rs/issues/189 is fixed
        fn get_media(msg: &gst_sdp::SDPMessage, idx: u32) -> Option<gst_sdp::SDPMedia> {
            extern crate gstreamer_sdp_sys as gst_sdp_sys;
            use glib::translate::*;
            unsafe {
                from_glib_none(gst_sdp_sys::gst_sdp_message_get_media(
                    msg.to_glib_none().0,
                    idx,
                ))
            }
        }
        self.pending_remote_mline_info.clear();
        for i in 0..sdp.medias_len() {
            let mut caps = gst::Caps::new_empty();
            let caps_mut = caps.get_mut().expect("Fresh caps should be uniquely owned");
            let media = get_media(&sdp, i).expect("Gstreamer reported incorrect medias_len()");
            for format in 0..media.formats_len() {
                let pt = media
                    .get_format(format)
                    .expect("Gstreamer reported incorrect formats_len()")
                    .parse()
                    .expect("Gstreamer provided noninteger format");
                caps_mut.append(
                    media
                        .get_caps_from_media(pt)
                        .expect("get_format() did not return a format from the SDP"),
                );
                self.pt_counter = cmp::max(self.pt_counter, pt + 1);
            }
            for cap in 0..caps_mut.get_size() {
                // the caps are application/x-unknown by default, which will fail
                // to intersect
                //
                // see https://gitlab.freedesktop.org/gstreamer/gst-plugins-bad/blob/ba62917fbfd98ea76d4e066a6f18b4a14b847362/ext/webrtc/gstwebrtcbin.c#L2521
                caps_mut
                    .get_mut_structure(cap)
                    .expect("Gstreamer reported incorrect get_size()")
                    .set_name("application/x-rtp")
            }
            // This info is not current until the promise from set-remote-description is resolved,
            // to avoid any races where we attempt to link streams before the promise resolves we
            // queue this up in a pending buffer
            self.pending_remote_mline_info.push(MLineInfo {
                caps: caps,
                // XXXManishearth in the (yet unsupported) case of dynamic stream addition and renegotiation
                // this will need to be checked against the current set of streams
                is_used: false,
                // XXXManishearth ideally, we keep track of all payloads and have the capability of picking
                // the appropriate decoder. For this, a bunch of the streams code will have to be moved into
                // a webrtc-specific abstraction.
                payload: media
                    .get_format(0)
                    .expect("Gstreamer reported incorrect formats_len()")
                    .parse()
                    .expect("Gstreamer provided noninteger format"),
            });
        }
    }

    /// Streams need to be linked to the correct pads, so we buffer them up until we know enough
    /// to do this.
    ///
    /// When we get a remote offer, we store the relevant m-line information so that we can
    /// pick the correct sink pad and payload. Shortly after we look for any pending streams
    /// and connect them to available compatible m-lines using link_stream.
    ///
    /// When we create an offer, we're controlling the pad order, so we set request_new_pads
    /// to true and forcefully link all pending streams before generating the offer.
    ///
    /// When request_new_pads is false, we may still request new pads, however we only do this for
    /// streams that have already been negotiated by the remote.
    fn link_stream(
        &mut self,
        stream_id: &MediaStreamId,
        stream: &mut GStreamerMediaStream,
        request_new_pads: bool,
    ) -> WebrtcResult {
        let caps = stream.caps();
        let idx = self
            .remote_mline_info
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.is_used)
            .find(|(_, x)| x.caps.can_intersect(&caps))
            .map(|x| x.0);
        let element = stream.src_element();

        if let Some(idx) = idx {
            if idx >= self.request_pad_counter {
                for i in self.request_pad_counter..=idx {
                    // webrtcbin needs you to request pads (or use element.link(webrtcbin))
                    // however, it also wants them to be connected in the correct order.
                    //
                    // Here, we make sure all the numbered sink pads have been created beforehand, up to
                    // and including the one we need here.
                    //
                    // An alternate fix is to sort pending_streams according to the m-line index
                    // and just do it in order. This also seems brittle.
                    self.webrtc
                        .get_request_pad(&format!("sink_{}", i))
                        .ok_or("Cannot request sink pad")?;
                }
                self.request_pad_counter = idx + 1;
            }
            stream.attach_to_pipeline(&self.pipeline);
            self.remote_mline_info[idx].is_used = true;
            let caps = stream.caps_with_payload(self.remote_mline_info[idx].payload);
            element.set_property("caps", &caps)?;
            let src = element
                .get_static_pad("src")
                .ok_or("Cannot request src pad")?;
            let sink = self
                .webrtc
                .get_static_pad(&format!("sink_{}", idx))
                .ok_or("Cannot request sink pad")?;
            src.link(&sink)?;
            self.streams.push(stream_id.clone());
        } else if request_new_pads {
            stream.attach_to_pipeline(&self.pipeline);
            let caps = stream.caps_with_payload(self.pt_counter);
            self.pt_counter += 1;
            element.set_property("caps", &caps)?;
            let src = element
                .get_static_pad("src")
                .ok_or("Cannot request src pad")?;
            let sink = self
                .webrtc
                .get_request_pad(&format!("sink_{}", self.request_pad_counter))
                .ok_or("Cannot request sink pad")?;
            self.request_pad_counter += 1;
            src.link(&sink)?;
            self.streams.push(stream_id.clone());
        } else {
            self.pending_streams.push(stream_id.clone());
        }
        Ok(())
    }

    /// link_stream, but for all pending streams
    fn flush_pending_streams(&mut self, request_new_pads: bool) -> WebrtcResult {
        let pending_streams = mem::replace(&mut self.pending_streams, vec![]);
        for stream_id in pending_streams {
            let stream =
                get_stream(&stream_id).expect("Media streams registry does not contain such ID");
            let mut stream = stream.lock().unwrap();
            let mut stream = stream
                .as_mut_any()
                .downcast_mut::<GStreamerMediaStream>()
                .ok_or("Does not currently support non-gstreamer streams")?;
            self.link_stream(&stream_id, &mut stream, request_new_pads)?;
        }
        Ok(())
    }
}

impl GStreamerWebRtcController {
    fn start_pipeline(&mut self) -> WebrtcResult {
        self.pipeline.add(&self.webrtc)?;

        // gstreamer needs Sync on these callbacks for some reason
        // https://github.com/sdroege/gstreamer-rs/issues/154
        let thread = Mutex::new(self.thread.clone());
        self.webrtc
            .connect("on-ice-candidate", false, move |values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::OnIceCandidate(candidate(values)));
                None
            })?;

        let pipe_clone = self.pipeline.clone();
        let thread = Arc::new(Mutex::new(self.thread.clone()));
        self.webrtc.connect("pad-added", false, move |values| {
            process_new_stream(values, &pipe_clone, thread.clone());
            None
        })?;

        // gstreamer needs Sync on these callbacks for some reason
        // https://github.com/sdroege/gstreamer-rs/issues/154
        let thread = Mutex::new(self.thread.clone());
        self.webrtc
            .connect("on-negotiation-needed", false, move |_values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::OnNegotiationNeeded);
                None
            })?;

        let thread = Mutex::new(self.thread.clone());
        self.webrtc
            .connect("notify::signaling-state", false, move |_values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::UpdateSignalingState);
                None
            })?;
        let thread = Mutex::new(self.thread.clone());
        self.webrtc
            .connect("notify::ice-connection-state", false, move |_values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::UpdateIceConnectionState);
                None
            })?;
        let thread = Mutex::new(self.thread.clone());
        self.webrtc
            .connect("notify::ice-gathering-state", false, move |_values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::UpdateGatheringState);
                None
            })?;
        self.pipeline.set_state(gst::State::Ready)?;
        Ok(())
    }
}

pub fn construct(
    signaller: Box<WebRtcSignaller>,
    thread: WebRtcThread,
) -> Result<GStreamerWebRtcController, WebrtcError> {
    let main_loop = glib::MainLoop::new(None, false);
    let pipeline = gst::Pipeline::new("webrtc main");
    pipeline.set_start_time(gst::ClockTime::none());
    pipeline.set_base_time(*BACKEND_BASE_TIME);
    pipeline.use_clock(Some(&gst::SystemClock::obtain()));
    let webrtc =
        gst::ElementFactory::make("webrtcbin", "sendrecv").ok_or("webrtcbin element not found")?;
    let mut controller = GStreamerWebRtcController {
        webrtc,
        pipeline,
        signaller,
        thread,
        remote_mline_info: vec![],
        pending_remote_mline_info: vec![],
        streams: vec![],
        pending_streams: vec![],
        pt_counter: 96,
        request_pad_counter: 0,
        remote_offer_generation: 0,
        delayed_negotiation: false,
        _main_loop: main_loop,
    };
    controller.start_pipeline()?;
    Ok(controller)
}

fn on_offer_or_answer_created(
    ty: SdpType,
    promise: &gst::Promise,
    cb: SendBoxFnOnce<'static, (SessionDescription,)>,
) {
    debug_assert!(ty == SdpType::Offer || ty == SdpType::Answer);

    let reply = promise.get_reply().unwrap();

    let reply = reply
        .get_value(ty.as_str())
        .unwrap()
        .get::<gst_webrtc::WebRTCSessionDescription>()
        .expect("Invalid argument");

    let type_ = match reply.get_type() {
        gst_webrtc::WebRTCSDPType::Answer => SdpType::Answer,
        gst_webrtc::WebRTCSDPType::Offer => SdpType::Offer,
        gst_webrtc::WebRTCSDPType::Pranswer => SdpType::Pranswer,
        gst_webrtc::WebRTCSDPType::Rollback => SdpType::Rollback,
        _ => panic!("unknown sdp response"),
    };

    let desc = SessionDescription {
        sdp: reply.get_sdp().as_text().unwrap(),
        type_,
    };

    cb.call(desc);
}
fn on_incoming_stream(pipe: &gst::Pipeline, thread: Arc<Mutex<WebRtcThread>>, pad: &gst::Pad) {
    let decodebin = gst::ElementFactory::make("decodebin", None).unwrap();
    let pipe_clone = pipe.clone();
    let caps = pad.query_caps(None).unwrap();
    let name = caps
        .get_structure(0)
        .unwrap()
        .get::<String>("media")
        .unwrap();
    let decodebin2 = decodebin.clone();
    decodebin
        .connect("pad-added", false, move |values| {
            on_incoming_decodebin_stream(values, &pipe_clone, thread.clone(), &name);
            None
        })
        .unwrap();
    pipe.add(&decodebin).unwrap();

    let decodepad = decodebin.get_static_pad("sink").unwrap();
    pad.link(&decodepad).unwrap();
    decodebin2.sync_state_with_parent().unwrap();
}

fn on_incoming_decodebin_stream(
    values: &[glib::Value],
    pipe: &gst::Pipeline,
    thread: Arc<Mutex<WebRtcThread>>,
    name: &str,
) {
    let pad = values[1].get::<gst::Pad>().expect("not a pad??");
    let proxy_src = gst::ElementFactory::make("proxysrc", None).unwrap();
    let proxy_sink = gst::ElementFactory::make("proxysink", None).unwrap();
    proxy_src.set_property("proxysink", &proxy_sink).unwrap();
    pipe.add(&proxy_sink).unwrap();
    let sinkpad = proxy_sink.get_static_pad("sink").unwrap();

    pad.link(&sinkpad).unwrap();
    proxy_sink.sync_state_with_parent().unwrap();

    let (stream, ty) = if name == "video" {
        (
            GStreamerMediaStream::create_video_from(proxy_src),
            MediaStreamType::Video,
        )
    } else {
        (
            GStreamerMediaStream::create_audio_from(proxy_src),
            MediaStreamType::Audio,
        )
    };
    thread
        .lock()
        .unwrap()
        .internal_event(InternalEvent::OnAddStream(stream, ty));
}

fn process_new_stream(
    values: &[glib::Value],
    pipe: &gst::Pipeline,
    thread: Arc<Mutex<WebRtcThread>>,
) {
    let pad = values[1].get::<gst::Pad>().expect("not a pad??");
    if pad.get_direction() != gst::PadDirection::Src {
        // Ignore outgoing pad notifications.
        return;
    }
    on_incoming_stream(pipe, thread, &pad)
}

fn candidate(values: &[glib::Value]) -> IceCandidate {
    let _webrtc = values[0].get::<gst::Element>().expect("Invalid argument");
    let sdp_mline_index = values[1].get::<u32>().expect("Invalid argument");
    let candidate = values[2].get::<String>().expect("Invalid argument");

    IceCandidate {
        sdp_mline_index,
        candidate,
    }
}
