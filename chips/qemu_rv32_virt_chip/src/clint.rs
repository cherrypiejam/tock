// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2022.

//! Machine Timer instantiation.

use kernel::utilities::StaticRef;
use kernel::threadlocal::{ThreadLocal, ThreadLocalDyn};
use kernel::thread_local_static;
use sifive::clint::ClintRegisters;

use crate::chip::QemuRv32VirtClint;
use crate::MAX_THREADS;

pub const CLINT_BASE: StaticRef<ClintRegisters> =
    unsafe { StaticRef::new(0x0200_0000 as *const ClintRegisters) };

static NO_CLIC: ThreadLocal<0, Option<QemuRv32VirtClint<'static>>> = ThreadLocal::new([]);

static mut CLIC: &'static dyn ThreadLocalDyn<Option<QemuRv32VirtClint<'static>>> = &NO_CLIC;

pub unsafe fn set_global_clic(
    clic: &'static dyn ThreadLocalDyn<Option<QemuRv32VirtClint<'static>>>
) {
    *core::ptr::addr_of_mut!(CLIC) = clic;
}

pub fn init_clic() {
    let state: &'static mut Option<QemuRv32VirtClint<'static>> = unsafe {
        let threadlocal: &'static dyn ThreadLocalDyn<_> = *core::ptr::addr_of_mut!(CLIC);
        threadlocal
            .get_mut()
            .map(|c| c.enter_nonreentrant(|clic| &mut *(clic as *mut _)))
            .expect("Current thread does not have access to a UART state")
    };
    state.replace(QemuRv32VirtClint::new(&CLINT_BASE));
}

unsafe fn with_clic<R, F>(f: F) -> Option<R>
where
    F: FnOnce(&mut QemuRv32VirtClint<'static>) -> R
{
    let threadlocal: &'static dyn ThreadLocalDyn<_> = *core::ptr::addr_of_mut!(CLIC);
    threadlocal
        .get_mut().and_then(|c| c.enter_nonreentrant(|v| v.as_mut().map(f)))
}


pub unsafe fn with_clic_panic<R, F>(f: F) -> R
where
    F: FnOnce(&mut QemuRv32VirtClint<'static>) -> R
{
    with_clic(f)
        .expect("Current thread does not have access to a valid CLIC")
}
