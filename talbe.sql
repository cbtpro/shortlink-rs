CREATE TABLE IF NOT EXISTS `short_links` (
  `id` BIGINT UNSIGNED NOT NULL AUTO_INCREMENT COMMENT '主键 ID，自增',
  `code` VARCHAR(10) NOT NULL UNIQUE COMMENT '短链接代码，唯一标识',
  `long_url` TEXT NOT NULL COMMENT '原始长链接地址',
  `created_at` TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '创建时间',
  `expire_at` TIMESTAMP NULL DEFAULT NULL COMMENT '过期时间（可选）',
  `max_visits` INT UNSIGNED DEFAULT NULL COMMENT '最大访问次数限制（可选）',
  `visit_count` INT UNSIGNED NOT NULL DEFAULT 0 COMMENT '当前访问次数',
  `password` VARCHAR(255) DEFAULT NULL COMMENT '访问密码（可选）',
  `ip_limit` JSON DEFAULT NULL COMMENT 'IP 限制规则，JSON 格式（可选）',
  `ua_limit` JSON DEFAULT NULL COMMENT 'User-Agent 限制规则，JSON 格式（可选）',
  PRIMARY KEY (`id`),
  INDEX `idx_code` (`code`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COMMENT='短链接主表';
