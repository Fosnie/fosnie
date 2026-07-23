// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

import { afterEach, describe, expect, it, vi } from "vitest";

const freshToken = vi.fn(async () => "identity-provider-token");
const serverAuthMode = vi.fn(() => "local" as "local" | "keycloak");

vi.mock("@/auth/keycloak", () => ({ freshToken: () => freshToken() }));
vi.mock("@/auth/config", () => ({ serverAuthMode: () => serverAuthMode() }));

const {
  apiUrl,
  authHeaders,
  authMode,
  configureInstance,
  credentialsMode,
  deviceMode,
  instanceBase,
  noteUnauthorised,
  setUnauthorisedHandler,
  wsBase,
} = await import("@/api/instance");

afterEach(() => {
  configureInstance(null);
  serverAuthMode.mockReturnValue("local");
  vi.unstubAllGlobals();
});

describe("instance base", () => {
  it("is empty until an instance is configured, keeping paths relative", () => {
    expect(deviceMode()).toBe(false);
    expect(instanceBase()).toBe("");
    expect(apiUrl("/api/whoami")).toBe("/api/whoami");
  });

  it("strips trailing slashes and surrounding whitespace", () => {
    configureInstance({ baseUrl: "  https://ai.example.com///  ", token: "t" });
    expect(instanceBase()).toBe("https://ai.example.com");
    expect(apiUrl("/api/whoami")).toBe("https://ai.example.com/api/whoami");
  });

  it("keeps a port and a path prefix", () => {
    configureInstance({ baseUrl: "http://localhost:8080/fosnie/", token: "t" });
    expect(apiUrl("/api/whoami")).toBe("http://localhost:8080/fosnie/api/whoami");
  });

  it("returns to relative paths when the instance is dropped", () => {
    configureInstance({ baseUrl: "https://ai.example.com", token: "t" });
    configureInstance(null);
    expect(deviceMode()).toBe(false);
    expect(apiUrl("/api/whoami")).toBe("/api/whoami");
  });
});

describe("authorisation", () => {
  it("sends the device token and no cookies once an instance is configured", async () => {
    configureInstance({ baseUrl: "https://ai.example.com", token: "sk-fosnie-abc" });
    expect(authMode()).toBe("device");
    expect(credentialsMode()).toBe("omit");
    expect((await authHeaders()).get("Authorization")).toBe("Bearer sk-fosnie-abc");
    expect(freshToken).not.toHaveBeenCalled();
  });

  it("sends an identity-provider token under external auth", async () => {
    serverAuthMode.mockReturnValue("keycloak");
    expect(authMode()).toBe("keycloak");
    expect(credentialsMode()).toBe("include");
    expect((await authHeaders()).get("Authorization")).toBe("Bearer identity-provider-token");
  });

  it("sends no token with built-in accounts — the session cookie carries it", async () => {
    expect(authMode()).toBe("local");
    expect(credentialsMode()).toBe("include");
    expect((await authHeaders()).has("Authorization")).toBe(false);
  });

  it("preserves headers the caller supplied", async () => {
    configureInstance({ baseUrl: "https://ai.example.com", token: "sk-fosnie-abc" });
    const headers = await authHeaders({ "Content-Type": "application/octet-stream" });
    expect(headers.get("Content-Type")).toBe("application/octet-stream");
    expect(headers.get("Authorization")).toBe("Bearer sk-fosnie-abc");
  });

  it("takes precedence over whatever the deployment's own auth mode is", async () => {
    serverAuthMode.mockReturnValue("keycloak");
    configureInstance({ baseUrl: "https://ai.example.com", token: "sk-fosnie-abc" });
    expect(authMode()).toBe("device");
    expect((await authHeaders()).get("Authorization")).toBe("Bearer sk-fosnie-abc");
  });
});

describe("socket endpoint", () => {
  it("follows the instance's scheme", () => {
    configureInstance({ baseUrl: "https://ai.example.com", token: "t" });
    expect(wsBase()).toBe("wss://ai.example.com/ws");
    configureInstance({ baseUrl: "http://localhost:8080", token: "t" });
    expect(wsBase()).toBe("ws://localhost:8080/ws");
  });

  it("follows the serving origin when no instance is configured", () => {
    vi.stubGlobal("window", { location: { protocol: "https:", host: "app.example.com" } });
    expect(wsBase()).toBe("wss://app.example.com/ws");
  });
});

describe("a refused device token", () => {
  afterEach(() => setUnauthorisedHandler(null));

  it("is reported once, however many screens notice it", () => {
    const handler = vi.fn();
    configureInstance({ baseUrl: "https://ai.example.com", token: "t" });
    setUnauthorisedHandler(handler);
    noteUnauthorised(401);
    noteUnauthorised(401);
    noteUnauthorised(401);
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("is not confused with the other ways a request can fail", () => {
    const handler = vi.fn();
    configureInstance({ baseUrl: "https://ai.example.com", token: "t" });
    setUnauthorisedHandler(handler);
    for (const status of [200, 204, 403, 404, 409, 500]) noteUnauthorised(status);
    expect(handler).not.toHaveBeenCalled();
  });

  it("is the browser's own business, not this seam's", () => {
    // In a browser a 401 means the session lapsed, which the sign-in flow
    // already handles. Only a device pairing ends here.
    const handler = vi.fn();
    setUnauthorisedHandler(handler);
    noteUnauthorised(401);
    expect(handler).not.toHaveBeenCalled();
  });

  it("can be reported again after the machine is paired afresh", () => {
    const handler = vi.fn();
    configureInstance({ baseUrl: "https://ai.example.com", token: "t" });
    setUnauthorisedHandler(handler);
    noteUnauthorised(401);
    // Pairing again re-arms the seam, exactly as the boot path does.
    configureInstance({ baseUrl: "https://ai.example.com", token: "t2" });
    setUnauthorisedHandler(handler);
    noteUnauthorised(401);
    expect(handler).toHaveBeenCalledTimes(2);
  });
});
