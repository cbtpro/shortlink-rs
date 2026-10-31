import { $ } from "./dom.js";

let timer = 0;

export function toast(message) {
  const node = $("#toast");
  if (!node) return;

  node.textContent = message;
  node.classList.add("toast--visible");
  clearTimeout(timer);
  timer = setTimeout(() => node.classList.remove("toast--visible"), 1800);
}
