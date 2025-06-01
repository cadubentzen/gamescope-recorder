use cros_codecs::{
    BlockingMode, FrameLayout, PlaneLayout, Resolution,
    backend::vaapi::{
        encoder::VaapiBackend,
        surface_pool::{PooledVaSurface, VaSurfacePool},
    },
    codec::h264::parser::{Level, Profile},
    decoder::FramePool,
    encoder::{
        FrameMetadata, PredictionStructure, Tunings, VideoEncoder,
        h264::{EncoderConfig, H264},
        stateless::StatelessEncoder,
    },
    libva::{Surface, UsageHint, VA_RT_FORMAT_YUV420},
    video_frame::{VideoFrame, generic_dma_video_frame::GenericDmaVideoFrame},
};

use std::{borrow::Borrow, io::Write};

fn main() {
    let width = 1280;
    let height = 720;
    let framerate = 60;

    let display = cros_codecs::libva::Display::open().expect("Failed to open VA display");
    let config = EncoderConfig {
        resolution: Resolution { width, height },
        profile: Profile::Main,
        level: Level::L4_1,
        pred_structure: PredictionStructure::LowDelay { limit: 240 }, // Every 4s for 60fps
        initial_tunings: Tunings {
            rate_control: cros_codecs::encoder::RateControl::ConstantBitrate(3_000_000),
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
    let blocking_mode = BlockingMode::Blocking;
    let mut encoder = StatelessEncoder::<H264, PooledVaSurface<()>, _>::new_vaapi2(
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

    let mut generator = generator::TestFrameGenerator::new(
        60, // Max count of frames to generate
        display,
        pool,
        frame_layout.clone(),
    );

    let mut output_file =
        std::fs::File::create("output.h264").expect("Failed to create output file");

    while let Some((meta, pooled_surface)) = generator.next() {
        encoder
            .encode(meta, pooled_surface)
            .expect("Failed to encode frame");

        while let Some(bitstream) = encoder.poll().expect("Failed to poll encoder") {
            println!("Encoded frame available");
            output_file
                .write_all(&bitstream.bitstream)
                .expect("Failed to write to output file");
            println!("Bitstream metadata: {:#?}", bitstream.metadata);
            println!("Bitstream size: {}", bitstream.bitstream.len());
        }
    }

    encoder.drain().expect("Failed to drain encoder");

    while let Some(bitstream) = encoder.poll().expect("Failed to poll encoder") {
        println!("Encoded frame available");
        output_file
            .write_all(&bitstream.bitstream)
            .expect("Failed to write to output file");
        println!("Bitstream metadata: {:#?}", bitstream.metadata);
        println!("Bitstream size: {}", bitstream.bitstream.len());
    }
}

mod generator {
    use std::rc::Rc;

    use super::*;

    use cros_codecs::{
        Fourcc,
        libva::{self, Display, SurfaceMemoryDescriptor, VA_FOURCC_NV12},
    };

    pub fn get_test_frame_t(ts: u64, max_ts: u64) -> f32 {
        2.0 * std::f32::consts::PI * (ts as f32) / (max_ts as f32)
    }

    fn gen_test_frame<F>(frame_width: usize, frame_height: usize, t: f32, mut set_pix: F)
    where
        F: FnMut(usize, usize, [f32; 3]),
    {
        let width = frame_width as f32;
        let height = frame_height as f32;
        let (sin, cos) = f32::sin_cos(t);
        let (sin2, cos2) = (sin.powi(2), cos.powi(2));

        // Pick the dot position
        let dot_col = height * (1.1 + 2.0 * sin * cos) / 2.2;
        let dot_row = width * (1.1 + sin) / 2.2;
        let dot_size2 = (width.min(height) * 0.05).powi(2);

        // Luma
        for frame_row in 0..frame_height {
            #[allow(clippy::needless_range_loop)]
            for frame_col in 0..frame_width {
                let row = frame_row as f32;
                let col = frame_col as f32;

                let dist = (dot_col - col).powi(2) + (dot_row - row).powi(2);

                let y = if dist < dot_size2 {
                    0.0
                } else {
                    (row + col) / (width + height)
                };

                let (u, v) = if dist < dot_size2 {
                    (0.5, 0.5)
                } else {
                    ((row / width) * sin2, (col / height) * cos2)
                };

                set_pix(frame_col, frame_row, [y, u, v]);
            }
        }
    }

    fn fill_test_frame_nm12(
        width: usize,
        height: usize,
        strides: [usize; 2],
        t: f32,
        y_plane: &mut [u8],
        uv_plane: &mut [u8],
    ) {
        gen_test_frame(width, height, t, |col, row, yuv| {
            /// Maximum value of color component for NV12
            const MAX_COMP_VAL: f32 = 0xff as f32;

            let (y, u, v) = (
                (yuv[0] * MAX_COMP_VAL).clamp(0.0, MAX_COMP_VAL) as u8,
                (yuv[1] * MAX_COMP_VAL).clamp(0.0, MAX_COMP_VAL) as u8,
                (yuv[2] * MAX_COMP_VAL).clamp(0.0, MAX_COMP_VAL) as u8,
            );
            let y_pos = row * strides[0] + col;

            y_plane[y_pos] = y;

            // Subsample with upper left pixel
            if col % 2 == 0 && row % 2 == 0 {
                let u_pos = (row / 2) * strides[1] + col;
                let v_pos = u_pos + 1;

                uv_plane[u_pos] = u;
                uv_plane[v_pos] = v;
            }
        });
    }

    pub fn fill_test_frame_nv12(
        width: usize,
        height: usize,
        strides: [usize; 2],
        offsets: [usize; 2],
        t: f32,
        raw: &mut [u8],
    ) {
        let (y_plane, uv_plane) = raw.split_at_mut(offsets[1]);
        let y_plane = &mut y_plane[offsets[0]..];

        fill_test_frame_nm12(width, height, strides, t, y_plane, uv_plane)
    }

    fn map_surface<'a, M: SurfaceMemoryDescriptor>(
        display: &Rc<Display>,
        surface: &'a Surface<M>,
        fourcc: u32,
    ) -> libva::Image<'a> {
        let image_fmts = display.query_image_formats().unwrap();
        let image_fmt = image_fmts.into_iter().find(|f| f.fourcc == fourcc).unwrap();

        libva::Image::create_from(surface, image_fmt, surface.size(), surface.size()).unwrap()
    }

    fn map_surface_nv12<'a, M: SurfaceMemoryDescriptor>(
        display: &Rc<Display>,
        surface: &'a Surface<M>,
    ) -> libva::Image<'a> {
        map_surface(display, surface, VA_FOURCC_NV12)
    }

    /// Uploads raw NV12 to Surface
    pub fn upload_nv12_img<M: SurfaceMemoryDescriptor>(
        display: &Rc<Display>,
        surface: &Surface<M>,
        width: u32,
        height: u32,
        data: &[u8],
    ) {
        let mut image = map_surface_nv12(display, surface);

        let va_image = *image.image();
        let dest = image.as_mut();
        let width = width as usize;
        let height = height as usize;

        let mut src: &[u8] = data;
        let mut dst = &mut dest[va_image.offsets[0] as usize..];

        // Copy luma
        for _ in 0..height {
            dst[..width].copy_from_slice(&src[..width]);
            dst = &mut dst[va_image.pitches[0] as usize..];
            src = &src[width..];
        }

        // Advance to the offset of the chroma plane
        let mut src = &data[width * height..];
        let mut dst = &mut dest[va_image.offsets[1] as usize..];

        let height = height / 2;

        // Copy chroma
        for _ in 0..height {
            dst[..width].copy_from_slice(&src[..width]);
            dst = &mut dst[va_image.pitches[1] as usize..];
            src = &src[width..];
        }

        surface.sync().unwrap();
        drop(image);
    }

    /// Helper struct. [`Iterator`] to fetch frames from [`SurfacePool`].
    pub struct PooledFrameIterator {
        counter: u64,
        display: Rc<Display>,
        pool: VaSurfacePool<()>,
        frame_layout: FrameLayout,
    }

    impl PooledFrameIterator {
        pub fn new(
            display: Rc<Display>,
            pool: VaSurfacePool<()>,
            frame_layout: FrameLayout,
        ) -> Self {
            Self {
                counter: 0,
                display,
                pool,
                frame_layout,
            }
        }
    }

    impl Iterator for PooledFrameIterator {
        type Item = (FrameMetadata, PooledVaSurface<()>);

        fn next(&mut self) -> Option<Self::Item> {
            let handle = self.pool.get_surface().unwrap();

            let meta = FrameMetadata {
                layout: self.frame_layout.clone(),
                force_keyframe: false,
                timestamp: self.counter,
            };

            self.counter += 1;

            Some((meta, handle))
        }
    }

    /// Helper struct. Uses [`Iterator`] with raw chunks and uploads to pooled surface from
    /// [`SurfacePool`] to produce frames.
    pub struct NV12FrameProducer<'l, I>
    where
        I: Iterator<Item = &'l [u8]>,
    {
        raw_iterator: I,
        pool_iter: PooledFrameIterator,
    }

    impl<'l, I> NV12FrameProducer<'l, I>
    where
        I: Iterator<Item = &'l [u8]>,
    {
        #[allow(dead_code)]
        pub fn new(
            raw_iterator: I,
            display: Rc<Display>,
            pool: VaSurfacePool<()>,
            frame_layout: FrameLayout,
        ) -> Self {
            Self {
                raw_iterator,
                pool_iter: PooledFrameIterator::new(display, pool, frame_layout),
            }
        }
    }

    impl<'l, I> Iterator for NV12FrameProducer<'l, I>
    where
        I: Iterator<Item = &'l [u8]>,
    {
        type Item = (FrameMetadata, PooledVaSurface<()>);

        fn next(&mut self) -> Option<Self::Item> {
            let raw = match self.raw_iterator.next() {
                Some(raw) => raw,
                None => return None,
            };

            let (meta, handle) = self.pool_iter.next().unwrap();

            let width = meta.layout.size.width;
            let height = meta.layout.size.height;
            debug_assert_eq!((width * height + width * height / 2) as usize, raw.len());

            upload_nv12_img(&self.pool_iter.display, handle.borrow(), width, height, raw);

            Some((meta, handle))
        }
    }

    pub fn upload_test_frame_nv12<M: SurfaceMemoryDescriptor>(
        display: &Rc<Display>,
        surface: &Surface<M>,
        t: f32,
    ) {
        let mut image = map_surface_nv12(display, surface);

        let (width, height) = image.display_resolution();

        let offsets = image.image().offsets;
        let pitches = image.image().pitches;

        fill_test_frame_nv12(
            width as usize,
            height as usize,
            [pitches[0] as usize, pitches[1] as usize],
            [offsets[0] as usize, offsets[1] as usize],
            t,
            image.as_mut(),
        );

        drop(image);
        surface.sync().unwrap();
    }

    /// Helper struct. Procedurally generate NV12 frames for test purposes.
    pub struct TestFrameGenerator {
        counter: u64,
        max_count: u64,
        pool_iter: PooledFrameIterator,
        display: Rc<Display>,
        fourcc: Fourcc,
    }

    impl TestFrameGenerator {
        pub fn new(
            max_count: u64,
            display: Rc<Display>,
            pool: VaSurfacePool<()>,
            frame_layout: FrameLayout,
        ) -> Self {
            Self {
                counter: 0,
                max_count,
                fourcc: frame_layout.format.0,
                pool_iter: PooledFrameIterator::new(display.clone(), pool, frame_layout),
                display,
            }
        }
    }

    impl Iterator for TestFrameGenerator {
        type Item = (FrameMetadata, PooledVaSurface<()>);

        fn next(&mut self) -> Option<Self::Item> {
            if self.counter > self.max_count {
                return None;
            }

            self.counter += 1;

            let (meta, handle) = self.pool_iter.next().unwrap();

            let surface: &Surface<()> = handle.borrow();

            let t = get_test_frame_t(meta.timestamp, self.max_count);
            match self.fourcc.0 {
                VA_FOURCC_NV12 => upload_test_frame_nv12(&self.display, surface, t),
                _ => unreachable!(),
            }

            Some((meta, handle))
        }
    }
}
