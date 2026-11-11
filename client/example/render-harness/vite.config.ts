import { defineConfig } from "vite";

export default defineConfig({
  server: {
    // Loopback only, matching the server's allowed browser origins.
    host: "localhost",
    port: 5173,
    strictPort: true,
  },
});
