#!/usr/bin/env python3
"""
Video Analysis Script
Analyzes GOP structure and per-frame bitrate of H.264 videos
"""

import json
import subprocess
import sys
import argparse
from pathlib import Path
import struct
import io

def run_ffprobe(video_path, show_type):
    """Run ffprobe to extract frame or packet information"""
    cmd = [
        'ffprobe',
        '-v', 'quiet',
        '-print_format', 'json',
        '-show_entries', show_type,
        str(video_path)
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return json.loads(result.stdout)
    except subprocess.CalledProcessError as e:
        print(f"Error running ffprobe: {e}")
        return None
    except json.JSONDecodeError as e:
        print(f"Error parsing ffprobe output: {e}")
        return None

def analyze_gop_structure(frames):
    """Analyze GOP structure from frame data"""
    gops = []
    current_gop = []
    
    for i, frame in enumerate(frames):
        frame_type = frame.get('pict_type', 'Unknown')
        
        if frame_type == 'I':
            if current_gop:  # Save previous GOP
                gops.append(current_gop)
            current_gop = [{'index': i, 'type': frame_type, 'pts': frame.get('pts', 'N/A')}]
        else:
            current_gop.append({'index': i, 'type': frame_type, 'pts': frame.get('pts', 'N/A')})
    
    if current_gop:  # Add last GOP
        gops.append(current_gop)
    
    return gops

def analyze_bitrate(packets):
    """Analyze per-frame bitrate from packet data"""
    frame_sizes = []
    total_size = 0
    
    for packet in packets:
        if packet.get('codec_type') == 'video':
            size = int(packet.get('size', 0))
            frame_sizes.append(size)
            total_size += size
    
    return frame_sizes, total_size

def print_gop_analysis(gops, framerate=60):
    """Print GOP structure analysis"""
    print("=== GOP Structure Analysis ===")
    print(f"Total GOPs: {len(gops)}")
    
    gop_sizes = [len(gop) for gop in gops]
    if gop_sizes:
        print(f"GOP sizes: min={min(gop_sizes)}, max={max(gop_sizes)}, avg={sum(gop_sizes)/len(gop_sizes):.1f}")
        print(f"GOP duration: min={min(gop_sizes)/framerate:.2f}s, max={max(gop_sizes)/framerate:.2f}s")
    
    print("\nFirst 5 GOPs:")
    for i, gop in enumerate(gops[:5]):
        frame_types = [f['type'] for f in gop]
        print(f"  GOP {i+1}: {' '.join(frame_types)} (size: {len(gop)})")
    
    if len(gops) > 5:
        print("  ...")

def print_bitrate_analysis(frame_sizes, framerate=60):
    """Print bitrate analysis"""
    print("\n=== Bitrate Analysis ===")
    
    if not frame_sizes:
        print("No frame data available")
        return
    
    # Convert bytes to bits
    frame_bits = [size * 8 for size in frame_sizes]
    
    total_bits = sum(frame_bits)
    duration = len(frame_sizes) / framerate
    avg_bitrate = total_bits / duration if duration > 0 else 0
    
    print(f"Total frames: {len(frame_sizes)}")
    print(f"Duration: {duration:.2f}s")
    print(f"Average bitrate: {avg_bitrate/1000:.0f} kbps ({avg_bitrate/1000000:.2f} Mbps)")
    
    # Frame size statistics
    print(f"Frame sizes (bytes): min={min(frame_sizes)}, max={max(frame_sizes)}, avg={sum(frame_sizes)/len(frame_sizes):.0f}")
    
    # Bitrate variation
    max_frame_bitrate = max(frame_bits) * framerate
    min_frame_bitrate = min(frame_bits) * framerate
    print(f"Peak frame bitrate: {max_frame_bitrate/1000000:.2f} Mbps")
    print(f"Min frame bitrate: {min_frame_bitrate/1000000:.2f} Mbps")
    
    # Show largest frames (likely I-frames)
    sorted_frames = sorted(enumerate(frame_sizes), key=lambda x: x[1], reverse=True)
    print(f"\nLargest frames:")
    for i, (frame_idx, size) in enumerate(sorted_frames[:5]):
        bitrate_mbps = (size * 8 * framerate) / 1000000
        print(f"  Frame {frame_idx}: {size} bytes ({bitrate_mbps:.2f} Mbps equivalent)")

def extract_raw_h264_stream(video_path):
    """Extract raw H.264 stream from video file"""
    cmd = [
        'ffmpeg',
        '-i', str(video_path),
        '-c:v', 'copy',
        '-an',
        '-f', 'h264',
        '-'
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, check=True)
        return result.stdout
    except subprocess.CalledProcessError as e:
        print(f"Error extracting H.264 stream: {e}")
        return None

def extract_h264_headers(video_path):
    """Extract detailed H.264 SPS/PPS/Slice headers using ffmpeg trace_headers"""
    cmd = [
        'ffmpeg',
        '-i', str(video_path),
        '-c:v', 'copy',
        '-bsf:v', 'trace_headers',
        '-f', 'null', '-'
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return result.stderr  # trace_headers outputs to stderr
    except subprocess.CalledProcessError as e:
        print(f"Error extracting H.264 headers: {e}")
        return ""

def parse_sps_parameters(trace_output):
    """Parse SPS parameters from trace_headers output"""
    sps_params = {}
    
    # Look for SPS section
    lines = trace_output.split('\n')
    in_sps = False
    
    for line in lines:
        if 'Sequence Parameter Set' in line:
            in_sps = True
            continue
        elif 'Picture Parameter Set' in line or 'Slice Header' in line:
            in_sps = False
            continue
        
        if in_sps and '=' in line:
            # Parse parameter lines like: "profile_idc                                          01100100 = 100"
            parts = line.split('=')
            if len(parts) == 2:
                param_line = parts[0].strip()
                value = parts[1].strip()
                
                # Extract parameter name (last word before the binary/hex value)
                param_parts = param_line.split()
                if param_parts:
                    param_name = param_parts[-1]
                    sps_params[param_name] = value
    
    return sps_params

def parse_pps_parameters(trace_output):
    """Parse PPS parameters from trace_headers output"""
    pps_params = {}
    
    lines = trace_output.split('\n')
    in_pps = False
    
    for line in lines:
        if 'Picture Parameter Set' in line:
            in_pps = True
            continue
        elif 'Slice Header' in line or 'Sequence Parameter Set' in line:
            in_pps = False
            continue
        
        if in_pps and '=' in line:
            parts = line.split('=')
            if len(parts) == 2:
                param_line = parts[0].strip()
                value = parts[1].strip()
                
                param_parts = param_line.split()
                if param_parts:
                    param_name = param_parts[-1]
                    pps_params[param_name] = value
    
    return pps_params

def parse_slice_headers(trace_output):
    """Parse first few slice headers from trace_headers output"""
    slice_headers = []
    
    lines = trace_output.split('\n')
    current_slice = {}
    in_slice = False
    
    for line in lines:
        if 'Slice Header' in line:
            if current_slice:
                slice_headers.append(current_slice)
            current_slice = {}
            in_slice = True
            continue
        elif ('Picture Parameter Set' in line or 'Sequence Parameter Set' in line) and current_slice:
            slice_headers.append(current_slice)
            current_slice = {}
            in_slice = False
            continue
        
        if in_slice and '=' in line:
            parts = line.split('=')
            if len(parts) == 2:
                param_line = parts[0].strip()
                value = parts[1].strip()
                
                param_parts = param_line.split()
                if param_parts:
                    param_name = param_parts[-1]
                    current_slice[param_name] = value
        
        # Limit to first 5 slice headers to avoid too much output
        if len(slice_headers) >= 5:
            break
    
    if current_slice and len(slice_headers) < 5:
        slice_headers.append(current_slice)
    
    return slice_headers

def compare_h264_headers(video_files):
    """Compare H.264 headers between multiple video files"""
    results = {}
    
    for video_path in video_files:
        path = Path(video_path)
        if not path.exists():
            print(f"Error: File {video_path} not found")
            continue
        
        print(f"\nExtracting H.264 headers for: {path.name}")
        trace_output = extract_h264_headers(path)
        
        if trace_output:
            sps = parse_sps_parameters(trace_output)
            pps = parse_pps_parameters(trace_output)
            slices = parse_slice_headers(trace_output)
            
            results[path.name] = {
                'sps': sps,
                'pps': pps,
                'slices': slices
            }
    
    return results

def print_header_comparison(header_results):
    """Print detailed header comparison"""
    if len(header_results) < 2:
        print("Need at least 2 files to compare")
        return
    
    file_names = list(header_results.keys())
    
    print("\n" + "="*80)
    print("H.264 HEADER COMPARISON")
    print("="*80)
    
    # SPS Comparison
    print(f"\n{'SPS PARAMETERS':<50} {file_names[0]:<15} {file_names[1]:<15}")
    print("-" * 80)
    
    all_sps_params = set()
    for file_data in header_results.values():
        all_sps_params.update(file_data['sps'].keys())
    
    for param in sorted(all_sps_params):
        val1 = header_results[file_names[0]]['sps'].get(param, 'N/A')
        val2 = header_results[file_names[1]]['sps'].get(param, 'N/A')
        
        # Highlight differences
        marker = " *** DIFF ***" if val1 != val2 else ""
        print(f"{param:<50} {val1:<15} {val2:<15}{marker}")
    
    # PPS Comparison
    print(f"\n{'PPS PARAMETERS':<50} {file_names[0]:<15} {file_names[1]:<15}")
    print("-" * 80)
    
    all_pps_params = set()
    for file_data in header_results.values():
        all_pps_params.update(file_data['pps'].keys())
    
    for param in sorted(all_pps_params):
        val1 = header_results[file_names[0]]['pps'].get(param, 'N/A')
        val2 = header_results[file_names[1]]['pps'].get(param, 'N/A')
        
        marker = " *** DIFF ***" if val1 != val2 else ""
        print(f"{param:<50} {val1:<15} {val2:<15}{marker}")
    
    # First Slice Header Comparison
    print(f"\n{'FIRST SLICE HEADER':<50} {file_names[0]:<15} {file_names[1]:<15}")
    print("-" * 80)
    
    slice1 = header_results[file_names[0]]['slices'][0] if header_results[file_names[0]]['slices'] else {}
    slice2 = header_results[file_names[1]]['slices'][0] if header_results[file_names[1]]['slices'] else {}
    
    all_slice_params = set(slice1.keys()) | set(slice2.keys())
    
    for param in sorted(all_slice_params):
        val1 = slice1.get(param, 'N/A')
        val2 = slice2.get(param, 'N/A')
        
        marker = " *** DIFF ***" if val1 != val2 else ""
        print(f"{param:<50} {val1:<15} {val2:<15}{marker}")

def analyze_sps_pps_parameters(video_path):
    """Extract and analyze SPS/PPS parameters"""
    cmd = [
        'ffprobe',
        '-v', 'quiet',
        '-print_format', 'json',
        '-show_packets',
        '-select_streams', 'v:0',
        str(video_path)
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        data = json.loads(result.stdout)
        packets = data.get('packets', [])
        
        # Look for codec extradata in the first few packets
        extradata_found = False
        for packet in packets[:10]:  # Check first 10 packets
            if 'side_data_list' in packet:
                for side_data in packet['side_data_list']:
                    if side_data.get('side_data_type') == 'H.264 parameter set':
                        extradata_found = True
                        break
            if extradata_found:
                break
        
        # Alternative: use ffprobe to get stream extradata
        stream_cmd = [
            'ffprobe',
            '-v', 'quiet',
            '-print_format', 'json',
            '-show_streams',
            '-select_streams', 'v:0',
            str(video_path)
        ]
        
        stream_result = subprocess.run(stream_cmd, capture_output=True, text=True, check=True)
        stream_data = json.loads(stream_result.stdout)
        stream = stream_data.get('streams', [{}])[0]
        
        return {
            'extradata_size': stream.get('extradata_size', 'Unknown'),
            'codec_tag': stream.get('codec_tag_string', 'Unknown'),
            'time_base': stream.get('time_base', 'Unknown'),
            'avg_frame_rate': stream.get('avg_frame_rate', 'Unknown'),
            'color_range': stream.get('color_range', 'Unknown'),
            'color_space': stream.get('color_space', 'Unknown'),
            'chroma_location': stream.get('chroma_location', 'Unknown'),
        }
        
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"Error analyzing SPS/PPS: {e}")
        return {}

def parse_nal_units(h264_data):
    """Parse NAL units from H.264 stream"""
    nal_units = []
    data = io.BytesIO(h264_data)
    
    while True:
        # Look for start code (0x000001 or 0x00000001)
        start_pos = data.tell()
        chunk = data.read(4)
        if len(chunk) < 4:
            break
            
        # Check for 4-byte start code
        if chunk == b'\x00\x00\x00\x01':
            start_code_len = 4
        elif chunk[:3] == b'\x00\x00\x01':
            # 3-byte start code, seek back one byte
            data.seek(-1, 1)
            start_code_len = 3
        else:
            # No start code found at this position, advance by 1 byte
            data.seek(start_pos + 1)
            continue
        
        # Find next start code to determine NAL unit length
        nal_start = data.tell()
        next_start = None
        
        while True:
            pos = data.tell()
            chunk = data.read(4)
            if len(chunk) < 3:
                # End of stream
                next_start = len(h264_data)
                break
            
            if (chunk[:4] == b'\x00\x00\x00\x01' or 
                chunk[:3] == b'\x00\x00\x01'):
                next_start = pos
                break
            
            data.seek(pos + 1)
        
        # Extract NAL unit
        data.seek(nal_start)
        nal_size = next_start - nal_start
        nal_data = data.read(nal_size)
        
        if nal_data:
            nal_type = nal_data[0] & 0x1F
            nal_units.append({
                'type': nal_type,
                'data': nal_data,
                'size': nal_size,
                'offset': nal_start
            })
        
        data.seek(next_start)
    
    return nal_units

def parse_slice_header(nal_data):
    """Parse slice header to extract reference frame information"""
    if len(nal_data) < 2:
        return None
    
    nal_type = nal_data[0] & 0x1F
    
    # Only process slice NAL units (types 1-5)
    if nal_type not in [1, 2, 3, 4, 5]:
        return None
    
    # Create bit reader
    bit_data = bytearray()
    for byte in nal_data[1:]:  # Skip NAL header
        bit_data.append(byte)
    
    try:
        # Parse slice header (simplified)
        slice_info = {
            'nal_type': nal_type,
            'slice_type': None,
            'frame_num': None,
            'idr_pic_id': None if nal_type != 5 else 0,
            'is_reference': nal_type in [1, 2, 5]  # Non-disposable reference pictures
        }
        
        # Determine slice type from NAL type
        if nal_type == 5:  # IDR slice
            slice_info['slice_type'] = 'I'
        elif nal_type == 1:  # Non-IDR slice
            slice_info['slice_type'] = 'P/B'  # Would need full parsing to distinguish
        
        return slice_info
        
    except Exception as e:
        return None

def analyze_reference_frames(nal_units):
    """Analyze reference frame usage from NAL units"""
    frame_info = []
    sps_found = False
    pps_found = False
    
    for nal in nal_units:
        nal_type = nal['type']
        
        if nal_type == 7:  # SPS
            sps_found = True
        elif nal_type == 8:  # PPS
            pps_found = True
        elif nal_type in [1, 2, 3, 4, 5]:  # Slice
            slice_info = parse_slice_header(nal['data'])
            if slice_info:
                frame_info.append({
                    'nal_type': nal_type,
                    'slice_type': slice_info['slice_type'],
                    'is_reference': slice_info['is_reference'],
                    'size': nal['size']
                })
    
    return {
        'has_sps': sps_found,
        'has_pps': pps_found,
        'frames': frame_info
    }

def print_reference_analysis(ref_data, filename):
    """Print reference frame analysis"""
    print(f"\n=== Reference Frame Analysis: {filename} ===")
    
    if not ref_data['has_sps'] or not ref_data['has_pps']:
        print("Warning: Missing SPS/PPS headers")
    
    frames = ref_data['frames']
    if not frames:
        print("No frame data found")
        return
    
    # Count frame types
    idr_count = sum(1 for f in frames if f['nal_type'] == 5)
    non_idr_ref_count = sum(1 for f in frames if f['nal_type'] in [1, 2] and f['is_reference'])
    non_ref_count = sum(1 for f in frames if not f['is_reference'])
    
    print(f"Total frames analyzed: {len(frames)}")
    print(f"IDR frames (I): {idr_count}")
    print(f"Non-IDR reference frames (P): {non_idr_ref_count}")
    print(f"Non-reference frames (B): {non_ref_count}")
    
    # Reference frame ratio
    ref_frames = idr_count + non_idr_ref_count
    ref_ratio = (ref_frames / len(frames)) * 100 if frames else 0
    print(f"Reference frame ratio: {ref_ratio:.1f}%")
    
    # Frame size analysis by type
    idr_sizes = [f['size'] for f in frames if f['nal_type'] == 5]
    p_sizes = [f['size'] for f in frames if f['nal_type'] in [1, 2]]
    
    if idr_sizes:
        print(f"IDR frame sizes: avg={sum(idr_sizes)/len(idr_sizes):.0f}, max={max(idr_sizes)}, min={min(idr_sizes)}")
    if p_sizes:
        print(f"P frame sizes: avg={sum(p_sizes)/len(p_sizes):.0f}, max={max(p_sizes)}, min={min(p_sizes)}")

def analyze_detailed_encoding_params(video_path):
    """Extract detailed encoding parameters using ffprobe"""
    cmd = [
        'ffprobe',
        '-v', 'quiet',
        '-print_format', 'json',
        '-show_entries', 'stream=profile,level,refs,has_b_frames,bit_rate,max_bit_rate,bits_per_raw_sample:format=bit_rate,duration',
        str(video_path)
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        data = json.loads(result.stdout)
        
        stream = data.get('streams', [{}])[0]
        format_info = data.get('format', {})
        
        return {
            'profile': stream.get('profile', 'Unknown'),
            'level': stream.get('level', 'Unknown'),
            'refs': stream.get('refs', 'Unknown'),
            'has_b_frames': stream.get('has_b_frames', 'Unknown'),
            'bit_rate': stream.get('bit_rate', 'Unknown'),
            'max_bit_rate': stream.get('max_bit_rate', 'Unknown'),
            'bits_per_raw_sample': stream.get('bits_per_raw_sample', 'Unknown'),
            'format_bit_rate': format_info.get('bit_rate', 'Unknown'),
            'duration': format_info.get('duration', 'Unknown')
        }
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"Error getting detailed params: {e}")
        return {}

def analyze_quantization_and_motion(video_path):
    """Analyze quantization parameters and motion vectors"""
    # Extract detailed frame information including QP values
    cmd = [
        'ffprobe',
        '-v', 'quiet',
        '-print_format', 'json',
        '-show_entries', 'frame=pict_type,coded_picture_number,display_picture_number,pkt_size,pkt_pts,pkt_dts',
        '-select_streams', 'v:0',
        str(video_path)
    ]
    
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        data = json.loads(result.stdout)
        frames = data.get('frames', [])
        
        # Analyze frame size distribution
        i_frames = [f for f in frames if f.get('pict_type') == 'I']
        p_frames = [f for f in frames if f.get('pict_type') == 'P']
        
        i_sizes = [int(f.get('pkt_size', 0)) for f in i_frames]
        p_sizes = [int(f.get('pkt_size', 0)) for f in p_frames]
        
        # Calculate frame size statistics
        stats = {}
        if i_sizes:
            stats['i_frame_avg'] = sum(i_sizes) / len(i_sizes)
            stats['i_frame_std'] = (sum((x - stats['i_frame_avg'])**2 for x in i_sizes) / len(i_sizes))**0.5
            stats['i_frame_count'] = len(i_sizes)
        
        if p_sizes:
            stats['p_frame_avg'] = sum(p_sizes) / len(p_sizes)
            stats['p_frame_std'] = (sum((x - stats['p_frame_avg'])**2 for x in p_sizes) / len(p_sizes))**0.5
            stats['p_frame_count'] = len(p_sizes)
        
        # Calculate bitrate variation (coefficient of variation)
        all_sizes = i_sizes + p_sizes
        if all_sizes:
            avg_size = sum(all_sizes) / len(all_sizes)
            std_size = (sum((x - avg_size)**2 for x in all_sizes) / len(all_sizes))**0.5
            stats['size_cv'] = std_size / avg_size if avg_size > 0 else 0
        
        return stats
        
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"Error analyzing quantization: {e}")
        return {}

def print_detailed_comparison(results, detailed_params, quant_stats):
    """Print detailed comparison table"""
    print("\n=== Detailed Encoder Comparison ===")
    print(f"{'File':<25} {'Profile':<8} {'Level':<6} {'Refs':<5} {'B-frames':<8} {'Bit Rate':<10}")
    print("-" * 70)
    
    for filename in results.keys():
        params = detailed_params.get(filename, {})
        profile = params.get('profile', 'N/A')
        level = params.get('level', 'N/A')
        refs = params.get('refs', 'N/A')
        has_b = params.get('has_b_frames', 'N/A')
        bit_rate = params.get('bit_rate', 'N/A')
        if bit_rate != 'N/A' and bit_rate != 'Unknown':
            bit_rate = f"{int(bit_rate)/1000:.0f}k"
        
        print(f"{filename:<25} {profile:<8} {level:<6} {refs:<5} {has_b:<8} {bit_rate:<10}")
    
    print("\n=== Frame Size Statistics ===")
    print(f"{'File':<25} {'I-Frame Avg':<12} {'I-Frame Std':<12} {'P-Frame Avg':<12} {'P-Frame Std':<12} {'Size CV':<8}")
    print("-" * 95)
    
    for filename in results.keys():
        stats = quant_stats.get(filename, {})
        i_avg = stats.get('i_frame_avg', 0)
        i_std = stats.get('i_frame_std', 0)
        p_avg = stats.get('p_frame_avg', 0)
        p_std = stats.get('p_frame_std', 0)
        size_cv = stats.get('size_cv', 0)
        
        print(f"{filename:<25} {i_avg:<12.0f} {i_std:<12.0f} {p_avg:<12.0f} {p_std:<12.0f} {size_cv:<8.3f}")

def print_comparison_table(results):
    """Print basic comparison table for multiple files"""
    print("\n=== Basic Comparison Summary ===")
    print(f"{'File':<20} {'Avg Bitrate':<12} {'GOP Size':<10} {'Peak Frame':<12} {'I-Frames':<10} {'Ref Ratio':<10}")
    print("-" * 80)
    
    for filename, data in results.items():
        avg_bitrate = data.get('avg_bitrate', 0) / 1000000  # Convert to Mbps
        gop_size = data.get('avg_gop_size', 0)
        peak_frame = data.get('peak_frame_mbps', 0)
        i_frame_count = data.get('i_frame_count', 0)
        ref_ratio = data.get('ref_ratio', 0)
        
        print(f"{filename:<20} {avg_bitrate:<12.2f} {gop_size:<10.1f} {peak_frame:<12.2f} {i_frame_count:<10} {ref_ratio:<10.1f}")

def analyze_video(video_path, framerate=60):
    """Analyze a single video file"""
    print(f"\nAnalyzing: {video_path}")
    print("=" * 50)
    
    # Get frame information
    frame_data = run_ffprobe(video_path, 'frame')
    if not frame_data:
        return None
    
    frames = frame_data.get('frames', [])
    
    # Get packet information for bitrate analysis
    packet_data = run_ffprobe(video_path, 'packet')
    if not packet_data:
        return None
    
    packets = packet_data.get('packets', [])
    
    # Analyze GOP structure
    gops = analyze_gop_structure(frames)
    print_gop_analysis(gops, framerate)
    
    # Analyze bitrate
    frame_sizes, total_size = analyze_bitrate(packets)
    print_bitrate_analysis(frame_sizes, framerate)
    
    # Analyze NAL units and reference frames
    print("\nExtracting H.264 stream for NAL analysis...")
    h264_data = extract_raw_h264_stream(video_path)
    if h264_data:
        nal_units = parse_nal_units(h264_data)
        ref_data = analyze_reference_frames(nal_units)
        print_reference_analysis(ref_data, video_path.name)
    else:
        print("Could not extract H.264 stream for NAL analysis")
        ref_data = {'frames': []}
    
    # Return summary data for comparison
    if frame_sizes and gops:
        duration = len(frame_sizes) / framerate
        avg_bitrate = (sum(frame_sizes) * 8) / duration if duration > 0 else 0
        avg_gop_size = sum(len(gop) for gop in gops) / len(gops) if gops else 0
        peak_frame_mbps = (max(frame_sizes) * 8 * framerate) / 1000000 if frame_sizes else 0
        i_frame_count = sum(1 for gop in gops for frame in gop if frame['type'] == 'I')
        
        # Add reference frame ratio
        ref_frames = ref_data.get('frames', [])
        ref_ratio = 0
        if ref_frames:
            total_ref = sum(1 for f in ref_frames if f['is_reference'])
            ref_ratio = (total_ref / len(ref_frames)) * 100
        
        return {
            'avg_bitrate': avg_bitrate,
            'avg_gop_size': avg_gop_size,
            'peak_frame_mbps': peak_frame_mbps,
            'i_frame_count': i_frame_count,
            'ref_ratio': ref_ratio
        }
    
    return None

def main():
    parser = argparse.ArgumentParser(description='Analyze video GOP structure and bitrate')
    parser.add_argument('videos', nargs='+', help='Video files to analyze')
    parser.add_argument('--framerate', '-f', type=int, default=60, help='Video framerate (default: 60)')
    parser.add_argument('--detailed', action='store_true', help='Enable detailed analysis')
    parser.add_argument('--headers', action='store_true', help='Compare H.264 SPS/PPS/Slice headers')
    
    args = parser.parse_args()
    
    results = {}
    detailed_params = {}
    quant_stats = {}
    
    for video_path in args.videos:
        path = Path(video_path)
        if not path.exists():
            print(f"Error: File {video_path} not found")
            continue
        
        result = analyze_video(path, args.framerate)
        if result:
            results[path.name] = result
        
        if args.detailed:
            detailed_params[path.name] = analyze_detailed_encoding_params(path)
            quant_stats[path.name] = analyze_quantization_and_motion(path)
    
    # Print comparison if multiple files
    if len(results) > 1:
        print_comparison_table(results)
        
        if args.detailed:
            print_detailed_comparison(results, detailed_params, quant_stats)
        
        if args.headers:
            header_results = compare_h264_headers(args.videos)
            print_header_comparison(header_results)

if __name__ == '__main__':
    main()