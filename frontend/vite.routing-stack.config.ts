import { mergeConfig } from "vite";
import production from "./vite.config";
const target = process.env.FOSNIE_E2E_BACKEND;
if (!target || !/^http:\/\/127\.0\.0\.1:\d+$/.test(target)) throw new Error("Isolated loopback backend required");
export default mergeConfig(production, { server: { proxy: {
  "/api": { target, changeOrigin: true }, "/health": { target, changeOrigin: true },
  "/ws": { target: target.replace(/^http/, "ws"), ws: true, changeOrigin: true },
} } });
