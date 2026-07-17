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

// Small glass dropdown replacing native <select> where we want the glass-era look
// and a slide-in animation (e.g. the Deep Research template picker). The menu is
// portalled (via <Popover>) so it escapes the glass composer's backdrop-filter root
// and frosts what's behind it; Popover also handles placement, outside-click + Esc.

import { useRef, useState, type ReactNode } from "react";
import { Popover } from "@/components/Popover";
import { Icon } from "@/components/icons";

export interface DropdownOption<T extends string> {
  value: T;
  label: string;
  /** Optional group header (rendered once when the group changes) — the glass
   *  equivalent of a native `<optgroup>`. Keep same-group options contiguous. */
  group?: string;
  /** Non-selectable option (shown muted). */
  disabled?: boolean;
}

export function Dropdown<T extends string>({
  value,
  options,
  onChange,
  ariaLabel,
  fullWidth = false,
  disabled = false,
  icon,
}: {
  value: T;
  options: DropdownOption<T>[];
  onChange: (v: T) => void;
  ariaLabel?: string;
  /** Stretch the trigger + menu to the container width (was `.dv-select full`). */
  fullWidth?: boolean;
  /** Disable the whole control. */
  disabled?: boolean;
  /** Optional leading glyph (the old `.dv-select` boxes often had one). */
  icon?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const cur = options.find((o) => o.value === value);

  return (
    <div className={"dd" + (fullWidth ? " dd-full" : "")}>
      <button
        ref={triggerRef}
        type="button"
        className="field dd-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        {icon && <span className="dd-ic">{icon}</span>}
        <span className="dd-val">{cur?.label ?? ""}</span>
        <Icon.Chevron size={14} className={"dd-chev" + (open ? " open" : "")} />
      </button>
      <Popover
        anchorRef={triggerRef}
        open={open}
        onClose={() => setOpen(false)}
        placement="bottom-start"
        matchWidth
        className="menu dd-menu glass glass--menu"
        role="listbox"
      >
        {options.map((o, i) => (
          <div key={o.value || `__${i}`}>
            {o.group && o.group !== options[i - 1]?.group && (
              <div className="dd-group mono">{o.group}</div>
            )}
            <button
              type="button"
              role="option"
              aria-selected={o.value === value}
              disabled={o.disabled}
              className={"dd-opt" + (o.value === value ? " on" : "") + (o.disabled ? " is-disabled" : "")}
              onClick={() => { if (!o.disabled) { onChange(o.value); setOpen(false); } }}
            >
              {o.label}
              {o.value === value && <Icon.Check size={14} />}
            </button>
          </div>
        ))}
      </Popover>
    </div>
  );
}
