use magnus::{eval, Error};

#[test]
fn it_includes_backtrace_in_debug() {
    let _cleanup = unsafe { magnus::embed::init() };

    let err = eval::<magnus::Value>(
        r#"
            def foo
              raise "bang"
            end

            def bar
              foo
            end

            def baz
              bar
            end

            def qux
              baz
            end

            qux
        "#,
    )
    .unwrap_err();

    let ex = match err {
        Error::Exception(e) => e,
        _ => panic!(),
    };

    assert_eq!(
        r#"RuntimeError: bang
eval:3:in `foo'
eval:7:in `bar'
eval:11:in `baz'
eval:15:in `qux'
eval:18:in `<main>'
"#,
        format!("{:#?}", ex)
    );
}
