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

import { useEffect, useRef } from "react";

// Depth/parallax neural-network canvas (ported from the brand site's Layout).
// Three layers of gold nodes drifting over navy — far (slow, blurred, faint),
// mid, near (sharp). Per-node sine pulse; intra-layer synapse lines; click on
// empty space releases an expanding-ring impulse that nudges nearby nodes.
// Fills its parent; reduced-motion → one static frame; pauses when tab hidden.

const GOLD = { r: 197, g: 168, b: 128 };
const rgba = (a: number) => `rgba(${GOLD.r},${GOLD.g},${GOLD.b},${a})`;

interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
  phase: number;
  freq: number;
}
interface LayerCfg {
  count: number;
  speed: number;
  rMin: number;
  rMax: number;
  depth: number;
  conn: number;
  alpha: number;
  blur: number;
}
interface Layer extends LayerCfg {
  particles: Particle[];
}
interface Impulse {
  x: number;
  y: number;
  frame: number;
  life: number;
  strength: number;
  radius: number;
}

export function NeuralBackground() {
  const ref = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = ref.current;
    const parent = canvas?.parentElement;
    if (!canvas || !parent) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const g = ctx;

    let W = 0,
      H = 0,
      dpr = 1;
    let layers: Layer[] = [];
    let impulses: Impulse[] = [];
    let animId: number | null = null;
    let frame = 0;
    let alive = true;

    function resize() {
      const rect = parent!.getBoundingClientRect();
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      W = Math.max(1, rect.width);
      H = Math.max(1, rect.height);
      canvas!.width = W * dpr;
      canvas!.height = H * dpr;
      canvas!.style.width = `${W}px`;
      canvas!.style.height = `${H}px`;
      g.setTransform(dpr, 0, 0, dpr, 0, 0);
    }

    function makeLayer(cfg: LayerCfg): Layer {
      const ps: Particle[] = new Array(cfg.count);
      for (let i = 0; i < cfg.count; i++) {
        ps[i] = {
          x: Math.random() * W,
          y: Math.random() * H,
          vx: (Math.random() - 0.5) * cfg.speed,
          vy: (Math.random() - 0.5) * cfg.speed,
          r: cfg.rMin + Math.random() * (cfg.rMax - cfg.rMin),
          phase: Math.random() * Math.PI * 2,
          freq: 0.3 + Math.random() * 0.7,
        };
      }
      return { ...cfg, particles: ps };
    }

    function seed() {
      impulses = [];
      const area = (W * H) / (1280 * 720);
      const scale = W < 1024 ? 0.7 : 1;
      const cfgs: LayerCfg[] = [
        { count: Math.round(70 * area * scale), speed: 0.55, rMin: 1.0, rMax: 2.0, depth: 0.25, conn: 95, alpha: 0.14, blur: 2.2 },
        { count: Math.round(95 * area * scale), speed: 0.75, rMin: 0.8, rMax: 1.5, depth: 0.55, conn: 125, alpha: 0.26, blur: 0.6 },
        { count: Math.round(55 * area * scale), speed: 1.0, rMin: 0.5, rMax: 1.1, depth: 1.0, conn: 140, alpha: 0.52, blur: 0 },
      ];
      layers = cfgs.map(makeLayer);
    }

    function applyImpulses(p: Particle) {
      for (const imp of impulses) {
        const age = frame - imp.frame;
        if (age > imp.life) continue;
        const dx = p.x - imp.x;
        const dy = p.y - imp.y;
        const d = Math.sqrt(dx * dx + dy * dy) || 0.0001;
        const front = (age / imp.life) * imp.radius;
        const band = imp.radius * 0.3;
        const decay = 1 - age / imp.life;
        const delta = d - front;
        if (delta >= -band && delta <= band) {
          const w = 1 - Math.abs(delta) / band;
          const k = Math.min(imp.strength * w * decay, 6);
          p.x += (dx / d) * k;
          p.y += (dy / d) * k;
        } else if (d < front) {
          const k = Math.min(imp.strength * 0.85 * decay, 4);
          p.x += (dx / d) * k;
          p.y += (dy / d) * k;
        }
      }
    }

    function renderLayer(L: Layer) {
      const ps = L.particles;
      const M = 40;
      for (const p of ps) {
        p.x += p.vx;
        p.y += p.vy;
        if (p.x < -M) p.x += W + 2 * M;
        else if (p.x > W + M) p.x -= W + 2 * M;
        if (p.y < -M) p.y += H + 2 * M;
        else if (p.y > H + M) p.y -= H + 2 * M;
        applyImpulses(p);
      }
      const connSq = L.conn * L.conn;
      g.lineWidth = 0.5 + L.depth * 0.25;
      for (let i = 0; i < ps.length; i++) {
        const pi = ps[i];
        for (let j = i + 1; j < ps.length; j++) {
          const pj = ps[j];
          const dx = pi.x - pj.x;
          const dy = pi.y - pj.y;
          const d2 = dx * dx + dy * dy;
          if (d2 < connSq) {
            const t = 1 - Math.sqrt(d2) / L.conn;
            g.strokeStyle = rgba(L.alpha * 0.18 * t * t);
            g.beginPath();
            g.moveTo(pi.x, pi.y);
            g.lineTo(pj.x, pj.y);
            g.stroke();
          }
        }
      }
      if (L.blur > 0) {
        g.save();
        g.shadowColor = rgba(L.alpha);
        g.shadowBlur = L.blur * 4;
        g.fillStyle = rgba(L.alpha * 0.9);
        for (const p of ps) {
          const pulse = 0.85 + 0.15 * Math.sin(frame * 0.015 * p.freq + p.phase);
          g.beginPath();
          g.arc(p.x, p.y, p.r * pulse, 0, Math.PI * 2);
          g.fill();
        }
        g.restore();
      } else {
        g.fillStyle = rgba(L.alpha);
        g.beginPath();
        for (const p of ps) {
          const pulse = 0.92 + 0.08 * Math.sin(frame * 0.015 * p.freq + p.phase);
          g.moveTo(p.x + p.r * pulse, p.y);
          g.arc(p.x, p.y, p.r * pulse, 0, Math.PI * 2);
        }
        g.fill();
      }
    }

    function tick() {
      if (!alive) return;
      frame++;
      g.clearRect(0, 0, W, H);
      for (const L of layers) renderLayer(L);
      if (impulses.length) impulses = impulses.filter((imp) => frame - imp.frame <= imp.life);
      animId = requestAnimationFrame(tick);
    }

    function start() {
      if (!animId && alive) animId = requestAnimationFrame(tick);
    }
    function stop() {
      if (animId) {
        cancelAnimationFrame(animId);
        animId = null;
      }
    }

    resize();
    seed();

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduced) {
      g.clearRect(0, 0, W, H);
      for (const L of layers) renderLayer(L);
      return;
    }
    start();

    const onVisibility = () => (document.hidden ? stop() : start());
    document.addEventListener("visibilitychange", onVisibility);

    const onClick = (e: MouseEvent) => {
      const rect = canvas.getBoundingClientRect();
      impulses.push({
        x: e.clientX - rect.left,
        y: e.clientY - rect.top,
        frame,
        life: 55,
        strength: 3.2,
        radius: 220,
      });
    };
    canvas.addEventListener("click", onClick);

    let resizeTimer: ReturnType<typeof setTimeout>;
    const ro = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        resize();
        seed();
      }, 200);
    });
    ro.observe(parent);

    return () => {
      alive = false;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
      canvas.removeEventListener("click", onClick);
      ro.disconnect();
    };
  }, []);

  return <canvas ref={ref} className="block h-full w-full" aria-hidden="true" />;
}
