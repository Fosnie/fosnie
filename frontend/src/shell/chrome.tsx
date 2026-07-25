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

// Mounting the window's frame.
//
// It goes in a root of its own, above the application's. Every screen the client
// can show has to have a frame around it — the pairing screen and the sign-in
// screen are rendered instead of the application, not within it, and a frame
// mounted inside the application would leave those two undraggable and
// uncloseable. One mount, outside all of them, is the only placement that holds
// for every screen.
//
// Windows only: the client asks for no system decorations there and nowhere
// else, so this is the only platform where anything is missing to draw.

import { createRoot } from "react-dom/client";
import { shellInfo } from "@/shell/bridge";
import { TitleBar } from "@/shell/TitleBar";

export async function mountFrame(): Promise<void> {
  let platform: string;
  try {
    ({ platform } = await shellInfo());
  } catch {
    // A client that cannot say what it is running on is left with whatever the
    // system drew; an application-drawn frame that failed to appear would be a
    // window nobody can move.
    return;
  }
  if (platform !== "windows") return;

  const host = document.getElementById("titlebar");
  if (!host) return;
  // Read by the stylesheet to make room for the band: the application fills the
  // window, and without this the last strip of it would sit under the frame.
  document.documentElement.dataset.frame = "app";
  createRoot(host).render(<TitleBar />);
}
