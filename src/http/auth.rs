//! Bearer 認証 middleware。
//!
//! `/api/*` 配下の全リクエストに `Authorization: Bearer <api_key>` を要求する。
//! ヘッダ欠損 / 形式不正 / トークン不一致はすべて `Error::Unauthorized` (→ 401)。
//! `/healthz` は意図的に認証を掛けない — 運用者が token 無しで死活確認
//! できるようにするため。

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use super::AppState;
use crate::error::Error;

pub async fn require_bearer(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, Error> {
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .ok_or(Error::Unauthorized)?;
    let value = header.to_str().map_err(|_| Error::Unauthorized)?;
    let token = value.strip_prefix("Bearer ").ok_or(Error::Unauthorized)?;
    if !constant_time_eq(token.as_bytes(), state.api_key.as_bytes()) {
        return Err(Error::Unauthorized);
    }
    Ok(next.run(req).await)
}

/// タイミング攻撃に耐性を持たせるため、長さが一致するときは必ず全バイト
/// 走査して差分を bit-OR で溜める。長さが違う場合は早期 return するが、
/// api_key の「長さ」は機密ではないので許容 (ユーザー指定の UUID 等は
/// 固定長)。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_equal_bytes() {
        assert!(constant_time_eq(b"secret", b"secret"));
    }

    #[test]
    fn constant_time_eq_rejects_different_bytes() {
        assert!(!constant_time_eq(b"secret", b"SECRET"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq(b"secret", b"secretx"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn constant_time_eq_accepts_empty_equal() {
        assert!(constant_time_eq(b"", b""));
    }
}
