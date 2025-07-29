use std::{
    fs::File,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use std::thread;
use std::time::Duration;

mod capture;
mod encode;

use capture::Capturer;
use encode::Encoder;

const FPS: u32 = 60;

fn main() -> anyhow::Result<()> {
    let mut encoder: Option<Encoder> = None;
    let mut output_file = File::create("output.h264")?;

    let capturer = Capturer::new()?;
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
        if let Some(frame) = capturer.read_frame() {
            if encoder.is_none() {
                encoder = Some(Encoder::new(FPS, &frame).expect("Failed to create encoder"));
            }
            // Encode the frame
            let encoder = encoder.as_mut().unwrap();
            encoder.encode(frame)?;
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
            next_frame_time += frame_duration;
        } else {
            // If we are behind schedule, skip to the next frame time
            next_frame_time = now + frame_duration;
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
