use std::{borrow::Borrow, fs::File, rc::Rc};

use anyhow::Result;

use cros_codecs::{
    backend::vaapi::{
        encoder::VaapiBackend,
        surface_pool::{PooledVaSurface, VaSurfacePool},
    },
    codec::h264::parser::{Level, Profile},
    decoder::FramePool,
    encoder::{
        h264::{EncoderConfig, H264},
        stateless::StatelessEncoder,
        FrameMetadata, PredictionStructure, Tunings, VideoEncoder,
    },
    libva::{Surface, UsageHint, VA_RT_FORMAT_YUV420},
    video_frame::generic_dma_video_frame::GenericDmaVideoFrame,
    BlockingMode, FrameLayout, PlaneLayout, Resolution,
};

pub struct Encoder {
    display: Rc<cros_codecs::libva::Display>,
    encoder: StatelessEncoder<H264, PooledVaSurface<()>, VaapiBackend<(), PooledVaSurface<()>>>,
    pub frame_layout: FrameLayout,
    pool: VaSurfacePool<()>,
    counter: u64,
}

impl Encoder {
    // FIXME: size changes will break this encoder
    pub fn new(width: u32, height: u32, framerate: u32) -> Result<Self> {
        let display = cros_codecs::libva::Display::open().expect("Failed to open VA display");
        let config = EncoderConfig {
            resolution: Resolution { width, height },
            profile: Profile::Main,
            level: Level::L4_1,
            pred_structure: PredictionStructure::LowDelay { limit: 240 }, // Every 4s for 60fps
            initial_tunings: Tunings {
                rate_control: cros_codecs::encoder::RateControl::ConstantBitrate(4_000_000),
                framerate,
                min_quality: 0,
                max_quality: u32::MAX,
            },
        };
        let fourcc = cros_codecs::Fourcc::from(b"NV12");
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
        let coded_size = cros_codecs::Resolution { width, height };
        let low_power = false;
        let blocking_mode = BlockingMode::NonBlocking;
        let encoder = StatelessEncoder::<H264, _, _>::new_native_vaapi(
            display.clone(),
            config,
            fourcc,
            coded_size,
            low_power,
            blocking_mode,
        )
        .expect("Failed to create H264 encoder");

        let mut pool = VaSurfacePool::<()>::new(
            display.clone(),
            VA_RT_FORMAT_YUV420,
            Some(UsageHint::USAGE_HINT_ENCODER),
            Resolution { width, height },
        );
        pool.add_frames(vec![(); 16])
            .expect("Failed to add frames to pool");

        Ok(Encoder {
            display: display.clone(),
            encoder,
            frame_layout: frame_layout.clone(),
            pool,
            counter: 0,
        })
    }

    pub fn encode(&mut self, dmabuf: &File) -> Result<()> {
        let pooled_surface = self
            .pool
            .get_surface()
            .expect("Failed to get surface from pool");

        let frame =
            GenericDmaVideoFrame::new(vec![dmabuf.try_clone().unwrap()], self.frame_layout.clone())
                .unwrap();
        let surface: &Surface<()> = pooled_surface.borrow();
        frame.copy_to_surface(surface, &self.display).unwrap();

        let meta = FrameMetadata {
            timestamp: self.counter,
            layout: self.frame_layout.clone(),
            force_keyframe: false,
        };
        self.counter += 1;
        // FIXME: implement Error for EncodeError
        self.encoder
            .encode(meta, pooled_surface)
            .expect("Failed to encode frame");
        Ok(())
    }

    pub fn drain(&mut self) -> Result<()> {
        // FIXME: implement Error for EncodeError
        self.encoder.drain().expect("Failed to drain encoder");
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<cros_codecs::encoder::CodedBitstreamBuffer>> {
        // FIXME: implement Error for EncodeError
        let bitstream_buffer = self.encoder.poll().expect("Failed to poll encoder");
        Ok(bitstream_buffer)
    }
}
