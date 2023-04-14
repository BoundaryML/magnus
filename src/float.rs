use std::fmt;

use rb_sys::{
    rb_float_new_in_heap, rb_float_value, rb_flt_rationalize, rb_flt_rationalize_with_prec,
    rb_to_float, ruby_special_consts, ruby_value_type, VALUE,
};

#[cfg(ruby_use_flonum)]
use crate::value::Flonum;
use crate::{
    error::{protect, Error},
    into_value::IntoValue,
    numeric::Numeric,
    r_rational::RRational,
    try_convert::TryConvert,
    value::{
        private::{self, ReprValue as _},
        NonZeroValue, ReprValue, Value,
    },
    Ruby,
};

/// # `Float`
///
/// Functions that can be used to create instances of [`Float`].
///
/// See also the [`Float`] type.
#[allow(missing_docs)]
impl Ruby {
    #[inline]
    pub fn float_from_f64(&self, n: f64) -> Float {
        unsafe {
            #[cfg(ruby_use_flonum)]
            let val = Flonum::from_f64_impl(n)
                .map(|f| f.as_rb_value())
                .unwrap_or_else(|| rb_float_new_in_heap(n));

            #[cfg(not(ruby_use_flonum))]
            let val = rb_float_new_in_heap(n);

            Float::from_rb_value_unchecked(val)
        }
    }
}

/// A type wrapping either a [`Flonum`](`crate::value::Flonum`) value or a
/// Value known to be an instance of Float.
///
/// See the [`ReprValue`] trait for additional methods available on this type.
/// See [`Ruby`](Ruby#float) for methods to create a `Float`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Float(NonZeroValue);

impl Float {
    /// Return `Some(Float)` if `val` is a `Float`, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Float};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert!(Float::from_value(eval("1.7272337110188893e-77").unwrap()).is_some());
    /// assert!(Float::from_value(eval("1.7272337110188890e-77").unwrap()).is_some());
    /// ```
    #[inline]
    pub fn from_value(val: Value) -> Option<Self> {
        unsafe {
            if val.as_rb_value() & ruby_special_consts::RUBY_FLONUM_MASK as VALUE
                == ruby_special_consts::RUBY_FLONUM_FLAG as VALUE
            {
                return Some(Self(NonZeroValue::new_unchecked(val)));
            }
            debug_assert_value!(val);
            (val.rb_type() == ruby_value_type::RUBY_T_FLOAT)
                .then(|| Self(NonZeroValue::new_unchecked(val)))
        }
    }

    #[inline]
    pub(crate) unsafe fn from_rb_value_unchecked(val: VALUE) -> Self {
        Self(NonZeroValue::new_unchecked(Value::new(val)))
    }

    /// Create a new `Float` from an `f64`.
    ///
    /// # Panics
    ///
    /// Panics if called from a non-Ruby thread. See [`Ruby::float_from_f64`]
    /// for the non-panicking version.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Float};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let res: bool = eval!(
    ///     "f == 1.7272337110188893e-77",
    ///     f = Float::from_f64(1.7272337110188893e-77)
    /// )
    /// .unwrap();
    /// assert!(res);
    /// let res: bool = eval!(
    ///     "f == 1.7272337110188890e-77",
    ///     f = Float::from_f64(1.7272337110188890e-77)
    /// )
    /// .unwrap();
    /// assert!(res);
    /// ```
    #[cfg(feature = "friendly-api")]
    #[inline]
    pub fn from_f64(n: f64) -> Self {
        get_ruby!().float_from_f64(n)
    }

    /// Convert `self` to a `f64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::{eval, Float};
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// assert_eq!(eval::<Float>("2.0").unwrap().to_f64(), 2.0);
    /// ```
    #[inline]
    pub fn to_f64(self) -> f64 {
        #[cfg(ruby_use_flonum)]
        if let Some(flonum) = Flonum::from_value(self.as_value()) {
            return flonum.to_f64();
        }
        unsafe { rb_float_value(self.as_rb_value()) }
    }

    /// Returns a rational approximation of `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::Float;
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let pi = Float::from_f64(3.141592);
    /// assert_eq!(
    ///     pi.rationalize_with_prec(Float::from_f64(0.001)).to_string(),
    ///     "201/64"
    /// );
    /// assert_eq!(
    ///     pi.rationalize_with_prec(Float::from_f64(0.01)).to_string(),
    ///     "22/7"
    /// );
    /// assert_eq!(
    ///     pi.rationalize_with_prec(Float::from_f64(0.1)).to_string(),
    ///     "16/5"
    /// );
    /// assert_eq!(
    ///     pi.rationalize_with_prec(Float::from_f64(1.)).to_string(),
    ///     "3/1"
    /// );
    /// ```
    pub fn rationalize_with_prec(self, prec: Self) -> RRational {
        unsafe {
            RRational::from_rb_value_unchecked(rb_flt_rationalize_with_prec(
                self.as_rb_value(),
                prec.as_rb_value(),
            ))
        }
    }

    /// Returns a rational approximation of `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use magnus::Float;
    /// # let _cleanup = unsafe { magnus::embed::init() };
    ///
    /// let pi = Float::from_f64(3.141592);
    /// assert_eq!(pi.rationalize().to_string(), "392699/125000");
    /// ```
    pub fn rationalize(self) -> RRational {
        unsafe { RRational::from_rb_value_unchecked(rb_flt_rationalize(self.as_rb_value())) }
    }
}

impl fmt::Display for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inspect())
    }
}

impl IntoValue for Float {
    fn into_value_with(self, _: &Ruby) -> Value {
        self.0.get()
    }
}

unsafe impl private::ReprValue for Float {}

impl Numeric for Float {}

impl ReprValue for Float {}

impl TryConvert for Float {
    fn try_convert(val: Value) -> Result<Self, Error> {
        match Self::from_value(val) {
            Some(i) => Ok(i),
            None => protect(|| {
                debug_assert_value!(val);
                unsafe { Self::from_rb_value_unchecked(rb_to_float(val.as_rb_value())) }
            }),
        }
    }
}
