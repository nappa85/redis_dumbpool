#![deny(warnings)]
#![deny(missing_docs)]

//! # Redis-Dumbpool
//!
//! Really dumb implementation of a Redis Connection Pool

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};
use std::time::Duration;

use redis::{Client, cmd, aio::Connection, RedisResult};

use async_lock::Mutex;

use tokio::time::sleep;

use futures_util::stream::{iter, StreamExt, TryStreamExt};

pub use redis;

struct PoolInner {
    client: Client,
    max: u8,
    sleep: u64,
    count: u8,
    pool: Vec<Connection>,
}

/// Redis connection pool
#[derive(Clone)]
pub struct Pool {
    inner: Arc<Mutex<PoolInner>>
}

impl Pool {
    /// Create a new Pool, pointing to a single Redis server, with minimum and maximum number of active connections and the amount of millisecond to wait between a retries when the connection pool at maximum capacity
    pub async fn factory(addr: &str, min: u8, max: u8, sleep: u64) -> RedisResult<Self> {
        let client = Client::open(addr)?;
        let pool = iter(0..min.min(max))
            .then(|_| async { client.get_async_connection().await })
            .try_collect()
            .await?;

        Ok(Pool {
            inner: Arc::new(Mutex::new(PoolInner {
                client,
                max,
                sleep,
                count: min,
                pool,
            }))
        })
    }

    /// Retrieve a connection from the pool
    /// The connection is tested before being released, if test fails a new connection is generated
    pub async fn get_conn(&self) -> RedisResult<ConnWrapper> {
        loop {
            let mut lock = self.inner.lock().await;

            let conn = if let Some(mut c) = lock.pool.pop() {
                //test connection
                if cmd("PING").query_async::<_, ()>(&mut c).await.is_err() {
                    #[cfg(test)]
                    log::info!("Invalid connection, re-created");

                    lock.client.get_async_connection().await?
                }
                else {
                    #[cfg(test)]
                    log::info!("Recycled connection");

                    c
                }
            }
            else {
                if lock.count >= lock.max {
                    let duration = Duration::from_millis(lock.sleep);
                    drop(lock);
                    sleep(duration).await;
                    continue;
                }

                #[cfg(test)]
                log::info!("New connection created");

                lock.count += 1;
                lock.client.get_async_connection().await?
            };
            drop(lock);

            return Ok(ConnWrapper::new(Arc::downgrade(&self.inner), conn));
        }
    }
}

///Redis connection wrapper
///Ensures the connection is returned to the pool on drop
pub struct ConnWrapper {
    pool: Weak<Mutex<PoolInner>>,
    conn: Option<Connection>,
}

impl ConnWrapper {
    fn new(pool: Weak<Mutex<PoolInner>>, conn: Connection) -> Self {
        ConnWrapper {
            pool,
            conn: Some(conn),
        }
    }
}

impl Deref for ConnWrapper {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().unwrap()
    }
}

impl DerefMut for ConnWrapper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn.as_mut().unwrap()
    }
}

impl Drop for ConnWrapper {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            if let Some(conn) = self.conn.take() {
                tokio::spawn(async move {
                    #[cfg(test)]
                    log::info!("Connection returned to pool");

                    let mut lock = pool.lock().await;
                    lock.pool.push(conn);
                });
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::Pool;

    use redis::AsyncCommands;

    #[tokio::test]
    async fn main() {
        env_logger::init();

        let pool = Pool::factory("redis://127.0.0.1:6379/", 10, 10, 1).await.unwrap();
        loop {
            let mut conn = pool.get_conn().await.unwrap();
            tokio::spawn(async move {
                conn.get::<_, Option<String>>("key").await.unwrap();
            });
        }
    }
}
