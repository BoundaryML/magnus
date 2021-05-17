mod method;
mod module;
mod object;
mod r_array;
mod r_basic;
mod r_bignum;
mod r_class;
mod r_complex;
mod r_file;
mod r_float;
mod r_hash;
mod r_match;
mod r_module;
mod r_object;
mod r_rational;
mod r_regexp;
mod r_string;
mod r_struct;
mod r_typed_data;
pub mod ruby_sys;
pub mod value;

use std::{
    ffi::CString,
    mem::transmute,
    os::raw::c_int,
    sync::atomic::{AtomicBool, Ordering},
};

use method::Method;
use ruby_sys::{
    rb_define_class, rb_define_global_function, rb_define_module, rb_errinfo,
    rb_eval_string_protect, rb_jump_tag, rb_protect, rb_set_errinfo, ruby_cleanup, ruby_init,
    VALUE,
};

pub use value::{Fixnum, Flonum, Qfalse, Qnil, Qtrue, Symbol, Value};
pub use {
    module::Module,
    object::Object,
    r_array::RArray,
    r_basic::RBasic,
    r_bignum::RBignum,
    r_class::RClass,
    r_complex::RComplex,
    r_file::RFile,
    r_float::RFloat,
    r_hash::RHash,
    r_match::RMatch,
    r_module::RModule,
    r_object::RObject,
    r_rational::RRational,
    r_regexp::RRegexp,
    r_string::RString,
    r_struct::RStruct,
    r_typed_data::{RTypedData, TypedData},
};

pub mod prelude {
    pub use crate::{module::Module, object::Object, r_typed_data::TypedData};
}

#[macro_export]
macro_rules! fn_ptr {
    ($fn:expr) => {
        $fn as unsafe extern "C" fn(_) -> _
    };
}

pub fn define_class(name: &str, superclass: RClass) -> Result<RClass, ProtectState> {
    debug_assert_value!(superclass);
    let name = CString::new(name).unwrap();
    let superclass = superclass.into_inner();
    unsafe {
        let res = protect(|| Value::new(rb_define_class(name.as_ptr(), superclass)));
        res.map(|v| RClass::from_value(&v).unwrap())
    }
}

pub fn define_module(name: &str) -> Result<RModule, ProtectState> {
    let name = CString::new(name).unwrap();
    unsafe {
        let res = protect(|| Value::new(rb_define_module(name.as_ptr())));
        res.map(|v| RModule::from_value(&v).unwrap())
    }
}

pub fn define_global_function<M>(name: &str, func: M)
where
    M: Method,
{
    let name = CString::new(name).unwrap();
    unsafe {
        rb_define_global_function(name.as_ptr(), transmute(func.as_ptr()), M::arity().into());
    }
}

#[derive(Debug)]
#[repr(transparent)]
pub struct ProtectState(c_int);

impl ProtectState {
    /// # Safety
    ///
    /// This function is currently marked unsafe as it is presumed that the
    /// ProtectState can get stale and thus no longer safe to resume.
    pub unsafe fn resume(self) -> ! {
        rb_jump_tag(self.0);
        unreachable!()
    }

    pub fn is_exception(&self) -> bool {
        // safe ffi to Ruby, call doesn't raise
        !Value::new(unsafe { rb_errinfo() }).is_nil()
    }

    pub fn into_exception(self) -> Value {
        // safe ffi to Ruby, call doesn't raise
        Value::new(unsafe { rb_errinfo() })
        // need to clear errinfo, that's done by drop
    }
}

impl Drop for ProtectState {
    fn drop(&mut self) {
        // safe ffi to Ruby, call doesn't raise
        unsafe { rb_set_errinfo(Qnil::new().into_inner()) };
    }
}

pub fn protect<F>(mut func: F) -> Result<Value, ProtectState>
where
    F: FnMut() -> Value,
{
    // nested function as this is totally unsafe to call out of this context
    // arg should not be a VALUE, but a mutable pointer to F, cast to VALUE
    unsafe extern "C" fn call<F>(arg: VALUE) -> VALUE
    where
        F: FnMut() -> Value,
    {
        let closure = arg as *mut F;
        (*closure)().into_inner()
    }

    let mut state = 0;
    // rb_protect takes:
    // arg1: function pointer that returns a VALUE
    // arg2: a VALUE
    // arg3: a pointer to an int.
    // rb_protect then calls arg1 with arg2 and returns the VALUE that arg1
    // returns. If a Ruby exception is raised (or other interrupt) the VALUE
    // returned is instead Qnil, and arg3 is set to non-zero.
    // As arg2 is only ever passed to arg1 and otherwise not touched we can
    // pack in whatever data we want that will fit into a VALUE. This is part
    // of the api and safe to do.
    // In this case we use arg2 to pass a pointer the Rust closure we actually
    // want to call, and arg1 is just a simple adapter to call arg2.
    let result = unsafe {
        let closure = &mut func as *mut F as VALUE;
        rb_protect(Some(call::<F>), closure, &mut state as *mut _)
    };

    if state == 0 {
        Ok(Value::new(result))
    } else {
        Err(ProtectState(state))
    }
}

pub fn eval_static(s: &'static str) -> Result<Value, ProtectState> {
    let mut state = 0;
    // safe ffi to Ruby, captures raised errors (+ brake, throw, etc) as state
    let result = unsafe {
        let s = CString::new(s).expect("NULL byte in eval string");
        rb_eval_string_protect(s.as_c_str().as_ptr(), &mut state as *mut _)
    };

    if state == 0 {
        Ok(Value::new(result))
    } else {
        Err(ProtectState(state))
    }
}

pub struct Cleanup();

impl Drop for Cleanup {
    fn drop(&mut self) {
        unsafe {
            ruby_cleanup(0);
        }
    }
}

/// # Safety
///
/// Must be called in `main()`, or at least a function higher up the stack than
/// any code calling Ruby. Must not drop Cleanup until the very end of the
/// process, after all Ruby execution has finished.
///
/// # Panics
///
/// Panics if called more than once.
///
/// # Examples
///
/// ```no_run
/// let _cleanup = unsafe { magnus::init() };
/// ```
#[inline(always)]
pub unsafe fn init() -> Cleanup {
    static INIT: AtomicBool = AtomicBool::new(false);
    match INIT.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst) {
        Ok(false) => {
            ruby_init();
        }
        Err(true) => panic!("Ruby already initialized"),
        r => panic!("unexpected INIT state {:?}", r),
    }
    Cleanup()
}
