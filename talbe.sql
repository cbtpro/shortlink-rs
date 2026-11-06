CREATE TABLE IF NOT EXISTS `users` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  -- 邮箱统一小写存储，唯一索引才对大小写不敏感
  `email` VARCHAR(254) NOT NULL COMMENT '登录邮箱（小写，RFC 5321 上限 254）',
  `display_name` VARCHAR(32) NOT NULL COMMENT '展示昵称，默认取邮箱 @ 前部分',
  -- 口令散列自带算法与迭代次数：pbkdf2-sha256$<iterations>$<salt>$<hash>
  -- 因此调高迭代次数不会让老账号登录失败
  `password_hash` VARCHAR(255) NOT NULL COMMENT 'PBKDF2-HMAC-SHA256 散列串',
  -- 角色与状态存在库里而不是配置里：改一次要立刻生效，不该重启服务。
  -- 用 VARCHAR 而非 ENUM，是为了让「未知值怎么处理」只由领域层决定：
  -- 角色认不出按普通用户（少给权限），状态认不出按禁用（宁可拒绝）。
  `role` VARCHAR(16) NOT NULL DEFAULT 'user' COMMENT '角色：user / admin，未知值按 user 处理',
  `status` VARCHAR(16) NOT NULL DEFAULT 'active' COMMENT '账号状态：active / disabled，未知值按 disabled 处理',
  `must_change_password` TINYINT(1) NOT NULL DEFAULT 0 COMMENT '为 1 时除改密外一律 403',
  -- 只写不读的审计字段：管理员重置过谁、对方有没有真的改掉
  `password_changed_at` DATETIME NULL DEFAULT NULL COMMENT '最近一次改密时间（UTC）',
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '注册时间（UTC）',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uk_users_email` (`email`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='用户';

-- 会话表存的是令牌的 SHA-256 指纹，不是令牌本身：
-- 明文只在签发那一刻出现在响应里，库被拖走也换不出能用的会话。
CREATE TABLE IF NOT EXISTS `user_sessions` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  `token_hash` CHAR(64) NOT NULL COMMENT '会话令牌的 SHA-256 十六进制',
  `user_id` BIGINT UNSIGNED NOT NULL,
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '登录时间（UTC）',
  `expires_at` DATETIME NOT NULL COMMENT '到期时间（UTC），绝对到期不续期',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uk_session_token` (`token_hash`),
  KEY `idx_session_user` (`user_id`),
  KEY `idx_session_expiry` (`expires_at`),
  CONSTRAINT `fk_session_user` FOREIGN KEY (`user_id`)
    REFERENCES `users` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='登录会话';

CREATE TABLE IF NOT EXISTS `short_links` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT COMMENT '主键 ID，自增',
  `code` VARCHAR(10) NOT NULL UNIQUE COMMENT '短链接代码，唯一标识',
  -- 归属用户。NULL 表示匿名创建：能创建、能跳转，但只能靠访客 cookie 找回。
  `user_id` BIGINT UNSIGNED DEFAULT NULL COMMENT '归属用户，NULL 为匿名创建',
  `long_url` TEXT NOT NULL COMMENT '原始长链接地址',
  -- 时间列统一用 DATETIME 而非 TIMESTAMP：
  -- 1) sqlx 把 TIMESTAMP 映射成 DateTime<Utc>，与领域层的 NaiveDateTime 不兼容；
  -- 2) DATETIME 不做会话时区转换，存的就是应用写入的字面量，读写对称；
  -- 3) TIMESTAMP 有 2038 上限，expire_at 迟早会踩到。
  -- 内容一律为 UTC（sqlx 每条连接都会下发 time_zone='+00:00'），
  -- 本地时间换算只发生在 API 边界。
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '创建时间（UTC）',
  `expire_at` DATETIME NULL DEFAULT NULL COMMENT '过期时间（可选，UTC）',
  `max_visits` INT UNSIGNED DEFAULT NULL COMMENT '最大访问次数限制（可选）',
  `visit_count` INT UNSIGNED NOT NULL DEFAULT 0 COMMENT '当前访问次数',
  `password` VARCHAR(255) DEFAULT NULL COMMENT '访问密码（可选）',
  `ip_limit` JSON DEFAULT NULL COMMENT 'IP 限制规则，JSON 格式（可选）',
  `ua_limit` JSON DEFAULT NULL COMMENT 'User-Agent 限制规则，JSON 格式（可选）',
  `interstitial` TINYINT(1) NOT NULL DEFAULT 0 COMMENT '跳转前是否先显示中转页',
  PRIMARY KEY (`id`),
  INDEX `idx_code` (`code`),
  -- 「我的链接」按归属倒序查，索引顺序要和 ORDER BY 一致
  INDEX `idx_links_user` (`user_id`, `created_at`),
  CONSTRAINT `fk_link_owner` FOREIGN KEY (`user_id`)
    REFERENCES `users` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='短链接主表';

-- 匿名访客的「最近创建」。登录用户的链接不走这张表 ——
-- 归属是 `short_links.user_id` 的属性，再存一份清单就成了两个事实来源。
-- 外键级联：短链被删掉后，历史里那条也跟着消失，不会留下点进去 404 的死链。
CREATE TABLE IF NOT EXISTS `link_creations` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  `visitor_id` CHAR(32) NOT NULL COMMENT '匿名访客标识（cookie，非账号）',
  `code` VARCHAR(10) NOT NULL COMMENT '创建出的短码',
  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '创建时间（UTC）',
  PRIMARY KEY (`id`),
  UNIQUE KEY `uk_visitor_code` (`visitor_id`, `code`),
  KEY `idx_visitor_created` (`visitor_id`, `created_at`),
  CONSTRAINT `fk_creation_link` FOREIGN KEY (`code`)
    REFERENCES `short_links` (`code`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='匿名访客的创建记录';
