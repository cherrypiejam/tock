// Safety: this doesn't prevents interrupts and thus may cause
// unexpected deadlocks in a preemptive environment.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize};
use core::sync::atomic::Ordering::{Acquire, Release, Relaxed};

use crate::smp::mutex::Mutex;

pub struct RwLock<T: ?Sized> {
    readers: Mutex::<usize>,
    writer_locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send> Sync for RwLock<T> {}

pub struct RwLockReadGuard<'a, T: ?Sized + 'a> {
    lock: &'a RwLock<T>,
}

impl<'a, T: ?Sized + 'a> Drop for RwLockReadGuard<'a, T> {
    fn drop(&mut self) {
        let readers = self.readers.lock();
        *readers -= 1;
        if *readers == 0 {
            self.lock.writer_locked.store(false, Release);
        }
    }
}

impl<'a, T: ?Sized + 'a> Deref for RwLockReadGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

pub struct RwLockWriteGuard<'a, T: ?Sized + 'a> {
    lock: &'a RwLock<T>,
}

impl<'a, T: ?Sized + 'a> Drop for RwLockWriteGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.writer_locked.store(false, Release);
    }
}

impl<'a, T: ?Sized + 'a> Deref for RwLockWriteGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<'a, T: ?Sized + 'a> DerefMut for RwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> RwLock<T> {
    pub const fn new(data: T) -> RwLock<T> {
        RwLock {
            readers: Mutex::new(0),
            writer_locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<T> {
        let readers = self.readers.lock();
        *readers += 1;
        if *readers == 1 {
            while self
                .writer_locked
                .compare_exchange(false, true, Acquire, Relaxed)
                != Ok(false)
            {}
        }
        RwLockReadGuard { lock: self }
    }

    pub fn write(&self) -> RwLockWriteGuard<T> {
        while self
            .writer_locked
            .compare_exchange(false, true, Acquire, Relaxed)
            != Ok(false)
        {}
        RwLockWriteGuard { lock: self }
    }
}
