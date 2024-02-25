use std::sync::atomic::{Ordering::{Acquire, Release, Relaxed}, AtomicU32, AtomicUsize, fence};
use atomic_wait::{wake_one, wake_all, wait};


struct InnerSemaphore {
    count: AtomicU32,
    ref_count: AtomicUsize,
    max_count: u32,
}

impl InnerSemaphore {
    fn new(max_count: u32) -> Self {
        Self { count: AtomicU32::new(max_count), ref_count: AtomicUsize::new(1), max_count }
    }

    fn init_with(max_count: u32, init_val: u32) -> Self {
        Self { count: AtomicU32::new(init_val), ref_count: AtomicUsize::new(1), max_count }
    }

    fn wait(&self) {
        loop {
            let cur_count = self.count.load(Relaxed);
            if cur_count == 0 {
                wait(&self.count, 0);
                continue;
            }
            if self.count.compare_exchange(cur_count, cur_count - 1, Release, Relaxed).is_ok() {
                fence(Acquire);
                break;
            }
        }
    }

    fn signal(&self) {
        let mut cur_count = self.count.load(Relaxed);
        loop {
            assert!(cur_count < self.max_count, "count may not exceed set maximum");
            match self.count.compare_exchange(cur_count, cur_count + 1, Release, Relaxed) {
                Ok(prev) => {
                    if prev == 0 {
                        wake_all(&self.count);
                    }
                    break;
                },
                Err(next) => cur_count = next,
            }
        }
    }
}

pub struct Semaphore {
    inner: *mut InnerSemaphore,
}

impl Semaphore {
    pub fn new(max_count: u32) -> Self {
        assert!(max_count > 0, "Semaphore cannot have a max count of 0");
        let inner = Box::into_raw(Box::new(InnerSemaphore::new(max_count)));
        Self { inner }
    }

    pub fn init_with(max_count: u32, init_count: u32) -> Self {
        assert!(max_count > 0, "Semaphore cannot have a max count of 0");
        assert!(init_count <= max_count, "Initial value cannot exceed max_count");
        let inner = Box::into_raw(Box::new(InnerSemaphore::init_with(max_count, init_count)));
        Self { inner }
    }

    pub fn wait(&self) {
        // Safety: This pointer will never be null
        unsafe { (*self.inner).wait(); }
    }

    pub fn signal(&self) {
        // Safety: This pointer will never be null
        unsafe { (*self.inner).signal(); }
    }
}

impl Clone for Semaphore {
    fn clone(&self) -> Semaphore {
        // Safety: This pointer will never be null
        unsafe { (*self.inner).ref_count.fetch_add(1, Relaxed); }
        Semaphore { inner: self.inner }
    }
}

impl Drop for Semaphore {
    fn drop(&mut self) {
        // Safety: This pointer will never be null up to this point
        unsafe {
            if (*self.inner).ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                // Safety: We have exclusive access to `self.inner` at this point.
                std::ptr::drop_in_place(self.inner);
            }
        }
    }
}

unsafe impl Send for Semaphore {}
unsafe impl Sync for Semaphore {}


#[cfg(test)]
mod test {
    use super::*;
    use std::thread;
    use std::sync::{Barrier, Arc};

    #[test]
    fn test_binary_semaphore_single_reader_single_writer() {
        // For counting the number of additions
        static mut COUNTER: u32 = 0;
        // For guarding access to the static variable
        let semaphore = Semaphore::new(1);
        // For signaling threads are finished
        let barrier = Arc::new(Barrier::new(6));

        for i in 0..5 {
            let semaphore = semaphore.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                for j in 0..100 {
                    semaphore.wait();
                    unsafe { COUNTER += 1; }
                    semaphore.signal();
                }
                barrier.wait();
            });
        }

        barrier.wait();

        println!("The value of COUNTER = {}", unsafe { COUNTER } );
        assert_eq!(unsafe { COUNTER }, 500);

        static mut COUNTS: [u32; 3] = [0, 0, 0];
        let barrier = Arc::new(Barrier::new(4));

        for i in 0..3 {
            let semaphore = semaphore.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    semaphore.wait();
                    unsafe {
                        COUNTS[0] += i + 1;
                        COUNTS[1] += i + 1;
                        COUNTS[2] += i + 1;
                    }
                    semaphore.signal();
                }
                barrier.wait();
            });
        }

        barrier.wait();
        println!("The value of COUNTS = {:?}", unsafe { COUNTS });
        assert_eq!(unsafe { COUNTS[0] }, 600);
        assert_eq!(unsafe { COUNTS[1] }, 600);
        assert_eq!(unsafe { COUNTS[2] }, 600);
    }
}
