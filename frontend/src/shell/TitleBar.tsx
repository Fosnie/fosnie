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

// The window's frame, where the application draws it.
//
// Windows only. macOS keeps its own traffic lights, which are a platform
// convention and not ours to replace; Linux keeps whatever the desktop draws.
// The band is a drag region, so it moves the window and a double-click on it
// maximises, exactly as a system title bar would; the controls sit inside it and
// are excluded from dragging by not carrying the attribute themselves.
//
// Closing asks rather than ends: the client answers a close request by hiding to
// the tray, because the socket it holds outlives the window.

import { useEffect, useState } from "react";
import { Icon } from "@/components/icons";
import {
  closeWindow,
  isWindowMaximised,
  minimiseWindow,
  onWindowResized,
  toggleMaximiseWindow,
} from "@/shell/bridge";

export function TitleBar() {
  const [maximised, setMaximised] = useState(false);

  // The window can be maximised and restored without these controls — the
  // system's own keyboard shortcuts do it, and so does a double-click on the
  // band — so the glyph follows the window rather than the last button pressed.
  useEffect(() => {
    let live = true;
    let stop: (() => void) | undefined;
    const read = () => {
      void isWindowMaximised()
        .then((v) => {
          if (live) setMaximised(v);
        })
        .catch(() => {});
    };
    read();
    void onWindowResized(read)
      .then((unlisten) => {
        if (live) stop = unlisten;
        else unlisten();
      })
      .catch(() => {});
    return () => {
      live = false;
      stop?.();
    };
  }, []);

  return (
    <div className="titlebar" data-tauri-drag-region>
      <span className="titlebar-name" data-tauri-drag-region>
        Fosnie
      </span>
      <div className="titlebar-controls">
        <button
          type="button"
          className="titlebar-btn"
          aria-label="Minimise"
          onClick={() => void minimiseWindow()}
        >
          <Icon.Minimise size={15} />
        </button>
        <button
          type="button"
          className="titlebar-btn"
          aria-label={maximised ? "Restore" : "Maximise"}
          onClick={() => void toggleMaximiseWindow()}
        >
          {maximised ? <Icon.Restore size={13} /> : <Icon.Maximise size={13} />}
        </button>
        <button
          type="button"
          className="titlebar-btn titlebar-btn--close"
          aria-label="Close"
          onClick={() => void closeWindow()}
        >
          <Icon.Close size={15} />
        </button>
      </div>
    </div>
  );
}
