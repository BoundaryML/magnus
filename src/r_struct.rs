use std::{fmt, ops::Deref};

use crate::{
    object::Object,
    r_basic::RBasic,
    ruby_sys::{ruby_value_type, VALUE},
    value::Value,
};

#[repr(transparent)]
pub struct RStruct(VALUE);

impl RStruct {
    /// # Safety
    ///
    /// val must not have been GC'd, return value must be kept on stack or
    /// otherwise protected from the GC.
    pub unsafe fn from_value(val: &Value) -> Option<Self> {
        let r_basic = RBasic::from_value(val)?;
        (r_basic.builtin_type() == ruby_value_type::RUBY_T_STRUCT).then(|| Self(val.into_inner()))
    }
}

impl Deref for RStruct {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        let self_ptr = self as *const Self;
        let value_ptr = self_ptr as *const Self::Target;
        // we just got this pointer from &self, so we know it's valid to deref
        unsafe { &*value_ptr }
    }
}

impl fmt::Display for RStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.to_s_infallible() })
    }
}

impl fmt::Debug for RStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe { self.inspect() })
    }
}

impl From<RStruct> for Value {
    fn from(val: RStruct) -> Self {
        *val
    }
}

impl Object for RStruct {}
