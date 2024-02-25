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
            self.sem.signal();
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

unsafe impl<T> Send for Stack<T> where T: Send {}


#[cfg(test)]
mod test {
    use super::*;
    use std::thread;

    #[test]
    fn test_stack_single_threaded() {
        let stack = Stack::new();
        for i in 0..100 {
            stack.push(i);
        }
        for i in (0..100).rev() {
            let Some(val) = stack.pop() else { panic!("stack should not be empty") };
            println!("popped {val}");
            assert_eq!(val, i);
        }
    }

    #[test]
    fn test_stack_single_producer_single_consumer_multi_threaded() {
        let stack = Stack::new();

        let producer_stack = stack.clone();

        let producer_jh = thread::spawn(move || {
            for i in 0..500 {
                producer_stack.push(i);
            }
        });

        let consumer_jh = thread::spawn(move || {
            let mut empty_count = 0;
            let mut non_empty_count = 0;

            while non_empty_count < 500 {
                if let Some(val) = stack.pop() {
                    println!("consumer received {val}");
                    non_empty_count += 1;
                } else {
                    empty_count += 1;
                }
            }

            assert_eq!(non_empty_count, 500);
            println!("Consumer received {} values", non_empty_count);
            println!("Ratio of empty count to non empty count: {:10.10}", empty_count as f32 / non_empty_count as f32);
        });

        producer_jh.join().expect("producer thread panicked");
        consumer_jh.join().expect("consumer thread panicked");
    }

    #[test]
    fn test_stack_single_consumer_multi_producer_multi_threaded() {
        let stack = Stack::new();

        let mut producer_jhs = vec![];
        for i in 0..3 {
            let producer_stack = stack.clone();
            producer_jhs.push(thread::spawn(move || {
                for j in 0..1000 {
                    producer_stack.push(7 * j + i);
                }
            }));
        }

        let consumer_jh = thread::spawn(move || {

            let mut empty_count = 0;
            let mut non_empty_count = 0;
            let mut received_count = [0, 0, 0];

            while non_empty_count < 3000 {
                if let Some(val) = stack.pop() {
                    let producer_id = (val % 7) as usize;
                    received_count[producer_id] += 1;
                    non_empty_count += 1;
                } else {
                    empty_count += 1;
                }
            }

            assert_eq!(non_empty_count, 3000);
            assert_eq!(received_count[0], 1000);
            assert_eq!(received_count[1], 1000);
            assert_eq!(received_count[2], 1000);

            println!("Consumer received {non_empty_count} values");
            println!("Ratio of empty count to non empty count {:10.10}", empty_count as f32 / non_empty_count as f32);
        });

        for jh in producer_jhs {
            jh.join().expect("producer panicked");
        }

        consumer_jh.join().expect("consumer panicked");
    }
}

