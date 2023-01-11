use crate::{ruby_handle::RubyHandle, value::Value};

impl RubyHandle {
    pub fn into_value<T>(&self, val: T) -> Value
    where
        T: IntoValue,
    {
        val.into_value(self)
    }
}

pub trait IntoValue {
    fn into_value(self, handle: &RubyHandle) -> Value;
}
