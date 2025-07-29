use std::{
    fs::File,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use anyhow::Result;
use cros_codecs::{
    backend::vaapi::surface_pool::{PooledVaSurface, VaSurfacePool},
    decoder::FramePool,
    libva::{Display, UsageHint, VA_RT_FORMAT_YUV420},
    video_frame::generic_dma_video_frame::GenericDmaVideoFrame,
    Fourcc, FrameLayout, PlaneLayout, Resolution,
};
use libspa::{
    self as spa,
    pod::{ChoiceValue, Pod, Property, Value},
    utils::{Choice, ChoiceEnum, ChoiceFlags},
};
use pipewire::{self as pw, main_loop, properties::properties};

#[allow(dead_code)]
struct UserData {
    format: spa::param::video::VideoInfoRaw,
    pool: Option<VaSurfacePool<()>>,
    last_frame: Option<Arc<PooledVaSurface<()>>>,
}

struct Terminate;

#[allow(dead_code)]
pub struct Capturer {
    capture_thread: Option<JoinHandle<anyhow::Result<()>>>,
    user_data: Arc<Mutex<UserData>>,
    pw_sender: pw::channel::Sender<Terminate>,
}

impl Capturer {
    pub fn new() -> Result<Self> {
        let user_data = Arc::new(Mutex::new(UserData {
            format: Default::default(),
            pool: None,
            last_frame: None,
        }));
        let (pw_sender, pw_receiver) = pw::channel::channel();
        let capture_thread = thread::spawn::<_, Result<()>>({
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
                    *pw::keys::TARGET_OBJECT => "gamescope",
                };

                let stream = pw::stream::Stream::new(&core, "zeroscope", props)?;

                let _listener = stream
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

                        println!("Got video format:");

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
                        println!("  color_range: {:?}", user_data.format.color_range());
                        println!("  color_matrix: {:?}", user_data.format.color_matrix());

                        let display = Display::open().unwrap();
                        let mut pool = VaSurfacePool::new(
                            display.clone(),
                            VA_RT_FORMAT_YUV420,
                            Some(UsageHint::USAGE_HINT_ENCODER),
                            Resolution {
                                width: user_data.format.size().width,
                                height: user_data.format.size().height,
                            },
                        );
                        pool.add_frames(vec![(); 16])
                            .expect("Failed to add frames to pool");
                        user_data.pool = Some(pool);
                    })
                    .process(|stream, user_data| match stream.dequeue_buffer() {
                        None => println!("out of buffers"),
                        Some(mut buffer) => {
                            let datas = buffer.datas_mut();
                            if datas.is_empty() {
                                eprintln!("No data in pipewire buffer");
                                return;
                            }
                            let data = &mut datas[0];
                            let fd: std::os::unix::prelude::BorrowedFd<'_> =
                                data.fd().expect("Failed to get fd from buffer data");
                            let file = File::from(fd.try_clone_to_owned().unwrap());

                            let mut user_data = user_data.lock().unwrap();

                            let fourcc = Fourcc::from(b"NV12");
                            let width = user_data.format.size().width;
                            let height = user_data.format.size().height;
                            let frame_layout = FrameLayout {
                                format: (fourcc, 0),
                                size: Resolution { width, height },
                                planes: vec![
                                    PlaneLayout {
                                        buffer_index: 0,
                                        offset: 0,
                                        stride: width as usize,
                                    },
                                    PlaneLayout {
                                        buffer_index: 0,
                                        offset: width as usize * height as usize,
                                        stride: width as usize,
                                    },
                                ],
                            };

                            let dma_frame = GenericDmaVideoFrame::new(vec![file], frame_layout)
                                .expect("Failed to create GenericDmaVideoFrame");

                            let pooled_surface = user_data
                                .pool
                                .as_mut()
                                .unwrap()
                                .get_surface()
                                .expect("Failed to get surface from pool");

                            dma_frame
                                .copy_to_surface(std::borrow::Borrow::borrow(&pooled_surface))
                                .unwrap();
                            user_data.last_frame = Some(Arc::new(pooled_surface));
                            // println!("Captured frame: {}x{}", width, height);
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
                    ),
                    // FIXME: implement enums for color structs and use property! macro
                    Property::new(
                        pw::spa::sys::SPA_FORMAT_VIDEO_colorRange,
                        Value::Choice(ChoiceValue::Id(Choice(
                            ChoiceFlags::_FAKE,
                            ChoiceEnum::Enum {
                                // Limited color range
                                default: pw::spa::utils::Id(2),
                                alternatives: vec![pw::spa::utils::Id(2)],
                            },
                        ))),
                    ),
                    Property::new(
                        pw::spa::sys::SPA_FORMAT_VIDEO_colorMatrix,
                        Value::Choice(ChoiceValue::Id(Choice(
                            ChoiceFlags::_FAKE,
                            ChoiceEnum::Enum {
                                // BT.709
                                default: pw::spa::utils::Id(3),
                                alternatives: vec![pw::spa::utils::Id(3)],
                            },
                        ))),
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

    pub fn read_frame(&self) -> Option<Arc<PooledVaSurface<()>>> {
        let user_data = self.user_data.lock().unwrap();
        user_data.last_frame.clone()
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        self.pw_sender.send(Terminate).ok();
        self.capture_thread.take().unwrap().join().ok();
    }
}
