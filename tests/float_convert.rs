use magnus::prelude::*;

#[test]
fn it_converts_floats() {
    let _cleanup = unsafe { magnus::init() };

    let val = magnus::eval_static("1.0").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert_eq!(res.ok().unwrap(), 1.0);

    let val = magnus::eval_static("1").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert_eq!(res.ok().unwrap(), 1.0);

    let val = magnus::eval_static("1/2r").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert_eq!(res.ok().unwrap(), 0.5);

    let val = magnus::eval_static("18446744073709551615").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert_eq!(res.ok().unwrap(), 18446744073709552000.0);

    let val = magnus::eval_static(r#""1.0""#).ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert!(res.is_err());

    let val = magnus::eval_static("Object.new").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert!(res.is_err());

    let val = magnus::eval_static("nil").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert!(res.is_err());

    let val = magnus::eval_static("Float::INFINITY").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert_eq!(res.ok().unwrap(), f64::INFINITY);

    let val = magnus::eval_static("Float::NAN").ok().unwrap();
    let res = unsafe { f64::try_convert(val) };
    assert!(res.ok().unwrap().is_nan());
}
