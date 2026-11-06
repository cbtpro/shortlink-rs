import { ApiError } from "./http.js";

/**
 * 服务端文案 → 页面文案。
 *
 * 错误信息只有 Rust 一处来源，页面只负责翻译。表集中在这里的理由很实际：
 * 同一条错误在首页、登录页、后台必须说成同一句话，各写一份迟早会漂成三种说法。
 */

const MESSAGES = {
  // 账号
  "email is not a valid address": "邮箱格式不对",
  "password must be at least 8 characters": "密码至少 8 位",
  "password must be at most 128 characters": "密码最多 128 位",
  "password must not be blank": "密码不能为空白",
  "display name must be at most 32 characters": "昵称最多 32 个字",
  "display name must not contain control characters": "昵称不能包含换行或制表符",
  "new password must differ from the current one": "新密码不能和原密码一样",
  "email or password is incorrect": "邮箱或密码不正确",
  "email is already registered": "这个邮箱已经注册过了，直接登录吧",

  // 会话与权限
  unauthorized: "登录已过期，请重新登录",
  "account is disabled": "账号已被停用，请联系管理员",
  "password must be changed before continuing": "请先修改密码",
  "administrator privileges required": "只有管理员能操作这个",

  // 短链
  "url must not be empty": "请填写目标地址",
  "url must be at most 2048 characters": "地址最长 2048 个字符",
  "url must start with http:// or https://": "只支持 http:// 或 https:// 开头的地址",
  "url must not contain whitespace": "地址中不能包含空格",
  "custom code must be 1-10 chars of [A-Za-z0-9-_]": "短码只能是 1–10 位的字母、数字、- 或 _",
  "password must not be empty": "密码不能为空白",
  "password must be at most 64 characters": "密码最长 64 位",
  "password must not contain whitespace": "密码不能包含空格",
  "password must not contain quotes, backslashes or semicolons":
    "密码不能包含引号、反斜杠或分号",
  "password must not contain SQL keywords": "密码不能包含 SQL 关键字",
  "max_visits must be between 1 and 100000000": "访问次数上限要在 1 到 1 亿之间",
  "expire_at must be in the future": "有效期必须晚于当前时间",
  "short code already exists": "这个短码已被占用，换一个试试",
  "failed to allocate a unique short code, please retry": "短码分配失败，请重试",
  "short link not found": "该短链已不存在，列表可能已过期",

  // 后台
  "user not found": "账号不存在，可能已被删除",
  "cannot change the status of your own account": "不能停用自己的账号",
  "cannot manage your own account from the console": "不能在后台管理自己的账号",
  "administrator accounts can only be managed by themselves": "管理员账号只能自己管理",
  "request body is not valid": "提交的数据格式不对，刷新页面重试",
};

/** 没有对应规则的按状态码兜底 */
const BY_STATUS = {
  409: "这个短码已被占用，换一个试试",
};

/** 把任意异常变成能直接显示给人看的一句话。 */
export function humanize(error) {
  if (!(error instanceof ApiError)) return error?.message ?? "出错了，请稍后重试";

  const translated = MESSAGES[error.message];
  if (translated) return translated;

  if (error.status === 0) return "连不上服务，请确认服务已启动";
  const fallback = BY_STATUS[error.status];
  if (fallback) return fallback;
  if (error.status >= 500) return "服务端出错了，请稍后重试";

  return error.message;
}
