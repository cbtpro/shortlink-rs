-- 已有库升级：账号带上角色、状态，以及「必须改密」标记。
-- 新装环境直接执行 talbe.sql 即可，不必再跑本文件。
-- 本文件不是幂等的（ALTER 重复执行会报列已存在），只需跑一次。
--
-- 默认值是按「升级不能让现有站点瘫掉」定的：
--   role=user              —— 老账号一律普通用户，管理员由启动引导创建；
--   status=active          —— 不能让已有账号升级后集体登不上；
--   must_change_password=0 —— 只有初始化生成、或被管理员重置的才是 1。
--
-- 两列都用 VARCHAR 而不是 ENUM：领域层的 Role::parse / AccountStatus::parse
-- 对未知值有明确的降级规则（角色降成 user，状态降成 disabled），
-- 而 ENUM 会在写入时就拒绝掉那些值，等于把判定分散到两处。

ALTER TABLE `users`
  ADD COLUMN `role` VARCHAR(16) NOT NULL DEFAULT 'user'
    COMMENT '角色：user / admin，未知值按 user 处理' AFTER `password_hash`,
  ADD COLUMN `status` VARCHAR(16) NOT NULL DEFAULT 'active'
    COMMENT '账号状态：active / disabled，未知值按 disabled 处理' AFTER `role`,
  ADD COLUMN `must_change_password` TINYINT(1) NOT NULL DEFAULT 0
    COMMENT '为 1 时除改密外一律 403' AFTER `status`,
  -- 只写不读：出事时能回答「这个口令是什么时候被换掉的」，
  -- 尤其是管理员重置过谁、对方到底改没改。业务分支不依赖它。
  ADD COLUMN `password_changed_at` DATETIME NULL DEFAULT NULL
    COMMENT '最近一次改密时间（UTC），仅作审计' AFTER `must_change_password`;
