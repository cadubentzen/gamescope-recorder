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

use capture::{Capturer, CapturerConfig};

const FPS: u32 = 60;

fn main() -> anyhow::Result<()> {
    let encoder: Option<encode::Encoder> = None;
    let mut output_file = File::create("output.bgrx")?;

    let capturer = Capturer::new(CapturerConfig { frame_rate: FPS })?;
    let running = Arc::new(AtomicBool::new(true));

    ctrlc::set_handler({
        let running = running.clone();
        move || {
            println!("Received Ctrl+C!");
            running.store(false, Ordering::SeqCst);
        }
    })
    .expect("Error setting Ctrl+C handler");

    let mut captured_count = 0;
    let encoded_count = 0;

    let frame_duration = Duration::from_secs_f64(1.0 / FPS as f64);
    let start = Instant::now();
    let mut next_frame_time = start + frame_duration;
    while running.load(Ordering::SeqCst) {
        // Get last frame from the capturer
        if let Some(frame) = capturer.last_frame() {
            captured_count += 1;
            // Print progress every 60 frames
            if captured_count % 60 == 0 {
                print!(".");
                std::io::stdout().flush().expect("Failed to flush stdout");
            }

            output_file
                .write_all(frame.data())
                .expect("Failed to write to output file");
        } else {
            eprintln!("No frame received, skipping...");
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
