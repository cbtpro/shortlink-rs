-- 已有库升级：给短链加上「中转页」开关。
-- 新装环境直接执行 talbe.sql 即可，不必再跑本文件。
ALTER TABLE `short_links`
  ADD COLUMN `interstitial` TINYINT(1) NOT NULL DEFAULT 0
    COMMENT '跳转前是否先显示中转页';
