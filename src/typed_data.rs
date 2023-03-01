//! Types and Traits for wrapping Rust types as Ruby objects.
//!
//! This, along with [`RTypedData`], provides a Rust API to the
//! `rb_data_typed_object_wrap` function from Ruby's C API.

use std::{
    collections::hash_map::DefaultHasher,
    ffi::{c_void, CString},
    fmt,
    hash::Hasher,
    marker::PhantomData,
    mem::size_of_val,
    ops::Deref,
    panic::catch_unwind,
    ptr,
};

#[cfg(ruby_gte_3_0)]
use rb_sys::rbimpl_typeddata_flags::{self, RUBY_TYPED_FREE_IMMEDIATELY, RUBY_TYPED_WB_PROTECTED};
use rb_sys::{
    self, rb_data_type_struct__bindgen_ty_1, rb_data_type_t, rb_obj_reveal,
    rb_singleton_class_attached, rb_singleton_class_clone, size_t, VALUE,
};

#[cfg(ruby_lt_3_0)]
const RUBY_TYPED_FREE_IMMEDIATELY: u32 = 1;

#[cfg(ruby_lt_3_0)]
const RUBY_TYPED_WB_PROTECTED: u32 = rb_sys::ruby_fl_type::RUBY_FL_WB_PROTECTED as u32;

use crate::{
    class::RClass,
    error::{bug_from_panic, Error},
    into_value::IntoValue,
    object::Object,
    r_typed_data::RTypedData,
    scan_args::{get_kwargs, scan_args},
    try_convert::TryConvert,
    value::{
        private::{self, ReprValue as _},
        ReprValue, Value,
    },
    Ruby,
};

/// A C struct containing metadata on a Rust type, for use with the
/// `rb_data_typed_object_wrap` API.
#[repr(transparent)]
pub struct DataType(rb_data_type_t);

impl DataType {
    /// Create a new `DataTypeBuilder`.
    ///
    /// `name` should be unique per wrapped type. It does not need to be a
    /// valid Ruby identifier.
    pub fn builder<T>(name: &'static str) -> DataTypeBuilder<T>
    where
        T: DataTypeFunctions,
    {
        DataTypeBuilder::new(name)
    }

    pub(crate) fn as_rb_data_type(&self) -> &rb_data_type_t {
        &self.0
    }
}

impl Drop for DataType {
    fn drop(&mut self) {
        unsafe {
            drop(CString::from_raw(self.0.wrap_struct_name as *mut _));
        }
    }
}

/// A helper trait used to define functions associated with a [`DataType`].
pub trait DataTypeFunctions
where
    Self: Send + Sized,
{
    /// Called when the Ruby wrapper object is garbage collected.
    ///
    /// This can be implemented to perform Ruby-specific clean up when your
    /// type is no longer referenced from Ruby, but it is likely easier to do
    /// this in a [`Drop`] implementation for your type.
    ///
    /// This function will always be called by Ruby on GC, it can not be opted
    /// out of.
    ///
    /// The default implementation simply drops `self`.
    ///
    /// If this function (or the [`Drop`] implementation for your type) call
    /// Ruby APIs you should not enable the `free_immediately` flag with the
    /// [`wrap`](macro@crate::wrap)/[`TypedData`](macro@crate::TypedData)
    /// macro or [`DataTypeBuilder::free_immediately`].
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn free(self: Box<Self>) {}

    /// Called when Ruby marks this object as part of garbage collection.
    ///
    /// If your type contains any Ruby values you must mark each of those
    /// values in this function to avoid them being garbage collected.
    ///
    /// This function is only called when the `mark` flag is set with the
    /// [`wrap`](macro@crate::wrap)/[`TypedData`](macro@crate::TypedData)
    /// macro or [`DataTypeBuilder::mark`].
    ///
    /// The default implementation does nothing.
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn mark(&self) {}

    /// Called by Ruby to establish the memory size of this data, to optimise
    /// when garbage collection happens.
    ///
    /// This function is only called when the `size` flag is set with the
    /// [`wrap`](macro@crate::wrap)/[`TypedData`](macro@crate::TypedData)
    /// macro or [`DataTypeBuilder::mark`].
    ///
    /// The default implementation delegates to [`std::mem::size_of_val`].
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn size(&self) -> usize {
        size_of_val(self)
    }

    /// Called during garbage collection.
    ///
    /// If your type contains any Ruby values that you have marked as moveable
    /// in your [`mark`](Self::mark) function, you must update them in this
    /// function using [`gc::location`](crate::gc::location).
    ///
    /// Ruby values would be concidered moveable if marked with the
    /// [`gc::mark_movable`](crate::gc::mark_movable) function. Other marking
    /// functions such as [`gc::mark`](crate::gc::mark) will prevent values
    /// being moved.
    ///
    /// As it is only safe for this function to receive a shared `&self`
    /// reference, you must implement interior mutablility to be able to update
    /// values. This is very hard to do correctly, and it is recommended to
    /// simply avoid using [`gc::mark_movable`](crate::gc::mark_movable) and
    /// `compact`.
    ///
    /// This function is only called when the `compact` flag is set with the
    /// [`wrap`](macro@crate::wrap)/[`TypedData`](macro@crate::TypedData)
    /// macro or [`DataTypeBuilder::mark`].
    ///
    /// The default implementation does nothing.
    ///
    /// This function **must not** panic. The process will abort if this
    /// function panics.
    fn compact(&self) {}

    /// Extern wrapper for `free`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Box<Self>`, and must not be aliased
    /// This function will free the memory pointed to by `ptr`.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_free(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::free(Box::from_raw(ptr as *mut _))) {
            bug_from_panic(e, "panic in DataTypeFunctions::free")
        }
    }

    /// Extern wrapper for `mark`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`, and must not be aliased.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_mark(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::mark(&*(ptr as *mut Self))) {
            bug_from_panic(e, "panic in DataTypeFunctions::mark")
        }
    }

    /// Extern wrapper for `size`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_size(ptr: *const c_void) -> size_t {
        match catch_unwind(|| Self::size(&*(ptr as *const Self)) as size_t) {
            Ok(v) => v,
            Err(e) => bug_from_panic(e, "panic in DataTypeFunctions::size"),
        }
    }

    /// Extern wrapper for `compact`. Don't define or call.
    ///
    /// # Safety
    ///
    /// `ptr` must be a vaild pointer to a `Self`, and must not be aliased.
    ///
    /// This function must not panic.
    #[doc(hidden)]
    unsafe extern "C" fn extern_compact(ptr: *mut c_void) {
        if let Err(e) = catch_unwind(|| Self::compact(&*(ptr as *mut Self))) {
            bug_from_panic(e, "panic in DataTypeFunctions::compact")
        }
    }
}

/// A builder for [`DataType`].
pub struct DataTypeBuilder<T> {
    name: &'static str,
    mark: bool,
    size: bool,
    compact: bool,
    free_immediately: bool,
    wb_protected: bool,
    frozen_shareable: bool,
    phantom: PhantomData<T>,
}

impl<T> DataTypeBuilder<T>
where
    T: DataTypeFunctions,
{
    /// Create a new `DataTypeBuilder`.
    ///
    /// `name` should be unique per wrapped type. It does not need to be a
    /// valid Ruby identifier.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            mark: false,
            size: false,
            compact: false,
            free_immediately: false,
            wb_protected: false,
            frozen_shareable: false,
            phantom: Default::default(),
        }
    }

    /// Enable using the the `mark` function from `<T as DataTypeFunctions>`.
    pub fn mark(&mut self) {
        self.mark = true;
    }

    /// Enable using the the `size` function from `<T as DataTypeFunctions>`.
    pub fn size(&mut self) {
        self.size = true;
    }

    /// Enable using the the `compact` function from `<T as DataTypeFunctions>`.
    pub fn compact(&mut self) {
        self.compact = true;
    }

    /// Enable the 'free_immediately' flag.
    ///
    /// This is safe to do as long as the `<T as DataTypeFunctions>::free`
    /// function or `T`'s drop function don't call Ruby in any way.
    ///
    /// If safe this should be enabled as this performs better and is more
    /// memory efficient.
    pub fn free_immediately(&mut self) {
        self.free_immediately = true;
    }

    /// Enable the 'write barrier protected' flag.
    ///
    /// You almost certainly don't want to enable this.
    pub fn wb_protected(&mut self) {
        self.wb_protected = true;
    }

    /// Consume the builder and create a DataType.
    pub fn build(self) -> DataType {
        let mut flags = 0_usize as VALUE;
        if self.free_immediately {
            flags |= RUBY_TYPED_FREE_IMMEDIATELY as VALUE;
        }
        if self.wb_protected || !self.mark {
            flags |= RUBY_TYPED_WB_PROTECTED as VALUE;
        }
        #[cfg(ruby_gte_3_0)]
        if self.frozen_shareable {
            flags |= rbimpl_typeddata_flags::RUBY_TYPED_FROZEN_SHAREABLE as VALUE;
        }
        let dmark = self.mark.then(|| T::extern_mark as _);
        let dfree = Some(T::extern_free as _);
        let dsize = self.size.then(|| T::extern_size as _);
        #[cfg(ruby_gte_2_7)]
        let dcompact = self.compact.then(|| T::extern_compact as _);
        DataType(rb_data_type_t {
            wrap_struct_name: CString::new(self.name).unwrap().into_raw() as _,
            function: rb_data_type_struct__bindgen_ty_1 {
                dmark,
                dfree,
                dsize,
                #[cfg(ruby_gte_2_7)]
                dcompact,
                #[cfg(ruby_gte_2_7)]
                reserved: [ptr::null_mut(); 1],
                #[cfg(ruby_lt_2_7)]
                reserved: [ptr::null_mut(); 2],
            },
            parent: ptr::null(),
            data: ptr::null_mut(),
            flags,
        })
    }
}

impl<T> DataTypeBuilder<T>
where
    T: DataTypeFunctions + Sync,
{
    /// Enable the 'frozen_shareable' flag.
    ///
    /// Set this if your type is thread safe when the Ruby wrapper object is
    /// frozen.
    pub fn frozen_shareable(&mut self) {
        self.frozen_shareable = true;
    }
}

/// A trait for Rust types that can be used with the `rb_data_typed_object_wrap`
/// API.
///
/// # Safety
///
/// This trait is unsafe to implement as the fields of [`DataType`] returned by
/// [`TypedData::data_type`] control low level behaviour that can go very wrong
/// if set incorrectly. Implementing this trait is the only way a [`DataType`]
/// can be passed to Ruby and result in safety violations, [`DataType`] is
/// otherwise safe (but useless) to create.
///
/// The [`TypedData`](`derive@crate::TypedData`) or [`wrap`](`crate::wrap`) macros can
/// help implementing this trait more safely.
pub unsafe trait TypedData
where
    Self: Send + Sized,
{
    /// Should return the class for the Ruby object wrapping the Rust type.
    ///
    /// This can be overridden on a case by case basis by implementing
    /// [`TypedData::class_for`], but the result of this function will always
    /// be used in error messages if a value fails to convert to `Self`.
    ///
    /// If using [`class_for`](Self::class_for) it is advised to have this
    /// function return the superclass of those returned by `class_for`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{prelude::*, memoize, RClass, Ruby, TypedData};
    /// # use magnus::DataType;
    ///
    /// struct Example();
    ///
    /// unsafe impl TypedData for Example {
    ///     fn class(ruby: &Ruby) -> RClass {
    ///         *memoize!(RClass: {
    ///           let class = ruby.define_class("Example", ruby.class_object()).unwrap();
    ///           class.undef_alloc_func();
    ///           class
    ///         })
    ///     }
    ///
    ///     // ...
    /// #   fn data_type() -> &'static DataType { unimplemented!() }
    /// }
    /// ```
    fn class(ruby: &Ruby) -> RClass;

    /// Should return a static reference to a [`DataType`] with metadata about
    /// the wrapped type.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{memoize, typed_data::DataTypeBuilder, DataType, DataTypeFunctions, TypedData};
    /// # use magnus::{RClass, Ruby};
    ///
    /// #[derive(DataTypeFunctions)]
    /// struct Example();
    ///
    /// unsafe impl TypedData for Example {
    /// #   fn class(ruby: &Ruby) -> RClass { unimplemented!() }
    ///     // ...
    ///
    ///     fn data_type() -> &'static DataType {
    ///         memoize!(DataType: DataTypeBuilder::<Example>::new("example").build())
    ///     }
    /// }
    /// ```
    fn data_type() -> &'static DataType;

    /// Used to customise the class wrapping a specific value of `Self`.
    ///
    /// The provided implementation simply returns the value of
    /// [`TypedData::class`].
    ///
    /// Be aware [`TypedData::class`], will always be used in error messages if
    /// a value fails to convert to `Self`. It is advised to have
    /// [`TypedData::class`] return the superclass of those returned by this
    /// function.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{prelude::*, memoize, RClass, Ruby, TypedData};
    /// # use magnus::DataType;
    ///
    /// enum Example {
    ///     A,
    ///     B,
    /// }
    ///
    /// unsafe impl TypedData for Example {
    /// #   fn class(ruby: &Ruby) -> RClass { unimplemented!() }
    /// #   fn data_type() -> &'static DataType { unimplemented!() }
    ///     // ...
    ///
    ///     fn class_for(ruby: &Ruby, value: &Self) -> RClass {
    ///         match value {
    ///             Self::A => *memoize!(RClass: {
    ///                 let class = ruby.define_class("A", <Self as TypedData>::class(ruby)).unwrap();
    ///                 class.undef_alloc_func();
    ///                 class
    ///             }),
    ///             Self::B => *memoize!(RClass: {
    ///                 let class = ruby.define_class("B", <Self as TypedData>::class(ruby)).unwrap();
    ///                 class.undef_alloc_func();
    ///                 class
    ///             }),
    ///         }
    ///     }
    /// }
    /// ```
    #[allow(unused_variables)]
    fn class_for(ruby: &Ruby, value: &Self) -> RClass {
        Self::class(ruby)
    }
}

impl<T> TryConvert for &T
where
    T: TypedData,
{
    fn try_convert(val: Value) -> Result<Self, Error> {
        let handle = Ruby::get_with(val);
        unsafe {
            RTypedData::from_value(val)
                .ok_or_else(|| {
                    Error::new(
                        handle.exception_type_error(),
                        format!(
                            "no implicit conversion of {} into {}",
                            val.classname(),
                            T::class(&handle)
                        ),
                    )
                })?
                .get_unconstrained()
        }
    }
}

impl<T> IntoValue for T
where
    T: TypedData,
{
    fn into_value_with(self, handle: &Ruby) -> Value {
        handle.wrap(self).into_value_with(handle)
    }
}

/// A Ruby Object wrapping a Rust type `T`.
///
/// This is a Value pointer to a RTypedData struct, Ruby’s internal
/// representation of objects that wrap foreign types. Unlike [`RTypedData`] it
/// tracks the Rust type it should contains and errors early in [`TryConvert`]
/// if types don't match, rather than on [`Obj::get`].
///
/// See the [`ReprValue`] and [`Object`] traits for additional methods
/// available on this type.
#[repr(transparent)]
pub struct Obj<T> {
    inner: RTypedData,
    phantom: PhantomData<T>,
}

impl<T> Copy for Obj<T> where T: TypedData {}

impl<T> Clone for Obj<T>
where
    T: TypedData,
{
    fn clone(&self) -> Self {
        *self
    }
}

#[allow(missing_docs)]
impl Ruby {
    pub fn obj_wrap<T>(&self, data: T) -> Obj<T>
    where
        T: TypedData,
    {
        let inner = RTypedData::wrap(data);
        Obj {
            inner,
            phantom: PhantomData,
        }
    }
}

impl<T> Obj<T>
where
    T: TypedData,
{
    /// Wrap the Rust type `T` in a Ruby object.
    ///
    /// # Panics
    ///
    /// Panics if called from a non-Ruby thread.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{class, define_class, prelude::*, typed_data};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// #[magnus::wrap(class = "Point")]
    /// struct Point {
    ///     x: isize,
    ///     y: isize,
    /// }
    ///
    /// let point_class = define_class("Point", class::object()).unwrap();
    ///
    /// let value = typed_data::Obj::wrap(Point { x: 4, y: 2 });
    /// assert!(value.is_kind_of(point_class));
    /// ```
    #[inline]
    pub fn wrap(data: T) -> Self {
        get_ruby!().obj_wrap(data)
    }

    #[doc(hidden)]
    #[deprecated(
        since = "0.6.0",
        note = "Obj::get() is unnecessary, Obj<T> derefs to T"
    )]
    pub fn get(&self) -> &T {
        self.inner.get().unwrap()
    }
}

impl<T> Deref for Obj<T>
where
    T: TypedData,
{
    type Target = T;

    /// Dereference to the Rust type wrapped in the Ruby object `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{class, define_class, typed_data};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// #[magnus::wrap(class = "Point")]
    /// #[derive(Debug, PartialEq, Eq)]
    /// struct Point {
    ///     x: isize,
    ///     y: isize,
    /// }
    ///
    /// let point_class = define_class("Point", class::object()).unwrap();
    /// let value = typed_data::Obj::wrap(Point { x: 4, y: 2 });
    ///
    /// assert_eq!(&*value, &Point { x: 4, y: 2 });
    /// ```
    fn deref(&self) -> &Self::Target {
        self.inner.get().unwrap()
    }
}

impl<T> fmt::Display for Obj<T>
where
    T: TypedData,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl<T> fmt::Debug for Obj<T>
where
    T: TypedData,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl<T> IntoValue for Obj<T>
where
    T: TypedData,
{
    fn into_value_with(self, handle: &Ruby) -> Value {
        self.inner.into_value_with(handle)
    }
}

impl<T> From<Obj<T>> for RTypedData
where
    T: TypedData,
{
    fn from(val: Obj<T>) -> Self {
        val.inner
    }
}

impl<T> Object for Obj<T> where T: TypedData {}

unsafe impl<T> private::ReprValue for Obj<T> where T: TypedData {}

impl<T> ReprValue for Obj<T> where T: TypedData {}

impl<T> TryConvert for Obj<T>
where
    T: TypedData,
{
    fn try_convert(val: Value) -> Result<Self, Error> {
        let handle = Ruby::get_with(val);
        let inner = RTypedData::from_value(val).ok_or_else(|| {
            Error::new(
                handle.exception_type_error(),
                format!(
                    "no implicit conversion of {} into {}",
                    unsafe { val.classname() },
                    T::class(&handle)
                ),
            )
        })?;

        // check it really does contain a T
        inner.get::<T>()?;

        Ok(Self {
            inner,
            phantom: PhantomData,
        })
    }
}

/// Trait for a Ruby-compatible `#hash` method.
///
/// Automatically implemented for any type implementing [`std::hash::Hash`].
///
/// See also [`Dup`], [`Inspect`], [`IsEql`], and [`typed_data::Cmp`](Cmp).
///
/// # Examples
///
/// ```
/// use std::hash::Hasher;
///
/// use magnus::{
///     prelude::*, class, define_class, function, gc, method, typed_data, value::Opaque,
///     DataTypeFunctions, Error, IntoValue, RHash, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     #[magnus(opaque_attr_reader)]
///     a: Opaque<Value>,
///     #[magnus(opaque_attr_reader)]
///     b: Opaque<Value>,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a: a.into(), b: b.into() }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(self.a());
///         gc::mark(self.b());
///     }
/// }
///
/// impl std::hash::Hash for Pair {
///     fn hash<H: Hasher>(&self, state: &mut H) {
///         state.write_i64(
///             self.a()
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///         state.write_i64(
///             self.b()
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///     }
/// }
///
/// impl PartialEq for Pair {
///     fn eq(&self, other: &Self) -> bool {
///         self.a().eql(other.a()).unwrap_or(false) && self.b().eql(other.b()).unwrap_or(false)
///     }
/// }
///
/// impl Eq for Pair {}
///
/// # let _cleanup = unsafe { magnus::embed::init() };
/// #
/// let class = define_class("Pair", class::object()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("hash", method!(<Pair as typed_data::Hash>::hash, 0))
///     .unwrap();
/// class
///     .define_method("eql?", method!(<Pair as typed_data::IsEql>::is_eql, 1))
///     .unwrap();
///
/// let a = Pair::new("foo".into_value(), 1.into_value());
/// let hash = RHash::new();
/// hash.aset(a, "test value").unwrap();
///
/// let b = Pair::new("foo".into_value(), 1.into_value());
/// assert_eq!("test value", hash.fetch::<_, String>(b).unwrap());
///
/// let c = Pair::new("bar".into_value(), 2.into_value());
/// assert!(hash.get(c).is_none());
/// ```
pub trait Hash {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn hash(&self) -> i64;
}

impl<T> Hash for T
where
    T: std::hash::Hash,
{
    fn hash(&self) -> i64 {
        let mut hasher = DefaultHasher::new();
        std::hash::Hash::hash(self, &mut hasher);
        // Ensure the Rust usize hash converts nicely to Ruby's expected range
        // if we return usize it'd truncate to 0 for anything negative.
        hasher.finish() as i64
    }
}

/// Trait for a Ruby-compatible `#eql?` method.
///
/// Automatically implemented for any type implementing [`Eq`] and
/// [`TryConvert`].
///
/// See also [`Dup`], [`Inspect`], [`typed_data::Cmp`](Cmp), and
/// [`typed_data::Hash`](Hash).
///
/// # Examples
///
/// ```
/// use std::hash::Hasher;
///
/// use magnus::{
///     prelude::*, class, define_class, function, gc, method, typed_data, value::Opaque,
///     DataTypeFunctions, Error, IntoValue, RHash, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     #[magnus(opaque_attr_reader)]
///     a: Opaque<Value>,
///     #[magnus(opaque_attr_reader)]
///     b: Opaque<Value>,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a: a.into(), b: b.into() }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(self.a());
///         gc::mark(self.b());
///     }
/// }
///
/// impl std::hash::Hash for Pair {
///     fn hash<H: Hasher>(&self, state: &mut H) {
///         state.write_i64(
///             self.a()
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///         state.write_i64(
///             self.b()
///                 .hash()
///                 .expect("#hash should not fail")
///                 .to_i64()
///                 .expect("#hash result guaranteed to be <= i64"),
///         );
///     }
/// }
///
/// impl PartialEq for Pair {
///     fn eq(&self, other: &Self) -> bool {
///         self.a().eql(other.a()).unwrap_or(false) && self.b().eql(other.b()).unwrap_or(false)
///     }
/// }
///
/// impl Eq for Pair {}
///
/// # let _cleanup = unsafe { magnus::embed::init() };
/// #
/// let class = define_class("Pair", class::object()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("hash", method!(<Pair as typed_data::Hash>::hash, 0))
///     .unwrap();
/// class
///     .define_method("eql?", method!(<Pair as typed_data::IsEql>::is_eql, 1))
///     .unwrap();
///
/// let a = Pair::new("foo".into_value(), 1.into_value());
/// let hash = RHash::new();
/// hash.aset(a, "test value").unwrap();
///
/// let b = Pair::new("foo".into_value(), 1.into_value());
/// assert_eq!("test value", hash.fetch::<_, String>(b).unwrap());
///
/// let c = Pair::new("bar".into_value(), 2.into_value());
/// assert!(hash.get(c).is_none());
/// ```
pub trait IsEql {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn is_eql(&self, other: Value) -> bool;
}

impl<'a, T> IsEql for T
where
    T: Eq + 'a,
    &'a T: TryConvert,
{
    fn is_eql(&self, other: Value) -> bool {
        <&'a T>::try_convert(other)
            .map(|o| self == o)
            .unwrap_or(false)
    }
}

/// Trait for a Ruby-compatible `#<=>` method.
///
/// Automatically implemented for any type implementing [`PartialOrd`] and
/// [`TryConvert`].
///
/// See also [`Dup`], [`Inspect`], [`IsEql`] and [`typed_data::Hash`](Hash).
///
/// # Examples
///
/// ```
/// use std::cmp::Ordering;
///
/// use magnus::{
///     prelude::*, class, define_class, eval, function, gc, method, module, typed_data, value::Opaque,
///     DataTypeFunctions, Error, IntoValue, Module, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     #[magnus(opaque_attr_reader)]
///     a: Opaque<Value>,
///     #[magnus(opaque_attr_reader)]
///     b: Opaque<Value>,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a: a.into(), b: b.into() }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(self.a());
///         gc::mark(self.b());
///     }
/// }
///
/// impl PartialEq for Pair {
///     fn eq(&self, other: &Self) -> bool {
///         self.a().eql(other.a()).unwrap_or(false) && self.b().eql(other.b()).unwrap_or(false)
///     }
/// }
///
/// impl PartialOrd for Pair {
///     fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
///         let a = self.a().funcall("<=>", (other.a(),)).ok().map(|o: i64| o.cmp(&0))?;
///         match a {
///             Ordering::Less | Ordering::Greater => Some(a),
///             Ordering::Equal => self.b().funcall("<=>", (other.b(),)).ok().map(|o: i64| o.cmp(&0)),
///         }
///     }
/// }
///
/// # let _cleanup = unsafe { magnus::embed::init() };
/// #
/// let class = define_class("Pair", class::object()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("<=>", method!(<Pair as typed_data::Cmp>::cmp, 1))
///     .unwrap();
/// class.include_module(module::comparable()).unwrap();
///
/// let a = Pair::new("foo".into_value(), 1.into_value());
/// let b = Pair::new("foo".into_value(), 2.into_value());
/// let res: bool = eval!("a < b", a, b).unwrap();
/// assert!(res);
///
/// let b = Pair::new("foo".into_value(), 2.into_value());
/// let c = Pair::new("bar".into_value(), 3.into_value());
/// let res: bool = eval!("b > c", b, c).unwrap();
/// assert!(res);
///
/// let a = Pair::new("foo".into_value(), 1.into_value());
/// let b = Pair::new("foo".into_value(), 2.into_value());
/// let res: i64 = eval!("a <=> b", a, b).unwrap();
/// assert_eq!(res, -1);
/// ```
pub trait Cmp {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn cmp(&self, other: Value) -> Option<i64>;
}

impl<'a, T> Cmp for T
where
    T: PartialOrd + 'a,
    &'a T: TryConvert,
{
    fn cmp(&self, other: Value) -> Option<i64> {
        <&'a T>::try_convert(other)
            .ok()
            .and_then(|o| self.partial_cmp(o))
            .map(|o| o as i64)
    }
}

/// Trait for a Ruby-compatible `#inspect` method.
///
/// Automatically implemented for any type implementing [`Debug`].
///
/// See also [`Dup`], [`IsEql`], [`typed_data::Cmp`](Cmp), and
/// [`typed_data::Hash`](Hash).
///
/// # Examples
///
/// ```
/// use std::fmt;
///
/// use magnus::{
///     prelude::*, class, define_class, eval, function, gc, method, typed_data, value::Opaque,
///     DataTypeFunctions, IntoValue, TypedData, Value,
/// };
///
/// #[derive(TypedData)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     #[magnus(opaque_attr_reader)]
///     a: Opaque<Value>,
///     #[magnus(opaque_attr_reader)]
///     b: Opaque<Value>,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a: a.into(), b: b.into() }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(self.a());
///         gc::mark(self.b());
///     }
/// }
///
/// impl fmt::Debug for Pair {
///     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
///         f.debug_struct("Pair")
///          .field("a", &self.a())
///          .field("b", &self.b())
///          .finish()
///     }
/// }
///
/// # let _cleanup = unsafe { magnus::embed::init() };
/// #
/// let class = define_class("Pair", class::object()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("inspect", method!(<Pair as typed_data::Inspect>::inspect, 0))
///     .unwrap();
///
/// let pair = Pair::new("foo".into_value(), 1.into_value());
/// let res: bool = eval!(r#"pair.inspect == "Pair { a: \"foo\", b: 1 }""#, pair).unwrap();
/// assert!(res);
/// ```
pub trait Inspect {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn inspect(&self) -> String;
}

impl<T> Inspect for T
where
    T: fmt::Debug,
{
    fn inspect(&self) -> String {
        format!("{:?}", self)
    }
}

/// Trait for a Ruby-compatible `#dup` and `#clone` methods.
///
/// Automatically implemented for any type implementing [`Clone`].
///
/// See also [`Inspect`], [`IsEql`], [`typed_data::Cmp`](Cmp), and
/// [`typed_data::Hash`](Hash).
///
/// # Examples
///
/// ```
/// use magnus::{
///     prelude::*, class, define_class, eval, function, gc, method, typed_data, value::Opaque,
///     DataTypeFunctions, IntoValue, TypedData, Value,
/// };
///
/// #[derive(TypedData, Clone)]
/// #[magnus(class = "Pair", free_immediatly, mark)]
/// struct Pair {
///     #[magnus(opaque_attr_reader)]
///     a: Opaque<Value>,
///     #[magnus(opaque_attr_reader)]
///     b: Opaque<Value>,
/// }
///
/// impl Pair {
///     fn new(a: Value, b: Value) -> Self {
///         Self { a: a.into(), b: b.into() }
///     }
/// }
///
/// impl DataTypeFunctions for Pair {
///     fn mark(&self) {
///         gc::mark(self.a());
///         gc::mark(self.b());
///     }
/// }
///
/// # let _cleanup = unsafe { magnus::embed::init() };
/// #
/// let class = define_class("Pair", class::object()).unwrap();
/// class
///     .define_singleton_method("new", function!(Pair::new, 2))
///     .unwrap();
/// class
///     .define_method("dup", method!(<Pair as typed_data::Dup>::dup, 0))
///     .unwrap();
/// class
///     .define_method("clone", method!(<Pair as typed_data::Dup>::clone, -1))
///     .unwrap();
///
/// let a = Pair::new("foo".into_value(), 1.into_value());
/// let res: bool = eval!("b = a.dup; a.object_id != b.object_id", a).unwrap();
/// assert!(res);
/// ```
pub trait Dup: Sized {
    // Docs at trait level.
    #![allow(missing_docs)]
    fn dup(&self) -> Self;
    fn clone(rbself: Obj<Self>, args: &[Value]) -> Result<Obj<Self>, Error>;
}

impl<T> Dup for T
where
    T: Clone + TypedData,
{
    fn dup(&self) -> Self {
        self.clone()
    }

    fn clone(rbself: Obj<Self>, args: &[Value]) -> Result<Obj<Self>, Error> {
        let args = scan_args::<(), (), (), (), _, ()>(args)?;
        let kwargs =
            get_kwargs::<_, (), (Option<Option<bool>>,), ()>(args.keywords, &[], &["freeze"])?;
        let (freeze,) = kwargs.optional;
        let freeze = freeze.flatten();

        let clone = Obj::wrap((*rbself).clone());
        let class_clone = unsafe { rb_singleton_class_clone(rbself.as_rb_value()) };
        unsafe { rb_obj_reveal(clone.as_rb_value(), class_clone) };
        unsafe { rb_singleton_class_attached(class_clone, clone.as_rb_value()) };
        match freeze {
            Some(true) => clone.freeze(),
            None if rbself.is_frozen() => clone.freeze(),
            _ => (),
        }
        Ok(clone)
    }
}
