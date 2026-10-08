import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const menu = document.getElementById("menu");

menu.addEventListener("click", event => {
  const item = event.target.closest(".menu-item");
  if (!item) return;
  event.preventDefault();
  invoke("float_menu_action", { action: item.dataset.action }).catch(error => console.error(error));
});

// 菜单里不该冒出浏览器的默认右键菜单
window.addEventListener("contextmenu", event => event.preventDefault());

window.addEventListener("keydown", event => {
  if (event.key === "Escape") {
    invoke("close_float_menu").catch(() => {});
  }
});

/* 每次弹出重播进场动效：先摘类、强制回流，再加回去 */
listen("menu-shown", () => {
  menu.classList.remove("enter");
  void menu.offsetWidth;
  menu.classList.add("enter");
}).catch(error => console.error(error));

/* 失焦即收起。Rust 侧还有一层 WindowEvent::Focused(false)，这里管的是
   webview 内自己的失焦，两层都在才跟系统原生菜单的手感一致。 */
window.addEventListener("blur", () => {
  invoke("close_float_menu").catch(() => {});
});
