use async_std::sync::{Arc, RwLock};
use roa::core::{throw, App, Context, Model, Result, StatusCode};
use roa::preload::*;
use roa::router::Router;
use serde::{Deserialize, Serialize};
use serde_json::json;
use slab::Slab;
use std::result::Result as StdResult;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct User {
    name: String,
    age: u8,
}

#[derive(Clone)]
struct Database {
    table: Arc<RwLock<Slab<User>>>,
}

impl Database {
    fn new() -> Self {
        Self {
            table: Arc::new(RwLock::new(Slab::new())),
        }
    }

    async fn create(&self, user: User) -> usize {
        self.table.write().await.insert(user)
    }

    async fn retrieve(&self, id: usize) -> Result<User> {
        match self.table.read().await.get(id) {
            Some(user) => Ok(user.clone()),
            None => throw!(StatusCode::NOT_FOUND),
        }
    }

    async fn update(&self, id: usize, new_user: &mut User) -> Result {
        match self.table.write().await.get_mut(id) {
            Some(user) => {
                std::mem::swap(new_user, user);
                Ok(())
            }
            None => throw!(StatusCode::NOT_FOUND),
        }
    }

    async fn delete(&self, id: usize) -> Result<User> {
        if !self.table.read().await.contains(id) {
            throw!(StatusCode::NOT_FOUND)
        }
        Ok(self.table.write().await.remove(id))
    }
}

struct AppModel {
    db: Database,
}

impl Model for AppModel {
    type State = Database;
    fn new_state(&self) -> Self::State {
        self.db.clone()
    }
}

async fn create_user(mut ctx: Context<Database>) -> Result {
    let user: User = ctx.read().await?;
    let id = ctx.state().await.create(user).await;
    ctx.write_json(&json!({ "id": id })).await?;
    ctx.resp_mut().await.status = StatusCode::CREATED;
    Ok(())
}

async fn get_user(mut ctx: Context<Database>) -> Result {
    let id: usize = ctx.must_param("id").await?.parse()?;
    let user = ctx.state().await.retrieve(id).await?;
    ctx.write_json(&user).await
}

async fn update_user(mut ctx: Context<Database>) -> Result {
    let id: usize = ctx.must_param("id").await?.parse()?;
    let mut user: User = ctx.read().await?;
    ctx.state().await.update(id, &mut user).await?;
    ctx.write_json(&user).await
}

async fn delete_user(mut ctx: Context<Database>) -> Result {
    let id: usize = ctx.must_param("id").await?.parse()?;
    let user = ctx.state().await.delete(id).await?;
    ctx.write_json(&user).await
}

#[async_std::main]
async fn main() -> StdResult<(), Box<dyn std::error::Error>> {
    let mut app = App::new(AppModel {
        db: Database::new(),
    });
    let mut router = Router::new();
    router
        .post("/", create_user)
        .get("/:id", get_user)
        .put("/:id", update_user)
        .delete("/:id", delete_user);
    app.gate(router.routes("/user")?);
    app.listen("127.0.0.1:8000", |addr| {
        println!("Server is listening on {}", addr)
    })?
    .await?;
    Ok(())
}
