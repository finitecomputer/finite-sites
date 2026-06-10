import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // Standalone output is the deployable artifact for Finite Sites tier 2:
  // a self-contained server.js + pruned node_modules, started with
  // `node server.js` listening on $PORT.
  output: "standalone",
};

export default nextConfig;
