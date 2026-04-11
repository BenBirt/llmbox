use clap::Parser;

#[derive(Parser)]
#[command(about = "Execute LLM-generated JavaScript in a V8 sandbox")]
struct Args {
    /// Natural language prompt describing what the JavaScript should do
    prompt: String,
}

const SYSTEM_PROMPT: &str = "You are a helpful assistant with access to a JavaScript execution environment. \
When you need to compute something or run code, respond with a ```javascript code block. \
The code will be executed and the result fed back to you as the next user message. \
You can iterate — write code, observe the result, write more code if needed. \
When you have a final answer for the user, respond in plain text without a code block. \
The last expression in your JavaScript code will be its return value.";

const MAX_ROUNDS: usize = 10;

fn main() {
    let args = Args::parse();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .expect("ANTHROPIC_API_KEY environment variable not set");

    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let final_answer = run_loop(&api_key, &args.prompt);
    println!("{}", final_answer);

    unsafe { v8::V8::dispose() };
    v8::V8::dispose_platform();
}

fn run_loop(api_key: &str, prompt: &str) -> String {
    let mut messages: Vec<serde_json::Value> = vec![
        serde_json::json!({"role": "user", "content": prompt}),
    ];

    for _ in 0..MAX_ROUNDS {
        let response = query_llm(api_key, &messages);
        messages.push(serde_json::json!({"role": "assistant", "content": response}));

        match extract_code_block(&response) {
            Some(code) => {
                let result = execute_js(&code);
                eprintln!("[js result: {}]", result);
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": format!("Result: {}", result)
                }));
            }
            None => return response,
        }
    }

    format!("Reached maximum rounds ({})", MAX_ROUNDS)
}

fn query_llm(api_key: &str, messages: &[serde_json::Value]) -> String {
    let body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 1024,
        "system": SYSTEM_PROMPT,
        "messages": messages,
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

/// Returns `Some(code)` if the response contains a JS code block, `None` if it is a plain-text
/// final answer.
fn extract_code_block(text: &str) -> Option<String> {
    let text = text.trim();
    let start = text.find("```")?;
    let after_fence = &text[start + 3..];
    // Skip optional language tag line (e.g. "javascript\n" or "js\n")
    let code_start = after_fence.find('\n').map(|i| i + 1)?;
    let code_body = &after_fence[code_start..];
    // Find closing fence
    let end = code_body.rfind("```")?;
    Some(code_body[..end].trim_end().to_string())
}

fn execute_js(js: &str) -> String {
    // Fresh isolate + context every call — no JS state survives between executions.
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let handle_scope, isolate);

    let context = v8::Context::new(handle_scope, Default::default());
    let scope = &v8::ContextScope::new(handle_scope, context);

    let code = v8::String::new(scope, js).expect("Failed to create V8 string");

    let script = match v8::Script::compile(scope, code, None) {
        Some(s) => s,
        None => return "Error: JS compile error".to_string(),
    };

    match script.run(scope) {
        Some(val) => {
            let s = val.to_string(scope).unwrap();
            s.to_rust_string_lossy(scope)
        }
        None => "Error: JS runtime error".to_string(),
    }
}
