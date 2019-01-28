use boxfnonce::SendBoxFnOnce;
use glib;
use glib::prelude::*;
use gst;
use gst::prelude::*;
use gst_sdp;
use gst_webrtc;
use media_stream::{GStreamerMediaStream, StreamType};
use servo_media_webrtc::thread::InternalEvent;
use servo_media_webrtc::WebRtcController as WebRtcThread;
use servo_media_webrtc::*;
use std::error::Error;
use std::sync::{Arc, Mutex};

// TODO:
// - add a proper error enum
// - figure out purpose of glib loop


pub struct GStreamerWebRtcController {
    webrtc: Option<gst::Element>,
    pipeline: gst::Pipeline,
    thread: WebRtcThread,
    signaller: Box<WebRtcSignaller>,
    ready_to_negotiate: bool,
    //send_msg_tx: mpsc::Sender<OwnedMessage>,
    //peer_id: String,
    _main_loop: glib::MainLoop,
    //bus: gst::Bus,
}

impl WebRtcControllerBackend for GStreamerWebRtcController {
    fn add_ice_candidate(&mut self, candidate: IceCandidate) {
        self.webrtc
            .as_ref()
            .unwrap()
            .emit(
                "add-ice-candidate",
                &[&candidate.sdp_mline_index, &candidate.candidate],
            )
            .unwrap();
    }

    fn set_remote_description(&mut self, desc: SessionDescription, cb: SendBoxFnOnce<'static, ()>) {

        self.set_description(desc, false, cb);
    }

    fn set_local_description(&mut self, desc: SessionDescription, cb: SendBoxFnOnce<'static, ()>) {

        self.set_description(desc, true, cb);

    }

    fn create_offer(&mut self, cb: SendBoxFnOnce<'static, (SessionDescription,)>) {
        let webrtc = self.webrtc.as_ref().unwrap();

        let promise = gst::Promise::new_with_change_func(move |promise| {
            on_offer_or_answer_created(SdpType::Offer, promise, cb);
        });

        webrtc
            .emit("create-offer", &[&None::<gst::Structure>, &promise])
            .unwrap();
    }

    fn create_answer(&mut self, cb: SendBoxFnOnce<'static, (SessionDescription,)>) {
        let webrtc = self.webrtc.as_ref().unwrap();

        let promise = gst::Promise::new_with_change_func(move |promise| {
            on_offer_or_answer_created(SdpType::Answer, promise, cb);
        });

        webrtc
            .emit("create-answer", &[&None::<gst::Structure>, &promise])
            .unwrap();
    }

    fn add_stream(&mut self, stream: &mut MediaStream) {
        println!("adding a stream");
        let stream = stream
            .as_mut_any()
            .downcast_mut::<GStreamerMediaStream>()
            .unwrap();
        stream.attach_to_pipeline(&self.pipeline, self.webrtc.as_ref().unwrap());
        self.prepare_for_negotiation();
    }

    fn configure(&mut self, stun_server: &str, policy: BundlePolicy) {
        let webrtc = self.webrtc.as_ref().unwrap();
        webrtc.set_property_from_str("stun-server", stun_server);
        webrtc.set_property_from_str("bundle-policy", policy.as_str());
    }

    fn internal_event(&mut self, e: thread::InternalEvent) {
        match e {
            InternalEvent::OnNegotiationNeeded => {
                self.signaller.on_negotiation_needed(&self.thread);
            }
            InternalEvent::OnIceCandidate(candidate) => {
                self.signaller.on_ice_candidate(&self.thread, candidate);
            }
            InternalEvent::OnAddStream(stream) => {
                self.signaller.on_add_stream(stream);
            }
        }
    }

    fn quit(&mut self) {
        self.signaller.close();

        self.pipeline.set_state(gst::State::Null).unwrap();

        //main_loop.quit();
    }
}

impl GStreamerWebRtcController {
    fn set_description(
        &self,
        desc: SessionDescription,
        local: bool,
        cb: SendBoxFnOnce<'static, ()>,
    ) {
        let ty = match desc.type_ {
            SdpType::Answer => gst_webrtc::WebRTCSDPType::Answer,
            SdpType::Offer => gst_webrtc::WebRTCSDPType::Offer,
            SdpType::Pranswer => gst_webrtc::WebRTCSDPType::Pranswer,
            SdpType::Rollback => gst_webrtc::WebRTCSDPType::Rollback,
        };

        let kind = if local {
            "set-local-description"
        } else {
            "set-remote-description"
        };

        let ret = gst_sdp::SDPMessage::parse_buffer(desc.sdp.as_bytes()).unwrap();
        let answer = gst_webrtc::WebRTCSessionDescription::new(ty, ret);
        let promise = gst::Promise::new_with_change_func(move |_promise| cb.call());
        self.webrtc
            .as_ref()
            .unwrap()
            .emit(kind, &[&answer, &promise])
            .unwrap();
    }
}

impl GStreamerWebRtcController {
    fn prepare_for_negotiation(&mut self) {
        if self.ready_to_negotiate {
            return;
        }
        self.ready_to_negotiate = true;
        let webrtc = self.webrtc.as_ref().unwrap();
        // gstreamer needs Sync on these callbacks for some reason
        // https://github.com/sdroege/gstreamer-rs/issues/154
        let thread = Mutex::new(self.thread.clone());
        // If the pipeline starts playing and this signal is present before there are any
        // media streams, an invalid SDP offer will be created. Therefore, delay setting up
        // the signal and starting the pipeline until after the first stream has been added.
        webrtc
            .connect("on-negotiation-needed", false, move |_values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::OnNegotiationNeeded);
                None
            })
            .unwrap();
        self.pipeline.set_state(gst::State::Playing).unwrap();
    }

    fn start_pipeline(&mut self) {
        let webrtc = gst::ElementFactory::make("webrtcbin", "sendrecv").unwrap();
        self.pipeline.add(&webrtc).unwrap();

        // gstreamer needs Sync on these callbacks for some reason
        // https://github.com/sdroege/gstreamer-rs/issues/154
        let thread = Mutex::new(self.thread.clone());
        webrtc
            .connect("on-ice-candidate", false, move |values| {
                thread
                    .lock()
                    .unwrap()
                    .internal_event(InternalEvent::OnIceCandidate(candidate(values)));
                None
            })
            .unwrap();

        let pipe_clone = self.pipeline.clone();
        let thread = Arc::new(Mutex::new(self.thread.clone()));
        webrtc
            .connect("pad-added", false, move |values| {
                println!("pad-added");
                process_new_stream(values, &pipe_clone, thread.clone());
                None
            })
            .unwrap();

        self.webrtc = Some(webrtc);
    }
}

pub fn construct(
    signaller: Box<WebRtcSignaller>,
    thread: WebRtcThread,
) -> GStreamerWebRtcController {
    let main_loop = glib::MainLoop::new(None, false);
    let pipeline = gst::Pipeline::new("main");

    let mut controller = GStreamerWebRtcController {
        webrtc: None,
        pipeline,
        signaller,
        thread,
        ready_to_negotiate: false,
        _main_loop: main_loop,
    };
    controller.start_pipeline();
    controller
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

fn handle_media_stream(
    pad: &gst::Pad,
    pipe: &gst::Pipeline,
    media_type: StreamType,
    thread: Arc<Mutex<WebRtcThread>>,
) -> Result<(), Box<Error>> {
    println!("Trying to handle stream {:?}", media_type);

    let (q, conv, elements) = match media_type {
        StreamType::Audio => {
            let q = gst::ElementFactory::make("queue", None).unwrap();
            let conv = gst::ElementFactory::make("audioconvert", None).unwrap();
            let resample = gst::ElementFactory::make("audioresample", None).unwrap();

            pipe.add_many(&[&q, &conv, &resample])?;
            gst::Element::link_many(&[&q, &conv, &resample])?;

            resample.sync_state_with_parent()?;

            let elements = vec![q.clone(), conv.clone(), resample];
            (q, conv, elements)
        }
        StreamType::Video => {
            let q = gst::ElementFactory::make("queue", None).unwrap();
            let conv = gst::ElementFactory::make("videoconvert", None).unwrap();

            pipe.add_many(&[&q, &conv])?;
            gst::Element::link_many(&[&q, &conv])?;

            let elements = vec![q.clone(), conv.clone()];
            (q, conv, elements)
        }
    };
    q.sync_state_with_parent()?;
    conv.sync_state_with_parent()?;

    let qpad = q.get_static_pad("sink").unwrap();
    pad.link(&qpad)?;

    let stream = Box::new(GStreamerMediaStream::create_stream_with_pipeline(
        media_type,
        elements,
        pipe.clone(),
    ));
    thread
        .lock()
        .unwrap()
        .internal_event(InternalEvent::OnAddStream(stream));

    Ok(())
}

fn on_incoming_decodebin_stream(
    values: &[glib::Value],
    pipe: &gst::Pipeline,
    thread: Arc<Mutex<WebRtcThread>>,
) -> Option<glib::Value> {
    let pad = values[1].get::<gst::Pad>().expect("Invalid argument");
    if !pad.has_current_caps() {
        println!("Pad {:?} has no caps, can't do anything, ignoring", pad);
        return None;
    }

    let caps = pad.get_current_caps().unwrap();
    let name = caps.get_structure(0).unwrap().get_name();

    let handled = if name.starts_with("video") {
        handle_media_stream(&pad, &pipe, StreamType::Video, thread)
    } else if name.starts_with("audio") {
        handle_media_stream(&pad, &pipe, StreamType::Audio, thread)
    } else {
        println!("Unknown pad {:?}, ignoring", pad);
        Ok(())
    };

    if let Err(err) = handled {
        eprintln!("Error adding pad with caps {} {:?}", name, err);
    }

    None
}

fn on_incoming_stream(
    values: &[glib::Value],
    pipe: &gst::Pipeline,
    thread: Arc<Mutex<WebRtcThread>>,
) -> Option<glib::Value> {
    let webrtc = values[0].get::<gst::Element>().expect("Invalid argument");

    let decodebin = gst::ElementFactory::make("decodebin", None).unwrap();
    let pipe_clone = pipe.clone();
    decodebin
        .connect("pad-added", false, move |values| {
            println!("decodebin pad-added");
            on_incoming_decodebin_stream(values, &pipe_clone, thread.clone())
        })
        .unwrap();

    pipe.add(&decodebin).unwrap();

    decodebin.sync_state_with_parent().unwrap();
    webrtc.link(&decodebin).unwrap();

    None
}

fn process_new_stream(
    values: &[glib::Value],
    pipe: &gst::Pipeline,
    thread: Arc<Mutex<WebRtcThread>>,
) -> Option<glib::Value> {
    let pad = values[1].get::<gst::Pad>().expect("not a pad??");
    if pad.get_direction() != gst::PadDirection::Src {
        // Ignore outgoing pad notifications.
        return None;
    }
    on_incoming_stream(values, pipe, thread)
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
