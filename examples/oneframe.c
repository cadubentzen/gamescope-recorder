#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <va/va.h>
#include <va/va_drm.h>
#include <va/va_enc_h264.h>

#define WIDTH 1920
#define HEIGHT 1080
#define CHECK_VASTATUS(va_status, func)                                        \
  if (va_status != VA_STATUS_SUCCESS) {                                        \
    fprintf(stderr, "%s failed with error code %d\n", func, va_status);        \
    exit(1);                                                                   \
  }

// Generate a simple test pattern (checkerboard)
void generate_test_frame(unsigned char *y_plane, unsigned char *uv_plane,
                         int width, int height) {
  // Fill Y plane with checkerboard pattern
  for (int i = 0; i < height; i++) {
    for (int j = 0; j < width; j++) {
      int block = ((i / 32) + (j / 32)) % 2;
      y_plane[i * width + j] = block ? 235 : 16; // White or black in YUV
    }
  }

  // Fill UV plane (NV12 format - interleaved U and V)
  int uv_height = height / 2;
  int uv_width = width / 2;
  for (int i = 0; i < uv_height; i++) {
    for (int j = 0; j < uv_width; j++) {
      uv_plane[i * width + j * 2] = 128;     // U component (neutral)
      uv_plane[i * width + j * 2 + 1] = 128; // V component (neutral)
    }
  }
}

int main() {
  VADisplay va_dpy;
  VAStatus va_status;
  VAConfigID config_id;
  VAContextID context_id;
  VASurfaceID surface_id;
  VABufferID coded_buf;
  VAEncSequenceParameterBufferH264 seq_param;
  VAEncPictureParameterBufferH264 pic_param;
  VAEncSliceParameterBufferH264 slice_param;

  // Open DRM device
  int drm_fd = open("/dev/dri/renderD128", O_RDWR);
  if (drm_fd < 0) {
    fprintf(stderr, "Failed to open DRM device\n");
    return 1;
  }

  // Initialize VAAPI with DRM
  va_dpy = vaGetDisplayDRM(drm_fd);
  if (!va_dpy) {
    fprintf(stderr, "Failed to get VA display\n");
    return 1;
  }

  int major_ver, minor_ver;
  va_status = vaInitialize(va_dpy, &major_ver, &minor_ver);
  CHECK_VASTATUS(va_status, "vaInitialize");
  printf("VA-API version %d.%d\n", major_ver, minor_ver);

  // Query and find H264 encoder
  VAEntrypoint entrypoints[5];
  int num_entrypoints;
  va_status = vaQueryConfigEntrypoints(va_dpy, VAProfileH264ConstrainedBaseline,
                                       entrypoints, &num_entrypoints);
  CHECK_VASTATUS(va_status, "vaQueryConfigEntrypoints");

  int supports_encode = 0;
  for (int i = 0; i < num_entrypoints; i++) {
    if (entrypoints[i] == VAEntrypointEncSlice) {
      supports_encode = 1;
      break;
    }
  }

  if (!supports_encode) {
    fprintf(stderr, "H264 encoding not supported\n");
    return 1;
  }

  // Create config for H264 encoding
  VAConfigAttrib attrib[2];
  attrib[0].type = VAConfigAttribRTFormat;
  attrib[1].type = VAConfigAttribRateControl;

  va_status = vaGetConfigAttributes(va_dpy, VAProfileH264ConstrainedBaseline,
                                    VAEntrypointEncSlice, attrib, 2);
  CHECK_VASTATUS(va_status, "vaGetConfigAttributes");

  attrib[0].value = VA_RT_FORMAT_YUV420;
  attrib[1].value = VA_RC_CQP; // Constant QP mode

  va_status = vaCreateConfig(va_dpy, VAProfileH264ConstrainedBaseline,
                             VAEntrypointEncSlice, attrib, 2, &config_id);
  CHECK_VASTATUS(va_status, "vaCreateConfig");

  // Create surface for input frame
  VASurfaceAttrib surface_attrib;
  surface_attrib.type = VASurfaceAttribPixelFormat;
  surface_attrib.flags = VA_SURFACE_ATTRIB_SETTABLE;
  surface_attrib.value.type = VAGenericValueTypeInteger;
  surface_attrib.value.value.i = VA_FOURCC_NV12;

  va_status = vaCreateSurfaces(va_dpy, VA_RT_FORMAT_YUV420, WIDTH, HEIGHT,
                               &surface_id, 1, &surface_attrib, 1);
  CHECK_VASTATUS(va_status, "vaCreateSurfaces");

  // Create encoding context
  va_status = vaCreateContext(va_dpy, config_id, WIDTH, HEIGHT, VA_PROGRESSIVE,
                              &surface_id, 1, &context_id);
  CHECK_VASTATUS(va_status, "vaCreateContext");

  // Create coded buffer for output
  va_status = vaCreateBuffer(va_dpy, context_id, VAEncCodedBufferType,
                             WIDTH * HEIGHT * 3 / 2, 1, NULL, &coded_buf);
  CHECK_VASTATUS(va_status, "vaCreateBuffer");

  // Upload test frame to surface
  VAImage image;
  va_status = vaDeriveImage(va_dpy, surface_id, &image);
  CHECK_VASTATUS(va_status, "vaDeriveImage");

  unsigned char *buf;
  va_status = vaMapBuffer(va_dpy, image.buf, (void **)&buf);
  CHECK_VASTATUS(va_status, "vaMapBuffer");

  // Generate and copy test pattern
  unsigned char *y_plane = buf + image.offsets[0];
  unsigned char *uv_plane = buf + image.offsets[1];
  generate_test_frame(y_plane, uv_plane, WIDTH, HEIGHT);

  va_status = vaUnmapBuffer(va_dpy, image.buf);
  CHECK_VASTATUS(va_status, "vaUnmapBuffer");

  va_status = vaDestroyImage(va_dpy, image.image_id);
  CHECK_VASTATUS(va_status, "vaDestroyImage");

  // Set up sequence parameters
  memset(&seq_param, 0, sizeof(seq_param));
  seq_param.seq_parameter_set_id = 0;
  seq_param.level_idc = 41; // Level 4.1
  seq_param.picture_width_in_mbs = (WIDTH + 15) / 16;
  seq_param.picture_height_in_mbs = (HEIGHT + 15) / 16;
  seq_param.bits_per_second = 10000000; // 10 Mbps
  seq_param.time_scale = 60; // For 30fps: time_scale=60, num_units_in_tick=1
  seq_param.num_units_in_tick = 1;
  seq_param.ip_period = 1;         // Only I and P frames
  seq_param.intra_period = 30;     // I-frame every 30 frames
  seq_param.intra_idr_period = 30; // IDR period
  seq_param.max_num_ref_frames = 1;
  seq_param.picture_width_in_mbs = (WIDTH + 15) / 16;
  seq_param.picture_height_in_mbs = (HEIGHT + 15) / 16;
  seq_param.seq_fields.bits.chroma_format_idc = 1;   // 4:2:0
  seq_param.seq_fields.bits.frame_mbs_only_flag = 1; // Progressive
  seq_param.seq_fields.bits.mb_adaptive_frame_field_flag = 0;
  seq_param.seq_fields.bits.seq_scaling_matrix_present_flag = 0;
  seq_param.seq_fields.bits.direct_8x8_inference_flag = 1;
  seq_param.seq_fields.bits.log2_max_frame_num_minus4 = 12;
  seq_param.seq_fields.bits.pic_order_cnt_type = 0;
  seq_param.seq_fields.bits.log2_max_pic_order_cnt_lsb_minus4 = 12;
  seq_param.seq_fields.bits.delta_pic_order_always_zero_flag = 0;
  seq_param.bit_depth_luma_minus8 = 0;
  seq_param.bit_depth_chroma_minus8 = 0;

  VABufferID seq_param_buf;
  va_status =
      vaCreateBuffer(va_dpy, context_id, VAEncSequenceParameterBufferType,
                     sizeof(seq_param), 1, &seq_param, &seq_param_buf);
  CHECK_VASTATUS(va_status, "vaCreateBuffer");

  // Set up rate control parameters (for CQP mode)
  VABufferID rc_param_buf;
  VAEncMiscParameterBuffer *misc_buffer;
  VAEncMiscParameterRateControl *rate_control_param;

  va_status = vaCreateBuffer(va_dpy, context_id, VAEncMiscParameterBufferType,
                             sizeof(VAEncMiscParameterBuffer) +
                                 sizeof(VAEncMiscParameterRateControl),
                             1, NULL, &rc_param_buf);
  CHECK_VASTATUS(va_status, "vaCreateBuffer");

  va_status = vaMapBuffer(va_dpy, rc_param_buf, (void **)&misc_buffer);
  CHECK_VASTATUS(va_status, "vaMapBuffer");

  misc_buffer->type = VAEncMiscParameterTypeRateControl;
  rate_control_param = (VAEncMiscParameterRateControl *)misc_buffer->data;
  memset(rate_control_param, 0, sizeof(VAEncMiscParameterRateControl));
  rate_control_param->bits_per_second = 10000000; // 10 Mbps
  rate_control_param->target_percentage = 100;
  rate_control_param->window_size = 1000;
  rate_control_param->initial_qp = 26;
  rate_control_param->min_qp = 10;
  rate_control_param->max_qp = 51;

  va_status = vaUnmapBuffer(va_dpy, rc_param_buf);
  CHECK_VASTATUS(va_status, "vaUnmapBuffer");

  // Set up picture parameters
  memset(&pic_param, 0, sizeof(pic_param));
  pic_param.CurrPic.picture_id = surface_id;
  pic_param.CurrPic.frame_idx = 0;
  pic_param.CurrPic.flags = 0;
  pic_param.CurrPic.TopFieldOrderCnt = 0;
  pic_param.CurrPic.BottomFieldOrderCnt = 0;
  pic_param.ReferenceFrames[0].picture_id = VA_INVALID_ID;
  pic_param.coded_buf = coded_buf;
  pic_param.pic_parameter_set_id = 0;
  pic_param.seq_parameter_set_id = 0;
  pic_param.last_picture = 0;
  pic_param.frame_num = 0;
  pic_param.pic_init_qp = 26;
  pic_param.num_ref_idx_l0_active_minus1 = 0;
  pic_param.num_ref_idx_l1_active_minus1 = 0;
  pic_param.chroma_qp_index_offset = 0;
  pic_param.second_chroma_qp_index_offset = 0;
  pic_param.pic_fields.bits.idr_pic_flag = 1; // IDR frame
  pic_param.pic_fields.bits.reference_pic_flag = 1;
  pic_param.pic_fields.bits.entropy_coding_mode_flag = 0; // CAVLC
  pic_param.pic_fields.bits.weighted_pred_flag = 0;
  pic_param.pic_fields.bits.weighted_bipred_idc = 0;
  pic_param.pic_fields.bits.constrained_intra_pred_flag = 0;
  pic_param.pic_fields.bits.transform_8x8_mode_flag = 0;
  pic_param.pic_fields.bits.deblocking_filter_control_present_flag = 1;

  VABufferID pic_param_buf;
  va_status =
      vaCreateBuffer(va_dpy, context_id, VAEncPictureParameterBufferType,
                     sizeof(pic_param), 1, &pic_param, &pic_param_buf);
  CHECK_VASTATUS(va_status, "vaCreateBuffer");

  // Set up slice parameters
  memset(&slice_param, 0, sizeof(slice_param));
  slice_param.macroblock_address = 0;
  slice_param.num_macroblocks =
      seq_param.picture_width_in_mbs * seq_param.picture_height_in_mbs;
  slice_param.pic_parameter_set_id = 0;
  slice_param.slice_type = 2; // I slice
  slice_param.direct_spatial_mv_pred_flag = 0;
  slice_param.num_ref_idx_l0_active_minus1 = 0;
  slice_param.num_ref_idx_l1_active_minus1 = 0;
  slice_param.cabac_init_idc = 0;
  slice_param.slice_qp_delta = 0;
  slice_param.disable_deblocking_filter_idc = 0;
  slice_param.slice_alpha_c0_offset_div2 = 0;
  slice_param.slice_beta_offset_div2 = 0;
  slice_param.idr_pic_id = 0;

  VABufferID slice_param_buf;
  va_status =
      vaCreateBuffer(va_dpy, context_id, VAEncSliceParameterBufferType,
                     sizeof(slice_param), 1, &slice_param, &slice_param_buf);
  CHECK_VASTATUS(va_status, "vaCreateBuffer");

  // Begin picture
  va_status = vaBeginPicture(va_dpy, context_id, surface_id);
  CHECK_VASTATUS(va_status, "vaBeginPicture");

  VABufferID buffers[] = {seq_param_buf, pic_param_buf, slice_param_buf,
                          rc_param_buf};

  // Render parameters
  va_status = vaRenderPicture(va_dpy, context_id, buffers,
                              sizeof(buffers) / sizeof(buffers[0]));
  CHECK_VASTATUS(va_status, "vaRenderPicture");

  // End picture
  va_status = vaEndPicture(va_dpy, context_id);
  CHECK_VASTATUS(va_status, "vaEndPicture");

  // Wait for encoding to complete
  va_status = vaSyncSurface(va_dpy, surface_id);
  CHECK_VASTATUS(va_status, "vaSyncSurface");

  // Get encoded data
  VACodedBufferSegment *buf_segment;
  va_status = vaMapBuffer(va_dpy, coded_buf, (void **)&buf_segment);
  CHECK_VASTATUS(va_status, "vaMapBuffer");

  // Write encoded data to file
  FILE *fp = fopen("output.h264", "wb");
  if (fp) {
    fwrite(buf_segment->buf, 1, buf_segment->size, fp);
    fclose(fp);
    printf("Encoded frame written to output.h264 (%d bytes)\n",
           buf_segment->size);
  } else {
    fprintf(stderr, "Failed to open output file\n");
  }

  va_status = vaUnmapBuffer(va_dpy, coded_buf);
  CHECK_VASTATUS(va_status, "vaUnmapBuffer");

  // Cleanup
  vaDestroyBuffer(va_dpy, seq_param_buf);
  vaDestroyBuffer(va_dpy, rc_param_buf);
  vaDestroyBuffer(va_dpy, pic_param_buf);
  vaDestroyBuffer(va_dpy, slice_param_buf);
  vaDestroyBuffer(va_dpy, coded_buf);
  vaDestroyContext(va_dpy, context_id);
  vaDestroySurfaces(va_dpy, &surface_id, 1);
  vaDestroyConfig(va_dpy, config_id);
  vaTerminate(va_dpy);
  close(drm_fd);

  return 0;
}
