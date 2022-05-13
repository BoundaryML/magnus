//! Magnus is a library for writing Ruby extentions in Rust, or running Ruby
//! code from Rust.
//!
//! # Overview
//!
//! All Ruby objects are represented by [`Value`]. To make it easier to work
//! with values that are instances of specific classes a number of wrapper
//! types are available. These wrappers will [`Deref`](`std::ops::Deref`) to
//! `Value`, so you can still use `Value`'s methods on them.
//!
//! | Ruby Class | Magnus Type |
//! |------------|-------------|
//! | `String`   | [`RString`] |
//! | `Integer`  | [`Integer`] |
//! | `Float`    | [`Float`]   |
//! | `Array`    | [`RArray`]  |
//! | `Hash`     | [`RHash`]   |
//! | `Symbol`   | [`Symbol`]  |
//! | `Class`    | [`RClass`]  |
//! | `Module`   | [`RModule`] |
//!
//! When writing Rust code to be called from Ruby the [`init`] attribute can
//! be used to mark your init function that Ruby will call when your library
//! is `require`d.
//!
//! When embedding Ruby in a Rust program, see [`embed::init`] for initialising
//! the Ruby VM.
//!
//! The [`method`](`macro@method`) macro can be used to wrap a Rust function
//! with automatic type conversion and error handing so it can be exposed to
//! Ruby. The [`TryConvert`] trait handles conversions from Ruby to Rust, and
//! anything implementing `Into<Value>` can be returned to Ruby. See the
//! [`Module`] and [`Object`] traits for defining methods.
//!
//! [`Value::funcall`] can be used to call Ruby methods from Rust.
//!
//! See the [`wrap`] attribute macro for wrapping Rust types as Ruby objects.
//!
//! ## Safety
//!
//! When using Magnus, in Rust code, Ruby objects must be kept on the stack. If
//! objects are moved to the heap the Ruby GC can not reach them, and they may
//! be garbage collected. This could lead to memory safety issues.
//!
//! It is not possible to enforce this rule in Rust's type system or via the
//! borrow checker, users of Magnus must maintain this rule manually.
//!
//! While it would be possible to mark any functions that could expose this
//! unsafty as `unsafe`, that would mean that almost every interaction with
//! Ruby would be `unsafe`. This would leave no way to differentiate the
//! *really* unsafe functions that need much more care to use.
//!
//! # Examples
//!
//! ```
//! use magnus::{define_module, function, method, prelude::*, Error};
//!
//! #[magnus::wrap(class = "Euclid::Point", free_immediatly, size)]
//! struct Point {
//!     x: isize,
//!     y: isize,
//! }
//!
//! impl Point {
//!     fn new(x: isize, y: isize) -> Self {
//!         Self { x, y }
//!     }
//!
//!     fn x(&self) -> isize {
//!         self.x
//!     }
//!
//!     fn y(&self) -> isize {
//!         self.y
//!     }
//! }
//!
//! fn distance(a: &Point, b: &Point) -> f64 {
//!     (((b.x - a.x).pow(2) + (b.y - a.y).pow(2)) as f64).sqrt()
//! }
//!
//! #[magnus::init]
//! fn init() -> Result<(), Error> {
//!     let module = define_module("Euclid")?;
//!     let class = module.define_class("Point", Default::default())?;
//!     class.define_singleton_method("new", function!(Point::new, 2))?;
//!     class.define_method("x", method!(Point::x, 0))?;
//!     class.define_method("y", method!(Point::y, 0))?;
//!     module.define_module_function("distance", function!(distance, 2))?;
//!     Ok(())
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

mod binding;
pub mod block;
pub mod class;
#[cfg(feature = "embed")]
#[cfg_attr(docsrs, doc(cfg(feature = "embed")))]
pub mod embed;
pub mod encoding;
mod enumerator;
pub mod error;
pub mod exception;
mod float;
pub mod gc;
mod integer;
pub mod method;
pub mod module;
mod object;
mod r_array;
mod r_bignum;
mod r_complex;
mod r_file;
mod r_float;
pub mod r_hash;
mod r_match;
mod r_object;
mod r_rational;
mod r_regexp;
pub mod r_string;
pub mod r_struct;
pub mod r_typed_data;
mod range;
#[cfg(feature = "rb-sys-interop")]
#[cfg_attr(docsrs, doc(cfg(feature = "rb-sys-interop")))]
pub mod rb_sys;
mod ruby_sys;
pub mod scan_args;
mod symbol;
mod try_convert;
pub mod value;

use std::{ffi::CString, mem::transmute, os::raw::c_int};

use crate::ruby_sys::{
    rb_call_super, rb_current_receiver, rb_define_class, rb_define_global_function,
    rb_define_module, rb_define_variable, rb_errinfo, rb_eval_string_protect, rb_set_errinfo,
    VALUE,
};

#[cfg(ruby_lt_2_7)]
use crate::ruby_sys::rb_require;

#[cfg(ruby_gte_2_7)]
use crate::ruby_sys::rb_require_string;

pub use magnus_macros::{init, wrap, DataTypeFunctions, TypedData};

use error::protect;
use method::Method;

pub use value::{Fixnum, Flonum, StaticSymbol, Value, QFALSE, QNIL, QTRUE};
pub use {
    binding::Binding,
    class::RClass,
    enumerator::Enumerator,
    error::Error,
    exception::{Exception, ExceptionClass},
    float::Float,
    integer::Integer,
    module::{Attr, Module, RModule},
    object::Object,
    r_array::RArray,
    r_bignum::RBignum,
    r_complex::RComplex,
    r_file::RFile,
    r_float::RFloat,
    r_hash::RHash,
    r_match::RMatch,
    r_object::RObject,
    r_rational::RRational,
    r_regexp::RRegexp,
    r_string::RString,
    r_struct::RStruct,
    r_typed_data::{DataType, DataTypeFunctions, RTypedData, TypedData},
    range::Range,
    symbol::Symbol,
    try_convert::{ArgList, TryConvert},
};

/// Traits that commonly should be in scope.
pub mod prelude {
    pub use crate::{module::Module, object::Object};
}

/// Utility to simplify initialising a static with [`std::sync::Once`].
///
/// Similar (but less generally useful) to
/// [`lazy_static!`](https://crates.io/crates/lazy_static) without an external
/// dependency.
///
/// # Examples
///
/// ```
/// use magnus::{define_class, memoize, RClass};
///
/// fn foo_class() -> &'static RClass {
///     memoize!(RClass: define_class("Foo", Default::default()).unwrap())
/// }
/// ```
#[macro_export]
macro_rules! memoize {
    ($type:ty: $val:expr) => {{
        static INIT: std::sync::Once = std::sync::Once::new();
        static mut VALUE: Option<$type> = None;
        unsafe {
            INIT.call_once(|| {
                VALUE = Some($val);
            });
            VALUE.as_ref().unwrap()
        }
    }};
}

/// Define a class in the root scope.
pub fn define_class(name: &str, superclass: RClass) -> Result<RClass, Error> {
    debug_assert_value!(superclass);
    let name = CString::new(name).unwrap();
    let superclass = superclass.as_rb_value();
    protect(|| unsafe {
        RClass::from_rb_value_unchecked(rb_define_class(name.as_ptr(), superclass))
    })
}

/// Define a module in the root scope.
pub fn define_module(name: &str) -> Result<RModule, Error> {
    let name = CString::new(name).unwrap();
    protect(|| unsafe { RModule::from_rb_value_unchecked(rb_define_module(name.as_ptr())) })
}

/// Define a global variable.
pub fn define_variable<T: Into<Value>>(name: &str, initial: T) -> Result<*mut Value, Error> {
    let initial = initial.into();
    debug_assert_value!(initial);
    let name = CString::new(name).unwrap();
    let ptr = Box::into_raw(Box::new(initial));
    unsafe {
        rb_define_variable(name.as_ptr(), ptr as *mut VALUE);
    }
    Ok(ptr)
}

/// Define a global variable.
#[deprecated(since = "0.3.0", note = "please use `define_variable` instead")]
pub fn define_global_variable<T: Into<Value>>(name: &str, initial: T) -> Result<*mut Value, Error> {
    define_variable(name, initial)
}

/// Define a method in the root scope.
pub fn define_global_function<M>(name: &str, func: M)
where
    M: Method,
{
    let name = CString::new(name).unwrap();
    unsafe {
        rb_define_global_function(name.as_ptr(), transmute(func.as_ptr()), M::arity().into());
    }
}

/// Return the Ruby `self` of the current method context.
///
/// Returns `Err` if called outside a method context or the conversion fails.
pub fn current_receiver<T>() -> Result<T, Error>
where
    T: TryConvert,
{
    protect(|| unsafe { Value::new(rb_current_receiver()) }).and_then(|v| v.try_convert())
}

/// Call the super method of the current method context.
///
/// Returns `Ok(T)` if the super method exists and returns without error, and
/// the return value converts to a `T`, or returns `Err` if there is no super
/// method, the super method raises or the conversion fails.
pub fn call_super<A, T>(args: A) -> Result<T, Error>
where
    A: ArgList,
    T: TryConvert,
{
    unsafe {
        let args = args.into_arg_list();
        let slice = args.as_ref();
        protect(|| {
            Value::new(rb_call_super(
                slice.len() as c_int,
                slice.as_ptr() as *const VALUE,
            ))
        })
        .and_then(|v| v.try_convert())
    }
}

/// Finds and loads the given feature if not already loaded.
///
/// # Examples
///
/// ```
/// # let _cleanup = unsafe { magnus::embed::init() };
/// use magnus::require;
///
/// assert!(require("net/http").unwrap());
/// ```
#[cfg(ruby_gte_2_7)]
pub fn require<T>(feature: T) -> Result<bool, Error>
where
    T: Into<RString>,
{
    let feature = feature.into();
    protect(|| unsafe { Value::new(rb_require_string(feature.as_rb_value())) })
        .and_then(|v| v.try_convert())
}

/// Finds and loads the given feature if not already loaded.
///
/// # Examples
///
/// ```
/// # let _cleanup = unsafe { magnus::embed::init() };
/// use magnus::require;
///
/// assert!(require("net/http").unwrap());
/// ```
#[cfg(ruby_lt_2_7)]
pub fn require(feature: &str) -> Result<bool, Error> {
    let feature = CString::new(feature).unwrap();
    protect(|| unsafe { Value::new(rb_require(feature.as_ptr())) }).and_then(|v| v.try_convert())
}

/// Evaluate a string of Ruby code, converting the result to a `T`.
///
/// Ruby will use the 'ASCII-8BIT' (aka binary) encoding for any Ruby string
/// literals in the passed string of Ruby code. See the
/// [`eval`](macro@crate::eval) macro or [`Binding::eval`] for alternatives that
/// support utf-8.
///
/// Errors if `s` contains a null byte, the conversion fails, or on an uncaught
/// Ruby exception.
///
/// # Examples
///
/// ```
/// # let _cleanup = unsafe { magnus::embed::init() };
///
/// assert_eq!(magnus::eval::<i64>("1 + 2").unwrap(), 3);
/// ```
pub fn eval<T>(s: &str) -> Result<T, Error>
where
    T: TryConvert,
{
    let mut state = 0;
    // safe ffi to Ruby, captures raised errors (+ brake, throw, etc) as state
    let result = unsafe {
        let s =
            CString::new(s).map_err(|e| Error::new(exception::script_error(), e.to_string()))?;
        rb_eval_string_protect(s.as_c_str().as_ptr(), &mut state as *mut _)
    };

    match state {
        // Tag::None
        0 => Value::new(result).try_convert(),
        // Tag::Raise
        6 => unsafe {
            let ex = Exception::from_rb_value_unchecked(rb_errinfo());
            rb_set_errinfo(QNIL.as_rb_value());
            Err(Error::Exception(ex))
        },
        other => Err(Error::Jump(unsafe { transmute(other) })),
    }
}
