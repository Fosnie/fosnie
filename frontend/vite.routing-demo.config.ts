import { mergeConfig } from "vite";
import production from "./vite.config";
const target = process.env.FOSNIE_DEMO_BACKEND;
const token = process.env.FOSNIE_DEMO_TOKEN;
if (!target || !/^http:\/\/127\.0\.0\.1:\d+$/.test(target) || !token || token.length < 32) throw new Error("Private loopback demo required");
const headers = { Authorization: `Bearer ${token}` };
export default mergeConfig(production, { server: { host:"127.0.0.1", port:5190, strictPort:true, proxy: {
  "/api": {target,headers,changeOrigin:true}, "/demo": {target,headers,changeOrigin:true},
  "/health": {target,headers,changeOrigin:true}, "/ws": {target:target.replace(/^http/,"ws"),headers,ws:true,changeOrigin:true},
} } });
