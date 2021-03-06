use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use http::StatusCode;
use multimap::MultiMap;
use roa::core::{throw, App, Model};
use roa::preload::*;
use roa::query::query_parser;
use roa::router::Router;
use serde::{Deserialize, Serialize};
use slab::Slab;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize, Hash, Eq, PartialEq)]
struct User {
    name: String,
    age: u8,
    favorite_fruit: String,
}

#[derive(Clone)]
struct DB {
    main_table: Slab<User>,
    name_index: MultiMap<String, usize>,
}

impl DB {
    fn new() -> Self {
        Self {
            main_table: Slab::new(),
            name_index: MultiMap::new(),
        }
    }

    fn add(&mut self, user: User) -> usize {
        let name = user.name.clone();
        let id = self.main_table.insert(user);
        self.name_index.insert(name, id);
        id
    }

    fn delete_index(&mut self, name: &str, id: usize) {
        if let Some(id_set) = self.name_index.get_vec_mut(name) {
            let uids = id_set.clone();
            for (index, uid) in uids.into_iter().enumerate() {
                if uid == id {
                    id_set.remove(index);
                }
            }
        }
    }

    fn delete(&mut self, id: usize) -> Option<User> {
        if !self.main_table.contains(id) {
            None
        } else {
            let user = self.main_table.remove(id);
            self.delete_index(&user.name, id);
            Some(user)
        }
    }

    fn get(&self, id: usize) -> Option<&User> {
        self.main_table.get(id)
    }

    fn get_by_name(&self, name: &str) -> Vec<(usize, &User)> {
        match self.name_index.get_vec(name) {
            None => Vec::new(),
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.get(*id).map(|user| (*id, user)))
                .collect(),
        }
    }

    fn update(&mut self, id: usize, new_user: &mut User) -> bool {
        let new_name = new_user.name.clone();
        let swapped = self
            .main_table
            .get_mut(id)
            .map(|user| std::mem::swap(user, new_user))
            .is_some();
        if swapped {
            self.delete_index(&new_user.name, id);
            self.name_index.insert(new_name, id);
        }
        swapped
    }
}

struct AppModel {
    db: Arc<RwLock<DB>>,
}

struct AppState {
    db: Arc<RwLock<DB>>,
}

impl AppModel {
    fn new() -> Self {
        Self {
            db: Arc::new(RwLock::new(DB::new())),
        }
    }
}

impl Model for AppModel {
    type State = AppState;

    fn new_state(&self) -> Self::State {
        AppState {
            db: self.db.clone(),
        }
    }
}

fn crud_router() -> Result<Router<AppState>, Box<dyn std::error::Error>> {
    let mut router = Router::<AppState>::new();
    let mut id_router = Router::<AppState>::new();
    router.post("", |mut ctx| async move {
        let user = ctx.read_json().await?;
        let id = ctx.state().await.db.write().await.add(user);
        let mut data = HashMap::new();
        data.insert("id", id);
        ctx.resp_mut().await.status = StatusCode::CREATED;
        ctx.write_json(&data).await
    });
    id_router
        .get("", |ctx| async move {
            let id = ctx.must_param("id").await?.parse()?;
            match ctx.state().await.db.read().await.get(id) {
                Some(user) => ctx.clone().write_json(user).await,
                None => throw!(StatusCode::NOT_FOUND, format!("id({}) not found", id)),
            }
        })
        .put("", |mut ctx| async move {
            let id = ctx.must_param("id").await?.parse()?;
            let mut user = ctx.read_json().await?;
            if ctx.state().await.db.write().await.update(id, &mut user) {
                ctx.write_json(&user).await
            } else {
                throw!(StatusCode::NOT_FOUND, format!("id({}) not found", id))
            }
        })
        .delete("", |ctx| async move {
            let id = ctx.must_param("id").await?.parse()?;
            match ctx.state().await.db.write().await.delete(id) {
                Some(user) => ctx.clone().write_json(&user).await,
                None => throw!(StatusCode::NOT_FOUND, format!("id({}) not found", id)),
            }
        });
    router.include("/:id", id_router);
    Ok(router)
}

#[tokio::test]
async fn restful_crud() -> Result<(), Box<dyn std::error::Error>> {
    let (addr, server) = App::new(AppModel::new())
        .gate(crud_router()?.routes("/user")?)
        .run_local()?;
    spawn(server);
    // first get, 404 Not Found
    let resp = reqwest::get(&format!("http://{}/user/0", addr)).await?;
    assert_eq!(StatusCode::NOT_FOUND, resp.status());

    let user = User {
        name: "Hexilee".to_string(),
        age: 20,
        favorite_fruit: "Apple".to_string(),
    };

    // post
    let client = reqwest::Client::new();
    let resp = client
        .post(&format!("http://{}/user", addr))
        .json(&user)
        .send()
        .await?;
    assert_eq!(StatusCode::CREATED, resp.status());
    let data: HashMap<String, usize> = serde_json::from_str(&resp.text().await?)?;
    assert_eq!(0, data["id"]);

    // get
    let resp = reqwest::get(&format!("http://{}/user/0", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(&user, &resp.json().await?);

    // put
    let another = User {
        name: "Bob".to_string(),
        age: 120,
        favorite_fruit: "Lemon".to_string(),
    };

    let resp = client
        .put(&format!("http://{}/user/0", addr))
        .json(&another)
        .send()
        .await?;
    assert_eq!(StatusCode::OK, resp.status());

    // return first user
    assert_eq!(&user, &resp.json().await?);

    // updated, get new user
    let resp = reqwest::get(&format!("http://{}/user/0", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(&another, &resp.json().await?);

    // delete, get deleted user
    let resp = client
        .delete(&format!("http://{}/user/0", addr))
        .send()
        .await?;
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(&another, &resp.json().await?);

    // delete again, 404 Not Found
    let resp = client
        .delete(&format!("http://{}/user/0", addr))
        .send()
        .await?;
    assert_eq!(StatusCode::NOT_FOUND, resp.status());

    // put again, 404 Not Found
    let resp = client
        .put(&format!("http://{}/user/0", addr))
        .json(&another)
        .send()
        .await?;
    assert_eq!(StatusCode::NOT_FOUND, resp.status());
    Ok(())
}

fn batch_router() -> Result<Router<AppState>, Box<dyn std::error::Error>> {
    let mut router = Router::<AppState>::new();
    router
        .post("/user", |mut ctx| async move {
            let users: Vec<User> = ctx.read_json().await?;
            let mut ids = Vec::new();
            let state = ctx.state().await;
            for user in users {
                ids.push(state.db.write().await.add(user))
            }
            drop(state);
            ctx.resp_mut().await.status = StatusCode::CREATED;
            ctx.write_json(&ids).await
        })
        .get("/user", |ctx| async move {
            let state = ctx.state().await;
            let db = state.db.read().await;
            let users = match ctx.query("name").await {
                Some(name) => db.get_by_name(&name),
                None => db.main_table.iter().collect(),
            };
            ctx.clone().write_json(&users).await
        });
    Ok(router)
}

#[tokio::test]
async fn batch() -> Result<(), Box<dyn std::error::Error>> {
    let (addr, server) = App::new(AppModel::new())
        .gate(query_parser)
        .gate(batch_router()?.routes("/")?)
        .run_local()?;
    spawn(server);
    // first get, list empty
    let resp = reqwest::get(&format!("http://{}/user", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    let data: Vec<(usize, User)> = resp.json().await?;
    assert!(data.is_empty());

    // post batch
    let client = reqwest::Client::new();
    let users = vec![
        User {
            name: "Hexilee".to_string(),
            age: 20,
            favorite_fruit: "Apple".to_string(),
        },
        User {
            name: "Bob".to_string(),
            age: 120,
            favorite_fruit: "Lemon".to_string(),
        },
        User {
            name: "Hexilee".to_string(),
            age: 40,
            favorite_fruit: "Orange".to_string(),
        },
    ];
    let resp = client
        .post(&format!("http://{}/user", addr))
        .json(&users)
        .send()
        .await?;
    assert_eq!(StatusCode::CREATED, resp.status());
    let ids: Vec<usize> = resp.json().await?;
    assert_eq!(vec![0, 1, 2], ids);

    // get all
    let resp = reqwest::get(&format!("http://{}/user", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    let data: Vec<(usize, User)> = resp.json().await?;
    assert_eq!(3, data.len());
    for (index, user) in data.iter() {
        assert_eq!(&users[*index], user);
    }

    // get by name
    let resp = reqwest::get(&format!("http://{}/user?name=Alice", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    let data: Vec<(usize, User)> = resp.json().await?;
    assert!(data.is_empty());

    let resp = reqwest::get(&format!("http://{}/user?name=Hexilee", addr)).await?;
    assert_eq!(StatusCode::OK, resp.status());
    let data: Vec<(usize, User)> = resp.json().await?;
    assert_eq!(2, data.len());
    assert_eq!(0, data[0].0);
    assert_eq!(&users[0], &data[0].1);
    assert_eq!(2, data[1].0);
    assert_eq!(&users[2], &data[1].1);
    Ok(())
}
