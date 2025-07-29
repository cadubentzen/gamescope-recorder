use std::{borrow::Borrow, sync::Arc};

use anyhow::{anyhow, Result};

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
    BlockingMode, FrameLayout, PlaneLayout, Resolution,
};

pub struct Encoder {
    encoder: StatelessEncoder<H264, PooledVaSurface<()>, VaapiBackend<(), PooledVaSurface<()>>>,
    pub frame_layout: FrameLayout,
    pool: VaSurfacePool<()>,
    counter: u64,
}

impl Encoder {
    // FIXME: size changes will break this encoder
    pub fn new(framerate: u32, first_frame: &Arc<PooledVaSurface<()>>) -> Result<Self> {
        let surface: &Surface<()> = std::borrow::Borrow::borrow(first_frame.as_ref());
        let width = surface.size().0;
        let height = surface.size().1;
        let display = surface.display().clone();
        let config = EncoderConfig {
            resolution: Resolution { width, height },
            profile: Profile::Main,
            level: Level::L4_1,
            pred_structure: PredictionStructure::LowDelay { limit: 240 }, // Every 4s for 60fps
            initial_tunings: Tunings {
                rate_control: cros_codecs::encoder::RateControl::ConstantBitrate(9_000_000),
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
            encoder,
            frame_layout: frame_layout.clone(),
            pool,
            counter: 0,
        })
    }

    pub fn encode(&mut self, input_surface: Arc<PooledVaSurface<()>>) -> Result<()> {
        let meta = FrameMetadata {
            timestamp: self.counter,
            layout: self.frame_layout.clone(),
            force_keyframe: false,
        };

        let pooled_surface = self
            .pool
            .get_surface()
            .expect("Failed to get surface from pool");
        copy_surfaces(input_surface.as_ref().borrow(), pooled_surface.borrow())
            .map_err(|e| anyhow!("{}", e))?;

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

pub fn copy_surfaces(src_surface: &Surface<()>, dst_surface: &Surface<()>) -> Result<(), String> {
    use cros_codecs::libva::{VAProfile::VAProfileNone, *};

    // TODO: implement proper bindings in cros-libva
    let mut vpp_config = Default::default();
    let mut vpp_context = Default::default();

    let raw_display = src_surface.display().handle();

    let ret = unsafe {
        vaCreateConfig(
            raw_display,
            VAProfileNone,
            VAEntrypoint::VAEntrypointVideoProc,
            std::ptr::null_mut(),
            0,
            &mut vpp_config,
        )
    };
    if ret != VA_STATUS_SUCCESS as i32 {
        return Err(format!("Error creating VPP config: {ret:?}"));
    }

    let ret = unsafe {
        vaCreateContext(
            raw_display,
            vpp_config,
            dst_surface.size().0 as i32,
            dst_surface.size().1 as i32,
            VA_PROGRESSIVE as i32,
            &mut dst_surface.id(),
            1,
            &mut vpp_context,
        )
    };
    if ret != VA_STATUS_SUCCESS as i32 {
        unsafe { vaDestroyConfig(raw_display, vpp_config) };
        return Err(format!("Error creating VPP context: {ret:?}"));
    }

    let pipeline_param = VAProcPipelineParameterBuffer {
        surface: src_surface.id(),
        ..Default::default()
    };
    let mut params = [pipeline_param];

    let mut pipeline_buf = Default::default();
    let ret = unsafe {
        vaCreateBuffer(
            raw_display,
            vpp_context,
            VABufferType::VAProcPipelineParameterBufferType,
            std::mem::size_of::<VAProcPipelineParameterBuffer>() as u32,
            1,
            params.as_mut_ptr() as *mut _,
            &mut pipeline_buf,
        )
    };

    if ret != VA_STATUS_SUCCESS as i32 {
        unsafe {
            vaDestroyContext(raw_display, vpp_context);
            vaDestroyConfig(raw_display, vpp_config);
        }
        return Err(format!("Error creating VPP pipeline buffer: {ret:?}"));
    }

    unsafe {
        vaBeginPicture(raw_display, vpp_context, dst_surface.id());
        vaRenderPicture(raw_display, vpp_context, &mut pipeline_buf, 1);
        vaEndPicture(raw_display, vpp_context);
        vaSyncSurface(raw_display, dst_surface.id());

        vaDestroyBuffer(raw_display, pipeline_buf);
        vaDestroyContext(raw_display, vpp_context);
        vaDestroyConfig(raw_display, vpp_config);
    };

    // TODO: detect and use vaCopy when possible instead as below, since it's faster.
    // It doesn't work on AMD though.

    // let mut dst_object = _VACopyObject {
    //     obj_type: VACopyObjectType::VACopyObjectSurface,
    //     object: _VACopyObject__bindgen_ty_1 { surface_id: dst_surface.id() },
    //     ..Default::default()
    // };
    // let mut src_object = _VACopyObject {
    //     obj_type: VACopyObjectType::VACopyObjectSurface,
    //     object: _VACopyObject__bindgen_ty_1 { surface_id: src_surface.id() },
    //     ..Default::default()
    // };

    // let ret = unsafe {
    //     vaCopy(display.handle(), &mut dst_object, &mut src_object, Default::default())
    // };

    // if ret != VA_STATUS_SUCCESS as i32 {
    //     return Err(format!("Error copying GenericDmaVideoFrame to VA-API surface: {ret:?}"));
    // }

    // unsafe { vaSyncSurface(display.handle(), dst_surface.id()) };

    Ok(())
}
