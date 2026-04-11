use clap::Parser;

#[derive(Parser)]
#[command(about = "Execute LLM-generated JavaScript in a V8 sandbox")]
struct Args {
    /// Natural language prompt describing what the JavaScript should do
    prompt: String,
}

fn main() {
    let args = Args::parse();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .expect("ANTHROPIC_API_KEY environment variable not set");

    let js_code = query_llm(&api_key, &args.prompt);
    let js_code = extract_code(&js_code);
    let result = execute_js(&js_code);
    println!("{}", result);
}

fn query_llm(api_key: &str, prompt: &str) -> String {
    let body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 1024,
        "system": "You are a JavaScript code generator. When given a task, respond with only the JavaScript code to accomplish it — no explanation, no markdown fences. The last expression's value will be printed as the result.",
        "messages": [{"role": "user", "content": prompt}]
    });

    let response: serde_json::Value = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(body)
        .expect("Anthropic API request failed")
        .into_json()
        .expect("Failed to parse Anthropic API response");

    response["content"][0]["text"]
        .as_str()
        .expect("Unexpected response shape from Anthropic API")
        .to_string()
}

fn extract_code(text: &str) -> String {
    let text = text.trim();
    // Strip opening fence (```javascript, ```js, or ```)
    let text = if let Some(rest) = text.strip_prefix("```") {
        let after_lang = rest
            .find('\n')
            .map(|i| &rest[i + 1..])
            .unwrap_or(rest);
        after_lang
    } else {
        text
    };
    // Strip closing fence
    let text = if let Some(pos) = text.rfind("```") {
        text[..pos].trim_end()
    } else {
        text.trim_end()
    };
    text.to_string()
}

fn execute_js(js: &str) -> String {
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let result = {
        let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
        v8::scope!(let handle_scope, isolate);

        let context = v8::Context::new(handle_scope, Default::default());
        let scope = &v8::ContextScope::new(handle_scope, context);

        let code = v8::String::new(scope, js).expect("Failed to create V8 string");

        let script = match v8::Script::compile(scope, code, None) {
            Some(s) => s,
            None => {
                eprintln!("JS compile error");
                std::process::exit(1);
            }
        };

        match script.run(scope) {
            Some(val) => {
                let s = val.to_string(scope).unwrap();
                s.to_rust_string_lossy(scope)
            }
            None => {
                eprintln!("JS runtime error");
                std::process::exit(1);
            }
        }
    };

    unsafe { v8::V8::dispose() };
    v8::V8::dispose_platform();

    result
}
