//! 应用错误与 HTTP 状态码的映射。

use std::fmt;

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use serde::Serialize;

use crate::cache::CacheError;
use crate::domain::account::AccountError;
use crate::domain::new_link::ValidationError;
use crate::domain::short_link::AccessDenial;
use crate::repository::RepoError;

#[derive(Debug)]
pub enum AppError {
    Validation(ValidationError),
    LinkNotFound,
    CodeExists,
    AccessDenied(AccessDenial),
    Storage(RepoError),
    Cache(CacheError),
    CodeGenerationExhausted,
    /// 未登录。401 而不是 403：前端据此把用户送去登录页。
    Unauthorized,
    /// 需要管理员角色
    AdminRequired,
    /// 账号被禁用
    AccountDisabled,
    /// 临时口令未更换，除了改密什么都做不了
    PasswordChangeRequired,
    /// 目标用户不存在
    UserNotFound,
    /// 拒绝这次操作，`&'static str` 说明原因（文案由前端翻译）
    Forbidden(&'static str),
    /// 注册时邮箱已被占用
    EmailTaken,
    /// 登录失败。不区分「没这个邮箱」和「密码不对」，否则等于给人做账号探测。
    InvalidCredentials,
    Account(AccountError),
    /// 请求体解析失败（缺字段、类型不对、不是合法 JSON）。
    ///
    /// 不带细节：解析器的原话里有 Rust 类型名与列号，对调用方没有帮助，
    /// 对探测内部结构的人倒是正好。原始错误进 debug 日志。
    InvalidBody,
    /// 后台线程异常结束（例如阻塞任务 panic）。属于服务端故障，细节只进日志。
    Internal(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Validation(err) => write!(f, "{err}"),
            AppError::LinkNotFound => write!(f, "short link not found"),
            AppError::CodeExists => write!(f, "short code already exists"),
            AppError::AccessDenied(reason) => match reason {
                AccessDenial::Expired => write!(f, "short link has expired"),
                AccessDenial::VisitLimitReached => write!(f, "visit limit exceeded"),
                AccessDenial::MissingPassword => write!(f, "password is required"),
                AccessDenial::InvalidPassword => write!(f, "invalid password"),
            },
            AppError::Storage(err) => write!(f, "{err}"),
            AppError::Cache(err) => write!(f, "{err}"),
            AppError::CodeGenerationExhausted => {
                write!(f, "failed to allocate a unique short code, please retry")
            }
            AppError::Unauthorized => write!(f, "unauthorized"),
            AppError::AdminRequired => write!(f, "administrator privileges required"),
            AppError::AccountDisabled => write!(f, "account is disabled"),
            AppError::PasswordChangeRequired => {
                write!(f, "password must be changed before continuing")
            }
            AppError::UserNotFound => write!(f, "user not found"),
            AppError::Forbidden(reason) => write!(f, "{reason}"),
            AppError::EmailTaken => write!(f, "email is already registered"),
            AppError::InvalidCredentials => write!(f, "email or password is incorrect"),
            AppError::Account(err) => write!(f, "{err}"),
            AppError::InvalidBody => write!(f, "request body is not valid"),
            AppError::Internal(detail) => write!(f, "internal task failure: {detail}"),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Validation(err) => Some(err),
            AppError::Account(err) => Some(err),
            AppError::Storage(err) => Some(err),
            AppError::Cache(err) => Some(err),
            _ => None,
        }
    }
}

/// 错误响应体，`code` 与 HTTP 状态码一致。
#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub code: u16,
    pub message: String,
}

impl AppError {
    /// 可以安全暴露给调用方的文案，内部细节不外泄。
    fn public_message(&self) -> String {
        match self {
            AppError::Storage(_) | AppError::Cache(_) | AppError::Internal(_) => {
                "internal server error".to_string()
            }
            other => other.to_string(),
        }
    }

    fn is_server_fault(&self) -> bool {
        matches!(
            self,
            AppError::Storage(_) | AppError::Cache(_) | AppError::Internal(_)
        )
    }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::LinkNotFound => StatusCode::NOT_FOUND,
            AppError::CodeExists => StatusCode::CONFLICT,
            AppError::AccessDenied(
                AccessDenial::MissingPassword | AccessDenial::InvalidPassword,
            ) => StatusCode::FORBIDDEN,
            AppError::AccessDenied(AccessDenial::Expired | AccessDenial::VisitLimitReached) => {
                StatusCode::GONE
            }
            AppError::Storage(_)
            | AppError::Cache(_)
            | AppError::CodeGenerationExhausted
            | AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Unauthorized | AppError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            AppError::AdminRequired | AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::AccountDisabled | AppError::PasswordChangeRequired => StatusCode::FORBIDDEN,
            AppError::UserNotFound => StatusCode::NOT_FOUND,
            AppError::EmailTaken => StatusCode::CONFLICT,
            AppError::Account(_) | AppError::InvalidBody => StatusCode::BAD_REQUEST,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if self.is_server_fault() {
            // 详细堆栈只进日志，响应体保持干净
            log::error!("request failed: {self}");
        }

        let status = self.status_code();
        HttpResponse::build(status).json(ApiErrorBody {
            code: status.as_u16(),
            message: self.public_message(),
        })
    }
}

impl From<ValidationError> for AppError {
    fn from(err: ValidationError) -> Self {
        AppError::Validation(err)
    }
}

impl From<AccountError> for AppError {
    fn from(err: AccountError) -> Self {
        AppError::Account(err)
    }
}

impl From<AccessDenial> for AppError {
    fn from(reason: AccessDenial) -> Self {
        AppError::AccessDenied(reason)
    }
}

impl From<RepoError> for AppError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::DuplicateCode => AppError::CodeExists,
            RepoError::DuplicateEmail => AppError::EmailTaken,
            RepoError::NotFound => AppError::LinkNotFound,
            other => AppError::Storage(other),
        }
    }
}

impl From<CacheError> for AppError {
    fn from(err: CacheError) -> Self {
        AppError::Cache(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_error() -> AppError {
        AppError::Storage(RepoError::Backend("connection reset by peer".into()))
    }

    fn cache_error() -> AppError {
        AppError::Cache(CacheError::Serialize(
            serde_json::from_str::<i32>("not-json").expect_err("literal is not valid json"),
        ))
    }

    #[test]
    fn maps_business_errors_to_expected_status() {
        let cases = [
            (
                AppError::Validation(ValidationError::EmptyUrl),
                StatusCode::BAD_REQUEST,
            ),
            (AppError::LinkNotFound, StatusCode::NOT_FOUND),
            (AppError::CodeExists, StatusCode::CONFLICT),
            (
                AppError::AccessDenied(AccessDenial::MissingPassword),
                StatusCode::FORBIDDEN,
            ),
            (
                AppError::AccessDenied(AccessDenial::InvalidPassword),
                StatusCode::FORBIDDEN,
            ),
            (
                AppError::AccessDenied(AccessDenial::Expired),
                StatusCode::GONE,
            ),
            (
                AppError::AccessDenied(AccessDenial::VisitLimitReached),
                StatusCode::GONE,
            ),
            (storage_error(), StatusCode::INTERNAL_SERVER_ERROR),
            (cache_error(), StatusCode::INTERNAL_SERVER_ERROR),
            (
                AppError::CodeGenerationExhausted,
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (AppError::AdminRequired, StatusCode::FORBIDDEN),
            (AppError::AccountDisabled, StatusCode::FORBIDDEN),
            (AppError::PasswordChangeRequired, StatusCode::FORBIDDEN),
            (
                AppError::Forbidden("cannot disable your own account"),
                StatusCode::FORBIDDEN,
            ),
            (AppError::UserNotFound, StatusCode::NOT_FOUND),
            (AppError::Unauthorized, StatusCode::UNAUTHORIZED),
            (AppError::EmailTaken, StatusCode::CONFLICT),
            (AppError::InvalidCredentials, StatusCode::UNAUTHORIZED),
            (
                AppError::Account(AccountError::PasswordTooShort),
                StatusCode::BAD_REQUEST,
            ),
            (AppError::InvalidBody, StatusCode::BAD_REQUEST),
            (
                AppError::Internal("worker panicked".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(
                error.status_code(),
                expected,
                "unexpected status for {error}"
            );
        }
    }

    #[test]
    fn hides_internal_details_from_clients() {
        assert_eq!(storage_error().public_message(), "internal server error");
        assert_eq!(cache_error().public_message(), "internal server error");
        assert_eq!(
            AppError::LinkNotFound.public_message(),
            "short link not found"
        );
        assert_eq!(
            AppError::AccessDenied(AccessDenial::Expired).public_message(),
            "short link has expired"
        );
    }

    #[test]
    fn permission_errors_carry_actionable_wording() {
        // 前端按这些字符串分流：401 回登录页、这个回改密面板、那个显示「仅管理员」。
        // 改文案等于改协议，所以在这里钉住。
        assert_eq!(
            AppError::PasswordChangeRequired.public_message(),
            "password must be changed before continuing"
        );
        assert_eq!(
            AppError::AccountDisabled.public_message(),
            "account is disabled"
        );
        assert_eq!(
            AppError::AdminRequired.public_message(),
            "administrator privileges required"
        );
        // Forbidden 的说明原样透出：它本来就是写给人看的，不需要翻译
        assert_eq!(
            AppError::Forbidden("cannot change the status of your own account").public_message(),
            "cannot change the status of your own account"
        );
    }

    #[test]
    fn marks_only_backend_failures_as_server_fault() {
        assert!(storage_error().is_server_fault());
        assert!(cache_error().is_server_fault());
        assert!(!AppError::LinkNotFound.is_server_fault());
        assert!(!AppError::CodeGenerationExhausted.is_server_fault());
    }

    #[test]
    fn maps_repository_errors_to_domain_errors() {
        assert!(matches!(
            AppError::from(RepoError::DuplicateCode),
            AppError::CodeExists
        ));
        assert!(matches!(
            AppError::from(RepoError::NotFound),
            AppError::LinkNotFound
        ));
        assert!(matches!(
            AppError::from(RepoError::DuplicateEmail),
            AppError::EmailTaken
        ));
        assert!(matches!(
            AppError::from(RepoError::Backend("boom".into())),
            AppError::Storage(_)
        ));
    }

    #[tokio::test]
    async fn response_body_code_matches_http_status() {
        let response = AppError::CodeExists.error_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("body should be readable");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("body should be json");

        assert_eq!(parsed["code"], 409);
        assert_eq!(parsed["message"], "short code already exists");
    }
}
