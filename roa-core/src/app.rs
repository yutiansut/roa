#[cfg(feature = "runtime")]
mod executor;

mod tcp;
use crate::{
    join, join_all, Context, Error, Middleware, Model, Next, Request, Response, Result,
};
use http::{Request as HttpRequest, Response as HttpResponse};
use hyper::service::Service;
use hyper::Body as HyperBody;
use std::future::Future;
use std::pin::Pin;
use std::result::Result as StdResult;
use std::sync::Arc;
use std::task::Poll;

pub use tcp::{AddrIncoming, AddrStream};

/// The Application of roa.
/// ### Example
/// ```rust,no_run
/// use roa_core::App;
/// use log::info;
/// use async_std::fs::File;
///
/// #[async_std::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let server = App::new(())
///         .gate_fn(|ctx, next| async move {
///             info!("{} {}", ctx.method().await, ctx.uri().await);
///             next().await
///         })
///         .end(|mut ctx| async move {
///             ctx.resp_mut().await.write(File::open("assets/welcome.html").await?);
///             Ok(())
///         })
///         .listen("127.0.0.1:8000", |addr| {
///             info!("Server is listening on {}", addr)
///         })?;
///     server.await;
///     Ok(())
/// }
/// ```
///
/// ### Model
/// The `Model` and its `State` is designed to share data or handler between middlewares.
/// The only one type implemented `Model` by this crate is `()`, you can implement your custom Model if neccassary.
///
/// ```rust,no_run
/// use roa_core::{App, Model};
/// use log::info;
/// use futures::lock::Mutex;
/// use std::sync::Arc;
/// use std::collections::HashMap;
///
/// struct AppModel {
///     default_id: u64,
///     database: Arc<Mutex<HashMap<u64, String>>>,
/// }
///
/// struct AppState {
///     id: u64,
///     database: Arc<Mutex<HashMap<u64, String>>>,
/// }
///
/// impl AppModel {
///     fn new() -> Self {
///         Self {
///             default_id: 0,
///             database: Arc::new(Mutex::new(HashMap::new()))
///         }
///     }
/// }
///
/// impl Model for AppModel {
///     type State = AppState;
///     fn new_state(&self) -> Self::State {
///         AppState {
///             id: self.default_id,
///             database: self.database.clone(),
///         }
///     }
/// }
///
/// #[async_std::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let server = App::new(AppModel::new())
///         .gate_fn(|mut ctx, next| async move {
///             ctx.state_mut().await.id = 1;
///             next().await
///         })
///         .end(|ctx| async move {
///             let id = ctx.state().await.id;
///             ctx.state().await.database.lock().await.get(&id);
///             Ok(())
///         })
///         .listen("127.0.0.1:8000", |addr| {
///             info!("Server is listening on {}", addr)
///         })?;
///     server.await;
///     Ok(())
/// }
/// ```
///
/// ### Graceful Shutdown
///
/// `App::listen` returns a hyper::Server, which supports graceful shutdown.
///
/// ```rust,no_run
/// use roa_core::App;
/// use log::info;
/// use futures::channel::oneshot;
///
/// #[async_std::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Prepare some signal for when the server should start shutting down...
///     let (tx, rx) = oneshot::channel::<()>();
///     let server = App::new(())
///         .listen("127.0.0.1:8000", |addr| {
///             info!("Server is listening on {}", addr)
///         })?
///         .with_graceful_shutdown(async {
///             rx.await.ok();
///         });
///     // Await the `server` receiving the signal...
///     server.await;
///     
///     // And later, trigger the signal by calling `tx.send(())`.
///     let _ = tx.send(());
///     Ok(())
/// }
/// ```
pub struct App<M: Model> {
    middleware: Arc<dyn Middleware<M::State>>,
    pub(crate) model: Arc<M>,
}

/// An implementation of hyper HttpService.
pub struct HttpService<M: Model> {
    middleware: Arc<dyn Middleware<M::State>>,
    stream: AddrStream,
    pub(crate) model: Arc<M>,
}

impl<M: Model> App<M> {
    /// Construct an application from a model.
    pub fn new(model: M) -> Self {
        Self {
            middleware: Arc::new(join_all(Vec::new())),
            model: Arc::new(model),
        }
    }

    /// Use a middleware.
    pub fn gate(&mut self, middleware: impl Middleware<M::State>) -> &mut Self {
        self.middleware = Arc::new(join(self.middleware.clone(), middleware));
        self
    }

    /// A sugar to match a lambda as a middleware.
    ///
    /// `App::gate` cannot match a lambda without parameter type indication.
    ///
    /// ```rust
    /// use roa_core::{App, Next};
    ///
    /// let mut app = App::new(());
    /// // app.gate(|_ctx, next| async move { next().await }); compile fails.
    /// app.gate(|_ctx, next: Next| async move { next().await });
    /// ```
    ///
    /// However, with `App::gate_fn`, you can match a lambda without type indication.
    /// ```rust
    /// use roa_core::{App, Next};
    ///
    /// let mut app = App::new(());
    /// app.gate_fn(|_ctx, next| async move { next().await });
    /// ```
    pub fn gate_fn<F>(
        &mut self,
        middleware: impl 'static + Sync + Send + Fn(Context<M::State>, Next) -> F,
    ) -> &mut Self
    where
        F: 'static + Send + Future<Output = Result>,
    {
        self.gate(middleware)
    }

    /// A sugar to match a function pointer like `async fn(Context<S>) -> impl Future`
    /// and use it as a middleware(endpoint).
    ///
    /// As the ducument of `Middleware`, an endpoint is defined as a template:
    ///
    /// ```rust
    /// use roa_core::{App, Context, Result};
    /// use std::future::Future;
    ///
    /// fn endpoint<F>(ctx: Context<()>) -> F
    /// where F: 'static + Send + Future<Output=Result> {
    ///     unimplemented!()
    /// }
    /// ```
    ///
    /// However, an async function is not a template,
    /// it needs a transfer function to suit for `App::gate`.
    ///
    /// ```rust
    /// use roa_core::{App, Context, Result, State, Middleware};
    /// use std::future::Future;
    ///
    /// async fn endpoint(ctx: Context<()>) -> Result {
    ///     Ok(())
    /// }
    ///
    /// fn transfer<S, F>(endpoint: fn(Context<S>) -> F) -> impl Middleware<S>
    /// where S: State,
    ///       F: 'static + Send + Future<Output=Result> {
    ///     endpoint
    /// }
    ///
    /// App::new(()).gate(transfer(endpoint));
    /// ```
    ///
    /// And `App::end` is a wrapper of `App::gate` with this transfer function.
    ///
    /// ```rust
    /// use roa_core::App;
    /// App::new(()).end(|_ctx| async { Ok(()) });
    /// ```
    pub fn end<F>(&mut self, endpoint: fn(Context<M::State>) -> F) -> &mut Self
    where
        F: 'static + Send + Future<Output = Result>,
    {
        self.gate(endpoint)
    }
}

#[cfg(feature = "runtime")]
use executor::Executor;

#[cfg(feature = "runtime")]
use hyper::Server as HyperServer;

#[cfg(feature = "runtime")]
use std::net::{SocketAddr, ToSocketAddrs};

#[cfg(feature = "runtime")]
type Server<M> = HyperServer<AddrIncoming, App<M>, Executor>;

#[cfg(feature = "runtime")]
impl<M: Model> App<M> {
    /// Listen on a socket addr, return a server and the real addr it binds.
    fn listen_on(
        &self,
        addr: impl ToSocketAddrs,
    ) -> std::io::Result<(SocketAddr, Server<M>)> {
        let incoming = AddrIncoming::bind(addr)?;
        let local_addr = incoming.local_addr();
        let server = HyperServer::builder(incoming)
            .executor(Executor)
            .serve(self.clone());
        Ok((local_addr, server))
    }

    /// Listen on a socket addr, return a server, and pass real addr to the callback.
    pub fn listen(
        &self,
        addr: impl ToSocketAddrs,
        callback: impl Fn(SocketAddr),
    ) -> std::io::Result<Server<M>> {
        let (addr, server) = self.listen_on(addr)?;
        callback(addr);
        Ok(server)
    }

    /// Listen on an unused port of 0.0.0.0, return a server and the real addr it binds.
    pub fn run(&self) -> std::io::Result<(SocketAddr, Server<M>)> {
        self.listen_on("0.0.0.0:0")
    }

    /// Listen on an unused port of 127.0.0.1, return a server and the real addr it binds.
    /// ### Example
    /// ```rust
    /// use roa_core::App;
    /// use async_std::task::spawn;
    /// use http::StatusCode;
    /// use std::time::Instant;
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     let (addr, server) = App::new(())
    ///         .gate_fn(|_ctx, next| async move {
    ///             let inbound = Instant::now();
    ///             next().await?;
    ///             println!("time elapsed: {} ms", inbound.elapsed().as_millis());
    ///             Ok(())
    ///         })
    ///         .run_local()?;
    ///     spawn(server);
    ///     let resp = reqwest::get(&format!("http://{}", addr)).await?;
    ///     assert_eq!(StatusCode::OK, resp.status());
    ///     Ok(())
    /// }
    /// ```
    pub fn run_local(&self) -> std::io::Result<(SocketAddr, Server<M>)> {
        self.listen_on("127.0.0.1:0")
    }
}

macro_rules! impl_poll_ready {
    () => {
        #[inline]
        fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> Poll<StdResult<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    };
}

type AppFuture<M> =
    Pin<Box<dyn 'static + Future<Output = std::io::Result<HttpService<M>>> + Send>>;

impl<M: Model> Service<&AddrStream> for App<M> {
    type Response = HttpService<M>;
    type Error = std::io::Error;
    type Future = AppFuture<M>;
    impl_poll_ready!();

    #[inline]
    fn call(&mut self, stream: &AddrStream) -> Self::Future {
        let middleware = self.middleware.clone();
        let stream = stream.clone();
        let model = self.model.clone();
        Box::pin(async move { Ok(HttpService::new(middleware, stream, model)) })
    }
}

type HttpFuture =
    Pin<Box<dyn 'static + Future<Output = Result<HttpResponse<HyperBody>>> + Send>>;

impl<M: Model> Service<HttpRequest<HyperBody>> for HttpService<M> {
    type Response = HttpResponse<HyperBody>;
    type Error = Error;
    type Future = HttpFuture;
    impl_poll_ready!();

    #[inline]
    fn call(&mut self, req: HttpRequest<HyperBody>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { Ok(service.serve(req.into()).await?.into()) })
    }
}

impl<M: Model> HttpService<M> {
    pub fn new(
        middleware: Arc<dyn Middleware<M::State>>,
        stream: AddrStream,
        model: Arc<M>,
    ) -> Self {
        Self {
            middleware,
            stream,
            model,
        }
    }

    pub async fn serve(&self, req: Request) -> Result<Response> {
        let mut context = Context::new(req, self.model.new_state(), self.stream.clone());
        let middleware = self.middleware.clone();
        if let Err(err) = middleware.end(context.clone()).await {
            context.resp_mut().await.status = err.status_code;
            if err.expose {
                context.resp_mut().await.write_str(&err.message);
            }
            if err.need_throw() {
                return Err(err);
            }
        }
        let mut response = context.resp_mut().await;
        Ok(std::mem::take(&mut *response))
    }
}

impl<M: Model> Clone for App<M> {
    fn clone(&self) -> Self {
        Self {
            middleware: self.middleware.clone(),
            model: self.model.clone(),
        }
    }
}

impl<M: Model> Clone for HttpService<M> {
    fn clone(&self) -> Self {
        Self {
            middleware: self.middleware.clone(),
            model: self.model.clone(),
            stream: self.stream.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use async_std::task::spawn;
    use http::StatusCode;
    use std::time::Instant;

    #[tokio::test]
    async fn gate_simple() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, server) = App::new(())
            .gate_fn(|_ctx, next| async move {
                let inbound = Instant::now();
                next().await?;
                println!("time elapsed: {} ms", inbound.elapsed().as_millis());
                Ok(())
            })
            .run_local()?;
        spawn(server);
        let resp = reqwest::get(&format!("http://{}", addr)).await?;
        assert_eq!(StatusCode::OK, resp.status());
        Ok(())
    }
}
