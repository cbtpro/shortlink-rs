//! 创建短链接的用例。

use std::sync::Arc;

use crate::domain::code::ShortCodeGenerator;
use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::ShortLink;
use crate::error::AppError;
use crate::repository::{LinkRepository, RepoError};

/// 自动生成短码时允许的碰撞重试次数。
const MAX_CODE_ATTEMPTS: usize = 5;

pub struct ShortenService {
    repository: Arc<dyn LinkRepository>,
    code_generator: Arc<dyn ShortCodeGenerator>,
    code_length: usize,
    max_code_length: usize,
    public_base_url: String,
}

impl ShortenService {
    pub fn new(
        repository: Arc<dyn LinkRepository>,
        code_generator: Arc<dyn ShortCodeGenerator>,
        code_length: usize,
        max_code_length: usize,
        public_base_url: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            code_generator,
            code_length,
            max_code_length,
            public_base_url: public_base_url.into(),
        }
    }

    /// 生成对外展示的短链接地址。
    pub fn short_url(&self, code: &str) -> String {
        format!("{}/{}", self.public_base_url, code)
    }

    pub async fn create(&self, request: NewShortLink) -> Result<ShortLink, AppError> {
        request.validate(self.max_code_length)?;

        match request.code.clone() {
            Some(code) => self.save(&request, &code).await,
            None => self.save_with_generated_code(&request).await,
        }
    }

    async fn save(&self, request: &NewShortLink, code: &str) -> Result<ShortLink, AppError> {
        // 冲突以唯一约束为准，不做先查后插
        Ok(self.repository.insert(request, code).await?)
    }

    /// 随机短码可能与既有记录碰撞，冲突时换一个再试，而不是直接失败。
    async fn save_with_generated_code(
        &self,
        request: &NewShortLink,
    ) -> Result<ShortLink, AppError> {
        for _ in 0..MAX_CODE_ATTEMPTS {
            let code = self.code_generator.generate(self.code_length);
            match self.repository.insert(request, &code).await {
                Ok(link) => return Ok(link),
                Err(RepoError::DuplicateCode) => continue,
                Err(err) => return Err(err.into()),
            }
        }

        Err(AppError::CodeGenerationExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::code::RandomCodeGenerator;
    use crate::domain::new_link::ValidationError;
    use crate::repository::memory::InMemoryLinkRepository;
    use crate::test_support::{BrokenRepository, ScriptedCodeGenerator};

    fn service() -> ShortenService {
        ShortenService::new(
            Arc::new(InMemoryLinkRepository::new()),
            Arc::new(RandomCodeGenerator),
            7,
            10,
            "https://s.example.com",
        )
    }

    fn request(url: &str, code: Option<&str>) -> NewShortLink {
        NewShortLink {
            url: url.to_string(),
            code: code.map(str::to_string),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn generates_code_when_absent() {
        let link = service()
            .create(request("https://example.com", None))
            .await
            .expect("create should succeed");
        assert_eq!(link.code.len(), 7);
        assert_eq!(link.visit_count, 0);
    }

    #[tokio::test]
    async fn keeps_custom_code() {
        let link = service()
            .create(request("https://example.com", Some("promo")))
            .await
            .expect("create should succeed");
        assert_eq!(link.code, "promo");
    }

    #[tokio::test]
    async fn rejects_duplicate_custom_code() {
        let service = service();
        service
            .create(request("https://example.com", Some("dup")))
            .await
            .expect("first insert should succeed");

        let result = service.create(request("https://example.org", Some("dup"))).await;
        assert!(matches!(result, Err(AppError::CodeExists)));
    }

    #[tokio::test]
    async fn rejects_invalid_url() {
        let result = service().create(request("javascript:alert(1)", None)).await;
        assert!(matches!(
            result,
            Err(AppError::Validation(ValidationError::UnsupportedScheme))
        ));
    }

    #[test]
    fn builds_public_url() {
        assert_eq!(
            service().short_url("abc"),
            "https://s.example.com/abc"
        );
    }

    fn service_with_generator(
        repository: Arc<InMemoryLinkRepository>,
        generator: Arc<ScriptedCodeGenerator>,
    ) -> ShortenService {
        ShortenService::new(repository, generator, 7, 10, "https://s.example.com")
    }

    #[tokio::test]
    async fn retries_with_another_code_on_collision() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&request("https://example.com", None), "taken")
            .await
            .expect("seed insert should succeed");

        let generator = Arc::new(ScriptedCodeGenerator::new(&["taken", "free"]));
        let service = service_with_generator(repository, generator.clone());

        let link = service
            .create(request("https://example.org", None))
            .await
            .expect("second attempt should succeed");

        assert_eq!(link.code, "free");
        assert_eq!(generator.taken(), 2);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&request("https://example.com", None), "taken")
            .await
            .expect("seed insert should succeed");

        // 脚本只有一个值，用尽后重复它 —— 每次生成都撞码
        let generator = Arc::new(ScriptedCodeGenerator::new(&["taken"]));
        let service = service_with_generator(repository, generator.clone());

        let result = service.create(request("https://example.org", None)).await;

        assert!(matches!(result, Err(AppError::CodeGenerationExhausted)));
        assert_eq!(generator.taken(), MAX_CODE_ATTEMPTS);
    }

    #[tokio::test]
    async fn custom_code_can_be_shorter_than_generated_length() {
        // 自动生成的码长是建议值，自定义短码只受上限约束
        let link = service()
            .create(request("https://example.com", Some("a")))
            .await
            .expect("create should succeed");
        assert_eq!(link.code, "a");
    }

    #[tokio::test]
    async fn rejects_code_longer_than_configured_limit() {
        let service = ShortenService::new(
            Arc::new(InMemoryLinkRepository::new()),
            Arc::new(RandomCodeGenerator),
            7,
            4,
            "https://s.example.com",
        );

        let result = service
            .create(request("https://example.com", Some("abcde")))
            .await;

        assert!(matches!(
            result,
            Err(AppError::Validation(ValidationError::InvalidCode))
        ));
    }

    #[tokio::test]
    async fn propagates_backend_failure_instead_of_retrying() {
        // 只有撞码才重试，存储故障直接冒泡
        let generator = Arc::new(ScriptedCodeGenerator::new(&["x"]));
        let service = ShortenService::new(
            Arc::new(BrokenRepository),
            generator.clone(),
            7,
            10,
            "https://s.example.com",
        );

        let result = service.create(request("https://example.com", None)).await;

        assert!(matches!(result, Err(AppError::Storage(_))));
        assert_eq!(generator.taken(), 1, "非碰撞错误不应重试");
    }
}
