use std::sync::atomic::{AtomicUsize, AtomicU32, AtomicBool, AtomicPtr, fence, Ordering::{Release, Acquire, Relaxed}};
use std::ptr::NonNull;
use atomic_wait::{wait, wake_one, wake_all};


#[derive(Debug)]
pub struct InnerRcuNode<T> {
    ref_count: AtomicUsize,
    data: Option<T>
}

impl<T: Clone> InnerRcuNode<T> {
    fn new(data: T) -> Self {
        Self {
            ref_count: AtomicUsize::new(1),
            data: Some(data),
        }
    }

    fn copy(&self) -> T {
        self.data.as_ref().unwrap().clone()
    }

    fn take(&mut self) -> Option<T> {
        self.data.take()
    }
}

#[derive(Debug)]
pub struct RcuNode<T: Clone> {
    inner: NonNull<InnerRcuNode<T>>,
}

impl<T: Clone> RcuNode<T> {
    pub fn new(data: T) -> Self {
        let inner = NonNull::new(Box::into_raw(Box::new(InnerRcuNode::new(data)))).expect("pointer should not be null");
        Self { inner }
    }

    pub fn copy(&self) -> T {
        // Safety: inner pointer will never be null
        unsafe {
            self.inner.as_ref().copy()
        }
    }

    fn take(&mut self) -> Option<T> {
        // Safety: inner pointer will never be null
        unsafe {
            self.inner.as_mut().take()
        }
    }
}

impl<T: Clone> Clone for RcuNode<T> {
    fn clone(&self) -> Self {
        // Safety: inner pointer will never be null
        unsafe {
            self.inner.as_ref().ref_count.fetch_add(1, Relaxed);
        }
        Self { inner: self.inner }
    }
}

impl<T: Clone> Drop for RcuNode<T> {
    fn drop(&mut self) {
        // Safety: inner pointer will never be null up to this point
        unsafe {
            if self.inner.as_ref().ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                // there are exactly zero references remaining to the inner pointer at this point
                // so it is safe to deallocate
                drop(Box::from_raw(self.inner.as_ptr()));
            }
        }
    }
}

const DEFAULT: u32 = 0;
const NEW_EPOCH_INIT: u32 = 1;

const NEW_EPOCH_COMMIT: u32 = 2;

const NEW_EPOCH_FINAL: u32 = 3;


#[derive(Debug)]
struct InnerRcu<T: Clone> {
    ref_count: AtomicUsize,
    num_reads: AtomicU32,
    state: AtomicU32,
    cur_alloc: AtomicPtr<RcuNode<T>>,
    prev_alloc: AtomicPtr<RcuNode<T>>,
}

impl<T: Clone> InnerRcu<T> {
    fn new(data: T) -> Self {
        let inner_alloc = Box::into_raw(Box::new(RcuNode::new(data)));
        Self {
            ref_count: AtomicUsize::new(1),
            num_reads: AtomicU32::new(0),
            state: AtomicU32::new(0),
            cur_alloc: AtomicPtr::new(inner_alloc),
            prev_alloc: AtomicPtr::new(inner_alloc),
        }
    }

    unsafe fn read(&self) -> RcuNode<T> {
        self.num_reads.fetch_add(1, Relaxed);

        let node = (*self.cur_alloc.load(Relaxed)).clone();

        if self.num_reads.fetch_sub(1, Release) == 1 && self.state.compare_exchange(NEW_EPOCH_INIT, NEW_EPOCH_COMMIT, Relaxed, Relaxed).is_ok() {
            fence(Acquire);
            self.state.store(NEW_EPOCH_FINAL, Release);
            wake_one(&self.state);
        }

        node
    }

    unsafe fn update(&self, new_data: T) -> Result<(), T> {
        let mut neo = Box::into_raw(Box::new(RcuNode::new(new_data)));
        let prev_ptr = self.prev_alloc.load(Acquire);

        if self.cur_alloc.compare_exchange(prev_ptr, neo, Relaxed, Relaxed).is_ok() {
            self.state.store(NEW_EPOCH_INIT, Release);

            if self.num_reads.load(Relaxed) != 0 {
                loop {
                    let cur_state = self.state.load(Relaxed);
                    if cur_state == 1 {
                        wait(&self.state, 1);
                    } else if cur_state == 2 {
                        wait(&self.state, 2);
                    } else {
                        break;
                    }
                }
            }

            // acquire matches the release store of `self.num_reads` and the store of NEW_EPOCH_FINAL
            // on `self.state`.
            fence(Acquire);

            // change the state back to default
            self.state.store(DEFAULT, Relaxed);

            // no other thread will dereference this pointer at this point
            std::ptr::drop_in_place(prev_ptr);
            self.prev_alloc.store(neo, Release);

            Ok(())
        } else {
            // Safety: no other thread has access to this pointer
            let err_val = (*neo).take().expect("option should not be none");
            Err(err_val)
        }
    }


}