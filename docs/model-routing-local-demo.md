# Interactive local routing demo

The `routing_local_demo` development example uses an explicit isolated marker,
loopback URLs, a dedicated routing_demo database/schema, private storage and a
random bearer token injected by the local Vite proxy. It mounts the actual Core
HTTP/WS implementation and product Chat/RoutingSettings components. Its synthetic
identity is exclusive to this development example; production authentication is
unchanged. The wider Core/Enterprise acceptance remains separate.

The prepared Windows workspace launcher has already connected the cached Qwen 4B
and Llama 3B, an isolated Ollama instance and disposable PostgreSQL/Redis. No cloud
credentials, model downloads or production changes were needed. Manual and Auto
answers, local judge audit and persistence after reload were exercised. Profiles
remain declared; this is not P12 quality admission or a financial-savings result.

The launcher and operator guide are workspace artifacts, not a portable installer.
On another host supply its own cached models, runtimes and isolated services before
running this example. Never point it at a production database or reuse a real
identity. Start/stop controls retain ownership checks. Stopping the disposable
demo destroys its chat history/configuration, and leaves the main model store intact.
The current demo scope is general text without web, documents, speech or cloud.
