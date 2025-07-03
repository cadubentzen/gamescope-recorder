use std::{
    collections::HashMap,
    fs::File,
    os::fd::{FromRawFd, RawFd},
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Result;
use libspa::{self as spa, pod::Pod};
use pipewire::{self as pw, properties::properties};

#[allow(dead_code)]
struct UserData {
    format: spa::param::video::VideoInfoRaw,
    dmabufs: HashMap<i32, File>,
    last_frame: Option<i32>,
}

#[allow(dead_code)]
pub struct Capturer {
    capture_thread: std::thread::JoinHandle<anyhow::Result<()>>,
    user_data: Arc<Mutex<UserData>>,
}

impl Capturer {
    pub fn new() -> Result<Self>
// where
// F: FnMut(spa::param::video::VideoInfoRaw, pw::buffer::Buffer) + 'static,
    {
        let user_data = Arc::new(Mutex::new(UserData {
            format: Default::default(),
            dmabufs: HashMap::new(),
            last_frame: None,
        }));
        let capture_thread = std::thread::spawn::<_, Result<()>>({
            let user_data = user_data.clone();
            move || {
                println!("Starting capture thread...");
                let main_loop = pw::main_loop::MainLoop::new(None)?;
                println!("Main loop created");
                let context = pw::context::Context::new(&main_loop)?;
                println!("Context created");
                let core = context.connect(None)?;
                println!("Connected to PipeWire core");

                let props = properties! {
                    *pw::keys::MEDIA_TYPE => "Video",
                    *pw::keys::MEDIA_CATEGORY => "Capture",
                    *pw::keys::MEDIA_ROLE => "Screen",
                    *pw::keys::NODE_NAME => "gamescope",
                };

                println!("Creating stream with properties: {:?}", props);
                let stream = pw::stream::Stream::new(&core, "zeroscope", props)?;

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
                        Some(mut buffer) => {
                            let mut user_data = user_data.lock().unwrap();
                            let datas = buffer.datas_mut();
                            if datas.is_empty() {
                                eprintln!("No data in pipewire buffer");
                                return;
                            }
                            let data = &mut datas[0];
                            let fd = RawFd::from(data.fd().unwrap() as i32);
                            if !user_data.dmabufs.contains_key(&fd) {
                                let file = unsafe { File::from_raw_fd(fd) };
                                user_data.dmabufs.insert(fd, file);
                            }
                            user_data.last_frame = Some(fd);
                        }
                    })
                    .register()?;

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

                println!("Serializing pod object: {:?}", obj);
                let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &pw::spa::pod::Value::Object(obj),
                )
                .expect("Failed to serialize pod")
                .0
                .into_inner();

                let mut params = [Pod::from_bytes(&values).unwrap()];

                println!("Connecting stream");
                stream.connect(
                    spa::utils::Direction::Input,
                    None,
                    pw::stream::StreamFlags::AUTOCONNECT,
                    &mut params,
                )?;

                println!("Stream connected. Running main loop...");
                main_loop.run();

                Ok(())
            }
        });

        if capture_thread.is_finished() {
            return Err(anyhow::anyhow!("Capture thread finished prematurely"));
        }

        Ok(Self {
            capture_thread,
            user_data,
        })
    }

    pub fn last_frame(&self) -> Option<FrameGuard> {
        let user_data = self.user_data.lock().unwrap();
        user_data
            .last_frame
            .and_then(|_| Some(FrameGuard { user_data }))
    }
}

pub struct FrameGuard<'s> {
    user_data: MutexGuard<'s, UserData>,
}

impl<'s> FrameGuard<'s> {
    pub fn width(&self) -> u32 {
        self.user_data.format.size().width
    }

    pub fn height(&self) -> u32 {
        self.user_data.format.size().height
    }

    pub fn dmabuf(&self) -> &File {
        // We check that last_frame is Some in Capturer::last_frame()
        self.user_data
            .dmabufs
            .get(&self.user_data.last_frame.unwrap())
            .unwrap()
    }
}
