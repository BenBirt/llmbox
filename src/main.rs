use clap::Parser;
use std::ffi::c_void;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
enum LlmError {
    #[error("Anthropic API request failed: {0}")]
    Request(#[from] ureq::Error),
    #[error("Failed to parse Anthropic API response: {0}")]
    Parse(#[from] std::io::Error),
    #[error("Unexpected response shape from Anthropic API")]
    UnexpectedShape,
}

#[derive(Parser)]
#[command(about = "Execute LLM-generated JavaScript in a V8 sandbox")]
struct Args {
    /// Natural language prompt describing what the JavaScript should do
    prompt: String,

    /// Mount a host directory into the sandbox (format: /path:ro or /path:rw)
    #[arg(long = "mount", value_name = "PATH:MODE")]
    mounts: Vec<String>,

    /// Enable HTTP access from JavaScript (http.get, http.post)
    #[arg(long = "http")]
    http: bool,

    /// Expose a specific environment variable to JavaScript (can be repeated)
    #[arg(long = "env", value_name = "VAR_NAME")]
    env_vars: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum MountMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone)]
struct FilesystemMount {
    path: PathBuf,
    mode: MountMode,
}

impl FilesystemMount {
    fn parse(s: &str) -> Result<Self, String> {
        let (path_str, mode_str) = s
            .rsplit_once(':')
            .ok_or_else(|| format!("expected PATH:MODE, got {:?}", s))?;
        let mode = match mode_str {
            "ro" => MountMode::ReadOnly,
            "rw" => MountMode::ReadWrite,
            other => return Err(format!("unknown mount mode {:?}; expected ro or rw", other)),
        };
        Ok(FilesystemMount { path: path_str.into(), mode })
    }

    fn is_within_mount(&self, target: &std::path::Path) -> bool {
        let Ok(mount) = std::fs::canonicalize(&self.path) else {
            return false;
        };
        // For existing paths, canonicalize fully.
        // For new files (writes), canonicalize the parent and append the filename.
        let canonical_target = if target.exists() {
            match std::fs::canonicalize(target) {
                Ok(p) => p,
                Err(_) => return false,
            }
        } else {
            let parent = target.parent().unwrap_or(target);
            match std::fs::canonicalize(parent) {
                Ok(p) => p.join(target.file_name().unwrap_or_default()),
                Err(_) => return false,
            }
        };
        canonical_target.starts_with(&mount)
    }
}

// V8 callbacks must match `for<'s, 'i> Fn(&mut PinScope<'s, 'i>, FunctionCallbackArguments<'s>,
// ReturnValue<'s>)`. Stateless fn items with late-bound lifetimes satisfy this.

fn throw_error(scope: &mut v8::PinScope, msg: &str) {
    let s = v8::String::new(scope, msg).expect("out of memory");
    let exc = v8::Exception::error(scope, s);
    scope.throw_exception(exc);
}

fn console_log_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    _rv: v8::ReturnValue<'s>,
) {
    let parts: Vec<String> = (0..args.length())
        .map(|i| match args.get(i).to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => "<unstringifiable>".to_string(),
        })
        .collect();
    eprintln!("{}", parts.join(" "));
}

fn fs_read_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s>,
) {
    // SAFETY: The FilesystemMount pointer stored in the External outlives this callback because
    // SandboxConfig (which owns the mount) is held by main() and outlives every execute_js() call.
    let mount = unsafe {
        let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
            throw_error(scope, "fs.read: internal error: missing external data");
            return;
        };
        &*(ext.value() as *const FilesystemMount)
    };
    let Some(path_v8) = args.get(0).to_string(scope) else {
        throw_error(scope, "fs.read: path argument is not a string");
        return;
    };
    let path_str = path_v8.to_rust_string_lossy(scope);
    let target = mount.path.join(&path_str);
    if !mount.is_within_mount(&target) {
        throw_error(scope, "path escapes mount point");
        return;
    }
    match std::fs::read_to_string(&target) {
        Ok(contents) => {
            let Some(s) = v8::String::new(scope, &contents) else {
                throw_error(scope, "fs.read: out of memory creating result");
                return;
            };
            rv.set(s.into());
        }
        Err(e) => throw_error(scope, &e.to_string()),
    }
}

// Only registered when mount mode is ReadWrite.
fn fs_write_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s>,
) {
    // SAFETY: same as fs_read_callback.
    let mount = unsafe {
        let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
            throw_error(scope, "fs.write: internal error: missing external data");
            return;
        };
        &*(ext.value() as *const FilesystemMount)
    };
    let Some(path_v8) = args.get(0).to_string(scope) else {
        throw_error(scope, "fs.write: path argument is not a string");
        return;
    };
    let path_str = path_v8.to_rust_string_lossy(scope);
    let Some(contents_v8) = args.get(1).to_string(scope) else {
        throw_error(scope, "fs.write: content argument is not a string");
        return;
    };
    let contents = contents_v8.to_rust_string_lossy(scope);
    let target = mount.path.join(&path_str);
    if !mount.is_within_mount(&target) {
        throw_error(scope, "path escapes mount point");
        return;
    }
    match std::fs::write(&target, contents.as_bytes()) {
        Ok(()) => rv.set_undefined(),
        Err(e) => throw_error(scope, &e.to_string()),
    }
}

fn extract_headers<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    val: v8::Local<'s, v8::Value>,
) -> Vec<(String, String)> {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(val) else {
        return vec![];
    };
    let Some(names) = obj.get_own_property_names(scope, v8::GetPropertyNamesArgs::default()) else {
        return vec![];
    };
    (0..names.length())
        .filter_map(|i| {
            let key = names.get_index(scope, i)?;
            let val = obj.get(scope, key)?;
            Some((
                key.to_string(scope)?.to_rust_string_lossy(scope),
                val.to_string(scope)?.to_rust_string_lossy(scope),
            ))
        })
        .collect()
}

fn http_get_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s>,
) {
    let Some(url_v8) = args.get(0).to_string(scope) else {
        throw_error(scope, "http.get: url argument is not a string");
        return;
    };
    let url = url_v8.to_rust_string_lossy(scope);
    let mut req = ureq::get(&url);
    if args.length() >= 2 {
        let hval = args.get(1);
        if hval.is_object() {
            for (k, v) in extract_headers(scope, hval) {
                req = req.set(&k, &v);
            }
        }
    }
    match req.call() {
        Ok(resp) => match resp.into_string() {
            Ok(body) => {
                let Some(sv) = v8::String::new(scope, &body) else {
                    throw_error(scope, "http.get: out of memory creating result");
                    return;
                };
                rv.set(sv.into());
            }
            Err(e) => throw_error(scope, &e.to_string()),
        },
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            throw_error(scope, &format!("HTTP {}: {}", code, body));
        }
        Err(e) => throw_error(scope, &e.to_string()),
    }
}

fn http_post_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s>,
) {
    let Some(url_v8) = args.get(0).to_string(scope) else {
        throw_error(scope, "http.post: url argument is not a string");
        return;
    };
    let url = url_v8.to_rust_string_lossy(scope);
    let Some(body_v8) = args.get(1).to_string(scope) else {
        throw_error(scope, "http.post: body argument is not a string");
        return;
    };
    let body = body_v8.to_rust_string_lossy(scope);
    let mut req = ureq::post(&url);
    if args.length() >= 3 {
        let hval = args.get(2);
        if hval.is_object() {
            for (k, v) in extract_headers(scope, hval) {
                req = req.set(&k, &v);
            }
        }
    }
    match req.send_string(&body) {
        Ok(resp) => match resp.into_string() {
            Ok(b) => {
                let Some(sv) = v8::String::new(scope, &b) else {
                    throw_error(scope, "http.post: out of memory creating result");
                    return;
                };
                rv.set(sv.into());
            }
            Err(e) => throw_error(scope, &e.to_string()),
        },
        Err(ureq::Error::Status(code, resp)) => {
            let b = resp.into_string().unwrap_or_default();
            throw_error(scope, &format!("HTTP {}: {}", code, b));
        }
        Err(e) => throw_error(scope, &e.to_string()),
    }
}

fn env_get_callback<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s>,
) {
    // SAFETY: The EnvCapability pointer stored in the External outlives this callback because
    // SandboxConfig (which owns the capability) is held by main() and outlives every execute_js() call.
    let cap = unsafe {
        let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
            throw_error(scope, "env.get: internal error: missing external data");
            return;
        };
        &*(ext.value() as *const EnvCapability)
    };
    let Some(name_v8) = args.get(0).to_string(scope) else {
        throw_error(scope, "env.get: name argument is not a string");
        return;
    };
    let name = name_v8.to_rust_string_lossy(scope);
    if !cap.allowlist.contains(&name) {
        rv.set_null();
        return;
    }
    match std::env::var(&name) {
        Ok(val) => {
            let Some(sv) = v8::String::new(scope, &val) else {
                throw_error(scope, "env.get: out of memory creating result");
                return;
            };
            rv.set(sv.into());
        }
        Err(_) => rv.set_null(),
    }
}

/// A sandbox capability bundles two things that must stay in sync:
/// the V8 host functions it registers, and the system prompt text that describes them.
///
/// Adding a new capability requires only defining a new struct and implementing this trait —
/// no changes needed to SandboxConfig, run_loop, execute_js, or build_system_prompt.
trait Capability {
    fn system_prompt_snippet(&self) -> String;
    // Generic lifetime 's is late-bound, making this method object-safe for dyn Capability.
    fn register<'s>(
        &self,
        scope: &v8::PinScope<'s, '_, ()>,
        global: v8::Local<'s, v8::ObjectTemplate>,
    );
}

impl Capability for FilesystemMount {
    fn system_prompt_snippet(&self) -> String {
        let mut lines = vec![format!(
            "- `fs.read(path)` — reads the file at `path` (relative to `{}`) and returns its contents as a string.",
            self.path.display()
        )];
        if self.mode == MountMode::ReadWrite {
            lines.push(format!(
                "- `fs.write(path, content)` — writes `content` to the file at `path` (relative to `{}`).",
                self.path.display()
            ));
        }
        lines.join("\n")
    }

    fn register<'s>(
        &self,
        scope: &v8::PinScope<'s, '_, ()>,
        global: v8::Local<'s, v8::ObjectTemplate>,
    ) {
        // Store a raw pointer to self as V8 External data so the stateless callbacks can recover
        // the mount configuration. Safe: see SAFETY note in fs_read_callback.
        let data: v8::Local<v8::Value> =
            v8::External::new(scope, self as *const Self as *mut c_void).into();

        let fs_obj = v8::ObjectTemplate::new(scope);

        let read_fn = v8::FunctionTemplate::builder(fs_read_callback)
            .data(data)
            .build(scope);
        fs_obj.set(
            v8::String::new(scope, "read").expect("out of memory").into(),
            read_fn.into(),
        );

        if self.mode == MountMode::ReadWrite {
            let write_fn = v8::FunctionTemplate::builder(fs_write_callback)
                .data(data)
                .build(scope);
            fs_obj.set(
                v8::String::new(scope, "write").expect("out of memory").into(),
                write_fn.into(),
            );
        }

        global.set(v8::String::new(scope, "fs").expect("out of memory").into(), fs_obj.into());
    }
}

struct HttpCapability;

impl Capability for HttpCapability {
    fn system_prompt_snippet(&self) -> String {
        "\
- `http.get(url)` — performs an HTTP GET and returns the response body as a string. \
Throws on network error or non-2xx status.\n\
- `http.get(url, headers)` — same, with `headers` as an object of extra request headers.\n\
- `http.post(url, body)` — performs an HTTP POST with a string body, returns the response body.\n\
- `http.post(url, body, headers)` — same, with `headers` as an object of extra request headers."
            .to_string()
    }

    fn register<'s>(
        &self,
        scope: &v8::PinScope<'s, '_, ()>,
        global: v8::Local<'s, v8::ObjectTemplate>,
    ) {
        let http_obj = v8::ObjectTemplate::new(scope);
        let get_fn = v8::FunctionTemplate::new(scope, http_get_callback);
        http_obj.set(v8::String::new(scope, "get").expect("out of memory").into(), get_fn.into());
        let post_fn = v8::FunctionTemplate::new(scope, http_post_callback);
        http_obj.set(v8::String::new(scope, "post").expect("out of memory").into(), post_fn.into());
        global.set(v8::String::new(scope, "http").expect("out of memory").into(), http_obj.into());
    }
}

struct EnvCapability {
    allowlist: Vec<String>,
}

impl Capability for EnvCapability {
    fn system_prompt_snippet(&self) -> String {
        format!(
            "- `env.get(name)` — returns the value of environment variable `name` as a string, \
or `null` if it is not set. Only these variables are accessible: {}.",
            self.allowlist.iter().map(|v| format!("`{}`", v)).collect::<Vec<_>>().join(", ")
        )
    }

    fn register<'s>(
        &self,
        scope: &v8::PinScope<'s, '_, ()>,
        global: v8::Local<'s, v8::ObjectTemplate>,
    ) {
        let data: v8::Local<v8::Value> =
            v8::External::new(scope, self as *const Self as *mut c_void).into();
        let env_obj = v8::ObjectTemplate::new(scope);
        let get_fn = v8::FunctionTemplate::builder(env_get_callback).data(data).build(scope);
        env_obj.set(v8::String::new(scope, "get").expect("out of memory").into(), get_fn.into());
        global.set(v8::String::new(scope, "env").expect("out of memory").into(), env_obj.into());
    }
}

struct SandboxConfig {
    capabilities: Vec<Box<dyn Capability>>,
}

impl SandboxConfig {
    fn from_args(args: &Args) -> Result<Self, String> {
        let mut capabilities: Vec<Box<dyn Capability>> = Vec::new();
        for spec in &args.mounts {
            capabilities.push(Box::new(FilesystemMount::parse(spec)?));
        }
        if args.http {
            capabilities.push(Box::new(HttpCapability));
        }
        if !args.env_vars.is_empty() {
            capabilities.push(Box::new(EnvCapability { allowlist: args.env_vars.clone() }));
        }
        Ok(SandboxConfig { capabilities })
    }

    fn build_system_prompt(&self) -> String {
        const BASE: &str = "You are a helpful assistant with access to a JavaScript execution \
environment. When you need to compute something or run code, respond with a ```javascript code \
block. The code will be executed and the result fed back to you as the next user message. You can \
iterate — write code, observe the result, write more code if needed. When you have a final answer \
for the user, respond in plain text without a code block. The last expression in your JavaScript \
code will be its return value. You may use `console.log(...)` for debug output; it is printed to \
stderr and does not affect the return value.";

        let snippets: Vec<String> =
            self.capabilities.iter().map(|c| c.system_prompt_snippet()).collect();

        if snippets.is_empty() {
            BASE.to_string()
        } else {
            format!(
                "{}\n\nYour JavaScript sandbox has the following host APIs:\n{}",
                BASE,
                snippets.join("\n")
            )
        }
    }

    fn setup_context<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
    ) -> v8::Local<'s, v8::Context> {
        let global = v8::ObjectTemplate::new(scope);

        // Always register console.log.
        let console_obj = v8::ObjectTemplate::new(scope);
        let log_fn = v8::FunctionTemplate::new(scope, console_log_callback);
        console_obj.set(v8::String::new(scope, "log").expect("out of memory").into(), log_fn.into());
        global.set(v8::String::new(scope, "console").expect("out of memory").into(), console_obj.into());

        for cap in &self.capabilities {
            cap.register(scope, global);
        }

        v8::Context::new(
            scope,
            v8::ContextOptions { global_template: Some(global), ..Default::default() },
        )
    }
}

const MAX_ROUNDS: usize = 10;

fn main() {
    let args = Args::parse();
    let config = SandboxConfig::from_args(&args).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| {
        eprintln!("Error: ANTHROPIC_API_KEY environment variable not set");
        std::process::exit(1);
    });

    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let final_answer = run_loop(&api_key, &args.prompt, &config).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });
    println!("{}", final_answer);

    unsafe { v8::V8::dispose() };
    v8::V8::dispose_platform();
}

fn run_loop(api_key: &str, prompt: &str, config: &SandboxConfig) -> Result<String, LlmError> {
    let system_prompt = config.build_system_prompt();
    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({"role": "user", "content": prompt})];

    for _ in 0..MAX_ROUNDS {
        let response = query_llm(api_key, &messages, &system_prompt)?;
        messages.push(serde_json::json!({"role": "assistant", "content": response}));

        match extract_code_block(&response) {
            Some(code) => {
                let result = execute_js(&code, config);
                eprintln!("[js result: {}]", result);
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": format!("Result: {}", result)
                }));
            }
            None => return Ok(response),
        }
    }

    Ok(format!("Reached maximum rounds ({})", MAX_ROUNDS))
}

fn query_llm(api_key: &str, messages: &[serde_json::Value], system_prompt: &str) -> Result<String, LlmError> {
    let body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 1024,
        "system": system_prompt,
        "messages": messages,
    });

    let response: serde_json::Value = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json")
        .send_json(body)?
        .into_json()?;

    response["content"][0]["text"]
        .as_str()
        .ok_or(LlmError::UnexpectedShape)
        .map(str::to_string)
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

fn execute_js(js: &str, config: &SandboxConfig) -> String {
    // Fresh isolate + context every call — no JS state survives between executions.
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let handle_scope, isolate);

    let context = config.setup_context(handle_scope);
    let scope = &v8::ContextScope::new(handle_scope, context);

    let Some(code) = v8::String::new(scope, js) else {
        return "Error: out of memory creating V8 string".to_string();
    };

    let script = match v8::Script::compile(scope, code, None) {
        Some(s) => s,
        None => return "Error: JS compile error".to_string(),
    };

    match script.run(scope) {
        Some(val) => match val.to_string(scope) {
            Some(s) => s.to_rust_string_lossy(scope),
            None => String::new(),
        },
        None => "Error: JS runtime error".to_string(),
    }
}
