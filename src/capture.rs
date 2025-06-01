use std::{
    sync::{Arc, Mutex},
};

use anyhow::Result;
use libspa::{self as spa, pod::Pod};
use pipewire::{self as pw, properties::properties};

struct UserData {
    format: spa::param::video::VideoInfoRaw,
    on_frame: Box<dyn FnMut(spa::param::video::VideoInfoRaw, pw::buffer::Buffer)>,
}

#[allow(dead_code)]
pub struct Capturer {
    main_loop: pw::main_loop::MainLoop,
    context: pw::context::Context,
    core: pw::core::Core,
    stream: pw::stream::Stream,
    user_data: Arc<Mutex<UserData>>,
    listener: pw::stream::StreamListener<Arc<Mutex<UserData>>>,
}

impl Capturer {
    pub fn new<F>(on_frame: F) -> Result<Self>
    where
        F: FnMut(spa::param::video::VideoInfoRaw, pw::buffer::Buffer) + 'static,
    {
        let main_loop = pw::main_loop::MainLoop::new(None)?;
        let context = pw::context::Context::new(&main_loop)?;
        let core = context.connect(None)?;

        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "gamescope",
        };

        let stream = pw::stream::Stream::new(&core, "zeroscope", props)?;

        let user_data = Arc::new(Mutex::new(UserData {
            format: Default::default(),
            on_frame: Box::new(on_frame),
        }));

        let listener = stream
            .add_local_listener_with_user_data(user_data.clone())
            .state_changed(|_, _, old_state, new_state| {
                println!("State changed: {:?} -> {:?}", old_state, new_state);
            })
            .param_changed(|_, user_data, id, param| {
                println!("Param changed: id = {}", id);
                let Some(param) = param else {
                    return;
                };
                if id != pw::spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let (media_type, media_subtype) =
                    match pw::spa::param::format_utils::parse_format(param) {
                        Ok(v) => v,
                        Err(_) => return,
                    };

                if media_type != pw::spa::param::format::MediaType::Video
                    || media_subtype != pw::spa::param::format::MediaSubtype::Raw
                {
                    return;
                }

                let mut user_data = user_data.lock().unwrap();
                user_data
                    .format
                    .parse(param)
                    .expect("Failed to parse format");
                println!("got video format:");
                println!(
                    "  format: {} ({:?})",
                    user_data.format.format().as_raw(),
                    user_data.format.format()
                );
                println!(
                    "  size: {}x{}",
                    user_data.format.size().width,
                    user_data.format.size().height
                );
                println!(
                    "  framerate: {}/{}",
                    user_data.format.framerate().num,
                    user_data.format.framerate().denom
                );
            })
            .process(|stream, user_data| match stream.dequeue_buffer() {
                None => println!("out of buffers"),
                Some(buffer) => {
                    let mut user_data = user_data.lock().unwrap();
                    let format = user_data.format.clone();
                    (user_data.on_frame)(format, buffer);
                }
            })
            .register()?;

        println!("Created stream {:#?}", stream);

        // FIXME: use 2 params, with second as shm fallback
        let obj = pw::spa::pod::object!(
            pw::spa::utils::SpaTypes::ObjectParamFormat,
            pw::spa::param::ParamType::EnumFormat,
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::MediaType,
                Id,
                pw::spa::param::format::MediaType::Video
            ),
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::MediaSubtype,
                Id,
                pw::spa::param::format::MediaSubtype::Raw
            ),
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoFormat,
                Id,
                pw::spa::param::video::VideoFormat::NV12
            ),
            // FIXME: modifier should have SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE props, but it works like that just
            // fine on Gamescope for now.
            // FIXME: use DRM_FORMAT_MOD_LINEAR here. Where can we find this constant in the Rust bindings?
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoModifier,
                Long,
                0
            ),
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                spa::utils::Rectangle {
                    width: 320,
                    height: 240
                },
                spa::utils::Rectangle {
                    width: 1,
                    height: 1
                },
                spa::utils::Rectangle {
                    width: 4096,
                    height: 4096
                }
            ),
            pw::spa::pod::property!(
                pw::spa::param::format::FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                spa::utils::Fraction { num: 25, denom: 1 },
                spa::utils::Fraction { num: 0, denom: 1 },
                spa::utils::Fraction {
                    num: 1000,
                    denom: 1
                }
            )
        );

        let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(obj),
        )
        .expect("Failed to serialize pod")
        .0
        .into_inner();

        let mut params = [Pod::from_bytes(&values).unwrap()];

        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT,
            &mut params,
        )?;

        println!("Stream connected");

        Ok(Self {
            main_loop,
            context,
            core,
            stream,
            user_data,
            listener,
        })
    }

    pub fn run(&self) {
        self.main_loop.run()
    }
}
