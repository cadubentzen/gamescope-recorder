# Input/Output files
raw_input := "-f rawvideo -pixel_format nv12 -video_size 1280x720 -framerate 60"
raw_scaled_input := "-f rawvideo -pixel_format nv12 -video_size 640x360 -framerate 60"
raw_file := "output.nv12"
libx264_file := "output.mp4"
vaapi_file := "output_vaapi.mp4"
vaapi_file_cbr := "output_vaapi_cbr.mp4"
vaapi_file_cbr_no_idr := "output_vaapi_cbr_no_idr.mp4"
cros_codecs_file := "output_cros_codecs.mp4"
cros_codecs_scaled_file := "output_cros_codecs_scaled.mp4"
cros_codecs_cbr_file := "output_cros_codecs_cbr.mp4"
cros_codecs_vbr_file := "output_cros_codecs_vbr.mp4"
cros_h264_file := "output_cros.h264"
cros_h264_cbr_file := "output_cros_cbr.h264"
cros_h264_vbr_file := "output_cros_vbr.h264"

# Encoding settings
bitrate := "4M"
maxrate := "6M"
bufsize := "2M"
cbr_bufsize := "500K"
fps := "60"
gop_size := "30"
keyint := "30"

# Hardware settings
vaapi_device := "/dev/dri/renderD128"

# Remote deployment settings
remote_host := "steamdeck"
remote_path := "~/Projects/discord/tests"
binary_names := "encode-sample scale-sample"

# Common commands
ffmpeg := "ffmpeg -y -hide_banner"
cargo_encode := "cargo run --release --bin encode-sample"
cargo_scale := "cargo run --release --bin scale-sample"

# Common FFmpeg options
libx264_opts := "-c:v libx264 -preset ultrafast -tune zerolatency -bf 0 -g " + gop_size + " -keyint_min " + keyint + " -sc_threshold 0 -pix_fmt yuv420p"
vaapi_opts := "-vaapi_device " + vaapi_device + " -vf 'format=nv12,hwupload' -c:v h264_vaapi -bf 0 -g " + gop_size + " -idr_interval " + keyint
vmaf_filter := "-lavfi libvmaf -f null -"

# Input dimensions
input_width := "1280"
input_height := "720"
output_width := "640"
output_height := "360"

play-raw:
    ffplay {{raw_input}} {{raw_file}}

encode-ffmpeg-libx264:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} {{libx264_opts}} -b:v {{bitrate}} -maxrate {{maxrate}} -bufsize {{bufsize}} {{libx264_file}}

encode-ffmpeg-vaapi:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} {{vaapi_opts}} -b:v {{bitrate}} -maxrate {{maxrate}} -bufsize {{bufsize}} -rc_mode VBR {{vaapi_file}}

encode-ffmpeg-vaapi-cbr:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} {{vaapi_opts}} -b:v {{bitrate}} -bufsize {{cbr_bufsize}} -rc_mode CBR {{vaapi_file_cbr}}

encode-cros-codecs:
    {{cargo_encode}} -- --input {{raw_file}} --output {{cros_h264_file}} --bitrate {{bitrate}} --maxrate {{maxrate}} --rc-mode cbr
    MP4Box -add {{cros_h264_file}}:fps={{fps}} -new {{cros_codecs_file}}

encode-cros-codecs-cbr:
    {{cargo_encode}} -- --input {{raw_file}} --output {{cros_h264_cbr_file}} --bitrate {{bitrate}} --maxrate {{maxrate}} --rc-mode cbr
    MP4Box -add {{cros_h264_cbr_file}}:fps={{fps}} -new {{cros_codecs_cbr_file}}

encode-cros-codecs-vbr:
    {{cargo_encode}} -- --input {{raw_file}} --output {{cros_h264_vbr_file}} --bitrate {{bitrate}} --maxrate {{maxrate}} --rc-mode vbr
    MP4Box -add {{cros_h264_vbr_file}}:fps={{fps}} -new {{cros_codecs_vbr_file}}

scale:
    {{cargo_scale}} -- --input {{raw_file}} --output {{cros_h264_file}} --input-width {{input_width}} --input-height {{input_height}} --output-width {{output_width}} --output-height {{output_height}} --bitrate {{bitrate}} --maxrate {{maxrate}} --rc-mode cbr --format h264
    MP4Box -add {{cros_h264_file}}:fps={{fps}} -new {{cros_codecs_scaled_file}}

scale-nv12:
    {{cargo_scale}} -- --input {{raw_file}} --output scaled_output.nv12 --input-width {{input_width}} --input-height {{input_height}} --output-width {{output_width}} --output-height {{output_height}} --format nv12

play-scaled-nv12:
    ffplay {{raw_scaled_input}} scaled_output.nv12

play-ffmpeg-libx264:
    ffplay {{libx264_file}}

play-ffmpeg-vaapi:
    ffplay {{vaapi_file}}

play-ffmpeg-vaapi-cbr:
    ffplay {{vaapi_file_cbr}}

play-cros-codecs:
    ffplay {{cros_codecs_file}}

play-cros-codecs-scaled:
    ffplay {{cros_codecs_scaled_file}}

play-cros-codecs-cbr:
    ffplay {{cros_codecs_cbr_file}}

play-cros-codecs-vbr:
    ffplay {{cros_codecs_vbr_file}}

vmaf-ffmpeg-libx264:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{libx264_file}} {{vmaf_filter}}

vmaf-ffmpeg-vaapi:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{vaapi_file}} {{vmaf_filter}}

vmaf-ffmpeg-vaapi-cbr:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{vaapi_file_cbr}} {{vmaf_filter}}

vmaf-cros-codecs:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{cros_codecs_file}} {{vmaf_filter}}

vmaf-cros-codecs-cbr:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{cros_codecs_cbr_file}} {{vmaf_filter}}

vmaf-cros-codecs-vbr:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{cros_codecs_vbr_file}} {{vmaf_filter}}

vmaf-ffmpeg-vaapi-cbr-no-idr:
    {{ffmpeg}} {{raw_input}} -i {{raw_file}} -i {{vaapi_file_cbr_no_idr}} {{vmaf_filter}}

# Build binaries in release mode
build:
    cargo build --release

# Deploy binaries to remote host
deploy: build
    ssh {{remote_host}} "mkdir -p {{remote_path}}"
    rsync -avz --progress target/release/gamescope-recorder {{remote_host}}:{{remote_path}}/

# Run encode-sample on remote host (same as encode-cros-codecs, default to 50 frames)
remote-run: deploy
    ssh {{remote_host}} "cd {{remote_path}} && XDG_RUNTIME_DIR=/run/user/1000 ./gamescope-recorder"

remote-download:
    rsync -avz --progress {{remote_host}}:{{remote_path}}/output.h264 ./remote_output.h264
    MP4Box -add remote_output.h264:fps={{fps}} -new remote_output.mp4

# Play downloaded remote files
remote-play: remote-download
    ffplay remote_output.mp4

run:
    cargo run --release

play-recorded:
    MP4Box -add output.h264:fps={{fps}} -new output.mp4
    ffplay output.mp4
