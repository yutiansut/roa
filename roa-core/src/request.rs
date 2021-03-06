use crate::Body;
use futures::stream::TryStreamExt;
use http::{HeaderValue, Method, Uri, Version};
use hyper::HeaderMap;
use std::io;
use std::ops::{Deref, DerefMut};

/// Http request type of roa.
pub struct Request {
    /// The request's method
    pub method: Method,

    /// The request's URI
    pub uri: Uri,

    /// The request's version
    pub version: Version,

    /// The request's headers
    pub headers: HeaderMap<HeaderValue>,

    body: Body,
}

impl Request {
    pub(crate) fn new() -> Self {
        Self {
            method: Method::default(),
            uri: Uri::default(),
            version: Version::default(),
            headers: HeaderMap::default(),
            body: Body::new(),
        }
    }
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for Request {
    type Target = Body;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

impl DerefMut for Request {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.body
    }
}

impl From<http::Request<Body>> for Request {
    fn from(req: http::Request<Body>) -> Self {
        let (parts, body) = req.into_parts();
        Self {
            method: parts.method,
            uri: parts.uri,
            version: parts.version,
            headers: parts.headers,
            body,
        }
    }
}

impl From<http::Request<hyper::Body>> for Request {
    fn from(req: http::Request<hyper::Body>) -> Self {
        let (parts, body) = req.into_parts();
        let mut new_req: Self = http::Request::from_parts(parts, Body::new()).into();
        new_req.write(
            body.map_err(|err| io::Error::new(io::ErrorKind::Other, err))
                .into_async_read(),
        );
        new_req
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use async_std::task::spawn;
    use futures::AsyncReadExt;
    use http::StatusCode;

    #[tokio::test]
    async fn body_read() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = App::new(());
        app.gate_fn(|mut ctx, _next| async move {
            let mut data = String::new();
            ctx.req_mut().await.read_to_string(&mut data).await?;
            assert_eq!("Hello, World!", data);
            Ok(())
        });
        let (addr, server) = app.run_local()?;
        spawn(server);
        let client = reqwest::Client::new();
        let resp = client
            .post(&format!("http://{}", addr))
            .body("Hello, World!")
            .send()
            .await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }
}
