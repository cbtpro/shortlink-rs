CREATE TABLE IF NOT EXISTS `short_links` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT COMMENT '主键 ID，自增',
  `code` VARCHAR(10) NOT NULL UNIQUE COMMENT '短链接代码，唯一标识',
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
  INDEX `idx_code` (`code`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='短链接主表';

-- 访客侧的「最近创建」。没有账号体系，浏览器拿一个匿名 cookie 认人，
-- 因此这张表回答的是「这个浏览器建过什么」，不是「这个用户建过什么」。
-- 外键级联：短链被后台删掉后，历史里那条也跟着消失，不会留下点进去 404 的死链。
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
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='访客创建记录';
