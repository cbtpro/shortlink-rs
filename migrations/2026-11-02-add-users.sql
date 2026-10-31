-- 已有库升级：加入账号体系，并把短链与会话挂到用户上。
-- 新装环境直接执行 talbe.sql 即可，不必再跑本文件。
-- 本文件不是幂等的（ALTER 重复执行会报列已存在），只需跑一次。

CREATE TABLE IF NOT EXISTS `users` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  `email` VARCHAR(254) NOT NULL COMMENT '登录邮箱（小写）',
  `display_name` VARCHAR(32) NOT NULL COMMENT '展示昵称',
  `password_hash` VARCHAR(255) NOT NULL COMMENT 'PBKDF2-HMAC-SHA256 散列串',
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '注册时间（UTC）',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uk_users_email` (`email`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='用户';

CREATE TABLE IF NOT EXISTS `user_sessions` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  `token_hash` CHAR(64) NOT NULL COMMENT '会话令牌的 SHA-256 十六进制',
  `user_id` BIGINT UNSIGNED NOT NULL,
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '登录时间（UTC）',
  `expires_at` DATETIME NOT NULL COMMENT '到期时间（UTC）',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uk_session_token` (`token_hash`),
  KEY `idx_session_user` (`user_id`),
  KEY `idx_session_expiry` (`expires_at`),
  CONSTRAINT `fk_session_user` FOREIGN KEY (`user_id`)
    REFERENCES `users` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='登录会话';

-- 存量短链一律是匿名的，user_id 保持 NULL；不回填，也没有可回填的归属信息。
ALTER TABLE `short_links`
  ADD COLUMN `user_id` BIGINT UNSIGNED DEFAULT NULL
    COMMENT '归属用户，NULL 为匿名创建' AFTER `code`,
  ADD INDEX `idx_links_user` (`user_id`, `created_at`),
  ADD CONSTRAINT `fk_link_owner` FOREIGN KEY (`user_id`)
    REFERENCES `users` (`id`) ON DELETE CASCADE;
