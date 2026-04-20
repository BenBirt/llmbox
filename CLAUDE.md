# llmbox — build instructions for Claude Code

## Building

```
bazel build //:llmbox
bazel run   //:llmbox
```

The `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` environment variables
must be set so that the v8 crate's build script uses the prebuilt static library
checked in under `third_party/v8/` rather than downloading it from GitHub.
If direnv is active these are already exported; otherwise set them manually:

```
export RUSTY_V8_ARCHIVE="$PWD/third_party/v8/librusty_v8_release_x86_64-unknown-linux-gnu.a.gz"
export RUSTY_V8_SRC_BINDING_PATH="$PWD/third_party/v8/src_binding_release_x86_64-unknown-linux-gnu.rs"
```

## Network access (Anthropic sandbox)

Bazel downloads modules from `bcr.bazel.build`, which is **not** in this
environment's proxy allowlist.  Before the first build, write a `.bazelrc.local`
that redirects the registry and configures the JVM trust store.

If an authenticated proxy is in use, `GLOBAL_AGENT_HTTP_PROXY` will be set
(the JWT rotates each session); the script below handles both cases.

Run this once per session:

```python
import os

proxy = os.environ.get("GLOBAL_AGENT_HTTP_PROXY", "")

lines = [
    "# Auto-generated — do not commit (gitignored)",
    "",
    "# bcr.bazel.build is blocked; use the GitHub mirror instead.",
    "common --registry=https://raw.githubusercontent.com/bazelbuild/bazel-central-registry/main",
    "",
    "# The sandbox performs TLS inspection; use the system trust store which",
    "# already trusts the proxy CA (Bazel's bundled JDK cacerts does not).",
    "startup --host_jvm_args=-Djavax.net.ssl.trustStore=/etc/ssl/certs/java/cacerts",
    "startup --host_jvm_args=-Djavax.net.ssl.trustStorePassword=changeit",
]

if proxy:
    no_scheme = proxy[len("http://"):]
    userinfo, hostport = no_scheme.rsplit("@", 1)
    host, port = hostport.rsplit(":", 1)
    user, password = userinfo.split(":", 1)
    lines += [
        "",
        "# Route Bazel's JVM through the authenticated proxy.",
        "# (Bazel 9 ignores JAVA_TOOL_OPTIONS, so these must be startup flags.)",
        f"startup --host_jvm_args=-Dhttps.proxyHost={host}",
        f"startup --host_jvm_args=-Dhttps.proxyPort={port}",
        f"startup --host_jvm_args=-Dhttps.proxyUser={user}",
        f"startup --host_jvm_args=-Dhttps.proxyPassword={password}",
        f"startup --host_jvm_args=-Dhttp.proxyHost={host}",
        f"startup --host_jvm_args=-Dhttp.proxyPort={port}",
        f"startup --host_jvm_args=-Dhttp.proxyUser={user}",
        f"startup --host_jvm_args=-Dhttp.proxyPassword={password}",
        "startup --host_jvm_args=-Dhttp.nonProxyHosts=localhost|127.*|[::1]",
        "",
        "# JDK 8u111+ disables Basic auth for HTTPS CONNECT tunneling by default.",
        "startup --host_jvm_args=-Djdk.http.auth.tunneling.disabledSchemes=",
        "startup --host_jvm_args=-Djdk.http.auth.proxying.disabledSchemes=",
    ]

with open(".bazelrc.local", "w") as f:
    f.write("\n".join(lines) + "\n")

print("Wrote .bazelrc.local")
```

After writing `.bazelrc.local`, restart the Bazel server so it picks up the new
startup flags:

```
bazel shutdown
```

Then build normally.
