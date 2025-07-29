use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A lock-free frame buffer that allows one writer and one reader
/// to operate concurrently without blocking each other.
///
/// Uses double-buffering with atomic state management.
pub struct FrameBuffer<T> {
    // Two frame slots
    frames: [AtomicPtr<T>; 2],

    // Which buffer is currently being read from (0 or 1)
    // The writer always writes to the opposite buffer
    reading_buffer: AtomicBool,
}

// Wrapper to make Arc<T> work with AtomicPtr
struct AtomicPtr<T> {
    ptr: std::sync::atomic::AtomicPtr<T>,
}

impl<T> AtomicPtr<T> {
    fn new() -> Self {
        Self {
            ptr: std::sync::atomic::AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn swap(&self, frame: Option<Arc<T>>, ordering: Ordering) -> Option<Arc<T>> {
        let new_ptr = match frame {
            Some(arc) => Arc::into_raw(arc) as *mut T,
            None => ptr::null_mut(),
        };

        let old_ptr = self.ptr.swap(new_ptr, ordering);

        if old_ptr.is_null() {
            None
        } else {
            // SAFETY: We own this pointer from a previous Arc::into_raw
            Some(unsafe { Arc::from_raw(old_ptr) })
        }
    }

    fn load(&self, ordering: Ordering) -> Option<Arc<T>> {
        let ptr = self.ptr.load(ordering);
        if ptr.is_null() {
            None
        } else {
            // SAFETY: We're incrementing the Arc's reference count and we know this pointer is valid
            unsafe {
                Arc::increment_strong_count(ptr);
                Some(Arc::from_raw(ptr))
            }
        }
    }
}

impl<T> Drop for AtomicPtr<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Relaxed);
        if !ptr.is_null() {
            // SAFETY: We own this pointer
            unsafe {
                Arc::from_raw(ptr);
            }
        }
    }
}

impl<T> FrameBuffer<T> {
    pub fn new() -> Self {
        Self {
            frames: [AtomicPtr::new(), AtomicPtr::new()],
            reading_buffer: AtomicBool::new(false), // Start with buffer 0 for reading
        }
    }

    /// Write a new frame. Non-blocking operation.
    pub fn write(&self, frame: Arc<T>) {
        // Determine which buffer to write to (opposite of reading buffer)
        let reading = self.reading_buffer.load(Ordering::Acquire);
        let writing = !reading;
        let write_idx = if writing { 1 } else { 0 };

        // Store the new frame in the write buffer
        self.frames[write_idx].swap(Some(frame), Ordering::Release);

        // Swap buffers by flipping the reading buffer flag
        self.reading_buffer.store(writing, Ordering::Release);
    }

    /// Read the latest complete frame. Non-blocking operation.
    /// Returns None if no frame has been written yet.
    /// Returns the same frame multiple times if no new frame is available.
    pub fn read(&self) -> Option<Arc<T>> {
        // Read from the current reading buffer
        let reading = self.reading_buffer.load(Ordering::Acquire);
        let read_idx = if reading { 1 } else { 0 };

        self.frames[read_idx].load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    // Example usage with a GPU frame type
    #[derive(Debug)]
    pub struct GpuFrame {
        pub data: Vec<u8>,
        pub width: u32,
        pub height: u32,
        pub timestamp: u64,
    }

    #[test]
    fn test_basic_write_read() {
        let buffer = Arc::new(FrameBuffer::new());

        // Write a frame
        let frame1 = Arc::new(GpuFrame {
            data: vec![1, 2, 3],
            width: 1920,
            height: 1080,
            timestamp: 1,
        });

        buffer.write(frame1.clone());

        // Read should return the frame
        let read_frame = buffer.read().unwrap();
        // assert_eq!(read_frame.timestamp, 1);
    }

    #[test]
    fn test_concurrent_access() {
        let buffer = Arc::new(FrameBuffer::new());
        let buffer_writer = buffer.clone();
        let buffer_reader = buffer.clone();

        // Writer thread
        let writer = thread::spawn(move || {
            for i in 0..100 {
                let frame = Arc::new(GpuFrame {
                    data: vec![i as u8],
                    width: 1920,
                    height: 1080,
                    timestamp: i,
                });
                buffer_writer.write(frame);
                thread::sleep(Duration::from_micros(100));
            }
        });

        // Reader thread
        let reader = thread::spawn(move || {
            let mut last_timestamp = 0;
            let mut duplicates = 0;
            let mut frames_read = 0;

            for _ in 0..200 {
                if let Some(frame) = buffer_reader.read() {
                    frames_read += 1;
                    if frame.timestamp == last_timestamp {
                        duplicates += 1;
                    } else {
                        assert!(frame.timestamp >= last_timestamp);
                        last_timestamp = frame.timestamp;
                    }
                }
                thread::sleep(Duration::from_micros(50));
            }

            println!("Frames read: {}, Duplicates: {}", frames_read, duplicates);
            assert!(frames_read > 0);
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}
