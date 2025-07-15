use std::{borrow::Borrow, fs::File, io::Read, rc::Rc};

use anyhow::{bail, Result};
use clap::Parser;
use cros_codecs::{
    backend::vaapi::surface_pool::VaSurfacePool,
    codec::h264::parser::{Level, Profile},
    decoder::FramePool,
    encoder::{
        h264::{EncoderConfig, H264},
        stateless::StatelessEncoder,
        FrameMetadata, PredictionStructure, Tunings, VideoEncoder,
    },
    libva::{Surface, UsageHint, VA_RT_FORMAT_YUV420, VA_FOURCC_NV12},
    BlockingMode, FrameLayout, PlaneLayout, Resolution,
};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FRAMERATE: u32 = 60;
const FRAME_SIZE: usize = (WIDTH * HEIGHT * 3 / 2) as usize; // NV12 format

fn parse_bitrate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty bitrate string".to_string());
    }

    let (number_part, suffix) = if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len()-1], 1_000_000)
    } else if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len()-1], 1_000)
    } else {
        (s, 1)
    };

    let number: u64 = number_part.parse()
        .map_err(|_| format!("Invalid number: {}", number_part))?;

    Ok(number * suffix)
}

#[derive(Parser)]
#[command(name = "encode-sample")]
#[command(about = "Encode raw NV12 frames to H.264 using VAAPI")]
struct Args {
    /// Input raw NV12 file
    #[arg(long)]
    input: String,

    /// Output H.264 file
    #[arg(long)]
    output: String,

    /// Bitrate (e.g., 6M, 500K, 6000000)
    #[arg(long, value_parser = parse_bitrate)]
    bitrate: u64,

    /// Maximum bitrate (e.g., 8M, 1000K, 8000000)
    #[arg(long, value_parser = parse_bitrate)]
    maxrate: u64,
    
    /// Rate control mode: cbr, vbr, or cqp
    #[arg(long, default_value = "cbr")]
    rc_mode: String,

    /// Maximum number of frames to process (optional, processes all frames if not specified)
    #[arg(long)]
    frames: Option<usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Starting H.264 encoding using VAAPI...");
    println!("Input: {}", args.input);
    println!("Output: {}", args.output);
    println!("Bitrate: {} bps ({:.1} Mbps)", args.bitrate, args.bitrate as f64 / 1_000_000.0);

    // Open the raw NV12 file
    let mut input_file = File::open(&args.input)?;

    // Get file size and calculate total frames
    let file_size = input_file.metadata()?.len() as usize;
    let available_frames = file_size / FRAME_SIZE;
    let total_frames = args.frames.unwrap_or(available_frames).min(available_frames);
    println!("Input file size: {} bytes, estimated frames: {}, processing: {}", file_size, available_frames, total_frames);

    // Initialize VAAPI display
    let Some(display) = cros_codecs::libva::Display::open() else {
        bail!("Failed to open VAAPI display");
    };

    // Configure encoder
    let config = EncoderConfig {
        resolution: Resolution { width: WIDTH, height: HEIGHT },
        profile: Profile::High,
        level: Level::L4_1,
        pred_structure: PredictionStructure::LowDelay { limit: 30 }, // Match FFmpeg keyframe interval
        initial_tunings: Tunings {
            rate_control: match args.rc_mode.as_str() {
                "cbr" => cros_codecs::encoder::RateControl::ConstantBitrate(args.bitrate),
                "vbr" => cros_codecs::encoder::RateControl::VariableBitrate {
                    target_bitrate: args.bitrate,
                    max_bitrate: args.maxrate,
                },
                "cqp" => cros_codecs::encoder::RateControl::ConstantQuality(23), // Default CQP value
                _ => {
                    bail!("Invalid rate control mode: {}. Use cbr, vbr, or cqp", args.rc_mode);
                }
            },
            framerate: FRAMERATE,
            min_quality: 0,
            max_quality: u32::MAX,
        },
    };

    let fourcc = cros_codecs::Fourcc::from(b"NV12");
    let frame_layout = FrameLayout {
        format: (fourcc, 0),
        size: Resolution { width: WIDTH, height: HEIGHT },
        planes: vec![
            PlaneLayout {
                buffer_index: 0,
                offset: 0,
                stride: WIDTH as usize,
            },
            PlaneLayout {
                buffer_index: 0,
                offset: (WIDTH * HEIGHT) as usize,
                stride: WIDTH as usize,
            },
        ],
    };

    // Create encoder
    let mut encoder = StatelessEncoder::<H264, _, _>::new_native_vaapi(
        display.clone(),
        config,
        fourcc,
        Resolution { width: WIDTH, height: HEIGHT },
        false, // low_power
        BlockingMode::NonBlocking,
    ).map_err(|e| anyhow::anyhow!("Failed to create encoder: {:?}", e))?;

    // Create surface pool
    let mut pool = VaSurfacePool::<()>::new(
        display.clone(),
        VA_RT_FORMAT_YUV420,
        Some(UsageHint::USAGE_HINT_ENCODER),
        Resolution { width: WIDTH, height: HEIGHT },
    );
    pool.add_frames(vec![(); 16])?;

    // Create output file
    let mut output_file = File::create(&args.output)?;
    let mut bitstream_data = Vec::new();

    println!("Encoding {} frames...", total_frames);

    // Frame buffer for reading one frame at a time
    let mut frame_buffer = vec![0u8; FRAME_SIZE];

    // Process each frame
    for frame_idx in 0..total_frames {
        // Read one frame from file
        match input_file.read_exact(&mut frame_buffer) {
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                println!("Reached end of file at frame {}", frame_idx);
                break;
            },
            Err(e) => return Err(e.into()),
        }

        // Get surface from pool
        let pooled_surface = pool.get_surface()
            .ok_or_else(|| anyhow::anyhow!("Failed to get surface from pool"))?;

        // Upload frame data to surface
        let surface: &Surface<()> = pooled_surface.borrow();
        upload_nv12_frame(&display, surface, &frame_buffer)?;

        // Create frame metadata
        let meta = FrameMetadata {
            timestamp: frame_idx as u64,
            layout: frame_layout.clone(),
            force_keyframe: frame_idx == 0, // Force keyframe for first frame
        };

        // Encode frame
        encoder.encode(meta, pooled_surface)
            .map_err(|e| anyhow::anyhow!("Failed to encode frame: {:?}", e))?;

        // Poll for encoded data
        while let Some(coded_buffer) = encoder.poll()
            .map_err(|e| anyhow::anyhow!("Failed to poll encoder: {:?}", e))? {
            bitstream_data.extend_from_slice(&coded_buffer.bitstream);
        }

        if frame_idx % 30 == 0 {
            println!("Encoded frame {}/{}", frame_idx + 1, total_frames);
        }
    }

    // Drain encoder
    encoder.drain()
        .map_err(|e| anyhow::anyhow!("Failed to drain encoder: {:?}", e))?;

    // Get remaining encoded data
    while let Some(coded_buffer) = encoder.poll()
        .map_err(|e| anyhow::anyhow!("Failed to poll encoder: {:?}", e))? {
        bitstream_data.extend_from_slice(&coded_buffer.bitstream);
    }

    // Write to output file
    use std::io::Write;
    output_file.write_all(&bitstream_data)?;
    output_file.flush()?;

    println!("Encoding complete! Output written to {}", args.output);
    println!("Encoded {} frames, output size: {} bytes", total_frames, bitstream_data.len());

    Ok(())
}

fn map_surface_nv12<'a>(
    display: &cros_codecs::libva::Display,
    surface: &'a Surface<()>,
) -> cros_codecs::libva::Image<'a> {
    let image_fmts = display.query_image_formats().unwrap();
    let image_fmt = image_fmts.into_iter().find(|f| f.fourcc == VA_FOURCC_NV12).unwrap();
    cros_codecs::libva::Image::create_from(surface, image_fmt, surface.size(), surface.size()).unwrap()
}

fn upload_nv12_frame(display: &cros_codecs::libva::Display, surface: &Surface<()>, frame_data: &[u8]) -> Result<()> {
    let mut image = map_surface_nv12(display, surface);
    let va_image = *image.image();
    let dest = image.as_mut();
    let width = WIDTH as usize;
    let height = HEIGHT as usize;

    // Copy Y plane
    let y_plane_size = width * height;
    let y_src = &frame_data[0..y_plane_size];
    let y_dst = &mut dest[va_image.offsets[0] as usize..va_image.offsets[0] as usize + y_plane_size];

    // Copy line by line to handle stride
    for row in 0..height {
        let src_offset = row * width;
        let dst_offset = row * va_image.pitches[0] as usize;
        let src_line = &y_src[src_offset..src_offset + width];
        let dst_line = &mut y_dst[dst_offset..dst_offset + width];
        dst_line.copy_from_slice(src_line);
    }

    // Copy UV plane
    let uv_plane_size = width * height / 2;
    let uv_src = &frame_data[y_plane_size..y_plane_size + uv_plane_size];
    let uv_dst = &mut dest[va_image.offsets[1] as usize..va_image.offsets[1] as usize + uv_plane_size];

    // Copy line by line to handle stride
    for row in 0..height/2 {
        let src_offset = row * width;
        let dst_offset = row * va_image.pitches[1] as usize;
        let src_line = &uv_src[src_offset..src_offset + width];
        let dst_line = &mut uv_dst[dst_offset..dst_offset + width];
        dst_line.copy_from_slice(src_line);
    }

    Ok(())
}
