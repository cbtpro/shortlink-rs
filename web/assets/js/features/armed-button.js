import { h } from "../core/dom.js";

/**
 * 点两次才生效的按钮。
 *
 * 后台的删除、重置、停用都不可逆，确认这一步不能省；但每按一次弹一个 modal，
 * 连着处理几行就会烦到让人下意识回车。让按钮自己承担确认：第一次变成
 * 「确认…」，`timeout` 内不再点就复原，真的点了才发请求。
 *
 * 请求期间按钮禁用，避免连点发出两次一模一样的写请求。
 */
export function armedButton({
  label,
  armedLabel,
  className = "row-action",
  title,
  onClick,
  timeout = 3000,
}) {
  const button = h("button", { type: "button", class: className, title, text: label });
  let timer = 0;

  const disarm = () => {
    clearTimeout(timer);
    button.textContent = label;
    button.classList.remove("row-action--armed");
  };

  button.addEventListener("click", async () => {
    if (!button.classList.contains("row-action--armed")) {
      button.classList.add("row-action--armed");
      button.textContent = armedLabel;
      timer = setTimeout(disarm, timeout);
      return;
    }

    disarm();
    button.disabled = true;
    try {
      await onClick();
    } finally {
      button.disabled = false;
    }
  });

  return button;
}
