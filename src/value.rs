//! Types for working with Ruby's VALUE type, representing all objects, and 'immediate' values such as Fixnum.

use std::{
    borrow::Cow,
    ffi::CStr,
    fmt,
    mem::transmute,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    os::raw::{c_char, c_int, c_long, c_ulong},
    ptr,
};

use crate::{
    class::RClass,
    enumerator::Enumerator,
    error::{protect, Error},
    float::Float,
    integer::{Integer, IntegerType},
    module::Module,
    r_bignum::RBignum,
    r_float::RFloat,
    r_string::RString,
    ruby_sys::{
        rb_any_to_s, rb_cFalseClass, rb_cFloat, rb_cInteger, rb_cNilClass, rb_cSymbol,
        rb_cTrueClass, rb_enumeratorize_with_size, rb_float_new, rb_float_value, rb_funcallv,
        rb_gc_register_address, rb_gc_register_mark_object, rb_gc_unregister_address, rb_id2name,
        rb_id2sym, rb_inspect, rb_intern2, rb_ll2inum, rb_num2ll, rb_num2long, rb_num2short,
        rb_num2ull, rb_num2ulong, rb_num2ushort, rb_obj_as_string, rb_obj_classname, rb_obj_freeze,
        rb_obj_is_kind_of, rb_sym2id, rb_ull2inum, ruby_fl_type, ruby_special_consts,
        ruby_value_type, RBasic, ID, VALUE,
    },
    symbol::Symbol,
    try_convert::{ArgList, TryConvert, TryConvertOwned},
};

/// Debug assertation that the Value hasn't been garbage collected.
///
// This isn't infallible, if the original object was gc'd and that slot
// reused already this won't panic like it should, but we're trying our
// best here.
#[doc(hidden)]
#[macro_export]
macro_rules! debug_assert_value {
    ($value:expr) => {
        // The memory this points to is managed by Ruby's GC and we can't
        // really know if it's safe to access as with GC compaction this may
        // point to memory now outside that owned by the process. We will likly
        // segfault in that case, which is kind of OK, as we're trying to panic
        // anyway.
        #[allow(unused_unsafe)]
        #[cfg(debug_assertions)]
        match unsafe { $value.rb_type() } {
            crate::ruby_sys::ruby_value_type::RUBY_T_NONE
            | crate::ruby_sys::ruby_value_type::RUBY_T_ZOMBIE => {
                panic!("Attempting to access garbage collected Object")
            }
            #[cfg(ruby_gte_2_7)]
            crate::ruby_sys::ruby_value_type::RUBY_T_MOVED => {
                panic!("Attempting to access garbage collected Object")
            }
            _ => (),
        };
    };
}

/// Ruby's `VALUE` type, which can represent any Ruby object.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Value(VALUE);

impl Value {
    #[inline]
    pub(crate) const fn new(val: VALUE) -> Self {
        Self(val)
    }

    #[inline]
    pub(crate) unsafe fn r_basic_unchecked(self) -> ptr::NonNull<RBasic> {
        #[cfg(debug_assertions)]
        if self.is_immediate() {
            panic!("attempting to access immediate value as pointer");
        }
        ptr::NonNull::new_unchecked(self.0 as *mut RBasic)
    }

    /// Returns whether `self` is an 'immediate' value.
    ///
    /// 'immediate' values are encoded directly into the `Value` and require
    /// no additional lookup. They will never be garbage collected.
    ///
    /// non-immediate values are pointers to other memory holding the data for
    /// the object.
    #[inline]
    fn is_immediate(self) -> bool {
        let value_p = self.as_rb_value();
        let immediate_p = value_p & ruby_special_consts::RUBY_IMMEDIATE_MASK as VALUE != 0;
        let test = value_p & !(ruby_special_consts::RUBY_Qnil as VALUE) != 0;
        immediate_p || !test // special_const_p
    }

    #[inline]
    pub(crate) fn r_basic(self) -> Option<ptr::NonNull<RBasic>> {
        unsafe { (!self.is_immediate()).then(|| self.r_basic_unchecked()) }
    }

    #[inline]
    fn is_false(self) -> bool {
        self.as_rb_value() == ruby_special_consts::RUBY_Qfalse as VALUE
    }

    /// Returns whether `self` is Ruby's `nil` value.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(eval::<Value>("nil").unwrap().is_nil());
    /// assert!(!eval::<Value>("Object.new").unwrap().is_nil());
    /// assert!(!eval::<Value>("0").unwrap().is_nil());
    /// assert!(!eval::<Value>("[]").unwrap().is_nil());
    /// ```
    #[inline]
    pub fn is_nil(self) -> bool {
        self.as_rb_value() == ruby_special_consts::RUBY_Qnil as VALUE
    }

    #[inline]
    fn is_true(self) -> bool {
        self.as_rb_value() == ruby_special_consts::RUBY_Qtrue as VALUE
    }

    #[inline]
    pub(crate) fn is_undef(self) -> bool {
        self.as_rb_value() == ruby_special_consts::RUBY_Qundef as VALUE
    }

    #[inline]
    fn is_fixnum(self) -> bool {
        self.as_rb_value() & ruby_special_consts::RUBY_FIXNUM_FLAG as VALUE != 0
    }

    #[inline]
    pub(crate) fn is_static_symbol(self) -> bool {
        const MASK: usize = !(usize::MAX << ruby_special_consts::RUBY_SPECIAL_SHIFT as usize);
        self.as_rb_value() as usize & MASK == ruby_special_consts::RUBY_SYMBOL_FLAG as usize
    }

    #[inline]
    fn is_flonum(self) -> bool {
        self.as_rb_value() & ruby_special_consts::RUBY_FLONUM_MASK as VALUE
            == ruby_special_consts::RUBY_FLONUM_FLAG as VALUE
    }

    // derefs a raw pointer that under GC compaction may be outside the
    // process's memory space if the Value has been allowed to get GC'd
    pub(crate) fn rb_type(self) -> ruby_value_type {
        match self.r_basic() {
            Some(r_basic) => {
                unsafe {
                    let ret = r_basic.as_ref().flags & (ruby_value_type::RUBY_T_MASK as VALUE);
                    // this bit is safe, ruby_value_type is #[repr(u32)], the flags
                    // value set by Ruby, and Ruby promises that flags masked like
                    // this will always be a valid entry in this enum
                    std::mem::transmute(ret as u32)
                }
            }
            None => {
                if self.is_false() {
                    ruby_value_type::RUBY_T_FALSE
                } else if self.is_nil() {
                    ruby_value_type::RUBY_T_NIL
                } else if self.is_true() {
                    ruby_value_type::RUBY_T_TRUE
                } else if self.is_undef() {
                    ruby_value_type::RUBY_T_UNDEF
                } else if self.is_fixnum() {
                    ruby_value_type::RUBY_T_FIXNUM
                } else if self.is_static_symbol() {
                    ruby_value_type::RUBY_T_SYMBOL
                } else if self.is_flonum() {
                    ruby_value_type::RUBY_T_FLOAT
                } else {
                    unreachable!()
                }
            }
        }
    }

    /// Returns the class that `self` is an instance of.
    ///
    /// # Panics
    ///
    /// panics if self is `Qundef`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Value>("true").unwrap().class().inspect(), "TrueClass");
    /// assert_eq!(eval::<Value>("[1,2,3]").unwrap().class().inspect(), "Array");
    /// ```
    pub fn class(self) -> RClass {
        unsafe {
            match self.r_basic() {
                Some(r_basic) => RClass::from_rb_value_unchecked(r_basic.as_ref().klass),
                None => {
                    if self.is_false() {
                        RClass::from_rb_value_unchecked(rb_cFalseClass)
                    } else if self.is_nil() {
                        RClass::from_rb_value_unchecked(rb_cNilClass)
                    } else if self.is_true() {
                        RClass::from_rb_value_unchecked(rb_cTrueClass)
                    } else if self.is_undef() {
                        panic!("undef does not have a class")
                    } else if self.is_fixnum() {
                        RClass::from_rb_value_unchecked(rb_cInteger)
                    } else if self.is_static_symbol() {
                        RClass::from_rb_value_unchecked(rb_cSymbol)
                    } else if self.is_flonum() {
                        RClass::from_rb_value_unchecked(rb_cFloat)
                    } else {
                        unreachable!()
                    }
                }
            }
        }
    }

    #[inline]
    pub(crate) const fn as_rb_value(self) -> VALUE {
        self.0
    }

    /// Registers `self` as to never be garbage collected.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, RArray};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let ary = eval::<RArray>("[1, 2, 3]").unwrap();
    /// ary.leak();
    /// ```
    pub fn leak(self) {
        debug_assert_value!(self);
        // safe ffi to Ruby, call doesn't raise
        unsafe { rb_gc_register_mark_object(self.as_rb_value()) }
    }

    /// Returns whether `self` is 'frozen'.
    ///
    /// Ruby prevents modifying frozen objects.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(eval::<Value>(":foo").unwrap().is_frozen());
    /// assert!(eval::<Value>("42").unwrap().is_frozen());
    /// assert!(!eval::<Value>("[]").unwrap().is_frozen());
    /// ```
    pub fn is_frozen(self) -> bool {
        match self.r_basic() {
            None => true,
            Some(r_basic) => unsafe {
                r_basic.as_ref().flags & ruby_fl_type::RUBY_FL_FREEZE as VALUE != 0
            },
        }
    }

    /// Returns an error if `self` is 'frozen'.
    ///
    /// Useful for checking if an object is frozen in a function that would
    /// modify it.
    ///
    /// # Examples
    /// ```
    /// use magnus::{eval, Error, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// fn mutate(val: Value) -> Result<(), Error> {
    ///     val.check_frozen()?;
    ///
    ///     /// ...
    ///
    ///     Ok(())
    /// }
    ///
    /// assert!(mutate(eval("Object.new").unwrap()).is_ok());
    /// assert!(mutate(eval(":foo").unwrap()).is_err());
    /// ```
    pub fn check_frozen(self) -> Result<(), Error> {
        if self.is_frozen() {
            Err(Error::frozen_error(format!(
                "can't modify frozen {}",
                unsafe { self.classname() }
            )))
        } else {
            Ok(())
        }
    }

    /// Mark `self` as frozen.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, RArray};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let ary = RArray::new();
    /// assert!(!ary.is_frozen());
    /// ary.freeze();
    /// assert!(ary.is_frozen());
    /// ```
    pub fn freeze(self) {
        unsafe { rb_obj_freeze(self.as_rb_value()) };
    }

    /// Convert `self` to a `bool`, following Ruby's rules of `false` and `nil`
    /// as boolean `false` and everything else boolean `true`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(!eval::<Value>("false").unwrap().to_bool());
    /// assert!(!eval::<Value>("nil").unwrap().to_bool());
    ///
    /// assert!(eval::<Value>("true").unwrap().to_bool());
    /// assert!(eval::<Value>("0").unwrap().to_bool());
    /// assert!(eval::<Value>("[]").unwrap().to_bool());
    /// assert!(eval::<Value>(":foo").unwrap().to_bool());
    /// assert!(eval::<Value>("Object.new").unwrap().to_bool());
    /// ```
    #[inline]
    pub fn to_bool(self) -> bool {
        self.as_rb_value() & !(ruby_special_consts::RUBY_Qnil as VALUE) != 0
    }

    /// Call the method named `method` on `self` with `args`.
    ///
    /// Returns `Ok(T)` if the method returns without error and the return
    /// value converts to a `T`, or returns `Err` if the method raises or the
    /// conversion fails.
    ///
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, RArray};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let values = eval::<RArray>(r#"["foo", 1, :bar]"#).unwrap();
    /// let result: String = values.funcall("join", (" & ",)).unwrap();
    /// assert_eq!(result, "foo & 1 & bar");
    /// ```
    pub fn funcall<M, A, T>(self, method: M, args: A) -> Result<T, Error>
    where
        M: Into<Id>,
        A: ArgList,
        T: TryConvert,
    {
        unsafe {
            let id = method.into();
            let args = args.into_arg_list();
            let slice = args.as_ref();
            protect(|| {
                Value::new(rb_funcallv(
                    self.as_rb_value(),
                    id.as_rb_id(),
                    slice.len() as c_int,
                    slice.as_ptr() as *const VALUE,
                ))
            })
            .and_then(|v| v.try_convert())
        }
    }

    /// Convert `self` to a Ruby `String`.
    ///
    /// If `self` is already a `String` is it wrapped as a `RString`, otherwise
    /// the Ruby `to_s` method is called.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, class, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let value = eval::<Value>("[]").unwrap();
    /// assert!(value.to_r_string().unwrap().is_kind_of(class::string()));
    /// ```
    pub fn to_r_string(self) -> Result<RString, Error> {
        match RString::from_value(self) {
            Some(v) => Ok(v),
            None => unsafe {
                protect(|| Value::new(rb_obj_as_string(self.as_rb_value())))
                    .map(|v| RString::from_rb_value_unchecked(v.as_rb_value()))
            },
        }
    }

    /// Convert `self` to a Rust string.
    ///
    /// # Safety
    ///
    /// This may return a direct view of memory owned and managed by Ruby. Ruby
    /// may modify or free the memory backing the returned str, the caller must
    /// ensure this does not happen.
    ///
    /// This can be used safely by immediately calling
    /// [`into_owned`](Cow::into_owned) on the return value.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, QTRUE};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let value = QTRUE;
    /// // safe as we neve give Ruby a chance to free the string.
    /// let s = unsafe { value.to_s() }.unwrap().into_owned();
    /// assert_eq!(s, "true");
    /// ```
    #[allow(clippy::wrong_self_convention)]
    pub unsafe fn to_s(&self) -> Result<Cow<str>, Error> {
        if let Some(s) = RString::ref_from_value(self) {
            if s.is_utf8_compatible_encoding() {
                return s.as_str().map(Cow::Borrowed);
            } else {
                return (*s).to_string().map(Cow::Owned);
            }
        }
        self.to_r_string()
            .and_then(|s| s.to_string().map(Cow::Owned))
    }

    /// Convert `self` to a string. If an error is encountered returns a
    /// generic string (usually the object's class name).
    ///
    /// # Safety
    ///
    /// This may return a direct view of memory owned and managed by Ruby. Ruby
    /// may modify or free the memory backing the returned str, the caller must
    /// ensure this does not happen.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) unsafe fn to_s_infallible(&self) -> Cow<str> {
        match self.to_s() {
            Ok(v) => v,
            Err(_) => Cow::Owned(
                RString::from_rb_value_unchecked(rb_any_to_s(self.as_rb_value()))
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }

    /// Convert `self` to its Ruby debug representation.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Symbol, QNIL};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(QNIL.inspect(), "nil");
    /// assert_eq!(Symbol::new("foo").inspect(), ":foo");
    /// ```
    pub fn inspect(self) -> String {
        unsafe {
            let s = protect(|| Value::new(rb_inspect(self.as_rb_value())))
                .map(|v| RString::from_rb_value_unchecked(v.as_rb_value()))
                .unwrap_or_else(|_| {
                    RString::from_rb_value_unchecked(rb_any_to_s(self.as_rb_value()))
                });
            s.encode_utf8().unwrap_or(s).to_string_lossy().into_owned()
        }
    }

    /// Return the name of `self`'s class.
    ///
    /// # Safety
    ///
    /// Ruby may modify or free the memory backing the returned str, the caller
    /// must ensure this does not happen.
    ///
    /// This can be used safely by immediately calling
    /// [`into_owned`](Cow::into_owned) on the return value.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, RHash};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let value = RHash::new();
    /// // safe as we neve give Ruby a chance to free the string.
    /// let s = unsafe { value.classname() }.into_owned();
    /// assert_eq!(s, "Hash");
    /// ```
    pub unsafe fn classname(&self) -> Cow<str> {
        let ptr = rb_obj_classname(self.as_rb_value());
        let cstr = CStr::from_ptr(ptr);
        cstr.to_string_lossy()
    }

    /// Returns whether or not `self` is an instance of `class`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{class, eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let value = eval::<Value>("[]").unwrap();
    /// assert!(value.is_kind_of(class::array()));
    /// ```
    pub fn is_kind_of<T>(self, class: T) -> bool
    where
        T: Deref<Target = Value> + Module,
    {
        unsafe { Value::new(rb_obj_is_kind_of(self.as_rb_value(), class.as_rb_value())).to_bool() }
    }

    /// Generate an [`Enumerator`] from `method` on `self`, passing `args` to
    /// `method`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{class, eval, RString};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let s = RString::new("foo\\bar\\baz");
    /// let mut i = 0;
    /// for line in s.enumeratorize("each_line", ("\\",)) {
    ///     assert!(line.unwrap().is_kind_of(class::string()));
    ///     i += 1;
    /// }
    /// assert_eq!(i, 3);
    /// ```
    pub fn enumeratorize<M, A>(self, method: M, args: A) -> Enumerator
    where
        M: Into<Symbol>,
        A: ArgList,
    {
        let args = args.into_arg_list();
        let slice = args.as_ref();
        unsafe {
            Enumerator::from_rb_value_unchecked(rb_enumeratorize_with_size(
                self.as_rb_value(),
                method.into().as_rb_value(),
                slice.len() as c_int,
                slice.as_ptr() as *const VALUE,
                None,
            ))
        }
    }

    /// Convert `self` to the Rust type `T`.
    ///
    /// See the types that [`TryConvert`] is implemented on for what this
    /// method can convert to.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Value>("42").unwrap().try_convert::<i64>().unwrap(), 42);
    /// assert_eq!(eval::<Value>("1.23").unwrap().try_convert::<i64>().unwrap(), 1);
    /// assert_eq!(eval::<Value>("1").unwrap().try_convert::<f64>().unwrap(), 1.0);
    /// assert_eq!(eval::<Value>("nil").unwrap().try_convert::<Option<i64>>().unwrap(), None);
    /// assert_eq!(eval::<Value>("42").unwrap().try_convert::<Option<i64>>().unwrap(), Some(42));
    /// ```
    #[inline]
    pub fn try_convert<T>(&self) -> Result<T, Error>
    where
        T: TryConvert,
    {
        T::try_convert(self)
    }
}

impl Default for Value {
    fn default() -> Self {
        Value::new(ruby_special_consts::RUBY_Qnil as VALUE)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<i8> for Value {
    fn from(value: i8) -> Self {
        Integer::from_i64(value as i64).into()
    }
}

impl From<i16> for Value {
    fn from(value: i16) -> Self {
        Integer::from_i64(value as i64).into()
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Integer::from_i64(value as i64).into()
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Integer::from_i64(value).into()
    }
}

impl From<isize> for Value {
    fn from(value: isize) -> Self {
        Integer::from_i64(value as i64).into()
    }
}

impl From<u8> for Value {
    fn from(value: u8) -> Self {
        Integer::from_u64(value as u64).into()
    }
}

impl From<u16> for Value {
    fn from(value: u16) -> Self {
        Integer::from_u64(value as u64).into()
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Integer::from_u64(value as u64).into()
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Integer::from_u64(value).into()
    }
}

impl From<usize> for Value {
    fn from(value: usize) -> Self {
        Integer::from_u64(value as u64).into()
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Float::from_f64(value as f64).into()
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Float::from_f64(value).into()
    }
}

impl TryConvert for Value {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        Ok(*val)
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct NonZeroValue(NonZeroUsize);

impl NonZeroValue {
    #[inline]
    pub(crate) const unsafe fn new_unchecked(val: Value) -> Self {
        Self(NonZeroUsize::new_unchecked(val.as_rb_value() as usize))
    }

    pub(crate) const fn get(self) -> Value {
        Value::new(self.0.get() as VALUE)
    }

    pub(crate) fn get_ref(&self) -> &Value {
        let self_ptr = self as *const Self;
        let value_ptr = self_ptr as *const Value;
        // we just got this pointer from &self, so we know it's valid to deref
        unsafe { &*value_ptr }
    }
}

/// Protects a Ruby Value from the garbage collector.
///
/// See also [`Value::leak`] for a value that should be permanently excluded
/// from garbage collection.
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
pub struct BoxValue(Box<Value>);

impl BoxValue {
    /// Create a new `BoxValue`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, gc, value::BoxValue, RString, Value};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// # #[inline(never)]
    /// fn box_value() -> BoxValue {
    ///     BoxValue::new(*RString::new("foo"))
    /// }
    ///
    /// # // get the Value in a different stack frame and copy it to a BoxValue
    /// # // test is invalid if this is done in this function.
    /// let boxed = box_value();
    ///
    /// # // make some garbage
    /// # eval::<Value>(r#"1024.times.map {|i| "test#{i}"}"#).unwrap();
    /// // run garbage collector
    /// gc::start();
    ///
    /// # // try and use value
    /// // boxed is still useable
    /// let result: String = eval!(r#"foo + "bar""#, foo = boxed).unwrap();
    ///
    /// assert_eq!(result, "foobar");
    ///
    /// # // didn't segfault? we passed!
    /// ```
    pub fn new(val: Value) -> Self {
        debug_assert_value!(val);
        let mut boxed = Box::new(val);
        unsafe { rb_gc_register_address(boxed.as_mut() as *mut _ as *mut VALUE) };
        Self(boxed)
    }
}

impl Drop for BoxValue {
    fn drop(&mut self) {
        unsafe {
            rb_gc_unregister_address(self.0.as_mut() as *mut _ as *mut VALUE);
        }
    }
}

impl AsRef<Value> for BoxValue {
    fn as_ref(&self) -> &Value {
        &self.0
    }
}

impl AsMut<Value> for BoxValue {
    fn as_mut(&mut self) -> &mut Value {
        &mut self.0
    }
}

impl Deref for BoxValue {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for BoxValue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl fmt::Display for BoxValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for BoxValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<BoxValue> for Value {
    fn from(val: BoxValue) -> Self {
        *val
    }
}

/// Ruby's `false` value.
///
/// See [`QFALSE`] to obtain a value of this type.
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Qfalse(VALUE);

/// Ruby's `false` value.
pub const QFALSE: Qfalse = Qfalse::new();

impl Qfalse {
    /// Create a new `Qfalse`.
    #[inline]
    const fn new() -> Self {
        Qfalse(ruby_special_consts::RUBY_Qfalse as VALUE)
    }

    /// Return `Some(Qfalse)` if `val` is a `Qfalse`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, value::Qfalse};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Qfalse::from_value(eval("false").unwrap()).is_some());
    /// assert!(Qfalse::from_value(eval("0").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        val.is_false().then(Self::new)
    }
}

impl Deref for Qfalse {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        let self_ptr = self as *const Self;
        let value_ptr = self_ptr as *const Self::Target;
        // we just got this pointer from &self, so we know it's valid to deref
        unsafe { &*value_ptr }
    }
}

impl fmt::Display for Qfalse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Qfalse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Qfalse> for Value {
    fn from(val: Qfalse) -> Self {
        *val
    }
}

impl TryConvert for Qfalse {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        Self::from_value(*val).ok_or_else(|| {
            Error::type_error(format!(
                "no implicit conversion of {} into FalseClass",
                unsafe { val.classname() },
            ))
        })
    }
}
impl TryConvertOwned for Qfalse {}

/// Ruby's `nil` value.
///
/// See [`QNIL`] to obtain a value of this type.
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Qnil(NonZeroValue);

/// Ruby's `nil` value.
pub const QNIL: Qnil = Qnil::new();

impl Qnil {
    /// Create a new `Qnil`.
    #[inline]
    const fn new() -> Self {
        unsafe {
            Self(NonZeroValue::new_unchecked(Value::new(
                ruby_special_consts::RUBY_Qnil as VALUE,
            )))
        }
    }

    /// Return `Some(Qnil)` if `val` is a `Qnil`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, value::Qnil};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Qnil::from_value(eval("nil").unwrap()).is_some());
    /// assert!(Qnil::from_value(eval("0").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        val.is_nil().then(Self::new)
    }
}

impl Deref for Qnil {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl fmt::Display for Qnil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Qnil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Qnil> for Value {
    fn from(val: Qnil) -> Self {
        *val
    }
}

impl From<()> for Value {
    fn from(_: ()) -> Self {
        QNIL.into()
    }
}

impl<T> From<Option<T>> for Value
where
    T: Into<Value>,
{
    fn from(val: Option<T>) -> Self {
        match val {
            Some(t) => t.into(),
            None => QNIL.into(),
        }
    }
}

impl TryConvert for Qnil {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        Self::from_value(*val).ok_or_else(|| {
            Error::type_error(format!(
                "no implicit conversion of {} into NilClass",
                unsafe { val.classname() },
            ))
        })
    }
}
impl TryConvertOwned for Qnil {}

/// Ruby's `true` value.
///
/// See [`QTRUE`] to obtain a value of this type.
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Qtrue(NonZeroValue);

/// Ruby's `true` value.
pub const QTRUE: Qtrue = Qtrue::new();

impl Qtrue {
    /// Create a new `Qtrue`.
    #[inline]
    const fn new() -> Self {
        unsafe {
            Self(NonZeroValue::new_unchecked(Value::new(
                ruby_special_consts::RUBY_Qtrue as VALUE,
            )))
        }
    }

    /// Return `Some(Qtrue)` if `val` is a `Qtrue`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, value::Qtrue};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Qtrue::from_value(eval("true").unwrap()).is_some());
    /// assert!(Qtrue::from_value(eval("1").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        val.is_true().then(Self::new)
    }
}

impl Deref for Qtrue {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl fmt::Display for Qtrue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Qtrue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Qtrue> for Value {
    fn from(val: Qtrue) -> Self {
        *val
    }
}

impl From<bool> for Value {
    fn from(val: bool) -> Self {
        if val {
            QTRUE.into()
        } else {
            QFALSE.into()
        }
    }
}

impl TryConvert for Qtrue {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        Self::from_value(*val).ok_or_else(|| {
            Error::type_error(format!(
                "no implicit conversion of {} into TrueClass",
                unsafe { val.classname() },
            ))
        })
    }
}
impl TryConvertOwned for Qtrue {}

/// A placeholder value that represents an undefined value. Not exposed to
/// Ruby level code.
///
/// See [`QUNDEF`] to obtain a value of this type.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Qundef(NonZeroValue);

/// A placeholder value that represents an undefined value. Not exposed to
/// Ruby level code.
pub const QUNDEF: Qundef = Qundef::new();

impl Qundef {
    /// Create a new `Qundef`.
    #[inline]
    const fn new() -> Self {
        unsafe {
            Self(NonZeroValue::new_unchecked(Value::new(
                ruby_special_consts::RUBY_Qundef as VALUE,
            )))
        }
    }

    /// Return `Some(Qundef)` if `val` is a `Qundef`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, value::Qundef};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// // nil is not undef
    /// assert!(Qundef::from_value(eval("nil").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        val.is_undef().then(Self::new)
    }

    /// Return `self` as a [`Value`].
    ///
    /// # Safety
    ///
    /// It is not a good idea to return this to Ruby code, bad things will
    /// happen. There are only a handful of places in Ruby's API where it is
    /// appropriate to pass a [`Value`] created from `Qundef` (hence this
    /// method, rather than implimenting [`Into<Value>`]).
    #[inline]
    pub unsafe fn to_value(self) -> Value {
        self.0.get()
    }
}

/// A Value known to be a fixnum, Ruby's internal representation of small
/// integers.
///
/// See also [`Integer`].
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Fixnum(NonZeroValue);

impl Fixnum {
    /// Return `Some(Fixnum)` if `val` is a `Fixnum`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Fixnum::from_value(eval("0").unwrap()).is_some());
    /// // too big
    /// assert!(Fixnum::from_value(eval("9223372036854775807").unwrap()).is_none());
    /// // not an int
    /// assert!(Fixnum::from_value(eval("1.23").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        unsafe {
            val.is_fixnum()
                .then(|| Self(NonZeroValue::new_unchecked(val)))
        }
    }

    #[inline]
    pub(crate) unsafe fn from_rb_value_unchecked(val: VALUE) -> Self {
        Self(NonZeroValue::new_unchecked(Value::new(val)))
    }

    /// Create a new `Fixnum` from an `i64.`
    ///
    /// Returns `Ok(Fixnum)` if `n` is in range for `Fixnum`, otherwise returns
    /// `Err(RBignum)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Fixnum::from_i64(0).is_ok());
    /// // too big
    /// assert!(Fixnum::from_i64(4611686018427387904).is_err());
    /// assert!(Fixnum::from_i64(-4611686018427387905).is_err());
    /// ```
    pub fn from_i64(n: i64) -> Result<Self, RBignum> {
        let val = unsafe { Value::new(rb_ll2inum(n)) };
        Self::from_value(val)
            .ok_or_else(|| unsafe { RBignum::from_rb_value_unchecked(val.as_rb_value()) })
    }

    /// Create a new `Fixnum` from a `u64.`
    ///
    /// Returns `Ok(Fixnum)` if `n` is in range for `Fixnum`, otherwise returns
    /// `Err(RBignum)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Fixnum::from_u64(0).is_ok());
    /// // too big
    /// assert!(Fixnum::from_u64(4611686018427387904).is_err());
    /// ```
    pub fn from_u64(n: u64) -> Result<Self, RBignum> {
        let val = unsafe { Value::new(rb_ull2inum(n)) };
        Self::from_value(val)
            .ok_or_else(|| unsafe { RBignum::from_rb_value_unchecked(val.as_rb_value()) })
    }

    fn is_negative(self) -> bool {
        unsafe { transmute::<_, isize>(self.0) < 0 }
    }

    /// Convert `self` to an `i8`. Returns `Err` if `self` is out of range for
    /// `i8`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("127").unwrap().to_i8().unwrap(), 127);
    /// assert!(eval::<Fixnum>("128").unwrap().to_i8().is_err());
    /// assert_eq!(eval::<Fixnum>("-128").unwrap().to_i8().unwrap(), -128);
    /// assert!(eval::<Fixnum>("-129").unwrap().to_i8().is_err());
    /// ```
    pub fn to_i8(self) -> Result<i8, Error> {
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2long(self.as_rb_value()) };
            *QNIL
        })?;
        if res > i8::MAX as c_long || res < i8::MIN as c_long {
            return Err(Error::range_error("fixnum too big to convert into `i8`"));
        }
        Ok(res as i8)
    }

    /// Convert `self` to an `i16`. Returns `Err` if `self` is out of range for
    /// `i16`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("32767").unwrap().to_i16().unwrap(), 32767);
    /// assert!(eval::<Fixnum>("32768").unwrap().to_i16().is_err());
    /// assert_eq!(eval::<Fixnum>("-32768").unwrap().to_i16().unwrap(), -32768);
    /// assert!(eval::<Fixnum>("-32769").unwrap().to_i16().is_err());
    /// ```
    pub fn to_i16(self) -> Result<i16, Error> {
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2short(self.as_rb_value()) };
            *QNIL
        })?;
        Ok(res)
    }

    /// Convert `self` to an `i32`. Returns `Err` if `self` is out of range for
    /// `i32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("2147483647").unwrap().to_i32().unwrap(), 2147483647);
    /// # #[cfg(not(windows))]
    /// assert!(eval::<Fixnum>("2147483648").unwrap().to_i32().is_err());
    /// assert_eq!(eval::<Fixnum>("-2147483648").unwrap().to_i32().unwrap(), -2147483648);
    /// # #[cfg(not(windows))]
    /// assert!(eval::<Fixnum>("-2147483649").unwrap().to_i32().is_err());
    /// ```
    pub fn to_i32(self) -> Result<i32, Error> {
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2long(self.as_rb_value()) };
            *QNIL
        })?;
        if res > i32::MAX as c_long || res < i32::MIN as c_long {
            return Err(Error::range_error("fixnum too big to convert into `i32`"));
        }
        Ok(res as i32)
    }

    /// Convert `self` to an `i64`. This is infallible as `i64` can represent a
    /// larger range than `Fixnum`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("4611686018427387903").unwrap().to_i64(), 4611686018427387903);
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("-4611686018427387904").unwrap().to_i64(), -4611686018427387904);
    /// ```
    pub fn to_i64(self) -> i64 {
        unsafe { rb_num2ll(self.as_rb_value()) }
    }

    /// Convert `self` to an `isize`. Returns `Err` if `self` is out of range
    /// for `isize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("4611686018427387903").unwrap().to_isize().unwrap(), 4611686018427387903);
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("-4611686018427387904").unwrap().to_isize().unwrap(), -4611686018427387904);
    /// ```
    pub fn to_isize(self) -> Result<isize, Error> {
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2ll(self.as_rb_value()) };
            *QNIL
        })?;
        Ok(res as isize)
    }

    /// Convert `self` to a `u8`. Returns `Err` if `self` is negative or out of
    /// range for `u8`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("255").unwrap().to_u8().unwrap(), 255);
    /// assert!(eval::<Fixnum>("256").unwrap().to_u8().is_err());
    /// assert!(eval::<Fixnum>("-1").unwrap().to_u8().is_err());
    /// ```
    pub fn to_u8(self) -> Result<u8, Error> {
        if self.is_negative() {
            return Err(Error::range_error(
                "can't convert negative integer to unsigned",
            ));
        }
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2ulong(self.as_rb_value()) };
            *QNIL
        })?;
        if res > u8::MAX as c_ulong {
            return Err(Error::range_error("fixnum too big to convert into `u8`"));
        }
        Ok(res as u8)
    }

    /// Convert `self` to a `u16`. Returns `Err` if `self` is negative or out
    /// of range for `u16`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("65535").unwrap().to_u16().unwrap(), 65535);
    /// assert!(eval::<Fixnum>("65536").unwrap().to_u16().is_err());
    /// assert!(eval::<Fixnum>("-1").unwrap().to_u16().is_err());
    /// ```
    pub fn to_u16(self) -> Result<u16, Error> {
        if self.is_negative() {
            return Err(Error::range_error(
                "can't convert negative integer to unsigned",
            ));
        }
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2ushort(self.as_rb_value()) };
            *QNIL
        })?;
        Ok(res)
    }

    /// Convert `self` to a `u32`. Returns `Err` if `self` is negative or out
    /// of range for `u32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Fixnum>("4294967295").unwrap().to_u32().unwrap(), 4294967295);
    /// # #[cfg(not(windows))]
    /// assert!(eval::<Fixnum>("4294967296").unwrap().to_u32().is_err());
    /// assert!(eval::<Fixnum>("-1").unwrap().to_u32().is_err());
    /// ```
    pub fn to_u32(self) -> Result<u32, Error> {
        if self.is_negative() {
            return Err(Error::range_error(
                "can't convert negative integer to unsigned",
            ));
        }
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2ulong(self.as_rb_value()) };
            *QNIL
        })?;
        if res > u32::MAX as c_ulong {
            return Err(Error::range_error("fixnum too big to convert into `u32`"));
        }
        Ok(res as u32)
    }

    /// Convert `self` to a `u64`. Returns `Err` if `self` is negative.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("4611686018427387903").unwrap().to_u64().unwrap(), 4611686018427387903);
    /// assert!(eval::<Fixnum>("-1").unwrap().to_u64().is_err());
    /// ```
    pub fn to_u64(self) -> Result<u64, Error> {
        if self.is_negative() {
            return Err(Error::range_error(
                "can't convert negative integer to unsigned",
            ));
        }
        let mut res = 0;
        unsafe {
            protect(|| {
                res = rb_num2ull(self.as_rb_value());
                *QNIL
            })?;
        }
        Ok(res)
    }

    /// Convert `self` to a `usize`. Returns `Err` if `self` is negative or out
    /// of range for `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Fixnum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// # #[cfg(not(windows))]
    /// assert_eq!(eval::<Fixnum>("4611686018427387903").unwrap().to_usize().unwrap(), 4611686018427387903);
    /// assert!(eval::<Fixnum>("-1").unwrap().to_usize().is_err());
    /// ```
    pub fn to_usize(self) -> Result<usize, Error> {
        if self.is_negative() {
            return Err(Error::range_error(
                "can't convert negative integer to unsigned",
            ));
        }
        let mut res = 0;
        protect(|| {
            res = unsafe { rb_num2ull(self.as_rb_value()) };
            *QNIL
        })?;
        Ok(res as usize)
    }
}

impl Deref for Fixnum {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl fmt::Display for Fixnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Fixnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Fixnum> for Value {
    fn from(val: Fixnum) -> Self {
        *val
    }
}

impl TryConvert for Fixnum {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        match val.try_convert::<Integer>()?.integer_type() {
            IntegerType::Fixnum(fix) => Ok(fix),
            IntegerType::Bignum(_) => Err(Error::range_error("integer too big for fixnum")),
        }
    }
}
impl TryConvertOwned for Fixnum {}

/// A static Ruby symbol that will live for the life of the program and never
/// be garbage collected.
///
/// See also [`Symbol`].
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct StaticSymbol(NonZeroValue);

impl StaticSymbol {
    /// Return `Some(StaticSymbol)` if `val` is a `StaticSymbol`, `None`
    /// otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, StaticSymbol};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(StaticSymbol::from_value(eval(":foo").unwrap()).is_some());
    /// assert!(StaticSymbol::from_value(eval(r#""bar""#).unwrap()).is_none());
    /// assert!(StaticSymbol::from_value(eval(r#""baz".to_sym"#).unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        unsafe {
            val.is_static_symbol()
                .then(|| Self(NonZeroValue::new_unchecked(val)))
        }
    }

    #[inline]
    pub(crate) unsafe fn from_rb_value_unchecked(val: VALUE) -> Self {
        Self(NonZeroValue::new_unchecked(Value::new(val)))
    }

    /// Create a new StaticSymbol.
    ///
    /// # Examples
    /// ```
    /// use magnus::{eval, StaticSymbol};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let sym = StaticSymbol::new("example");
    /// let result: bool = eval!(":example == sym", sym).unwrap();
    /// assert!(result);
    /// ```
    #[inline]
    pub fn new<T: Into<Id>>(name: T) -> Self {
        name.into().into()
    }

    /// Return the symbol as a static string reference.
    ///
    /// May error if the name is not valid utf-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::StaticSymbol;
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let sym = StaticSymbol::new("example");
    /// assert_eq!(sym.name().unwrap(), "example");
    /// ```
    pub fn name(self) -> Result<&'static str, Error> {
        Id::from(self).name()
    }
}

impl Deref for StaticSymbol {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl fmt::Display for StaticSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for StaticSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Id> for StaticSymbol {
    fn from(id: Id) -> Self {
        unsafe { Self::from_rb_value_unchecked(rb_id2sym(id.as_rb_id())) }
    }
}

impl From<&str> for StaticSymbol {
    fn from(s: &str) -> Self {
        Id::from(s).into()
    }
}

impl From<String> for StaticSymbol {
    fn from(s: String) -> Self {
        Id::from(s).into()
    }
}

impl From<StaticSymbol> for Value {
    fn from(val: StaticSymbol) -> Self {
        *val
    }
}

impl TryConvert for StaticSymbol {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        val.try_convert::<Symbol>().map(|s| s.to_static())
    }
}
impl TryConvertOwned for StaticSymbol {}

/// The internal value of a Ruby symbol.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Id(ID);

impl Id {
    pub(crate) fn as_rb_id(self) -> ID {
        self.0
    }

    /// Return the symbol name associated with this Id as a static string
    /// reference.
    ///
    /// May error if the name is not valid utf-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::value::Id;
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let id = Id::from("example");
    /// assert_eq!(id.name().unwrap(), "example");
    /// ```
    pub fn name(self) -> Result<&'static str, Error> {
        unsafe {
            let ptr = rb_id2name(self.as_rb_id());
            let cstr = CStr::from_ptr(ptr);
            cstr.to_str()
                .map_err(|e| Error::encoding_error(e.to_string()))
        }
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Self(unsafe { rb_intern2(s.as_ptr() as *const c_char, s.len() as c_long) })
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        s.as_str().into()
    }
}

impl From<StaticSymbol> for Id {
    fn from(sym: StaticSymbol) -> Self {
        Self(unsafe { rb_sym2id(sym.as_rb_value()) })
    }
}

impl From<Symbol> for Id {
    fn from(sym: Symbol) -> Self {
        Self(unsafe { rb_sym2id(sym.as_rb_value()) })
    }
}

/// A Value known to be a flonum, Ruby's internal representation of lower
/// precision floating point numbers.
///
/// See also `Float`.
///
/// All [`Value`] methods should be available on this type through [`Deref`],
/// but some may be missed by this documentation.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Flonum(NonZeroValue);

impl Flonum {
    /// Return `Some(Flonum)` if `val` is a `Flonum`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Flonum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Flonum::from_value(eval("1.7272337110188893e-77").unwrap()).is_some());
    /// // representable as a Float, but Flonum does not have enough precision
    /// assert!(Flonum::from_value(eval("1.7272337110188890e-77").unwrap()).is_none());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        unsafe {
            val.is_flonum()
                .then(|| Self(NonZeroValue::new_unchecked(val)))
        }
    }

    #[inline]
    pub(crate) unsafe fn from_rb_value_unchecked(val: VALUE) -> Self {
        Self(NonZeroValue::new_unchecked(Value::new(val)))
    }

    /// Create a new `Flonum` from a `f64.`
    ///
    /// Returns `Ok(Flonum)` if `n` can be represented as a `Flonum`, otherwise
    /// returns `Err(RFloat)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Flonum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Flonum::from_f64(1.7272337110188893e-77).is_ok());
    /// // representable as a Float, but Flonum does not have enough precision
    /// assert!(Flonum::from_f64(1.7272337110188890e-77).is_err());
    /// ```
    pub fn from_f64(n: f64) -> Result<Self, RFloat> {
        let val = unsafe { Value::new(rb_float_new(n)) };
        Self::from_value(val)
            .ok_or_else(|| unsafe { RFloat::from_rb_value_unchecked(val.as_rb_value()) })
    }

    /// Convert `self` to a `f64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Flonum};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Flonum>("2.0").unwrap().to_f64(), 2.0);
    /// ```
    pub fn to_f64(self) -> f64 {
        unsafe { rb_float_value(self.as_rb_value()) }
    }
}

impl Deref for Flonum {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl fmt::Display for Flonum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Flonum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl From<Flonum> for Value {
    fn from(val: Flonum) -> Self {
        *val
    }
}

impl TryConvert for Flonum {
    #[inline]
    fn try_convert(val: &Value) -> Result<Self, Error> {
        let float = val.try_convert::<Float>()?;
        if let Some(flonum) = Flonum::from_value(*float) {
            Ok(flonum)
        } else {
            Err(Error::range_error("float out of range for flonum"))
        }
    }
}
impl TryConvertOwned for Flonum {}
