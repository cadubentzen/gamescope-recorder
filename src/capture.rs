use std::{
    sync::{Arc, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Result;
use libspa::{self as spa, pod::Pod};
use pipewire::{self as pw, main_loop, properties::properties};

#[allow(dead_code)]
struct UserData {
    format: spa::param::video::VideoInfoRaw,
    out_buffer: Vec<u8>,
    last_frame_time: Option<Instant>,
    config: CapturerConfig,
}

pub struct FrameGuard<'s> {
    user_data: MutexGuard<'s, UserData>,
}

struct Terminate;

#[allow(dead_code)]
pub struct Capturer {
    capture_thread: Option<JoinHandle<anyhow::Result<()>>>,
    user_data: Arc<Mutex<UserData>>,
    pw_sender: pw::channel::Sender<Terminate>,
}

pub struct CapturerConfig {
    pub frame_rate: u32,
}

impl Capturer {
    pub fn new(config: CapturerConfig) -> Result<Self> {
        let user_data = Arc::new(Mutex::new(UserData {
            format: Default::default(),
            out_buffer: Vec::new(),
            last_frame_time: None,
            config,
        }));
        let (pw_sender, pw_receiver) = pw::channel::channel();
        let capture_thread = std::thread::spawn::<_, Result<()>>({
            let user_data = user_data.clone();
            move || {
                let main_loop = main_loop::MainLoop::new(None)?;
                let context = pw::context::Context::new(&main_loop)?;
                let core = context.connect(None)?;

                let _receiver = pw_receiver.attach(main_loop.loop_(), {
                    let main_loop = main_loop.clone();
                    move |_| main_loop.quit()
                });

                let props = properties! {
                    *pw::keys::MEDIA_TYPE => "Video",
                    *pw::keys::MEDIA_CATEGORY => "Capture",
                    *pw::keys::MEDIA_ROLE => "Screen",
                    *pw::keys::NODE_NAME => "gamescope",
                };

                let stream = pw::stream::Stream::new(&core, "zeroscope", props)?;

                let _listener = stream
                    .add_local_listener_with_user_data(user_data.clone())
                    .state_changed(|_, _, old_state, new_state| {
                        println!("State changed: {old_state:?} -> {new_state:?}");
                    })
                    .param_changed(|_, user_data, id, param| {
                        println!("Param changed: id = {id}");
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

                            if let Some(last_frame_time) = user_data.last_frame_time {
                                // Evict old frame if older than half the frame duration
                                let frame_timeout =
                                    Duration::from_millis(500 / user_data.config.frame_rate as u64);
                                if Instant::now() - last_frame_time < frame_timeout {
                                    return;
                                } else {
                                    user_data.last_frame_time = Some(Instant::now());
                                }
                            }

                            let datas = buffer.datas_mut();
                            if datas.is_empty() {
                                eprintln!("No data in pipewire buffer");
                                return;
                            }
                            let data = &mut datas[0];
                            assert!(
                                data.type_() == pw::spa::buffer::DataType::MemFd,
                                "Expected MemFd data type"
                            );
                            let fd = data.fd().unwrap();
                            let mmap = unsafe {
                                memmap2::Mmap::map(fd as i32)
                                    .expect("Failed to map pipewire buffer to memory")
                            };
                            let size = data.chunk().size() as usize;
                            user_data.out_buffer.resize(size, 0);
                            user_data.out_buffer.copy_from_slice(&mmap[..size]);
                        }
                    })
                    .register()?;

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
                        pw::spa::param::video::VideoFormat::BGRx
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
                    ),
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

                main_loop.run();

                Ok(())
            }
        });

        if capture_thread.is_finished() {
            return Err(anyhow::anyhow!("Capture thread finished prematurely"));
        }

        Ok(Self {
            capture_thread: Some(capture_thread),
            user_data,
            pw_sender,
        })
    }

    pub fn last_frame(&self) -> Option<FrameGuard> {
        let user_data: MutexGuard<UserData> = self.user_data.lock().unwrap();
        if user_data.out_buffer.is_empty() {
            None
        } else {
            Some(FrameGuard { user_data })
        }
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        self.pw_sender.send(Terminate).ok();
        self.capture_thread.take().unwrap().join().ok();
    }
}

impl<'s> FrameGuard<'s> {
    pub fn data(&self) -> &[u8] {
        &self.user_data.out_buffer
    }
}
