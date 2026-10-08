import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

const entry = (path) => fileURLToPath(new URL(path, import.meta.url));

export default defineConfig({
  root: "web",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    // 多入口：主界面 + 悬浮窗 + 右键菜单窗。漏掉哪个，对应窗口就是白屏 ——
    // 它们的 html 不会出现在 dist/ 里。
    rollupOptions: {
      input: {
        main: entry("./web/index.html"),
        float: entry("./web/float.html"),
        menu: entry("./web/menu.html"),
      },
    },
  },
  clearScreen: false,
});
