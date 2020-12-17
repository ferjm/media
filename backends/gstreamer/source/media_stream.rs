use glib;
use glib::subclass;
use glib::subclass::prelude::*;
use glib::translate::*;
use gst;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::UniqueFlowCombiner;
use media_stream::{GStreamerMediaStream, RTP_CAPS_OPUS, RTP_CAPS_VP8};
use servo_media_streams::{MediaStream, MediaStreamType};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use url::Url;

// Implementation sub-module of the GObject
mod imp {
    use super::*;

    lazy_static! {
        static ref AUDIO_SRC_PAD_TEMPLATE: gst::PadTemplate = {
            gst::PadTemplate::new(
                "audio_src",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &RTP_CAPS_OPUS,
            )
            .expect("Could not create audio src pad template")
        };
        static ref VIDEO_SRC_PAD_TEMPLATE: gst::PadTemplate = {
            gst::PadTemplate::new(
                "video_src",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &RTP_CAPS_VP8,
            )
            .expect("Could not create video src pad template")
        };
    }

    pub struct ServoMediaStreamSrc {
        cat: gst::DebugCategory,
        audio_proxysrc: gst::Element,
        audio_srcpad: gst::GhostPad,
        video_proxysrc: gst::Element,
        video_srcpad: gst::GhostPad,
        flow_combiner: Arc<Mutex<UniqueFlowCombiner>>,
        has_audio_stream: Arc<AtomicBool>,
        has_video_stream: Arc<AtomicBool>,
    }

    impl ServoMediaStreamSrc {
        pub fn set_stream(
            &self,
            stream: &mut GStreamerMediaStream,
            src: &gst::Element,
            only_stream: bool,
        ) {
            // XXXferjm the current design limits the number of streams to one
            // per type. This fulfills the basic use case for WebRTC, but we should
            // implement support for multiple streams per type at some point, which
            // likely involves encoding and muxing all streams of the same type
            // in a single stream.

            gst_log!(self.cat, "Setting stream");

            // Append a proxysink to the media stream pipeline.
            let pipeline = stream.pipeline_or_new();
            let last_element = stream.encoded();
            let sink = gst::ElementFactory::make("proxysink", None).unwrap();
            pipeline.add(&sink).unwrap();
            gst::Element::link_many(&[&last_element, &sink][..]).unwrap();

            // Create the appropriate proxysrc depending on the stream type
            // and connect the media stream proxysink to it.
            self.setup_proxy_src(stream.ty(), &sink, src, only_stream);

            sink.sync_state_with_parent().unwrap();

            pipeline.set_state(gst::State::Playing).unwrap();
        }

        fn setup_proxy_src(
            &self,
            stream_type: MediaStreamType,
            sink: &gst::Element,
            src: &gst::Element,
            only_stream: bool,
        ) {
            let (proxysrc, src_pad, no_more_pads) = match stream_type {
                MediaStreamType::Audio => {
                    self.has_audio_stream.store(true, Ordering::Relaxed);
                    (
                        &self.audio_proxysrc,
                        &self.audio_srcpad,
                        self.has_video_stream.load(Ordering::Relaxed),
                    )
                }
                MediaStreamType::Video => {
                    self.has_video_stream.store(true, Ordering::Relaxed);
                    (
                        &self.video_proxysrc,
                        &self.video_srcpad,
                        self.has_audio_stream.load(Ordering::Relaxed),
                    )
                }
            };
            proxysrc
                .set_property("proxysink", &sink)
                .expect("Could not set proxysink property on proxysrc");

            // Add proxysrc to bin
            let bin = src.downcast_ref::<gst::Bin>().unwrap();
            bin.add(proxysrc)
                .expect("Could not add proxysrc element to bin");

            let target_pad = proxysrc
                .get_static_pad("src")
                .expect("Could not get proxysrc's static src pad");
            src_pad
                .set_target(Some(&target_pad))
                .expect("Could not set target pad");

            src.add_pad(src_pad)
                .expect("Could not add source pad to media stream src");
            ::set_element_flags(src, gst::ElementFlags::SOURCE);

            let proxy_pad = src_pad.get_internal().unwrap();
            src_pad.set_active(true).expect("Could not active pad");
            self.flow_combiner.lock().unwrap().add_pad(&proxy_pad);
            let combiner = self.flow_combiner.clone();
            proxy_pad.set_chain_function(move |pad, parent, buffer| {
                let chain_result = pad.proxy_pad_chain_default(parent, buffer);
                let result = combiner.lock().unwrap().update_pad_flow(pad, chain_result);
                if result == Err(gst::FlowError::Flushing) {
                    return chain_result;
                }
                result
            });

            src.sync_state_with_parent().unwrap();

            if no_more_pads || only_stream {
                src.no_more_pads();
            }
        }
    }

    // Basic declaration of our type for the GObject type system.
    impl ObjectSubclass for ServoMediaStreamSrc {
        const NAME: &'static str = "ServoMediaStreamSrc";
        type ParentType = gst::Bin;
        type Instance = gst::subclass::ElementInstanceStruct<Self>;
        type Class = subclass::simple::ClassStruct<Self>;

        glib_object_subclass!();

        // Called once at the very beginning of instantiation of each instance and
        // creates the data structure that contains all our state
        fn new_with_class(_: &subclass::simple::ClassStruct<Self>) -> Self {
            let audio_proxysrc = gst::ElementFactory::make("proxysrc", None)
                .expect("Could not create proxysrc element");
            let audio_srcpad = gst::GhostPad::new_no_target_from_template(
                Some("audio_src"),
                &AUDIO_SRC_PAD_TEMPLATE,
            )
            .unwrap();

            let video_proxysrc = gst::ElementFactory::make("proxysrc", None)
                .expect("Could not create proxysrc element");
            let video_srcpad = gst::GhostPad::new_no_target_from_template(
                Some("video_src"),
                &VIDEO_SRC_PAD_TEMPLATE,
            )
            .unwrap();

            Self {
                cat: gst::DebugCategory::new(
                    "servomediastreamsrc",
                    gst::DebugColorFlags::empty(),
                    Some("Servo media stream source"),
                ),
                audio_proxysrc,
                audio_srcpad,
                video_proxysrc,
                video_srcpad,
                flow_combiner: Arc::new(Mutex::new(UniqueFlowCombiner::new())),
                has_video_stream: Arc::new(AtomicBool::new(false)),
                has_audio_stream: Arc::new(AtomicBool::new(false)),
            }
        }

        // Adds interface implementations in the class
        fn type_init(type_: &mut subclass::InitializingType<Self>) {
            type_.add_interface::<gst::URIHandler>();
        }

        // Called exactly once before the first instantiation of an instance. This
        // sets up any type-specific things, in this specific case it installs the
        // properties so that GObject knows about their existence and they can be
        // used on instances of our type
        fn class_init(klass: &mut subclass::simple::ClassStruct<Self>) {
            klass.set_metadata(
                "Servo Media Stream Source",
                "Source/Audio/Video",
                "Feed player with media stream data",
                "Servo developers",
            );

            // Let playbin3 know we are a live source.
            klass.install_properties(&[subclass::Property("is-live", |name| {
                glib::ParamSpec::boolean(
                    name,
                    "Is Live",
                    "Let playbin3 know we are a live source",
                    true,
                    glib::ParamFlags::READWRITE,
                )
            })]);

            // Add pad templates for our audio and video source pads.
            // These are later used for actually creating the pads and beforehand
            // already provide information to GStreamer about all possible
            // pads that could exist for this type.
            klass.add_pad_template(AUDIO_SRC_PAD_TEMPLATE.clone());
            klass.add_pad_template(VIDEO_SRC_PAD_TEMPLATE.clone());
        }
    }

    // The ObjectImpl trait provides the setters/getters for GObject properties.
    // Here we need to provide the values that are internally stored back to the
    // caller, or store whatever new value the caller is providing.
    //
    // This maps between the GObject properties and our internal storage of the
    // corresponding values of the properties.
    impl ObjectImpl for ServoMediaStreamSrc {
        glib_object_impl!();

        fn get_property(&self, _: &glib::Object, id: usize) -> Result<glib::Value, ()> {
            // We have a single property: is-live
            if id != 0 {
                return Err(());
            }
            Ok(true.to_value())
        }
    }

    // Implementation of gst::Element virtual methods
    impl ElementImpl for ServoMediaStreamSrc {}

    // Implementation of gst::Bin virtual methods
    impl BinImpl for ServoMediaStreamSrc {}

    impl URIHandlerImpl for ServoMediaStreamSrc {
        fn get_uri(&self, _element: &gst::URIHandler) -> Option<String> {
            Some("mediastream://".to_string())
        }

        fn set_uri(&self, _element: &gst::URIHandler, uri: &str) -> Result<(), glib::Error> {
            if let Ok(uri) = Url::parse(uri) {
                if uri.scheme() == "mediastream" {
                    return Ok(());
                }
            }
            Err(glib::Error::new(
                gst::URIError::BadUri,
                format!("Invalid URI '{:?}'", uri,).as_str(),
            ))
        }

        fn get_uri_type() -> gst::URIType {
            gst::URIType::Src
        }

        fn get_protocols() -> Vec<String> {
            vec!["mediastream".into()]
        }
    }
}

// Public part of the ServoMediaStreamSrc type. This behaves like a normal
// GObject binding
glib_wrapper! {
    pub struct ServoMediaStreamSrc(Object<gst::subclass::ElementInstanceStruct<imp::ServoMediaStreamSrc>,
                                   subclass::simple::ClassStruct<imp::ServoMediaStreamSrc>, ServoMediaStreamSrcClass>)
        @extends gst::Bin, gst::Element, gst::Object, @implements gst::URIHandler;

    match fn {
        get_type => || imp::ServoMediaStreamSrc::get_type().to_glib(),
    }
}

unsafe impl Send for ServoMediaStreamSrc {}
unsafe impl Sync for ServoMediaStreamSrc {}

impl ServoMediaStreamSrc {
    pub fn set_stream(&self, stream: &mut GStreamerMediaStream, only_stream: bool) {
        imp::ServoMediaStreamSrc::from_instance(self).set_stream(
            stream,
            self.upcast_ref::<gst::Element>(),
            only_stream,
        )
    }
}

// Registers the type for our element, and then registers in GStreamer
// under the name "servomediastreamsrc" for being able to instantiate it via e.g.
// gst::ElementFactory::make().
pub fn register_servo_media_stream_src() -> Result<(), glib::BoolError> {
    gst::Element::register(
        None,
        "servomediastreamsrc",
        gst::Rank::None,
        ServoMediaStreamSrc::static_type(),
    )
}
