#![deny(warnings)]
#![deny(missing_docs)]

//! # Redis-Dumbpool
//!
//! Really dumb implementation of a Redis Connection Pool

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Weak};
use std::time::Duration;

use redis::{Client, cmd, aio::Connection, RedisResult};

use tokio::{time::{sleep, interval_at, Instant}, sync::Mutex};

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
    /// Create a new Pool, pointing to a single Redis server, with minimum and maximum number of active connections,
    /// the amount of nanoseconds to wait between retries when the connection pool is at maximum capacity
    /// and the amount of nanoseconds between every connection keepalive (0 for no keepalive)
    pub async fn factory(addr: &str, min: u8, max: u8, sleep: u64, refresh: u64) -> RedisResult<Self> {
        let client = Client::open(addr)?;
        let pool = iter(0..min.min(max))
            .then(|_| async { client.get_async_connection().await })
            .try_collect()
            .await?;

        let inner = Arc::new(Mutex::new(PoolInner {
            client,
            max,
            sleep,
            count: min.min(max),
            pool,
        }));

        if refresh > 0 {
            let inner2 = Arc::clone(&inner);
            tokio::spawn(async move {
                let duration = Duration::from_nanos(refresh);
                let mut interval = interval_at(Instant::now() + duration, duration);
                loop {
                    interval.tick().await;

                    let mut lock = inner2.lock().await;

                    #[cfg(test)]
                    log::info!("Connections keepalive ({})", lock.pool.len());

                    while let Some(mut c) = lock.pool.pop() {
                        let inner3 = Arc::clone(&inner2);
                        tokio::spawn(async move {
                            if cmd("PING").query_async::<_, ()>(&mut c).await.is_ok() {
                                #[cfg(test)]
                                log::info!("Connection alive");
            
                                let mut lock = inner3.lock().await;
                                lock.pool.push(c);
                            }
                            else {
                                #[cfg(test)]
                                log::info!("Connection dead");
            
                                let mut lock = inner3.lock().await;
                                lock.count -= 1;
                            }
                        });
                    }
                }
            });
        }

        Ok(Pool {
            inner
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
                    let duration = Duration::from_nanos(lock.sleep);
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

    use std::env;
    use std::time::Duration;

    use tokio::time::sleep;

    use rand::{thread_rng, Rng};

    use redis::AsyncCommands;

    #[tokio::test]
    async fn main() {
        env_logger::init();

        let pool = Pool::factory(&env::var("REDIS_SERVER").unwrap_or_else(|_| String::from("redis://127.0.0.1:6379/")), 10, 10, 1, 1000000000).await.unwrap();
        let mut rng = thread_rng();
        loop {
            let mut conn = pool.get_conn().await.unwrap();
            let wait = rng.gen_range(0..10);
            tokio::spawn(async move {
                sleep(Duration::from_secs(wait)).await;
                conn.get::<_, Option<String>>(&env::var("REDIS_KEY").unwrap_or_else(|_| String::from("key"))).await.unwrap();
            });
        }
    }
}
