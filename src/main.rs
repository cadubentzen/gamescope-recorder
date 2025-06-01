use std::{
    fs::File,
    io::Write,
    os::fd::{FromRawFd, RawFd},
};

use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use nix::unistd::dup;

mod capture;
mod encode;

fn main() -> anyhow::Result<()> {
    let mut encoder: Option<encode::Encoder> = None;
    let mut output_file = File::create("output.h264")?;
    let capturer = capture::Capturer::new(move |format, mut buffer| {
        
        if encoder.is_none() {
            let size = format.size();
            let mut framerate = format.framerate().num;
            if framerate == 0 {
                framerate = 30;
            }
            encoder = Some(
                encode::Encoder::new(size.width, size.height, framerate)
                    .expect("Failed to create encoder"),
            );
        }
        let encoder = encoder.as_mut().unwrap();
        let datas = buffer.datas_mut();
        if datas.is_empty() {
            eprintln!("No data in pipewire buffer");
            return;
        }
        let data = &mut datas[0];
        let fd = RawFd::from(data.fd().unwrap() as i32);
        let frame = GenericDmaVideoFrame::new(
            vec![unsafe { File::from_raw_fd(dup(fd).unwrap()) }],
            encoder.frame_layout.clone(),
        ).unwrap();
        encoder.encode(frame).unwrap();
        let output = encoder.poll().expect("Failed to poll encoder");
        if let Some(bitstream) = output {
            output_file
                .write_all(&bitstream.bitstream)
                .expect("Failed to write to output file");
        }
    })?;
    capturer.run();
    // FIXME drain the encoder at the end
    
    Ok(())
}
