use std::{
    fs::File,
    io::Write,
    os::fd::{FromRawFd, RawFd},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use std::thread;
use std::time::Duration;

use cros_codecs::{
    backend::vaapi::encoder, video_frame::generic_dma_video_frame::GenericDmaVideoFrame,
};
use nix::unistd::dup;

mod capture;
mod encode;

const FPS: u32 = 60;

fn main() -> anyhow::Result<()> {
    let mut encoder: Option<encode::Encoder> = None;
    let mut output_file = File::create("output.h264")?;

    let capturer = capture::Capturer::new()?;
    let running = Arc::new(AtomicBool::new(true));

    ctrlc::set_handler({
        let running = running.clone();
        move || {
            println!("Received Ctrl+C!");
            running.store(false, Ordering::SeqCst);
        }
    })
    .expect("Error setting Ctrl+C handler");

    let mut frame_count = 0;
    let frame_duration = Duration::from_secs_f64(1.0 / FPS as f64);
    let start = Instant::now();
    let mut next_frame_time = start + frame_duration;
    while running.load(Ordering::SeqCst) {
        // Get last frame from the capturer
        if let Some(frame) = capturer.last_frame() {
            if encoder.is_none() {
                encoder = Some(
                    encode::Encoder::new(frame.width(), frame.height(), FPS)
                        .expect("Failed to create encoder"),
                );
            }
            // Encode the frame
            let encoder = encoder.as_mut().unwrap();
            encoder.encode(frame.dmabuf())?;
        } else {
            eprintln!("No frame captured");
        }

        // Write the encoded frame to the output file
        if let Some(encoder) = &mut encoder {
            while let Some(bitstream) = encoder.poll()? {
                frame_count += 1;
                if frame_count % 60 == 0 {
                    print!(".");
                    std::io::stdout().flush().expect("Failed to flush stdout");
                }
                output_file
                    .write_all(&bitstream.bitstream)
                    .expect("Failed to write to output file");
            }
        }

        // Wait 1/60s-processing_time before capturing the next frame
        let now = Instant::now();
        if next_frame_time >= now {
            thread::sleep(next_frame_time - now);
        }
        next_frame_time += frame_duration;
    }
    // Drain the encoder and write any remaining frames to the output file
    println!("\nDraining encoder...");
    if let Some(mut encoder) = encoder {
        encoder.drain()?;
        while let Some(bitstream) = encoder.poll()? {
            output_file
                .write_all(&bitstream.bitstream)
                .expect("Failed to write to output file");
        }
    }

    Ok(())
}
