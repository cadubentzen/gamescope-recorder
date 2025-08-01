use std::{
    ffi::{c_uint, c_void, CString},
    fs::File,
    io::Write,
    slice,
    str::FromStr,
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use cros_codecs::{
    backend::vaapi::surface_pool::PooledVaSurface,
    libva::{Surface, VADisplay, VASurfaceID},
};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avutil::{ra, AVDictionary, AVFrame, AVHWDeviceContext},
    error::RsmpegError,
    ffi::{
        self, AV_HWDEVICE_TYPE_VAAPI, AV_PIX_FMT_NV12, AV_PIX_FMT_VAAPI, FF_PROFILE_H264_BASELINE,
        FF_PROFILE_H264_CONSTRAINED_BASELINE,
    },
};

#[repr(C)]
pub struct AVVAAPIDeviceContext {
    pub display: *mut c_void, // VADisplay is typically a void pointer
    pub driver_quirks: c_uint,
}

pub struct Encoder {
    _counter: u64,
    avctx: AVCodecContext,
}

impl Encoder {
    // FIXME: size changes will break this encoder
    pub fn new(framerate: i32, first_frame: &Arc<PooledVaSurface<()>>) -> Result<Self> {
        println!("Encoder::new - Starting encoder initialization");
        let surface: &Surface<()> = std::borrow::Borrow::borrow(first_frame.as_ref());
        let width = surface.size().0 as i32;
        let height = surface.size().1 as i32;
        println!("Encoder::new - Surface size: {}x{}", width, height);
        let display = surface.display().clone();
        let mut hw_device_ctx = AVHWDeviceContext::alloc(AV_HWDEVICE_TYPE_VAAPI);
        let device_ctx = unsafe { *hw_device_ctx.as_mut_ptr() }.data as *mut ffi::AVHWDeviceContext;
        let vaapi_ctx = unsafe { *device_ctx }.hwctx as *mut AVVAAPIDeviceContext;
        unsafe {
            (*vaapi_ctx).display = display.handle();
        }

        hw_device_ctx
            .init()
            .context("Failed to initialize VAAPI device context")?;

        let codec =
            AVCodec::find_encoder_by_name(c"h264_vaapi").context("Could not find encoder.")?;
        let mut avctx = AVCodecContext::new(&codec);

        avctx.set_width(width);
        avctx.set_height(height);
        avctx.set_time_base(ra(1, framerate));
        avctx.set_framerate(ra(framerate, 1));
        avctx.set_sample_aspect_ratio(ra(1, 1));
        avctx.set_pix_fmt(AV_PIX_FMT_VAAPI);

        // WebRTC settings
        avctx.set_bit_rate(9_000_000);
        avctx.set_rc_max_rate(11_000_000);
        avctx.set_rc_buffer_size(9_000_000 * 2);
        avctx.set_max_b_frames(0);
        avctx.set_gop_size(framerate);
        avctx.set_keyint_min(framerate);
        avctx.set_refs(1);
        avctx.set_qmin(20);
        avctx.set_qmax(32);
        avctx.set_profile(FF_PROFILE_H264_CONSTRAINED_BASELINE as i32);

        let opts = AVDictionary::new_int(CString::from_str("rc_mode").unwrap().as_c_str(), 3, 0)
            .set_int(CString::from_str("quality").unwrap().as_c_str(), 4, 0);

        let mut hw_frames_ref = hw_device_ctx.hwframe_ctx_alloc();
        hw_frames_ref.data().format = AV_PIX_FMT_VAAPI;
        hw_frames_ref.data().sw_format = AV_PIX_FMT_NV12;
        hw_frames_ref.data().width = width as i32;
        hw_frames_ref.data().height = height as i32;
        hw_frames_ref.data().initial_pool_size = 16;

        hw_frames_ref
            .init()
            .context("Failed to initialize VAAPI frame context")?;
        avctx.set_hw_frames_ctx(hw_frames_ref);

        avctx
            .open(Some(opts))
            .context("Cannot open video encoder codec")?;

        println!("Encoder::new - Encoder created successfully");
        Ok(Encoder { _counter: 0, avctx })
    }

    pub fn encode(&mut self, input_surface: Arc<PooledVaSurface<()>>) -> Result<()> {
        let surface: &Surface<()> = std::borrow::Borrow::borrow(input_surface.as_ref());
        let width = surface.size().0 as i32;
        let height = surface.size().1 as i32;

        let mut pooled_frame = AVFrame::new();
        self.avctx
            .hw_frames_ctx_mut()
            .unwrap()
            .get_buffer(&mut pooled_frame)
            .context("Get buffer failed")?;

        let dpy = surface.display().handle();
        let src_surface = surface.id();
        let dst_surface = pooled_frame.data_mut()[3] as u32;
        copy_surfaces(dpy, src_surface, dst_surface, width, height)
            .context("Failed to copy surfaces")?;

        self.avctx
            .send_frame(Some(&pooled_frame))
            .context("Send frame failed")?;

        Ok(())
    }

    pub fn drain_write(&mut self, file: &mut File) -> Result<()> {
        println!("Encoder::drain_write - Starting drain");
        self.avctx.send_frame(None).context("Send frame failed")?;
        let mut packet_count = 0;
        loop {
            let mut packet = match self.avctx.receive_packet() {
                Ok(packet) => packet,
                Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => {
                    break;
                }
                Err(e) => {
                    println!("Encoder::drain_write - Error receiving packet: {:?}", e);
                    Err(e).context("Receive packet failed.")?
                }
            };
            packet.set_stream_index(0);
            let data = unsafe { slice::from_raw_parts(packet.data, packet.size as usize) };
            file.write_all(data).context("Write output frame failed.")?;
            packet_count += 1;
        }
        println!(
            "Encoder::drain_write - Drain complete, wrote {} packets",
            packet_count
        );
        Ok(())
    }

    pub fn poll_write(&mut self, file: &mut File) -> Result<usize> {
        let mut num_packets = 0;
        let mut packet = match self.avctx.receive_packet() {
            Ok(packet) => {
                num_packets += 1;
                packet
            }
            Err(RsmpegError::EncoderDrainError) | Err(RsmpegError::EncoderFlushedError) => {
                return Ok(num_packets);
            }
            Err(e) => Err(e).context("Receive packet failed.")?,
        };
        packet.set_stream_index(0);
        let data = unsafe { slice::from_raw_parts(packet.data, packet.size as usize) };
        file.write_all(data)
            .context("Failed to write packet data to file")?;
        Ok(num_packets)
    }
}

pub fn copy_surfaces(
    raw_display: VADisplay,
    src_surface: VASurfaceID,
    mut dst_surface: VASurfaceID,
    width: i32,
    height: i32,
) -> Result<()> {
    use cros_codecs::libva::{VAProfile::VAProfileNone, *};

    // TODO: implement proper bindings in cros-libva
    let mut vpp_config = Default::default();
    let mut vpp_context = Default::default();

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
        bail!("Error creating VPP config: {ret:?}");
    }

    let ret = unsafe {
        vaCreateContext(
            raw_display,
            vpp_config,
            width,
            height,
            VA_PROGRESSIVE as i32,
            &mut dst_surface,
            1,
            &mut vpp_context,
        )
    };
    if ret != VA_STATUS_SUCCESS as i32 {
        unsafe { vaDestroyConfig(raw_display, vpp_config) };
        bail!("Error creating VPP context: {ret:?}");
    }

    let pipeline_param = VAProcPipelineParameterBuffer {
        surface: src_surface,
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
        bail!("Error creating VPP pipeline buffer: {ret:?}");
    }

    unsafe {
        vaBeginPicture(raw_display, vpp_context, dst_surface);
        vaRenderPicture(raw_display, vpp_context, &mut pipeline_buf, 1);
        vaEndPicture(raw_display, vpp_context);
        vaSyncSurface(raw_display, dst_surface);

        vaDestroyBuffer(raw_display, pipeline_buf);
        vaDestroyContext(raw_display, vpp_context);
        vaDestroyConfig(raw_display, vpp_config);
    };

    Ok(())
}
