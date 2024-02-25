use std::ptr;
use std::sync::atomic::{AtomicUsize, AtomicU32, AtomicPtr};
use std::sync::atomic::Ordering::{Relaxed, Release, Acquire};
use std::sync::atomic::fence;
use crate::semaphore::Semaphore;


struct StackNode<T> {
    data: Option<T>,
    next: *mut StackNode<T>,
}

impl<T> StackNode<T> {
    fn new() -> Self {
        Self { data: None, next: ptr::null_mut::<StackNode<T>>() }
    }

    fn init_with(val: T) -> Self {
        Self { data: Some(val), next: ptr::null_mut::<StackNode<T>>() }
    }
}

impl<T> Default for StackNode<T>
where T: Default
{
    fn default() -> Self {
        Self {
            data: Some(T::default()),
            next: ptr::null_mut::<StackNode<T>>(),
        }
    }
}

struct InnerStack<T> {
    head: *mut StackNode<T>,
    sem: Semaphore,
    ref_count: AtomicUsize,
}

impl<T>  InnerStack<T> {
    fn new() -> Self {
        let head = ptr::null_mut::<StackNode<T>>();
        let sem = Semaphore::init_with(1, 1);
        let ref_count = AtomicUsize::new(1);
        Self { head, sem, ref_count }
    }

    fn push(&mut self, val: T) {
        let mut neo = Box::into_raw(Box::new(StackNode::init_with(val)));
        self.sem.wait();
        // Safety: Only this thread has access to neo at this point as well as `self.head`
        unsafe {
            (*neo).next = self.head;
            self.head = neo;
        }
        self.sem.signal();
    }

    fn pop(&mut self) -> Option<T> {
        self.sem.wait();
        // Safety: We know we are the only thread that has access to `self.head` at this point
        let res = if self.head.is_null() {
            None
        } else {
            unsafe {
                let prev = self.head;
                let next = (*prev).next;
                let data = (*prev).data.take();
                self.head = next;
                self.sem.signal();
                ptr::drop_in_place(prev);
                data
            }
        };
        res
    }
}

impl<T> Drop for InnerStack<T> {
    fn drop(&mut self) {
        assert_eq!(self.ref_count.load(Relaxed), 0);
        let mut cur = self.head;
        // Safety: There are no threads that have access to `self.head`
        unsafe {
            while !cur.is_null() {
                let next = (*cur).next;
                (*cur).next = ptr::null_mut::<StackNode<T>>();
                ptr::drop_in_place(cur);
                cur = next
            }
        }
    }
}

pub struct Stack<T> {
    inner: *mut InnerStack<T>
}

impl<T> Stack<T> {
    pub fn new() -> Self {
        let inner = Box::into_raw(Box::new(InnerStack::new()));
        Self { inner }
    }

    pub fn push(&self, val: T) {
        // Safety: We know this pointer will never be null
        unsafe { (*self.inner).push(val); }
    }

    pub fn pop(&self) -> Option<T> {
        // Safety: We know this pointer will never be null
        unsafe { (*self.inner).pop() }
    }
}

impl<T> Clone for Stack<T> {
    fn clone(&self) -> Self {
        // Safety: We know this pointer will not be null
        unsafe {
            (*self.inner).ref_count.fetch_add(1, Relaxed);
        }
        Stack { inner: self.inner }
    }
}

impl<T> Drop for Stack<T> {
    fn drop(&mut self) {
        // Safety: This pointer will not be null at this point
        unsafe {
            if (*self.inner).ref_count.fetch_sub(1, Release) == 1 {
                fence(Acquire);
                // We have exclusive access to `self.inner` at this point, and no other thread
                // will ever have access to it again so it is safe to drop
                ptr::drop_in_place(self.inner);
            }
        }
    }
}

