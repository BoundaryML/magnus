use magnus::{
    define_class, define_global_variable, embed::init, eval_static, DataTypeFunctions, QNIL,
    TypedData, Value,
};

macro_rules! rb_assert {
    ($eval:literal) => {
        assert!(magnus::eval_static($eval).unwrap().to_bool())
    };
}

#[derive(DataTypeFunctions, TypedData)]
#[magnus(class = "Example", free_immediatly)]
struct Example {
    value: String,
}

fn make_rb_example(value: &str) -> Value {
    let ex = Example {
        value: value.to_owned(),
    };
    ex.into()
}

#[test]
fn it_wraps_rust_struct() {
    let _cleanup = unsafe { init() };

    define_class("Example", Default::default()).unwrap();

    let val = define_global_variable("$val", QNIL).unwrap();
    rb_assert!("$val == nil");

    unsafe { val.replace(make_rb_example("foo")) };
    rb_assert!("$val.class == Example");

    let value = eval_static("$val").unwrap();
    let ex = value.try_convert::<&Example>().unwrap();
    assert_eq!("foo", ex.value)
}
