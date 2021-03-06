//! The forward module of roa.
//! This module provides a context extension `Forward`,
//! which is used to parse `X-Forwarded-*` request headers.

use crate::core::header::HOST;
use crate::core::{async_trait, throw, Context, Result, State, StatusCode};
use crate::preload::*;
use std::net::IpAddr;

/// A context extension `Forward` used to parse `X-Forwarded-*` request headers.
#[async_trait]
pub trait Forward {
    /// Get true host.
    /// - If "x-forwarded-host" is set and valid, use it.
    /// - Else if "host" is set and valid, use it.
    /// - Else throw Err(400 BAD REQUEST).
    ///
    /// ### Example
    /// ```rust
    /// use roa::core::{Context, Result};
    /// use roa::forward::Forward;
    ///
    /// async fn get(ctx: Context<()>) -> Result {
    ///     println!("host: {}", ctx.host().await?);
    ///     Ok(())
    /// }
    /// ```
    async fn host(&self) -> Result<String>;

    /// Get true client ip.
    /// - If "x-forwarded-for" is set and valid, use the first ip.
    /// - Else use the ip of `Context::remote_addr()`.
    ///
    /// ### Example
    /// ```rust
    /// use roa::core::{Context, Result};
    /// use roa::forward::Forward;
    ///
    /// async fn get(ctx: Context<()>) -> Result {
    ///     println!("client ip: {}", ctx.client_ip().await);
    ///     Ok(())
    /// }
    /// ```
    async fn client_ip(&self) -> IpAddr;

    /// Get true forwarded ips.
    /// - If "x-forwarded-for" is set and valid, use it.
    /// - Else return an empty vector.
    ///
    /// ### Example
    /// ```rust
    /// use roa::core::{Context, Result};
    /// use roa::forward::Forward;
    ///
    /// async fn get(ctx: Context<()>) -> Result {
    ///     println!("forwarded ips: {:?}", ctx.forwarded_ips().await);
    ///     Ok(())
    /// }
    /// ```
    async fn forwarded_ips(&self) -> Vec<IpAddr>;

    /// Try to get forwarded proto.
    /// - If "x-forwarded-proto" is not set, return None.
    /// - If "x-forwarded-proto" is set but fails to string, return Some(Err(400 BAD REQUEST)).
    ///
    /// ### Example
    /// ```rust
    /// use roa::core::{Context, Result};
    /// use roa::forward::Forward;
    ///
    /// async fn get(ctx: Context<()>) -> Result {
    ///     if let Some(result) = ctx.forwarded_proto().await {
    ///         println!("forwarded proto: {}", result?);
    ///     }
    ///     Ok(())
    /// }
    /// ```
    async fn forwarded_proto(&self) -> Option<Result<String>>;
}

#[async_trait]
impl<S: State> Forward for Context<S> {
    async fn host(&self) -> Result<String> {
        if let Some(Ok(value)) = self.req().await.get("x-forwarded-host") {
            Ok(value.to_string())
        } else if let Some(Ok(value)) = self.req().await.get(HOST) {
            Ok(value.to_string())
        } else {
            throw!(
                StatusCode::BAD_REQUEST,
                "header `host` and `x-forwarded-host` are both not set"
            )
        }
    }

    async fn client_ip(&self) -> IpAddr {
        let addrs = self.forwarded_ips().await;
        if addrs.is_empty() {
            self.remote_addr().ip()
        } else {
            addrs[0]
        }
    }

    async fn forwarded_ips(&self) -> Vec<IpAddr> {
        let mut addrs = Vec::new();
        if let Some(Ok(value)) = self.req().await.get("x-forwarded-for") {
            for addr_str in value.split(',') {
                if let Ok(addr) = addr_str.trim().parse() {
                    addrs.push(addr)
                }
            }
        }
        addrs
    }

    async fn forwarded_proto(&self) -> Option<Result<String>> {
        self.req()
            .await
            .get("x-forwarded-proto")
            .map(|result| result.map(|value| value.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::Forward;
    use crate::core::App;
    use async_std::task::spawn;
    use http::header::HOST;
    use http::{HeaderValue, StatusCode};

    #[tokio::test]
    async fn host() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(())
            .gate_fn(move |ctx, _next| async move {
                assert_eq!("github.com", ctx.host().await?);
                Ok(())
            })
            .run_local()?;
        spawn(server);
        let client = reqwest::Client::new();
        let resp = client
            .get(&format!("http://{}", addr))
            .header(HOST, HeaderValue::from_static("github.com"))
            .send()
            .await?;
        assert_eq!(StatusCode::OK, resp.status());

        let resp = client
            .get(&format!("http://{}", addr))
            .header(HOST, "google.com")
            .header("x-forwarded-host", "github.com")
            .send()
            .await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }

    #[tokio::test]
    async fn host_err() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(())
            .end(move |mut ctx| async move {
                ctx.req_mut().await.headers.remove(HOST);
                assert_eq!("", ctx.host().await?);
                Ok(())
            })
            .run_local()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::BAD_REQUEST, resp.status());
        assert_eq!(
            "header `host` and `x-forwarded-host` are both not set",
            resp.text().await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn client_ip() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(())
            .gate_fn(move |ctx, _next| async move {
                assert_eq!(ctx.remote_addr().ip(), ctx.client_ip().await);
                Ok(())
            })
            .run_local()?;
        spawn(server);
        reqwest::get(&format!("http://{}", addr)).await?;

        let (addr, server) = App::new(())
            .gate_fn(move |ctx, _next| async move {
                assert_eq!("192.168.0.1", ctx.client_ip().await.to_string());
                Ok(())
            })
            .run_local()?;
        spawn(server);
        let client = reqwest::Client::new();
        client
            .get(&format!("http://{}", addr))
            .header("x-forwarded-for", "192.168.0.1, 8.8.8.8")
            .send()
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn forwarded_proto() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(())
            .gate_fn(move |ctx, _next| async move {
                assert_eq!("https", ctx.forwarded_proto().await.unwrap()?);
                Ok(())
            })
            .run_local()?;
        spawn(server);
        let client = reqwest::Client::new();
        client
            .get(&format!("http://{}", addr))
            .header("x-forwarded-proto", "https")
            .send()
            .await?;

        Ok(())
    }
}
