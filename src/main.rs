fn main() {
    // Initialize V8.
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    {
        let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
        v8::scope!(let handle_scope, isolate);

        let context = v8::Context::new(handle_scope, Default::default());
        let scope = &v8::ContextScope::new(handle_scope, context);

        // Run a small JavaScript snippet inside the V8 isolate.
        let v8_version = v8::V8::get_version();
        let js = format!(
            r#"
            const greet = (name) => `Hello from ${{name}}!`;
            `${{greet("llmbox")}} (V8 {v8_version})`;
            "#
        );

        let code = v8::String::new(scope, &js).unwrap();
        let script = v8::Script::compile(scope, code, None).unwrap();
        let result = script.run(scope).unwrap();
        let result = result.to_string(scope).unwrap();
        println!("{}", result.to_rust_string_lossy(scope));
    }

    unsafe { v8::V8::dispose() };
    v8::V8::dispose_platform();
}
