mod middleware;
mod status_handler;
use crate::{Context, Model, Status, StatusFuture};
pub use middleware::{DynMiddleware, Middleware};
pub use status_handler::{default_status_handler, DynStatusHandler, StatusHandler};
use std::future::Future;

pub type DynHandler<M, R = ()> = dyn 'static + Sync + Send + Fn(Context<M>) -> StatusFuture<R>;
pub type DynTargetHandler<M, Target, R = ()> =
    dyn 'static + Sync + Send + Fn(Context<M>, Target) -> StatusFuture<R>;

pub trait Handler<M: Model, R = ()>: 'static + Sync + Send {
    type StatusFuture: 'static + Future<Output = Result<R, Status>> + Send;
    fn handle(&self, ctx: Context<M>) -> Self::StatusFuture;
    fn dynamic(self: Box<Self>) -> Box<DynHandler<M, R>> {
        Box::new(move |ctx| Box::pin(self.handle(ctx)))
    }
}

impl<M, R, F, T> Handler<M, R> for T
where
    M: Model,
    F: 'static + Future<Output = Result<R, Status>> + Send,
    T: 'static + Sync + Send + Fn(Context<M>) -> F,
{
    type StatusFuture = F;
    fn handle(&self, ctx: Context<M>) -> Self::StatusFuture {
        (self)(ctx)
    }
}

pub trait TargetHandler<M: Model, Target, R = ()>: 'static + Sync + Send {
    type StatusFuture: 'static + Future<Output = Result<R, Status>> + Send;
    fn handle(&self, ctx: Context<M>, target: Target) -> Self::StatusFuture;
    fn dynamic(self: Box<Self>) -> Box<DynTargetHandler<M, Target, R>> {
        Box::new(move |ctx, target| Box::pin(self.handle(ctx, target)))
    }
}

impl<M, F, Target, R, T> TargetHandler<M, Target, R> for T
where
    M: Model,
    F: 'static + Future<Output = Result<R, Status>> + Send,
    T: 'static + Sync + Send + Fn(Context<M>, Target) -> F,
{
    type StatusFuture = F;
    fn handle(&self, ctx: Context<M>, target: Target) -> Self::StatusFuture {
        (self)(ctx, target)
    }
}