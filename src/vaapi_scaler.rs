use anyhow::{bail, Result};
use cros_codecs::libva::{
    Display, Surface, VARectangle, VABufferID, VABufferType, VAConfigID, VAContextID, 
    VAEntrypoint, VAProfile, VAProcPipelineParameterBuffer, VASurfaceID, VA_PROGRESSIVE, 
    VA_STATUS_SUCCESS, vaBeginPicture, vaCreateBuffer, vaCreateConfig, vaCreateContext, 
    vaDestroyBuffer, vaDestroyConfig, vaDestroyContext, vaEndPicture, vaRenderPicture, 
    vaSyncSurface
};
use std::rc::Rc;

/// RAII wrapper for VAConfigID
struct VppConfig {
    display: Rc<Display>,
    config: VAConfigID,
}

impl VppConfig {
    fn new(display: Rc<Display>) -> Result<Self> {
        let mut config = Default::default();
        let ret = unsafe {
            vaCreateConfig(
                display.handle(),
                VAProfile::VAProfileNone,
                VAEntrypoint::VAEntrypointVideoProc,
                std::ptr::null_mut(),
                0,
                &mut config,
            )
        };
        if ret != VA_STATUS_SUCCESS as i32 {
            bail!("Error creating VPP config: {ret:?}");
        }
        Ok(Self { display, config })
    }

    fn id(&self) -> VAConfigID {
        self.config
    }
}

impl Drop for VppConfig {
    fn drop(&mut self) {
        unsafe {
            vaDestroyConfig(self.display.handle(), self.config);
        }
    }
}

/// RAII wrapper for VAContextID
struct VppContext {
    display: Rc<Display>,
    context: VAContextID,
}

impl VppContext {
    fn new(display: Rc<Display>, config: VAConfigID, width: u32, height: u32, render_targets: &[VASurfaceID]) -> Result<Self> {
        let mut context = Default::default();
        let ret = unsafe {
            vaCreateContext(
                display.handle(),
                config,
                width as i32,
                height as i32,
                VA_PROGRESSIVE as i32,
                render_targets.as_ptr() as *mut VASurfaceID,
                render_targets.len() as i32,
                &mut context,
            )
        };
        if ret != VA_STATUS_SUCCESS as i32 {
            bail!("Error creating VPP context: {ret:?}");
        }
        Ok(Self { display, context })
    }

    fn id(&self) -> VAContextID {
        self.context
    }
}

impl Drop for VppContext {
    fn drop(&mut self) {
        unsafe {
            vaDestroyContext(self.display.handle(), self.context);
        }
    }
}

/// RAII wrapper for VABufferID
struct VppBuffer {
    display: Rc<Display>,
    buffer: VABufferID,
}

impl VppBuffer {
    fn new(
        display: Rc<Display>,
        context: VAContextID,
        buffer_type: VABufferType::Type,
        size: u32,
        data: *mut std::ffi::c_void,
    ) -> Result<Self> {
        let mut buffer = Default::default();
        let ret = unsafe {
            vaCreateBuffer(
                display.handle(),
                context,
                buffer_type,
                size,
                1,
                data,
                &mut buffer,
            )
        };
        if ret != VA_STATUS_SUCCESS as i32 {
            bail!("Error creating VPP buffer: {ret:?}");
        }
        Ok(Self { display, buffer })
    }

    fn id(&self) -> VABufferID {
        self.buffer
    }
}

impl Drop for VppBuffer {
    fn drop(&mut self) {
        unsafe {
            vaDestroyBuffer(self.display.handle(), self.buffer);
        }
    }
}

/// High-level VAAPI scaler that reuses contexts and configurations
pub struct VaapiScaler {
    display: Rc<Display>,
    config: VppConfig,
    // Context will be created per-scaling operation since it depends on surface dimensions
}

impl VaapiScaler {
    /// Create a new VAAPI scaler instance
    pub fn new(display: Rc<Display>) -> Result<Self> {
        let config = VppConfig::new(display.clone())?;
        Ok(Self { display, config })
    }

    /// Scale a frame from source surface to destination surface
    pub fn scale(&self, src_surface: &Surface<()>, dst_surface: &Surface<()>) -> Result<()> {
        // Create context for this specific scaling operation
        let render_targets = [dst_surface.id()];
        let context = VppContext::new(
            self.display.clone(),
            self.config.id(),
            dst_surface.size().0,
            dst_surface.size().1,
            &render_targets,
        )?;

        // Define source and destination rectangles for scaling
        let src_rect = VARectangle {
            x: 0,
            y: 0,
            width: src_surface.size().0 as u16,
            height: src_surface.size().1 as u16,
        };

        let dst_rect = VARectangle {
            x: 0,
            y: 0,
            width: dst_surface.size().0 as u16,
            height: dst_surface.size().1 as u16,
        };

        // Create pipeline parameter buffer
        let pipeline_param = VAProcPipelineParameterBuffer {
            surface: src_surface.id(),
            surface_region: &src_rect,
            output_region: &dst_rect,
            ..Default::default()
        };
        let mut params = [pipeline_param];

        let pipeline_buf = VppBuffer::new(
            self.display.clone(),
            context.id(),
            VABufferType::VAProcPipelineParameterBufferType,
            std::mem::size_of::<VAProcPipelineParameterBuffer>() as u32,
            (&mut params).as_mut_ptr() as *mut _,
        )?;

        // Perform the scaling operation
        unsafe {
            vaBeginPicture(self.display.handle(), context.id(), dst_surface.id());
            vaRenderPicture(self.display.handle(), context.id(), &mut pipeline_buf.id(), 1);
            vaEndPicture(self.display.handle(), context.id());
        }

        Ok(())
    }

    /// Scale a frame and synchronize (wait for completion)
    pub fn scale_sync(&self, src_surface: &Surface<()>, dst_surface: &Surface<()>) -> Result<()> {
        self.scale(src_surface, dst_surface)?;
        unsafe {
            vaSyncSurface(self.display.handle(), dst_surface.id());
        }
        Ok(())
    }
}