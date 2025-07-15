#!/bin/bash
set -e

echo "=== Rate Control Mode Comparison ==="
echo "Encoding with different rate control modes and analyzing results..."
echo

# Encode with both modes
echo "1. Encoding with CBR mode..."
just encode-cros-codecs-cbr

echo "2. Encoding with VBR mode..."
just encode-cros-codecs-vbr

echo "3. Running VMAF analysis..."
echo

# Run VMAF tests
echo "CBR VMAF:"
just vmaf-cros-codecs-cbr | tail -1

echo "VBR VMAF:"
just vmaf-cros-codecs-vbr | tail -1

echo

# Analyze frame structures
echo "4. Analyzing frame structures..."
echo
python3 analyze_video.py output_cros_codecs_cbr.mp4 output_cros_codecs_vbr.mp4

echo
echo "=== Analysis Complete ==="
echo "Files generated:"
echo "  - output_cros_cbr.h264 (raw H.264 CBR)"
echo "  - output_cros_vbr.h264 (raw H.264 VBR)"
echo "  - output_cros_codecs_cbr.mp4 (MP4 CBR)"
echo "  - output_cros_codecs_vbr.mp4 (MP4 VBR)"