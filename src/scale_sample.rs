use anyhow::{bail, Result};
use clap::Parser;
use cros_codecs::{
    backend::vaapi::surface_pool::VaSurfacePool,
    codec::h264::parser::{Level, Profile},
    decoder::FramePool,
    encoder::{
        h264::{EncoderConfig, H264},
        stateless::StatelessEncoder,
        FrameMetadata, PredictionStructure, RateControl, Tunings, VideoEncoder,
    },
    libva::{Surface, UsageHint, VA_RT_FORMAT_YUV420, VA_FOURCC_NV12},
    BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution,
};
use std::{
    borrow::Borrow,
    fs::File,
    io::{Read, Write},
    time::Instant,
};

mod vaapi_scaler;

#[derive(Parser)]
#[command(name = "scale-sample")]
#[command(about = "Scale raw NV12 frames using VAAPI")]
struct Args {
    /// Input raw NV12 file
    #[arg(long)]
    input: String,

    /// Output raw NV12 file
    #[arg(long)]
    output: String,

    /// Input width
    #[arg(long)]
    input_width: u32,

    /// Input height
    #[arg(long)]
    input_height: u32,

    /// Output width
    #[arg(long)]
    output_width: u32,

    /// Output height
    #[arg(long)]
    output_height: u32,

    /// Target bitrate (e.g., "4M", "2000K") - required for H264 format
    #[arg(long)]
    bitrate: Option<String>,

    /// Maximum bitrate (e.g., "6M", "3000K") - required for H264 format
    #[arg(long)]
    maxrate: Option<String>,

    /// Rate control mode
    #[arg(long, value_enum, default_value = "cbr")]
    rc_mode: RcMode,

    /// Output format (h264 for encoded, nv12 for raw frames)
    #[arg(long, value_enum, default_value = "h264")]
    format: OutputFormat,

    /// Maximum number of frames to process (optional, processes all frames if not specified)
    #[arg(long)]
    frames: Option<usize>,
}

#[derive(clap::ValueEnum, Clone)]
enum OutputFormat {
    H264,
    Nv12,
}

#[derive(clap::ValueEnum, Clone)]
enum RcMode {
    Cbr,
    Vbr,
}

fn parse_bitrate(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty bitrate string".to_string());
    }

    let (number_part, suffix) = if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len() - 1], 1_000_000)
    } else if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len() - 1], 1_000)
    } else {
        (s, 1)
    };

    let number: u64 = number_part
        .parse()
        .map_err(|_| format!("Invalid number: {}", number_part))?;

    Ok(number * suffix)
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Starting NV12 frame scaling + encoding using VAAPI...");
    println!("Input: {}", args.input);
    println!("Output: {}", args.output);
    println!(
        "Input resolution: {}x{}",
        args.input_width, args.input_height
    );
    println!(
        "Output resolution: {}x{}",
        args.output_width, args.output_height
    );

    let input_frame_size = (args.input_width * args.input_height * 3 / 2) as usize;

    // Parse bitrates (required for H264 format)
    let (bitrate, maxrate) = if matches!(args.format, OutputFormat::H264) {
        let bitrate_str = args
            .bitrate
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--bitrate is required for H264 format"))?;
        let maxrate_str = args
            .maxrate
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--maxrate is required for H264 format"))?;

        let bitrate =
            parse_bitrate(bitrate_str).map_err(|e| anyhow::anyhow!("Invalid bitrate: {}", e))?;
        let maxrate =
            parse_bitrate(maxrate_str).map_err(|e| anyhow::anyhow!("Invalid maxrate: {}", e))?;

        (bitrate, maxrate)
    } else {
        (0, 0) // Dummy values for NV12 mode
    };

    // Open the raw NV12 file
    let mut input_file = File::open(&args.input)?;

    // Get file size and calculate total frames
    let file_size = input_file.metadata()?.len() as usize;
    let available_frames = file_size / input_frame_size;
    let total_frames = args
        .frames
        .unwrap_or(available_frames)
        .min(available_frames);
    println!(
        "Input file size: {} bytes, estimated frames: {}, processing: {}",
        file_size, available_frames, total_frames
    );

    // Initialize VAAPI display
    let Some(display) = cros_codecs::libva::Display::open() else {
        bail!("Failed to open VAAPI display");
    };

    // Create reusable scaler
    let scaler = vaapi_scaler::VaapiScaler::new(display.clone())?;

    let fourcc = Fourcc::from(b"NV12");

    // Create encoder only if output format is H264
    let mut encoder = if matches!(args.format, OutputFormat::H264) {
        let rate_control = match args.rc_mode {
            RcMode::Cbr => RateControl::ConstantBitrate(bitrate),
            RcMode::Vbr => RateControl::VariableBitrate {
                target_bitrate: bitrate,
                max_bitrate: maxrate,
            },
        };

        let encoder_config = EncoderConfig {
            resolution: Resolution {
                width: args.output_width,
                height: args.output_height,
            },
            profile: Profile::High,
            level: Level::L4_1,
            pred_structure: PredictionStructure::LowDelay { limit: 30 },
            initial_tunings: Tunings {
                rate_control,
                framerate: 60,
                min_quality: 0,
                max_quality: u32::MAX,
            },
        };

        Some(
            StatelessEncoder::<H264, _, _>::new_native_vaapi(
                display.clone(),
                encoder_config,
                fourcc,
                Resolution {
                    width: args.output_width,
                    height: args.output_height,
                },
                false, // low_power
                BlockingMode::NonBlocking,
            )
            .map_err(|e| anyhow::anyhow!("Failed to create encoder: {:?}", e))?,
        )
    } else {
        None
    };

    let frame_layout = if matches!(args.format, OutputFormat::H264) {
        Some(FrameLayout {
            format: (fourcc, 0),
            size: Resolution {
                width: args.output_width,
                height: args.output_height,
            },
            planes: vec![
                PlaneLayout {
                    buffer_index: 0,
                    offset: 0,
                    stride: args.output_width as usize,
                },
                PlaneLayout {
                    buffer_index: 0,
                    offset: (args.output_width * args.output_height) as usize,
                    stride: args.output_width as usize,
                },
            ],
        })
    } else {
        None
    };

    // Create surface pool for input resolution with VPP read hint
    let mut src_pool = VaSurfacePool::<()>::new(
        display.clone(),
        VA_RT_FORMAT_YUV420,
        Some(UsageHint::USAGE_HINT_VPP_READ),
        Resolution {
            width: args.input_width,
            height: args.input_height,
        },
    );
    src_pool.add_frames(vec![(); 1])?; // Only need 1 surface for input

    // Create surface pool for output resolution
    let usage_hint = match args.format {
        OutputFormat::H264 => Some(UsageHint::USAGE_HINT_ENCODER | UsageHint::USAGE_HINT_VPP_WRITE),
        OutputFormat::Nv12 => Some(UsageHint::USAGE_HINT_VPP_WRITE),
    };

    let mut dst_pool = VaSurfacePool::<()>::new(
        display.clone(),
        VA_RT_FORMAT_YUV420,
        usage_hint,
        Resolution {
            width: args.output_width,
            height: args.output_height,
        },
    );
    dst_pool.add_frames(vec![(); 16])?;

    // Create output file
    let mut output_file = File::create(&args.output)?;

    let action = match args.format {
        OutputFormat::H264 => "Encoding",
        OutputFormat::Nv12 => "Scaling",
    };
    println!("{} {} frames...", action, total_frames);

    // Frame buffer for reading one frame at a time
    let mut frame_buffer = vec![0u8; input_frame_size];

    // Output buffer for NV12 mode
    let output_frame_size = (args.output_width * args.output_height * 3 / 2) as usize;
    let mut output_buffer = if matches!(args.format, OutputFormat::Nv12) {
        Some(vec![0u8; output_frame_size])
    } else {
        None
    };

    // Timing variables
    let mut total_upload_time = std::time::Duration::ZERO;
    let mut total_scale_time = std::time::Duration::ZERO;
    let mut total_download_time = std::time::Duration::ZERO;

    // Process each frame
    for frame_idx in 0..total_frames {
        // Read one frame from file
        match input_file.read_exact(&mut frame_buffer) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                println!("Reached end of file at frame {}", frame_idx);
                break;
            }
            Err(e) => return Err(e.into()),
        }

        println!("Processing frame {}/{}", frame_idx + 1, total_frames);

        // Get a surface from the input pool
        let src_pooled_surface = src_pool
            .get_surface()
            .ok_or_else(|| anyhow::anyhow!("Failed to get source surface from pool"))?;

        // Upload frame data to source surface
        let src_surface: &Surface<()> = src_pooled_surface.borrow();
        let upload_start = Instant::now();
        upload_nv12_frame(
            &display,
            src_surface,
            &frame_buffer,
            args.input_width,
            args.input_height,
        )?;
        let upload_time = upload_start.elapsed();
        total_upload_time += upload_time;

        // Get a surface from the output pool for the scaled output
        let dst_pooled_surface = dst_pool
            .get_surface()
            .ok_or_else(|| anyhow::anyhow!("Failed to get destination surface from pool"))?;

        // Scale the frame from input resolution to output resolution
        let dst_surface: &Surface<()> = dst_pooled_surface.borrow();
        let scale_start = Instant::now();
        match args.format {
            OutputFormat::H264 => scaler.scale(src_surface, dst_surface)?,
            OutputFormat::Nv12 => scaler.scale_sync(src_surface, dst_surface)?,
        }
        let scale_time = scale_start.elapsed();
        total_scale_time += scale_time;

        match args.format {
            OutputFormat::H264 => {
                // Create frame metadata
                let meta = FrameMetadata {
                    timestamp: frame_idx as u64,
                    layout: frame_layout.as_ref().unwrap().clone(),
                    force_keyframe: frame_idx == 0, // Force keyframe for first frame
                };

                // Encode the scaled frame
                if let Some(ref mut enc) = encoder {
                    enc.encode(meta, dst_pooled_surface)
                        .map_err(|e| anyhow::anyhow!("Failed to encode frame: {:?}", e))?;

                    // Poll for encoded data
                    while let Some(coded_buffer) = enc
                        .poll()
                        .map_err(|e| anyhow::anyhow!("Failed to poll encoder: {:?}", e))?
                    {
                        output_file.write_all(&coded_buffer.bitstream)?;
                    }
                }
            }
            OutputFormat::Nv12 => {
                // Download the scaled frame as raw NV12
                if let Some(ref mut buf) = output_buffer {
                    buf.fill(0); // Clear buffer
                    let download_start = Instant::now();
                    download_nv12_frame(
                        &display,
                        dst_surface,
                        buf,
                        args.output_width,
                        args.output_height,
                    )?;
                    let download_time = download_start.elapsed();
                    total_download_time += download_time;
                    output_file.write_all(buf)?;
                }
            }
        }

        if frame_idx % 30 == 0 {
            println!("Processed frame {}/{}", frame_idx + 1, total_frames);
        }
    }

    // Drain encoder (only for H264 mode)
    if let Some(ref mut enc) = encoder {
        enc.drain()
            .map_err(|e| anyhow::anyhow!("Failed to drain encoder: {:?}", e))?;

        // Get remaining encoded data
        while let Some(coded_buffer) = enc
            .poll()
            .map_err(|e| anyhow::anyhow!("Failed to poll encoder: {:?}", e))?
        {
            output_file.write_all(&coded_buffer.bitstream)?;
        }
    }

    match args.format {
        OutputFormat::H264 => println!("Scaling and encoding completed successfully!"),
        OutputFormat::Nv12 => println!("Scaling completed successfully!"),
    }

    // Print timing summary
    let frames_processed = total_frames;
    println!("\n=== Timing Summary ===");
    println!("Total frames processed: {}", frames_processed);
    println!(
        "Upload time:   {:.2}ms total, {:.3}ms avg per frame",
        total_upload_time.as_secs_f64() * 1000.0,
        total_upload_time.as_secs_f64() * 1000.0 / frames_processed as f64
    );
    println!(
        "Scale time:    {:.2}ms total, {:.3}ms avg per frame",
        total_scale_time.as_secs_f64() * 1000.0,
        total_scale_time.as_secs_f64() * 1000.0 / frames_processed as f64
    );
    if matches!(args.format, OutputFormat::Nv12) {
        println!(
            "Download time: {:.2}ms total, {:.3}ms avg per frame",
            total_download_time.as_secs_f64() * 1000.0,
            total_download_time.as_secs_f64() * 1000.0 / frames_processed as f64
        );
    }

    Ok(())
}

fn map_surface_nv12<'a>(
    display: &cros_codecs::libva::Display,
    surface: &'a Surface<()>,
) -> cros_codecs::libva::Image<'a> {
    let image_fmts = display.query_image_formats().unwrap();
    let image_fmt = image_fmts
        .into_iter()
        .find(|f| f.fourcc == VA_FOURCC_NV12)
        .unwrap();
    cros_codecs::libva::Image::create_from(surface, image_fmt, surface.size(), surface.size())
        .unwrap()
}

fn upload_nv12_frame(
    display: &cros_codecs::libva::Display,
    surface: &Surface<()>,
    frame_data: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let mut image = map_surface_nv12(display, surface);
    let va_image = *image.image();
    let dest = image.as_mut();
    let width = width as usize;
    let height = height as usize;

    // Copy Y plane
    let y_plane_size = width * height;
    let y_src = &frame_data[0..y_plane_size];
    let y_dst =
        &mut dest[va_image.offsets[0] as usize..va_image.offsets[0] as usize + y_plane_size];

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
    let uv_dst =
        &mut dest[va_image.offsets[1] as usize..va_image.offsets[1] as usize + uv_plane_size];

    // Copy line by line to handle stride
    for row in 0..height / 2 {
        let src_offset = row * width;
        let dst_offset = row * va_image.pitches[1] as usize;
        let src_line = &uv_src[src_offset..src_offset + width];
        let dst_line = &mut uv_dst[dst_offset..dst_offset + width];
        dst_line.copy_from_slice(src_line);
    }

    Ok(())
}

fn download_nv12_frame(
    display: &cros_codecs::libva::Display,
    surface: &Surface<()>,
    frame_data: &mut [u8],
    width: u32,
    height: u32,
) -> Result<()> {
    let image = map_surface_nv12(display, surface);
    let va_image = *image.image();
    let src = image.as_ref();
    let width = width as usize;
    let height = height as usize;

    // Copy Y plane - use stride-aware copying
    let y_plane_size = width * height;
    let y_dst = &mut frame_data[0..y_plane_size];

    for row in 0..height {
        let src_row_start = va_image.offsets[0] as usize + row * va_image.pitches[0] as usize;
        let dst_row_start = row * width;

        if src_row_start + width <= src.len() && dst_row_start + width <= y_dst.len() {
            let src_row = &src[src_row_start..src_row_start + width];
            let dst_row = &mut y_dst[dst_row_start..dst_row_start + width];
            dst_row.copy_from_slice(src_row);
        }
    }

    // Copy UV plane - use stride-aware copying
    let uv_plane_size = width * height / 2;
    let uv_dst = &mut frame_data[y_plane_size..y_plane_size + uv_plane_size];

    for row in 0..height / 2 {
        let src_row_start = va_image.offsets[1] as usize + row * va_image.pitches[1] as usize;
        let dst_row_start = row * width;

        if src_row_start + width <= src.len() && dst_row_start + width <= uv_dst.len() {
            let src_row = &src[src_row_start..src_row_start + width];
            let dst_row = &mut uv_dst[dst_row_start..dst_row_start + width];
            dst_row.copy_from_slice(src_row);
        }
    }

    Ok(())
}

