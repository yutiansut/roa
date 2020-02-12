mod err;
mod path;

use err::{Conflict, RouterError};
use path::{join_path, standardize_path, Path, RegexPath};

use crate::core::{join_all, throw, Context, Error, Middleware, Next, Result, State, Variable};
use async_trait::async_trait;
use http::{Method, StatusCode};
use percent_encoding::percent_decode_str;
use radix_trie::Trie;
use std::collections::HashMap;
use std::convert::AsRef;
use std::future::Future;
use std::result::Result as StdResult;
use std::sync::Arc;

const ALL_METHODS: [Method; 9] = [
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::OPTIONS,
    Method::DELETE,
    Method::HEAD,
    Method::TRACE,
    Method::CONNECT,
];

struct RouterSymbol;

#[async_trait]
pub trait RouterParam {
    async fn param<'a>(&self, name: &'a str) -> Result<Variable<'a>>;
    async fn try_param<'a>(&self, name: &'a str) -> Option<Variable<'a>>;
}

pub struct Router<S: State> {
    middlewares: Vec<Arc<dyn Middleware<S>>>,
    endpoints: Vec<(Method, String, Arc<dyn Middleware<S>>)>,
}

struct RouteTable<S: State> {
    static_route: Trie<String, Arc<dyn Middleware<S>>>,
    dynamic_route: Vec<(RegexPath, Arc<dyn Middleware<S>>)>,
}

pub struct RouteEndpoint<S: State>(HashMap<Method, RouteTable<S>>);

impl<S: State> Router<S> {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
            endpoints: Vec::new(),
        }
    }

    pub fn gate(&mut self, middleware: impl Middleware<S>) -> &mut Self {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    pub fn gate_fn<F>(
        &mut self,
        middleware: impl 'static + Sync + Send + Fn(Context<S>, Next) -> F,
    ) -> &mut Self
    where
        F: 'static + Future<Output = Result> + Send,
    {
        self.gate(middleware);
        self
    }

    pub fn end(
        &mut self,
        methods: &[Method],
        path: &'static str,
        endpoint: impl Middleware<S>,
    ) -> &mut Self {
        let endpoint_ptr = Arc::new(endpoint);
        for method in methods {
            self.endpoints
                .push((method.clone(), path.to_string(), endpoint_ptr.clone()));
        }
        self
    }

    pub fn end_fn<F>(
        &mut self,
        path: &'static str,
        methods: &[Method],
        endpoint: fn(Context<S>) -> F,
    ) -> &mut Self
    where
        F: 'static + Future<Output = Result> + Send,
    {
        self.end(methods, path, endpoint)
    }

    pub fn include(&mut self, prefix: &'static str, router: Router<S>) -> &mut Self {
        self.include_methods(prefix, router, ALL_METHODS)
    }

    pub fn include_methods(
        &mut self,
        prefix: &'static str,
        router: Router<S>,
        methods: impl AsRef<[Method]>,
    ) -> &mut Self {
        for (method, path, endpoint) in router.on(prefix) {
            if methods.as_ref().contains(&method) {
                self.endpoints.push((method, path, endpoint))
            }
        }
        self
    }

    fn on(
        &self,
        prefix: &'static str,
    ) -> impl '_ + Iterator<Item = (Method, String, Arc<dyn Middleware<S>>)> {
        self.endpoints.iter().map(move |(method, path, endpoint)| {
            let mut middlewares = self.middlewares.clone();
            middlewares.push(endpoint.clone());
            let new_endpoint: Arc<dyn Middleware<S>> = Arc::new(join_all(middlewares));
            let new_path = join_path(&vec![prefix, path.as_str()]);
            (method.clone(), new_path, new_endpoint)
        })
    }

    pub fn routes(self, prefix: &'static str) -> StdResult<RouteEndpoint<S>, RouterError> {
        let mut route_endpoint = RouteEndpoint::default();
        for (method, raw_path, endpoint) in self.on(prefix) {
            route_endpoint.insert(method, raw_path, endpoint)?;
        }
        Ok(route_endpoint)
    }
}

macro_rules! impl_http_method {
    ($end:ident, $end_fn:ident, $($method:expr),*) => {
        pub fn $end(&mut self, path: &'static str, endpoint: impl Middleware<S>) -> &mut Self {
            self.end([$($method, )*].as_ref(), path, endpoint)
        }
        pub fn $end_fn<F>(&mut self, path: &'static str, endpoint: fn(Context<S>) -> F) -> &mut Self
        where
            F: 'static + Send + Future<Output = Result>,
        {
            self.end([$($method, )*].as_ref(), path, endpoint)
        }
    };
}

impl<S: State> Router<S> {
    impl_http_method!(get, get_fn, Method::GET);
    impl_http_method!(post, post_fn, Method::POST);
    impl_http_method!(put, put_fn, Method::PUT);
    impl_http_method!(patch, patch_fn, Method::PATCH);
    impl_http_method!(options, options_fn, Method::OPTIONS);
    impl_http_method!(delete, delete_fn, Method::DELETE);
    impl_http_method!(head, head_fn, Method::HEAD);
    impl_http_method!(trace, trace_fn, Method::TRACE);
    impl_http_method!(connect, connect_fn, Method::CONNECT);
    impl_http_method!(
        all,
        all_fn,
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::OPTIONS,
        Method::DELETE,
        Method::HEAD,
        Method::TRACE,
        Method::CONNECT
    );
}

impl<S: State> Default for RouteEndpoint<S> {
    fn default() -> Self {
        let mut map = HashMap::new();
        for method in ALL_METHODS.as_ref() {
            map.insert(method.clone(), RouteTable::new());
        }
        Self(map)
    }
}

impl<S: State> RouteEndpoint<S> {
    fn insert(
        &mut self,
        method: Method,
        raw_path: impl AsRef<str>,
        endpoint: Arc<dyn Middleware<S>>,
    ) -> StdResult<(), RouterError> {
        match self.0.get_mut(&method) {
            Some(route_table) => route_table.insert(raw_path, endpoint),
            None => {
                self.0.insert(method.clone(), RouteTable::new());
                self.insert(method, raw_path, endpoint)
            }
        }
    }
}

impl<S: State> RouteTable<S> {
    fn new() -> Self {
        Self {
            static_route: Trie::new(),
            dynamic_route: Vec::new(),
        }
    }

    fn insert(
        &mut self,
        raw_path: impl AsRef<str>,
        endpoint: Arc<dyn Middleware<S>>,
    ) -> StdResult<(), RouterError> {
        match raw_path.as_ref().parse()? {
            Path::Static(path) => {
                if self
                    .static_route
                    .insert(path.to_string(), endpoint)
                    .is_some()
                {
                    return Err(Conflict::Path(path.to_string()).into());
                }
            }
            Path::Dynamic(regex_path) => self.dynamic_route.push((regex_path.clone(), endpoint)),
        }
        Ok(())
    }

    async fn end(&self, ctx: Context<S>) -> Result {
        let uri = ctx.uri().await;
        let path = standardize_path(&percent_decode_str(uri.path()).decode_utf8().map_err(
            |err| {
                Error::new(
                    StatusCode::BAD_REQUEST,
                    format!("{}\npath `{}` is not a valid utf-8 string", err, uri.path()),
                    true,
                )
            },
        )?);
        if let Some(handler) = self.static_route.get(&path) {
            return handler.clone().end(ctx).await;
        }

        for (regexp_path, handler) in self.dynamic_route.iter() {
            if let Some(cap) = regexp_path.re.captures(&path) {
                for var in regexp_path.vars.iter() {
                    ctx.store::<RouterSymbol>(var, cap[var.as_str()].to_string())
                        .await;
                }
                return handler.clone().end(ctx).await;
            }
        }
        throw!(StatusCode::NOT_FOUND)
    }
}

#[async_trait]
impl<S: State> Middleware<S> for RouteEndpoint<S> {
    async fn handle(self: Arc<Self>, ctx: Context<S>, _next: Next) -> Result {
        match self.0.get(&ctx.method().await) {
            None => throw!(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("method {} is not allowed", &ctx.method().await)
            ),
            Some(handler) => handler.end(ctx).await,
        }
    }
}

#[async_trait]
impl<S: State> RouterParam for Context<S> {
    async fn param<'a>(&self, name: &'a str) -> Result<Variable<'a>> {
        self.try_param(name).await.ok_or_else(|| {
            Error::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("router variable `{}` is required", name),
                false,
            )
        })
    }
    async fn try_param<'a>(&self, name: &'a str) -> Option<Variable<'a>> {
        self.load::<RouterSymbol>(name).await
    }
}

#[cfg(test)]
mod tests {
    use super::Router;
    use crate::core::App;
    use async_std::task::spawn;
    use encoding::EncoderTrap;
    use http::StatusCode;
    use percent_encoding::NON_ALPHANUMERIC;

    #[tokio::test]
    async fn gate() -> Result<(), Box<dyn std::error::Error>> {
        struct TestSymbol;
        let mut router = Router::<()>::new();
        router
            .gate_fn(|ctx, next| async move {
                ctx.store::<TestSymbol>("id", "0".to_string()).await;
                next().await
            })
            .get_fn("/", |ctx| async move {
                let id: u64 = ctx.load::<TestSymbol>("id").await.unwrap().parse()?;
                assert_eq!(0, id);
                Ok(())
            });
        let (addr, server) = App::new(()).gate(router.routes("/route")?).run_local()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}/route", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }

    #[tokio::test]
    async fn route() -> Result<(), Box<dyn std::error::Error>> {
        struct TestSymbol;
        let mut router = Router::<()>::new();
        let mut user_router = Router::<()>::new();
        router.gate_fn(|ctx, next| async move {
            ctx.store::<TestSymbol>("id", "0".to_string()).await;
            next().await
        });
        user_router.get_fn("/", |ctx| async move {
            let id: u64 = ctx.load::<TestSymbol>("id").await.unwrap().parse()?;
            assert_eq!(0, id);
            Ok(())
        });
        router.include("/user", user_router);

        let (addr, server) = App::new(()).gate(router.routes("/route")?).run_local()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}/route/user", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }

    #[test]
    fn conflict_path() -> Result<(), Box<dyn std::error::Error>> {
        let mut router = Router::<()>::new();
        let mut evil_router = Router::<()>::new();
        router.get_fn("/route/endpoint", |_ctx| async { Ok(()) });
        evil_router.get_fn("/endpoint", |_ctx| async { Ok(()) });
        router.include("/route", evil_router);
        let ret = router.routes("/");
        assert!(ret.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn route_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(()).gate(Router::new().routes("/")?).run_local()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::NOT_FOUND, resp.status());
        Ok(())
    }

    #[tokio::test]
    async fn non_utf8_uri() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(()).gate(Router::new().routes("/")?).run_local()?;
        spawn(server);
        let gbk_path = encoding::label::encoding_from_whatwg_label("gbk")
            .unwrap()
            .encode("路由", EncoderTrap::Strict)
            .unwrap();
        let encoded_path =
            percent_encoding::percent_encode(&gbk_path, NON_ALPHANUMERIC).to_string();
        let uri = format!("http://{}/{}", addr, encoded_path);
        let resp = reqwest::get(&uri).await?;
        assert_eq!(StatusCode::BAD_REQUEST, resp.status());
        assert!(resp
            .text()
            .await?
            .ends_with("path `/%C2%B7%D3%C9` is not a valid utf-8 string"));
        Ok(())
    }
}
