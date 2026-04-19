//! `GET /api/status` (T11) — 認証なしの運用診断エンドポイント。
//!
//! 返す JSON:
//! ```text
//! {
//!   "server": {
//!     "version": "0.1.0",
//!     "uptimeSeconds": 12345,
//!     "sqlite": { "path": "/Users/.../tasks.db", "ok": true }
//!   },
//!   "ngrok": { "enabled": false }
//!   // OR { "enabled": true, "reachable": false, "error": "..." }
//!   // OR { "enabled": true, "reachable": true, "publicUrl": "...", ... }
//! }
//! ```
//!
//! 認証を外した理由: public URL が機能しているか my-own デプロイ前に curl で
//! 確認したいユースケース。`api_key` が漏れないよう、ここで公開する情報は
//! 数値メトリクス + URL / ローカル bind 先に限定する。

use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AppState;

/// ngrok admin API の呼び出しタイムアウト。遅いとスレッドが固まるので短め。
const NGROK_ADMIN_TIMEOUT_SECS: u64 = 2;
const NGROK_ADMIN_URL: &str = "http://localhost:4040/api/tunnels";

// ------------------------------------------------------------------
// Response DTOs
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub server: ServerStatus,
    pub ngrok: NgrokStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// `env!("CARGO_PKG_VERSION")` 由来の static 文字列をコピーして持つ。
    /// `&'static str` にすると Deserialize が付けられないため String に統一。
    pub version: String,
    pub uptime_seconds: u64,
    pub sqlite: SqliteStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqliteStatus {
    pub path: String,
    pub ok: bool,
}

/// ngrok の 3 状態をフラットな JSON オブジェクトで表現する。
///
/// - **disabled**: `{ "enabled": false }`
/// - **unreachable**: `{ "enabled": true, "reachable": false, "error": "..." }`
/// - **up**: `{ "enabled": true, "reachable": true, "publicUrl": "...", ... }`
///
/// `skip_serializing_if = Option::is_none` により、状態に応じて不要な
/// フィールドは出力されない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NgrokStatus {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarding_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_requests_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_requests_per_minute: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connections_total: Option<u64>,
}

// ------------------------------------------------------------------
// Handler
// ------------------------------------------------------------------

/// `/api/status` ハンドラ。エラー系を握りつぶして必ず 200 を返すのが設計 —
/// "status" そのものが落ちるとトリアージに使えなくなるため、SQLite ヘルス
/// / ngrok 到達性を JSON のフィールドとして表現する。
pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let uptime_seconds = state.started_at.elapsed().as_secs();

    let sqlite_ok = check_sqlite(&state);
    let sqlite = SqliteStatus {
        path: state.sqlite_path.as_str().to_string(),
        ok: sqlite_ok,
    };

    let ngrok = match state.ngrok_domain.as_deref() {
        None => NgrokStatus {
            enabled: false,
            ..Default::default()
        },
        Some(_domain) => match fetch_ngrok_tunnels().await {
            Ok(v) => parse_tunnel_status(&v),
            Err(msg) => NgrokStatus {
                enabled: true,
                reachable: Some(false),
                error: Some(msg),
                ..Default::default()
            },
        },
    };

    Json(StatusResponse {
        server: ServerStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_seconds,
            sqlite,
        },
        ngrok,
    })
}

/// `SELECT 1` で SQLite が読めるか確認。mutex 毒化 / SQL 失敗は `false` に
/// 寄せて status レスポンスを 200 に保つ。
fn check_sqlite(state: &AppState) -> bool {
    let Ok(conn) = state.conn.lock() else {
        return false;
    };
    conn.query_row("SELECT 1", [], |_| Ok::<(), rusqlite::Error>(()))
        .is_ok()
}

/// ngrok admin API (`http://localhost:4040/api/tunnels`) を叩いて JSON を
/// 返す。接続失敗 / 非 2xx / パース失敗はすべて文字列エラーに畳む。
async fn fetch_ngrok_tunnels() -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(NGROK_ADMIN_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("reqwest client build failed: {e}"))?;
    let resp = client
        .get(NGROK_ADMIN_URL)
        .send()
        .await
        .map_err(|e| format!("ngrok admin unreachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("ngrok admin returned {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("ngrok admin body parse failed: {e}"))
}

/// ngrok `/api/tunnels` レスポンスから必要なフィールドを抽出する純関数。
///
/// 期待する shape (実データの抜粋):
/// ```json
/// { "tunnels": [{ "public_url": "...", "config": { "addr": "..." },
///                "metrics": { "conns": { "count": N },
///                             "http": { "count": M, "rate1": R } } }] }
/// ```
///
/// `tunnels` が空 / 存在しない / 想定外 shape の場合も panic せず
/// `reachable: true, publicUrl: None` で縮退する。
fn parse_tunnel_status(v: &Value) -> NgrokStatus {
    let tunnel = v
        .get("tunnels")
        .and_then(|ts| ts.as_array())
        .and_then(|ts| ts.first());

    let public_url = tunnel
        .and_then(|t| t.get("public_url"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let forwarding_to = tunnel
        .and_then(|t| t.get("config"))
        .and_then(|c| c.get("addr"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let metrics = tunnel.and_then(|t| t.get("metrics"));
    let http_metrics = metrics.and_then(|m| m.get("http"));
    let conns_metrics = metrics.and_then(|m| m.get("conns"));

    let http_requests_total = http_metrics
        .and_then(|h| h.get("count"))
        .and_then(|v| v.as_u64());
    // ngrok の `rate1` は「直近 1 分間の秒あたりレート」なので x60 で
    // 分あたりに変換する (ngrok 側の慣習を吸収)。
    let http_requests_per_minute = http_metrics
        .and_then(|h| h.get("rate1"))
        .and_then(|v| v.as_f64())
        .map(|r| r * 60.0);
    let connections_total = conns_metrics
        .and_then(|c| c.get("count"))
        .and_then(|v| v.as_u64());

    NgrokStatus {
        enabled: true,
        reachable: Some(true),
        error: None,
        public_url,
        forwarding_to,
        http_requests_total,
        http_requests_per_minute,
        connections_total,
    }
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_tunnel_status_extracts_full_shape() {
        // ユーザーから共有された実レスポンスを元に組み立てた典型例
        let v = json!({
            "tunnels": [{
                "name": "command_line",
                "public_url": "https://unedified-carrie-nondiathermanous.ngrok-free.dev",
                "proto": "https",
                "config": {
                    "addr": "http://localhost:3333",
                    "inspect": true
                },
                "metrics": {
                    "conns": { "count": 50, "gauge": 0 },
                    "http": { "count": 56, "rate1": 0.7138732962499688 }
                }
            }],
            "uri": "/api/tunnels"
        });
        let s = parse_tunnel_status(&v);
        assert!(s.enabled);
        assert_eq!(s.reachable, Some(true));
        assert_eq!(
            s.public_url.as_deref(),
            Some("https://unedified-carrie-nondiathermanous.ngrok-free.dev")
        );
        assert_eq!(s.forwarding_to.as_deref(), Some("http://localhost:3333"));
        assert_eq!(s.http_requests_total, Some(56));
        assert_eq!(s.connections_total, Some(50));
        // 0.7138732962499688 * 60 ≈ 42.83
        let per_min = s.http_requests_per_minute.unwrap();
        assert!(
            (per_min - 42.832_397_774_998_13).abs() < 1e-6,
            "http_requests_per_minute: expected ~42.83, got {per_min}"
        );
    }

    #[test]
    fn parse_tunnel_status_handles_empty_tunnels_array() {
        let v = json!({ "tunnels": [], "uri": "/api/tunnels" });
        let s = parse_tunnel_status(&v);
        assert!(s.enabled);
        assert_eq!(s.reachable, Some(true));
        // tunnels が 0 件でも panic せず、値系フィールドは None で縮退
        assert!(s.public_url.is_none());
        assert!(s.forwarding_to.is_none());
        assert!(s.http_requests_total.is_none());
    }

    #[test]
    fn parse_tunnel_status_handles_missing_metrics() {
        // config.addr だけあって metrics が無いケース — やはり縮退する
        let v = json!({
            "tunnels": [{
                "public_url": "https://x.ngrok-free.dev",
                "config": { "addr": "http://localhost:3333" }
            }]
        });
        let s = parse_tunnel_status(&v);
        assert_eq!(s.public_url.as_deref(), Some("https://x.ngrok-free.dev"));
        assert!(s.http_requests_total.is_none());
        assert!(s.http_requests_per_minute.is_none());
        assert!(s.connections_total.is_none());
    }

    #[test]
    fn parse_tunnel_status_handles_top_level_garbage() {
        // tunnels キーが無い / 型が違う shape でも panic しない
        assert!(parse_tunnel_status(&json!({})).enabled);
        assert!(parse_tunnel_status(&json!("not an object")).enabled);
        assert!(parse_tunnel_status(&json!({ "tunnels": "not-an-array" })).enabled);
    }

    #[test]
    fn ngrok_status_disabled_serializes_minimal_shape() {
        // enabled=false のときは reachable / error / publicUrl 系を
        // 出さない (skip_serializing_if が効いていることの pin)
        let s = NgrokStatus {
            enabled: false,
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({ "enabled": false }));
    }

    #[test]
    fn ngrok_status_unreachable_serializes_error_only() {
        let s = NgrokStatus {
            enabled: true,
            reachable: Some(false),
            error: Some("connection refused".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({
                "enabled": true,
                "reachable": false,
                "error": "connection refused"
            })
        );
    }
}
