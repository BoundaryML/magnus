use magnus::{RArray, Value};

#[test]
fn it_can_call_method_with_block() {
    let _cleanup = unsafe { magnus::embed::init() };

    let ary = RArray::from_slice(&[Value::from(1), Value::from(2), Value::from(3)]);
    let _: Value = ary
        .block_call("map!", (), |args, _| {
            args[0].try_convert::<i64>().map(|i| i * 4)
        })
        .unwrap();

    assert_eq!(ary.to_vec::<i64>().unwrap(), vec![4, 8, 12]);
}
