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
that routes Bazel's JVM through the proxy and redirects the registry.

Run this once per session (the JWT in `GLOBAL_AGENT_HTTP_PROXY` rotates):

```python
import base64, os, re

proxy = os.environ["GLOBAL_AGENT_HTTP_PROXY"]   # http://user:jwt@host:port
no_scheme = proxy[len("http://"):]
userinfo, hostport = no_scheme.rsplit("@", 1)
host, port = hostport.rsplit(":", 1)

lines = [
    "# Auto-generated — do not commit (gitignored)",
    "",
    "# bcr.bazel.build is blocked; use the GitHub mirror instead.",
    "common --registry=https://raw.githubusercontent.com/bazelbuild/bazel-central-registry/main",
    "",
    "# Route Bazel's JVM through the authenticated proxy.",
    "# (Bazel 9 ignores JAVA_TOOL_OPTIONS, so these must be startup flags.)",
    f"startup --host_jvm_args=-Dhttps.proxyHost={host}",
    f"startup --host_jvm_args=-Dhttps.proxyPort={port}",
    f"startup --host_jvm_args=-Dhttps.proxyUser={userinfo.split(':')[0]}",
    f"startup --host_jvm_args=-Dhttps.proxyPassword={userinfo.split(':', 1)[1]}",
    f"startup --host_jvm_args=-Dhttp.proxyHost={host}",
    f"startup --host_jvm_args=-Dhttp.proxyPort={port}",
    f"startup --host_jvm_args=-Dhttp.proxyUser={userinfo.split(':')[0]}",
    f"startup --host_jvm_args=-Dhttp.proxyPassword={userinfo.split(':', 1)[1]}",
    "startup --host_jvm_args=-Dhttp.nonProxyHosts=localhost|127.*|[::1]",
    "",
    "# JDK 8u111+ disables Basic auth for HTTPS CONNECT tunneling by default.",
    "startup --host_jvm_args=-Djdk.http.auth.tunneling.disabledSchemes=",
    "startup --host_jvm_args=-Djdk.http.auth.proxying.disabledSchemes=",
    "",
    "# The proxy performs TLS inspection; use the system trust store which",
    "# already trusts the proxy CA (Bazel's bundled JDK cacerts does not).",
    "startup --host_jvm_args=-Djavax.net.ssl.trustStore=/etc/ssl/certs/java/cacerts",
    "startup --host_jvm_args=-Djavax.net.ssl.trustStorePassword=changeit",
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
